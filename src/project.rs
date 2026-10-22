//! Datenmodell und Schnittlogik.
//!
//! Grundidee: Ein Clip referenziert einen Quellbereich `[src_in, src_out)` einer
//! Mediendatei. Text- und Stille-Schnitte werden als `Cut`s im Quellzeitbereich
//! gespeichert. Die tatsächlich abgespielten Bereiche („kept ranges“) ergeben
//! sich aus Quellbereich minus Schnitte, gerastert auf das Frame-Raster des
//! Projekts. Dadurch bleiben alle Schnitte nicht-destruktiv und jederzeit
//! wiederherstellbar.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

pub type Id = u64;

/// Pausen ab dieser Länge erscheinen als eigenes Token im Transkript.
pub const MIN_SILENCE_TOKEN: f64 = 0.3;

// ---------------------------------------------------------------------------
// Zeitbereiche
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

/// `base` minus die Vereinigung aller `holes`.
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

/// Vereinigt überlappende/angrenzende Bereiche.
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
// Medien
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
    /// Kern des Worts laut DTW (ohne Lücken zu den Nachbarn). Begrenzt beim
    /// Löschen den Schnitt, damit Nachbarwörter nie angeschnitten werden.
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
    /// Spitzenpegel je Wort in dB (parallel zu `words`), aus der Wellenform.
    #[serde(default)]
    pub levels_db: Vec<f32>,
    /// Median der Wortpegel – Bezug für „unsicher“.
    #[serde(default)]
    pub median_db: Option<f32>,
}

/// Wörter, die so viel leiser als die übrige Sprache sind, gelten als unsicher
/// (Whisper erfindet bei Geräuschen/Stille gelegentlich Wörter).
pub const UNCERTAIN_BELOW_MEDIAN_DB: f32 = 25.0;

impl Transcript {
    /// Misst den Spitzenpegel jedes Worts (auf den an den Pausen korrigierten
    /// Zeiten). `peaks`: 100 Werte/s, √-komprimiert (siehe `media::waveform`).
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
    /// Von ffmpeg erkannte Stille-Bereiche (Quellzeit).
    #[serde(default)]
    pub silences: Option<Vec<Range>>,
    /// Pegelspitzen, 100 Werte pro Sekunde (0..1). Wird nicht gespeichert.
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
// Spuren & Clips
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
    /// Ganzes Bild sichtbar, ggf. schwarze Balken.
    Fit,
    /// Bildfüllend, Überstand wird abgeschnitten.
    Fill,
    /// Freie Position/Größe (Bild-in-Bild).
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
    /// Linke obere Ecke, normiert auf die Leinwand (0..1).
    pub x: f32,
    pub y: f32,
    /// Breite relativ zur Leinwandbreite.
    pub scale: f32,
    pub opacity: f32,
    /// Rahmenbreite in Pixeln bezogen auf 1080p-Höhe.
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
    /// Startzeit auf der Timeline.
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
    /// Tatsächlich abgespielte Quellbereiche, auf das Frame-Raster gerastert.
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

    /// (Timeline-Start, Quellbereich) für jedes abgespielte Stück.
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

    /// Lokale Clipzeit (0 = Clipanfang) → Quellzeit.
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

    /// Quellzeit → lokale Clipzeit. Liegt die Zeit in einem Schnitt, wird der
    /// Anfang des nächsten behaltenen Stücks geliefert.
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

    /// Anteil eines Quellbereichs, der tatsächlich abgespielt wird (0..1).
    pub fn kept_fraction(&self, r: Range, fps: u32) -> f64 {
        let r = if r.len() < 0.04 { Range::new(r.mid() - 0.02, r.mid() + 0.02) } else { r };
        let kept: f64 = self.kept(fps).iter().map(|k| k.overlap(&r)).sum();
        kept / r.len()
    }

    /// Gilt ein Token als gelöscht (im Text durchgestrichen)? Maßgeblich ist der
    /// Wortkern – genau der Bereich, den das Löschen schneidet. Die volle
    /// Wortspanne enthält auch die Lücken zu den Nachbarn und bliebe sonst
    /// zu großen Teilen „behalten“.
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

    /// Entfernt Schnitte (optional nur einer Art) innerhalb von `r`.
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

    /// Art des Schnitts, der eine Lücke zwischen zwei behaltenen Stücken
    /// erzeugt. Text-Schnitte haben Vorrang vor Stille.
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
// Weiche Übergänge an Schnittstellen
// ---------------------------------------------------------------------------

/// Form der Aus-/Einblendung. `x` läuft von 0 (Schnittstelle) bis 1 (volle Lautstärke).
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
            FadeCurve::Sine => "Sinus (weich, empfohlen)",
            FadeCurve::SCurve => "S-Kurve (sanfter Start & Ende)",
            FadeCurve::Fast => "Schnell (Logarithmisch)",
            FadeCurve::Slow => "Langsam (Exponentiell)",
        }
    }

    /// Verstärkung 0..1 für x in 0..1.
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

    /// Passende Kurve des ffmpeg-Filters `afade` (Ausblenden spiegelt sie).
    pub fn afade_curve(self) -> &'static str {
        match self {
            FadeCurve::Linear => "tri",
            FadeCurve::Sine => "qsin",
            FadeCurve::SCurve => "hsin",
            FadeCurve::Fast => "ipar",
            FadeCurve::Slow => "qua",
        }
    }

    /// Dieselbe Kurve als ffmpeg-Ausdruck.
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

/// Weiche Tonübergänge an Schnittstellen innerhalb eines Clips: Der Ton wird
/// vor dem Schnitt aus- und danach wieder eingeblendet (Gesamtlänge `ms`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CutTransition {
    pub enabled: bool,
    /// Gesamtlänge in Millisekunden (je Hälfte aus- bzw. einblenden).
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
// Übergänge zwischen Clips
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionStyle {
    /// Bild löst sich ins nächste auf, Ton wird übergeblendet.
    #[default]
    Dissolve,
    /// Ab- und Aufblende über Schwarz bzw. Stille.
    DipToBlack,
}

impl TransitionStyle {
    pub fn label(self) -> &'static str {
        match self {
            TransitionStyle::Dissolve => "Überblenden",
            TransitionStyle::DipToBlack => "Über Schwarz / Stille",
        }
    }
}

/// Weiche Übergänge an Stellen, an denen zwei Clips auf derselben Spur
/// direkt aneinanderstoßen.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipTransition {
    pub video: bool,
    pub audio: bool,
    /// Gesamtlänge des Übergangs in Millisekunden.
    pub ms: f32,
    pub style: TransitionStyle,
    /// Kurve für den Ton (das Bild blendet linear).
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

/// Behandlung eines Clip-Rands durch einen Übergang.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edge {
    /// So weit läuft der Clip über seinen Rand hinaus (Material vor In- bzw.
    /// nach Out-Punkt, „Handle“).
    pub extend: f64,
    /// Länge der Bild-Blende (Deckkraft) an diesem Rand.
    pub video_fade: f64,
    /// Länge der Ton-Blende an diesem Rand.
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
// Untertitel
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubPosition {
    #[default]
    Bottom,
    Center,
    Top,
}

/// Aussehen eingebrannter Untertitel. Größen relativ zur Videohöhe, damit sie
/// in jeder Exportgröße gleich wirken.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SubtitleStyle {
    /// In Vorschau und Video-Export einbrennen.
    pub burn_in: bool,
    /// Schriftdatei (.ttf/.otf/.ttc); leer = Standardschrift des Systems.
    pub font: String,
    /// Schriftgröße in % der Videohöhe.
    pub size: f32,
    pub position: SubPosition,
    /// Abstand zum Bildrand in % der Videohöhe.
    pub margin: f32,
    pub color: [u8; 3],
    /// Gerade gesprochenes Wort farbig hervorheben.
    pub highlight: bool,
    pub highlight_color: [u8; 3],
    /// Konturbreite in Pixeln bezogen auf 1080p.
    pub outline: f32,
    pub outline_color: [u8; 3],
    pub box_bg: bool,
    pub box_opacity: f32,
    pub uppercase: bool,
    /// Höchstens so viele Zeichen pro Untertitel.
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
// Transkript-Token (Wörter + Pausen) für den Texteditor
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
    /// Kaum Ton unter dem Wort – vermutlich von Whisper erfunden.
    pub uncertain: bool,
    /// Wortkern (siehe `Word::core`).
    pub core: Option<Range>,
}

// ---------------------------------------------------------------------------
// Projekt
// ---------------------------------------------------------------------------

/// Video-Projekt (mit Bild, Bild-in-Bild …) oder reines Audio-Projekt (Podcast, Voiceover).
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
    /// Leinwandgröße wird beim ersten Video automatisch übernommen.
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
            name: if audio { "Unbenanntes Audio-Projekt" } else { "Unbenanntes Projekt" }.into(),
            width: 1920,
            height: 1080,
            // Audio: 10-ms-Raster für feinere Schnitte.
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

    /// Videospuren in Kompositionsreihenfolge (V1 ganz unten zuerst).
    pub fn video_tracks(&self) -> Vec<&Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect()
    }

    pub fn audio_tracks(&self) -> Vec<&Track> {
        self.tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect()
    }

    /// Hauptspur (V1 bzw. im Audio-Modus A1): magnetisch, Clips liegen
    /// lückenlos hintereinander.
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

    /// Clips in „Dokument-Reihenfolge“: erst Hauptspur, dann übrige Spuren.
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

    /// Schiebt die Clips der Hauptspur lückenlos zusammen (Ripple).
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

    /// Fügt einen Clip für eine Quelle ein. Auf der Hauptspur wird angehängt,
    /// auf anderen Spuren an `at` platziert.
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

    /// Positioniert einen Bild-in-Bild-Clip an einer Ecke/Mitte.
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

    /// Teilt einen Clip an Timeline-Zeit `t`. Liefert die ID des neuen Clips.
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

    /// Übergänge an den Rändern eines Clips. Bei „Überblenden“ läuft der
    /// vordere Clip mit Material nach seinem Out-Punkt weiter, der hintere
    /// beginnt mit Material vor seinem In-Punkt – mittig um die Schnittstelle,
    /// sodass sich die Timeline-Länge nicht ändert. Fehlt solches Material
    /// (Dateianfang/-ende), wird stattdessen über Schwarz/Stille geblendet.
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
        // Handle-Länge für den Übergang a → b (auf ganze Frames abgerundet).
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
                // Überblenden: beide Seiten laufen h weiter. Das Bild des oberen
                // (hinteren) Clips blendet ein, der Ton beider Clips blendet über.
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

    /// Wie `Clip::segments`, aber inklusive der Übergangs-Handles; direkt
    /// aneinander anschließende Stücke werden zusammengefasst.
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

    /// Quellzeit → Timeline-Zeit für einen Clip.
    pub fn source_to_timeline(&self, clip: &Clip, src: f64) -> f64 {
        clip.start + clip.source_to_local(src, self.fps)
    }

    // -----------------------------------------------------------------------
    // Text-basierter Schnitt
    // -----------------------------------------------------------------------

    /// Wörter und Pausen eines Clips in zeitlicher Reihenfolge.
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
                // Ohne Stille-Analyse: große Lücken zwischen Wörtern als Pausen zeigen.
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

    /// Schnittbereich für die Token `first..=last`: vom Anfang des ersten bis
    /// zum Anfang des folgenden Tokens, damit die Lücke danach mit verschwindet.
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

    /// Schnittbereich zum *Löschen*: wie `token_cut_range`, lässt aber zu
    /// benachbarten Wörtern einen kleinen Puffer aus der Lücke stehen. So blendet
    /// der weiche Übergang in der Stille ein/aus und schluckt keine leisen
    /// Wortanfänge („S“ in „Sphäre“) oder -enden.
    pub fn token_delete_range(clip: &Clip, tokens: &[Token], first: usize, last: usize) -> Range {
        const GUARD_BEFORE: f64 = 0.02;
        const GUARD_AFTER: f64 = 0.04;
        let (a, b) = (first.min(last), first.max(last));
        let r = Self::token_cut_range(clip, tokens, a, b);
        let is_word = |i: Option<usize>| i.and_then(|i| tokens.get(i)).is_some_and(|t| t.kind == TokenKind::Word);
        let mut start = r.start;
        let mut end = r.end;
        if is_word(a.checked_sub(1)) {
            // Nicht vor dem Kern des ersten gelöschten Worts beginnen.
            start = (start + GUARD_BEFORE).max(tokens[a].core.map_or(f64::MIN, |c| c.start));
        }
        if is_word(Some(b + 1)) {
            // Nicht über den Kern des letzten gelöschten Worts hinaus schneiden –
            // leise Anlaute wie das „S“ in „Sphäre“ liegen sonst im Schnitt.
            end = (end - GUARD_AFTER).min(tokens[b].core.map_or(f64::MAX, |c| c.end));
        }
        if end - start >= 0.05 { Range::new(start, end) } else { r }
    }

    /// Löscht (bzw. stellt wieder her) die Token `first..=last` eines Clips.
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

    /// Entfernt erkannte Stille aus einem Clip. `padding` Sekunden bleiben an
    /// jedem Rand stehen, damit Schnitte natürlich klingen.
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

    /// Schneidet Füllwörter („äh“, „ähm“ …) heraus. Unsichere Treffer (kaum Ton)
    /// bleiben stehen. Liefert (entfernt, unsicher übersprungen).
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

    /// Löscht alle Vorkommen einer Wortfolge (Suchen & Löschen).
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

/// Wörter und Pausen einer Quelle, aufeinander abgestimmt:
/// Wortzeiten werden an den Pausen korrigiert (`refine_words`), und Pausen,
/// zwischen denen nur ein kurzes Geräusch ohne Wort liegt (Atmer, Schmatzen),
/// werden zu einer Pause zusammengefasst. `None`, wenn noch keine Stille-Analyse
/// vorliegt.
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

/// Geräusche zwischen zwei Pausen, die höchstens so lang sind und kein Wort
/// enthalten, gelten als Teil der Pause.
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

/// Liegt (nennenswert) ein Wort im Bereich? `words` ist nach Start sortiert.
fn has_word_in(words: &[Word], r: Range) -> bool {
    let idx = words.partition_point(|w| w.start < r.end);
    words[..idx].iter().rev().take(8).any(|w| w.range().overlap(&r) > 0.05 || (w.end - w.start < 0.05 && r.contains(w.start)))
}

/// Korrigiert ungenaue Whisper-Zeitstempel anhand der (sortierten) Pausen:
/// 1. Kein Wort beginnt oder endet in einer Pause – die Grenze wandert an den
///    Pausenrand (Whisper setzt Wortanfänge oft zu früh).
/// 2. Kein Wort überspannt eine Pause – es kommt auf die Seite mit mehr Ton.
/// 3. Wörter mit (fast) gleicher Startzeit werden gleichmäßig verteilt.
pub fn refine_words(words: &mut [Word], silences: &[Range]) {
    let silences: Vec<Range> = silences.iter().copied().filter(|s| s.len() >= MIN_SILENCE_TOKEN).collect();
    for w in words.iter_mut() {
        // Nur Pausen ansehen, die das Wort berühren (Pausen sind sortiert).
        let first = silences.partition_point(|s| s.end <= w.start);
        for s in &silences[first..] {
            if s.start >= w.end {
                break;
            }
            let starts_in = w.start >= s.start && w.start < s.end;
            let ends_in = w.end > s.start && w.end <= s.end;
            if starts_in && ends_in {
                // Ganz in der Pause: an den näheren Rand schieben.
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

/// Kleinbuchstaben, ohne Satzzeichen – für Wortvergleiche.
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
        // Zeit innerhalb eines Schnitts → Anfang des nächsten Stücks.
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
        // „ähm“ (1.1) bis Anfang „Welt.“ (1.6), mit Puffer 20/40 ms zu den Nachbarwörtern,
        // gerastert auf 30 fps: 1.1333–1.5667 → 0.4333 s.
        let expected = 10.0 - 13.0 / 30.0;
        assert!(approx(p.clip(a).unwrap().duration(30), expected), "{}", p.clip(a).unwrap().duration(30));
        assert!(approx(p.clip(b).unwrap().start, expected), "Hauptspur rückt nach");

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
        // 2.0–4.0 mit Padding → 1.8 s weg; 5.0–10.0 am Clipende → 4.9 s weg.
        let d = p.clip(a).unwrap().duration(30);
        assert!(approx(d, 10.0 - 1.8 - 4.9), "{d}");
        // Erneutes Anwenden ersetzt die alten Stille-Schnitte.
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
        p.media.push(m); // Videodatei: im Audio-Modus zählt nur der Ton
        let main = p.main_track();
        assert_eq!(p.track(main).unwrap().kind, TrackKind::Audio);
        let a = p.add_clip(id, main, 0.0).unwrap();
        let b = p.add_clip(id, main, 0.0).unwrap();
        assert_eq!(p.fps, 100);
        assert_eq!(p.clip(a).unwrap().transform.fit, FitMode::Fit);
        p.apply_silence_removal(a, 0.0, 0.5);
        assert!(approx(p.clip(b).unwrap().start, 3.0), "rückt nach: {}", p.clip(b).unwrap().start);
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
        // Whisper: Wörter beginnen in der Pause 7.0–9.3 und teilen sich die Startzeit.
        let mut words = vec![
            w("schneiden.", 6.1, 7.6),
            w("Im", 9.0, 9.0),
            w("und", 9.0, 9.1),
            w("stille", 9.0, 9.6),
            w("wird", 9.7, 9.9),
        ];
        refine_words(&mut words, &[Range::new(7.0, 9.3)]);
        assert!(approx(words[0].end, 7.0), "Ende an Pausenanfang: {}", words[0].end);
        for x in &words[1..4] {
            assert!(x.start >= 9.3 - 1e-9, "{} beginnt nicht in der Pause: {}", x.text, x.start);
        }
        assert!(words[1].start < words[2].start && words[2].start < words[3].start, "verteilt");
        assert!(words[3].end <= words[4].start + 1e-9);

        // Eine Pause davor schneidet das Wort danach nicht mehr an.
        let (mut p, src) = test_project();
        p.media[0].transcript.as_mut().unwrap().words[3].start = 3.5; // „Das“ beginnt laut Whisper in der Pause 2–4
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        p.apply_silence_removal(a, 0.1, 0.5);
        let clip = p.clip(a).unwrap();
        let das = p.clip_tokens(clip).into_iter().find(|t| t.text == "Das").unwrap();
        assert!(clip.kept_fraction(das.range, 30) > 0.9);
    }

    /// Echte Werte aus einer Aufnahme: „Ozean. [Pause – Atmer – Pause] Wasserstoffeis“.
    /// Whisper legt „Wasserstoffeis“ auf 29.36–34.00, gesprochen wird es ab 32.96.
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

        // Stille entfernen nimmt auch den Atmer dazwischen mit.
        p.apply_silence_removal(c, 0.0, 0.5);
        let clip = p.clip(c).unwrap();
        assert!(clip.removed_time(p.fps) > 4.1, "{}", clip.removed_time(p.fps));
        assert!(clip.kept_fraction(tokens[3].range, p.fps) > 0.99);

        // Liegt ein Wort im Geräusch, bleiben es zwei Pausen.
        p.media[0].transcript.as_mut().unwrap().words.insert(2, w("Ja.", 30.75, 31.1));
        let tokens = p.clip_tokens(p.clip(c).unwrap());
        let texts: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(texts, vec!["astralen", "Ozean.", "⏸ 1.9s", "Ja.", "⏸ 1.8s", "Wasserstoffeis", "klebt"]);
    }

    #[test]
    fn quiet_words_are_uncertain_and_kept_by_filler_removal() {
        let (mut p, src) = test_project();
        // Wellenform: überall Sprache (≈ −10 dB), nur bei 1.1–1.5 s („ähm“) fast still.
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
        assert!(approx(p.clip(a).unwrap().duration(30), 10.0), "nichts geschnitten");
    }

    #[test]
    fn deleting_respects_word_core() {
        let (mut p, src) = test_project();
        // „ähm“ liegt laut DTW nur bei 1.1–1.3; danach beginnt leise schon „Welt.“
        p.media_mut(src).unwrap().transcript.as_mut().unwrap().words[1].core = Some((1.1, 1.3));
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        p.set_tokens_deleted(a, 1, 1, true);
        let cut = p.clip(a).unwrap().cuts[0].range;
        assert!(approx(cut.start, 1.12) && approx(cut.end, 1.3), "{cut:?}");
        // Trotz kleinerem Schnitt gilt das Wort als gelöscht (Anzeige: durchgestrichen).
        let clip = p.clip(a).unwrap();
        let tokens = p.clip_tokens(clip);
        assert!(clip.is_token_deleted(&tokens[1], 30), "„ähm“ muss als gelöscht gelten");
        assert!(!clip.is_token_deleted(&tokens[0], 30) && !clip.is_token_deleted(&tokens[2], 30));
        // Wiederherstellen entfernt den Schnitt vollständig.
        p.set_tokens_deleted(a, 1, 1, false);
        assert!(p.clip(a).unwrap().cuts.is_empty());
    }

    #[test]
    fn clip_transitions_between_adjacent_clips() {
        let (mut p, src) = test_project();
        let main = p.main_track();
        let a = p.add_clip(src, main, 0.0).unwrap();
        let b = p.add_clip(src, main, 0.0).unwrap();
        // A spielt 0–4 s der Quelle, B 6–10 s → Handles vorhanden.
        p.clip_mut(a).unwrap().src_out = 4.0;
        p.clip_mut(b).unwrap().src_in = 6.0;
        p.repack_main();
        assert_eq!(p.clip_edges(p.clip(a).unwrap()), ClipEdges::default(), "aus = keine Übergänge");

        p.clip_transition = ClipTransition { video: true, audio: true, ms: 1000.0, ..Default::default() };
        let (ea, eb) = (p.clip_edges(p.clip(a).unwrap()), p.clip_edges(p.clip(b).unwrap()));
        assert!(!ea.start.is_some() && !eb.end.is_some(), "nur an der Berührstelle");
        assert!(approx(ea.end.extend, 0.5) && approx(eb.start.extend, 0.5));
        assert!(approx(eb.start.video_fade, 1.0) && approx(ea.end.video_fade, 0.0), "oberer Clip blendet ein");
        assert!(approx(ea.end.audio_fade, 1.0) && approx(eb.start.audio_fade, 1.0));
        let segs = p.segments_with_edges(p.clip(b).unwrap(), &eb);
        assert!(approx(segs[0].0, 3.5) && approx(segs[0].1.start, 5.5), "{segs:?}");
        assert_eq!(segs.len(), 1, "Handle und Clip zu einem Stück zusammengefasst");
        assert!(approx(p.duration(), 8.0), "Timeline-Länge unverändert");

        // Ohne Material vor dem In-Punkt (B beginnt bei 0) → über Schwarz.
        p.clip_mut(b).unwrap().src_in = 0.0;
        p.repack_main();
        let eb = p.clip_edges(p.clip(b).unwrap());
        assert!(approx(eb.start.extend, 0.0) && approx(eb.start.video_fade, 0.5) && approx(eb.start.audio_fade, 0.5));

        // Nur Audiospuren aktiv → Videoclips unverändert.
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
