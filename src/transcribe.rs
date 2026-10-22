//! Spracherkennung mit whisper.cpp (`whisper-cli`) – lokal, offline.
//!
//! Ablauf: Audio als 16 kHz-WAV extrahieren → `whisper-cli` mit
//! Wort-Zeitstempeln (`-ml 1 -sow`) aufrufen → JSON einlesen.

use crate::media::{Tools, extract_wav16k};
use crate::project::{Transcript, Word};
use crate::util::{command, models_dir};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Verfügbare Modelle: (Name, ungefähre Größe in MB).
pub const MODELS: &[(&str, u64)] = &[
    ("tiny", 75),
    ("base", 142),
    ("small", 466),
    ("medium", 1500),
    ("large-v3-turbo", 1620),
];

pub fn model_path(name: &str) -> PathBuf {
    models_dir().join(format!("ggml-{name}.bin"))
}

pub fn model_url(name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{name}.bin")
}

pub struct WhisperConfig {
    pub binary: PathBuf,
    pub model: PathBuf,
    /// "auto", "de", "en", …
    pub language: String,
    pub threads: usize,
    /// Start-Prompt, z.B. mit Füllwörtern für wortgetreue Transkription.
    pub prompt: Option<String>,
    /// DTW-Voreinstellung passend zum Modell (genauere Wortzeiten).
    pub dtw: Option<String>,
}

/// whisper.cpp-DTW-Voreinstellung für eine Modelldatei (`ggml-small.bin` → `small`).
pub fn dtw_preset(model: &Path) -> Option<String> {
    let stem = model.file_stem()?.to_str()?.strip_prefix("ggml-")?;
    let name = stem.split(['-', '.']).collect::<Vec<_>>();
    let preset = match name.as_slice() {
        ["tiny"] | ["tiny", "en"] | ["base"] | ["base", "en"] | ["small"] | ["small", "en"] | ["medium"] | ["medium", "en"] => {
            name.join(".")
        }
        ["large", "v1"] | ["large", "v2"] | ["large", "v3"] | ["large", "v3", "turbo"] => name.join("."),
        _ => return None,
    };
    Some(preset)
}

/// Prompt mit Füllwörtern: Whisper übernimmt den Stil und schreibt „äh“, „ähm“,
/// Wiederholungen und Versprecher mit, statt sie zu glätten.
pub fn verbatim_prompt(language: &str) -> Option<&'static str> {
    Some(match language {
        "de" => "Ähm, also, äh, ich meine, hm, ähm, ja. Äh, genau, ähm.",
        "en" => "Um, so, uh, I mean, hmm, um, yeah. Uh, like, right, um.",
        "fr" => "Euh, alors, euh, je veux dire, hum, euh, oui. Bah, voilà, euh.",
        "es" => "Eh, bueno, este, o sea, mmm, eh, sí. Pues, eh, vale.",
        "it" => "Ehm, allora, eh, cioè, mmm, ehm, sì. Ecco, eh, insomma.",
        "nl" => "Eh, dus, uh, ik bedoel, hm, eh, ja. Nou, eh, precies.",
        _ => return None,
    })
}

/// Transkribiert eine Mediendatei. `progress` erhält Werte von 0..1.
pub fn transcribe(tools: &Tools, cfg: &WhisperConfig, media: &Path, progress: &dyn Fn(f32)) -> Result<Transcript> {
    if !cfg.model.is_file() {
        bail!("Whisper-Modell nicht gefunden: {} – bitte in den Einstellungen herunterladen.", cfg.model.display());
    }
    let tmp = crate::util::cache_dir().join(format!("tcf_{}_{}", std::process::id(), unique()));
    let wav = tmp.with_extension("wav");
    progress(0.02);
    extract_wav16k(tools, media, &wav)?;
    progress(0.08);

    let mut child = command(&cfg.binary)
        .arg("-m")
        .arg(&cfg.model)
        .arg("-f")
        .arg(&wav)
        .args(cfg.prompt.iter().flat_map(|p| ["--prompt", p.as_str()]))
        // DTW braucht volle JSON-Ausgabe und verträgt sich nicht mit Flash-Attention.
        .args(cfg.dtw.iter().flat_map(|d| ["-dtw", d.as_str(), "-nfa", "-ojf"]))
        .args(["-l", &cfg.language, "-ml", "1", "-sow", "-oj", "-pp", "-t", &cfg.threads.to_string(), "-of"])
        .arg(&tmp)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "whisper-cli konnte nicht gestartet werden ({}). Installation z.B. mit `brew install whisper-cpp`.",
                cfg.binary.display()
            )
        })?;

    let mut log_tail: Vec<String> = Vec::new();
    if let Some(stderr) = child.stderr.take() {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(p) = parse_progress(&line) {
                progress(0.08 + 0.9 * p);
            }
            log_tail.push(line);
            if log_tail.len() > 20 {
                log_tail.remove(0);
            }
        }
    }
    let status = child.wait()?;
    let peaks = wav_peaks(&wav).ok();
    let _ = std::fs::remove_file(&wav);
    let json_path = tmp.with_extension("json");
    if !status.success() || !json_path.is_file() {
        bail!("whisper-cli ist fehlgeschlagen:\n{}", log_tail.join("\n"));
    }
    let text = std::fs::read_to_string(&json_path)?;
    let _ = std::fs::remove_file(&json_path);
    let mut transcript = parse_whisper_json_with_audio(&text, peaks.as_deref())?;
    if transcript.language.is_empty() {
        transcript.language = cfg.language.clone();
    }
    progress(1.0);
    Ok(transcript)
}

fn unique() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

fn parse_progress(line: &str) -> Option<f32> {
    let idx = line.find("progress =")?;
    let rest = line[idx + "progress =".len()..].trim().trim_end_matches('%');
    rest.trim().parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
}

/// Liest das JSON von `whisper-cli -oj` (mit `-ml 1 -sow` = ein Wort pro Segment).
#[cfg(test)]
pub fn parse_whisper_json(text: &str) -> Result<Transcript> {
    parse_whisper_json_with_audio(text, None)
}

/// Wie `parse_whisper_json`, verfeinert Wortgrenzen aber am Pegel der Aufnahme.
pub fn parse_whisper_json_with_audio(text: &str, peaks: Option<&[f32]>) -> Result<Transcript> {
    let json: Value = serde_json::from_str(text).context("Whisper-JSON ungültig")?;
    let segments = json["transcription"].as_array().ok_or_else(|| anyhow!("Whisper-JSON: 'transcription' fehlt"))?;
    let language = json["result"]["language"].as_str().unwrap_or("").to_string();
    // Pro Wort zusätzlich die DTW-Zeiten (erster/letzter Wortteil ohne Satzzeichen).
    let mut words: Vec<Word> = Vec::new();
    let mut dtw: Vec<(Option<f64>, Option<f64>)> = Vec::new();
    for seg in segments {
        let raw = seg["text"].as_str().unwrap_or("");
        let from = seg["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        let to = seg["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0;
        let trimmed = raw.trim();
        if trimmed.is_empty() || is_annotation(trimmed) {
            continue;
        }
        let token_times: Vec<f64> = seg["tokens"]
            .as_array()
            .map(|toks| {
                toks.iter()
                    .filter(|t| t["text"].as_str().is_some_and(|x| x.chars().any(|c| c.is_alphanumeric()) && !x.starts_with("[_")))
                    .filter_map(|t| t["t_dtw"].as_f64().filter(|v| *v >= 0.0).map(|v| v / 100.0))
                    .collect()
            })
            .unwrap_or_default();
        let (first, last) = (token_times.first().copied(), token_times.last().copied());
        let continues_word = !raw.starts_with(' ') || trimmed.chars().all(|c| c.is_ascii_punctuation());
        match (words.last_mut(), dtw.last_mut()) {
            (Some(prev), Some(prev_dtw)) if continues_word => {
                prev.text.push_str(trimmed);
                prev.end = prev.end.max(to);
                prev_dtw.0 = prev_dtw.0.or(first);
                prev_dtw.1 = last.or(prev_dtw.1);
            }
            _ => {
                words.push(Word { text: trimmed.to_string(), start: from, end: to.max(from), core: None });
                dtw.push((first, last));
            }
        }
    }
    apply_dtw_times(&mut words, &dtw, peaks);
    Ok(Transcript { language, words, ..Default::default() })
}

/// DTW-Zeitstempel (`-dtw`) sind deutlich genauer als die Segmentzeiten, liegen
/// aber mit etwas Verzögerung *innerhalb* der Wortteile. Die Grenze zwischen zwei
/// Wörtern liegt daher zwischen dem letzten Wortteil des einen und dem ersten des
/// nächsten – genau dort wird der leiseste Punkt der Wellenform gesucht
/// (`peaks`: Spitzenpegel 100/s). Ohne Wellenform: Erfahrungswert (−0,2 s).
/// Pausen korrigiert danach `project::refine_words`.
fn apply_dtw_times(words: &mut [Word], dtw: &[(Option<f64>, Option<f64>)], peaks: Option<&[f32]>) {
    const LAG: f64 = 0.2;
    let with_dtw = dtw.iter().filter(|(_, l)| l.is_some()).count();
    if words.is_empty() || with_dtw * 10 < words.len() * 8 {
        return; // ohne (ausreichend) DTW-Daten: Segmentzeiten behalten
    }
    // Fehlende Werte mit Segmentzeiten auffüllen.
    let times: Vec<(f64, f64)> = words
        .iter()
        .zip(dtw)
        .map(|(w, (f, l))| (f.unwrap_or(w.start + LAG), l.or(*f).unwrap_or(w.end)))
        .collect();
    let n = words.len();
    words[0].start = (times[0].0 - LAG - 0.1).max(0.0);
    for i in 0..n - 1 {
        let lo = times[i].1;
        let hi = times[i + 1].0;
        let b = if hi <= lo {
            (lo + hi) / 2.0
        } else {
            match peaks {
                Some(p) if !p.is_empty() => quietest_point(p, lo, hi),
                _ => (hi - LAG).max(lo),
            }
        };
        let b = b.max(words[i].start + 0.03);
        words[i].end = b;
        words[i + 1].start = b;
    }
    words[n - 1].end = (times[n - 1].1 + 0.3).max(words[n - 1].start + 0.05);
    for ((w, (f, l)), t) in words.iter_mut().zip(dtw).zip(&times) {
        if f.is_some() && l.is_some() {
            let core_start = (t.0 - LAG).clamp(w.start, w.end);
            w.core = Some((core_start, t.1.clamp(core_start, w.end)));
        }
    }
}

/// Zeitpunkt mit dem niedrigsten (über 30 ms geglätteten) Pegel in `[lo, hi]`.
fn quietest_point(peaks: &[f32], lo: f64, hi: f64) -> f64 {
    let a = ((lo * 100.0).round().max(0.0) as usize).min(peaks.len() - 1);
    let b = ((hi * 100.0).round() as usize).clamp(a, peaks.len() - 1);
    let level = |i: usize| {
        let s = i.saturating_sub(1);
        let e = (i + 2).min(peaks.len());
        peaks[s..e].iter().sum::<f32>() / (e - s) as f32
    };
    let best = (a..=b).min_by(|x, y| level(*x).total_cmp(&level(*y))).unwrap_or(a);
    best as f64 / 100.0
}

/// Spitzenpegel (0..1, 100 Werte/s) aus einer 16-Bit-PCM-WAV-Datei.
pub fn wav_peaks(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("keine WAV-Datei");
    }
    let (mut pos, mut rate, mut channels) = (12usize, 16000u32, 1u16);
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        if id == b"fmt " && len >= 16 {
            channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap()).max(1);
            rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
        } else if id == b"data" {
            let data = &bytes[body..(body + len).min(bytes.len())];
            let per_peak = (rate as usize / 100).max(1) * channels as usize;
            return Ok(data
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
                .collect::<Vec<_>>()
                .chunks(per_peak)
                .map(|c| *c.iter().max().unwrap_or(&0) as f32 / 32768.0)
                .collect());
        }
        pos = body + len + (len % 2);
    }
    bail!("WAV ohne Daten")
}

/// Whisper-Markierungen wie `[BLANK_AUDIO]`, `[Musik]`, `(Applaus)`.
fn is_annotation(t: &str) -> bool {
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')')) || t.starts_with("[_")
}

/// Lädt ein Modell per `curl` herunter. `progress` erhält 0..1.
pub fn download_model(name: &str, progress: &dyn Fn(f32)) -> Result<PathBuf> {
    let target = model_path(name);
    let part = target.with_extension("part");
    let expected_mb = MODELS.iter().find(|(n, _)| *n == name).map(|(_, s)| *s).unwrap_or(500);
    let mut child = command("curl")
        .args(["-L", "--fail", "-s", "-o"])
        .arg(&part)
        .arg(model_url(name))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("curl konnte nicht gestartet werden")?;
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let _ = std::fs::remove_file(&part);
                bail!("Download fehlgeschlagen (curl Exit-Code {:?})", status.code());
            }
            break;
        }
        let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        progress((size as f32 / (expected_mb as f32 * 1_048_576.0)).min(0.99));
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    std::fs::rename(&part, &target)?;
    progress(1.0);
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_level_json() {
        let json = r#"{
          "result": {"language": "de"},
          "transcription": [
            {"offsets": {"from": 0, "to": 0}, "text": ""},
            {"offsets": {"from": 120, "to": 480}, "text": " Hallo"},
            {"offsets": {"from": 480, "to": 500}, "text": ","},
            {"offsets": {"from": 520, "to": 900}, "text": " Welt"},
            {"offsets": {"from": 900, "to": 1000}, "text": "chen"},
            {"offsets": {"from": 1000, "to": 3000}, "text": " [BLANK_AUDIO]"},
            {"offsets": {"from": 3100, "to": 3500}, "text": " Tschüss!"}
          ]
        }"#;
        let t = parse_whisper_json(json).unwrap();
        assert_eq!(t.language, "de");
        let texts: Vec<&str> = t.words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, vec!["Hallo,", "Weltchen", "Tschüss!"]);
        assert_eq!(t.words[1].start, 0.52);
        assert_eq!(t.words[1].end, 1.0);
    }

    #[test]
    fn uses_dtw_times_when_present() {
        // Ausschnitt aus einer echten Ausgabe: Segmentzeiten liegen in der Pause, DTW stimmt.
        let json = r#"{"transcription": [
          {"offsets": {"from": 28020, "to": 29000}, "text": " Ozeanen.",
           "tokens": [{"text": " Oze", "t_dtw": 2786}, {"text": "anen", "t_dtw": 2880}, {"text": ".", "t_dtw": 2990}]},
          {"offsets": {"from": 29000, "to": 31430}, "text": " Wasserstoffeis",
           "tokens": [{"text": " Wasser", "t_dtw": 3348}, {"text": "stoff", "t_dtw": 3372}, {"text": "eis", "t_dtw": 3406}]},
          {"offsets": {"from": 31430, "to": 32310}, "text": " klebt",
           "tokens": [{"text": " kle", "t_dtw": 3426}, {"text": "bt", "t_dtw": 3436}]}
        ]}"#;
        let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
        // Ohne Wellenform: Grenze 0,2 s vor dem ersten Wortteil des nächsten Worts.
        let t = parse_whisper_json(json).unwrap();
        let w = &t.words;
        assert!(close(w[0].end, 33.28) && close(w[1].start, 33.28), "{:?}", w[0]);
        assert!(close(w[1].end, 34.06), "nicht vor dem letzten Wortteil: {:?}", w[1]);
        // Mit Wellenform: Grenze im leisesten Punkt zwischen den Wörtern.
        let mut peaks = vec![0.5f32; 4000];
        for v in &mut peaks[2885..=3320] {
            *v = 0.001; // Pause nach „Ozeanen“
        }
        for v in &mut peaks[3407..=3410] {
            *v = 0.01; // kurzes Tal zwischen „Wasserstoffeis“ und „klebt“
        }
        let t = parse_whisper_json_with_audio(json, Some(&peaks)).unwrap();
        let w = &t.words;
        assert!(w[0].end >= 28.85 && w[0].end <= 33.2, "{:?}", w[0]);
        assert!(w[1].end >= 34.07 && w[1].end <= 34.10, "{:?}", w[1]);
        assert!(close(w[2].start, w[1].end));
    }

    #[test]
    fn reads_wav_peaks() {
        let dir = std::env::temp_dir().join(format!("tcf_wav_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        // 0,2 s Mono 16 kHz: erst still, dann laut.
        let samples: Vec<i16> = (0..3200).map(|i| if i < 1600 { 0 } else { 16384 }).collect();
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend(((36 + data.len()) as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(1u16.to_le_bytes());
        wav.extend(16000u32.to_le_bytes());
        wav.extend(32000u32.to_le_bytes());
        wav.extend(2u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((data.len() as u32).to_le_bytes());
        wav.extend(data);
        std::fs::write(&path, wav).unwrap();
        let p = wav_peaks(&path).unwrap();
        assert_eq!(p.len(), 20);
        assert_eq!(p[0], 0.0);
        assert_eq!(p[19], 0.5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dtw_presets_from_model_files() {
        assert_eq!(dtw_preset(Path::new("/m/ggml-small.bin")).as_deref(), Some("small"));
        assert_eq!(dtw_preset(Path::new("ggml-large-v3-turbo.bin")).as_deref(), Some("large.v3.turbo"));
        assert_eq!(dtw_preset(Path::new("ggml-base.en.bin")).as_deref(), Some("base.en"));
        assert_eq!(dtw_preset(Path::new("mein-modell.bin")), None);
    }

    #[test]
    fn parses_progress_lines() {
        assert_eq!(parse_progress("whisper_print_progress_callback: progress =  45%"), Some(0.45));
        assert_eq!(parse_progress("irgendwas"), None);
    }
}
