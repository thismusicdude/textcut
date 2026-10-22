//! Persistent app settings (`~/.textcutforge/settings.json`).

use crate::media::Tools;
use crate::transcribe::{WhisperConfig, model_path};
use crate::util::{app_dir, find_program, program_or_name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The single filler list of older versions (German and English mixed).
const LEGACY_DEFAULT_FILLERS: &str = "äh, ähm, öhm, hm, hmm, ehm, uh, um, uhm, erm";

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
    /// Edited filler lists per language code; languages missing here use
    /// [`crate::transcribe::default_fillers`].
    pub filler_words: BTreeMap<String, String>,
    /// Single list of older versions – migrated into `filler_words` on load.
    #[serde(rename = "fillers", skip_serializing)]
    legacy_fillers: Option<String>,
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
            filler_words: BTreeMap::new(),
            legacy_fillers: None,
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
        let mut s: Settings = std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        s.migrate();
        s
    }

    /// A customized list from older versions becomes the list for the transcription language.
    fn migrate(&mut self) {
        if let Some(old) = self.legacy_fillers.take() {
            if old.trim() != LEGACY_DEFAULT_FILLERS && !old.trim().is_empty() {
                let lang = if self.language.trim().is_empty() || self.language == "auto" { "de".to_string() } else { self.language.clone() };
                self.filler_words.entry(lang).or_insert(old);
            }
        }
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
            return Err(format!("whisper-cli not found ({})", crate::util::install_hint("whisper")));
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

    /// Language whose filler list applies to a transcript: the language Whisper
    /// detected, else the configured one. Empty for "auto" without detection.
    pub fn filler_language(&self, transcript_language: &str) -> String {
        let detected = transcript_language.trim();
        if !detected.is_empty() && detected != "auto" {
            return detected.to_string();
        }
        let configured = self.language.trim();
        if configured == "auto" { String::new() } else { configured.to_string() }
    }

    /// Filler list of a language as editable text (edited version or default).
    pub fn filler_text(&self, language: &str) -> String {
        self.filler_words.get(language).cloned().unwrap_or_else(|| crate::transcribe::default_fillers(language).to_string())
    }

    pub fn filler_list(&self, language: &str) -> Vec<String> {
        self.filler_text(language).split([',', ';', '\n']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
    }

    pub fn add_recent(&mut self, path: PathBuf) {
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fillers_follow_the_language() {
        let mut s = Settings { language: "auto".into(), ..Default::default() };
        assert_eq!(s.filler_language("en"), "en");
        assert_eq!(s.filler_language(""), "");
        assert!(s.filler_list("en").contains(&"um".to_string()));
        assert!(!s.filler_list("en").contains(&"äh".to_string()));
        assert!(s.filler_list("fr").contains(&"euh".to_string()));
        assert!(s.filler_list("").contains(&"hmm".to_string()), "unknown language → generic list");
        s.language = "de".into();
        assert_eq!(s.filler_language(""), "de");
        s.filler_words.insert("de".into(), "äh; halt".into());
        assert_eq!(s.filler_list("de"), vec!["äh".to_string(), "halt".to_string()]);
    }

    #[test]
    fn migrates_customized_legacy_list() {
        let old: Settings = serde_json::from_str(r#"{"language": "de", "fillers": "äh, ähm"}"#).unwrap();
        let mut s = old.clone();
        s.migrate();
        assert_eq!(s.filler_text("de"), "äh, ähm");
        let mut untouched: Settings = serde_json::from_str(&format!(r#"{{"fillers": "{LEGACY_DEFAULT_FILLERS}"}}"#)).unwrap();
        untouched.migrate();
        assert!(untouched.filler_words.is_empty());
        assert!(!serde_json::to_string(&s).unwrap().contains("\"fillers\""));
    }
}
