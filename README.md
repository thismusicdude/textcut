# TextCut

A video editor written in Rust where you **edit text instead of video**: every clip is
transcribed and shown like a text document. Delete words and the video is cut at exactly
those spots. **Silence is detected and removed automatically.**

This project is used as experiment to see how far agentic coding already is.

**Website & screenshots:** [thismusicdude.github.io/textcut](https://thismusicdude.github.io/textcut/)

## How can i support this work?

TextCut is free. Its code was largely generated with AI, and I don't want you to pay for something that i rarely touched. 
If the app is useful to you, please donate whatever you like to the following organizations.
If you choose to donate because of this text, send me a notification on Instagram ([@enbytiousmusic](https://www.instagram.com/enbytiousmusic)). That would make me really happy!

- [PETA](https://action.peta.de/page/184137/donate/) 
- [FOUR PAWS](https://www.four-paws.org)
- [UNICEF](https://www.unicef.org)

Or watch the documentary [**Dominion**](https://www.dominionmovement.com/watch) (free to stream, narrated by Joaquin Phoenix) <br> That would mean the world to me!

## Two modes

On startup you pick the project type (later via *File → New project …*):

- **🎬 Video project:** everything described below, including picture-in-picture and export in multiple sizes.
  The timeline's audio can also be exported as an audio file.
- **🎧 Audio project** for podcasts, interviews and voiceover: audio tracks only, main track A1 is magnetic,
  cuts snap to 10 ms. Video files contribute their audio. Instead of the video preview, a
  player shows the sentence currently being spoken.

## Features

**Core features**
- **Text editing:** select words (click/drag, ⇧+click) → `Delete` cuts them out.
  Press `Delete` again to restore them. Everything is non-destructive with undo/redo.
- **Remove silence automatically:** ffmpeg `silencedetect` finds pauses. One click cuts them
  from all clips, with adjustable threshold (dB), minimum duration and padding.
  Pauses appear as `⏸ 1.2s` markers in the text and can also be deleted individually.
- Remove filler words ("äh", "ähm" …), search & delete all matches. Whisper normally omits filler
  words. With **"Transcribe filler words (verbatim)"** (Settings, on by default) it gets a
  prompt containing filler words and transcribes them, along with repetitions and slips of the tongue (de/en/fr/es/it/nl).
  Words with hardly any audio underneath are **underlined in yellow**: Whisper may have invented them. "Remove
  filler words" leaves these in place.
- Filler lists per language (de, en, fr, es, it, nl, pl, pt, tr, uk), editable in Settings. Each
  recording uses the list of the language Whisper detected (or the configured language); other
  languages fall back to neutral sounds (hm, hmm, uh, um). Only pure hesitation sounds are in the
  defaults – real words that often act as fillers ("also", "like") would cut content.
- Precise word boundaries: DTW timestamps from whisper.cpp plus level analysis. When deleting, only the
  word core is cut; quiet onsets of neighboring words (e.g. the "S" in "Sphäre") are preserved.
- **Smooth transitions:** at every cut point the audio briefly fades out and back in.
  Length (5–500 ms) and curve (Linear, Sine, S-curve, Fast, Slow) are adjustable,
  separately for text and silence cuts, with a curve preview. The timeline length does not change.
- Whisper timestamps are corrected using the silence analysis: words never start or end inside
  a pause and never span one. Words with the same start time are spread out. Short wordless noises
  between two pauses (breaths, lip smacks) count as part of the pause and are removed along with silence.

**Editing**
- Timeline with a main video track (magnetic, closes gaps automatically) and any number of
  additional video and audio tracks
- Split, trim (drag clip edges), move, duplicate, delete, magnet/snapping,
  zoom, waveform, markers
- Volume, mute, fade in/out, mute or hide tracks
- **Clip transitions** (video and/or audio tracks) where two clips meet: **Crossfade** (picture dissolves,
  audio crossfades; uses material before/after the clip boundaries, the timeline length stays the same) or
  **via black/silence**. Length 50–4000 ms, adjustable audio curve, transitions are marked in the timeline.
- **Picture-in-picture:** clips on V2+ with position presets, size, X/Y, opacity and border
- **Audio files** (mp3, wav, m4a, flac …) on audio tracks
- Preview with audio. It uses the same render graph as the export.

**Export**
- Multiple sizes in one pass: Original, 4K, 1440p, 1080p, 720p, 480p,
  9:16 (Reels/TikTok/Shorts), 1:1, 4:5, custom size
- Other aspect ratios: fit (letterbox) or fill (crop)
- H.264, H.265, H.264 hardware (VideoToolbox on macOS; NVENC, Quick Sync or AMF on Windows/Linux –
  whichever works on the machine, otherwise fast software encoding), ProRes 422
- **Audio export** (in both modes): MP3, M4A (AAC), WAV, FLAC, bitrate 128–320 kbit/s,
  optional **loudness normalization to −16 LUFS**
- **Burn in subtitles** (💬 in the transcript): text from the transcript, follows all cuts. Presets Classic,
  Social Media (large capitals, current word highlighted) and Box. Font, size, position, colors, outline,
  background box and line length are adjustable, sizes relative to video height (works equally for 16:9 and 9:16).
  Rendering is done in Rust; ffmpeg needs no text filters (libass/drawtext) for it.
- SRT subtitles that follow the cuts (same lines as when burning in)

## Requirements

TextCut runs on **macOS, Windows and Linux**. It needs three tools:

| Tool | Purpose |
|---|---|
| Rust ≥ 1.86 | Build (via [rustup](https://rustup.rs)) |
| ffmpeg + ffprobe (7 or newer recommended) | Analysis, preview, export |
| whisper.cpp (`whisper-cli`) | Transcription (local, offline) |

TextCut finds the tools in `PATH`, in the usual install locations and **next to its own
executable** (also in a `bin` folder there) – handy for a portable setup. Paths can also be set
by hand in **Settings**.

### macOS

```bash
brew install ffmpeg whisper-cpp
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo run --release
```

### Windows 10/11

1. **Rust:** install [rustup-init.exe](https://rustup.rs) and accept the
   *Visual Studio C++ Build Tools* it offers (needed for linking).
2. **ffmpeg:** in PowerShell `winget install Gyan.FFmpeg`, then open a new terminal.
   (Alternatives: `scoop install ffmpeg`, `choco install ffmpeg`, or unpack a build from
   [gyan.dev](https://www.gyan.dev/ffmpeg/builds/) to `C:\ffmpeg`.)
3. **whisper.cpp:** download `whisper-bin-x64.zip` from the
   [whisper.cpp releases](https://github.com/ggml-org/whisper.cpp/releases) and unpack it to
   `C:\whisper` (with an NVIDIA GPU: `whisper-cublas-…-bin-x64.zip` is much faster).
4. Build and start:
   ```powershell
   cargo run --release
   ```
   The finished program is `target\release\textcut.exe`. To pass it on, put
   `ffmpeg.exe`, `ffprobe.exe` and the whisper files next to it (or into a `bin` folder beside it).

The model download uses `curl.exe`, which ships with Windows 10 (1803) and later.

### Linux

Build dependencies (window, audio output) plus ffmpeg:

```bash
# Debian / Ubuntu
sudo apt install build-essential cmake git pkg-config ffmpeg \
  libasound2-dev libxkbcommon-dev libgl1-mesa-dev libwayland-dev \
  libx11-dev libxcursor-dev libxrandr-dev libxi-dev

# Fedora (ffmpeg with all codecs from RPM Fusion)
sudo dnf install gcc gcc-c++ cmake git pkgconf-pkg-config ffmpeg \
  alsa-lib-devel libxkbcommon-devel mesa-libGL-devel wayland-devel libX11-devel

# Arch
sudo pacman -S base-devel cmake git ffmpeg alsa-lib libxkbcommon mesa wayland
```

whisper.cpp is rarely packaged, so build it once (takes a few minutes):

```bash
git clone https://github.com/ggml-org/whisper.cpp
cd whisper.cpp
cmake -B build -DBUILD_SHARED_LIBS=OFF        # with NVIDIA GPU additionally: -DGGML_CUDA=1
cmake --build build -j --config Release
mkdir -p ~/.local/bin && cp build/bin/whisper-cli ~/.local/bin/
```

Then install Rust (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`) and run
`cargo run --release` in the TextCut folder. File dialogs use the XDG desktop portal
(present on GNOME, KDE and most desktops).

Ubuntu 22.04 ships ffmpeg 4.4 – too old. Use Ubuntu 24.04 or newer, or a current static
build from [johnvansickle.com/ffmpeg](https://johnvansickle.com/ffmpeg/) copied to `~/.local/bin`.

### Whisper model

TextCut downloads the Whisper model via **Settings → Model → Download**
to `~/.textcut/models/` (Windows: `C:\Users\<name>\.textcut\models`).
`base` is fast, `small` or `large-v3-turbo` are more accurate.

### Subtitle fonts

Fonts are picked from the system: Arial/Impact/Verdana/Segoe UI (Windows, macOS),
DejaVu, Liberation or Noto Sans (Linux). Any other `.ttf`/`.otf` can be chosen in the subtitle window.

## Running

```bash
cargo run --release                   # start the app
cargo run --release -- video.mp4      # start with a video
cargo run --release -- projekt.tcf    # open a project
cargo test                            # tests (including real ffmpeg renders)
```

## Prebuilt binaries (CI)

GitHub Actions ([.github/workflows/build.yml](.github/workflows/build.yml)) tests and builds every
push on Linux, Windows and macOS (universal binary for Apple Silicon and Intel). The archives can be
downloaded from the run's **Artifacts** section.

To publish a release, push a version tag – the archives are attached to a GitHub release automatically:

```bash
git tag v0.2.0
git push origin v0.2.0
```

The archives contain the program only; ffmpeg and whisper.cpp still have to be installed (see above).
The macOS binary is not signed: after downloading, run `xattr -d com.apple.quarantine textcut`
once, or allow it under *System Settings → Privacy & Security*.

## Workflow

1. Drag a video into the window. It lands on the main track and is transcribed and analyzed for silence.
2. Click **🔇 Remove silence automatically**.
3. Select slips of the tongue in the text and delete them with `Delete`.
4. Optional: put a second video on V2 via **🖼 Picture-in-picture** and position it in the inspector.
5. **⬆ Export**, e.g. 1080p and 9:16 at the same time.

## Keyboard shortcuts

`Space` play · `Delete` delete text/clip · `S` split · `⌘Z`/`⇧⌘Z` undo/redo ·
`⌘D` duplicate · `←/→` frame (with ⇧: 1 s) · `J/K/L` · `⌘S/⌘O/⌘I/⌘E`

## Architecture

```
src/
├── project.rs     data model + editing logic (cuts, split, time mapping, tokens), tested
├── render.rs      timeline → ffmpeg filter_complex (shared by preview & export)
├── preview.rs     scrub frames + real-time playback (rawvideo/PCM pipes, rodio)
├── export.rs      export presets, codecs, progress, SRT
├── subtitles.rs   subtitle lines, text rendering (ab_glyph), burn-in via image sequence
├── media.rs       ffprobe, waveform, silencedetect
├── transcribe.rs  whisper-cli invocation, JSON parser, model download
├── settings.rs    ~/.textcut/settings.json
├── app.rs         app state, undo/redo, background jobs, keyboard
└── ui/            transcript editor, timeline, panels, dialogs (egui)
```

Cuts are stored non-destructively as source time ranges. The played ranges are the
source range minus the cuts, snapped to the frame grid. ffmpeg selects them per clip
in a single pass via `select`/`aselect`. This stays fast even with hundreds of text cuts,
and picture and sound stay in sync.
