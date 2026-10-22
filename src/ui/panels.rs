//! Menu, status bar, media panel, inspector and preview.

use super::style;
use crate::app::{App, Placement, TaskKind};
use crate::project::{CutKind, FitMode, Id, MediaKind, PipPreset, TrackKind};
use crate::util::fmt_time;
use eframe::egui::{self, Color32, RichText, Sense, Stroke, StrokeKind, Vec2};

pub fn menu_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::top("menu").show(ctx, |ui| {
        egui::menu::bar(ui, |ui| {
            ui.label(RichText::new("⚒ TextCutForge").strong().color(style::ACCENT));
            let mode = if app.project.is_audio() { "🎧 Audio" } else { "🎬 Video" };
            ui.label(RichText::new(mode).small().background_color(egui::Color32::from_gray(50)))
                .on_hover_text("Project mode - use \"File → New project\" to switch");
            ui.separator();
            ui.menu_button("File", |ui| {
                if ui.button("New project …").clicked() {
                    app.show_start = true;
                    ui.close_menu();
                }
                if ui.button("Open project …   ⌘O").clicked() {
                    app.open_dialog();
                    ui.close_menu();
                }
                let recent = app.settings.recent.clone();
                if !recent.is_empty() {
                    ui.menu_button("Open recent", |ui| {
                        for p in recent {
                            if ui.button(p.display().to_string()).clicked() {
                                app.open_project(&p);
                                ui.close_menu();
                            }
                        }
                    });
                }
                if ui.button("Save   ⌘S").clicked() {
                    app.save(false);
                    ui.close_menu();
                }
                if ui.button("Save as …   ⇧⌘S").clicked() {
                    app.save(true);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Import media …   ⌘I").clicked() {
                    app.import_dialog(Placement::Main);
                    ui.close_menu();
                }
                if ui.button("Export …   ⌘E").clicked() {
                    app.export_ui.open = true;
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Settings …").clicked() {
                    app.show_settings = true;
                    ui.close_menu();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(app.can_undo(), egui::Button::new("Undo   ⌘Z")).clicked() {
                    app.undo();
                    ui.close_menu();
                }
                if ui.add_enabled(app.can_redo(), egui::Button::new("Redo   ⇧⌘Z")).clicked() {
                    app.redo();
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Split at playhead   S").clicked() {
                    app.split_at_playhead();
                    ui.close_menu();
                }
                if ui.button("Delete clip   Del").clicked() {
                    app.delete_selected_clip();
                    ui.close_menu();
                }
                if ui.button("Duplicate clip   ⌘D").clicked() {
                    app.duplicate_selected_clip();
                    ui.close_menu();
                }
            });
            ui.menu_button("Text & Silence", |ui| {
                if ui.button("🔇 Remove silence automatically (all clips)").clicked() {
                    app.remove_silence_all();
                    ui.close_menu();
                }
                if ui.button("↺ Reset all silence cuts").clicked() {
                    let ids = app.project.clips.iter().map(|c| c.id).collect();
                    app.restore_silence(ids);
                    ui.close_menu();
                }
                if ui.button("🔄 Re-analyze silence (new threshold)").clicked() {
                    let ids: Vec<Id> = app.project.media.iter().map(|m| m.id).collect();
                    for id in ids {
                        app.start_silence_detection(id);
                    }
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("🗑 Remove filler words").clicked() {
                    app.remove_fillers_all();
                    ui.close_menu();
                }
                ui.menu_button("〰 Smooth transitions", |ui| super::transcript::transition_settings(app, ui));
                if ui.button("📝 Transcribe all media").clicked() {
                    let ids: Vec<Id> =
                        app.project.media.iter().filter(|m| m.transcript.is_none() && m.has_audio).map(|m| m.id).collect();
                    for id in ids {
                        app.start_transcription(id);
                    }
                    ui.close_menu();
                }
                if ui.button("↺ Reset all text cuts").clicked() {
                    app.checkpoint();
                    for c in &mut app.project.clips {
                        c.clear_cuts(Some(CutKind::Text));
                    }
                    app.project.repack_main();
                    ui.close_menu();
                }
            });
            ui.menu_button("Project", |ui| {
                if !app.project.is_audio() {
                    if ui.button("Format & frame rate …").clicked() {
                        app.show_project_settings = true;
                        ui.close_menu();
                    }
                    if ui.button("+ Video track").clicked() {
                        app.checkpoint();
                        app.project.add_track(TrackKind::Video);
                        ui.close_menu();
                    }
                }
                if ui.button("+ Audio track").clicked() {
                    app.checkpoint();
                    app.project.add_track(TrackKind::Audio);
                    ui.close_menu();
                }
                ui.separator();
                ui.menu_button("🎞 Clip transitions", |ui| super::timeline::clip_transition_settings(app, ui));
                if !app.project.is_audio() && ui.button("💬 Subtitles …").clicked() {
                    app.show_subtitles = true;
                    ui.close_menu();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts & guide").clicked() {
                    app.show_help = true;
                    ui.close_menu();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let btn = egui::Button::new(RichText::new("⬆ Export").strong()).fill(style::ACCENT_DIM);
                if ui.add(btn).clicked() {
                    app.export_ui.open = true;
                }
            });
        });
    });
}

pub fn status_bar(app: &mut App, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let mut cancel: Option<Id> = None;
            for t in app.tasks.iter().take(4) {
                ui.add(egui::ProgressBar::new(t.progress).desired_width(110.0).text(RichText::new(&t.label).small()));
                if t.cancel.is_some() && ui.small_button("✖").on_hover_text("Cancel").clicked() {
                    cancel = Some(t.id);
                }
            }
            if app.tasks.len() > 4 {
                ui.label(format!("+{} more", app.tasks.len() - 4));
            }
            if let Some(id) = cancel {
                if let Some(c) = app.tasks.iter().find(|t| t.id == id).and_then(|t| t.cancel.clone()) {
                    c.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            ui.label(RichText::new(&app.status).small()).on_hover_ui(|ui| {
                for l in app.log.iter().rev().take(15) {
                    ui.label(l);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let p = &app.project;
                let info = if p.is_audio() {
                    format!("Audio project · {}", fmt_time(p.duration()))
                } else {
                    format!("{}×{} · {} fps · {}", p.width, p.height, p.fps, fmt_time(p.duration()))
                };
                ui.label(RichText::new(info).small().weak());
            });
        });
    });
}

pub fn left_panel(app: &mut App, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        media_bin(app, ui);
        ui.add_space(8.0);
        ui.separator();
        inspector(app, ui);
    });
}

fn media_bin(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("🎞 Media");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("📂 Import").clicked() {
                app.import_dialog(Placement::Main);
            }
        });
    });
    let audio_mode = app.project.is_audio();
    if app.project.media.is_empty() {
        let hint = if audio_mode {
            "Drag files here or import them.\nAudio (including the audio of videos) goes on main track A1."
        } else {
            "Drag files here or import them.\nVideos go on the main track, audio on A1."
        };
        ui.label(RichText::new(hint).weak().small());
        return;
    }
    let mut action: Option<(Id, &str)> = None;
    for m in &app.project.media {
        let frame = egui::Frame::group(ui.style()).inner_margin(6.0).corner_radius(6.0);
        frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let icon = if m.kind == MediaKind::Video { "🎬" } else { "🎵" };
                ui.label(RichText::new(format!("{icon} {}", m.name)).strong()).on_hover_text(m.path.display().to_string());
            });
            let mut info = fmt_time(m.duration);
            if m.has_video {
                info += &format!(" · {}×{} · {:.0} fps", m.width, m.height, m.fps);
            }
            ui.label(RichText::new(info).small().weak());
            ui.horizontal_wrapped(|ui| {
                let tr = if app.task_running(TaskKind::Transcribe(m.id)) {
                    RichText::new("📝 running …").small().color(style::WARN)
                } else if let Some(t) = &m.transcript {
                    RichText::new(format!("📝 {} words", t.words.len())).small().color(Color32::LIGHT_GREEN)
                } else {
                    RichText::new("📝 –").small().weak()
                };
                ui.label(tr);
                let si = if app.task_running(TaskKind::Silence(m.id)) {
                    RichText::new("🔇 running …").small().color(style::WARN)
                } else if let Some(s) = &m.silences {
                    let total: f64 = s.iter().map(|r| r.len()).sum();
                    RichText::new(format!("🔇 {} pauses ({total:.0} s)", s.len())).small().color(Color32::LIGHT_GREEN)
                } else {
                    RichText::new("🔇 –").small().weak()
                };
                ui.label(si);
            });
            ui.horizontal_wrapped(|ui| {
                if audio_mode {
                    if m.has_audio && ui.small_button("➕ Main track").on_hover_text("Append to the end of A1").clicked() {
                        action = Some((m.id, "main"));
                    }
                    if m.has_audio
                        && ui.small_button("🎵 Second track").on_hover_text("On A2 at the playhead - e.g. music or ambience").clicked()
                    {
                        action = Some((m.id, "audio"));
                    }
                } else {
                    if m.kind == MediaKind::Video {
                        if ui.small_button("➕ Main track").clicked() {
                            action = Some((m.id, "main"));
                        }
                        if ui.small_button("🖼 Picture-in-picture").on_hover_text("On V2 at the playhead").clicked() {
                            action = Some((m.id, "pip"));
                        }
                    }
                    if m.has_audio && ui.small_button("🎵 Audio track").on_hover_text("Audio only, on A1 at the playhead").clicked() {
                        action = Some((m.id, "audio"));
                    }
                }
                if m.has_audio && ui.small_button("📝").on_hover_text("Transcribe (again)").clicked() {
                    action = Some((m.id, "transcribe"));
                }
                if ui.small_button("🗑").on_hover_text("Remove from project").clicked() {
                    action = Some((m.id, "remove"));
                }
            });
        });
    }
    if let Some((id, what)) = action {
        match what {
            "main" => app.add_to_timeline(id, Placement::Main),
            "pip" => app.add_to_timeline(id, Placement::Overlay),
            "audio" => app.add_to_timeline(id, Placement::Audio),
            "transcribe" => app.start_transcription(id),
            "remove" => app.remove_media(id),
            _ => {}
        }
    }
}

fn inspector(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("🔧 Inspector");
    let Some(id) = app.selected_clip else {
        ui.label(RichText::new("Select a clip in the timeline.").weak());
        return;
    };
    let Some(before) = app.project.clip(id).cloned() else { return };
    let fps = app.project.fps;
    let main = app.project.main_track();
    let track_kind = app.project.track(before.track).map(|t| t.kind);
    let media = app.project.media(before.source).cloned();
    let src_dur = media.as_ref().map(|m| m.duration).unwrap_or(before.src_out);
    let has_audio = media.as_ref().is_some_and(|m| m.has_audio);
    let is_video = track_kind == Some(TrackKind::Video);
    let mut clip = before.clone();
    let mut pip_preset: Option<PipPreset> = None;
    let mut remove_silence = false;

    ui.label(RichText::new(media.as_ref().map(|m| m.name.as_str()).unwrap_or("?")).strong());
    egui::Grid::new("clip_info").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
        ui.label("Timeline");
        ui.label(format!("{} – {}", fmt_time(clip.start), fmt_time(clip.end(fps))));
        ui.end_row();
        ui.label("Duration");
        ui.label(format!("{} (source {})", fmt_time(clip.duration(fps)), fmt_time(clip.src_out - clip.src_in)));
        ui.end_row();
        ui.label("In point");
        ui.add(egui::DragValue::new(&mut clip.src_in).range(0.0..=clip.src_out - 0.1).speed(0.04).suffix(" s").max_decimals(2));
        ui.end_row();
        ui.label("Out point");
        ui.add(egui::DragValue::new(&mut clip.src_out).range(clip.src_in + 0.1..=src_dur).speed(0.04).suffix(" s").max_decimals(2));
        ui.end_row();
        if clip.track != main {
            ui.label("Start");
            ui.add(egui::DragValue::new(&mut clip.start).range(0.0..=f64::MAX).speed(0.04).suffix(" s").max_decimals(2));
            ui.end_row();
        }
    });

    let text_cuts = clip.cuts.iter().filter(|c| c.kind == CutKind::Text).count();
    let silence_cuts = clip.cuts.iter().filter(|c| c.kind == CutKind::Silence).count();
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("✂ {text_cuts} text · 🔇 {silence_cuts} silence · −{:.1} s", clip.removed_time(fps))).color(style::CUT));
    });
    ui.horizontal_wrapped(|ui| {
        if ui.small_button("🔇 Remove silence").clicked() {
            remove_silence = true;
        }
        if ui.small_button("↺ Reset cuts").clicked() {
            clip.clear_cuts(None);
        }
    });

    if has_audio {
        ui.add_space(6.0);
        ui.label(RichText::new("🔊 Audio").strong());
        let mut vol_pct = clip.volume * 100.0;
        ui.add(egui::Slider::new(&mut vol_pct, 0.0..=200.0).suffix(" %").text("Volume"));
        clip.volume = vol_pct / 100.0;
        ui.checkbox(&mut clip.muted, "Mute");
    }
    ui.add_space(6.0);
    ui.label(RichText::new("🌓 Fades").strong());
    let edges = app.project.clip_edges(&before);
    if edges.start.is_some() || edges.end.is_some() {
        let side = match (edges.start.is_some(), edges.end.is_some()) {
            (true, true) => "start and end",
            (true, false) => "start",
            _ => "end",
        };
        ui.label(RichText::new(format!("🎞 Clip transition at {side} replaces the fade there")).small().weak());
    }
    ui.add(egui::Slider::new(&mut clip.fade_in, 0.0..=3.0).suffix(" s").text("Fade in"));
    ui.add(egui::Slider::new(&mut clip.fade_out, 0.0..=3.0).suffix(" s").text("Fade out"));

    if is_video {
        ui.add_space(6.0);
        ui.label(RichText::new("🖼 Picture / Picture-in-picture").strong());
        let t = &mut clip.transform;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut t.fit, FitMode::Fit, "Fit");
            ui.selectable_value(&mut t.fit, FitMode::Fill, "Fill");
            ui.selectable_value(&mut t.fit, FitMode::Custom, "Picture-in-picture");
        });
        if t.fit == FitMode::Custom {
            ui.horizontal(|ui| {
                ui.label("Position:");
                for (label, preset) in [
                    ("↖", PipPreset::TopLeft),
                    ("↗", PipPreset::TopRight),
                    ("⊙", PipPreset::Center),
                    ("↙", PipPreset::BottomLeft),
                    ("↘", PipPreset::BottomRight),
                ] {
                    if ui.button(label).clicked() {
                        pip_preset = Some(preset);
                    }
                }
            });
            let mut scale_pct = t.scale * 100.0;
            ui.add(egui::Slider::new(&mut scale_pct, 5.0..=100.0).suffix(" %").text("Size"));
            t.scale = scale_pct / 100.0;
            ui.add(egui::Slider::new(&mut t.x, -0.5..=1.0).text("X"));
            ui.add(egui::Slider::new(&mut t.y, -0.5..=1.0).text("Y"));
            ui.add(egui::Slider::new(&mut t.border, 0.0..=30.0).text("Border (px)"));
            if t.border > 0.0 {
                ui.horizontal(|ui| {
                    ui.label("Border color");
                    ui.color_edit_button_srgb(&mut t.border_color);
                });
            }
        }
        let mut op = t.opacity * 100.0;
        ui.add(egui::Slider::new(&mut op, 0.0..=100.0).suffix(" %").text("Opacity"));
        t.opacity = op / 100.0;
    }

    // Apply changes - one undo step per gesture.
    let changed = serde_json::to_string(&clip).ok() != serde_json::to_string(&before).ok();
    if changed || pip_preset.is_some() {
        if !app.inspector_editing {
            let mut snapshot = app.project.clone();
            if let Some(c) = snapshot.clip_mut(id) {
                *c = before.clone();
            }
            app.push_undo(snapshot);
            app.inspector_editing = true;
        } else {
            app.touch();
        }
        if let Some(c) = app.project.clip_mut(id) {
            *c = clip;
        }
        if let Some(p) = pip_preset {
            app.project.place_pip(id, p);
        }
        app.project.repack_main();
    }
    if !ui.input(|i| i.pointer.any_down()) && !ui.ctx().wants_keyboard_input() {
        app.inspector_editing = false;
    }
    if remove_silence {
        app.remove_silence(vec![id]);
    }
}

pub fn preview_panel(app: &mut App, ui: &mut egui::Ui) {
    if app.project.is_audio() {
        audio_player(app, ui);
        return;
    }
    ui.horizontal(|ui| {
        ui.heading("▶ Preview");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(fmt_time(app.playhead)).monospace().size(15.0));
        });
    });
    let avail = ui.available_size() - Vec2::new(0.0, 36.0);
    let aspect = app.project.width as f32 / app.project.height.max(1) as f32;
    let mut size = Vec2::new(avail.x, avail.x / aspect);
    if size.y > avail.y {
        size = Vec2::new(avail.y * aspect, avail.y);
    }
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(avail.x, size.y.max(50.0)), Sense::click());
    let img_rect = egui::Rect::from_center_size(rect.center(), size);
    ui.painter().rect_filled(img_rect, 0.0, Color32::BLACK);
    if let Some(tex) = &app.preview_tex {
        ui.painter().image(tex.id(), img_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    } else if app.project.clips.is_empty() {
        ui.painter().text(img_rect.center(), egui::Align2::CENTER_CENTER, "No clips", egui::FontId::proportional(16.0), Color32::GRAY);
    }
    if let Some(e) = &app.preview_error {
        ui.painter().text(img_rect.left_bottom() + Vec2::new(6.0, -6.0), egui::Align2::LEFT_BOTTOM, e, egui::FontId::proportional(11.0), style::CUT);
    }
    // Draw the frame of the selected picture-in-picture clip
    if let Some(c) = app.selected_clip.and_then(|id| app.project.clip(id)) {
        if c.transform.fit == FitMode::Custom && app.playhead >= c.start && app.playhead < c.end(app.project.fps) {
            let aspect_src = app.project.media(c.source).map(|m| m.aspect()).unwrap_or(16.0 / 9.0) as f32;
            let w = c.transform.scale * img_rect.width();
            let h = w / aspect_src;
            let r = egui::Rect::from_min_size(
                img_rect.min + Vec2::new(c.transform.x * img_rect.width(), c.transform.y * img_rect.height()),
                Vec2::new(w, h),
            );
            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.5, style::ACCENT), StrokeKind::Outside);
        }
    }
    if resp.clicked() {
        app.toggle_play();
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("⏮").clicked() {
            app.seek(0.0);
        }
        if ui.button("◀ 1 s").clicked() {
            app.seek(app.playhead - 1.0);
        }
        let play = if app.is_playing() { "⏸ Pause" } else { "▶ Play" };
        if ui.add(egui::Button::new(RichText::new(play).strong()).min_size(Vec2::new(100.0, 0.0))).clicked() {
            app.toggle_play();
        }
        if ui.button("1 s ▶").clicked() {
            app.seek(app.playhead + 1.0);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            egui::ComboBox::from_id_salt("pq")
                .selected_text(format!("{}p", app.settings.preview_height))
                .width(70.0)
                .show_ui(ui, |ui| {
                    for h in [270, 360, 540, 720, 1080] {
                        ui.selectable_value(&mut app.settings.preview_height, h, format!("{h}p"));
                    }
                });
            ui.label(RichText::new("Quality").small());
        });
    });
}

/// Audio mode: player with a large time display and the sentence currently being spoken.
fn audio_player(app: &mut App, ui: &mut egui::Ui) {
    ui.heading("🎧 Player");
    ui.add_space(10.0);
    let duration = app.project.duration();
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(fmt_time(app.playhead)).monospace().size(40.0).strong());
        ui.label(RichText::new(format!("of {}", fmt_time(duration))).monospace().weak());
    });
    ui.add_space(8.0);

    // Progress bar for seeking
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::click_and_drag());
    ui.painter().rect_filled(rect, 4.0, Color32::from_gray(50));
    if duration > 0.0 {
        let frac = (app.playhead / duration).clamp(0.0, 1.0) as f32;
        let done = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width() * frac, rect.height()));
        ui.painter().rect_filled(done, 4.0, style::ACCENT);
        if let Some(p) = resp.interact_pointer_pos().filter(|_| resp.clicked() || resp.drag_stopped()) {
            app.seek(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64 * duration);
        }
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        if ui.button("⏮").clicked() {
            app.seek(0.0);
        }
        if ui.button("◀ 5 s").clicked() {
            app.seek(app.playhead - 5.0);
        }
        let play = if app.is_playing() { "⏸ Pause" } else { "▶ Play" };
        if ui.add(egui::Button::new(RichText::new(play).strong().size(16.0)).min_size(Vec2::new(120.0, 30.0))).clicked() {
            app.toggle_play();
        }
        if ui.button("5 s ▶").clicked() {
            app.seek(app.playhead + 5.0);
        }
    });

    // Current sentence: words around the playhead, the spoken one highlighted.
    ui.add_space(16.0);
    ui.separator();
    ui.label(RichText::new("Now speaking").small().weak());
    let fps = app.project.fps;
    let main = app.project.main_track();
    let clip = app.project.clips_on(main).into_iter().find(|c| app.playhead >= c.start && app.playhead < c.end(fps)).cloned();
    let Some(clip) = clip else {
        ui.label(RichText::new("—").weak());
        return;
    };
    let src = clip.local_to_source(app.playhead - clip.start, fps);
    let words: Vec<_> = app
        .project
        .clip_tokens(&clip)
        .into_iter()
        .filter(|t| t.kind == crate::project::TokenKind::Word && !clip.is_token_deleted(t, fps))
        .collect();
    if words.is_empty() {
        ui.label(RichText::new("No transcript").weak());
        return;
    }
    let cur = words.iter().rposition(|w| w.range.start <= src).unwrap_or(0);
    let from = cur.saturating_sub(8);
    let to = (cur + 12).min(words.len());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        for (i, w) in words[from..to].iter().enumerate() {
            let text = RichText::new(&w.text).size(20.0);
            let text = if from + i == cur {
                text.background_color(style::CURRENT).color(Color32::WHITE)
            } else if from + i < cur {
                text.weak()
            } else {
                text
            };
            ui.label(text);
        }
    });
}
