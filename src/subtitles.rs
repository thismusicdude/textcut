//! Subtitles: build lines from the transcript, draw them as transparent images
//! and burn them into the video via an ffconcat image sequence.
//!
//! Drawing happens in Rust (ab_glyph) because many ffmpeg builds - including
//! the current Homebrew package - lack text filters (libass/drawtext).

use crate::project::{Project, SubPosition, SubtitleStyle, TokenKind};
use crate::util::cache_dir;
use ab_glyph::{Font, FontVec, Glyph, PxScale, ScaleFont, point};
use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// Lines (cues)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct CueWord {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub words: Vec<CueWord>,
}

impl Cue {
    pub fn text(&self) -> String {
        self.words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ")
    }
}

/// Subtitle lines of the main track in timeline time (after all cuts).
/// New line at sentence end, pauses > 0.8 s or when `max_chars` is reached.
pub fn cues(p: &Project, max_chars: usize) -> Vec<Cue> {
    let fps = p.fps;
    let max_chars = max_chars.max(8);
    let max_words = (max_chars / 5).max(2);
    let mut words: Vec<CueWord> = Vec::new();
    for clip in p.clips_on(p.main_track()) {
        for t in p.clip_tokens(clip) {
            if t.kind != TokenKind::Word || clip.is_token_deleted(&t, fps) {
                continue;
            }
            let r = t.core.unwrap_or(t.range);
            let start = p.source_to_timeline(clip, r.start).max(clip.start);
            let end = p.source_to_timeline(clip, r.end).min(start + r.len()).max(start + 0.05);
            if let Some(prev) = words.last() {
                if start < prev.start {
                    continue;
                }
            }
            words.push(CueWord { text: t.text, start, end });
        }
    }

    let mut out: Vec<Cue> = Vec::new();
    let mut cur: Vec<CueWord> = Vec::new();
    let flush = |cur: &mut Vec<CueWord>, out: &mut Vec<Cue>| {
        if let (Some(first), Some(last)) = (cur.first(), cur.last()) {
            out.push(Cue { start: first.start, end: last.end, words: std::mem::take(cur) });
        }
    };
    for w in words {
        if let Some(prev) = cur.last() {
            let chars: usize = cur.iter().map(|w| w.text.chars().count() + 1).sum();
            if w.start - prev.end > 0.8 || chars + w.text.chars().count() > max_chars || cur.len() >= max_words {
                flush(&mut cur, &mut out);
            }
        }
        let sentence_end = w.text.ends_with(['.', '!', '?']);
        cur.push(w);
        if sentence_end {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);

    // Keep briefly on screen so it can be read to the end - but never into the next line.
    for i in 0..out.len() {
        let limit = out.get(i + 1).map(|n| n.start - 0.04).unwrap_or(f64::INFINITY);
        out[i].end = (out[i].end + 0.4).min(limit).max(out[i].end);
    }
    out
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// Highly legible fonts available on the system: (name, file).
pub fn available_fonts() -> Vec<(String, PathBuf)> {
    let candidates: &[(&str, &[&str])] = &[
        (
            "Arial Bold",
            &[
                "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
                "/Library/Fonts/Arial Bold.ttf",
                "C:\\Windows\\Fonts\\arialbd.ttf",
                "/usr/share/fonts/truetype/msttcorefonts/Arial_Bold.ttf",
            ],
        ),
        ("Arial Black", &["/System/Library/Fonts/Supplemental/Arial Black.ttf", "C:\\Windows\\Fonts\\ariblk.ttf"]),
        ("Arial Rounded Bold", &["/System/Library/Fonts/Supplemental/Arial Rounded Bold.ttf"]),
        ("Impact", &["/System/Library/Fonts/Supplemental/Impact.ttf", "C:\\Windows\\Fonts\\impact.ttf"]),
        ("Verdana Bold", &["/System/Library/Fonts/Supplemental/Verdana Bold.ttf", "C:\\Windows\\Fonts\\verdanab.ttf"]),
        ("Trebuchet MS Bold", &["/System/Library/Fonts/Supplemental/Trebuchet MS Bold.ttf", "C:\\Windows\\Fonts\\trebucbd.ttf"]),
        ("Georgia Bold", &["/System/Library/Fonts/Supplemental/Georgia Bold.ttf", "C:\\Windows\\Fonts\\georgiab.ttf"]),
        ("Helvetica", &["/System/Library/Fonts/Helvetica.ttc"]),
        (
            "DejaVu Sans Bold",
            &["/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf"],
        ),
        ("Liberation Sans Bold", &["/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf"]),
    ];
    candidates
        .iter()
        .filter_map(|(name, paths)| paths.iter().map(PathBuf::from).find(|p| p.is_file()).map(|p| (name.to_string(), p)))
        .collect()
}

pub fn font_path(style: &SubtitleStyle) -> Option<PathBuf> {
    if !style.font.trim().is_empty() {
        return Some(PathBuf::from(style.font.trim()));
    }
    available_fonts().into_iter().next().map(|(_, p)| p)
}

fn load_font(path: &Path) -> Result<Arc<FontVec>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<FontVec>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(f) = cache.lock().unwrap().get(path) {
        return Ok(f.clone());
    }
    let bytes = std::fs::read(path).with_context(|| format!("Font not readable: {}", path.display()))?;
    let font = Arc::new(FontVec::try_from_vec_and_index(bytes, 0).map_err(|_| anyhow!("Invalid font file: {}", path.display()))?);
    cache.lock().unwrap().insert(path.to_path_buf(), font.clone());
    Ok(font)
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Subtitles are drawn into a full-width band that is overlaid on the
/// video at a fixed position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub font_px: f32,
    pub outline: f32,
    pub pad: f32,
    pub band_w: u32,
    pub band_h: u32,
    /// Top edge of the band in the video.
    pub y: u32,
}

pub fn geometry(style: &SubtitleStyle, w: u32, h: u32) -> Geometry {
    let hf = h as f32;
    let font_px = (style.size / 100.0 * hf).max(8.0);
    let outline = (style.outline * hf / 1080.0).max(0.0);
    let pad = outline + font_px * 0.3;
    let band_h = ((style.max_lines.max(1) as f32 * font_px * 1.22 + 2.0 * pad).ceil() as u32).min(h);
    let margin = style.margin / 100.0 * hf;
    let y = match style.position {
        SubPosition::Bottom => (hf - margin - band_h as f32).max(0.0),
        SubPosition::Top => margin.min(hf - band_h as f32),
        SubPosition::Center => (hf - band_h as f32) / 2.0,
    };
    Geometry { font_px, outline, pad, band_w: w, band_h, y: y.round() as u32 }
}

/// Laid-out lines: (word index, glyphs) per line plus line widths.
struct Layout {
    lines: Vec<(Vec<(usize, Vec<Glyph>)>, f32)>,
    px: f32,
}

fn layout(font: &FontVec, style: &SubtitleStyle, geo: &Geometry, words: &[String]) -> Layout {
    let max_w = geo.band_w as f32 * 0.92 - 2.0 * geo.pad;
    let mut px = geo.font_px;
    loop {
        let sf = font.as_scaled(PxScale::from(px));
        let space = sf.h_advance(sf.glyph_id(' '));
        let width = |w: &str| {
            let mut x = 0.0;
            let mut prev = None;
            for c in w.chars() {
                let id = sf.glyph_id(c);
                if let Some(p) = prev {
                    x += sf.kern(p, id);
                }
                x += sf.h_advance(id);
                prev = Some(id);
            }
            x
        };
        // Wrap lines (greedy)
        let mut lines: Vec<(Vec<usize>, f32)> = vec![(Vec::new(), 0.0)];
        for (i, w) in words.iter().enumerate() {
            let ww = width(w);
            let (line, lw) = lines.last_mut().unwrap();
            let needed = if line.is_empty() { ww } else { *lw + space + ww };
            if needed > max_w && !line.is_empty() {
                lines.push((vec![i], ww));
            } else {
                line.push(i);
                *lw = needed;
            }
        }
        let fits = lines.len() <= style.max_lines.max(1) && lines.iter().all(|(_, w)| *w <= max_w);
        if fits || px < 10.0 {
            // Position glyphs
            let line_h = px * 1.22;
            let block_h = lines.len() as f32 * line_h;
            let top = match style.position {
                SubPosition::Bottom => geo.band_h as f32 - geo.pad - block_h,
                SubPosition::Top => geo.pad,
                SubPosition::Center => (geo.band_h as f32 - block_h) / 2.0,
            };
            let ascent = sf.ascent();
            let mut placed = Vec::new();
            for (li, (idxs, lw)) in lines.iter().enumerate() {
                let mut x = (geo.band_w as f32 - lw) / 2.0;
                let baseline = top + li as f32 * line_h + ascent + (line_h - (ascent - sf.descent())) / 2.0;
                let mut line_glyphs = Vec::new();
                for (k, &wi) in idxs.iter().enumerate() {
                    if k > 0 {
                        x += space;
                    }
                    let mut glyphs = Vec::new();
                    let mut prev = None;
                    for c in words[wi].chars() {
                        let id = sf.glyph_id(c);
                        if let Some(p) = prev {
                            x += sf.kern(p, id);
                        }
                        glyphs.push(id.with_scale_and_position(PxScale::from(px), point(x, baseline)));
                        x += sf.h_advance(id);
                        prev = Some(id);
                    }
                    line_glyphs.push((wi, glyphs));
                }
                placed.push((line_glyphs, *lw));
            }
            return Layout { lines: placed, px };
        }
        px *= 0.9;
    }
}

/// Draws a subtitle as an RGBA image (band). `active`: highlighted word.
pub fn rasterize(font: &FontVec, style: &SubtitleStyle, geo: &Geometry, words: &[String], active: Option<usize>) -> Vec<u8> {
    let (bw, bh) = (geo.band_w as usize, geo.band_h as usize);
    let mut rgba = vec![0u8; bw * bh * 4];
    if words.is_empty() {
        return rgba;
    }
    let lay = layout(font, style, geo, words);
    let mut cov = vec![0f32; bw * bh];
    let mut owner = vec![u16::MAX; bw * bh];
    let (mut x0, mut y0, mut x1, mut y1) = (bw, bh, 0usize, 0usize);
    for (line, _) in &lay.lines {
        for (wi, glyphs) in line {
            for g in glyphs {
                let Some(og) = font.outline_glyph(g.clone()) else { continue };
                let b = og.px_bounds();
                og.draw(|gx, gy, c| {
                    let x = b.min.x as i32 + gx as i32;
                    let y = b.min.y as i32 + gy as i32;
                    if x >= 0 && y >= 0 && (x as usize) < bw && (y as usize) < bh && c > 0.0 {
                        let i = y as usize * bw + x as usize;
                        if c > cov[i] {
                            cov[i] = c.min(1.0);
                            owner[i] = *wi as u16;
                        }
                        x0 = x0.min(x as usize);
                        y0 = y0.min(y as usize);
                        x1 = x1.max(x as usize);
                        y1 = y1.max(y as usize);
                    }
                });
            }
        }
    }
    if x1 < x0 {
        return rgba;
    }
    // Working area: text box plus outline/box margin
    let grow = (geo.outline.ceil() as usize) + (lay.px * 0.4) as usize + 2;
    let (rx0, ry0) = (x0.saturating_sub(grow), y0.saturating_sub(grow));
    let (rx1, ry1) = ((x1 + grow).min(bw - 1), (y1 + grow).min(bh - 1));

    // Outline: dilate coverage by `outline` pixels (separable maximum)
    let r = geo.outline.round() as usize;
    let mut outline = vec![0f32; bw * bh];
    if r > 0 {
        let mut tmp = vec![0f32; bw * bh];
        for y in ry0..=ry1 {
            for x in rx0..=rx1 {
                let (a, b) = (x.saturating_sub(r).max(rx0), (x + r).min(rx1));
                tmp[y * bw + x] = (a..=b).map(|xx| cov[y * bw + xx]).fold(0.0, f32::max);
            }
        }
        for y in ry0..=ry1 {
            for x in rx0..=rx1 {
                let (a, b) = (y.saturating_sub(r).max(ry0), (y + r).min(ry1));
                outline[y * bw + x] = (a..=b).map(|yy| tmp[yy * bw + x]).fold(0.0, f32::max);
            }
        }
    }

    // Background box per line (rounded)
    let mut boxes: Vec<(f32, f32, f32, f32)> = Vec::new();
    if style.box_bg {
        let line_h = lay.px * 1.22;
        let block_h = lay.lines.len() as f32 * line_h;
        let top = match style.position {
            SubPosition::Bottom => geo.band_h as f32 - geo.pad - block_h,
            SubPosition::Top => geo.pad,
            SubPosition::Center => (geo.band_h as f32 - block_h) / 2.0,
        };
        for (li, (_, lw)) in lay.lines.iter().enumerate() {
            let px = lay.px * 0.35;
            let lx = (geo.band_w as f32 - lw) / 2.0 - px;
            let ly = top + li as f32 * line_h - lay.px * 0.04;
            boxes.push((lx, ly, lx + lw + 2.0 * px, ly + line_h + lay.px * 0.08));
        }
    }
    let radius = lay.px * 0.22;
    let in_box = |x: f32, y: f32| {
        boxes.iter().any(|&(l, t, r, b)| {
            if x < l || x > r || y < t || y > b {
                return false;
            }
            let cx = x.clamp(l + radius, r - radius);
            let cy = y.clamp(t + radius, b - radius);
            (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
        })
    };
    let (bx0, by0, bx1, by1) = if boxes.is_empty() {
        (rx0, ry0, rx1, ry1)
    } else {
        let l = boxes.iter().map(|b| b.0).fold(f32::MAX, f32::min).max(0.0) as usize;
        let t = boxes.iter().map(|b| b.1).fold(f32::MAX, f32::min).max(0.0) as usize;
        let r = (boxes.iter().map(|b| b.2).fold(0.0, f32::max).ceil() as usize).min(bw - 1);
        let b = (boxes.iter().map(|b| b.3).fold(0.0, f32::max).ceil() as usize).min(bh - 1);
        (l.min(rx0), t.min(ry0), r.max(rx1), b.max(ry1))
    };

    let col = |c: [u8; 3]| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0];
    let (text_c, hl_c, out_c) = (col(style.color), col(style.highlight_color), col(style.outline_color));
    for y in by0..=by1 {
        for x in bx0..=bx1 {
            let i = y * bw + x;
            // "Over" compositing with unpremultiplied alpha
            let mut acc = [0f32; 3];
            let mut a = 0f32;
            let mut over = |c: [f32; 3], sa: f32| {
                if sa <= 0.0 {
                    return;
                }
                let na = sa + a * (1.0 - sa);
                for k in 0..3 {
                    acc[k] = (c[k] * sa + acc[k] * a * (1.0 - sa)) / na;
                }
                a = na;
            };
            if !boxes.is_empty() && in_box(x as f32 + 0.5, y as f32 + 0.5) {
                over([0.0, 0.0, 0.0], style.box_opacity.clamp(0.0, 1.0));
            }
            if r > 0 {
                over(out_c, outline[i]);
            }
            if cov[i] > 0.0 {
                let c = if style.highlight && active == Some(owner[i] as usize) { hl_c } else { text_c };
                over(c, cov[i]);
            }
            let o = i * 4;
            rgba[o] = (acc[0] * 255.0).round() as u8;
            rgba[o + 1] = (acc[1] * 255.0).round() as u8;
            rgba[o + 2] = (acc[2] * 255.0).round() as u8;
            rgba[o + 3] = (a * 255.0).round() as u8;
        }
    }
    rgba
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<()> {
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

fn subs_dir() -> PathBuf {
    let d = cache_dir().join("subs");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn hash_of(parts: &impl Hash) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// Image for a subtitle (cached).
fn cue_image(font_file: &Path, style: &SubtitleStyle, geo: &Geometry, words: &[String], active: Option<usize>) -> Result<PathBuf> {
    let style_key = serde_json::to_string(style).unwrap_or_default();
    let key = hash_of(&(
        "v1",
        style_key,
        font_file,
        geo.band_w,
        geo.band_h,
        words,
        if style.highlight { active } else { None },
    ));
    let path = subs_dir().join(format!("{key:016x}.png"));
    if !path.is_file() {
        let font = load_font(font_file)?;
        let rgba = rasterize(&font, style, geo, words, active);
        let tmp = path.with_extension("tmp");
        write_png(&tmp, geo.band_w, geo.band_h, &rgba)?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// Burn-in
// ---------------------------------------------------------------------------

/// ffmpeg input with the subtitle images for a timeline range.
pub struct Overlay {
    /// ffconcat list (image + duration, starting at `from`).
    pub list: PathBuf,
    /// Top edge of the subtitle band in the video.
    pub y: u32,
}

/// Subtitle image sequence for `[from, to)`; `None` if there is nothing to burn in.
pub fn overlay(p: &Project, w: u32, h: u32, from: f64, to: f64) -> Result<Option<Overlay>> {
    let style = &p.subtitles;
    if !style.burn_in {
        return Ok(None);
    }
    let all = cues(p, style.max_chars);
    let visible: Vec<&Cue> = all.iter().filter(|c| c.end > from && c.start < to).collect();
    if visible.is_empty() {
        return Ok(None);
    }
    let font_file = font_path(style).ok_or_else(|| anyhow!("No font found for subtitles"))?;
    let geo = geometry(style, w, h);

    // (start, end, image) in timeline time
    let mut events: Vec<(f64, f64, PathBuf)> = Vec::new();
    for cue in visible {
        let words: Vec<String> =
            cue.words.iter().map(|w| if style.uppercase { w.text.to_uppercase() } else { w.text.clone() }).collect();
        if style.highlight {
            for k in 0..words.len() {
                let s = if k == 0 { cue.start } else { cue.words[k].start };
                let e = cue.words.get(k + 1).map(|n| n.start).unwrap_or(cue.end);
                if e > s && e > from && s < to {
                    events.push((s, e, cue_image(&font_file, style, &geo, &words, Some(k))?));
                }
            }
        } else {
            events.push((cue.start, cue.end, cue_image(&font_file, style, &geo, &words, None)?));
        }
    }
    let blank = cue_image(&font_file, style, &geo, &[], None)?;

    // List relative to `from` (image changes land exactly on the timestamps).
    let total = to - from;
    let quote = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let mut list = String::from("ffconcat version 1.0\n");
    let mut t = 0.0f64;
    let push = |list: &mut String, path: &Path, d: f64| {
        // framerate 1000: otherwise ffmpeg reads still images at 25 fps and rounds the times to 40 ms.
        list.push_str(&format!("file {}\noption framerate 1000\nduration {d:.6}\n", quote(path)));
    };
    for (s, e, img) in &events {
        let s = (s - from).max(t).max(0.0);
        let e = (e - from).min(total);
        if e - s < 1e-4 {
            continue;
        }
        if s - t > 1e-4 {
            push(&mut list, &blank, s - t);
        }
        push(&mut list, img, e - s);
        t = e;
    }
    push(&mut list, &blank, (total - t).max(0.05));
    list.push_str(&format!("file {}\noption framerate 1000\n", quote(&blank)));
    let path = subs_dir().join(format!("list_{:016x}.ffconcat", hash_of(&list)));
    if !path.is_file() {
        std::fs::write(&path, &list)?;
    }
    Ok(Some(Overlay { list: path, y: geo.y }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{MediaKind, MediaSource, Transcript, Word};

    fn project(words: &[(&str, f64, f64)]) -> (Project, u64) {
        let mut p = Project::default();
        let id = p.alloc_id();
        p.media.push(MediaSource {
            id,
            path: "x.mp4".into(),
            name: "x".into(),
            kind: MediaKind::Video,
            duration: 20.0,
            width: 640,
            height: 360,
            fps: 30.0,
            has_video: true,
            has_audio: true,
            transcript: Some(Transcript {
                language: "de".into(),
                words: words.iter().map(|(t, s, e)| Word { text: t.to_string(), start: *s, end: *e, core: None }).collect(),
                ..Default::default()
            }),
            silences: None,
            waveform: None,
        });
        let main = p.main_track();
        let c = p.add_clip(id, main, 0.0).unwrap();
        (p, c)
    }

    #[test]
    fn cues_split_at_sentences_pauses_and_length() {
        let (p, _) = project(&[
            ("Hallo", 0.0, 0.4),
            ("Welt.", 0.5, 0.9),
            ("Das", 1.0, 1.2),
            ("ist", 1.3, 1.5),
            ("ein", 3.0, 3.2), // pause > 0.8 s before
            ("Test", 3.3, 3.6),
        ]);
        let c = cues(&p, 42);
        let texts: Vec<String> = c.iter().map(|c| c.text()).collect();
        assert_eq!(texts, vec!["Hallo Welt.", "Das ist", "ein Test"]);
        assert!(c[0].end <= c[1].start - 0.04 + 1e-9, "not overlapping");
        assert!((c[2].end - 4.0).abs() < 1e-9, "end + 0.4 s reading time");
        let short = cues(&p, 10);
        assert!(short.iter().all(|c| c.text().chars().count() <= 10), "{:?}", short.iter().map(|c| c.text()).collect::<Vec<_>>());
    }

    #[test]
    fn cues_follow_text_cuts() {
        let (mut p, c) = project(&[("Eins", 0.0, 0.5), ("zwei", 0.6, 1.0), ("drei.", 1.1, 1.5)]);
        p.set_tokens_deleted(c, 1, 1, true);
        let cs = cues(&p, 42);
        assert_eq!(cs[0].text(), "Eins drei.");
        assert!(cs[0].words[1].start < 1.1, "\"drei.\" moves forward: {}", cs[0].words[1].start);
    }

    /// Real burn-in with ffmpeg: text appears exactly during its line.
    #[test]
    fn burns_in_with_ffmpeg_frame_accurately() {
        use std::process::Command;
        if font_path(&SubtitleStyle::default()).is_none()
            || Command::new("ffmpeg").arg("-version").output().is_err()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tcf_subs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("gray.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=0x404040:s=640x360:r=30:d=4"])
            .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", "4", "-c:v", "libx264", "-c:a", "aac"])
            .arg(&src)
            .status()
            .unwrap()
            .success());
        let (mut p, _) = project(&[("Hallo", 1.0, 1.4), ("Welt.", 1.5, 1.9)]);
        p.media[0].path = src;
        p.media[0].duration = 4.0;
        let c = p.clips[0].id;
        p.clip_mut(c).unwrap().src_out = 4.0;
        p.repack_main();
        p.subtitles.burn_in = true;
        let cue = &cues(&p, 42)[0];
        assert!((cue.start - 1.0).abs() < 1e-9 && (cue.end - 2.3).abs() < 1e-9);

        let spec = crate::render::RenderSpec {
            width: 640,
            height: 360,
            from: 0.0,
            to: 4.0,
            video: true,
            audio: false,
            fit_override: None,
            pix_fmt: "rgb24",
            out_fps: None,
            loudnorm: false,
        };
        let g = crate::render::build(&p, &spec);
        assert!(g.args.iter().any(|a| a == "concat"), "subtitle input present");
        let out = Command::new("ffmpeg")
            .args(["-v", "error"])
            .args(&g.args)
            .args(["-map", "[vout]", "-f", "rawvideo", "pipe:1"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let fs = 640 * 360 * 3;
        assert_eq!(out.stdout.len() / fs, 120);
        let white = |k: usize| out.stdout[k * fs..(k + 1) * fs].chunks(3).filter(|p| p.iter().all(|v| *v > 240)).count();
        assert_eq!(white(29), 0, "0.967 s: no subtitle yet");
        assert!(white(30) > 200, "1.000 s: subtitle shown");
        assert!(white(68) > 200, "2.267 s: still shown (reading time)");
        assert_eq!(white(69), 0, "2.300 s: gone again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn geometry_positions_band() {
        let mut s = SubtitleStyle::default();
        let g = geometry(&s, 1920, 1080);
        assert!(g.y as f32 + g.band_h as f32 <= 1080.0 - 0.07 * 1080.0 + 1.0);
        s.position = SubPosition::Top;
        assert_eq!(geometry(&s, 1920, 1080).y, (0.07f32 * 1080.0).round() as u32);
        // Relative to height: 9:16 has the same size in percent
        let v = geometry(&SubtitleStyle::default(), 1080, 1920);
        assert!((v.font_px / 1920.0 - g.font_px / 1080.0).abs() < 1e-4);
    }

    #[test]
    fn rasterizes_text_with_outline_and_highlight() {
        let Some(font_file) = font_path(&SubtitleStyle::default()) else {
            eprintln!("no font - skipped");
            return;
        };
        let font = load_font(&font_file).unwrap();
        let mut style = SubtitleStyle { highlight: true, ..Default::default() };
        let geo = geometry(&style, 1920, 1080);
        let words = vec!["Hallo".to_string(), "Welt".to_string()];
        let img = rasterize(&font, &style, &geo, &words, Some(1));
        let px = |x: usize, y: usize| &img[(y * geo.band_w as usize + x) * 4..][..4];
        let opaque = (0..geo.band_h as usize)
            .flat_map(|y| (0..geo.band_w as usize).map(move |x| (x, y)))
            .filter(|&(x, y)| px(x, y)[3] > 250)
            .collect::<Vec<_>>();
        assert!(opaque.len() > 200, "text visible");
        let white = opaque.iter().filter(|&&(x, y)| px(x, y)[..3] == [255, 255, 255]).count();
        let yellow = opaque.iter().filter(|&&(x, y)| px(x, y)[..3] == [255, 210, 40]).count();
        let black = opaque.iter().filter(|&&(x, y)| px(x, y)[..3] == [0, 0, 0]).count();
        assert!(white > 30 && yellow > 30 && black > 30, "white {white}, yellow {yellow}, outline {black}");
        // Yellow word to the right of the white one
        let avg_x = |c: [u8; 3]| {
            let v: Vec<usize> = opaque.iter().filter(|&&(x, y)| px(x, y)[..3] == c).map(|&(x, _)| x).collect();
            v.iter().sum::<usize>() as f32 / v.len() as f32
        };
        assert!(avg_x([255, 210, 40]) > avg_x([255, 255, 255]));
        // Box: semi-transparent background
        style.box_bg = true;
        let img = rasterize(&font, &style, &geo, &words, None);
        assert!(img.chunks(4).any(|p| p[3] > 100 && p[3] < 200), "box with opacity 0.6");
    }
}
