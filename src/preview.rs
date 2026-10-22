//! Preview: single frames while scrubbing and real-time playback with audio.
//!
//! Both use the render graph from `render.rs` and read raw frames or
//! PCM audio from ffmpeg via pipes.

use crate::project::Project;
use crate::render::{self, RenderSpec, SAMPLE_RATE};
use crate::util::command;
use anyhow::{Context, Result};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub time: f64,
}

fn preview_spec(size: (u32, u32), from: f64, to: f64, video: bool, audio: bool, fps: u32) -> RenderSpec {
    RenderSpec {
        width: size.0,
        height: size.1,
        from,
        to,
        video,
        audio,
        fit_override: None,
        pix_fmt: "rgba",
        out_fps: Some(fps),
        loudnorm: false,
    }
}

// ---------------------------------------------------------------------------
// Single frames (scrubbing)
// ---------------------------------------------------------------------------

struct ScrubRequest {
    args: Vec<String>,
    size: (u32, u32),
    time: f64,
}

/// Background thread that only ever renders the most recent request.
pub struct ScrubWorker {
    tx: Sender<ScrubRequest>,
    rx: Receiver<Result<Frame, String>>,
}

impl ScrubWorker {
    pub fn new(ffmpeg: PathBuf) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<ScrubRequest>();
        let (res_tx, res_rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(mut req) = req_rx.recv() {
                while let Ok(newer) = req_rx.try_recv() {
                    req = newer;
                }
                let res = render_single(&ffmpeg, &req).map_err(|e| e.to_string());
                if res_tx.send(res).is_err() {
                    break;
                }
            }
        });
        ScrubWorker { tx: req_tx, rx: res_rx }
    }

    pub fn request(&self, p: &Project, time: f64, size: (u32, u32)) {
        let frame = p.frame();
        let spec = preview_spec(size, time, time + frame, true, false, p.fps);
        let graph = render::build(p, &spec);
        let _ = self.tx.send(ScrubRequest { args: graph.args, size, time });
    }

    /// Most recent finished frame (if any).
    pub fn poll(&self) -> Option<Result<Frame, String>> {
        let mut last = None;
        while let Ok(f) = self.rx.try_recv() {
            last = Some(f);
        }
        last
    }
}

fn render_single(ffmpeg: &PathBuf, req: &ScrubRequest) -> Result<Frame> {
    let out = command(ffmpeg)
        .args(["-v", "error", "-nostdin"])
        .args(&req.args)
        .args(["-map", "[vout]", "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("Could not start ffmpeg")?;
    let need = (req.size.0 * req.size.1 * 4) as usize;
    if out.stdout.len() < need {
        anyhow::bail!("Preview failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let mut rgba = out.stdout;
    rgba.truncate(need);
    Ok(Frame { width: req.size.0, height: req.size.1, rgba, time: req.time })
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// PCM source for rodio, fed from the ffmpeg pipe.
struct PipeSource {
    rx: Receiver<Vec<i16>>,
    buf: Vec<i16>,
    pos: usize,
}

impl Iterator for PipeSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.pos >= self.buf.len() {
            match self.rx.recv_timeout(Duration::from_millis(20)) {
                Ok(b) => self.buf = b,
                // Buffer empty: insert a short silence (even length → channels stay aligned).
                Err(RecvTimeoutError::Timeout) => self.buf = vec![0; 480 * 2],
                Err(RecvTimeoutError::Disconnected) => return None,
            }
            self.pos = 0;
            if self.buf.is_empty() {
                return Some(0);
            }
        }
        let s = self.buf[self.pos];
        self.pos += 1;
        Some(s)
    }
}

impl rodio::Source for PipeSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        2
    }
    fn sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

pub struct Playback {
    pub from: f64,
    pub to: f64,
    out_fps: u32,
    video_rx: Receiver<Frame>,
    video_done: Arc<AtomicBool>,
    pending: Option<Frame>,
    sink: Option<rodio::Sink>,
    clock: Option<Instant>,
    children: Vec<Arc<Mutex<Child>>>,
    pub error: Arc<Mutex<Option<String>>>,
}

impl Playback {
    pub fn start(
        ffmpeg: &PathBuf,
        p: &Project,
        from: f64,
        to: f64,
        size: (u32, u32),
        out_fps: u32,
        audio: Option<&rodio::OutputStreamHandle>,
        with_video: bool,
    ) -> Result<Self> {
        let error = Arc::new(Mutex::new(None));
        let mut children = Vec::new();
        let (vtx, vrx): (SyncSender<Frame>, Receiver<Frame>) = mpsc::sync_channel(6);
        let video_done = Arc::new(AtomicBool::new(!with_video));
        let actual_fps = out_fps.min(p.fps);

        // Video
        if with_video {
        let spec = preview_spec(size, from, to, true, false, out_fps);
        let graph = render::build(p, &spec);
        let mut child = command(ffmpeg)
            .args(["-v", "error", "-nostdin"])
            .args(&graph.args)
            .args(["-map", "[vout]", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Could not start ffmpeg")?;
        let mut stdout = child.stdout.take().unwrap();
        spawn_stderr_collector(&mut child, error.clone());
        children.push(Arc::new(Mutex::new(child)));
        let done = video_done.clone();
        let (w, h) = size;
        std::thread::spawn(move || {
            let need = (w * h * 4) as usize;
            let mut k: u64 = 0;
            loop {
                let mut buf = vec![0u8; need];
                if stdout.read_exact(&mut buf).is_err() {
                    break;
                }
                let frame = Frame { width: w, height: h, rgba: buf, time: from + k as f64 / actual_fps as f64 };
                k += 1;
                if vtx.send(frame).is_err() {
                    break;
                }
            }
            done.store(true, Ordering::SeqCst);
        });
        }

        // Audio
        let mut sink = None;
        if let Some(handle) = audio {
            let spec = preview_spec(size, from, to, false, true, out_fps);
            let graph = render::build(p, &spec);
            let mut child = command(ffmpeg)
                .args(["-v", "error", "-nostdin"])
                .args(&graph.args)
                .args(["-map", "[aout]", "-f", "s16le", "-ac", "2", "-ar", &SAMPLE_RATE.to_string(), "pipe:1"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .context("Could not start ffmpeg (audio)")?;
            let mut stdout = child.stdout.take().unwrap();
            spawn_stderr_collector(&mut child, error.clone());
            children.push(Arc::new(Mutex::new(child)));
            let (atx, arx) = mpsc::sync_channel::<Vec<i16>>(256);
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 8192];
                loop {
                    let mut filled = 0;
                    while filled < buf.len() {
                        match stdout.read(&mut buf[filled..]) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => filled += n,
                        }
                    }
                    let usable = filled - filled % 4;
                    if usable > 0 {
                        let samples =
                            buf[..usable].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
                        if atx.send(samples).is_err() {
                            break;
                        }
                    }
                    if filled < buf.len() {
                        break;
                    }
                }
            });
            if let Ok(s) = rodio::Sink::try_new(handle) {
                s.pause();
                s.append(PipeSource { rx: arx, buf: Vec::new(), pos: 0 });
                sink = Some(s);
            }
        }

        Ok(Playback {
            from,
            to,
            out_fps: actual_fps,
            video_rx: vrx,
            video_done,
            pending: None,
            sink,
            clock: None,
            children,
            error,
        })
    }

    /// Returns the frame to display (if new), the current time and whether
    /// the end of the window has been reached.
    pub fn update(&mut self) -> (Option<Frame>, f64, bool) {
        if self.pending.is_none() {
            match self.video_rx.try_recv() {
                Ok(f) => self.pending = Some(f),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => {}
            }
        }
        // The clock starts as soon as the first frame arrives – audio starts then too.
        if self.clock.is_none() {
            if self.pending.is_some() || self.video_done.load(Ordering::SeqCst) {
                self.clock = Some(Instant::now());
                if let Some(s) = &self.sink {
                    s.play();
                }
            } else {
                return (None, self.from, false);
            }
        }
        let now = self.from + self.clock.unwrap().elapsed().as_secs_f64();
        let mut show = None;
        // Fetch all due frames, display only the most recent one.
        loop {
            match &self.pending {
                Some(f) if f.time <= now => {
                    show = self.pending.take();
                    match self.video_rx.try_recv() {
                        Ok(f) => self.pending = Some(f),
                        Err(_) => break,
                    }
                }
                _ => break,
            }
        }
        let finished = now >= self.to
            || (self.video_done.load(Ordering::SeqCst)
                && self.pending.is_none()
                && now >= self.to - 1.0 / self.out_fps as f64);
        (show, now.min(self.to), finished)
    }

    pub fn stop(&mut self) {
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        for c in &self.children {
            if let Ok(mut c) = c.lock() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_stderr_collector(child: &mut Child, error: Arc<Mutex<Option<String>>>) {
    if let Some(mut stderr) = child.stderr.take() {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            let s = s.trim();
            if !s.is_empty() {
                if let Ok(mut e) = error.lock() {
                    *e = Some(s.lines().last().unwrap_or(s).to_string());
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Tools, probe};
    use std::process::Command;

    #[test]
    fn scrub_and_playback_deliver_frames() {
        if Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("tcf_prev_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("s.mp4");
        assert!(Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=s=320x180:r=30:d=3"])
            .args(["-f", "lavfi", "-i", "sine=d=3", "-shortest", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac"])
            .arg(&src)
            .status()
            .unwrap()
            .success());
        let tools = Tools { ffmpeg: "ffmpeg".into(), ffprobe: "ffprobe".into() };
        let mut p = Project::default();
        p.media.push(probe(&tools, &src, 1).unwrap());
        p.next_id = 5;
        let main = p.main_track();
        p.add_clip(1, main, 0.0).unwrap();
        let size = crate::render::preview_size(&p, 90);

        let scrub = ScrubWorker::new("ffmpeg".into());
        scrub.request(&p, 1.0, size);
        let start = Instant::now();
        let frame = loop {
            if let Some(f) = scrub.poll() {
                break f.unwrap();
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((frame.width, frame.height), size);
        assert_eq!(frame.rgba.len(), (size.0 * size.1 * 4) as usize);

        let mut pb = Playback::start(&"ffmpeg".into(), &p, 1.0, 2.0, size, 15, None, true).unwrap();
        let mut frames = 0;
        let start = Instant::now();
        loop {
            let (f, t, finished) = pb.update();
            if f.is_some() {
                frames += 1;
            }
            assert!(t >= 1.0 && t <= 2.0);
            if finished || start.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(frames >= 10, "only {frames} frames");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
