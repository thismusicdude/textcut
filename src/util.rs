//! Kleine Helfer: Zeitformatierung, App-Verzeichnisse, Programmsuche.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Formatiert Sekunden als `mm:ss.ff` (bzw. `h:mm:ss.ff`).
pub fn fmt_time(t: f64) -> String {
    let t = t.max(0.0);
    let total_cs = (t * 100.0).round() as u64;
    let cs = total_cs % 100;
    let s = (total_cs / 100) % 60;
    let m = (total_cs / 6000) % 60;
    let h = total_cs / 360000;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{cs:02}")
    } else {
        format!("{m:02}:{s:02}.{cs:02}")
    }
}

/// Zeitstempel im SRT-Format `hh:mm:ss,mmm`.
pub fn fmt_srt_time(t: f64) -> String {
    let total_ms = (t.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// Basisverzeichnis für Einstellungen, Modelle und Cache: `~/.textcutforge`.
pub fn app_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = home.join(".textcutforge");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn cache_dir() -> PathBuf {
    let dir = app_dir().join("cache");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn models_dir() -> PathBuf {
    let dir = app_dir().join("models");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Sucht ein Programm im PATH (plus typische Homebrew-Pfade, da GUI-Apps
/// unter macOS oft einen reduzierten PATH haben).
pub fn find_program(names: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    for name in names {
        for dir in &dirs {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

/// Ein Programm, das nicht gefunden wurde, wird trotzdem per Name aufgerufen,
/// damit die Fehlermeldung des Betriebssystems sichtbar wird.
pub fn program_or_name(configured: &str, names: &[&str]) -> PathBuf {
    if !configured.trim().is_empty() {
        return PathBuf::from(configured.trim());
    }
    find_program(names).unwrap_or_else(|| PathBuf::from(names[0]))
}

/// Erzeugt einen `Command`, der unter Windows kein Konsolenfenster öffnet.
pub fn command(program: impl AsRef<Path>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program.as_ref());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// Rundet auf gerade Pixelwerte (nötig für yuv420p).
pub fn even(v: f64) -> u32 {
    let v = v.round().max(2.0) as u32;
    v - (v % 2)
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unbenannt".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_time() {
        assert_eq!(fmt_time(0.0), "00:00.00");
        assert_eq!(fmt_time(61.5), "01:01.50");
        assert_eq!(fmt_time(3723.25), "1:02:03.25");
        assert_eq!(fmt_srt_time(3723.256), "01:02:03,256");
    }

    #[test]
    fn even_values() {
        assert_eq!(even(641.0), 640);
        assert_eq!(even(1.0), 2);
        assert_eq!(even(1079.6), 1080);
    }
}
