//! Central app state: project, undo/redo, background jobs, playback.

use crate::export::{self, ExportJob};
use crate::media::{self, Tools};
use crate::preview::{Playback, ScrubWorker};
use crate::project::*;
use crate::render::preview_size;
use crate::settings::Settings;
use crate::transcribe;
use crate::ui;
use eframe::egui;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Transcript,
    Timeline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    None,
    Main,
    Overlay,
    Audio,
}

pub enum JobMsg {
    Progress(u64, f32),
    Done(u64),
    Probed { path: PathBuf, result: Result<MediaSource, String>, placement: Placement },
    Waveform { media: Id, peaks: Vec<f32> },
    Silences { media: Id, result: Result<Vec<Range>, String> },
    Transcript { media: Id, result: Result<Transcript, String> },
    ModelDownloaded(Result<PathBuf, String>),
    Exported { label: String, result: Result<PathBuf, String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskKind {
    Import,
    Waveform,
    Silence(Id),
    Transcribe(Id),
    Download,
    Export,
}

pub struct Task {
    pub id: u64,
    pub label: String,
    pub progress: f32,
    pub kind: TaskKind,
    pub cancel: Option<Arc<AtomicBool>>,
}

/// Passed to background jobs to report progress/results.
#[derive(Clone)]
pub struct Reporter {
    tx: Sender<JobMsg>,
    ctx: egui::Context,
    pub task: u64,
}

impl Reporter {
    pub fn progress(&self, v: f32) {
        let _ = self.tx.send(JobMsg::Progress(self.task, v));
        self.ctx.request_repaint();
    }

    pub fn send(&self, msg: JobMsg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }
}

type SerialJob = Box<dyn FnOnce() + Send>;

pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "aac", "flac", "ogg", "opus", "aiff", "aif"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi", "mts"];

fn serial_worker() -> Sender<SerialJob> {
    let (tx, rx) = mpsc::channel::<SerialJob>();
    std::thread::spawn(move || {
        for job in rx {
            job();
        }
    });
    tx
}

pub struct Sel {
    pub clip: Id,
    pub anchor: usize,
    pub focus: usize,
}

impl Sel {
    pub fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
}

pub struct App {
    pub ctx: egui::Context,
    pub project: Project,
    pub project_path: Option<PathBuf>,
    pub dirty: bool,
    pub revision: u64,
    undo: Vec<Project>,
    redo: Vec<Project>,
    pub settings: Settings,

    job_tx: Sender<JobMsg>,
    job_rx: Receiver<JobMsg>,
    pub tasks: Vec<Task>,
    next_task: u64,
    heavy_queue: Sender<SerialJob>,
    export_queue: Sender<SerialJob>,
    /// Clips whose silence should be removed as soon as the analysis is finished.
    pending_silence: Vec<Id>,

    pub playhead: f64,
    pub playback: Option<Playback>,
    playback_rev: u64,
    audio_out: Option<(rodio::OutputStream, rodio::OutputStreamHandle)>,
    audio_failed: bool,
    scrub: ScrubWorker,
    scrub_key: Option<(u64, u64, (u32, u32))>,
    pub preview_tex: Option<egui::TextureHandle>,
    pub preview_error: Option<String>,

    pub selected_clip: Option<Id>,
    pub focus: Focus,
    pub sel: Option<Sel>,
    pub inspector_editing: bool,
    pub transcript_ui: ui::transcript::TranscriptUi,
    pub timeline_ui: ui::timeline::TimelineUi,
    pub export_ui: ui::dialogs::ExportUi,
    pub show_settings: bool,
    pub show_project_settings: bool,
    pub show_help: bool,
    pub show_subtitles: bool,
    /// Start screen: choose video or audio project.
    pub show_start: bool,
    pub confirm_close: bool,
    allow_close: bool,
    pub status: String,
    status_at: Instant,
    pub log: Vec<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        ui::style::apply(&cc.egui_ctx);
        let settings = Settings::load();
        let tools = settings.tools();
        let (job_tx, job_rx) = mpsc::channel();
        let mut app = App {
            ctx: cc.egui_ctx.clone(),
            project: Project::default(),
            project_path: None,
            dirty: false,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            settings,
            job_tx,
            job_rx,
            tasks: Vec::new(),
            next_task: 1,
            heavy_queue: serial_worker(),
            export_queue: serial_worker(),
            pending_silence: Vec::new(),
            playhead: 0.0,
            playback: None,
            playback_rev: 0,
            audio_out: None,
            audio_failed: false,
            scrub: ScrubWorker::new(tools.ffmpeg.clone()),
            scrub_key: None,
            preview_tex: None,
            preview_error: None,
            selected_clip: None,
            focus: Focus::Transcript,
            sel: None,
            inspector_editing: false,
            transcript_ui: Default::default(),
            timeline_ui: Default::default(),
            export_ui: Default::default(),
            show_settings: false,
            show_project_settings: false,
            show_help: false,
            show_subtitles: false,
            show_start: open.is_none(),
            confirm_close: false,
            allow_close: false,
            status: String::new(),
            status_at: Instant::now(),
            log: Vec::new(),
        };
        if media::probe(&tools, Path::new("/nonexistent"), 0).err().is_some_and(|e| e.to_string().contains("Could not start")) {
            app.set_status("⚠ ffmpeg/ffprobe not found – please install it (brew install ffmpeg) or set the path in Settings.");
        }
        if let Some(path) = open {
            if path.extension().is_some_and(|e| e == "tcf") {
                app.open_project(&path);
            } else {
                // Pure audio file → audio project, otherwise video project.
                let audio = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
                app.new_project(if audio { ProjectMode::Audio } else { ProjectMode::Video });
                app.import_files(vec![path], Placement::Main);
            }
        }
        app
    }

    pub fn tools(&self) -> Tools {
        self.settings.tools()
    }

    pub fn set_status(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.log.push(s.clone());
        if self.log.len() > 200 {
            self.log.remove(0);
        }
        self.status = s;
        self.status_at = Instant::now();
    }

    // -----------------------------------------------------------------------
    // Undo / Redo
    // -----------------------------------------------------------------------

    /// Call before every change: saves the current state.
    pub fn checkpoint(&mut self) {
        self.push_undo(self.project.clone());
    }

    pub fn push_undo(&mut self, snapshot: Project) {
        self.undo.push(snapshot);
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.touch();
    }

    /// Discards the last checkpoint (e.g. when a drag changed nothing).
    pub fn drop_checkpoint(&mut self) {
        self.undo.pop();
    }

    pub fn touch(&mut self) {
        self.revision += 1;
        self.dirty = true;
    }

    fn restore(&mut self, mut state: Project) -> Project {
        // Media (transcripts, analyses) always stay up to date.
        state.media = self.project.media.clone();
        state.next_id = state.next_id.max(self.project.next_id);
        let old = std::mem::replace(&mut self.project, state);
        self.sel = None;
        if self.selected_clip.is_some_and(|id| self.project.clip(id).is_none()) {
            self.selected_clip = None;
        }
        self.touch();
        old
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            let cur = self.restore(prev);
            self.redo.push(cur);
            self.set_status("Undo");
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let cur = self.restore(next);
            self.undo.push(cur);
            self.set_status("Redo");
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // -----------------------------------------------------------------------
    // Background jobs
    // -----------------------------------------------------------------------

    fn spawn_task(
        &mut self,
        label: String,
        kind: TaskKind,
        serial: Option<Sender<SerialJob>>,
        with_cancel: bool,
        f: impl FnOnce(Reporter, Arc<AtomicBool>) + Send + 'static,
    ) -> u64 {
        let id = self.next_task;
        self.next_task += 1;
        let cancel = Arc::new(AtomicBool::new(false));
        self.tasks.push(Task { id, label, progress: 0.0, kind, cancel: with_cancel.then(|| cancel.clone()) });
        let rep = Reporter { tx: self.job_tx.clone(), ctx: self.ctx.clone(), task: id };
        let job: SerialJob = Box::new(move || {
            let done = rep.clone();
            f(rep, cancel);
            done.send(JobMsg::Done(done.task));
        });
        match serial {
            Some(q) => {
                let _ = q.send(job);
            }
            None => {
                std::thread::spawn(job);
            }
        }
        id
    }

    pub fn task_running(&self, kind: TaskKind) -> bool {
        self.tasks.iter().any(|t| t.kind == kind)
    }

    pub fn import_dialog(&mut self, placement: Placement) {
        let all: Vec<&str> = AUDIO_EXTENSIONS.iter().chain(VIDEO_EXTENSIONS).copied().collect();
        let mut dialog = rfd::FileDialog::new().set_title("Import media");
        dialog = if self.project.is_audio() {
            dialog.add_filter("Audio", AUDIO_EXTENSIONS).add_filter("Video (audio only)", VIDEO_EXTENSIONS)
        } else {
            dialog.add_filter("Video & Audio", &all)
        };
        let files = dialog.add_filter("All files", &["*"]).pick_files();
        if let Some(files) = files {
            self.import_files(files, placement);
        }
    }

    pub fn import_files(&mut self, paths: Vec<PathBuf>, placement: Placement) {
        let tools = self.tools();
        for path in paths {
            if path.extension().is_some_and(|e| e == "tcf") {
                self.open_project(&path);
                continue;
            }
            let id = self.project.alloc_id();
            let name = crate::util::file_stem(&path);
            let tools = tools.clone();
            self.spawn_task(format!("Analyzing {name}"), TaskKind::Import, None, false, move |rep, _| {
                let result = media::probe(&tools, &path, id).map_err(|e| e.to_string());
                rep.send(JobMsg::Probed { path, result, placement });
            });
        }
    }

    fn on_probed(&mut self, path: PathBuf, result: Result<MediaSource, String>, placement: Placement) {
        let media = match result {
            Ok(m) => m,
            Err(e) => {
                self.set_status(format!("❌ {}: {e}", path.display()));
                return;
            }
        };
        let id = media.id;
        let kind = media.kind;
        self.set_status(format!("Imported: {}", media.name));
        self.project.media.push(media);
        self.touch();

        let placement = match (self.project.mode, placement, kind) {
            // In audio mode everything (including video files) goes onto the main track as audio.
            (ProjectMode::Audio, Placement::Overlay, _) => Placement::Audio,
            (ProjectMode::Audio, p, _) => p,
            (ProjectMode::Video, Placement::Main | Placement::Overlay, MediaKind::Audio) => Placement::Audio,
            (_, p, _) => p,
        };
        if placement != Placement::None {
            self.add_to_timeline(id, placement);
        }
        self.start_waveform(id);
        if self.settings.auto_detect_silence {
            self.start_silence_detection(id);
        }
        if self.settings.auto_transcribe && self.settings.whisper_ready().is_ok() {
            self.start_transcription(id);
        }
    }

    pub fn add_to_timeline(&mut self, media: Id, placement: Placement) {
        let track = match placement {
            Placement::None => return,
            Placement::Main => self.project.main_track(),
            Placement::Overlay => match self.project.video_tracks().get(1) {
                Some(t) => t.id,
                None => self.project.add_track(TrackKind::Video),
            },
            // Video mode: A1. Audio mode: A1 is the main track → second track (music, ambience).
            Placement::Audio => {
                let index = if self.project.is_audio() { 1 } else { 0 };
                match self.project.audio_tracks().get(index) {
                    Some(t) => t.id,
                    None => self.project.add_track(TrackKind::Audio),
                }
            }
        };
        self.checkpoint();
        let at = self.playhead;
        match self.project.add_clip(media, track, at) {
            Some(clip) => {
                self.selected_clip = Some(clip);
                self.transcript_ui.scroll_to = Some(clip);
            }
            None => {
                self.drop_checkpoint();
                self.set_status("This clip does not fit on this track.");
            }
        }
    }

    /// Measures word levels (for the "uncertain" marking) once transcript and
    /// waveform are available. Returns the number of uncertain words.
    fn measure_levels(&mut self, media: Id) -> usize {
        let Some(m) = self.project.media_mut(media) else { return 0 };
        let (Some(peaks), Some(t)) = (m.waveform.clone(), m.transcript.as_mut()) else { return 0 };
        t.measure_levels(&peaks, m.silences.as_deref().unwrap_or(&[]));
        (0..t.words.len()).filter(|&i| t.is_uncertain(i)).count()
    }

    pub fn start_waveform(&mut self, media: Id) {
        let Some(m) = self.project.media(media) else { return };
        if !m.has_audio {
            return;
        }
        let path = m.path.clone();
        let tools = self.tools();
        self.spawn_task(format!("Waveform {}", m.name), TaskKind::Waveform, None, false, move |rep, _| {
            if let Ok(peaks) = media::waveform(&tools, &path) {
                rep.send(JobMsg::Waveform { media, peaks });
            }
        });
    }

    pub fn start_silence_detection(&mut self, media: Id) {
        if self.task_running(TaskKind::Silence(media)) {
            return;
        }
        let Some(m) = self.project.media(media) else { return };
        if !m.has_audio {
            return;
        }
        let (path, duration) = (m.path.clone(), m.duration);
        let tools = self.tools();
        let (db, min) = (self.settings.silence_db, self.settings.silence_min);
        self.spawn_task(format!("Silence analysis {}", m.name), TaskKind::Silence(media), None, false, move |rep, _| {
            let result = media::detect_silence(&tools, &path, db, min, duration).map_err(|e| e.to_string());
            rep.send(JobMsg::Silences { media, result });
        });
    }

    pub fn start_transcription(&mut self, media: Id) {
        if self.task_running(TaskKind::Transcribe(media)) {
            return;
        }
        if let Err(e) = self.settings.whisper_ready() {
            self.set_status(format!("Transcription not possible: {e}"));
            self.show_settings = true;
            return;
        }
        let Some(m) = self.project.media(media) else { return };
        if !m.has_audio {
            self.set_status("This file has no audio track.");
            return;
        }
        let path = m.path.clone();
        let tools = self.tools();
        let cfg = self.settings.whisper_config();
        let queue = self.heavy_queue.clone();
        self.spawn_task(format!("Transcribing {}", m.name), TaskKind::Transcribe(media), Some(queue), false, move |rep, _| {
            let result = transcribe::transcribe(&tools, &cfg, &path, &|p| rep.progress(p)).map_err(|e| format!("{e:#}"));
            rep.send(JobMsg::Transcript { media, result });
        });
    }

    pub fn download_model(&mut self) {
        if self.task_running(TaskKind::Download) {
            return;
        }
        let name = self.settings.model.clone();
        self.spawn_task(format!("Downloading Whisper model \"{name}\""), TaskKind::Download, None, false, move |rep, _| {
            let result = transcribe::download_model(&name, &|p| rep.progress(p)).map_err(|e| e.to_string());
            rep.send(JobMsg::ModelDownloaded(result));
        });
    }

    pub fn queue_exports(&mut self, jobs: Vec<ExportJob>) {
        let project = Arc::new(self.project.clone());
        let tools = self.tools();
        let queue = self.export_queue.clone();
        for job in jobs {
            let project = project.clone();
            let tools = tools.clone();
            let label = job.label.clone();
            self.spawn_task(format!("Export {label}"), TaskKind::Export, Some(queue.clone()), true, move |rep, cancel| {
                let result = export::run(&tools, &project, &job, &|p| rep.progress(p), &cancel)
                    .map(|_| job.path.clone())
                    .map_err(|e| e.to_string());
                rep.send(JobMsg::Exported { label, result });
            });
        }
    }

    fn handle_jobs(&mut self) {
        while let Ok(msg) = self.job_rx.try_recv() {
            match msg {
                JobMsg::Progress(id, v) => {
                    if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
                        t.progress = v;
                    }
                }
                JobMsg::Done(id) => self.tasks.retain(|t| t.id != id),
                JobMsg::Probed { path, result, placement } => self.on_probed(path, result, placement),
                JobMsg::Waveform { media, peaks } => {
                    if let Some(m) = self.project.media_mut(media) {
                        m.waveform = Some(Arc::new(peaks));
                    }
                    self.measure_levels(media);
                }
                JobMsg::Silences { media, result } => match result {
                    Ok(s) => {
                        let n = s.len();
                        if let Some(m) = self.project.media_mut(media) {
                            m.silences = Some(s);
                        }
                        self.measure_levels(media);
                        self.touch();
                        // Update clips that are waiting for the result or already have silence cuts.
                        let clips: Vec<Id> = self
                            .project
                            .clips
                            .iter()
                            .filter(|c| {
                                c.source == media
                                    && (self.pending_silence.contains(&c.id)
                                        || c.cuts.iter().any(|k| k.kind == CutKind::Silence))
                            })
                            .map(|c| c.id)
                            .collect();
                        if !clips.is_empty() {
                            self.checkpoint();
                            let removed = self.apply_silence(&clips);
                            self.set_status(format!("{n} pauses detected – {removed} removed."));
                        } else {
                            self.set_status(format!("Silence analysis finished: {n} pauses detected."));
                        }
                        self.pending_silence.retain(|c| !clips.contains(c));
                    }
                    Err(e) => self.set_status(format!("❌ Silence analysis: {e}")),
                },
                JobMsg::Transcript { media, result } => match result {
                    Ok(t) => {
                        let n = t.words.len();
                        if let Some(m) = self.project.media_mut(media) {
                            m.transcript = Some(t);
                        }
                        let unsure = self.measure_levels(media);
                        self.touch();
                        let extra = if unsure > 0 { format!(" ({unsure} uncertain, highlighted in yellow)") } else { String::new() };
                        self.set_status(format!("Transcription finished: {n} words{extra}."));
                    }
                    Err(e) => self.set_status(format!("❌ Transcription: {e}")),
                },
                JobMsg::ModelDownloaded(r) => match r {
                    Ok(p) => self.set_status(format!("Model saved: {}", p.display())),
                    Err(e) => self.set_status(format!("❌ Model download: {e}")),
                },
                JobMsg::Exported { label, result } => match result {
                    Ok(p) => {
                        self.export_ui.finished.push((label, p.clone()));
                        self.set_status(format!("✅ Exported: {}", p.display()));
                    }
                    Err(e) => self.set_status(format!("❌ Export {label}: {e}")),
                },
            }
        }
    }

    // -----------------------------------------------------------------------
    // Editing actions
    // -----------------------------------------------------------------------

    fn apply_silence(&mut self, clips: &[Id]) -> usize {
        let (pad, min) = (self.settings.silence_pad, self.settings.silence_min);
        clips.iter().map(|&c| self.project.apply_silence_removal(c, pad, min)).sum()
    }

    /// Removes silence from the given clips (starts the analysis first if needed).
    pub fn remove_silence(&mut self, clips: Vec<Id>) {
        let mut ready = Vec::new();
        for c in clips {
            let Some(clip) = self.project.clip(c) else { continue };
            let source = clip.source;
            match self.project.media(source) {
                Some(m) if m.silences.is_some() => ready.push(c),
                Some(m) if m.has_audio => {
                    if !self.pending_silence.contains(&c) {
                        self.pending_silence.push(c);
                    }
                    self.start_silence_detection(source);
                }
                _ => {}
            }
        }
        if !ready.is_empty() {
            self.checkpoint();
            let before = self.project.duration();
            let n = self.apply_silence(&ready);
            let saved = before - self.project.duration();
            self.set_status(format!("🔇 {n} pauses removed – {saved:.1} s shorter."));
        } else if !self.pending_silence.is_empty() {
            self.set_status("Analyzing silence – it will be removed automatically afterwards …");
        }
    }

    pub fn remove_silence_all(&mut self) {
        let ids = self.project.clips.iter().map(|c| c.id).collect();
        self.remove_silence(ids);
    }

    pub fn restore_silence(&mut self, clips: Vec<Id>) {
        self.checkpoint();
        for c in clips {
            if let Some(clip) = self.project.clip_mut(c) {
                clip.clear_cuts(Some(CutKind::Silence));
            }
        }
        self.project.repack_main();
    }

    pub fn remove_fillers_all(&mut self) {
        self.checkpoint();
        let ids: Vec<Id> = self.project.clips.iter().map(|c| c.id).collect();
        let (mut n, mut unsure) = (0, 0);
        for c in ids {
            let fillers = self.fillers_for_clip(c);
            let (a, b) = self.project.remove_fillers(c, &fillers);
            n += a;
            unsure += b;
        }
        if n == 0 {
            self.drop_checkpoint();
        }
        let hint = if unsure > 0 { format!(" {unsure} uncertain ones (underlined in yellow) were kept – please check them yourself.") } else { String::new() };
        let none = if n == 0 && unsure == 0 && self.settings.verbatim {
            " Tip: older transcripts lack filler words – re-transcribe the media (📝)."
        } else if n == 0 && unsure == 0 {
            " Tip: enable \"Transcribe filler words too (verbatim)\" in Settings and re-transcribe."
        } else {
            ""
        };
        self.set_status(format!("{n} filler words removed.{hint}{none}"));
    }

    /// Filler language of a clip's media (detected by Whisper or configured).
    pub fn clip_filler_language(&self, clip: Id) -> String {
        let detected = self
            .project
            .clip(clip)
            .and_then(|c| self.project.media(c.source))
            .and_then(|m| m.transcript.as_ref())
            .map(|t| t.language.clone())
            .unwrap_or_default();
        self.settings.filler_language(&detected)
    }

    fn fillers_for_clip(&self, clip: Id) -> Vec<String> {
        self.settings.filler_list(&self.clip_filler_language(clip))
    }

    pub fn delete_phrase_all(&mut self, phrase: &str) {
        self.checkpoint();
        let ids: Vec<Id> = self.project.clips.iter().map(|c| c.id).collect();
        let n: usize = ids.iter().map(|&c| self.project.delete_phrase(c, phrase)).sum();
        if n == 0 {
            self.drop_checkpoint();
        }
        self.set_status(format!("\"{phrase}\": {n} occurrences deleted."));
    }

    /// Deletes the selected words – or restores them if they
    /// are already all deleted.
    pub fn toggle_selected_words(&mut self) {
        let Some(sel) = &self.sel else { return };
        let (clip_id, (a, b)) = (sel.clip, sel.range());
        let Some(clip) = self.project.clip(clip_id) else { return };
        let tokens = self.project.clip_tokens(clip);
        if b >= tokens.len() {
            return;
        }
        let fps = self.project.fps;
        let all_deleted = (a..=b).all(|i| clip.is_token_deleted(&tokens[i], fps));
        self.set_words_deleted(clip_id, a, b, !all_deleted);
    }

    pub fn set_words_deleted(&mut self, clip: Id, a: usize, b: usize, deleted: bool) {
        self.checkpoint();
        self.project.set_tokens_deleted(clip, a, b, deleted);
        self.set_status(if deleted { "✂ Text cut" } else { "↺ Text restored" });
        if deleted {
            self.sel = None;
        }
    }

    pub fn split_at_playhead(&mut self) {
        let t = self.playhead;
        let fps = self.project.fps;
        let targets: Vec<Id> = match self.selected_clip.and_then(|id| self.project.clip(id)) {
            Some(c) if t > c.start && t < c.end(fps) => vec![c.id],
            _ => self.project.clips.iter().filter(|c| t > c.start && t < c.end(fps)).map(|c| c.id).collect(),
        };
        if targets.is_empty() {
            self.set_status("No clip under the playhead.");
            return;
        }
        self.checkpoint();
        let mut n = 0;
        for id in targets {
            if let Some(new) = self.project.split_clip(id, t) {
                n += 1;
                self.selected_clip = Some(new);
            }
        }
        if n == 0 {
            self.drop_checkpoint();
        } else {
            self.set_status(format!("✂ {n} clip(s) split."));
        }
    }

    pub fn delete_selected_clip(&mut self) {
        if let Some(id) = self.selected_clip.take() {
            self.checkpoint();
            self.project.delete_clip(id);
            self.sel = None;
            self.set_status("Clip deleted.");
        }
    }

    pub fn duplicate_selected_clip(&mut self) {
        if let Some(id) = self.selected_clip {
            self.checkpoint();
            self.selected_clip = self.project.duplicate_clip(id);
        }
    }

    pub fn remove_media(&mut self, media: Id) {
        self.checkpoint();
        self.project.clips.retain(|c| c.source != media);
        self.project.media.retain(|m| m.id != media);
        self.project.repack_main();
        self.selected_clip = self.selected_clip.filter(|c| self.project.clip(*c).is_some());
    }

    // -----------------------------------------------------------------------
    // Playback
    // -----------------------------------------------------------------------

    pub fn preview_size(&self) -> (u32, u32) {
        preview_size(&self.project, self.settings.preview_height)
    }

    pub fn is_playing(&self) -> bool {
        self.playback.is_some()
    }

    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.stop_playback();
        } else {
            self.start_playback();
        }
    }

    pub fn start_playback(&mut self) {
        let dur = self.project.duration();
        if dur <= 0.0 {
            return;
        }
        if self.playhead >= dur - self.project.frame() {
            self.playhead = 0.0;
        }
        if self.audio_out.is_none() && !self.audio_failed {
            match rodio::OutputStream::try_default() {
                Ok(o) => self.audio_out = Some(o),
                Err(e) => {
                    self.audio_failed = true;
                    self.set_status(format!("⚠ No audio device: {e}"));
                }
            }
        }
        // Shorter windows with subtitles so that frames are ready quickly.
        let window = if self.project.subtitles.burn_in { 60.0 } else { 300.0 };
        let to = (self.playhead + window).min(dur);
        let handle = self.audio_out.as_ref().map(|(_, h)| h);
        let size = self.preview_size();
        let with_video = !self.project.is_audio();
        match Playback::start(&self.tools().ffmpeg, &self.project, self.playhead, to, size, self.settings.preview_fps, handle, with_video) {
            Ok(pb) => {
                self.playback = Some(pb);
                self.playback_rev = self.revision;
            }
            Err(e) => self.set_status(format!("❌ Playback: {e}")),
        }
    }

    pub fn stop_playback(&mut self) {
        if let Some(mut pb) = self.playback.take() {
            pb.stop();
        }
        self.scrub_key = None;
    }

    pub fn seek(&mut self, t: f64) {
        let t = snap(t.clamp(0.0, self.project.duration().max(0.0)), self.project.fps);
        self.playhead = t;
        if self.is_playing() {
            self.stop_playback();
            self.start_playback();
        }
    }

    fn update_preview(&mut self, ctx: &egui::Context) {
        // Changes during playback → restart from the current position.
        if self.is_playing() && self.playback_rev != self.revision {
            self.stop_playback();
            self.start_playback();
        }
        if let Some(pb) = &mut self.playback {
            let (frame, t, finished) = pb.update();
            let to = pb.to;
            let err = pb.error.lock().ok().and_then(|e| e.clone());
            self.playhead = t;
            if let Some(f) = frame {
                self.upload_frame(ctx, f);
            }
            if finished {
                self.stop_playback();
                if to < self.project.duration() - self.project.frame() {
                    self.playhead = to;
                    self.start_playback();
                } else if let Some(e) = err.filter(|_| self.preview_tex.is_none()) {
                    self.set_status(format!("❌ Playback: {e}"));
                }
            }
            ctx.request_repaint();
            return;
        }
        if self.project.is_audio() {
            self.preview_tex = None;
            return;
        }
        let size = self.preview_size();
        let key = (self.revision, (self.playhead * 1000.0).round() as u64, size);
        if self.scrub_key != Some(key) {
            self.scrub_key = Some(key);
            if self.project.duration() > 0.0 {
                let t = self.playhead.min(self.project.duration() - self.project.frame()).max(0.0);
                self.scrub.request(&self.project, t, size);
            } else {
                self.preview_tex = None;
            }
        }
        match self.scrub.poll() {
            Some(Ok(f)) => {
                self.preview_error = None;
                self.upload_frame(ctx, f);
            }
            Some(Err(e)) => self.preview_error = Some(e),
            None => {}
        }
    }

    fn upload_frame(&mut self, ctx: &egui::Context, f: crate::preview::Frame) {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
        match &mut self.preview_tex {
            Some(t) => t.set(img, egui::TextureOptions::LINEAR),
            None => self.preview_tex = Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR)),
        }
    }

    // -----------------------------------------------------------------------
    // Project files
    // -----------------------------------------------------------------------

    pub fn new_project(&mut self, mode: ProjectMode) {
        self.stop_playback();
        self.project = Project::new(mode);
        self.show_start = false;
        self.project_path = None;
        self.undo.clear();
        self.redo.clear();
        self.selected_clip = None;
        self.sel = None;
        self.playhead = 0.0;
        self.preview_tex = None;
        self.dirty = false;
        self.revision += 1;
    }

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("TextCutForge project", &["tcf"]).pick_file() {
            self.open_project(&p);
        }
    }

    pub fn open_project(&mut self, path: &Path) {
        let loaded = std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|s| serde_json::from_str::<Project>(&s).map_err(|e| e.to_string()));
        match loaded {
            Ok(p) => {
                self.new_project(p.mode);
                let missing: Vec<String> =
                    p.media.iter().filter(|m| !m.path.is_file()).map(|m| m.path.display().to_string()).collect();
                let ids: Vec<Id> = p.media.iter().map(|m| m.id).collect();
                self.project = p;
                self.project_path = Some(path.to_path_buf());
                self.settings.add_recent(path.to_path_buf());
                self.settings.save();
                for id in ids {
                    self.start_waveform(id);
                }
                if missing.is_empty() {
                    self.set_status(format!("Project opened: {}", path.display()));
                } else {
                    self.set_status(format!("⚠ Missing files: {}", missing.join(", ")));
                }
                self.dirty = false;
            }
            Err(e) => self.set_status(format!("❌ Could not open project: {e}")),
        }
    }

    pub fn save(&mut self, save_as: bool) -> bool {
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let Some(mut p) = rfd::FileDialog::new()
                    .add_filter("TextCutForge project", &["tcf"])
                    .set_file_name(format!("{}.tcf", self.project.name))
                    .save_file()
                else {
                    return false;
                };
                if p.extension().is_none() {
                    p.set_extension("tcf");
                }
                p
            }
        };
        self.project.name = crate::util::file_stem(&path);
        match serde_json::to_string_pretty(&self.project).map_err(|e| e.to_string()).and_then(|s| {
            std::fs::write(&path, s).map_err(|e| e.to_string())
        }) {
            Ok(()) => {
                self.project_path = Some(path.clone());
                self.dirty = false;
                self.settings.add_recent(path.clone());
                self.settings.save();
                self.set_status(format!("💾 Saved: {}", path.display()));
                true
            }
            Err(e) => {
                self.set_status(format!("❌ Saving failed: {e}"));
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        let consume = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));

        if consume(cmd_shift(Key::Z)) || consume(cmd(Key::Y)) {
            self.redo();
        }
        if consume(cmd(Key::Z)) {
            self.undo();
        }
        if consume(cmd_shift(Key::S)) {
            self.save(true);
        }
        if consume(cmd(Key::S)) {
            self.save(false);
        }
        if consume(cmd(Key::O)) {
            self.open_dialog();
        }
        if consume(cmd(Key::I)) {
            self.import_dialog(Placement::Main);
        }
        if consume(cmd(Key::E)) {
            self.export_ui.open = true;
        }
        if ctx.wants_keyboard_input() || self.show_start {
            return;
        }
        let pressed = |k| ctx.input(|i| i.key_pressed(k));
        let shift = ctx.input(|i| i.modifiers.shift);
        let frame = self.project.frame();
        if pressed(Key::Space) {
            self.toggle_play();
        }
        if pressed(Key::S) || consume(cmd(Key::B)) {
            self.split_at_playhead();
        }
        if pressed(Key::Delete) || pressed(Key::Backspace) {
            if self.focus == Focus::Transcript && self.sel.is_some() {
                self.toggle_selected_words();
            } else {
                self.delete_selected_clip();
            }
        }
        if pressed(Key::Escape) {
            self.sel = None;
        }
        if pressed(Key::D) && ctx.input(|i| i.modifiers.command) {
            self.duplicate_selected_clip();
        }
        let step = if shift { 1.0 } else { frame };
        if pressed(Key::ArrowLeft) {
            self.seek(self.playhead - step);
        }
        if pressed(Key::ArrowRight) {
            self.seek(self.playhead + step);
        }
        if pressed(Key::Home) {
            self.seek(0.0);
        }
        if pressed(Key::End) {
            self.seek(self.project.duration());
        }
        if pressed(Key::J) {
            self.seek(self.playhead - 5.0);
        }
        if pressed(Key::L) {
            self.seek(self.playhead + 5.0);
        }
        if pressed(Key::K) {
            self.stop_playback();
        }
        if pressed(Key::Plus) || pressed(Key::Equals) {
            self.timeline_ui.zoom(1.25);
        }
        if pressed(Key::Minus) {
            self.timeline_ui.zoom(0.8);
        }
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        if self.confirm_close {
            egui::Window::new("Unsaved changes")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("The project has been modified. Save before quitting?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("💾 Save").clicked() && self.save(false) {
                            self.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("Discard").clicked() {
                            self.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_close = false;
                        }
                    });
                });
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_jobs();

        // Files via drag & drop
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.import_files(dropped, Placement::Main);
        }

        self.shortcuts(ctx);
        self.update_preview(ctx);

        let title = format!(
            "TextCutForge – {}{}",
            self.project.name,
            if self.dirty { " •" } else { "" }
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        ui::panels::menu_bar(self, ctx);
        ui::panels::status_bar(self, ctx);
        egui::TopBottomPanel::bottom("timeline")
            .resizable(true)
            .default_height(290.0)
            .min_height(160.0)
            .show(ctx, |ui| ui::timeline::show(self, ui));
        egui::SidePanel::left("left")
            .resizable(true)
            .default_width(300.0)
            .min_width(240.0)
            .show(ctx, |ui| ui::panels::left_panel(self, ui));
        egui::SidePanel::right("preview")
            .resizable(true)
            .default_width(560.0)
            .min_width(320.0)
            .show(ctx, |ui| ui::panels::preview_panel(self, ui));
        egui::CentralPanel::default().show(ctx, |ui| ui::transcript::show(self, ui));

        ui::dialogs::export_window(self, ctx);
        ui::dialogs::settings_window(self, ctx);
        ui::dialogs::project_settings_window(self, ctx);
        ui::dialogs::help_window(self, ctx);
        ui::dialogs::subtitle_window(self, ctx);
        ui::dialogs::start_window(self, ctx);
        // Undo gestures (sliders etc.) end as soon as the mouse is released.
        if !ctx.input(|i| i.pointer.any_down()) && !ctx.wants_keyboard_input() {
            self.inspector_editing = false;
        }
        self.handle_close(ctx);

        if !self.tasks.is_empty() || self.status_at.elapsed() < Duration::from_secs(1) {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        // Scrub results come from a thread without a repaint signal.
        ctx.request_repaint_after(Duration::from_millis(60));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.stop_playback();
        self.settings.save();
    }
}
