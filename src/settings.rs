//! Persistent app settings (`~/.textcutforge/settings.json`).

use crate::media::Tools;
use crate::transcribe::{WhisperConfig, model_path};
use crate::util::{app_dir, find_program, program_or_name};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ffmpeg: String,
    pub ffprobe: String,
    pub whisper: String,
    /// Model name (tiny/base/small/…) – or a custom path in `model_path`.
    pub model: String,
    pub model_path: String,
    pub language: String,
    pub threads: usize,
    pub auto_transcribe: bool,
    /// Also transcribe filler words ("äh", "ähm"), repetitions and slips of the tongue.
    pub verbatim: bool,
    pub auto_detect_silence: bool,
    pub silence_db: f32,
    pub silence_min: f64,
    pub silence_pad: f64,
    pub fillers: String,
    pub preview_height: u32,
    pub preview_fps: u32,
    pub text_size: f32,
    pub export_dir: String,
    pub recent: Vec<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            ffmpeg: String::new(),
            ffprobe: String::new(),
            whisper: String::new(),
            model: "base".into(),
            model_path: String::new(),
            language: "de".into(),
            threads: std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4),
            auto_transcribe: true,
            verbatim: true,
            auto_detect_silence: true,
            silence_db: -35.0,
            silence_min: 0.6,
            silence_pad: 0.12,
            fillers: "äh, ähm, öhm, hm, hmm, ehm, uh, um, uhm, erm".into(),
            preview_height: 540,
            preview_fps: 30,
            text_size: 17.0,
            export_dir: String::new(),
            recent: Vec::new(),
        }
    }
}

impl Settings {
    fn path() -> PathBuf {
        app_dir().join("settings.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(Self::path(), s);
        }
    }

    pub fn tools(&self) -> Tools {
        Tools {
            ffmpeg: program_or_name(&self.ffmpeg, &["ffmpeg"]),
            ffprobe: program_or_name(&self.ffprobe, &["ffprobe"]),
        }
    }

    pub fn whisper_binary(&self) -> Option<PathBuf> {
        if !self.whisper.trim().is_empty() {
            return Some(PathBuf::from(self.whisper.trim()));
        }
        find_program(&["whisper-cli", "whisper-cpp", "whisper.cpp"])
    }

    pub fn model_file(&self) -> PathBuf {
        if !self.model_path.trim().is_empty() {
            PathBuf::from(self.model_path.trim())
        } else {
            model_path(&self.model)
        }
    }

    /// Is transcription ready to use? Otherwise returns the reason.
    pub fn whisper_ready(&self) -> Result<(), String> {
        if self.whisper_binary().is_none() {
            return Err("whisper-cli not found (brew install whisper-cpp)".into());
        }
        if !self.model_file().is_file() {
            return Err(format!("Model \"{}\" is missing – download it in Settings", self.model));
        }
        Ok(())
    }

    pub fn whisper_config(&self) -> WhisperConfig {
        WhisperConfig {
            binary: self.whisper_binary().unwrap_or_else(|| PathBuf::from("whisper-cli")),
            model: self.model_file(),
            language: if self.language.trim().is_empty() { "auto".into() } else { self.language.clone() },
            threads: self.threads.max(1),
            prompt: if self.verbatim { crate::transcribe::verbatim_prompt(self.language.trim()).map(String::from) } else { None },
            dtw: crate::transcribe::dtw_preset(&self.model_file()),
        }
    }

    pub fn filler_list(&self) -> Vec<String> {
        self.fillers.split([',', ';', '\n']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    }

    pub fn add_recent(&mut self, path: PathBuf) {
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(8);
    }
}
