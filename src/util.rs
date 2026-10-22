//! Small helpers: time formatting, app directories, program lookup.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Formats seconds as `mm:ss.ff` (or `h:mm:ss.ff`).
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

/// Timestamp in SRT format `hh:mm:ss,mmm`.
pub fn fmt_srt_time(t: f64) -> String {
    let total_ms = (t.max(0.0) * 1000.0).round() as u64;
    let ms = total_ms % 1000;
    let s = (total_ms / 1000) % 60;
    let m = (total_ms / 60000) % 60;
    let h = total_ms / 3_600_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

/// Base directory for settings, models and cache: `~/.textcut`.
pub fn app_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = home.join(".textcut");
    // The app was called TextCutForge before: take over its settings and models.
    let old = home.join(".textcutforge");
    if !dir.exists() && old.is_dir() {
        let _ = std::fs::rename(&old, &dir);
    }
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

/// Looks up a program next to the TextCut executable (portable setup),
/// in PATH, and in typical install locations – GUI apps often start with a
/// reduced PATH (macOS) or before a fresh install updated it (Windows).
pub fn find_program(names: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        dirs.push(exe_dir.join("bin"));
        dirs.push(exe_dir);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(extra_program_dirs());
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

#[cfg(target_os = "windows")]
fn extra_program_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut dirs = Vec::new();
    if let Some(local) = env("LOCALAPPDATA") {
        dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
    }
    if let Some(home) = env("USERPROFILE") {
        dirs.push(home.join("scoop").join("shims"));
    }
    dirs.push(PathBuf::from(r"C:\ProgramData\chocolatey\bin"));
    dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
    dirs.push(PathBuf::from(r"C:\whisper"));
    dirs.push(PathBuf::from(r"C:\whisper\Release"));
    dirs
}

#[cfg(not(target_os = "windows"))]
fn extra_program_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/snap/bin"].iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local").join("bin"));
    }
    dirs
}

/// How to install a missing tool on this operating system.
pub fn install_hint(tool: &str) -> &'static str {
    let windows = cfg!(target_os = "windows");
    let macos = cfg!(target_os = "macos");
    match tool {
        "ffmpeg" if macos => "brew install ffmpeg",
        "ffmpeg" if windows => "winget install Gyan.FFmpeg",
        "ffmpeg" => "sudo apt install ffmpeg  (or dnf/pacman)",
        _ if macos => "brew install whisper-cpp",
        _ if windows => "download whisper-bin-x64.zip from github.com/ggml-org/whisper.cpp/releases and put it in C:\\whisper",
        _ => "build whisper.cpp (see README) and copy whisper-cli to ~/.local/bin",
    }
}

/// A program that was not found is still invoked by name,
/// so that the operating system's error message becomes visible.
pub fn program_or_name(configured: &str, names: &[&str]) -> PathBuf {
    if !configured.trim().is_empty() {
        return PathBuf::from(configured.trim());
    }
    find_program(names).unwrap_or_else(|| PathBuf::from(names[0]))
}

/// Creates a `Command` that does not open a console window on Windows.
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

/// Rounds to even pixel values (required for yuv420p).
pub fn even(v: f64) -> u32 {
    let v = v.round().max(2.0) as u32;
    v - (v % 2)
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Untitled".into())
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
