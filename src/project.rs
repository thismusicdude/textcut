//! Data model and cutting logic.
//!
//! Core idea: a clip references a source range `[src_in, src_out)` of a
//! media file. Text and silence cuts are stored as `Cut`s in source time.
//! The ranges actually played ("kept ranges") are the source range minus
//! the cuts, snapped to the project's frame grid. This keeps all cuts
//! non-destructive and restorable at any time.
//!

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

pub type Id = u64;

/// Pauses at least this long appear as their own token in the transcript.
pub const MIN_SILENCE_TOKEN: f64 = 0.3;

// ---------------------------------------------------------------------------
// Time ranges
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub start: f64,
    pub end: f64,
}

impl Range {
    pub fn new(a: f64, b: f64) -> Self {
        Range { start: a.min(b), end: a.max(b) }
    }

    pub fn len(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    pub fn intersect(&self, o: &Range) -> Option<Range> {
        let s = self.start.max(o.start);
        let e = self.end.min(o.end);
        (e > s).then_some(Range { start: s, end: e })
    }

    pub fn overlap(&self, o: &Range) -> f64 {
        self.intersect(o).map(|r| r.len()).unwrap_or(0.0)
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }

    pub fn mid(&self) -> f64 {
        (self.start + self.end) * 0.5
    }
}

/// `base` minus the union of all `holes`.
pub fn subtract(base: Range, holes: &[Range]) -> Vec<Range> {
    let mut holes: Vec<Range> = holes.iter().filter_map(|h| h.intersect(&base)).collect();
    holes.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = Vec::new();
    let mut cur = base.start;
    for h in holes {
        if h.start > cur {
            out.push(Range { start: cur, end: h.start });
        }
        cur = cur.max(h.end);
    }
    if cur < base.end {
        out.push(Range { start: cur, end: base.end });
    }
    out
}

/// Merges overlapping/adjacent ranges.
pub fn merge(mut ranges: Vec<Range>, join_gap: f64) -> Vec<Range> {
    ranges.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out: Vec<Range> = Vec::with_capacity(ranges.len());
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end + join_gap => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

pub fn snap(t: f64, fps: u32) -> f64 {
    let f = fps as f64;
    (t * f).round() / f
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// Core of the word according to DTW (without gaps to its neighbors). Limits
    /// the cut when deleting so neighboring words are never clipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<(f64, f64)>,
}

impl Word {
    pub fn range(&self) -> Range {
        Range::new(self.start, self.end)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transcript {
    pub language: String,
    pub words: Vec<Word>,
    /// Peak level per word in dB (parallel to `words`), from the waveform.
    #[serde(default)]
    pub levels_db: Vec<f32>,
    /// Median of the word levels - reference for "unsure".
    #[serde(default)]
    pub median_db: Option<f32>,
}

/// Words this much quieter than the rest of the speech are considered unsure
/// (Whisper occasionally invents words during noise/silence).
pub const UNCERTAIN_BELOW_MEDIAN_DB: f32 = 25.0;

impl Transcript {
    /// Measures the peak level of each word (on the times corrected at the
    /// pauses). `peaks`: 100 values/s, √-compressed (see `media::waveform`).
    pub fn measure_levels(&mut self, peaks: &[f32], silences: &[Range]) {
        if peaks.is_empty() {
            return;
        }
        let mut words = self.words.clone();
        let mut sorted_silences = silences.to_vec();
        sorted_silences.sort_by(|a, b| a.start.total_cmp(&b.start));
        refine_words(&mut words, &sorted_silences);
        self.levels_db = words
            .iter()
            .map(|w| {
                let a = ((w.start * 100.0).floor().max(0.0) as usize).min(peaks.len() - 1);
                let b = ((w.end * 100.0).ceil() as usize + 1).clamp(a + 1, peaks.len());
                let p = peaks[a..b].iter().copied().fold(0.0f32, f32::max);
                20.0 * (p * p).max(1e-5).log10()
            })
            .collect();
        let mut sorted = self.levels_db.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        self.median_db = sorted.get(sorted.len() / 2).copied();
    }

    pub fn is_uncertain(&self, word_index: usize) -> bool {
        match (self.levels_db.get(word_index), self.median_db) {
            (Some(l), Some(m)) => *l < m - UNCERTAIN_BELOW_MEDIAN_DB,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaSource {
    pub id: Id,
    pub path: PathBuf,
    pub name: String,
    pub kind: MediaKind,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_video: bool,
    pub has_audio: bool,
    #[serde(default)]
    pub transcript: Option<Transcript>,
    /// Silence ranges detected by ffmpeg (source time).
    #[serde(default)]
    pub silences: Option<Vec<Range>>,
    /// Peak levels, 100 values per second (0..1). Not saved.
    #[serde(skip)]
    pub waveform: Option<Arc<Vec<f32>>>,
}

impl MediaSource {
    pub fn aspect(&self) -> f64 {
        if self.width > 0 && self.height > 0 {
            self.width as f64 / self.height as f64
        } else {
            16.0 / 9.0
        }
    }

    pub fn peak_at(&self, t: f64) -> f32 {
        match &self.waveform {
            Some(w) if !w.is_empty() => {
                let i = ((t * 100.0) as usize).min(w.len() - 1);
                w[i]
            }
            _ => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tracks & clips
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub kind: TrackKind,
    pub name: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CutKind {
    Text,
    Silence,
    Manual,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Cut {
    pub range: Range,
    pub kind: CutKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FitMode {
    /// Whole image visible, black bars if needed.
    Fit,
    /// Fill the frame, overflow is cropped.
    Fill,
    /// Free position/size (picture-in-picture).
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipPreset {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    Center,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Transform {
    pub fit: FitMode,
    /// Top-left corner, normalized to the canvas (0..1).
    pub x: f32,
    pub y: f32,
    /// Width relative to the canvas width.
    pub scale: f32,
    pub opacity: f32,
    /// Border width in pixels relative to 1080p height.
    pub border: f32,
    pub border_color: [u8; 3],
}

impl Transform {
    pub fn full(fit: FitMode) -> Self {
        Transform { fit, x: 0.0, y: 0.0, scale: 1.0, opacity: 1.0, border: 0.0, border_color: [255, 255, 255] }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub source: Id,
    pub track: Id,
    /// Start time on the timeline.
    pub start: f64,
    pub src_in: f64,
    pub src_out: f64,
    #[serde(default)]
    pub cuts: Vec<Cut>,
    pub volume: f32,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
    pub transform: Transform,
}

impl Clip {
    /// Source ranges actually played, snapped to the frame grid.
    pub fn kept(&self, fps: u32) -> Vec<Range> {
        let base = Range::new(self.src_in, self.src_out);
        let holes: Vec<Range> = self.cuts.iter().map(|c| c.range).collect();
        let min = 0.5 / fps as f64;
        let snapped: Vec<Range> = subtract(base, &holes)
            .into_iter()
            .map(|r| Range { start: snap(r.start, fps), end: snap(r.end, fps) })
            .filter(|r| r.len() > min)
            .collect();
        merge(snapped, 1e-9)
    }

    pub fn duration(&self, fps: u32) -> f64 {
        self.kept(fps).iter().map(|r| r.len()).sum()
    }

    pub fn end(&self, fps: u32) -> f64 {
        self.start + self.duration(fps)
    }

    /// (Timeline start, source range) for each played piece.
    pub fn segments(&self, fps: u32) -> Vec<(f64, Range)> {
        let mut t = self.start;
        self.kept(fps)
            .into_iter()
            .map(|r| {
                let seg = (t, r);
                t += r.len();
                seg
            })
            .collect()
    }

    /// Local clip time (0 = clip start) → source time.
    pub fn local_to_source(&self, local: f64, fps: u32) -> f64 {
        let kept = self.kept(fps);
        let mut acc = 0.0;
        for r in &kept {
            if local < acc + r.len() {
                return r.start + (local - acc).max(0.0);
            }
            acc += r.len();
        }
        kept.last().map(|r| r.end).unwrap_or(self.src_in)
    }

    /// Source time → local clip time. If the time lies inside a cut, the
    /// start of the next kept piece is returned.
    pub fn source_to_local(&self, src: f64, fps: u32) -> f64 {
        let mut acc = 0.0;
        for r in self.kept(fps) {
            if src < r.start {
                return acc;
            }
            if src < r.end {
                return acc + (src - r.start);
            }
            acc += r.len();
        }
        acc
    }

    /// Fraction of a source range that is actually played (0..1).
    pub fn kept_fraction(&self, r: Range, fps: u32) -> f64 {
        let r = if r.len() < 0.04 { Range::new(r.mid() - 0.02, r.mid() + 0.02) } else { r };
        let kept: f64 = self.kept(fps).iter().map(|k| k.overlap(&r)).sum();
        kept / r.len()
    }

    /// Is a token considered deleted (struck through in the text)? What counts
    /// is the word core - exactly the range that deleting cuts. The full word
    /// span also contains the gaps to the neighbors and would otherwise remain
    /// largely "kept".
    pub fn is_token_deleted(&self, tok: &Token, fps: u32) -> bool {
        let r = tok.core.filter(|c| c.len() >= 0.02).unwrap_or(tok.range);
        self.kept_fraction(r, fps) < 0.5
    }

    pub fn add_cut(&mut self, r: Range, kind: CutKind) {
        let Some(r) = r.intersect(&Range::new(self.src_in, self.src_out)) else { return };
        if r.len() < 1e-3 {
            return;
        }
        self.cuts.push(Cut { range: r, kind });
        self.normalize_cuts();
    }

    /// Removes cuts (optionally only of one kind) within `r`.
    pub fn remove_cuts(&mut self, r: Range, kind: Option<CutKind>) {
        let mut out = Vec::new();
        for c in &self.cuts {
            if kind.is_some_and(|k| k != c.kind) {
                out.push(*c);
                continue;
            }
            for piece in subtract(c.range, &[r]) {
                if piece.len() > 1e-3 {
                    out.push(Cut { range: piece, kind: c.kind });
                }
            }
        }
        self.cuts = out;
    }

    pub fn clear_cuts(&mut self, kind: Option<CutKind>) {
        self.cuts.retain(|c| kind.is_some_and(|k| k != c.kind));
    }

    fn normalize_cuts(&mut self) {
        let mut out = Vec::new();
        for kind in [CutKind::Text, CutKind::Silence, CutKind::Manual] {
            let ranges = self.cuts.iter().filter(|c| c.kind == kind).map(|c| c.range).collect();
            out.extend(merge(ranges, 0.0).into_iter().map(|range| Cut { range, kind }));
        }
        self.cuts = out;
    }

    /// Kind of cut that creates a gap between two kept pieces.
    /// Text cuts take precedence over silence.
    pub fn cut_kind_in(&self, gap: Range) -> CutKind {
        let probe = Range::new(gap.start - 1e-3, gap.end + 1e-3);
        let kinds: Vec<CutKind> = self.cuts.iter().filter(|c| c.range.overlap(&probe) > 0.0).map(|c| c.kind).collect();
        if kinds.iter().all(|k| *k == CutKind::Silence) && !kinds.is_empty() { CutKind::Silence } else { CutKind::Text }
    }

    pub fn removed_time(&self, fps: u32) -> f64 {
        (self.src_out - self.src_in) - self.duration(fps)
    }
}

// ---------------------------------------------------------------------------
// Smooth transitions at cut points
// ---------------------------------------------------------------------------

/// Shape of the fade out/in. `x` runs from 0 (cut point) to 1 (full volume).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FadeCurve {
    Linear,
    #[default]
    Sine,
    SCurve,
    Fast,
    Slow,
}

impl FadeCurve {
    pub const ALL: [FadeCurve; 5] = [FadeCurve::Linear, FadeCurve::Sine, FadeCurve::SCurve, FadeCurve::Fast, FadeCurve::Slow];

    pub fn label(self) -> &'static str {
        match self {
            FadeCurve::Linear => "Linear",
            FadeCurve::Sine => "Sine (smooth, recommended)",
            FadeCurve::SCurve => "S-curve (gentle start & end)",
            FadeCurve::Fast => "Fast (logarithmic)",
            FadeCurve::Slow => "Slow (exponential)",
        }
    }

    /// Gain 0..1 for x in 0..1.
    pub fn eval(self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        match self {
            FadeCurve::Linear => x,
            FadeCurve::Sine => (x * std::f64::consts::FRAC_PI_2).sin(),
            FadeCurve::SCurve => (1.0 - (std::f64::consts::PI * x).cos()) / 2.0,
            FadeCurve::Fast => 1.0 - (1.0 - x) * (1.0 - x),
            FadeCurve::Slow => x * x,
        }
    }

    /// Matching curve of the ffmpeg filter `afade` (fade-out mirrors it).
    pub fn afade_curve(self) -> &'static str {
        match self {
            FadeCurve::Linear => "tri",
            FadeCurve::Sine => "qsin",
            FadeCurve::SCurve => "hsin",
            FadeCurve::Fast => "ipar",
            FadeCurve::Slow => "qua",
        }
    }

    /// The same curve as an ffmpeg expression.
    pub fn expr(self, x: &str) -> String {
        match self {
            FadeCurve::Linear => format!("({x})"),
            FadeCurve::Sine => format!("sin(({x})*PI/2)"),
            FadeCurve::SCurve => format!("((1-cos(PI*({x})))/2)"),
            FadeCurve::Fast => format!("(1-(1-({x}))*(1-({x})))"),
            FadeCurve::Slow => format!("(({x})*({x}))"),
        }
    }
}

/// Smooth audio transitions at cut points within a clip: the audio fades
/// out before the cut and back in after it (total length `ms`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CutTransition {
    pub enabled: bool,
    /// Total length in milliseconds (half fade-out, half fade-in).
    pub ms: f32,
    pub curve: FadeCurve,
    pub text_cuts: bool,
    pub silence_cuts: bool,
}

impl Default for CutTransition {
    fn default() -> Self {
        CutTransition { enabled: true, ms: 40.0, curve: FadeCurve::Sine, text_cuts: true, silence_cuts: true }
    }
}

impl CutTransition {
    pub fn applies_to(&self, kind: CutKind) -> bool {
        self.enabled
            && self.ms > 0.0
            && match kind {
                CutKind::Silence => self.silence_cuts,
                CutKind::Text | CutKind::Manual => self.text_cuts,
            }
    }
}

// ---------------------------------------------------------------------------
// Transitions between clips
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionStyle {
    /// Picture dissolves into the next one, audio crossfades.
    #[default]
    Dissolve,
    /// Fade out and in through black or silence.
    DipToBlack,
}

impl TransitionStyle {
    pub fn label(self) -> &'static str {
        match self {
            TransitionStyle::Dissolve => "Dissolve",
            TransitionStyle::DipToBlack => "Dip to black / silence",
        }
    }
}

/// Smooth transitions where two clips on the same track
/// directly adjoin each other.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipTransition {
    pub video: bool,
    pub audio: bool,
    /// Total length of the transition in milliseconds.
    pub ms: f32,
    pub style: TransitionStyle,
    /// Curve for the audio (the picture fades linearly).
    pub curve: FadeCurve,
}

impl Default for ClipTransition {
    fn default() -> Self {
        ClipTransition { video: false, audio: false, ms: 500.0, style: TransitionStyle::Dissolve, curve: FadeCurve::Sine }
    }
}

impl ClipTransition {
    pub fn applies_to(&self, kind: TrackKind) -> bool {
        self.ms > 0.0
            && match kind {
                TrackKind::Video => self.video,
                TrackKind::Audio => self.audio,
            }
    }
}

/// Treatment of a clip edge by a transition.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edge {
    /// How far the clip runs beyond its edge (material before the in point or
    /// after the out point, "handle").
    pub extend: f64,
    /// Length of the picture fade (opacity) at this edge.
    pub video_fade: f64,
    /// Length of the audio fade at this edge.
    pub audio_fade: f64,
}

impl Edge {
    pub fn is_some(&self) -> bool {
        self.video_fade > 0.0 || self.audio_fade > 0.0
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClipEdges {
    pub start: Edge,
    pub end: Edge,
    pub curve: FadeCurve,
}

// ---------------------------------------------------------------------------
// Subtitles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubPosition {
    #[default]
    Bottom,
    Center,
    Top,
}

/// Appearance of burned-in subtitles. Sizes relative to the video height so
/// they look the same at every export size.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleStyle {
    /// Burn in to preview and video export.
    pub burn_in: bool,
    /// Font file (.ttf/.otf/.ttc); empty = system default font.
    pub font: String,
    /// Font size in % of the video height.
    pub size: f32,
    pub position: SubPosition,
    /// Distance from the frame edge in % of the video height.
    pub margin: f32,
    pub color: [u8; 3],
    /// Highlight the currently spoken word in color.
    pub highlight: bool,
    pub highlight_color: [u8; 3],
    /// Outline width in pixels relative to 1080p.
    pub outline: f32,
    pub outline_color: [u8; 3],
    pub box_bg: bool,
    pub box_opacity: f32,
    pub uppercase: bool,
    /// At most this many characters per subtitle.
    pub max_chars: usize,
    pub max_lines: usize,
}

impl Default for SubtitleStyle {
    fn default() -> Self {
        SubtitleStyle {
            burn_in: false,
            font: String::new(),
            size: 5.5,
            position: SubPosition::Bottom,
            margin: 7.0,
            color: [255, 255, 255],
            highlight: false,
            highlight_color: [255, 210, 40],
            outline: 4.0,
            outline_color: [0, 0, 0],
            box_bg: false,
            box_opacity: 0.6,
            uppercase: false,
            max_chars: 42,
            max_lines: 2,
        }
    }
}

// ---------------------------------------------------------------------------
// Transcript tokens (words + pauses) for the text editor
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Silence,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range,
    pub text: String,
    /// Barely any audio under the word - probably invented by Whisper.
    pub uncertain: bool,
    /// Word core (see `Word::core`).
    pub core: Option<Range>,
}

// ---------------------------------------------------------------------------
// Project
// ---------------------------------------------------------------------------

/// Video project (with picture, picture-in-picture …) or pure audio project (podcast, voiceover).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectMode {
    #[default]
    Video,
    Audio,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub mode: ProjectMode,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Canvas size is taken from the first video automatically.
    #[serde(default)]
    pub canvas_auto: bool,
    pub media: Vec<MediaSource>,
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
    pub next_id: Id,
    #[serde(default)]
    pub transition: CutTransition,
    #[serde(default)]
    pub clip_transition: ClipTransition,
    #[serde(default)]
    pub subtitles: SubtitleStyle,
}

impl Default for Project {
    fn default() -> Self {
        Project::new(ProjectMode::Video)
    }
}

impl Project {
    pub fn new(mode: ProjectMode) -> Self {
        let audio = mode == ProjectMode::Audio;
        let mut p = Project {
            mode,
            name: if audio { "Untitled audio project" } else { "Untitled project" }.into(),
            width: 1920,
            height: 1080,
            // Audio: 10 ms grid for finer cuts.
            fps: if audio { 100 } else { 30 },
            canvas_auto: !audio,
            media: Vec::new(),
            tracks: Vec::new(),
            clips: Vec::new(),
            next_id: 1,
            transition: CutTransition::default(),
            clip_transition: ClipTransition::default(),
            subtitles: SubtitleStyle::default(),
        };
        if audio {
            p.add_track(TrackKind::Audio);
            p.add_track(TrackKind::Audio);
        } else {
            p.add_track(TrackKind::Video);
            p.add_track(TrackKind::Video);
            p.add_track(TrackKind::Audio);
        }
        p
    }

    pub fn is_audio(&self) -> bool {
        self.mode == ProjectMode::Audio
    }

    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn frame(&self) -> f64 {
        1.0 / self.fps as f64
    }

    pub fn media(&self, id: Id) -> Option<&MediaSource> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn media_mut(&mut self, id: Id) -> Option<&mut MediaSource> {
        self.media.iter_mut().find(|m| m.id == id)
    }

    pub fn clip(&self, id: Id) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: Id) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    pub fn track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Video tracks in compositing order (V1 at the bottom first).
    pub fn video_tracks(&self) -> Vec<&Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect()
    }

    pub fn audio_tracks(&self) -> Vec<&Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect()
    }

    /// Main track (V1, or A1 in audio mode): magnetic, clips sit
    /// back to back without gaps.
    pub fn main_track(&self) -> Id {
        let tracks = if self.is_audio() { self.audio_tracks() } else { self.video_tracks() };
        tracks.first().map(|t| t.id).unwrap_or(0)
    }

    pub fn add_track(&mut self, kind: TrackKind) -> Id {
        let id = self.alloc_id();
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        let name = match kind {
            TrackKind::Video => format!("V{n}"),
            TrackKind::Audio => format!("A{n}"),
        };
        self.tracks.push(Track { id, kind, name, muted: false, hidden: false });
        id
    }

    pub fn remove_track(&mut self, id: Id) -> bool {
        let Some(track) = self.track(id) else { return false };
        let kind = track.kind;
        let same_kind = self.tracks.iter().filter(|t| t.kind == kind).count();
        if same_kind <= 1 || id == self.main_track() {
            return false;
        }
        self.clips.retain(|c| c.track != id);
        self.tracks.retain(|t| t.id != id);
        true
    }

    pub fn clips_on(&self, track: Id) -> Vec<&Clip> {
        let mut v: Vec<&Clip> = self.clips.iter().filter(|c| c.track == track).collect();
        v.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.id.cmp(&b.id)));
        v
    }

    /// Clips in "document order": main track first, then the other tracks.
    pub fn clips_in_document_order(&self) -> Vec<Id> {
        let mut ids: Vec<Id> = Vec::new();
        let main = self.main_track();
        ids.extend(self.clips_on(main).iter().map(|c| c.id));
        for t in &self.tracks {
            if t.id != main {
                ids.extend(self.clips_on(t.id).iter().map(|c| c.id));
            }
        }
        ids
    }

    /// Closes gaps between the main track's clips (ripple).
    pub fn repack_main(&mut self) {
        let fps = self.fps;
        let main = self.main_track();
        let order: Vec<Id> = self.clips_on(main).iter().map(|c| c.id).collect();
        let mut t = 0.0;
        for id in order {
            if let Some(c) = self.clip_mut(id) {
                c.start = t;
                t += c.duration(fps);
            }
        }
    }

    pub fn track_end(&self, track: Id) -> f64 {
        self.clips_on(track).iter().map(|c| c.end(self.fps)).fold(0.0, f64::max)
    }

    pub fn duration(&self) -> f64 {
        self.clips.iter().map(|c| c.end(self.fps)).fold(0.0, f64::max)
    }

    /// Inserts a clip for a source. On the main track it is appended,
    /// on other tracks it is placed at `at`.
    pub fn add_clip(&mut self, source: Id, track: Id, at: f64) -> Option<Id> {
        let media = self.media(source)?.clone();
        let track_kind = self.track(track)?.kind;
        if media.kind == MediaKind::Audio && track_kind == TrackKind::Video {
            return None;
        }
        if self.canvas_auto && !self.is_audio() && media.has_video && media.width > 0 {
            self.width = media.width - media.width % 2;
            self.height = media.height - media.height % 2;
            if media.fps >= 20.0 && media.fps <= 120.0 {
                self.fps = media.fps.round() as u32;
            }
            self.canvas_auto = false;
        }
        let main = self.main_track();
        let transform = if track_kind == TrackKind::Video && track != main {
            let mut t = Transform::full(FitMode::Custom);
            t.scale = 0.3;
            t.border = 4.0;
            t
        } else {
            Transform::full(FitMode::Fit)
        };
        let id = self.alloc_id();
        let start = if track == main { self.track_end(main) } else { snap(at.max(0.0), self.fps) };
        self.clips.push(Clip {
            id,
            source,
            track,
            start,
            src_in: 0.0,
            src_out: snap(media.duration, self.fps).max(self.frame()),
            cuts: Vec::new(),
            volume: 1.0,
            muted: false,
            fade_in: 0.0,
            fade_out: 0.0,
            transform,
        });
        if transform.fit == FitMode::Custom {
            self.place_pip(id, PipPreset::BottomRight);
        }
        self.repack_main();
        Some(id)
    }

    /// Positions a picture-in-picture clip at a corner/center.
    pub fn place_pip(&mut self, clip_id: Id, preset: PipPreset) {
        let canvas_aspect = self.width as f64 / self.height.max(1) as f64;
        let Some(clip) = self.clip(clip_id) else { return };
        let aspect = self.media(clip.source).map(|m| m.aspect()).unwrap_or(16.0 / 9.0);
        let clip = self.clip_mut(clip_id).unwrap();
        let t = &mut clip.transform;
        t.fit = FitMode::Custom;
        let w = t.scale as f64;
        let h = w * canvas_aspect / aspect;
        let margin_x = 0.03;
        let margin_y = margin_x * canvas_aspect;
        let (x, y) = match preset {
            PipPreset::TopLeft => (margin_x, margin_y),
            PipPreset::TopRight => (1.0 - w - margin_x, margin_y),
            PipPreset::BottomLeft => (margin_x, 1.0 - h - margin_y),
            PipPreset::BottomRight => (1.0 - w - margin_x, 1.0 - h - margin_y),
            PipPreset::Center => ((1.0 - w) / 2.0, (1.0 - h) / 2.0),
        };
        t.x = x as f32;
        t.y = y as f32;
    }

    /// Splits a clip at timeline time `t`. Returns the ID of the new clip.
    pub fn split_clip(&mut self, id: Id, t: f64) -> Option<Id> {
        let fps = self.fps;
        let frame = self.frame();
        let clip = self.clip(id)?.clone();
        let local = snap(t - clip.start, fps);
        if local < frame || local > clip.duration(fps) - frame {
            return None;
        }
        let split_src = snap(clip.local_to_source(local, fps), fps);
        let new_id = self.alloc_id();
        let mut second = clip.clone();
        second.id = new_id;
        second.src_in = split_src;
        second.start = clip.start + local;
        second.fade_in = 0.0;
        second.cuts.retain(|c| c.range.end > split_src);
        let first = self.clip_mut(id)?;
        first.src_out = split_src;
        first.fade_out = 0.0;
        first.cuts.retain(|c| c.range.start < split_src);
        self.clips.push(second);
        self.repack_main();
        Some(new_id)
    }

    pub fn delete_clip(&mut self, id: Id) {
        self.clips.retain(|c| c.id != id);
        self.repack_main();
    }

    pub fn duplicate_clip(&mut self, id: Id) -> Option<Id> {
        let fps = self.fps;
        let mut copy = self.clip(id)?.clone();
        let new_id = self.alloc_id();
        let orig_end = copy.end(fps);
        copy.id = new_id;
        copy.start = if copy.track == self.main_track() { copy.start + 1e-6 } else { orig_end };
        self.clips.push(copy);
        self.repack_main();
        Some(new_id)
    }

    /// Transitions at the edges of a clip. With "Dissolve" the outgoing
    /// clip continues with material after its out point, the incoming one
    /// starts with material before its in point - centered on the cut point,
    /// so the timeline length does not change. If such material is missing
    /// (start/end of file), it fades through black/silence instead.
    pub fn clip_edges(&self, clip: &Clip) -> ClipEdges {
        let tr = self.clip_transition;
        let fps = self.fps;
        let mut edges = ClipEdges { curve: tr.curve, ..Default::default() };
        let Some(kind) = self.track(clip.track).map(|t| t.kind) else { return edges };
        if !tr.applies_to(kind) {
            return edges;
        }
        let on_track = self.clips_on(clip.track);
        let Some(pos) = on_track.iter().position(|c| c.id == clip.id) else { return edges };
        let touches = |a: &Clip, b: &Clip| (b.start - a.end(fps)).abs() < 0.5 / fps as f64;
        let half = (tr.ms as f64 / 2000.0).max(0.0);
        let frame = 1.0 / fps as f64;
        // Handle length for the transition a → b (rounded down to whole frames).
        let handle = |a: &Clip, b: &Clip| -> f64 {
            if tr.style != TransitionStyle::Dissolve {
                return 0.0;
            }
            let after_a = match (self.media(a.source), a.kept(fps).last()) {
                (Some(m), Some(r)) => m.duration - r.end,
                _ => 0.0,
            };
            let before_b = b.kept(fps).first().map(|r| r.start).unwrap_or(0.0);
            let h = half.min(after_a).min(before_b).max(0.0);
            (h * fps as f64 + 1e-6).floor() * frame
        };
        let is_video = kind == TrackKind::Video;
        let make = |h: f64, is_start: bool| -> Edge {
            if h >= frame {
                // Dissolve: both sides run on for h. The picture of the upper
                // (later) clip fades in, the audio of both clips crossfades.
                Edge { extend: h, video_fade: if is_start && is_video { 2.0 * h } else { 0.0 }, audio_fade: 2.0 * h }
            } else {
                Edge { extend: 0.0, video_fade: if is_video { half } else { 0.0 }, audio_fade: half }
            }
        };
        if pos > 0 && touches(on_track[pos - 1], clip) {
            edges.start = make(handle(on_track[pos - 1], clip), true);
        }
        if let Some(next) = on_track.get(pos + 1).filter(|n| touches(clip, n)) {
            edges.end = make(handle(clip, next), false);
        }
        edges
    }

    /// Like `Clip::segments`, but including the transition handles; directly
    /// adjoining pieces are merged.
    pub fn segments_with_edges(&self, clip: &Clip, edges: &ClipEdges) -> Vec<(f64, Range)> {
        let mut segs = clip.segments(self.fps);
        if segs.is_empty() {
            return segs;
        }
        if edges.start.extend > 0.0 {
            let (t0, r0) = segs[0];
            segs.insert(0, (t0 - edges.start.extend, Range::new(r0.start - edges.start.extend, r0.start)));
        }
        if edges.end.extend > 0.0 {
            let (tl, rl) = *segs.last().unwrap();
            segs.push((tl + rl.len(), Range::new(rl.end, rl.end + edges.end.extend)));
        }
        let mut merged: Vec<(f64, Range)> = Vec::with_capacity(segs.len());
        for (t, r) in segs {
            match merged.last_mut() {
                Some((pt, pr)) if (pr.end - r.start).abs() < 1e-6 && (*pt + pr.len() - t).abs() < 1e-6 => pr.end = r.end,
                _ => merged.push((t, r)),
            }
        }
        merged
    }

    /// Source time → timeline time for a clip.
    pub fn source_to_timeline(&self, clip: &Clip, src: f64) -> f64 {
        clip.start + clip.source_to_local(src, self.fps)
    }

    // -----------------------------------------------------------------------
    // Text-based editing
    // -----------------------------------------------------------------------

    /// Words and pauses of a clip in chronological order.
    pub fn clip_tokens(&self, clip: &Clip) -> Vec<Token> {
        let Some(src) = self.media(clip.source) else { return Vec::new() };
        let clip_range = Range::new(clip.src_in, clip.src_out);
        let mut tokens: Vec<Token> = Vec::new();
        let (words, silences) = analyze_source(src);
        for (i, w) in words.iter().enumerate() {
            if clip_range.contains(w.range().mid()) {
                let uncertain = src.transcript.as_ref().is_some_and(|t| t.is_uncertain(i));
                let core = w.core.map(|(a, b)| Range::new(a, b));
                tokens.push(Token { kind: TokenKind::Word, range: w.range(), text: w.text.clone(), uncertain, core });
            }
        }
        let silences: Vec<Range> = match silences {
            Some(s) => s,
            None => {
                // Without silence analysis: show large gaps between words as pauses.
                let mut gaps = Vec::new();
                for pair in tokens.windows(2) {
                    let g = Range::new(pair[0].range.end, pair[1].range.start);
                    if g.len() >= 1.0 {
                        gaps.push(g);
                    }
                }
                gaps
            }
        };
        for s in silences {
            if let Some(r) = s.intersect(&clip_range) {
                if r.len() >= MIN_SILENCE_TOKEN {
                    tokens.push(Token {
                        kind: TokenKind::Silence,
                        range: r,
                        text: format!("⏸ {:.1}s", r.len()),
                        uncertain: false,
                        core: None,
                    });
                }
            }
        }
        tokens.sort_by(|a, b| a.range.start.total_cmp(&b.range.start));
        tokens
    }

    /// Cut range for tokens `first..=last`: from the start of the first to the
    /// start of the following token, so the gap after it disappears too.
    pub fn token_cut_range(clip: &Clip, tokens: &[Token], first: usize, last: usize) -> Range {
        let (first, last) = (first.min(last), first.max(last));
        let start = tokens[first..=last].iter().map(|t| t.range.start).fold(f64::INFINITY, f64::min);
        let sel_end = tokens[first..=last].iter().map(|t| t.range.end).fold(f64::NEG_INFINITY, f64::max);
        let end = match tokens.get(last + 1) {
            Some(next) => next.range.start.max(sel_end),
            None => sel_end,
        };
        let start = if first == 0 { start.min(clip.src_in.max(start - 0.05)) } else { start };
        Range::new(start.max(clip.src_in), end.min(clip.src_out))
    }

    /// Cut range for *deleting*: like `token_cut_range`, but leaves a small
    /// buffer of the gap next to neighboring words. That way the smooth
    /// transition fades in/out within the silence and doesn't swallow quiet
    /// word onsets ("S" in "Sphäre") or endings.
    pub fn token_delete_range(clip: &Clip, tokens: &[Token], first: usize, last: usize) -> Range {
        const GUARD_BEFORE: f64 = 0.02;
        const GUARD_AFTER: f64 = 0.04;
        let (a, b) = (first.min(last), first.max(last));
        let r = Self::token_cut_range(clip, tokens, a, b);
        let is_word = |i: Option<usize>| i.and_then(|i| tokens.get(i)).is_some_and(|t| t.kind == TokenKind::Word);
        let mut start = r.start;
        let mut end = r.end;
        if is_word(a.checked_sub(1)) {
            // Don't start before the core of the first deleted word.
            start = (start + GUARD_BEFORE).max(tokens[a].core.map_or(f64::MIN, |c| c.start));
        }
        if is_word(Some(b + 1)) {
            // Don't cut past the core of the last deleted word -
            // quiet onsets like the "S" in "Sphäre" would otherwise fall into the cut.
            end = (end - GUARD_AFTER).min(tokens[b].core.map_or(f64::MAX, |c| c.end));
        }
        if end - start >= 0.05 { Range::new(start, end) } else { r }
    }

    /// Deletes (or restores) tokens `first..=last` of a clip.
    pub fn set_tokens_deleted(&mut self, clip_id: Id, first: usize, last: usize, deleted: bool) {
        let Some(clip) = self.clip(clip_id) else { return };
        let tokens = self.clip_tokens(clip);
        if tokens.is_empty() || first.max(last) >= tokens.len() {
            return;
        }
        let r = if deleted {
            Self::token_delete_range(clip, &tokens, first, last)
        } else {
            Self::token_cut_range(clip, &tokens, first, last)
        };
        let clip = self.clip_mut(clip_id).unwrap();
        if deleted {
            clip.add_cut(r, CutKind::Text);
        } else {
            clip.remove_cuts(r, None);
        }
        self.repack_main();
    }

    /// Removes detected silence from a clip. `padding` seconds remain at
    /// each edge so cuts sound natural.
    pub fn apply_silence_removal(&mut self, clip_id: Id, padding: f64, min_len: f64) -> usize {
        let Some(clip) = self.clip(clip_id) else { return 0 };
        let Some(silences) = self.media(clip.source).and_then(|m| analyze_source(m).1) else { return 0 };
        let clip_range = Range::new(clip.src_in, clip.src_out);
        let clip = self.clip_mut(clip_id).unwrap();
        clip.clear_cuts(Some(CutKind::Silence));
        let mut count = 0;
        for s in silences {
            let Some(s) = s.intersect(&clip_range) else { continue };
            if s.len() < min_len {
                continue;
            }
            let pad_start = if s.start <= clip_range.start + 0.01 { 0.0 } else { padding };
            let pad_end = if s.end >= clip_range.end - 0.01 { 0.0 } else { padding };
            let r = Range { start: s.start + pad_start, end: s.end - pad_end };
            if r.len() >= 0.05 {
                clip.add_cut(r, CutKind::Silence);
                count += 1;
            }
        }
        self.repack_main();
        count
    }

    /// Cuts out filler words ("äh", "ähm" …). Unsure matches (barely any audio)
    /// are kept. Returns (removed, unsure skipped).
    pub fn remove_fillers(&mut self, clip_id: Id, fillers: &[String]) -> (usize, usize) {
        let Some(clip) = self.clip(clip_id) else { return (0, 0) };
        let tokens = self.clip_tokens(clip);
        let fillers: Vec<String> = fillers.iter().map(|f| normalize_word(f)).filter(|f| !f.is_empty()).collect();
        let hits: Vec<usize> = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TokenKind::Word && fillers.contains(&normalize_word(&t.text)))
            .map(|(i, _)| i)
            .collect();
        let (sure, unsure): (Vec<usize>, Vec<usize>) = hits.into_iter().partition(|&i| !tokens[i].uncertain);
        for &i in &sure {
            self.set_tokens_deleted(clip_id, i, i, true);
        }
        (sure.len(), unsure.len())
    }

    /// Deletes all occurrences of a phrase (find & delete).
    pub fn delete_phrase(&mut self, clip_id: Id, phrase: &str) -> usize {
        let needle: Vec<String> = phrase.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
        if needle.is_empty() {
            return 0;
        }
        let Some(clip) = self.clip(clip_id) else { return 0 };
        let tokens = self.clip_tokens(clip);
        let words: Vec<(usize, String)> = tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TokenKind::Word)
            .map(|(i, t)| (i, normalize_word(&t.text)))
            .collect();
        let mut hits = Vec::new();
        let mut i = 0;
        while i + needle.len() <= words.len() {
            if words[i..i + needle.len()].iter().zip(&needle).all(|((_, w), n)| w == n) {
                hits.push((words[i].0, words[i + needle.len() - 1].0));
                i += needle.len();
            } else {
                i += 1;
            }
        }
        for &(a, b) in &hits {
            self.set_tokens_deleted(clip_id, a, b, true);
        }
        hits.len()
    }
}

/// Words and pauses of a source, aligned with each other:
/// word times are corrected at the pauses (`refine_words`), and pauses
/// separated only by a short noise without a word (breaths, lip smacks)
/// are merged into one pause. `None` if no silence analysis is
/// available yet.
pub fn analyze_source(src: &MediaSource) -> (Vec<Word>, Option<Vec<Range>>) {
    let mut words = src.transcript.as_ref().map(|t| t.words.clone()).unwrap_or_default();
    let Some(raw) = &src.silences else {
        refine_words(&mut words, &[]);
        return (words, None);
    };
    let mut silences = raw.clone();
    silences.sort_by(|a, b| a.start.total_cmp(&b.start));
    refine_words(&mut words, &silences);
    if src.transcript.is_some() {
        silences = merge_wordless_gaps(&silences, &words, MAX_BREATH_GAP);
    }
    (words, Some(silences))
}

/// Noises between two pauses that are at most this long and contain no
/// word count as part of the pause.
pub const MAX_BREATH_GAP: f64 = 1.0;

fn merge_wordless_gaps(silences: &[Range], words: &[Word], max_gap: f64) -> Vec<Range> {
    let mut out: Vec<Range> = Vec::with_capacity(silences.len());
    for s in silences {
        if let Some(last) = out.last_mut() {
            let gap = Range::new(last.end, s.start);
            if gap.len() <= max_gap && !has_word_in(words, gap) {
                last.end = last.end.max(s.end);
                continue;
            }
        }
        out.push(*s);
    }
    out
}

/// Is there (significantly) a word in the range? `words` is sorted by start.
fn has_word_in(words: &[Word], r: Range) -> bool {
    let idx = words.partition_point(|w| w.start < r.end);
    words[..idx].iter().rev().take(8).any(|w| w.range().overlap(&r) > 0.05 || (w.end - w.start < 0.05 && r.contains(w.start)))
}

/// Corrects inaccurate Whisper timestamps using the (sorted) pauses:
/// 1. No word starts or ends inside a pause - the boundary moves to the
///    pause edge (Whisper often places word starts too early).
/// 2. No word spans a pause - it goes to the side with more audio.
/// 3. Words with (almost) the same start time are distributed evenly.
pub fn refine_words(words: &mut [Word], silences: &[Range]) {
    let silences: Vec<Range> = silences.iter().copied().filter(|s| s.len() >= MIN_SILENCE_TOKEN).collect();
    for w in words.iter_mut() {
        // Only look at pauses that touch the word (pauses are sorted).
        let first = silences.partition_point(|s| s.end <= w.start);
        for s in &silences[first..] {
            if s.start >= w.end {
                break;
            }
            let starts_in = w.start >= s.start && w.start < s.end;
            let ends_in = w.end > s.start && w.end <= s.end;
            if starts_in && ends_in {
                // Entirely inside the pause: move to the nearer edge.
                let len = (w.end - w.start).clamp(0.05, 0.3);
                if s.end - w.end < w.start - s.start {
                    (w.start, w.end) = (s.end, s.end + len);
                } else {
                    (w.start, w.end) = (s.start - len, s.start);
                }
            } else if starts_in {
                w.start = s.end;
            } else if ends_in {
                w.end = s.start;
            } else if w.start < s.start && w.end > s.end {
                if w.end - s.end >= s.start - w.start {
                    w.start = s.end;
                } else {
                    w.end = s.start;
                }
            }
        }
        w.start = w.start.max(0.0);
    }
    for k in 1..words.len() {
        if words[k].start < words[k - 1].start {
            words[k].start = words[k - 1].start;
        }
        words[k].end = words[k].end.max(words[k].start);
    }
    let n = words.len();
    let mut i = 0;
    while i < n {
        let mut j = i + 1;
        while j < n && words[j].start - words[i].start < 0.02 {
            j += 1;
        }
        let count = j - i;
        if count > 1 {
            let start = words[i].start;
            let mut end = words[i..j].iter().map(|w| w.end).fold(start, f64::max).max(start + 0.12 * count as f64);
            if let Some(next) = words.get(j) {
                end = end.min(next.start);
            }
            let step = (end - start) / count as f64;
            for (k, w) in words[i..j].iter_mut().enumerate() {
                w.start = start + step * k as f64;
                w.end = start + step * (k + 1) as f64;
            }
        }
        i = j;
    }
}

/// Lowercase, without punctuation - for word comparisons.
pub fn normalize_word(w: &str) -> String {
    w.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn test_project() -> (Project, Id) {
        let mut p = Project::default();
        let id = p.alloc_id();
        p.media.push(MediaSource {
            id,
            path: "test.mp4".into(),
            name: "test".into(),
            kind: MediaKind::Video,
            duration: 10.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_video: true,
            has_audio: true,
            transcript: Some(Transcript {
                language: "de".into(),
                words: vec![
                    Word { text: "Hallo".into(), start: 0.5, end: 1.0, core: None },
                    Word { text: "ähm".into(), start: 1.1, end: 1.5, core: None },
                    Word { text: "Welt.".into(), start: 1.6, end: 2.0, core: None },
                    Word { text: "Das".into(), start: 4.0, end: 4.3, core: None },
                    Word { text: "ist".into(), start: 4.4, end: 4.6, core: None },
                    Word { text: "gut".into(), start: 4.7, end: 5.0, core: None },
                ],
                ..Default::default()
            }),
            silences: Some(vec![Range::new(2.0, 4.0), Range::new(5.0, 10.0)]),
            waveform: None,
        });
        (p, id)
    }

    #[test]
    fn subtract_and_merge() {
        let r = subtract(Range::new(0.0, 10.0), &[Range::new(2.0, 3.0), Range::new(2.5, 4.0), Range::new(9.0, 12.0)]);
        assert_eq!(r, vec![Range::new(0.0, 2.0), Range::new(4.0, 9.0)]);
        let m = merge(vec![Range::new(5.0, 6.0), Range::new(0.0, 1.0), Range::new(0.5, 2.0)], 0.0);
        assert_eq!(m, vec![Range::new(0.0, 2.0), Range::new(5.0, 6.0)]);
    }

    #[test]
    fn kept_ranges_are_frame_aligned() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let c = p.add_clip(src, main, 0.0).unwrap();
        let clip = p.clip_mut(c).unwrap();
        clip.add_cut(Range::new(1.01, 2.02), CutKind::Text);
        let kept = clip.kept(30);
        assert_eq!(kept.len(), 2);
        for r in &kept {
            assert!(approx((r.start * 30.0).round(), r.start * 30.0));
            assert!(approx((r.end * 30.0).round(), r.end * 30.0));
        }
        assert!(approx(clip.duration(30), 10.0 - (61.0 - 30.0) / 30.0));
    }

    #[test]
    fn time_mapping_roundtrip() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let c = p.add_clip(src, main, 0.0).unwrap();
        let clip = p.clip_mut(c).unwrap();
        clip.add_cut(Range::new(2.0, 4.0), CutKind::Silence);
        assert!(approx(clip.local_to_source(1.0, 30), 1.0));
        assert!(approx(clip.local_to_source(2.5, 30), 4.5));
        assert!(approx(clip.source_to_local(4.5, 30), 2.5));
        // Time inside a cut → start of the next piece.
        assert!(approx(clip.source_to_local(3.0, 30), 2.0));
    }

    #[test]
    fn deleting_words_cuts_video_and_ripples() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        let b = p.add_clip(src, main, 0.0).unwrap();
        assert!(approx(p.clip(b).unwrap().start, 10.0));

        let tokens = p.clip_tokens(p.clip(a).unwrap());
        let ahm = tokens.iter().position(|t| t.text == "ähm").unwrap();
        p.set_tokens_deleted(a, ahm, ahm, true);
        // "ähm" (1.1) to start of "Welt." (1.6), with 20/40 ms buffer to the neighboring words,
        // snapped to 30 fps: 1.1333–1.5667 → 0.4333 s.
        let expected = 10.0 - 13.0 / 30.0;
        assert!(approx(p.clip(a).unwrap().duration(30), expected), "{}", p.clip(a).unwrap().duration(30));
        assert!(approx(p.clip(b).unwrap().start, expected), "main track ripples");

        let tokens = p.clip_tokens(p.clip(a).unwrap());
        assert!(p.clip(a).unwrap().is_token_deleted(&tokens[ahm], 30));

        p.set_tokens_deleted(a, ahm, ahm, false);
        assert!(approx(p.clip(a).unwrap().duration(30), 10.0));
    }

    #[test]
    fn silence_removal_with_padding() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        let n = p.apply_silence_removal(a, 0.1, 0.5);
        assert_eq!(n, 2);
        // 2.0–4.0 with padding → 1.8 s removed; 5.0–10.0 at clip end → 4.9 s removed.
        let d = p.clip(a).unwrap().duration(30);
        assert!(approx(d, 10.0 - 1.8 - 4.9), "{d}");
        // Applying again replaces the old silence cuts.
        p.apply_silence_removal(a, 0.1, 0.5);
        assert!(approx(p.clip(a).unwrap().duration(30), d));
    }

    #[test]
    fn fillers_and_phrases() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        assert_eq!(p.remove_fillers(a, &["ähm".into(), "äh".into()]), (1, 0));
        assert_eq!(p.delete_phrase(a, "das IST"), 1);
        let clip = p.clip(a).unwrap();
        let tokens = p.clip_tokens(clip);
        let deleted: Vec<&str> =
            tokens.iter().filter(|t| clip.is_token_deleted(t, 30)).map(|t| t.text.as_str()).collect();
        assert_eq!(deleted, vec!["ähm", "Das", "ist"]);
    }

    #[test]
    fn split_keeps_total_duration() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        p.clip_mut(a).unwrap().add_cut(Range::new(2.0, 4.0), CutKind::Silence);
        p.repack_main();
        let before = p.duration();
        let b = p.split_clip(a, 3.0).unwrap();
        assert!(approx(p.duration(), before));
        let (ca, cb) = (p.clip(a).unwrap(), p.clip(b).unwrap());
        assert!(approx(ca.duration(30), 3.0));
        assert!(approx(cb.start, 3.0));
        assert!(approx(cb.src_in, 5.0));
    }

    #[test]
    fn audio_mode_uses_a1_as_magnetic_main_track() {
        let mut p = Project::new(ProjectMode::Audio);
        assert!(p.video_tracks().is_empty());
        let id = p.alloc_id();
        let (template, _) = test_project();
        let mut m = template.media[0].clone();
        m.id = id;
        p.media.push(m); // Video file: in audio mode only the audio counts
        let main = p.main_track();
        assert_eq!(p.track(main).unwrap().kind, TrackKind::Audio);
        let a = p.add_clip(id, main, 0.0).unwrap();
        let b = p.add_clip(id, main, 0.0).unwrap();
        assert_eq!(p.fps, 100);
        assert_eq!(p.clip(a).unwrap().transform.fit, FitMode::Fit);
        p.apply_silence_removal(a, 0.0, 0.5);
        assert!(approx(p.clip(b).unwrap().start, 3.0), "ripples: {}", p.clip(b).unwrap().start);
    }

    #[test]
    fn fade_curves_go_from_silent_to_full() {
        for c in FadeCurve::ALL {
            assert!(approx(c.eval(0.0), 0.0) && approx(c.eval(1.0), 1.0), "{c:?}");
            assert!(c.eval(0.5) > 0.0 && c.eval(0.5) < 1.0);
        }
        assert!(FadeCurve::Fast.eval(0.3) > FadeCurve::Linear.eval(0.3));
        assert!(FadeCurve::Slow.eval(0.3) < FadeCurve::Linear.eval(0.3));
    }

    #[test]
    fn whisper_timestamps_are_refined() {
        let w = |t: &str, s: f64, e: f64| Word { text: t.into(), start: s, end: e, core: None };
        // Whisper: words start in the pause 7.0–9.3 and share the start time.
        let mut words = vec![
            w("schneiden.", 6.1, 7.6),
            w("Im", 9.0, 9.0),
            w("und", 9.0, 9.1),
            w("stille", 9.0, 9.6),
            w("wird", 9.7, 9.9),
        ];
        refine_words(&mut words, &[Range::new(7.0, 9.3)]);
        assert!(approx(words[0].end, 7.0), "end at pause start: {}", words[0].end);
        for x in &words[1..4] {
            assert!(x.start >= 9.3 - 1e-9, "{} does not start in the pause: {}", x.text, x.start);
        }
        assert!(words[1].start < words[2].start && words[2].start < words[3].start, "distributed");
        assert!(words[3].end <= words[4].start + 1e-9);

        // A pause before it no longer clips the following word.
        let (mut p, src) = test_project();
        p.media[0].transcript.as_mut().unwrap().words[3].start = 3.5; // "Das" starts inside the pause 2–4 according to Whisper
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        p.apply_silence_removal(a, 0.1, 0.5);
        let clip = p.clip(a).unwrap();
        let das = p.clip_tokens(clip).into_iter().find(|t| t.text == "Das").unwrap();
        assert!(clip.kept_fraction(das.range, 30) > 0.9);
    }

    /// Real values from a recording: "Ozean. [pause - breath - pause] Wasserstoffeis".
    /// Whisper places "Wasserstoffeis" at 29.36–34.00, it is spoken from 32.96.
    #[test]
    fn word_spanning_pauses_and_breath_between_pauses() {
        let w = |t: &str, s: f64, e: f64| Word { text: t.into(), start: s, end: e, core: None };
        let mut p = Project::new(ProjectMode::Audio);
        let id = p.alloc_id();
        p.media.push(MediaSource {
            id,
            path: "x.m4a".into(),
            name: "x".into(),
            kind: MediaKind::Audio,
            duration: 40.0,
            width: 0,
            height: 0,
            fps: 0.0,
            has_video: false,
            has_audio: true,
            transcript: Some(Transcript {
                language: "de".into(),
                words: vec![
                    w("astralen", 27.12, 28.18),
                    w("Ozean.", 28.18, 29.36),
                    w("Wasserstoffeis", 29.36, 34.00),
                    w("klebt", 34.00, 34.35),
                ],
                ..Default::default()
            }),
            silences: Some(vec![Range::new(28.790567, 30.706122), Range::new(31.190091, 32.958662)]),
            waveform: None,
        });
        let main = p.main_track();
        let c = p.add_clip(id, main, 0.0).unwrap();
        let tokens = p.clip_tokens(p.clip(c).unwrap());
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["astralen", "Ozean.", "⏸ 4.2s", "Wasserstoffeis", "klebt"]);
        assert!((tokens[3].range.start - 32.958662).abs() < 1e-6);
        assert!((tokens[1].range.end - 28.790567).abs() < 1e-6);

        // Removing silence also takes the breath in between.
        p.apply_silence_removal(c, 0.0, 0.5);
        let clip = p.clip(c).unwrap();
        assert!(clip.removed_time(p.fps) > 4.1, "{}", clip.removed_time(p.fps));
        assert!(clip.kept_fraction(tokens[3].range, p.fps) > 0.99);

        // If a word lies in the noise, there remain two pauses.
        p.media[0].transcript.as_mut().unwrap().words.insert(2, w("Ja.", 30.75, 31.1));
        let tokens = p.clip_tokens(p.clip(c).unwrap());
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["astralen", "Ozean.", "⏸ 1.9s", "Ja.", "⏸ 1.8s", "Wasserstoffeis", "klebt"]);
    }

    #[test]
    fn quiet_words_are_uncertain_and_kept_by_filler_removal() {
        let (mut p, src) = test_project();
        // Waveform: speech everywhere (≈ −10 dB), only nearly silent at 1.1–1.5 s ("ähm").
        let mut peaks = vec![0.55f32; 1000];
        for v in &mut peaks[110..=150] {
            *v = 0.05;
        }
        let m = p.media_mut(src).unwrap();
        m.transcript.as_mut().unwrap().measure_levels(&peaks, &[]);
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        let tokens = p.clip_tokens(p.clip(a).unwrap());
        let unsure: Vec<&str> = tokens.iter().filter(|t| t.uncertain).map(|t| t.text.as_str()).collect();
        assert_eq!(unsure, vec!["ähm"]);
        assert_eq!(p.remove_fillers(a, &["ähm".into()]), (0, 1));
        assert!(approx(p.clip(a).unwrap().duration(30), 10.0), "nothing cut");
    }

    #[test]
    fn deleting_respects_word_core() {
        let (mut p, src) = test_project();
        // "ähm" lies only at 1.1–1.3 according to DTW; after that "Welt." quietly begins.
        p.media_mut(src).unwrap().transcript.as_mut().unwrap().words[1].core = Some((1.1, 1.3));
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        p.set_tokens_deleted(a, 1, 1, true);
        let cut = p.clip(a).unwrap().cuts[0].range;
        assert!(approx(cut.start, 1.12) && approx(cut.end, 1.3), "{cut:?}");
        // Despite the smaller cut, the word counts as deleted (display: struck through).
        let clip = p.clip(a).unwrap();
        let tokens = p.clip_tokens(clip);
        assert!(clip.is_token_deleted(&tokens[1], 30), "\"ähm\" must count as deleted");
        assert!(!clip.is_token_deleted(&tokens[0], 30) && !clip.is_token_deleted(&tokens[2], 30));
        // Restoring removes the cut completely.
        p.set_tokens_deleted(a, 1, 1, false);
        assert!(p.clip(a).unwrap().cuts.is_empty());
    }

    #[test]
    fn clip_transitions_between_adjacent_clips() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        let b = p.add_clip(src, main, 0.0).unwrap();
        // A plays 0–4 s of the source, B 6–10 s → handles available.
        p.clip_mut(a).unwrap().src_out = 4.0;
        p.clip_mut(b).unwrap().src_in = 6.0;
        p.repack_main();
        assert_eq!(p.clip_edges(p.clip(a).unwrap()), ClipEdges::default(), "off = no transitions");

        p.clip_transition = ClipTransition { video: true, audio: true, ms: 1000.0, ..Default::default() };
        let (ea, eb) = (p.clip_edges(p.clip(a).unwrap()), p.clip_edges(p.clip(b).unwrap()));
        assert!(!ea.start.is_some() && !eb.end.is_some(), "only at the contact point");
        assert!(approx(ea.end.extend, 0.5) && approx(eb.start.extend, 0.5));
        assert!(approx(eb.start.video_fade, 1.0) && approx(ea.end.video_fade, 0.0), "upper clip fades in");
        assert!(approx(ea.end.audio_fade, 1.0) && approx(eb.start.audio_fade, 1.0));
        let segs = p.segments_with_edges(p.clip(b).unwrap(), &eb);
        assert!(approx(segs[0].0, 3.5) && approx(segs[0].1.start, 5.5), "{segs:?}");
        assert_eq!(segs.len(), 1, "handle and clip merged into one piece");
        assert!(approx(p.duration(), 8.0), "timeline length unchanged");

        // Without material before the in point (B starts at 0) → through black.
        p.clip_mut(b).unwrap().src_in = 0.0;
        p.repack_main();
        let eb = p.clip_edges(p.clip(b).unwrap());
        assert!(approx(eb.start.extend, 0.0) && approx(eb.start.video_fade, 0.5) && approx(eb.start.audio_fade, 0.5));

        // Only audio tracks active → video clips unchanged.
        p.clip_transition.video = false;
        assert_eq!(p.clip_edges(p.clip(b).unwrap()), ClipEdges { curve: FadeCurve::Sine, ..Default::default() });
    }

    #[test]
    fn cut_kind_detection() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        let c = p.clip_mut(a).unwrap();
        c.add_cut(Range::new(1.0, 2.0), CutKind::Text);
        c.add_cut(Range::new(5.0, 6.0), CutKind::Silence);
        assert_eq!(c.cut_kind_in(Range::new(1.0, 2.0)), CutKind::Text);
        assert_eq!(c.cut_kind_in(Range::new(5.0, 6.0)), CutKind::Silence);
        let mut t = CutTransition::default();
        t.silence_cuts = false;
        assert!(t.applies_to(CutKind::Text) && !t.applies_to(CutKind::Silence));
    }

    #[test]
    fn old_project_files_default_to_video_mode() {
        let mut v: serde_json::Value = serde_json::to_value(Project::default()).unwrap();
        v.as_object_mut().unwrap().remove("mode");
        let p: Project = serde_json::from_value(v).unwrap();
        assert_eq!(p.mode, ProjectMode::Video);
    }

    #[test]
    fn pip_defaults_on_overlay_track() {
        let (mut p, src) = test_project();
        let v2 = p.video_tracks()[1].id;
        let c = p.add_clip(src, v2, 2.0).unwrap();
        let clip = p.clip(c).unwrap();
        assert_eq!(clip.transform.fit, FitMode::Custom);
        assert!(approx(clip.start, 2.0));
        assert!(clip.transform.x > 0.5 && clip.transform.y > 0.5);
    }
}
