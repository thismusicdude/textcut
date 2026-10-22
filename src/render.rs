//! Translates the timeline into an ffmpeg `filter_complex`.
//!
//! Preview and export use the same graph so the preview matches the result
//! exactly. Each clip opens its source file once, and the kept ranges are
//! selected in a single pass via `select`/`aselect` - this stays efficient
//! even with hundreds of text cuts.

use crate::project::{FitMode, Project, Range, TrackKind, snap};
use crate::util::even;

pub const SAMPLE_RATE: u32 = 48_000;

#[derive(Clone, Debug)]
pub struct RenderSpec {
    pub width: u32,
    pub height: u32,
    /// Timeline range.
    pub from: f64,
    pub to: f64,
    pub video: bool,
    pub audio: bool,
    /// Overrides fit/fill of full-frame clips (e.g. "Fill" for a 9:16 export).
    pub fit_override: Option<FitMode>,
    /// "yuv420p" for export, "rgba" for the preview.
    pub pix_fmt: &'static str,
    /// Optional reduced output frame rate (preview).
    pub out_fps: Option<u32>,
    /// Normalize loudness to -16 LUFS (podcast standard).
    pub loudnorm: bool,
}

#[derive(Clone, Debug, Default)]
pub struct RenderGraph {
    pub args: Vec<String>,
    pub duration: f64,
}

struct Layer {
    label: String,
    x: String,
    y: String,
}

pub fn build(p: &Project, spec: &RenderSpec) -> RenderGraph {
    let fps = p.fps;
    let frame = p.frame();
    let half = frame * 0.5;
    let (w, h) = (spec.width.max(2), spec.height.max(2));
    let from = snap(spec.from.max(0.0), fps);
    let to = snap(spec.to, fps).max(from + frame);
    let dur = to - from;
    let window = Range::new(from, to);
    let ref_scale = h as f64 / 1080.0;

    let mut inputs: Vec<String> = Vec::new();
    let mut chains: Vec<String> = Vec::new();
    let mut vlayers: Vec<Layer> = Vec::new();
    let mut alayers: Vec<String> = Vec::new();
    let mut n_inputs = 0usize;

    let tracks: Vec<_> = p.video_tracks().into_iter().chain(p.audio_tracks()).collect();
    for track in tracks {
        for clip in p.clips_on(track.id) {
            let Some(media) = p.media(clip.source) else { continue };
            // Transitions to adjacent clips: extend beyond the edges if needed.
            let edges = p.clip_edges(clip);
            let all_segs = p.segments_with_edges(clip, &edges);
            let Some(&(ext_start, _)) = all_segs.first() else { continue };
            let ext_end = all_segs.last().map(|(t, r)| t + r.len()).unwrap_or(ext_start);
            // Kept pieces, clipped to the window: (timeline start, source range).
            let segs: Vec<(f64, Range)> = all_segs
                .into_iter()
                .filter_map(|(ts, r)| {
                    let tl = Range::new(ts, ts + r.len());
                    let wnd = tl.intersect(&window)?;
                    let s = snap(r.start + (wnd.start - ts), fps);
                    let e = snap(r.start + (wnd.end - ts), fps);
                    (e - s > half).then(|| (snap(wnd.start, fps), Range::new(s, e)))
                })
                .collect();
            if segs.is_empty() {
                continue;
            }
            let want_video = spec.video && track.kind == TrackKind::Video && media.has_video && !track.hidden;
            let want_audio =
                spec.audio && media.has_audio && !clip.muted && !track.muted && clip.volume > 0.001;
            if !want_video && !want_audio {
                continue;
            }

            let ss = segs[0].1.start;
            let last = segs.last().unwrap().1.end;
            let idx = n_inputs;
            n_inputs += 1;
            inputs.extend([
                "-ss".into(),
                format!("{ss:.6}"),
                "-t".into(),
                format!("{:.6}", last - ss + 2.0 * frame),
                "-i".into(),
                media.path.to_string_lossy().into_owned(),
            ]);

            let offset = segs[0].0 - from;
            let out_len: f64 = segs.iter().map(|(_, r)| r.len()).sum();
            // Fades at start/end: transitions take precedence over clip fades.
            // (video length, audio length) - relative to the start of this layer in the window.
            let (v_in, a_in) =
                if edges.start.is_some() { (edges.start.video_fade, edges.start.audio_fade) } else { (clip.fade_in, clip.fade_in) };
            let (v_out, a_out) =
                if edges.end.is_some() { (edges.end.video_fade, edges.end.audio_fade) } else { (clip.fade_out, clip.fade_out) };
            let fade_in = |d: f64| (d > 0.0 && ext_start >= from - half).then_some(d.min(out_len));
            let fade_out = |d: f64| {
                let st = ext_end - ext_start.max(from) - d;
                (d > 0.0 && ext_end <= to + half && st >= 0.0).then_some((st, d))
            };
            let (vfade_in, vfade_out) = (fade_in(v_in), fade_out(v_out));
            let (afade_in, afade_out) = (fade_in(a_in), fade_out(a_out));
            // Transitions use the configured curve, regular clip fades are linear.
            let curve_in = if edges.start.is_some() { edges.curve.afade_curve() } else { "tri" };
            let curve_out = if edges.end.is_some() { edges.curve.afade_curve() } else { "tri" };

            if want_video {
                let expr = segs
                    .iter()
                    .map(|(_, r)| format!("gte(t,{:.6})*lt(t,{:.6})", r.start - ss - half, r.end - ss - half))
                    .collect::<Vec<_>>()
                    .join("+");
                let t = &clip.transform;
                let fit = match (t.fit, spec.fit_override) {
                    (FitMode::Custom, _) => FitMode::Custom,
                    (f, None) => f,
                    (_, Some(o)) => o,
                };
                let (geom, x, y) = match fit {
                    FitMode::Fit => (
                        format!("scale={w}:{h}:force_original_aspect_ratio=decrease:force_divisible_by=2,setsar=1"),
                        "(main_w-overlay_w)/2".to_string(),
                        "(main_h-overlay_h)/2".to_string(),
                    ),
                    FitMode::Fill => (
                        format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1"),
                        "0".to_string(),
                        "0".to_string(),
                    ),
                    FitMode::Custom => {
                        let pw = even(w as f64 * t.scale.clamp(0.02, 4.0) as f64);
                        let mut g = format!("scale={pw}:-2,setsar=1");
                        let b = (t.border as f64 * ref_scale).round() as u32;
                        if b > 0 {
                            let [r, gr, bl] = t.border_color;
                            g += &format!(",pad=iw+{}:ih+{}:{b}:{b}:color=0x{r:02x}{gr:02x}{bl:02x}", 2 * b, 2 * b);
                        }
                        (g, format!("{}", (t.x as f64 * w as f64).round()), format!("{}", (t.y as f64 * h as f64).round()))
                    }
                };
                let mut alpha = String::new();
                if t.opacity < 0.999 {
                    alpha += &format!(",format=rgba,colorchannelmixer=aa={:.3}", t.opacity.max(0.0));
                } else if vfade_in.is_some() || vfade_out.is_some() {
                    alpha += ",format=yuva420p";
                }
                if let Some(d) = vfade_in {
                    alpha += &format!(",fade=t=in:st=0:d={d:.3}:alpha=1");
                }
                if let Some((st, d)) = vfade_out {
                    alpha += &format!(",fade=t=out:st={st:.3}:d={d:.3}:alpha=1");
                }
                let label = format!("v{}", vlayers.len());
                chains.push(format!(
                    "[{idx}:v]setpts=PTS-STARTPTS,fps={fps},select='{expr}',setpts=N/{fps}/TB,{geom}{alpha},setpts=PTS+{offset:.6}/TB[{label}]"
                ));
                vlayers.push(Layer { label, x, y });
            }

            if want_audio {
                let eps = 0.0005;
                let expr = segs
                    .iter()
                    .map(|(_, r)| format!("gte(t,{:.6})*lt(t,{:.6})", r.start - ss - eps, r.end - ss - eps))
                    .collect::<Vec<_>>()
                    .join("+");
                let mut post = String::new();
                if (clip.volume - 1.0).abs() > 0.001 {
                    post += &format!(",volume={:.3}", clip.volume);
                }
                if let Some(d) = afade_in {
                    post += &format!(",afade=t=in:st=0:d={d:.3}:curve={curve_in}");
                }
                if let Some((st, d)) = afade_out {
                    post += &format!(",afade=t=out:st={st:.3}:d={d:.3}:curve={curve_out}");
                }
                // Soft transitions: volume envelope that dips to 0 at every cut point.
                // Runs in 1 ms steps (asetnsamples=48).
                let mut envelope = String::new();
                let tr = p.transition;
                if tr.enabled && tr.ms > 0.0 {
                    let h = tr.ms as f64 / 2000.0;
                    let terms: Vec<String> = segs
                        .windows(2)
                        .filter(|w| tr.applies_to(clip.cut_kind_in(Range::new(w[0].1.end, w[1].1.start))))
                        .map(|w| tr.curve.expr(&format!("min(1,abs(t-{:.6})/{h:.6})", w[1].0 - segs[0].0)))
                        .collect();
                    if !terms.is_empty() {
                        envelope = format!(",volume=volume='{}':eval=frame", terms.join("*"));
                    }
                }
                let delay = (offset * SAMPLE_RATE as f64).round() as u64;
                let label = format!("a{}", alayers.len());
                chains.push(format!(
                    "[{idx}:a]asetpts=PTS-STARTPTS,aresample={SAMPLE_RATE},aformat=sample_fmts=fltp:channel_layouts=stereo,\
                     asetnsamples=n=48:p=0,aselect='{expr}',asetpts=N/SR/TB{envelope}{post},adelay={delay}S:all=1[{label}]"
                ));
                alayers.push(label);
            }
        }
    }

    // Subtitles as an image sequence (drawn in Rust, see subtitles.rs)
    let mut subs: Option<(usize, u32)> = None;
    if spec.video {
        match crate::subtitles::overlay(p, w, h, from, to) {
            Ok(Some(o)) => {
                inputs.extend(["-f", "concat", "-safe", "0", "-i"].map(String::from));
                inputs.push(o.list.to_string_lossy().into_owned());
                subs = Some((n_inputs, o.y));
            }
            Ok(None) => {}
            Err(e) => eprintln!("Subtitles: {e:#}"),
        }
    }

    if spec.video {
        chains.push(format!("color=c=black:s={w}x{h}:r={fps}:d={dur:.6},format=yuv420p[bg0]"));
        for (k, layer) in vlayers.iter().enumerate() {
            chains.push(format!(
                "[bg{k}][{}]overlay=x={}:y={}:eof_action=pass:repeatlast=0[bg{}]",
                layer.label,
                layer.x,
                layer.y,
                k + 1
            ));
        }
        let mut last = format!("bg{}", vlayers.len());
        if let Some((idx, y)) = subs {
            chains.push(format!("[{idx}:v]format=rgba[subs];[{last}][subs]overlay=x=0:y={y}:eof_action=pass[bgsub]"));
            last = "bgsub".into();
        }
        let rate = spec.out_fps.filter(|f| *f < fps).map(|f| format!("fps={f},")).unwrap_or_default();
        chains.push(format!("[{last}]{rate}format={}[vout]", spec.pix_fmt));
    }

    if spec.audio {
        let base = format!("anullsrc=r={SAMPLE_RATE}:cl=stereo,atrim=duration={dur:.6}");
        if alayers.is_empty() {
            chains.push(format!("{base}[aout]"));
        } else {
            chains.push(format!("{base}[abase]"));
            let ins: String = alayers.iter().map(|l| format!("[{l}]")).collect();
            let norm = if spec.loudnorm { ",loudnorm=I=-16:TP=-1.5:LRA=11,aresample=48000" } else { "" };
            chains.push(format!(
                "[abase]{ins}amix=inputs={}:normalize=0:duration=first:dropout_transition=0{norm}[aout]",
                alayers.len() + 1
            ));
        }
    }

    let mut args = inputs;
    args.push("-filter_complex".into());
    args.push(chains.join(";"));
    RenderGraph { args, duration: dur }
}

/// Preview size for a given height, matching the project aspect ratio.
pub fn preview_size(p: &Project, height: u32) -> (u32, u32) {
    let h = height.min(p.height.max(2));
    let w = even(p.width as f64 * h as f64 / p.height.max(1) as f64);
    (w, even(h as f64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{CutKind, MediaKind, MediaSource};
    use std::path::PathBuf;
    use std::process::Command;

    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn source(id: u64, path: PathBuf, duration: f64) -> MediaSource {
        MediaSource {
            id,
            path,
            name: "t".into(),
            kind: MediaKind::Video,
            duration,
            width: 640,
            height: 360,
            fps: 30.0,
            has_video: true,
            has_audio: true,
            transcript: None,
            silences: None,
            waveform: None,
        }
    }

    #[test]
    fn graph_contains_expected_parts() {
        let mut p = Project::default();
        let id = p.alloc_id();
        p.media.push(source(id, "a.mp4".into(), 10.0));
        let main = p.main_track();
        let c = p.add_clip(id, main, 0.0).unwrap();
        p.clip_mut(c).unwrap().add_cut(Range::new(2.0, 4.0), CutKind::Text);
        let v2 = p.video_tracks()[1].id;
        p.add_clip(id, v2, 1.0).unwrap();
        let spec = RenderSpec {
            width: 640,
            height: 360,
            from: 0.0,
            to: p.duration(),
            video: true,
            audio: true,
            fit_override: None,
            pix_fmt: "yuv420p",
            out_fps: None,
            loudnorm: false,
        };
        let g = build(&p, &spec);
        let fc = g.args.last().unwrap();
        assert_eq!(g.args.iter().filter(|a| *a == "-i").count(), 2);
        assert!(fc.contains("select='gte(t,-0.016667)*lt(t,1.983333)+gte(t,3.983333)*lt(t,9.983333)'"), "{fc}");
        assert!(fc.contains("overlay=x="));
        assert!(fc.contains("amix=inputs=3"));
        assert!(fc.contains("adelay=48000S"), "PiP audio starts at 1 s");
    }

    /// Transitions between clips: dissolve mixes picture & sound, "dip to black"
    /// fades out - both without changing the timeline length.
    #[test]
    fn clip_transitions_render() {
        if !ffmpeg_available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tcf_xfade_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, color: &str, freq: u32| {
            let path = dir.join(name);
            assert!(Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("color=c={color}:s=64x36:r=30:d=4")])
                .args(["-f", "lavfi", "-i", &format!("sine=frequency={freq}:sample_rate=48000:duration=4")])
                .args(["-c:v", "libx264", "-pix_fmt", "yuv444p", "-crf", "0", "-c:a", "pcm_s16le", "-shortest"])
                .arg(&path)
                .status()
                .unwrap()
                .success());
            path
        };
        let red = make("red.mov", "red", 440);
        let blue = make("blue.mov", "blue", 880);
        let mut p = Project::default();
        p.canvas_auto = false;
        p.media.push(source(1, red, 4.0));
        p.media.push(source(2, blue, 4.0));
        p.next_id = 10;
        let main = p.main_track();
        let a = p.add_clip(1, main, 0.0).unwrap();
        let b = p.add_clip(2, main, 0.0).unwrap();
        for id in [a, b] {
            let c = p.clip_mut(id).unwrap();
            c.src_in = 1.0;
            c.src_out = 3.0;
        }
        p.repack_main();
        let spec = RenderSpec {
            width: 64,
            height: 36,
            from: 0.0,
            to: p.duration(),
            video: true,
            audio: true,
            fit_override: None,
            pix_fmt: "yuv420p",
            out_fps: None,
            loudnorm: false,
        };
        let render = |p: &Project, name: &str| -> std::path::PathBuf {
            let out = dir.join(name);
            let g = build(p, &spec);
            let r = Command::new("ffmpeg")
                .args(["-v", "error", "-y"])
                .args(&g.args)
                .args(["-map", "[vout]", "-map", "[aout]", "-c:v", "libx264", "-crf", "0", "-c:a", "pcm_s16le"])
                .arg(&out)
                .output()
                .unwrap();
            assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
            out
        };
        let rgb_at = |path: &std::path::Path, t: f64| -> (f64, f64, f64) {
            let o = Command::new("ffmpeg")
                .args(["-v", "error", "-ss", &t.to_string(), "-i"])
                .arg(path)
                .args(["-frames:v", "1", "-vf", "scale=1:1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
                .output()
                .unwrap();
            (o.stdout[0] as f64, o.stdout[1] as f64, o.stdout[2] as f64)
        };
        let rms_at = |path: &std::path::Path, t: f64| -> f64 {
            let o = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(path)
                .args(["-vn", "-ac", "1", "-f", "s16le", "pipe:1"])
                .output()
                .unwrap();
            let s: Vec<f64> = o.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f64).collect();
            let i = (t * 48000.0) as usize;
            (s[i - 480..i + 480].iter().map(|x| x * x).sum::<f64>() / 960.0).sqrt()
        };

        p.clip_transition = crate::project::ClipTransition { video: true, audio: true, ms: 1000.0, ..Default::default() };
        assert!((p.duration() - 4.0).abs() < 1e-9);
        let out = render(&p, "dissolve.mov");
        let (r, _, b) = rgb_at(&out, 1.25);
        assert!(r > 200.0 && b < 60.0, "red before: {r} {b}");
        let (r, _, b) = rgb_at(&out, 2.0);
        assert!(r > 80.0 && r < 175.0 && b > 80.0 && b < 175.0, "mixed in the middle: {r} {b}");
        let (r, _, b) = rgb_at(&out, 2.75);
        assert!(b > 200.0 && r < 60.0, "blue after: {r} {b}");
        let full = rms_at(&out, 0.5);
        assert!(rms_at(&out, 2.0) > full * 0.6, "audio crossfades, no gap");
        let probe = crate::media::probe(&crate::media::Tools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() }, &out, 9).unwrap();
        assert!((probe.duration - 4.0).abs() < 0.05, "length unchanged: {}", probe.duration);

        p.clip_transition.style = crate::project::TransitionStyle::DipToBlack;
        let out = render(&p, "dip.mov");
        let (r, g, b) = rgb_at(&out, 2.0);
        assert!(r + g + b < 40.0, "black at the cut point: {r} {g} {b}");
        assert!(rms_at(&out, 2.0) < full * 0.1, "and silent");
        let (r, _, _) = rgb_at(&out, 0.5);
        assert!(r > 200.0, "unchanged outside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Soft transitions: the level dips to 0 at the cut and is full elsewhere.
    #[test]
    fn soft_transition_dips_audio_at_cut() {
        if !ffmpeg_available() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tcf_fade_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("tone.wav");
        assert!(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=4"])
            .arg(&src)
            .status()
            .unwrap()
            .success());
        let mut p = Project::new(crate::project::ProjectMode::Audio);
        let id = p.alloc_id();
        let mut m = source(id, src, 4.0);
        m.kind = MediaKind::Audio;
        m.has_video = false;
        p.media.push(m);
        let main = p.main_track();
        let c = p.add_clip(id, main, 0.0).unwrap();
        p.clip_mut(c).unwrap().add_cut(Range::new(1.0, 2.0), CutKind::Text);
        p.repack_main();
        p.transition = crate::project::CutTransition {
            enabled: true,
            ms: 200.0,
            curve: crate::project::FadeCurve::Linear,
            text_cuts: true,
            silence_cuts: true,
        };
        let spec = RenderSpec {
            width: 2,
            height: 2,
            from: 0.0,
            to: p.duration(),
            video: false,
            audio: true,
            fit_override: None,
            pix_fmt: "yuv420p",
            out_fps: None,
            loudnorm: false,
        };
        let g = build(&p, &spec);
        assert!(g.args.last().unwrap().contains("eval=frame"));
        let out = Command::new("ffmpeg")
            .args(["-v", "error"])
            .args(&g.args)
            .args(["-map", "[aout]", "-ac", "1", "-f", "s16le", "pipe:1"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let samples: Vec<i16> = out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        assert!((samples.len() as f64 / 48000.0 - 3.0).abs() < 0.01, "length unchanged");
        // Peak level in a 4 ms window around t
        let peak = |t: f64| {
            let i = (t * 48000.0) as usize;
            samples[i - 96..i + 96].iter().map(|s| s.unsigned_abs()).max().unwrap() as f64
        };
        let full = peak(0.5);
        assert!(full > 1000.0, "base level {full}");
        assert!(peak(1.0) < full * 0.06, "silent at the cut: {} vs {full}", peak(1.0));
        assert!((peak(0.95) / full - 0.5).abs() < 0.08, "half faded out: {}", peak(0.95) / full);
        assert!((peak(1.05) / full - 0.5).abs() < 0.08, "half faded in: {}", peak(1.05) / full);
        assert!(peak(1.2) > full * 0.97, "full again afterwards");

        // Disabled: no dip
        p.transition.enabled = false;
        let g = build(&p, &spec);
        assert!(!g.args.last().unwrap().contains("eval=frame"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// End-to-end: real rendering with ffmpeg, checks the resulting duration.
    #[test]
    fn renders_with_ffmpeg() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg missing - test skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("tcf_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("src.mp4");
        let ok = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=30:duration=6"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6", "-shortest"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&src)
            .status()
            .unwrap()
            .success();
        assert!(ok);

        let mut p = Project::default();
        let id = p.alloc_id();
        p.media.push(source(id, src.clone(), 6.0));
        let main = p.main_track();
        let c = p.add_clip(id, main, 0.0).unwrap();
        {
            let clip = p.clip_mut(c).unwrap();
            clip.add_cut(Range::new(1.0, 2.0), CutKind::Text);
            clip.add_cut(Range::new(3.0, 3.5), CutKind::Silence);
            clip.fade_out = 0.5;
        }
        let v2 = p.video_tracks()[1].id;
        let pip = p.add_clip(id, v2, 0.5).unwrap();
        p.clip_mut(pip).unwrap().src_out = 2.0;
        p.clip_mut(pip).unwrap().transform.opacity = 0.7;
        p.repack_main();
        let expected = p.duration();
        assert!((expected - 4.5).abs() < 1e-6);

        let spec = RenderSpec {
            width: 320,
            height: 180,
            from: 0.0,
            to: expected,
            video: true,
            audio: true,
            fit_override: None,
            pix_fmt: "yuv420p",
            out_fps: None,
            loudnorm: false,
        };
        let g = build(&p, &spec);
        let out = dir.join("out.mp4");
        let status = Command::new("ffmpeg")
            .args(["-v", "error", "-y"])
            .args(&g.args)
            .args(["-map", "[vout]", "-map", "[aout]", "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac"])
            .args(["-t", &format!("{expected}")])
            .arg(&out)
            .output()
            .unwrap();
        assert!(status.status.success(), "{}", String::from_utf8_lossy(&status.stderr));

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-count_frames", "-show_entries"])
            .args(["stream=nb_read_frames", "-of", "csv=p=0"])
            .arg(&out)
            .output()
            .unwrap();
        let frames: u32 = String::from_utf8_lossy(&probe.stdout).trim().parse().unwrap();
        assert_eq!(frames, 135, "4.5 s × 30 fps");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
