//! Timeline mit Spuren, Clips, Wellenform, Schnittmarken und Abspielkopf.

use super::style;
use crate::app::{App, Focus};
use crate::project::{Clip, FadeCurve, FitMode, Id, PipPreset, TrackKind, TransitionStyle, snap};
use crate::util::fmt_time;
use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

const HEADER_W: f32 = 70.0;
const RULER_H: f32 = 22.0;
const EDGE: f32 = 7.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Move,
    TrimIn,
    TrimOut,
}

struct ClipDrag {
    clip: Id,
    mode: DragMode,
    orig: Clip,
    grab_t: f64,
}

pub struct TimelineUi {
    /// Pixel pro Sekunde.
    pub pps: f32,
    /// Sichtbarer Anfang in Sekunden.
    pub scroll: f64,
    pub snapping: bool,
    drag: Option<ClipDrag>,
    scrubbing: bool,
}

impl Default for TimelineUi {
    fn default() -> Self {
        TimelineUi { pps: 40.0, scroll: 0.0, snapping: true, drag: None, scrubbing: false }
    }
}

impl TimelineUi {
    pub fn zoom(&mut self, f: f32) {
        self.pps = (self.pps * f).clamp(2.0, 600.0);
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    toolbar(app, ui);
    ui.add_space(2.0);
    tracks(app, ui);
}

fn toolbar(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        if ui.button("⏮").on_hover_text("Zum Anfang (Pos1)").clicked() {
            app.seek(0.0);
        }
        let play = if app.is_playing() { "⏸" } else { "▶" };
        if ui.add(egui::Button::new(egui::RichText::new(play).size(16.0)).min_size(Vec2::new(36.0, 0.0))).on_hover_text("Abspielen/Pause (Leertaste)").clicked() {
            app.toggle_play();
        }
        if ui.button("⏭").on_hover_text("Zum Ende (Ende)").clicked() {
            app.seek(app.project.duration());
        }
        ui.label(
            egui::RichText::new(format!("{} / {}", fmt_time(app.playhead), fmt_time(app.project.duration())))
                .monospace()
                .size(14.0),
        );
        ui.separator();
        if ui.button("✂ Teilen").on_hover_text("Clip am Abspielkopf teilen (S)").clicked() {
            app.split_at_playhead();
        }
        let has_sel = app.selected_clip.is_some();
        if ui.add_enabled(has_sel, egui::Button::new("🗑 Löschen")).on_hover_text("Ausgewählten Clip löschen (Entf)").clicked() {
            app.delete_selected_clip();
        }
        if ui.add_enabled(has_sel, egui::Button::new("📋 Duplizieren")).on_hover_text("Cmd/Strg+D").clicked() {
            app.duplicate_selected_clip();
        }
        if ui.add_enabled(has_sel, egui::Button::new("🔇 Stille (Clip)")).on_hover_text("Stille nur aus diesem Clip entfernen").clicked() {
            if let Some(id) = app.selected_clip {
                app.remove_silence(vec![id]);
            }
        }
        ui.separator();
        if ui.button("⟲").on_hover_text("Rückgängig (Cmd/Strg+Z)").clicked() {
            app.undo();
        }
        if ui.button("⟳").on_hover_text("Wiederholen (Cmd/Strg+Shift+Z)").clicked() {
            app.redo();
        }
        ui.separator();
        let ct = app.project.clip_transition;
        let label = if ct.video || ct.audio { format!("🎞 Übergänge: {:.0} ms", ct.ms) } else { "🎞 Übergänge: aus".into() };
        ui.menu_button(label, |ui| clip_transition_settings(app, ui))
            .response
            .on_hover_text("Weiche Übergänge, wo zwei Clips auf einer Spur aneinanderstoßen");
        ui.separator();
        if !app.project.is_audio() && ui.button("+ Videospur").clicked() {
            app.checkpoint();
            app.project.add_track(TrackKind::Video);
        }
        if ui.button("+ Audiospur").clicked() {
            app.checkpoint();
            app.project.add_track(TrackKind::Audio);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("⬌ Einpassen").clicked() {
                let w = (ui.available_width() + 400.0).max(200.0);
                let d = app.project.duration().max(1.0);
                app.timeline_ui.pps = ((w - HEADER_W) as f64 / (d * 1.05)) as f32;
                app.timeline_ui.scroll = 0.0;
            }
            ui.add(egui::Slider::new(&mut app.timeline_ui.pps, 2.0..=600.0).logarithmic(true).show_value(false).text("Zoom"));
            ui.checkbox(&mut app.timeline_ui.snapping, "Magnet");
        });
    });
}

struct Row {
    track: Id,
    kind: TrackKind,
    rect: Rect,
}

fn tracks(app: &mut App, ui: &mut egui::Ui) {
    let avail = ui.available_size();
    let (rect, bg_resp) = ui.allocate_exact_size(avail, Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let fps = app.project.fps;
    let main = app.project.main_track();

    // Zeilenlayout: Videospuren von oben (höchste) nach unten (V1), dann Audiospuren.
    let mut order: Vec<(Id, TrackKind)> =
        app.project.video_tracks().iter().rev().map(|t| (t.id, t.kind)).collect();
    order.extend(app.project.audio_tracks().iter().map(|t| (t.id, t.kind)));
    let n = order.len().max(1) as f32;
    let track_h = ((rect.height() - RULER_H - 4.0) / n).clamp(26.0, 64.0);
    let rows: Vec<Row> = order
        .iter()
        .enumerate()
        .map(|(i, (id, kind))| Row {
            track: *id,
            kind: *kind,
            rect: Rect::from_min_size(
                Pos2::new(rect.left(), rect.top() + RULER_H + i as f32 * track_h),
                Vec2::new(rect.width(), track_h - 2.0),
            ),
        })
        .collect();
    let lane_left = rect.left() + HEADER_W;
    let tl = &mut app.timeline_ui;

    // Scrollen & Zoomen
    if bg_resp.hovered() || ui.rect_contains_pointer(rect) {
        let (delta, zoom, ptr) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        if (zoom - 1.0).abs() > 1e-3 {
            if let Some(p) = ptr {
                let t_ptr = tl.scroll + ((p.x - lane_left) / tl.pps) as f64;
                tl.zoom(zoom);
                tl.scroll = (t_ptr - ((p.x - lane_left) / tl.pps) as f64).max(0.0);
            }
        } else if delta != Vec2::ZERO {
            let d = if delta.x.abs() > delta.y.abs() { delta.x } else { delta.y };
            tl.scroll = (tl.scroll - (d / tl.pps) as f64).max(0.0);
        }
    }
    let pps = tl.pps;
    let scroll = tl.scroll;
    let x_of = |t: f64| lane_left + ((t - scroll) as f32) * pps;
    let t_of = |x: f32| scroll + ((x - lane_left) / pps) as f64;

    // Hintergrund
    painter.rect_filled(rect, 0.0, Color32::from_gray(22));
    for (i, row) in rows.iter().enumerate() {
        let shade = if i % 2 == 0 { 30 } else { 34 };
        painter.rect_filled(row.rect, 0.0, Color32::from_gray(shade));
    }

    // Lineal
    let ruler = Rect::from_min_size(Pos2::new(lane_left, rect.top()), Vec2::new(rect.width() - HEADER_W, RULER_H));
    painter.rect_filled(ruler, 0.0, Color32::from_gray(40));
    let step = [0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0]
        .into_iter()
        .find(|s| *s as f32 * pps >= 80.0)
        .unwrap_or(1200.0);
    let t_end = t_of(rect.right());
    let mut t = (scroll / step).floor() * step;
    while t <= t_end {
        let x = x_of(t);
        if x >= lane_left {
            painter.line_segment([Pos2::new(x, ruler.top() + 10.0), Pos2::new(x, rect.bottom())], Stroke::new(1.0, Color32::from_gray(45)));
            painter.text(Pos2::new(x + 3.0, ruler.top() + 2.0), Align2::LEFT_TOP, fmt_time(t), FontId::monospace(10.0), Color32::from_gray(170));
        }
        for k in 1..5 {
            let xm = x_of(t + step * k as f64 / 5.0);
            if xm >= lane_left {
                painter.line_segment([Pos2::new(xm, ruler.bottom() - 5.0), Pos2::new(xm, ruler.bottom())], Stroke::new(1.0, Color32::from_gray(90)));
            }
        }
        t += step;
    }

    // Lineal: Klicken/Ziehen setzt den Abspielkopf
    let ruler_resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click_and_drag());
    if ruler_resp.is_pointer_button_down_on() {
        if let Some(p) = ruler_resp.interact_pointer_pos() {
            app.timeline_ui.scrubbing = true;
            let t = t_of(p.x).max(0.0);
            if app.is_playing() {
                app.stop_playback();
            }
            app.playhead = snap(t.min(app.project.duration()), fps);
        }
    } else {
        app.timeline_ui.scrubbing = false;
    }

    // Klick auf leere Fläche: Abspielkopf setzen, Auswahl aufheben
    if bg_resp.clicked() {
        if let Some(p) = bg_resp.interact_pointer_pos() {
            if p.x > lane_left {
                app.seek(t_of(p.x));
            }
            app.selected_clip = None;
            app.focus = Focus::Timeline;
        }
    }

    // Spurköpfe
    for row in &rows {
        let head = Rect::from_min_size(row.rect.min, Vec2::new(HEADER_W - 4.0, row.rect.height()));
        painter.rect_filled(head, 0.0, Color32::from_gray(44));
        let Some(track) = app.project.track(row.track).cloned() else { continue };
        let is_main = row.track == main;
        painter.text(
            head.left_center() + Vec2::new(6.0, -7.0),
            Align2::LEFT_CENTER,
            &track.name,
            FontId::proportional(13.0),
            style::track_color(is_main, Some(row.kind)),
        );
        if is_main {
            painter.text(head.left_center() + Vec2::new(6.0, 8.0), Align2::LEFT_CENTER, "Haupt", FontId::proportional(9.0), Color32::GRAY);
        }
        // Umschalter: Ausblenden (Video) / Stumm (Audio)
        let (icon, active) = match row.kind {
            TrackKind::Video => (if track.hidden { "🚫" } else { "👁" }, track.hidden),
            TrackKind::Audio => (if track.muted { "🔇" } else { "🔊" }, track.muted),
        };
        let btn = Rect::from_center_size(head.right_center() - Vec2::new(14.0, 0.0), Vec2::splat(20.0));
        let r = ui.interact(btn, ui.id().with(("trk", row.track)), Sense::click());
        painter.text(btn.center(), Align2::CENTER_CENTER, icon, FontId::proportional(13.0), if active { style::CUT } else { Color32::LIGHT_GRAY });
        if r.clicked() {
            app.checkpoint();
            if let Some(t) = app.project.track_mut(row.track) {
                match row.kind {
                    TrackKind::Video => t.hidden = !t.hidden,
                    TrackKind::Audio => t.muted = !t.muted,
                }
            }
        }
        let head_resp = ui.interact(head, ui.id().with(("head", row.track)), Sense::click());
        head_resp.context_menu(|ui| {
            let mut extra = false;
            if row.kind == TrackKind::Video {
                if ui.button(if track.muted { "🔊 Ton einschalten" } else { "🔇 Ton der Spur stumm" }).clicked() {
                    app.checkpoint();
                    if let Some(t) = app.project.track_mut(row.track) {
                        t.muted = !t.muted;
                    }
                    ui.close_menu();
                }
                extra = true;
            }
            if extra {
                ui.separator();
            }
            if ui.add_enabled(!is_main, egui::Button::new("🗑 Spur entfernen")).clicked() {
                app.checkpoint();
                if !app.project.remove_track(row.track) {
                    app.drop_checkpoint();
                }
                ui.close_menu();
            }
        });
    }

    // Clips
    let lane_clip = Rect::from_min_max(Pos2::new(lane_left, rect.top()), rect.max);
    let clip_painter = painter.with_clip_rect(lane_clip);
    let dragging_id = app.timeline_ui.drag.as_ref().map(|d| d.clip);
    let mut clicked_clip: Option<Id> = None;
    let mut drag_start: Option<(Id, DragMode, f64)> = None;
    let mut drag_active = false;
    let mut drag_released = false;
    let mut ctx_action: Option<(Id, ClipAction)> = None;
    let pointer = ui.input(|i| i.pointer.interact_pos());

    let mut draw_order: Vec<(Id, Rect)> = Vec::new();
    for row in &rows {
        for clip in app.project.clips_on(row.track) {
            let x0 = x_of(clip.start);
            let x1 = x_of(clip.end(fps));
            if x1 < lane_left || x0 > rect.right() {
                continue;
            }
            let r = Rect::from_min_max(Pos2::new(x0, row.rect.top() + 2.0), Pos2::new(x1.max(x0 + 3.0), row.rect.bottom() - 2.0));
            draw_order.push((clip.id, r));
        }
    }
    // Gezogener Clip zuletzt (oben)
    draw_order.sort_by_key(|(id, _)| Some(*id) == dragging_id);

    for (clip_id, r) in &draw_order {
        let clip_id = *clip_id;
        let Some(clip) = app.project.clip(clip_id) else { continue };
        let media = app.project.media(clip.source);
        let is_main = clip.track == main;
        let kind = app.project.track(clip.track).map(|t| t.kind);
        let selected = app.selected_clip == Some(clip_id);
        let base = style::track_color(is_main, kind);
        let fill = if selected { base.gamma_multiply(1.25) } else { base.gamma_multiply(0.85) };
        clip_painter.rect_filled(*r, 4.0, fill);
        if selected {
            clip_painter.rect_stroke(*r, 4.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Inside);
        }

        // Wellenform
        if let Some(m) = media.filter(|m| m.waveform.is_some()) {
            let kept = clip.kept(fps);
            let mid_y = r.bottom() - r.height() * 0.28;
            let amp = r.height() * 0.26;
            let mut seg = 0usize;
            let mut acc = 0.0f64;
            let x_start = r.left().max(lane_left);
            let x_end = r.right().min(rect.right());
            let mut x = x_start;
            while x < x_end {
                let local = ((x - r.left()) / pps) as f64;
                while seg < kept.len() && local >= acc + kept[seg].len() {
                    acc += kept[seg].len();
                    seg += 1;
                }
                if seg >= kept.len() {
                    break;
                }
                let src = kept[seg].start + (local - acc);
                let p = m.peak_at(src) * amp;
                clip_painter.line_segment([Pos2::new(x, mid_y - p), Pos2::new(x, mid_y + p)], Stroke::new(1.0, Color32::from_white_alpha(90)));
                x += 2.0;
            }
        }

        // Schnittmarken an den Übergängen behaltener Stücke
        let segs = clip.segments(fps);
        for (ts, _) in segs.iter().skip(1) {
            let x = x_of(*ts);
            clip_painter.line_segment([Pos2::new(x, r.top()), Pos2::new(x, r.top() + r.height() * 0.45)], Stroke::new(1.5, style::CUT));
        }

        // Titel
        let name = media.map(|m| m.name.as_str()).unwrap_or("?");
        let mut label = name.to_string();
        if clip.transform.fit == FitMode::Custom && kind == Some(TrackKind::Video) {
            label = format!("🖼 {label}");
        }
        if clip.muted {
            label = format!("🔇 {label}");
        }
        let text_pos = Pos2::new(r.left().max(lane_left) + 5.0, r.top() + 3.0);
        let text_painter = clip_painter.with_clip_rect(r.intersect(lane_clip));
        text_painter.text(text_pos, Align2::LEFT_TOP, label, FontId::proportional(12.0), Color32::WHITE);
        if segs.len() > 1 && r.width() > 140.0 {
            text_painter.text(
                Pos2::new(r.right() - 5.0, r.top() + 3.0),
                Align2::RIGHT_TOP,
                format!("✂{}", segs.len() - 1),
                FontId::proportional(10.0),
                Color32::from_white_alpha(200),
            );
        }

        // Interaktion: Körper (verschieben) und Ränder (trimmen)
        let body = ui.interact(*r, ui.id().with(("clip", clip_id)), Sense::click_and_drag());
        let left = Rect::from_min_max(r.min, Pos2::new(r.left() + EDGE, r.bottom()));
        let right = Rect::from_min_max(Pos2::new(r.right() - EDGE, r.top()), r.max);
        let l_resp = ui.interact(left, ui.id().with(("clip_l", clip_id)), Sense::drag());
        let r_resp = ui.interact(right, ui.id().with(("clip_r", clip_id)), Sense::drag());
        if l_resp.hovered() || r_resp.hovered() || l_resp.dragged() || r_resp.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        } else if body.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }

        let grab_t = pointer.map(|p| t_of(p.x)).unwrap_or(0.0);
        if l_resp.drag_started() {
            drag_start = Some((clip_id, DragMode::TrimIn, grab_t));
        } else if r_resp.drag_started() {
            drag_start = Some((clip_id, DragMode::TrimOut, grab_t));
        } else if body.drag_started() {
            drag_start = Some((clip_id, DragMode::Move, grab_t));
        }
        if body.dragged() || l_resp.dragged() || r_resp.dragged() {
            drag_active = true;
        }
        if body.drag_stopped() || l_resp.drag_stopped() || r_resp.drag_stopped() {
            drag_released = true;
        }
        if body.clicked() {
            clicked_clip = Some(clip_id);
        }
        if body.double_clicked() {
            app.seek(clip.start);
        }
        body.context_menu(|ui| {
            for (label, action) in ClipAction::menu(is_main, kind == Some(TrackKind::Video)) {
                if ui.button(label).clicked() {
                    ctx_action = Some((clip_id, action));
                    ui.close_menu();
                }
            }
        });
    }

    // Übergänge zwischen Clips markieren (Fliege über der Schnittstelle).
    for (clip_id, r) in &draw_order {
        let Some(clip) = app.project.clip(*clip_id) else { continue };
        let e = app.project.clip_edges(clip).start;
        if !e.is_some() {
            continue;
        }
        let x = x_of(clip.start);
        // Überblenden: ±Handle um die Schnittstelle; über Schwarz: je eine Blende davor/danach.
        let half = if e.extend > 0.0 { e.extend } else { e.audio_fade.max(e.video_fade) };
        let w = (half as f32 * pps).max(4.0);
        let (top, bot) = (r.top() + 1.0, r.bottom() - 1.0);
        let col = Color32::from_white_alpha(70);
        let shape = |a: Pos2, b: Pos2, c: Pos2| egui::Shape::convex_polygon(vec![a, b, c], col, Stroke::new(1.0, Color32::from_white_alpha(160)));
        clip_painter.add(shape(Pos2::new(x - w, top), Pos2::new(x, (top + bot) / 2.0), Pos2::new(x - w, bot)));
        clip_painter.add(shape(Pos2::new(x + w, top), Pos2::new(x + w, bot), Pos2::new(x, (top + bot) / 2.0)));
    }

    if let Some(id) = clicked_clip {
        app.selected_clip = Some(id);
        app.focus = Focus::Timeline;
        app.transcript_ui.scroll_to = Some(id);
    }
    if let Some((id, action)) = ctx_action {
        app.selected_clip = Some(id);
        action.run(app, id);
    }

    // Drag-Logik
    if let Some((id, mode, grab_t)) = drag_start {
        if let Some(orig) = app.project.clip(id).cloned() {
            app.checkpoint();
            app.selected_clip = Some(id);
            app.focus = Focus::Timeline;
            app.timeline_ui.drag = Some(ClipDrag { clip: id, mode, orig, grab_t });
        }
    }
    if drag_active {
        if let (Some(p), Some(drag)) = (pointer, &app.timeline_ui.drag) {
            let dt = t_of(p.x) - drag.grab_t;
            let target_row = rows.iter().find(|r| p.y >= r.rect.top() && p.y < r.rect.bottom()).map(|r| (r.track, r.kind));
            apply_drag(app, dt, target_row, pps);
        }
    }
    if drag_released {
        if let Some(drag) = app.timeline_ui.drag.take() {
            app.project.repack_main();
            if let Some(c) = app.project.clip(drag.clip) {
                let unchanged = c.start == drag.orig.start
                    && c.src_in == drag.orig.src_in
                    && c.src_out == drag.orig.src_out
                    && c.track == drag.orig.track;
                if unchanged {
                    app.drop_checkpoint();
                }
            }
        }
    }

    // Abspielkopf
    let px = x_of(app.playhead);
    if px >= lane_left && px <= rect.right() {
        painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(2.0, style::PLAYHEAD));
        let tri = vec![Pos2::new(px - 6.0, rect.top()), Pos2::new(px + 6.0, rect.top()), Pos2::new(px, rect.top() + 9.0)];
        painter.add(egui::Shape::convex_polygon(tri, style::PLAYHEAD, Stroke::NONE));
    }

    // Abspielkopf im Blick halten
    if app.is_playing() {
        let tl = &mut app.timeline_ui;
        let visible = ((rect.right() - lane_left) / tl.pps) as f64;
        if app.playhead > tl.scroll + visible * 0.9 || app.playhead < tl.scroll {
            tl.scroll = (app.playhead - visible * 0.1).max(0.0);
        }
    }
}

/// Wendet eine Maus-Verschiebung `dt` auf den gezogenen Clip an.
fn apply_drag(app: &mut App, dt: f64, target_row: Option<(Id, TrackKind)>, pps: f32) {
    let Some(drag) = &app.timeline_ui.drag else { return };
    let (id, mode, orig) = (drag.clip, drag.mode, drag.orig.clone());
    let fps = app.project.fps;
    let frame = app.project.frame();
    let main = app.project.main_track();
    let snapping = app.timeline_ui.snapping;
    let src_dur = app.project.media(orig.source).map(|m| m.duration).unwrap_or(orig.src_out);
    let threshold = 8.0 / pps as f64;

    // Fangpunkte: Abspielkopf, 0, Kanten aller anderen Clips
    let mut points = vec![0.0, app.playhead];
    for c in &app.project.clips {
        if c.id != id {
            points.push(c.start);
            points.push(c.end(fps));
        }
    }
    let snap_to = |t: f64| -> Option<f64> {
        points.iter().copied().filter(|p| (p - t).abs() < threshold).min_by(|a, b| (a - t).abs().total_cmp(&(b - t).abs()))
    };

    let mut clip = orig.clone();
    match mode {
        DragMode::Move => {
            let dur = orig.duration(fps);
            let mut start = (orig.start + dt).max(0.0);
            if snapping {
                if let Some(s) = snap_to(start) {
                    start = s;
                } else if let Some(e) = snap_to(start + dur) {
                    start = (e - dur).max(0.0);
                }
            }
            clip.start = snap(start, fps);
            if let Some((track, kind)) = target_row {
                let orig_kind = app.project.track(orig.track).map(|t| t.kind);
                if Some(kind) == orig_kind && track != clip.track {
                    // Spurwechsel: Hauptspur = Vollbild, andere Videospuren = Bild-in-Bild.
                    if kind == TrackKind::Video {
                        if track == main {
                            clip.transform.fit = FitMode::Fit;
                        } else if clip.transform.fit != FitMode::Custom {
                            clip.transform.scale = 0.3;
                            clip.transform.border = 4.0;
                            clip.transform.fit = FitMode::Custom;
                        }
                    }
                    clip.track = track;
                }
            }
        }
        DragMode::TrimIn => {
            let mut new_in = (orig.src_in + dt).clamp(0.0, orig.src_out - frame);
            if snapping && orig.track != main {
                if let Some(s) = snap_to(orig.start + (new_in - orig.src_in)) {
                    new_in = (orig.src_in + (s - orig.start)).clamp(0.0, orig.src_out - frame);
                }
            }
            let new_in = snap(new_in, fps);
            clip.src_in = new_in;
            if orig.track != main {
                clip.start = snap((orig.start + (new_in - orig.src_in)).max(0.0), fps);
            }
        }
        DragMode::TrimOut => {
            let mut new_out = (orig.src_out + dt).clamp(orig.src_in + frame, src_dur);
            if snapping {
                let end = orig.start + orig.duration(fps) + (new_out - orig.src_out);
                if let Some(s) = snap_to(end) {
                    new_out = (orig.src_out + (s - orig.end(fps))).clamp(orig.src_in + frame, src_dur);
                }
            }
            clip.src_out = snap(new_out, fps);
        }
    }
    let became_pip = clip.transform.fit == FitMode::Custom && orig.transform.fit != FitMode::Custom;
    if let Some(c) = app.project.clip_mut(id) {
        *c = clip;
    }
    if became_pip {
        app.project.place_pip(id, PipPreset::BottomRight);
    }
    // Hauptspur beim Trimmen sofort nachziehen; beim Verschieben erst nach dem Loslassen.
    if mode != DragMode::Move {
        app.project.repack_main();
    }
}

#[derive(Clone, Copy)]
enum ClipAction {
    Split,
    Duplicate,
    Delete,
    RemoveSilence,
    RestoreSilence,
    ResetCuts,
    Transcribe,
    ToPip,
    ToMain,
    Mute,
}

impl ClipAction {
    fn menu(is_main: bool, is_video: bool) -> Vec<(&'static str, ClipAction)> {
        let mut v = vec![
            ("✂ Am Abspielkopf teilen", ClipAction::Split),
            ("📋 Duplizieren", ClipAction::Duplicate),
            ("🔇 Stille entfernen", ClipAction::RemoveSilence),
            ("↺ Stille-Schnitte zurücksetzen", ClipAction::RestoreSilence),
            ("↺ Alle Schnitte zurücksetzen", ClipAction::ResetCuts),
            ("📝 Transkribieren", ClipAction::Transcribe),
            ("🔈 Stumm an/aus", ClipAction::Mute),
        ];
        if is_video {
            if is_main {
                v.push(("🖼 Als Bild-in-Bild auf V2", ClipAction::ToPip));
            } else {
                v.push(("⬇ Auf Hauptspur verschieben", ClipAction::ToMain));
            }
        }
        v.push(("🗑 Löschen", ClipAction::Delete));
        v
    }

    fn run(self, app: &mut App, id: Id) {
        match self {
            ClipAction::Split => app.split_at_playhead(),
            ClipAction::Duplicate => app.duplicate_selected_clip(),
            ClipAction::Delete => app.delete_selected_clip(),
            ClipAction::RemoveSilence => app.remove_silence(vec![id]),
            ClipAction::RestoreSilence => app.restore_silence(vec![id]),
            ClipAction::ResetCuts => {
                app.checkpoint();
                if let Some(c) = app.project.clip_mut(id) {
                    c.clear_cuts(None);
                }
                app.project.repack_main();
            }
            ClipAction::Transcribe => {
                if let Some(c) = app.project.clip(id) {
                    let src = c.source;
                    app.start_transcription(src);
                }
            }
            ClipAction::Mute => {
                app.checkpoint();
                if let Some(c) = app.project.clip_mut(id) {
                    c.muted = !c.muted;
                }
            }
            ClipAction::ToPip => {
                let target = match app.project.video_tracks().get(1) {
                    Some(t) => t.id,
                    None => app.project.add_track(TrackKind::Video),
                };
                app.checkpoint();
                let start = app.playhead;
                if let Some(c) = app.project.clip_mut(id) {
                    c.track = target;
                    c.start = start;
                    c.transform.fit = FitMode::Custom;
                    c.transform.scale = 0.3;
                    c.transform.border = 4.0;
                }
                app.project.place_pip(id, PipPreset::BottomRight);
                app.project.repack_main();
            }
            ClipAction::ToMain => {
                app.checkpoint();
                let main = app.project.main_track();
                let end = app.project.track_end(main);
                if let Some(c) = app.project.clip_mut(id) {
                    c.track = main;
                    c.start = end;
                    c.transform.fit = FitMode::Fit;
                }
                app.project.repack_main();
            }
        }
    }
}

/// Einstellungen für Übergänge zwischen Clips (mit Vorschau des Verlaufs).
pub fn clip_transition_settings(app: &mut App, ui: &mut egui::Ui) {
    ui.set_min_width(320.0);
    let before = app.project.clip_transition;
    let mut t = before;
    ui.label(egui::RichText::new("Übergänge zwischen Clips").strong());
    ui.label(
        egui::RichText::new(
            "Gilt überall, wo zwei Clips auf derselben Spur direkt aneinanderstoßen\n(z.B. nach „Teilen“ oder auf der Hauptspur). Die Timeline-Länge bleibt gleich.",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label("Aktiv für:");
        if !app.project.is_audio() {
            ui.checkbox(&mut t.video, "Videospuren (Bild + Ton)");
        }
        ui.checkbox(&mut t.audio, "Audiospuren");
    });
    let active = t.video || t.audio;
    ui.add_enabled_ui(active, |ui| {
        ui.horizontal(|ui| {
            ui.label("Art:");
            for s in [TransitionStyle::Dissolve, TransitionStyle::DipToBlack] {
                ui.selectable_value(&mut t.style, s, s.label());
            }
        });
        ui.add(egui::Slider::new(&mut t.ms, 50.0..=4000.0).logarithmic(true).suffix(" ms").text("Länge"));
        ui.horizontal(|ui| {
            for ms in [250.0, 500.0, 1000.0, 2000.0] {
                if ui.small_button(format!("{ms:.0}")).clicked() {
                    t.ms = ms;
                }
            }
        });
        egui::ComboBox::from_label("Kurve (Ton)").selected_text(t.curve.label()).width(220.0).show_ui(ui, |ui| {
            for c in FadeCurve::ALL {
                ui.selectable_value(&mut t.curve, c, c.label());
            }
        });
        transition_preview(ui, &t);
        let hint = match t.style {
            TransitionStyle::Dissolve => "Nutzt Material vor/nach den Clip-Grenzen. Fehlt es (Dateianfang/-ende), wird über Schwarz geblendet.",
            TransitionStyle::DipToBlack => "Clip A blendet aus, dann blendet Clip B ein.",
        };
        ui.label(egui::RichText::new(hint).small().weak());
    });

    if t != before {
        if !app.inspector_editing {
            let mut snapshot = app.project.clone();
            snapshot.clip_transition = before;
            app.push_undo(snapshot);
            app.inspector_editing = true;
        } else {
            app.touch();
        }
        app.project.clip_transition = t;
    }
}

/// Zeichnet Lautstärke-/Deckkraftverlauf von Clip A (aus) und B (ein).
fn transition_preview(ui: &mut egui::Ui, t: &crate::project::ClipTransition) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(300.0), 70.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, Color32::from_gray(30));
    let plot = rect.shrink2(Vec2::new(8.0, 10.0));
    let d = t.ms as f64 / 1000.0;
    let span = d;
    let x_of = |s: f64| plot.left() + ((s + span) / (2.0 * span)) as f32 * plot.width();
    let y_of = |g: f64| plot.bottom() - g as f32 * plot.height();
    let (gain_a, gain_b): (Box<dyn Fn(f64) -> f64>, Box<dyn Fn(f64) -> f64>) = match t.style {
        TransitionStyle::Dissolve => {
            let c = t.curve;
            (
                Box::new(move |s: f64| c.eval(((d / 2.0 - s) / d).clamp(0.0, 1.0))),
                Box::new(move |s: f64| c.eval(((s + d / 2.0) / d).clamp(0.0, 1.0))),
            )
        }
        TransitionStyle::DipToBlack => {
            let c = t.curve;
            let h = d / 2.0;
            (
                Box::new(move |s: f64| if s >= 0.0 { 0.0 } else { c.eval((-s / h).clamp(0.0, 1.0)) }),
                Box::new(move |s: f64| if s < 0.0 { 0.0 } else { c.eval((s / h).clamp(0.0, 1.0)) }),
            )
        }
    };
    for (f, col) in [(&gain_a, style::MAIN_CLIP), (&gain_b, style::ACCENT)] {
        let pts: Vec<Pos2> = (0..=120)
            .map(|i| {
                let s = -span + 2.0 * span * i as f64 / 120.0;
                Pos2::new(x_of(s), y_of(f(s)))
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, col)));
    }
    p.line_segment([Pos2::new(x_of(0.0), rect.top()), Pos2::new(x_of(0.0), rect.bottom())], Stroke::new(1.0, Color32::GRAY));
    let font = FontId::proportional(10.0);
    p.text(rect.left_top() + Vec2::new(4.0, 2.0), Align2::LEFT_TOP, "Clip A", font.clone(), style::MAIN_CLIP);
    p.text(rect.right_top() + Vec2::new(-4.0, 2.0), Align2::RIGHT_TOP, "Clip B", font.clone(), style::ACCENT);
    p.text(Pos2::new(x_of(0.0) + 3.0, rect.bottom() - 2.0), Align2::LEFT_BOTTOM, "Schnitt", font, Color32::GRAY);
}
