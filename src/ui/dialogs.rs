//! Dialoge: Export, Einstellungen, Projektformat, Hilfe.

use super::style;
use crate::app::{App, Placement, TaskKind};
use crate::export::{AspectHandling, AudioFormat, Codec, ExportJob, ExportKind, PRESETS};
use crate::project::ProjectMode;
use crate::transcribe::MODELS;
use crate::util::{even, fmt_time};
use eframe::egui::{self, RichText};
use std::path::PathBuf;

pub struct ExportUi {
    pub open: bool,
    pub presets: Vec<bool>,
    pub custom: bool,
    pub custom_w: u32,
    pub custom_h: u32,
    pub codec: Codec,
    pub crf: u32,
    pub aspect: AspectHandling,
    /// Ausgewählte Audioformate (Reihenfolge wie `AudioFormat::ALL`).
    pub audio: [bool; 4],
    pub kbps: u32,
    pub loudnorm: bool,
    pub srt: bool,
    pub dir: String,
    pub base_name: String,
    pub finished: Vec<(String, PathBuf)>,
    /// Modus, für den die Auswahl zuletzt vorbelegt wurde.
    defaults_for: Option<ProjectMode>,
}

impl Default for ExportUi {
    fn default() -> Self {
        ExportUi {
            open: false,
            presets: vec![false; PRESETS.len()],
            custom: false,
            custom_w: 1920,
            custom_h: 1080,
            codec: Codec::H264,
            crf: 20,
            aspect: AspectHandling::AsClip,
            audio: [false; 4],
            kbps: 192,
            loudnorm: false,
            srt: false,
            dir: String::new(),
            base_name: String::new(),
            finished: Vec::new(),
            defaults_for: None,
        }
    }
}

impl ExportUi {
    /// Sinnvolle Vorauswahl je nach Projektmodus.
    fn apply_defaults(&mut self, mode: ProjectMode) {
        if self.defaults_for == Some(mode) {
            return;
        }
        self.defaults_for = Some(mode);
        self.presets.iter_mut().for_each(|p| *p = false);
        self.audio = [false; 4];
        match mode {
            ProjectMode::Video => self.presets[0] = true,
            ProjectMode::Audio => {
                self.audio[0] = true;
                self.loudnorm = true;
            }
        }
    }

    fn count(&self, mode: ProjectMode) -> usize {
        let video = match mode {
            ProjectMode::Video => self.presets.iter().filter(|b| **b).count() + self.custom as usize,
            ProjectMode::Audio => 0,
        };
        video + self.audio.iter().filter(|b| **b).count() + self.srt as usize
    }
}

pub fn export_window(app: &mut App, ctx: &egui::Context) {
    if !app.export_ui.open {
        return;
    }
    let mode = app.project.mode;
    let mut open = true;
    let mut start = false;
    egui::Window::new("⬆ Exportieren")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(540.0)
        .show(ctx, |ui| {
            let duration = app.project.duration();
            let (pw, ph) = (app.project.width, app.project.height);
            let e = &mut app.export_ui;
            e.apply_defaults(mode);
            if e.base_name.is_empty() {
                e.base_name = app.project.name.clone();
            }
            if e.dir.is_empty() {
                e.dir = default_export_dir(&app.settings.export_dir, app.project_path.as_ref());
            }
            match mode {
                ProjectMode::Video => ui.label(format!("Timeline: {} · Projekt {}×{}", fmt_time(duration), pw, ph)),
                ProjectMode::Audio => ui.label(format!("Audio-Projekt · {}", fmt_time(duration))),
            };

            if mode == ProjectMode::Video {
                ui.add_space(6.0);
                section(ui, "🎬 Video – Größen (mehrere gleichzeitig möglich)");
                egui::Grid::new("presets").num_columns(2).show(ui, |ui| {
                    for (i, (name, _, w, _)) in PRESETS.iter().enumerate() {
                        let label = if *w == 0 { format!("{name} – {pw}×{ph}") } else { name.to_string() };
                        ui.checkbox(&mut e.presets[i], label);
                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut e.custom, "Eigene Größe:");
                    ui.add_enabled(e.custom, egui::DragValue::new(&mut e.custom_w).range(16..=7680).suffix(" px"));
                    ui.label("×");
                    ui.add_enabled(e.custom, egui::DragValue::new(&mut e.custom_h).range(16..=7680).suffix(" px"));
                });
                ui.horizontal(|ui| {
                    ui.label("Anderes Seitenverhältnis:");
                    ui.radio_value(&mut e.aspect, AspectHandling::AsClip, "Wie im Clip");
                    ui.radio_value(&mut e.aspect, AspectHandling::Fit, "Einpassen");
                    ui.radio_value(&mut e.aspect, AspectHandling::Fill, "Füllen");
                });
                egui::Grid::new("codec").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                    ui.label("Codec");
                    egui::ComboBox::from_id_salt("codec").selected_text(e.codec.label()).width(280.0).show_ui(ui, |ui| {
                        for c in Codec::ALL {
                            ui.selectable_value(&mut e.codec, c, c.label());
                        }
                    });
                    ui.end_row();
                    if matches!(e.codec, Codec::H264 | Codec::H265) {
                        ui.label("Qualität (CRF)");
                        ui.add(egui::Slider::new(&mut e.crf, 14..=32).text("niedriger = besser"));
                        ui.end_row();
                    }
                });
            }

            ui.add_space(8.0);
            section(
                ui,
                if mode == ProjectMode::Video { "🎵 Nur Audio exportieren (Tonspur der Timeline)" } else { "🎵 Audio-Formate" },
            );
            ui.horizontal_wrapped(|ui| {
                for (i, f) in AudioFormat::ALL.into_iter().enumerate() {
                    ui.checkbox(&mut e.audio[i], f.label());
                }
            });
            let lossy = AudioFormat::ALL.iter().zip(e.audio).any(|(f, on)| on && f.lossy());
            ui.add_enabled_ui(lossy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Bitrate (MP3/M4A)");
                    for k in [128, 192, 256, 320] {
                        ui.selectable_value(&mut e.kbps, k, format!("{k} kbit/s"));
                    }
                });
            });
            ui.checkbox(&mut e.loudnorm, "Lautheit normalisieren (−16 LUFS, Podcast-/Streaming-Standard)")
                .on_hover_text("Gleicht die Lautstärke an. Gilt für alle Exporte dieses Durchgangs, auch Videos.");

            ui.add_space(8.0);
            section(ui, "📝 Untertitel / Transkript");
            if mode == ProjectMode::Video {
                ui.horizontal(|ui| {
                    let mut burn = app.project.subtitles.burn_in;
                    if ui.checkbox(&mut burn, "💬 Untertitel ins Video einbrennen").changed() {
                        let mut snapshot = app.project.clone();
                        snapshot.subtitles.burn_in = !burn;
                        app.push_undo(snapshot);
                        app.project.subtitles.burn_in = burn;
                    }
                    if ui.button("Stil …").clicked() {
                        app.show_subtitles = true;
                    }
                });
            }
            let e = &mut app.export_ui;
            ui.checkbox(&mut e.srt, "SRT-Untertitel als Datei (Zeiten passend zu den Schnitten)");

            ui.add_space(8.0);
            egui::Grid::new("target").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Ordner");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut e.dir).desired_width(280.0));
                    if ui.button("…").clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_directory(&e.dir).pick_folder() {
                            e.dir = d.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("Dateiname");
                ui.add(egui::TextEdit::singleline(&mut e.base_name).desired_width(280.0));
                ui.end_row();
            });

            let count = e.count(mode);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let enabled = count > 0 && duration > 0.0;
                let btn = egui::Button::new(RichText::new(format!("⬆ {count} Datei(en) exportieren")).strong()).fill(style::ACCENT_DIM);
                if ui.add_enabled(enabled, btn).clicked() {
                    start = true;
                }
                if duration <= 0.0 {
                    ui.label(RichText::new("Timeline ist leer").color(style::WARN));
                }
            });

            let running: Vec<_> = app.tasks.iter().filter(|t| t.kind == TaskKind::Export).collect();
            if !running.is_empty() {
                ui.separator();
                for t in running {
                    ui.horizontal(|ui| {
                        ui.add(egui::ProgressBar::new(t.progress).show_percentage().desired_width(220.0));
                        ui.label(&t.label);
                        if let Some(c) = &t.cancel {
                            if ui.small_button("✖").clicked() {
                                c.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                        }
                    });
                }
            }
            if !app.export_ui.finished.is_empty() {
                ui.separator();
                ui.label(RichText::new("Fertig:").strong());
                for (label, path) in app.export_ui.finished.iter().rev().take(8) {
                    ui.horizontal(|ui| {
                        ui.label(format!("✅ {label}"));
                        if ui.small_button("Im Ordner zeigen").clicked() {
                            reveal(path);
                        }
                    });
                }
            }
        });
    app.export_ui.open = open;
    if start {
        let jobs = build_jobs(app);
        app.settings.export_dir = app.export_ui.dir.clone();
        app.settings.save();
        app.queue_exports(jobs);
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.separator();
    ui.label(RichText::new(title).strong());
}

fn build_jobs(app: &App) -> Vec<ExportJob> {
    let e = &app.export_ui;
    let mode = app.project.mode;
    let dir = PathBuf::from(&e.dir);
    let base = if e.base_name.trim().is_empty() { "export".to_string() } else { e.base_name.trim().to_string() };
    let loudnorm = e.loudnorm;
    let mut jobs = Vec::new();
    if mode == ProjectMode::Video {
        let mut video = |label: String, slug: String, w: u32, h: u32| {
            jobs.push(ExportJob {
                label,
                kind: ExportKind::Video { width: w, height: h, codec: e.codec, crf: e.crf, aspect: e.aspect },
                path: dir.join(format!("{base}_{slug}.{}", e.codec.extension())),
                loudnorm,
            });
        };
        for (i, (name, slug, w, h)) in PRESETS.iter().enumerate() {
            if e.presets[i] {
                video(name.to_string(), slug.to_string(), *w, *h);
            }
        }
        if e.custom {
            let (w, h) = (even(e.custom_w as f64), even(e.custom_h as f64));
            video(format!("{w}×{h}"), format!("{w}x{h}"), w, h);
        }
    }
    for (format, on) in AudioFormat::ALL.into_iter().zip(e.audio) {
        if on {
            jobs.push(ExportJob {
                label: format!("Audio {}", format.label()),
                kind: ExportKind::Audio { format, kbps: e.kbps },
                path: dir.join(format!("{base}.{}", format.extension())),
                loudnorm,
            });
        }
    }
    if e.srt {
        jobs.push(ExportJob { label: "Untertitel".into(), kind: ExportKind::Subtitles, path: dir.join(format!("{base}.srt")), loudnorm });
    }
    jobs
}

fn default_export_dir(saved: &str, project: Option<&PathBuf>) -> String {
    if !saved.is_empty() {
        return saved.to_string();
    }
    if let Some(dir) = project.and_then(|p| p.parent()) {
        return dir.display().to_string();
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let movies = home.join("Movies");
    if movies.is_dir() { movies } else { home.join("Videos") }.display().to_string()
}

fn reveal(path: &PathBuf) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}

pub fn settings_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_settings {
        return;
    }
    let mut open = true;
    let mut download = false;
    let mut retranscribe_hint = false;
    let mut retranscribe_all = false;
    egui::Window::new("⚙ Einstellungen").open(&mut open).default_width(560.0).show(ctx, |ui| {
        let s = &mut app.settings;
        ui.label(RichText::new("📝 Spracherkennung (whisper.cpp)").strong());
        let found = s.whisper_binary();
        egui::Grid::new("whisper").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("whisper-cli");
            ui.vertical(|ui| {
                ui.add(egui::TextEdit::singleline(&mut s.whisper).hint_text("automatisch suchen").desired_width(300.0));
                match &found {
                    Some(p) if p.is_file() => ui.label(RichText::new(format!("✅ {}", p.display())).small().color(egui::Color32::LIGHT_GREEN)),
                    _ => ui.label(RichText::new("❌ Nicht gefunden – Installation: brew install whisper-cpp").small().color(style::WARN)),
                };
            });
            ui.end_row();
            ui.label("Modell");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("model").selected_text(&s.model).show_ui(ui, |ui| {
                    for (name, mb) in MODELS {
                        ui.selectable_value(&mut s.model, name.to_string(), format!("{name} (~{mb} MB)"));
                    }
                });
                let exists = s.model_file().is_file();
                if exists {
                    ui.label(RichText::new("✅ vorhanden").color(egui::Color32::LIGHT_GREEN));
                } else if app.tasks.iter().any(|t| t.kind == TaskKind::Download) {
                    ui.spinner();
                } else if ui.button("⬇ Herunterladen").clicked() {
                    download = true;
                }
            });
            ui.end_row();
            ui.label("Eigene Modelldatei");
            ui.add(egui::TextEdit::singleline(&mut s.model_path).hint_text("optional: Pfad zu ggml-*.bin").desired_width(300.0));
            ui.end_row();
            ui.label("Sprache");
            egui::ComboBox::from_id_salt("lang").selected_text(&s.language).show_ui(ui, |ui| {
                for l in ["auto", "de", "en", "fr", "es", "it", "nl", "pl", "pt", "tr", "ru"] {
                    ui.selectable_value(&mut s.language, l.to_string(), l);
                }
            });
            ui.end_row();
            ui.label("Threads");
            ui.add(egui::Slider::new(&mut s.threads, 1..=16));
            ui.end_row();
            ui.label("Beim Import");
            ui.vertical(|ui| {
                ui.checkbox(&mut s.auto_transcribe, "automatisch transkribieren");
                let r = ui.checkbox(&mut s.verbatim, "Füllwörter mit transkribieren (wortgetreu)").on_hover_text(
                    "Whisper lässt „äh“, „ähm“, Wiederholungen und Versprecher normalerweise weg.\n\
                     Mit dieser Option werden sie mitgeschrieben und lassen sich herausschneiden.\n\
                     Für de/en/fr/es/it/nl. Wörter mit kaum Ton werden gelb als unsicher markiert.",
                );
                if r.changed() {
                    retranscribe_hint = true;
                }
                ui.checkbox(&mut s.auto_detect_silence, "automatisch Stille analysieren");
            });
            ui.end_row();
        });

        if !app.project.media.is_empty() && ui.button("📝 Alle Medien neu transkribieren").on_hover_text("Nötig nach Änderung von Modell, Sprache oder „wortgetreu“").clicked() {
            retranscribe_all = true;
        }

        ui.separator();
        ui.label(RichText::new("🔇 Stille-Erkennung").strong());
        egui::Grid::new("silence").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Schwelle");
            ui.add(egui::Slider::new(&mut s.silence_db, -70.0..=-15.0).suffix(" dB"))
                .on_hover_text("Alles leiser als dieser Pegel gilt als Stille. Bei Hintergrundrauschen höher stellen (z.B. -30 dB).");
            ui.end_row();
            ui.label("Mindestdauer");
            ui.add(egui::Slider::new(&mut s.silence_min, 0.2..=3.0).suffix(" s"));
            ui.end_row();
            ui.label("Rand behalten");
            ui.add(egui::Slider::new(&mut s.silence_pad, 0.0..=0.5).suffix(" s"))
                .on_hover_text("So viel Pause bleibt vor/nach dem Sprechen stehen, damit Schnitte natürlich klingen.");
            ui.end_row();
            ui.label("Füllwörter");
            ui.add(egui::TextEdit::multiline(&mut s.fillers).desired_rows(2).desired_width(300.0));
            ui.end_row();
        });
        ui.label(RichText::new("Nach Änderung von Schwelle/Mindestdauer: „Text & Stille → Stille neu analysieren“.").small().weak());

        ui.separator();
        ui.label(RichText::new("🎬 Vorschau & Programme").strong());
        egui::Grid::new("misc").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            ui.label("Vorschau-Bildrate");
            ui.add(egui::Slider::new(&mut s.preview_fps, 10..=60).suffix(" fps"));
            ui.end_row();
            ui.label("Textgröße Transkript");
            ui.add(egui::Slider::new(&mut s.text_size, 12.0..=28.0));
            ui.end_row();
            ui.label("ffmpeg");
            ui.add(egui::TextEdit::singleline(&mut s.ffmpeg).hint_text("automatisch").desired_width(300.0));
            ui.end_row();
            ui.label("ffprobe");
            ui.add(egui::TextEdit::singleline(&mut s.ffprobe).hint_text("automatisch").desired_width(300.0));
            ui.end_row();
        });
        ui.add_space(4.0);
        if ui.button("💾 Speichern").clicked() {
            s.save();
        }
    });
    if download {
        app.download_model();
    }
    if retranscribe_hint && !app.project.media.is_empty() {
        app.set_status("Gilt für neue Transkriptionen – „📝 Alle Medien neu transkribieren“ wendet es auf vorhandene an.");
    }
    if retranscribe_all {
        app.settings.save();
        let ids: Vec<crate::project::Id> = app.project.media.iter().filter(|m| m.has_audio).map(|m| m.id).collect();
        for id in ids {
            app.start_transcription(id);
        }
    }
    if !open {
        app.settings.save();
    }
    app.show_settings = open;
}

pub fn project_settings_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_project_settings {
        return;
    }
    let mut open = true;
    egui::Window::new("Projektformat").open(&mut open).show(ctx, |ui| {
        let (mut w, mut h, mut fps) = (app.project.width, app.project.height, app.project.fps);
        ui.label("Leinwand (Vorschau & „Projektgröße“-Export)");
        ui.horizontal_wrapped(|ui| {
            for (label, pw, ph) in [
                ("16:9 1080p", 1920, 1080),
                ("16:9 4K", 3840, 2160),
                ("9:16 Hochkant", 1080, 1920),
                ("1:1", 1080, 1080),
                ("4:5", 1080, 1350),
            ] {
                if ui.button(label).clicked() {
                    (w, h) = (pw, ph);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut w).range(16..=7680).suffix(" px"));
            ui.label("×");
            ui.add(egui::DragValue::new(&mut h).range(16..=7680).suffix(" px"));
        });
        ui.horizontal(|ui| {
            ui.label("Bildrate");
            for f in [24, 25, 30, 50, 60] {
                ui.selectable_value(&mut fps, f, format!("{f}"));
            }
        });
        if (w, h, fps) != (app.project.width, app.project.height, app.project.fps) {
            app.checkpoint();
            app.project.width = even(w as f64);
            app.project.height = even(h as f64);
            app.project.fps = fps;
            app.project.canvas_auto = false;
            app.project.repack_main();
        }
    });
    app.show_project_settings = open;
}

pub fn help_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_help {
        return;
    }
    let mut open = true;
    egui::Window::new("Hilfe").open(&mut open).default_width(520.0).show(ctx, |ui| {
        ui.label(RichText::new("So funktioniert TextCutForge").strong().size(16.0));
        ui.label("0. Beim Start Video- oder Audio-Projekt wählen (Datei → Neues Projekt …). Im Audio-Modus gibt es nur Tonspuren; Videodateien liefern ihren Ton.");
        ui.label("1. Medien importieren (oder ins Fenster ziehen). Sie werden automatisch transkribiert und auf Stille analysiert.");
        ui.label("2. Im Text-Editor Wörter markieren und Entf drücken → die Stelle wird aus dem Video geschnitten. Nochmal Entf stellt sie wieder her.");
        ui.label("3. „Stille automatisch entfernen“ schneidet alle Pausen heraus. ⏸-Marken im Text sind Pausen, die du auch einzeln löschen kannst.");
        ui.label("4. Bild-in-Bild: Clip auf V2 legen (Medien → 🖼), Position/Größe im Inspektor.");
        ui.label("5. Exportieren: mehrere Größen auf einmal (z.B. 16:9 und 9:16) – und/oder nur den Ton als MP3/M4A/WAV/FLAC, optional lautheitsnormalisiert.");
        ui.add_space(8.0);
        ui.label(RichText::new("Tastenkürzel").strong());
        egui::Grid::new("keys").num_columns(2).striped(true).show(ui, |ui| {
            for (k, d) in [
                ("Leertaste", "Abspielen / Pause"),
                ("Entf / ⌫", "Markierten Text schneiden/wiederherstellen bzw. Clip löschen"),
                ("S  oder  ⌘B", "Am Abspielkopf teilen"),
                ("⌘Z / ⇧⌘Z", "Rückgängig / Wiederholen"),
                ("⌘D", "Clip duplizieren"),
                ("← / →", "Ein Bild zurück / vor (mit ⇧: 1 Sekunde)"),
                ("J / K / L", "5 s zurück / Stopp / 5 s vor"),
                ("Pos1 / Ende", "Anfang / Ende"),
                ("+ / −, ⌘+Scrollen", "Timeline zoomen"),
                ("⇧+Klick", "Textauswahl erweitern"),
                ("Esc", "Auswahl aufheben"),
                ("⌘S / ⌘O / ⌘I / ⌘E", "Speichern / Öffnen / Importieren / Exportieren"),
            ] {
                ui.label(RichText::new(k).monospace());
                ui.label(d);
                ui.end_row();
            }
        });
    });
    app.show_help = open;
}

/// Startbildschirm: Video- oder Audio-Projekt wählen.
pub fn start_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_start {
        return;
    }
    let can_cancel = !app.project.clips.is_empty() || !app.project.media.is_empty();
    let mut choice: Option<ProjectMode> = None;
    let mut open_dialog = false;
    let mut open_recent: Option<PathBuf> = None;
    let mut cancel = false;
    egui::Modal::new(egui::Id::new("start")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("⚒ TextCutForge").size(28.0).strong().color(style::ACCENT));
            ui.label(RichText::new("Schneide Video und Audio wie ein Textdokument").weak());
        });
        ui.add_space(14.0);
        ui.label(RichText::new("Neues Projekt").strong().size(16.0));
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            let cards = [
                (
                    ProjectMode::Video,
                    "🎬",
                    "Video-Projekt",
                    "Videos per Text schneiden, Stille entfernen,\nBild-in-Bild, Audiospuren, Export in\nmehreren Größen – plus Audio-Export.",
                ),
                (
                    ProjectMode::Audio,
                    "🎧",
                    "Audio-Projekt",
                    "Podcasts, Interviews, Voiceover:\nnur Ton, per Text schneiden, Stille\nentfernen, Export als MP3/M4A/WAV/FLAC.",
                ),
            ];
            for (ui, (mode, icon, title, desc)) in cols.iter_mut().zip(cards) {
                let text = format!("{icon}\n{title}");
                let btn = egui::Button::new(RichText::new(text).size(22.0).strong()).min_size(egui::vec2(ui.available_width(), 110.0));
                if ui.add(btn).clicked() {
                    choice = Some(mode);
                }
                ui.label(RichText::new(desc).small().weak());
            }
        });
        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("📂 Projekt öffnen …").clicked() {
                open_dialog = true;
            }
            if can_cancel && ui.button("Abbrechen").clicked() {
                cancel = true;
            }
        });
        if !app.settings.recent.is_empty() {
            ui.label(RichText::new("Zuletzt geöffnet").small().weak());
            for p in app.settings.recent.iter().take(5) {
                let name = crate::util::file_stem(p);
                if ui.link(name).on_hover_text(p.display().to_string()).clicked() {
                    open_recent = Some(p.clone());
                }
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Tipp: Dateien einfach ins Fenster ziehen.").small().weak());
    });
    if let Some(mode) = choice {
        app.new_project(mode);
        app.import_dialog(Placement::Main);
    }
    if open_dialog {
        app.open_dialog();
    }
    if let Some(p) = open_recent {
        app.open_project(&p);
    }
    if cancel {
        app.show_start = false;
    }
}

/// Untertitel-Fenster: Einbrennen und Stil (Änderungen sofort in der Vorschau).
pub fn subtitle_window(app: &mut App, ctx: &egui::Context) {
    if !app.show_subtitles {
        return;
    }
    use crate::project::{SubPosition, SubtitleStyle};
    let mut open = true;
    let before = app.project.subtitles.clone();
    let mut st = before.clone();
    let mut jump_next = false;
    egui::Window::new("💬 Untertitel").open(&mut open).default_width(430.0).show(ctx, |ui| {
        ui.checkbox(&mut st.burn_in, RichText::new("Untertitel ins Video einbrennen").strong())
            .on_hover_text("Wirkt in Vorschau und Video-Export. Der Text folgt allen Schnitten.");
        ui.label(RichText::new("Text kommt aus dem Transkript der Hauptspur; gelöschte Wörter fehlen automatisch.").small().weak());
        ui.add_enabled_ui(st.burn_in, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Vorlage:");
                if ui.button("Klassisch").clicked() {
                    st = SubtitleStyle { burn_in: true, font: st.font.clone(), ..Default::default() };
                }
                if ui.button("Social Media").clicked() {
                    st = SubtitleStyle {
                        burn_in: true,
                        font: st.font.clone(),
                        size: 7.0,
                        position: SubPosition::Bottom,
                        margin: 18.0,
                        highlight: true,
                        outline: 7.0,
                        uppercase: true,
                        max_chars: 18,
                        max_lines: 2,
                        ..Default::default()
                    };
                }
                if ui.button("Box").clicked() {
                    st = SubtitleStyle { burn_in: true, font: st.font.clone(), box_bg: true, outline: 0.0, ..Default::default() };
                }
            });
            ui.separator();
            egui::Grid::new("subs").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Schrift");
                ui.horizontal(|ui| {
                    let fonts = crate::subtitles::available_fonts();
                    let current = crate::subtitles::font_path(&st).map(|p| {
                        fonts.iter().find(|(_, f)| *f == p).map(|(n, _)| n.clone()).unwrap_or_else(|| crate::util::file_stem(&p))
                    });
                    egui::ComboBox::from_id_salt("font").selected_text(current.unwrap_or_else(|| "– keine –".into())).width(170.0).show_ui(ui, |ui| {
                        for (name, path) in &fonts {
                            if ui.selectable_label(st.font == path.to_string_lossy(), name).clicked() {
                                st.font = path.to_string_lossy().into_owned();
                            }
                        }
                    });
                    if ui.button("Andere …").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Schrift", &["ttf", "otf", "ttc"]).pick_file() {
                            st.font = p.display().to_string();
                        }
                    }
                });
                ui.end_row();
                ui.label("Größe");
                ui.add(egui::Slider::new(&mut st.size, 2.0..=14.0).suffix(" % der Höhe"));
                ui.end_row();
                ui.label("Position");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut st.position, SubPosition::Bottom, "Unten");
                    ui.selectable_value(&mut st.position, SubPosition::Center, "Mitte");
                    ui.selectable_value(&mut st.position, SubPosition::Top, "Oben");
                });
                ui.end_row();
                if st.position != SubPosition::Center {
                    ui.label("Abstand zum Rand");
                    ui.add(egui::Slider::new(&mut st.margin, 0.0..=40.0).suffix(" %"));
                    ui.end_row();
                }
                ui.label("Textfarbe");
                ui.color_edit_button_srgb(&mut st.color);
                ui.end_row();
                ui.label("Aktuelles Wort");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.highlight, "hervorheben");
                    if st.highlight {
                        ui.color_edit_button_srgb(&mut st.highlight_color);
                    }
                });
                ui.end_row();
                ui.label("Kontur");
                ui.horizontal(|ui| {
                    ui.add(egui::Slider::new(&mut st.outline, 0.0..=16.0).suffix(" px"));
                    ui.color_edit_button_srgb(&mut st.outline_color);
                });
                ui.end_row();
                ui.label("Hintergrund");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut st.box_bg, "Box");
                    if st.box_bg {
                        ui.add(egui::Slider::new(&mut st.box_opacity, 0.1..=1.0).text("Deckkraft"));
                    }
                });
                ui.end_row();
                ui.label("Schreibweise");
                ui.checkbox(&mut st.uppercase, "GROSSBUCHSTABEN");
                ui.end_row();
                ui.label("Zeichen pro Untertitel");
                ui.add(egui::Slider::new(&mut st.max_chars, 8..=80));
                ui.end_row();
                ui.label("Max. Zeilen");
                ui.add(egui::Slider::new(&mut st.max_lines, 1..=3));
                ui.end_row();
            });
            if crate::subtitles::font_path(&st).is_none_or(|p| !p.is_file()) {
                ui.colored_label(style::WARN, "⚠ Keine Schrift gefunden – bitte über „Andere …“ eine .ttf/.otf wählen.");
            }
            ui.add_space(4.0);
            if ui.button("⏭ Zur nächsten Untertitelzeile springen").on_hover_text("Damit die Vorschau einen Untertitel zeigt").clicked() {
                jump_next = true;
            }
        });
    });
    if st != before {
        if !app.inspector_editing {
            let mut snapshot = app.project.clone();
            snapshot.subtitles = before;
            app.push_undo(snapshot);
            app.inspector_editing = true;
        } else {
            app.touch();
        }
        app.project.subtitles = st;
    }
    if jump_next {
        let cues = crate::subtitles::cues(&app.project, app.project.subtitles.max_chars);
        let t = app.playhead + 0.01;
        match cues.iter().find(|c| c.start > t).or(cues.first()) {
            Some(c) => app.seek(c.start + 0.05),
            None => app.set_status("Keine Untertitel – zuerst transkribieren."),
        }
    }
    app.show_subtitles = open;
}
