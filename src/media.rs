//! Media analysis via ffprobe/ffmpeg: metadata, waveform, silence detection.

use crate::project::{MediaKind, MediaSource, Range};
use crate::util::command;
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(Clone, Debug)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

/// Reads duration, resolution, frame rate and available streams.
pub fn probe(tools: &Tools, path: &Path, id: u64) -> Result<MediaSource> {
    let out = command(&tools.ffprobe)
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .with_context(|| format!("Could not start ffprobe ({})", tools.ffprobe.display()))?;
    if !out.status.success() {
        bail!("ffprobe: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let json: Value = serde_json::from_slice(&out.stdout)?;
    let streams = json["streams"].as_array().cloned().unwrap_or_default();

    // Cover images in audio files do not count as video.
    let video = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64().unwrap_or(0) == 0
    });
    let audio = streams.iter().find(|s| s["codec_type"] == "audio");
    if video.is_none() && audio.is_none() {
        bail!("No video or audio track found");
    }

    let parse_f = |v: &Value| v.as_str().and_then(|s| s.parse::<f64>().ok()).or_else(|| v.as_f64());
    let mut duration = parse_f(&json["format"]["duration"]).unwrap_or(0.0);
    if duration <= 0.0 {
        duration = streams.iter().filter_map(|s| parse_f(&s["duration"])).fold(0.0, f64::max);
    }
    if duration <= 0.0 {
        bail!("Could not determine the file duration");
    }

    let (mut width, mut height, mut fps) = (0, 0, 0.0);
    if let Some(v) = video {
        width = v["width"].as_u64().unwrap_or(0) as u32;
        height = v["height"].as_u64().unwrap_or(0) as u32;
        // Rotated portrait phone videos: swap width/height.
        let rotation = v["side_data_list"]
            .as_array()
            .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64()))
            .or_else(|| v["tags"]["rotate"].as_str().and_then(|r| r.parse().ok()))
            .unwrap_or(0.0);
        if (rotation.abs() - 90.0).abs() < 1.0 || (rotation.abs() - 270.0).abs() < 1.0 {
            std::mem::swap(&mut width, &mut height);
        }
        fps = parse_rate(v["avg_frame_rate"].as_str().unwrap_or(""))
            .or_else(|| parse_rate(v["r_frame_rate"].as_str().unwrap_or("")))
            .unwrap_or(30.0);
    }

    Ok(MediaSource {
        id,
        path: path.to_path_buf(),
        name: crate::util::file_stem(path),
        kind: if video.is_some() { MediaKind::Video } else { MediaKind::Audio },
        duration,
        width,
        height,
        fps,
        has_video: video.is_some(),
        has_audio: audio.is_some(),
        transcript: None,
        silences: None,
        waveform: None,
    })
}

fn parse_rate(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

/// Level peaks at 100 values per second.
pub fn waveform(tools: &Tools, path: &Path) -> Result<Vec<f32>> {
    let mut child = command(&tools.ffmpeg)
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "4000", "-f", "s16le", "-acodec", "pcm_s16le", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut bytes = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut bytes)?;
    child.wait()?;
    const PER_PEAK: usize = 40; // 4000 Hz / 100
    let samples: Vec<i16> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let mut peaks: Vec<f32> = samples
        .chunks(PER_PEAK)
        .map(|c| c.iter().map(|s| (*s as i32).unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0)
        .collect();
    // Some compression so that quiet speech stays visible.
    for p in &mut peaks {
        *p = p.sqrt().min(1.0);
    }
    Ok(peaks)
}

/// Detects silence via `silencedetect`. `threshold_db` e.g. -35, `min_len` in s.
pub fn detect_silence(tools: &Tools, path: &Path, threshold_db: f32, min_len: f64, duration: f64) -> Result<Vec<Range>> {
    let filter = format!("silencedetect=noise={threshold_db}dB:d={min_len}");
    let out = command(&tools.ffmpeg)
        .args(["-hide_banner", "-nostats", "-i"])
        .arg(path)
        .args(["-vn", "-af", &filter, "-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("ffmpeg silencedetect: {}", err.lines().last().unwrap_or(""));
    }
    Ok(parse_silences(&String::from_utf8_lossy(&out.stderr), duration))
}

pub fn parse_silences(log: &str, duration: f64) -> Vec<Range> {
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    for line in log.lines() {
        if let Some(v) = value_after(line, "silence_start:") {
            open = Some(v.max(0.0));
        } else if let Some(v) = value_after(line, "silence_end:") {
            let start = open.take().unwrap_or(0.0);
            if v > start {
                out.push(Range::new(start, v));
            }
        }
    }
    // Silence up to the end of the file has no silence_end.
    if let Some(start) = open {
        if duration > start {
            out.push(Range::new(start, duration));
        }
    }
    out
}

fn value_after(line: &str, key: &str) -> Option<f64> {
    let idx = line.find(key)?;
    line[idx + key.len()..].split_whitespace().next()?.trim_end_matches('|').parse().ok()
}

/// Extracts 16 kHz mono WAV for Whisper.
pub fn extract_wav16k(tools: &Tools, path: &Path, out: &Path) -> Result<()> {
    let status = command(&tools.ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !status.status.success() {
        return Err(anyhow!("Audio extraction failed: {}", String::from_utf8_lossy(&status.stderr).trim()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_silencedetect_output() {
        let log = "\
[silencedetect @ 0x1] silence_start: 1.234
[silencedetect @ 0x1] silence_end: 2.5 | silence_duration: 1.266
[silencedetect @ 0x1] silence_start: -0.01
[silencedetect @ 0x1] silence_end: 0.4 | silence_duration: 0.41
[silencedetect @ 0x1] silence_start: 8.0
";
        let s = parse_silences(log, 10.0);
        assert_eq!(s, vec![Range::new(1.234, 2.5), Range::new(0.0, 0.4), Range::new(8.0, 10.0)]);
    }

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate("30000/1001").map(|r| (r * 100.0).round()), Some(2997.0));
        assert_eq!(parse_rate("0/0"), None);
    }
}
