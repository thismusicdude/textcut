//! Dialogs: export, settings, project format, help.

use super::style;
use crate::app::{App, Placement, TaskKind};
use crate::export::{AspectHandling, AudioFormat, Codec, ExportJob, ExportKind, PRESETS};
use crate::project::ProjectMode;
use crate::transcribe::{LANGUAGES, MODELS, default_fillers};
use crate::util::{even, fmt_time};
use eframe::egui::{self, RichText};
use std::path::PathBuf;

pub struct ExportUi {
    pub open: bool,
    pub presets: Vec<bool>,
    pub custom: bool,
    pub custom_w: u32,
    pub custom_h: u32,
    pub codec: Codec,
    pub crf: u32,
    pub aspect: AspectHandling,
    /// Selected audio formats (same order as `AudioFormat::ALL`).
    pub audio: [bool; 4],
    pub kbps: u32,
    pub loudnorm: bool,
    pub srt: bool,
    pub dir: String,
    pub base_name: String,
    pub finished: Vec<(String, PathBuf)>,
    /// Mode for which the selection was last pre-filled.
    defaults_for: Option<ProjectMode>,
}

impl Default for ExportUi {
    fn default() -> Self {
        ExportUi {
            open: false,
            presets: vec![false; PRESETS.len()],
            custom: false,
            custom_w: 1920,
            custom_h: 1080,
            codec: Codec::H264,
            crf: 20,
            aspect: AspectHandling::AsClip,
            audio: [false; 4],
            kbps: 192,
            loudnorm: false,
            srt: false,
            dir: String::new(),
            base_name: String::new(),
            finished: Vec::new(),
            defaults_for: None,
        }
    }
}

impl ExportUi {
    /// Sensible default selection depending on the project mode.
    fn apply_defaults(&mut self, mode: ProjectMode) {
        if self.defaults_for == Some(mode) {
            return;
        }
        self.defaults_for = Some(mode);
        self.presets.iter_mut().for_each(|p| *p = false);
        self.audio = [false; 4];
        match mode {
            ProjectMode::Video => self.presets[0] = true,
            ProjectMode::Audio => {
                self.audio[0] = true;
                self.loudnorm = true;
            }
        }
    }

    fn count(&self, mode: ProjectMode) -> usize {
        let video = match mode {
            ProjectMode::Video => self.presets.iter().filter(|b| **b).count() + self.custom as usize,
            ProjectMode::Audio => 0,
        };
        video + self.audio.iter().filter(|b| **b).count() + self.srt as usize
    }
}

pub fn export_window(app: &mut App, ctx: &egui::Context) {
    if !app.export_ui.open {
        return;
    }
    let mode = app.project.mode;
    let mut open = true;
    let mut start = false;
    egui::Window::new("⬆ Export")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(540.0)
        .show(ctx, |ui| {
            let duration = app.project.duration();
            let (pw, ph) = (app.project.width, app.project.height);
            let e = &mut app.export_ui;
            e.apply_defaults(mode);
            if e.base_name.is_empty() {
                e.base_name = app.project.name.clone();
            }
            if e.dir.is_empty() {
                e.dir = default_export_dir(&app.settings.export_dir, app.project_path.as_ref());
            }
            match mode {
                ProjectMode::Video => ui.label(format!("Timeline: {} · Project {}×{}", fmt_time(duration), pw, ph)),
                ProjectMode::Audio => ui.label(format!("Audio project · {}", fmt_time(duration))),
            };

            if mode == ProjectMode::Video {
                ui.add_space(6.0);
                section(ui, "🎬 Video – sizes (multiple at once possible)");
                egui::Grid::new("presets").num_columns(2).show(ui, |ui| {
                    for (i, (name, _, w, _)) in PRESETS.iter().enumerate() {
                        let label = if *w == 0 { format!("{name} – {pw}×{ph}") } else { name.to_string() };
                        ui.checkbox(&mut e.presets[i], label);
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut e.custom, "Custom size:");
                    ui.add_enabled(e.custom, egui::DragValue::new(&mut e.custom_w).range(16..=7680).suffix(" px"));
                    ui.label("×");
                    ui.add_enabled(e.custom, egui::DragValue::new(&mut e.custom_h).range(16..=7680).suffix(" px"));
                });
                ui.horizontal(|ui| {
                    ui.label("Different aspect ratio:");
                    ui.radio_value(&mut e.aspect, AspectHandling::AsClip, "As in clip");
                    ui.radio_value(&mut e.aspect, AspectHandling::Fit, "Fit");
                    ui.radio_value(&mut e.aspect, AspectHandling::Fill, "Fill");
                });
                egui::Grid::new("codec").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Codec");
                    egui::ComboBox::from_id_salt("codec").selected_text(e.codec.label()).width(280.0).show_ui(ui, |ui| {
                        for c in Codec::ALL {
                            ui.selectable_value(&mut e.codec, c, c.label());
                        }
                    });
                    ui.end_row();
                    if matches!(e.codec, Codec::H264 | Codec::H265) {
                        ui.label("Quality (CRF)");
                        ui.add(egui::Slider::new(&mut e.crf, 14..=32).text("lower = better"));
                        ui.end_row();
                    }
                });
            }

            ui.add_space(8.0);
            section(
                ui,
                if mode == ProjectMode::Video { "🎵 Export audio only (timeline soundtrack)" } else { "🎵 Audio formats" },
            );
            ui.horizontal_wrapped(|ui| {
                for (i, f) in AudioFormat::ALL.into_iter().enumerate() {
                    ui.checkbox(&mut e.audio[i], f.label());
                }
            });
            let lossy = AudioFormat::ALL.iter().zip(e.audio).any(|(f, on)| on && f.lossy());
            ui.add_enabled_ui(lossy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Bitrate (MP3/M4A)");
                    for k in [128, 192, 256, 320] {
                        ui.selectable_value(&mut e.kbps, k, format!("{k} kbit/s"));
                    }
                });
            });
            ui.checkbox(&mut e.loudnorm, "Normalize loudness (−16 LUFS, podcast/streaming standard)")
                .on_hover_text("Evens out the volume. Applies to all exports in this run, including videos.");

            ui.add_space(8.0);
            section(ui, "📝 Subtitles / transcript");
            if mode == ProjectMode::Video {
                ui.horizontal(|ui| {
                    let mut burn = app.project.subtitles.burn_in;
                    if ui.checkbox(&mut burn, "💬 Burn subtitles into video").changed() {
                        let mut snapshot = app.project.clone();
                        snapshot.subtitles.burn_in = !burn;
                        app.push_undo(snapshot);
                        app.project.subtitles.burn_in = burn;
                    }
                    if ui.button("Style …").clicked() {
                        app.show_subtitles = true;
                    }
                });
            }
            let e = &mut app.export_ui;
            ui.checkbox(&mut e.srt, "SRT subtitles as file (timing matches the cuts)");

            ui.add_space(8.0);
            egui::Grid::new("target").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Folder");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut e.dir).desired_width(280.0));
                    if ui.button("…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&e.dir).pick_folder() {
                            e.dir = d.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("File name");
                ui.add(egui::TextEdit::singleline(&mut e.base_name).desired_width(280.0));
                ui.end_row();
            });

            let count = e.count(mode);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let enabled = count > 0 && duration > 0.0;
                let btn = egui::Button::new(RichText::new(format!("⬆ Export {count} file(s)")).strong()).fill(style::ACCENT_DIM);
                if ui.add_enabled(enabled, btn).clicked() {
                    start = true;
                }
                if duration <= 0.0 {
                    ui.label(RichText::new("Timeline is empty").color(style::WARN));
                }
            });

            let running: Vec<_> = app.tasks.iter().filter(|t| t.kind == TaskKind::Export).collect();
            if !running.is_empty() {
                ui.separator();
                for t in running {
                    ui.horizontal(|ui| {
                        ui.add(egui::ProgressBar::new(t.progress).show_percentage().desired_width(220.0));
                        ui.label(&t.label);
                        if let Some(c) = &t.cancel {
                            if ui.small_button("✖").clicked() {
                                c.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                    });
                }
            }
            if !app.export_ui.finished.is_empty() {
                ui.separator();
                ui.label(RichText::new("Done:").strong());
                for (label, path) in app.export_ui.finished.iter().rev().take(8) {
                    ui.horizontal(|ui| {
                        ui.label(format!("✅ {label}"));
                        if ui.small_button("Show in folder").clicked() {
                            reveal(path);
                        }
                    });
                }
            }
        });
    app.export_ui.open = open;
    if start {
        let jobs = build_jobs(app);
        app.settings.export_dir = app.export_ui.dir.clone();
        app.settings.save();
        app.queue_exports(jobs);
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.separator();
    ui.label(RichText::new(title).strong());
}

fn build_jobs(app: &App) -> Vec<ExportJob> {
    let e = &app.export_ui;
    let mode = app.project.mode;
    let dir = PathBuf::from(&e.dir);
    let base = if e.base_name.trim().is_empty() { "export".to_string() } else { e.base_name.trim().to_string() };
    let loudnorm = e.loudnorm;
    let mut jobs = Vec::new();
    if mode == ProjectMode::Video {
        let mut video = |label: String, slug: String, w: u32, h: u32| {
            jobs.push(ExportJob {
                label,
                kind: ExportKind::Video { width: w, height: h, codec: e.codec, crf: e.crf, aspect: e.aspect },
                path: dir.join(format!("{base}_{slug}.{}", e.codec.extension())),
                loudnorm,
            });
        };
        for (i, (name, slug, w, h)) in PRESETS.iter().enumerate() {
            if e.presets[i] {
                video(name.to_string(), slug.to_string(), *w, *h);
            }
        }
        if e.custom {
            let (w, h) = (even(e.custom_w as f64), even(e.custom_h as f64));
            video(format!("{w}×{h}"), format!("{w}x{h}"), w, h);
        }
    }
    for (format, on) in AudioFormat::ALL.into_iter().zip(e.audio) {
        if on {
            jobs.push(ExportJob {
                label: format!("Audio {}", format.label()),
                kind: ExportKind::Audio { format, kbps: e.kbps },
                path: dir.join(format!("{base}.{}", format.extension())),
                loudnorm,
            });
        }
    }
    if e.srt {
        jobs.push(ExportJob { label: "Subtitles".into(), kind: ExportKind::Subtitles, path: dir.join(format!("{base}.srt")), loudnorm });
    }
    jobs
}

fn default_export_dir(saved: &str, project: Option<&PathBuf>) -> String {
    if !saved.is_empty() {
        return saved.to_string();
    }
    if let Some(dir) = project.and_then(|p| p.parent()) {
        return dir.display().to_string();
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let movies = home.join("Movies");
    if movies.is_dir() { movies } else { home.join("Videos") }.display().to_string()
}

fn reveal(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}

/// "de – German"; "auto" reads as automatic detection.
fn language_name(code: &str) -> String {
    match LANGUAGES.iter().find(|(c, _)| *c == code) {
        Some((c, name)) => format!("{c} – {name}"),
        None if code == "auto" => "auto – detect".to_string(),
        None => code.to_string(),
    }
}

/// One filler list per language; the list matching each recording's language is used.
fn filler_editor(ui: &mut egui::Ui, s: &mut crate::settings::Settings) {
    let id = egui::Id::new("filler_lang");
    let fallback = if s.language == "auto" { "en".to_string() } else { s.language.clone() };
    let mut lang: String = ui.data(|d| d.get_temp(id)).unwrap_or(fallback);
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("filler_lang_combo").selected_text(language_name(&lang)).show_ui(ui, |ui| {
                for (code, _) in LANGUAGES {
                    ui.selectable_value(&mut lang, code.to_string(), language_name(code));
                }
            });
            if s.filler_words.contains_key(&lang) && ui.button("⟲ Default").on_hover_text(default_fillers(&lang)).clicked() {
                s.filler_words.remove(&lang);
            }
        });
        let mut text = s.filler_text(&lang);
        if ui.add(egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(300.0)).changed() {
            if text.trim() == default_fillers(&lang) {
                s.filler_words.remove(&lang);
            } else {
                s.filler_words.insert(lang.clone(), text);
            }
        }
        ui.label(
            RichText::new("Each recording uses the list of its language (detected by Whisper). Other languages: hm, hmm, uh, um …")
                .small()
                .weak(),
        );
    });
    ui.data_mut(|d| d.insert_temp(id, lang));
}

pub fn settings_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut download = false;
    let mut retranscribe_hint = false;
    let mut retranscribe_all = false;
    egui::Window::new("⚙ Settings").open(&mut open).default_width(560.0).show(ctx, |ui| {
        let s = &mut app.settings;
        ui.label(RichText::new("📝 Speech recognition (whisper.cpp)").strong());
        let found = s.whisper_binary();
        egui::Grid::new("whisper").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("whisper-cli");
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut s.whisper).hint_text("search automatically").desired_width(300.0));
                match &found {
                    Some(p) if p.is_file() => ui.label(RichText::new(format!("✅ {}", p.display())).small().color(egui::Color32::LIGHT_GREEN)),
                    _ => ui.label(RichText::new(format!("❌ Not found – install: {}", crate::util::install_hint("whisper"))).small().color(style::WARN)),
                };
            });
            ui.end_row();
            ui.label("Model");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("model").selected_text(&s.model).show_ui(ui, |ui| {
                    for (name, mb) in MODELS {
                        ui.selectable_value(&mut s.model, name.to_string(), format!("{name} (~{mb} MB)"));
                    }
                });
                let exists = s.model_file().is_file();
                if exists {
                    ui.label(RichText::new("✅ available").color(egui::Color32::LIGHT_GREEN));
                } else if app.tasks.iter().any(|t| t.kind == TaskKind::Download) {
                    ui.spinner();
                } else if ui.button("⬇ Download").clicked() {
                    download = true;
                }
            });
            ui.end_row();
            ui.label("Custom model file");
            ui.add(egui::TextEdit::singleline(&mut s.model_path).hint_text("optional: path to ggml-*.bin").desired_width(300.0));
            ui.end_row();
            ui.label("Language");
            egui::ComboBox::from_id_salt("lang").selected_text(language_name(&s.language)).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.language, "auto".to_string(), language_name("auto"));
                for (code, _) in LANGUAGES {
                    ui.selectable_value(&mut s.language, code.to_string(), language_name(code));
                }
            });
            ui.end_row();
            ui.label("Threads");
            ui.add(egui::Slider::new(&mut s.threads, 1..=16));
            ui.end_row();
            ui.label("On import");
            ui.vertical(|ui| {
                ui.checkbox(&mut s.auto_transcribe, "transcribe automatically");
                let r = ui.checkbox(&mut s.verbatim, "Transcribe filler words too (verbatim)").on_hover_text(
                    "Whisper normally omits \"uh\", \"um\", repetitions and slips of the tongue.\n\
                     With this option they are transcribed and can be cut out.\n\
                     For de/en/fr/es/it/nl. Words with barely any audio are marked yellow as uncertain.",
                );
                if r.changed() {
                    retranscribe_hint = true;
                }
                ui.checkbox(&mut s.auto_detect_silence, "analyze silence automatically");
            });
            ui.end_row();
        });

        if !app.project.media.is_empty() && ui.button("📝 Re-transcribe all media").on_hover_text("Needed after changing model, language or \"verbatim\"").clicked() {
            retranscribe_all = true;
        }

        ui.separator();
        ui.label(RichText::new("🔇 Silence detection").strong());
        egui::Grid::new("silence").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Threshold");
            ui.add(egui::Slider::new(&mut s.silence_db, -70.0..=-15.0).suffix(" dB"))
                .on_hover_text("Anything quieter than this level counts as silence. Raise it if there is background noise (e.g. -30 dB).");
            ui.end_row();
            ui.label("Minimum duration");
            ui.add(egui::Slider::new(&mut s.silence_min, 0.2..=3.0).suffix(" s"));
            ui.end_row();
            ui.label("Keep padding");
            ui.add(egui::Slider::new(&mut s.silence_pad, 0.0..=0.5).suffix(" s"))
                .on_hover_text("This much pause is kept before/after speech so cuts sound natural.");
            ui.end_row();
            ui.label("Filler words");
            filler_editor(ui, s);
            ui.end_row();
        });
        ui.label(RichText::new("After changing threshold/minimum duration: \"Text & Silence → Re-analyze silence\".").small().weak());

        ui.separator();
        ui.label(RichText::new("🎬 Preview & programs").strong());
        egui::Grid::new("misc").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Preview frame rate");
            ui.add(egui::Slider::new(&mut s.preview_fps, 10..=60).suffix(" fps"));
            ui.end_row();
            ui.label("Transcript text size");
            ui.add(egui::Slider::new(&mut s.text_size, 12.0..=28.0));
            ui.end_row();
            ui.label("ffmpeg");
            ui.add(egui::TextEdit::singleline(&mut s.ffmpeg).hint_text("automatic").desired_width(300.0));
            ui.end_row();
            ui.label("ffprobe");
            ui.add(egui::TextEdit::singleline(&mut s.ffprobe).hint_text("automatic").desired_width(300.0));
            ui.end_row();
        });
        ui.add_space(4.0);
        if ui.button("💾 Save").clicked() {
            s.save();
        }
    });
    if download {
        app.download_model();
    }
    if retranscribe_hint && !app.project.media.is_empty() {
        app.set_status("Applies to new transcriptions – \"📝 Re-transcribe all media\" applies it to existing ones.");
    }
    if retranscribe_all {
        app.settings.save();
        let ids: Vec<crate::project::Id> = app.project.media.iter().filter(|m| m.has_audio).map(|m| m.id).collect();
        for id in ids {
            app.start_transcription(id);
        }
    }
    if !open {
        app.settings.save();
    }
    app.show_settings = open;
}

pub fn project_settings_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_project_settings {
        return;
    }
    let mut open = true;
    egui::Window::new("Project format").open(&mut open).show(ctx, |ui| {
        let (mut w, mut h, mut fps) = (app.project.width, app.project.height, app.project.fps);
        ui.label("Canvas (preview & \"project size\" export)");
        ui.horizontal_wrapped(|ui| {
            for (label, pw, ph) in [
                ("16:9 1080p", 1920, 1080),
                ("16:9 4K", 3840, 2160),
                ("9:16 Portrait", 1080, 1920),
                ("1:1", 1080, 1080),
                ("4:5", 1080, 1350),
            ] {
                if ui.button(label).clicked() {
                    (w, h) = (pw, ph);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut w).range(16..=7680).suffix(" px"));
            ui.label("×");
            ui.add(egui::DragValue::new(&mut h).range(16..=7680).suffix(" px"));
        });
        ui.horizontal(|ui| {
            ui.label("Frame rate");
            for f in [24, 25, 30, 50, 60] {
                ui.selectable_value(&mut fps, f, format!("{f}"));
            }
        });
        if (w, h, fps) != (app.project.width, app.project.height, app.project.fps) {
            app.checkpoint();
            app.project.width = even(w as f64);
            app.project.height = even(h as f64);
            app.project.fps = fps;
            app.project.canvas_auto = false;
            app.project.repack_main();
        }
    });
    app.show_project_settings = open;
}

pub fn help_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_help {
        return;
    }
    let mut open = true;
    egui::Window::new("Help").open(&mut open).default_width(520.0).show(ctx, |ui| {
        ui.label(RichText::new("How TextCut works").strong().size(16.0));
        ui.label("0. On startup, choose a video or audio project (File → New project …). Audio mode has audio tracks only; video files contribute their audio.");
        ui.label("1. Import media (or drag it into the window). It is transcribed and analyzed for silence automatically.");
        ui.label("2. Select words in the text editor and press Delete → that part is cut from the video. Press Delete again to restore it.");
        ui.label("3. \"Remove silence automatically\" cuts out all pauses. ⏸ marks in the text are pauses that you can also delete individually.");
        ui.label("4. Picture-in-picture: put a clip on V2 (Media → 🖼), set position/size in the inspector.");
        ui.label("5. Export: multiple sizes at once (e.g. 16:9 and 9:16) – and/or audio only as MP3/M4A/WAV/FLAC, optionally loudness-normalized.");
        ui.add_space(8.0);
        ui.label(RichText::new("Keyboard shortcuts").strong());
        egui::Grid::new("keys").num_columns(2).striped(true).show(ui, |ui| {
            for (k, d) in [
                ("Space", "Play / Pause"),
                ("Del / ⌫", "Cut/restore selected text or delete clip"),
                ("S  or  ⌘B", "Split at playhead"),
                ("⌘Z / ⇧⌘Z", "Undo / Redo"),
                ("⌘D", "Duplicate clip"),
                ("← / →", "One frame back / forward (with ⇧: 1 second)"),
                ("J / K / L", "5 s back / Stop / 5 s forward"),
                ("Home / End", "Start / End"),
                ("+ / −, ⌘+Scroll", "Zoom timeline"),
                ("⇧+Click", "Extend text selection"),
                ("Esc", "Clear selection"),
                ("⌘S / ⌘O / ⌘I / ⌘E", "Save / Open / Import / Export"),
            ] {
                ui.label(RichText::new(k).monospace());
                ui.label(d);
                ui.end_row();
            }
        });
    });
    app.show_help = open;
}

/// Start screen: choose a video or audio project.
pub fn start_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_start {
        return;
    }
    let can_cancel = !app.project.clips.is_empty() || !app.project.media.is_empty();
    let mut choice: Option<ProjectMode> = None;
    let mut open_dialog = false;
    let mut open_recent: Option<PathBuf> = None;
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("start")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("✂ TextCut").size(28.0).strong().color(style::ACCENT));
            ui.label(RichText::new("Edit video and audio like a text document").weak());
        });
        ui.add_space(14.0);
        ui.label(RichText::new("New project").strong().size(16.0));
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            let cards = [
                (
                    ProjectMode::Video,
                    "🎬",
                    "Video project",
                    "Edit videos via text, remove silence,\npicture-in-picture, audio tracks, export in\nmultiple sizes – plus audio export.",
                ),
                (
                    ProjectMode::Audio,
                    "🎧",
                    "Audio project",
                    "Podcasts, interviews, voiceover:\naudio only, edit via text, remove\nsilence, export as MP3/M4A/WAV/FLAC.",
                ),
            ];
            for (ui, (mode, icon, title, desc)) in cols.iter_mut().zip(cards) {
                let text = format!("{icon}\n{title}");
                let btn = egui::Button::new(RichText::new(text).size(22.0).strong()).min_size(egui::vec2(ui.available_width(), 110.0));
                if ui.add(btn).clicked() {
                    choice = Some(mode);
                }
                ui.label(RichText::new(desc).small().weak());
            }
        });
        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("📂 Open project …").clicked() {
                open_dialog = true;
            }
            if can_cancel && ui.button("Cancel").clicked() {
                cancel = true;
            }
        });
        if !app.settings.recent.is_empty() {
            ui.label(RichText::new("Recently opened").small().weak());
            for p in app.settings.recent.iter().take(5) {
                let name = crate::util::file_stem(p);
                if ui.link(name).on_hover_text(p.display().to_string()).clicked() {
                    open_recent = Some(p.clone());
                }
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Tip: just drag files into the window.").small().weak());
    });
    if let Some(mode) = choice {
        app.new_project(mode);
        app.import_dialog(Placement::Main);
    }
    if open_dialog {
        app.open_dialog();
    }
    if let Some(p) = open_recent {
        app.open_project(&p);
    }
    if cancel {
        app.show_start = false;
    }
}

/// Subtitle window: burn-in and style (changes show immediately in the preview).
pub fn subtitle_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_subtitles {
        return;
    }
    use crate::project::{SubPosition, SubtitleStyle};
    let mut open = true;
    let before = app.project.subtitles.clone();
    let mut st = before.clone();
    let mut jump_next = false;
    egui::Window::new("💬 Subtitles").open(&mut open).default_width(430.0).show(ctx, |ui| {
        ui.checkbox(&mut st.burn_in, RichText::new("Burn subtitles into video").strong())
            .on_hover_text("Affects preview and video export. The text follows all cuts.");
        ui.label(RichText::new("Text comes from the main track transcript; deleted words are omitted automatically.").small().weak());
        ui.add_enabled_ui(st.burn_in, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Preset:");
                if ui.button("Classic").clicked() {
                    st = SubtitleStyle { burn_in: true, font: st.font.clone(), ..Default::default() };
                }
                if ui.button("Social Media").clicked() {
                    st = SubtitleStyle {
                        burn_in: true,
                        font: st.font.clone(),
                        size: 7.0,
                        position: SubPosition::Bottom,
                        margin: 18.0,
                        highlight: true,
                        outline: 7.0,
                        uppercase: true,
                        max_chars: 18,
                        max_lines: 2,
                        ..Default::default()
                    };
                }
                if ui.button("Box").clicked() {
                    st = SubtitleStyle { burn_in: true, font: st.font.clone(), box_bg: true, outline: 0.0, ..Default::default() };
                }
            });
            ui.separator();
            egui::Grid::new("subs").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Font");
                ui.horizontal(|ui| {
                    let fonts = crate::subtitles::available_fonts();
                    let current = crate::subtitles::font_path(&st).map(|p| {
                        fonts.iter().find(|(_, f)| *f == p).map(|(n, _)| n.clone()).unwrap_or_else(|| crate::util::file_stem(&p))
                    });
                    egui::ComboBox::from_id_salt("font").selected_text(current.unwrap_or_else(|| "– none –".into())).width(170.0).show_ui(ui, |ui| {
                        for (name, path) in &fonts {
                            if ui.selectable_label(st.font == path.to_string_lossy(), name).clicked() {
                                st.font = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                    if ui.button("Other …").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Font", &["ttf", "otf", "ttc"]).pick_file() {
                            st.font = p.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("Size");
                ui.add(egui::Slider::new(&mut st.size, 2.0..=14.0).suffix(" % of height"));
                ui.end_row();
                ui.label("Position");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut st.position, SubPosition::Bottom, "Bottom");
                    ui.selectable_value(&mut st.position, SubPosition::Center, "Center");
                    ui.selectable_value(&mut st.position, SubPosition::Top, "Top");
                });
                ui.end_row();
                if st.position != SubPosition::Center {
                    ui.label("Edge margin");
                    ui.add(egui::Slider::new(&mut st.margin, 0.0..=40.0).suffix(" %"));
                    ui.end_row();
                }
                ui.label("Text color");
                ui.color_edit_button_srgb(&mut st.color);
                ui.end_row();
                ui.label("Current word");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.highlight, "highlight");
                    if st.highlight {
                        ui.color_edit_button_srgb(&mut st.highlight_color);
                    }
                });
                ui.end_row();
                ui.label("Outline");
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut st.outline, 0.0..=16.0).suffix(" px"));
                    ui.color_edit_button_srgb(&mut st.outline_color);
                });
                ui.end_row();
                ui.label("Background");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.box_bg, "Box");
                    if st.box_bg {
                        ui.add(egui::Slider::new(&mut st.box_opacity, 0.1..=1.0).text("Opacity"));
                    }
                });
                ui.end_row();
                ui.label("Case");
                ui.checkbox(&mut st.uppercase, "UPPERCASE");
                ui.end_row();
                ui.label("Characters per subtitle");
                ui.add(egui::Slider::new(&mut st.max_chars, 8..=80));
                ui.end_row();
                ui.label("Max. lines");
                ui.add(egui::Slider::new(&mut st.max_lines, 1..=3));
                ui.end_row();
            });
            if crate::subtitles::font_path(&st).is_none_or(|p| !p.is_file()) {
                ui.colored_label(style::WARN, "⚠ No font found – please choose a .ttf/.otf via \"Other …\".");
            }
            ui.add_space(4.0);
            if ui.button("⏭ Jump to next subtitle line").on_hover_text("So the preview shows a subtitle").clicked() {
                jump_next = true;
            }
        });
    });
    if st != before {
        if !app.inspector_editing {
            let mut snapshot = app.project.clone();
            snapshot.subtitles = before;
            app.push_undo(snapshot);
            app.inspector_editing = true;
        } else {
            app.touch();
        }
        app.project.subtitles = st;
    }
    if jump_next {
        let cues = crate::subtitles::cues(&app.project, app.project.subtitles.max_chars);
        let t = app.playhead + 0.01;
        match cues.iter().find(|c| c.start > t).or(cues.first()) {
            Some(c) => app.seek(c.start + 0.05),
            None => app.set_status("No subtitles – transcribe first."),
        }
    }
    app.show_subtitles = open;
}
