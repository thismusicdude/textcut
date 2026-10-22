//! Export to various sizes/formats as well as subtitles (SRT).

use crate::media::Tools;
use crate::project::{FitMode, Project};
use crate::render::{self, RenderSpec};
use crate::util::{command, even, fmt_srt_time};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// (display name, file suffix, width, height). 0×0 = project size.
pub const PRESETS: &[(&str, &str, u32, u32)] = &[
    ("Project size (original)", "original", 0, 0),
    ("4K UHD – 3840×2160", "4k", 3840, 2160),
    ("1440p – 2560×1440", "1440p", 2560, 1440),
    ("Full HD – 1920×1080", "1080p", 1920, 1080),
    ("HD – 1280×720", "720p", 1280, 720),
    ("SD – 854×480", "480p", 854, 480),
    ("Vertical 9:16 – 1080×1920 (Reels/TikTok/Shorts)", "9x16", 1080, 1920),
    ("Square 1:1 – 1080×1080", "1x1", 1080, 1080),
    ("Portrait 4:5 – 1080×1350", "4x5", 1080, 1350),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
    H264Hardware,
    ProRes,
}

impl Codec {
    pub const ALL: [Codec; 4] = [Codec::H264, Codec::H265, Codec::H264Hardware, Codec::ProRes];

    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264 (MP4, compatible)",
            Codec::H265 => "H.265/HEVC (MP4, smaller)",
            Codec::H264Hardware => "H.264 hardware (fast, macOS)",
            Codec::ProRes => "ProRes 422 (MOV, editing quality)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Codec::ProRes => "mov",
            _ => "mp4",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AspectHandling {
    /// As set on the clip.
    AsClip,
    /// Whole picture with bars.
    Fit,
    /// Crop to fill the frame.
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    M4a,
    Wav,
    Flac,
}

impl AudioFormat {
    pub const ALL: [AudioFormat; 4] = [AudioFormat::Mp3, AudioFormat::M4a, AudioFormat::Wav, AudioFormat::Flac];

    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "MP3",
            AudioFormat::M4a => "M4A (AAC)",
            AudioFormat::Wav => "WAV (uncompressed)",
            AudioFormat::Flac => "FLAC (lossless)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::M4a => "m4a",
            AudioFormat::Wav => "wav",
            AudioFormat::Flac => "flac",
        }
    }

    /// Lossy → bitrate adjustable.
    pub fn lossy(self) -> bool {
        matches!(self, AudioFormat::Mp3 | AudioFormat::M4a)
    }

    fn codec_args(self, kbps: u32) -> Vec<String> {
        let b = format!("{kbps}k");
        let v: Vec<&str> = match self {
            AudioFormat::Mp3 => vec!["-c:a", "libmp3lame", "-b:a", &b],
            AudioFormat::M4a => vec!["-c:a", "aac", "-b:a", &b, "-movflags", "+faststart"],
            AudioFormat::Wav => vec!["-c:a", "pcm_s16le"],
            AudioFormat::Flac => vec!["-c:a", "flac"],
        };
        v.into_iter().map(String::from).collect()
    }
}

#[derive(Clone, Debug)]
pub enum ExportKind {
    Video { width: u32, height: u32, codec: Codec, crf: u32, aspect: AspectHandling },
    Audio { format: AudioFormat, kbps: u32 },
    Subtitles,
}

#[derive(Clone, Debug)]
pub struct ExportJob {
    pub label: String,
    pub kind: ExportKind,
    pub path: PathBuf,
    /// Normalize loudness to -16 LUFS.
    pub loudnorm: bool,
}

/// Runs an export. `progress` receives 0..1.
pub fn run(tools: &Tools, p: &Project, job: &ExportJob, progress: &dyn Fn(f32), cancel: &Arc<AtomicBool>) -> Result<()> {
    let duration = p.duration();
    if duration <= 0.0 {
        bail!("The timeline is empty.");
    }
    if let Some(dir) = job.path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (spec, codec_args): (RenderSpec, Vec<String>) = match &job.kind {
        ExportKind::Subtitles => {
            std::fs::write(&job.path, build_srt(p))?;
            progress(1.0);
            return Ok(());
        }
        ExportKind::Video { width, height, codec, crf, aspect } => {
            let (w, h) = if *width == 0 { (p.width, p.height) } else { (*width, *height) };
            let spec = RenderSpec {
                width: even(w as f64),
                height: even(h as f64),
                from: 0.0,
                to: duration,
                video: true,
                audio: true,
                fit_override: match aspect {
                    AspectHandling::AsClip => None,
                    AspectHandling::Fit => Some(FitMode::Fit),
                    AspectHandling::Fill => Some(FitMode::Fill),
                },
                pix_fmt: if *codec == Codec::ProRes { "yuv422p10le" } else { "yuv420p" },
                out_fps: None,
                loudnorm: job.loudnorm,
            };
            let mut a: Vec<String> = vec!["-map".into(), "[vout]".into(), "-map".into(), "[aout]".into()];
            let crf = crf.to_string();
            let v: Vec<&str> = match codec {
                Codec::H264 => vec!["-c:v", "libx264", "-preset", "medium", "-crf", &crf, "-c:a", "aac", "-b:a", "192k"],
                Codec::H265 => {
                    vec!["-c:v", "libx265", "-preset", "medium", "-crf", &crf, "-tag:v", "hvc1", "-c:a", "aac", "-b:a", "192k"]
                }
                Codec::H264Hardware => vec!["-c:v", "h264_videotoolbox", "-q:v", "65", "-c:a", "aac", "-b:a", "192k"],
                Codec::ProRes => vec!["-c:v", "prores_ks", "-profile:v", "2", "-c:a", "pcm_s16le"],
            };
            a.extend(v.into_iter().map(String::from));
            if codec.extension() == "mp4" {
                a.extend(["-movflags".into(), "+faststart".into()]);
            }
            a.extend(["-r".into(), p.fps.to_string()]);
            (spec, a)
        }
        ExportKind::Audio { format, kbps } => {
            let spec = RenderSpec {
                width: 2,
                height: 2,
                from: 0.0,
                to: duration,
                video: false,
                audio: true,
                fit_override: None,
                pix_fmt: "yuv420p",
                out_fps: None,
                loudnorm: job.loudnorm,
            };
            let mut a: Vec<String> = vec!["-map".into(), "[aout]".into()];
            a.extend(format.codec_args(*kbps));
            (spec, a)
        }
    };

    let graph = render::build(p, &spec);
    let mut child = command(&tools.ffmpeg)
        .args(["-v", "error", "-nostdin", "-y", "-progress", "pipe:1", "-nostats"])
        .args(&graph.args)
        .args(&codec_args)
        .args(["-t", &format!("{:.6}", graph.duration)])
        .arg(&job.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start ffmpeg")?;

    let mut stderr = child.stderr.take().unwrap();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let stdout = child.stdout.take().unwrap();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&job.path);
            bail!("Cancelled");
        }
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
            progress(((us / 1e6) / graph.duration).clamp(0.0, 0.999) as f32);
        }
    }
    let status = child.wait()?;
    let err = err_thread.join().unwrap_or_default();
    if !status.success() {
        bail!("ffmpeg error:\n{}", err.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    progress(1.0);
    Ok(())
}

/// Subtitles from the main track's transcript - with the timings after cutting.
/// Same lines as when burning in (character limit from the subtitle style).
pub fn build_srt(p: &Project) -> String {
    crate::subtitles::cues(p, p.subtitles.max_chars)
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}\n{} --> {}\n{}\n\n", i + 1, fmt_srt_time(c.start), fmt_srt_time(c.end), c.text()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{MediaKind, MediaSource, Transcript, Word};

    #[test]
    fn srt_follows_cuts() {
        let mut p = Project::default();
        let id = p.alloc_id();
        let words = ["Hallo", "liebe", "Leute.", "Heute", "schneiden", "wir."];
        p.media.push(MediaSource {
            id,
            path: "x.mp4".into(),
            name: "x".into(),
            kind: MediaKind::Video,
            duration: 10.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_video: true,
            has_audio: true,
            transcript: Some(Transcript {
                language: "de".into(),
                words: words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| Word { text: w.to_string(), start: i as f64, end: i as f64 + 0.8, core: None })
                    .collect(),
                ..Default::default()
            }),
            silences: None,
            waveform: None,
        });
        let main = p.main_track();
        let c = p.add_clip(id, main, 0.0).unwrap();
        p.set_tokens_deleted(c, 1, 1, true); // remove "liebe"
        let srt = build_srt(&p);
        // "liebe" (1.0-2.0) with padding → 1.0333-1.9667 removed (0.9333 s);
        // the line stays 0.4 s longer but ends before the next one (2.067 - 0.04).
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:02,027\nHallo Leute.\n"), "{srt}");
        assert!(srt.contains("2\n00:00:02,067 --> "), "{srt}");
    }
}

#[cfg(test)]
mod e2e {
    use super::*;
    use crate::media::{Tools, detect_silence, probe};
    use std::process::Command;

    fn has_ffmpeg() -> bool {
        Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn frame_rgb(path: &std::path::Path, t: f64, w: u32, h: u32) -> Vec<u8> {
        let out = Command::new("ffmpeg")
            .args(["-v", "error", "-ss", &t.to_string(), "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"])
            .output()
            .unwrap();
        assert_eq!(out.stdout.len(), (w * h * 3) as usize);
        out.stdout
    }

    /// Mean brightness of an image region.
    fn region_mean(px: &[u8], w: u32, x0: u32, y0: u32, x1: u32, y1: u32) -> f64 {
        let mut sum = 0u64;
        let mut n = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w + x) * 3) as usize;
                sum += px[i] as u64 + px[i + 1] as u64 + px[i + 2] as u64;
                n += 3;
            }
        }
        sum as f64 / n as f64
    }

    #[test]
    fn vertical_export_with_pip_and_silence() {
        if !has_ffmpeg() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tcf_e2e_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tools = Tools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() };

        // Main video: dark gray, audio: alternating 1 s tone / 1 s silence (6 s).
        let main_src = dir.join("main.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=0x202020:s=640x360:r=30:d=6"])
            .args(["-f", "lavfi", "-i", "aevalsrc=if(lt(mod(t\\,2)\\,1)\\,0.5*sin(2*PI*440*t)\\,0):s=48000:d=6"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&main_src)
            .status()
            .unwrap()
            .success());
        // PiP: white
        let pip_src = dir.join("pip.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=white:s=320x180:r=30:d=6"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&pip_src)
            .status()
            .unwrap()
            .success());

        let mut p = Project::default();
        let mut m = probe(&tools, &main_src, 1).unwrap();
        assert!(m.has_audio && m.has_video && (m.duration - 6.0).abs() < 0.1);
        m.silences = Some(detect_silence(&tools, &main_src, -35.0, 0.5, m.duration).unwrap());
        let sil = m.silences.clone().unwrap();
        assert_eq!(sil.len(), 3, "{sil:?}");
        assert!((sil[0].start - 1.0).abs() < 0.05 && (sil[0].end - 2.0).abs() < 0.05, "{sil:?}");
        p.media.push(m);
        p.media.push(probe(&tools, &pip_src, 2).unwrap());
        p.next_id = 10;

        let main = p.main_track();
        let c = p.add_clip(1, main, 0.0).unwrap();
        let v2 = p.video_tracks()[1].id;
        let pip = p.add_clip(2, v2, 1.0).unwrap();
        p.clip_mut(pip).unwrap().src_out = 1.0; // PiP visible from 1.0 to 2.0
        p.clip_mut(pip).unwrap().transform.border = 0.0;
        p.place_pip(pip, crate::project::PipPreset::TopLeft);

        // Remove silence (no padding): 6 s → 3 s
        assert_eq!(p.apply_silence_removal(c, 0.0, 0.5), 3);
        let d = p.duration();
        assert!((d - 3.0).abs() < 0.07, "duration {d}");

        let job = ExportJob {
            label: "9x16".into(),
            kind: ExportKind::Video { width: 360, height: 640, codec: Codec::H264, crf: 23, aspect: AspectHandling::Fill },
            path: dir.join("out.mp4"),
            loudnorm: false,
        };
        run(&tools, &p, &job, &|_| {}, &Arc::new(AtomicBool::new(false))).unwrap();
        let out = probe(&tools, &job.path, 99).unwrap();
        assert_eq!((out.width, out.height), (360, 640));
        assert!((out.duration - d).abs() < 0.1, "{} vs {d}", out.duration);

        // PiP top left: not there yet at 0.5 s, visible (white) at 1.5 s, gone again at 2.5 s.
        let pip_t = p.clip(pip).unwrap().transform;
        let (x0, y0) = ((pip_t.x * 360.0) as u32 + 4, (pip_t.y * 640.0) as u32 + 4);
        let region = |t: f64| region_mean(&frame_rgb(&job.path, t, 360, 640), 360, x0, y0, x0 + 20, y0 + 20);
        assert!(region(0.5) < 80.0, "PiP visible too early");
        assert!(region(1.5) > 200.0, "PiP missing");
        assert!(region(2.5) < 80.0, "PiP visible too long");

        // Audio export + subtitle job run as well
        // Audio export in all formats (one of them with loudness normalization)
        for (i, format) in AudioFormat::ALL.into_iter().enumerate() {
            let job = ExportJob {
                label: format.label().into(),
                kind: ExportKind::Audio { format, kbps: 192 },
                path: dir.join(format!("out.{}", format.extension())),
                loudnorm: i == 1,
            };
            run(&tools, &p, &job, &|_| {}, &Arc::new(AtomicBool::new(false))).unwrap();
            let a = probe(&tools, &job.path, 98).unwrap();
            assert!(a.has_audio && !a.has_video);
            assert!((a.duration - d).abs() < 0.15, "{format:?}: {}", a.duration);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
