//! Transkript-Editor: Jeder Clip wird als Text dargestellt. Markierte Wörter
//! löschen schneidet das Video; Pausen erscheinen als ⏸-Marken.

use super::style;
use crate::app::{App, Focus, Sel, TaskKind};
use crate::project::{FadeCurve, Id, TokenKind};
use crate::util::fmt_time;
use eframe::egui::{self, Align, Color32, FontId, RichText, Sense, Stroke, text::LayoutJob, TextFormat};

pub struct TranscriptUi {
    pub show_deleted: bool,
    pub follow: bool,
    pub search: String,
    /// Clip, zu dem beim nächsten Zeichnen gescrollt wird.
    pub scroll_to: Option<Id>,
    last_follow: Option<(Id, usize)>,
    press: Option<(Id, usize, usize)>,
}

impl Default for TranscriptUi {
    fn default() -> Self {
        TranscriptUi { show_deleted: true, follow: true, search: String::new(), scroll_to: None, last_follow: None, press: None }
    }
}

/// Ein klickbarer Textabschnitt: Zeichenbereich → Token-Bereich.
struct Span {
    chars: (usize, usize),
    first: usize,
    last: usize,
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    toolbar(app, ui);
    ui.separator();

    let doc = app.project.clips_in_document_order();
    if doc.is_empty() {
        empty_state(app, ui);
        return;
    }

    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(4.0);
        for clip_id in doc {
            clip_block(app, ui, clip_id);
            ui.add_space(14.0);
        }
        ui.add_space(40.0);
    });
}

fn toolbar(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.heading(RichText::new("📝 Text-Schnitt").strong());
        ui.add_space(8.0);
        let silence_busy = app.tasks.iter().any(|t| matches!(t.kind, TaskKind::Silence(_)));
        let btn = egui::Button::new(RichText::new("🔇 Stille automatisch entfernen").strong()).fill(style::ACCENT_DIM);
        if ui
            .add_enabled(!silence_busy, btn)
            .on_hover_text("Erkennt Pausen im Ton und schneidet sie aus allen Clips heraus.\nSchwelle/Mindestdauer/Rand in den Einstellungen.")
            .clicked()
        {
            app.remove_silence_all();
        }
        if ui.button("↺ Stille zurück").on_hover_text("Alle automatischen Stille-Schnitte rückgängig machen").clicked() {
            let ids = app.project.clips.iter().map(|c| c.id).collect();
            app.restore_silence(ids);
        }
        if ui.button("🗑 Füllwörter entfernen").on_hover_text(format!("Entfernt: {}", app.settings.fillers)).clicked() {
            app.remove_fillers_all();
        }
        let subs_label = if app.project.subtitles.burn_in { "💬 Untertitel: an" } else { "💬 Untertitel" };
        if ui.button(subs_label).on_hover_text("Text als Untertitel ins Video einbrennen").clicked() {
            app.show_subtitles = true;
        }
        let tr = app.project.transition;
        let label = if tr.enabled { format!("〰 Weiche Übergänge: {:.0} ms", tr.ms) } else { "〰 Weiche Übergänge: aus".into() };
        ui.menu_button(label, |ui| transition_settings(app, ui))
            .response
            .on_hover_text("Ton an Schnittstellen sanft aus- und einblenden statt hart schneiden");
    });
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut app.transcript_ui.search)
                .hint_text("🔍 Wort oder Phrase suchen …")
                .desired_width(200.0),
        );
        let phrase = app.transcript_ui.search.trim().to_string();
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui
            .add_enabled(!phrase.is_empty(), egui::Button::new("✂ Alle Treffer löschen"))
            .clicked()
            || (enter && !phrase.is_empty())
        {
            app.delete_phrase_all(&phrase);
        }
        ui.separator();
        ui.checkbox(&mut app.transcript_ui.show_deleted, "Gelöschtes anzeigen");
        ui.checkbox(&mut app.transcript_ui.follow, "Mitlaufen");
    });
    if let Some(sel) = &app.sel {
        let (a, b) = sel.range();
        let clip = sel.clip;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("Auswahl: {} Element(e)", b - a + 1)).color(style::ACCENT));
            if ui.button("✂ Löschen (Entf)").clicked() {
                app.set_words_deleted(clip, a, b, true);
            }
            if ui.button("↺ Wiederherstellen").clicked() {
                app.set_words_deleted(clip, a, b, false);
            }
            if ui.button("Auswahl aufheben (Esc)").clicked() {
                app.sel = None;
            }
        });
    } else {
        ui.label(
            RichText::new("Tipp: Wörter anklicken/ziehen zum Markieren, Entf zum Herausschneiden. Erneut Entf stellt wieder her. Klick springt zur Stelle. Gelb unterstrichen = unsicher (kaum Ton, evtl. von Whisper erfunden).")
                .small()
                .weak(),
        );
    }
}

fn empty_state(app: &mut App, ui: &mut egui::Ui) {
    ui.vertical_centered(|ui| {
        ui.add_space(60.0);
        ui.label(RichText::new("Noch keine Clips").size(22.0).strong());
        ui.add_space(6.0);
        let what = if app.project.is_audio() { "Audiodateien" } else { "Videos" };
        ui.label(format!(
            "Ziehe {what} hierher oder importiere sie. Sie werden automatisch transkribiert,\nund du kannst sie dann wie ein Textdokument schneiden."
        ));
        ui.add_space(12.0);
        if ui.button(RichText::new("📂 Medien importieren …").size(16.0)).clicked() {
            app.import_dialog(crate::app::Placement::Main);
        }
        if let Err(e) = app.settings.whisper_ready() {
            ui.add_space(16.0);
            ui.colored_label(style::WARN, format!("⚠ Transkription noch nicht eingerichtet: {e}"));
            if ui.button("⚙ Einstellungen öffnen").clicked() {
                app.show_settings = true;
            }
        }
    });
}

fn clip_block(app: &mut App, ui: &mut egui::Ui, clip_id: Id) {
    let fps = app.project.fps;
    let Some(clip) = app.project.clip(clip_id).cloned() else { return };
    let Some(media) = app.project.media(clip.source) else { return };
    let media_name = media.name.clone();
    let has_transcript = media.transcript.is_some();
    let has_audio = media.has_audio;
    let track = app.project.track(clip.track).map(|t| t.name.clone()).unwrap_or_default();
    let is_main = clip.track == app.project.main_track();
    let transcribing = app.task_running(TaskKind::Transcribe(clip.source));
    let selected = app.selected_clip == Some(clip_id);

    // Kopfzeile
    let header = ui.horizontal(|ui| {
        let chip_color = style::track_color(is_main, app.project.track(clip.track).map(|t| t.kind));
        ui.label(RichText::new(format!(" {track} ")).background_color(chip_color).color(Color32::WHITE).strong());
        let name = RichText::new(&media_name).strong().size(15.0);
        if ui.selectable_label(selected, name).clicked() {
            app.selected_clip = Some(clip_id);
            app.focus = Focus::Timeline;
            app.seek(clip.start);
        }
        ui.label(RichText::new(format!("{} – {}", fmt_time(clip.start), fmt_time(clip.end(fps)))).weak().small());
        let removed = clip.removed_time(fps);
        if removed > 0.05 {
            ui.label(RichText::new(format!("−{removed:.1} s geschnitten")).color(style::CUT).small());
        }
        if transcribing {
            ui.spinner();
            let p = app.tasks.iter().find(|t| t.kind == TaskKind::Transcribe(clip.source)).map(|t| t.progress).unwrap_or(0.0);
            ui.label(RichText::new(format!("Transkribiere … {:.0} %", p * 100.0)).small());
        } else if !has_transcript && has_audio {
            if ui.small_button("📝 Transkribieren").clicked() {
                app.start_transcription(clip.source);
            }
        }
    });
    if app.transcript_ui.scroll_to == Some(clip_id) {
        header.response.scroll_to_me(Some(Align::TOP));
        app.transcript_ui.scroll_to = None;
    }

    let tokens = app.project.clip_tokens(&clip);
    if tokens.is_empty() {
        let msg = if !has_audio {
            "Kein Ton – kein Transkript möglich."
        } else if transcribing {
            "Transkript wird erstellt …"
        } else {
            "Noch kein Transkript. Klicke „Transkribieren“ (whisper.cpp)."
        };
        ui.label(RichText::new(msg).italics().weak());
        return;
    }

    // Aktuelles Wort unter dem Abspielkopf
    let current_src = (app.playhead >= clip.start && app.playhead < clip.end(fps))
        .then(|| clip.local_to_source(app.playhead - clip.start, fps));
    let sel_range = app.sel.as_ref().filter(|s| s.clip == clip_id).map(|s| s.range());
    let show_deleted = app.transcript_ui.show_deleted;
    let size = app.settings.text_size;
    let visuals = ui.visuals().clone();

    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width() - 8.0;
    let mut spans: Vec<Span> = Vec::new();
    let mut chars = 0usize;
    let mut current_char: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < tokens.len() {
        let tok = &tokens[i];
        let kept = !clip.is_token_deleted(tok, fps);
        if !kept && !show_deleted {
            // Zusammenhängend gelöschte Wörter als ein ✂-Marker.
            let first = i;
            while i + 1 < tokens.len() && clip.is_token_deleted(&tokens[i + 1], fps) {
                i += 1;
            }
            let selected = sel_range.is_some_and(|(a, b)| first <= b && i >= a);
            let mut fmt = TextFormat::simple(FontId::proportional(size * 0.75), style::CUT);
            if selected {
                fmt.background = style::SELECTION;
            }
            let text = "✂";
            job.append(text, 0.0, fmt);
            job.append(" ", 0.0, TextFormat::simple(FontId::proportional(size), visuals.text_color()));
            let n = text.chars().count();
            spans.push(Span { chars: (chars, chars + n), first, last: i });
            chars += n + 1;
            i += 1;
            continue;
        }
        let selected = sel_range.is_some_and(|(a, b)| i >= a && i <= b);
        let is_current = current_src.is_some_and(|s| tok.range.contains(s)) && kept;
        let mut fmt = match tok.kind {
            TokenKind::Word => TextFormat::simple(FontId::proportional(size), visuals.text_color()),
            TokenKind::Silence => {
                let mut f = TextFormat::simple(FontId::monospace(size * 0.72), style::SILENCE_TEXT);
                f.background = style::SILENCE_BG;
                f
            }
        };
        if tok.uncertain {
            fmt.color = style::WARN;
            fmt.underline = Stroke::new(1.5, style::WARN);
        }
        if !kept {
            fmt.color = style::CUT.gamma_multiply(0.85);
            fmt.strikethrough = Stroke::new(1.5, style::CUT);
        }
        if is_current {
            fmt.background = style::CURRENT;
            fmt.color = Color32::WHITE;
        }
        if selected {
            fmt.background = style::SELECTION;
        }
        let text = tok.text.as_str();
        job.append(text, 0.0, fmt);
        let n = text.chars().count();
        if is_current {
            current_char = Some((chars, i));
        }
        let mut space = TextFormat::simple(FontId::proportional(size), visuals.text_color());
        if selected && sel_range.is_some_and(|(_, b)| i < b) {
            space.background = style::SELECTION;
        }
        job.append(" ", 0.0, space);
        spans.push(Span { chars: (chars, chars + n), first: i, last: i });
        chars += n + 1;
        i += 1;
    }

    let galley = ui.fonts(|f| f.layout_job(job));
    let (rect, resp) = ui.allocate_exact_size(galley.size() + egui::vec2(8.0, 4.0), Sense::click_and_drag());
    let origin = rect.min + egui::vec2(4.0, 2.0);
    if selected {
        ui.painter().rect_filled(rect.expand(3.0), 6.0, style::BLOCK_SELECTED);
    }
    ui.painter().galley(origin, galley.clone(), visuals.text_color());

    // Mitlaufen während der Wiedergabe
    if let Some((c, tok)) = current_char {
        if app.transcript_ui.follow && app.is_playing() && app.transcript_ui.last_follow != Some((clip_id, tok)) {
            let r = galley.pos_from_ccursor(egui::text::CCursor::new(c)).translate(origin.to_vec2());
            ui.scroll_to_rect(r.expand(30.0), Some(Align::Center));
            app.transcript_ui.last_follow = Some((clip_id, tok));
        }
    }

    // Trefferprüfung Maus → Span
    let span_at = |pos: egui::Pos2| -> Option<usize> {
        let cursor = galley.cursor_from_pos(pos - origin);
        let ci = cursor.ccursor.index;
        spans
            .iter()
            .position(|s| ci >= s.chars.0 && ci <= s.chars.1)
            .or_else(|| spans.iter().rposition(|s| s.chars.0 <= ci))
    };

    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
    }
    let shift = ui.input(|i| i.modifiers.shift);
    let pointer = ui.input(|i| i.pointer.interact_pos());

    if resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) {
        if let Some(si) = pointer.and_then(span_at) {
            let s = &spans[si];
            app.focus = Focus::Transcript;
            match &mut app.sel {
                Some(sel) if shift && sel.clip == clip_id => {
                    sel.focus = if s.first >= sel.anchor { s.last } else { s.first };
                }
                _ => {
                    app.sel = Some(Sel { clip: clip_id, anchor: s.first, focus: s.last });
                    app.transcript_ui.press = Some((clip_id, s.first, s.last));
                }
            }
        }
    }
    if resp.dragged() {
        if let (Some(si), Some((pc, pf, pl))) = (pointer.and_then(span_at), app.transcript_ui.press) {
            if pc == clip_id {
                let s = &spans[si];
                app.sel = Some(if s.first >= pf {
                    Sel { clip: clip_id, anchor: pf, focus: s.last }
                } else {
                    Sel { clip: clip_id, anchor: pl, focus: s.first }
                });
            }
        }
    }
    if resp.clicked() && !shift {
        if let Some(si) = pointer.and_then(span_at) {
            let tok = &tokens[spans[si].first];
            let t = app.project.source_to_timeline(&clip, tok.range.start);
            app.selected_clip = Some(clip_id);
            app.seek(t);
        }
    }
    if resp.double_clicked() {
        if let Some(si) = pointer.and_then(span_at) {
            app.sel = Some(Sel { clip: clip_id, anchor: spans[si].first, focus: spans[si].last });
        }
    }

    resp.context_menu(|ui| {
        let Some(sel) = app.sel.as_ref().filter(|s| s.clip == clip_id) else {
            ui.label("Erst Text markieren");
            return;
        };
        let (a, b) = sel.range();
        if ui.button("✂ Auswahl herausschneiden").clicked() {
            app.set_words_deleted(clip_id, a, b, true);
            ui.close_menu();
        }
        if ui.button("↺ Auswahl wiederherstellen").clicked() {
            app.set_words_deleted(clip_id, a, b, false);
            ui.close_menu();
        }
        if ui.button("✂ Clip hier teilen").clicked() {
            let t = app.project.source_to_timeline(&clip, tokens[a].range.start);
            app.selected_clip = Some(clip_id);
            app.seek(t);
            app.split_at_playhead();
            ui.close_menu();
        }
        if ui.button("📋 Text kopieren").clicked() {
            let text: Vec<&str> =
                tokens[a..=b].iter().filter(|t| t.kind == TokenKind::Word).map(|t| t.text.as_str()).collect();
            ui.ctx().copy_text(text.join(" "));
            ui.close_menu();
        }
    });
}

/// Einstellungen für weiche Übergänge an Schnittstellen (mit Kurvenvorschau).
pub fn transition_settings(app: &mut App, ui: &mut egui::Ui) {
    ui.set_min_width(300.0);
    let before = app.project.transition;
    let mut t = before;
    ui.label(RichText::new("Weiche Übergänge an Schnitten").strong());
    ui.label(RichText::new("Der Ton wird vor jedem Schnitt aus- und danach wieder eingeblendet.\nDas Bild bleibt ein harter Schnitt, die Länge ändert sich nicht.").small().weak());
    ui.add_space(4.0);
    ui.checkbox(&mut t.enabled, "Aktiv");
    ui.add_enabled_ui(t.enabled, |ui| {
        ui.add(egui::Slider::new(&mut t.ms, 5.0..=500.0).logarithmic(true).suffix(" ms").text("Länge"))
            .on_hover_text("Gesamtlänge: je die Hälfte zum Aus- und Einblenden.\n20–60 ms: unhörbar weich · 100–300 ms: deutlich hörbar");
        ui.horizontal(|ui| {
            for ms in [20.0, 40.0, 80.0, 150.0, 300.0] {
                if ui.small_button(format!("{ms:.0}")).clicked() {
                    t.ms = ms;
                }
            }
        });
        egui::ComboBox::from_label("Kurve").selected_text(t.curve.label()).width(220.0).show_ui(ui, |ui| {
            for c in FadeCurve::ALL {
                ui.selectable_value(&mut t.curve, c, c.label());
            }
        });
        ui.horizontal(|ui| {
            ui.label("Anwenden auf:");
            ui.checkbox(&mut t.text_cuts, "Text-Schnitte");
            ui.checkbox(&mut t.silence_cuts, "Stille-Schnitte");
        });
        curve_preview(ui, &t);
    });

    if t != before {
        // Eine Undo-Stufe pro Geste (z.B. ganzer Slider-Zug).
        if !app.inspector_editing {
            let mut snapshot = app.project.clone();
            snapshot.transition = before;
            app.push_undo(snapshot);
            app.inspector_editing = true;
        } else {
            app.touch();
        }
        app.project.transition = t;
    }
}

/// Zeigt den Lautstärkeverlauf um einen Schnitt herum.
fn curve_preview(ui: &mut egui::Ui, t: &crate::project::CutTransition) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(280.0), 70.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, Color32::from_gray(30));
    let plot = rect.shrink2(egui::vec2(8.0, 10.0));
    // Sichtbereich: ±(Länge) um den Schnitt
    let half = t.ms as f64 / 2.0;
    let span = half * 2.0;
    let x_of = |ms: f64| plot.left() + ((ms + span) / (2.0 * span)) as f32 * plot.width();
    let y_of = |g: f64| plot.bottom() - g as f32 * plot.height();
    let pts: Vec<egui::Pos2> = (0..=120)
        .map(|i| {
            let ms = -span + 2.0 * span * i as f64 / 120.0;
            let g = if t.enabled { t.curve.eval(ms.abs() / half) } else { 1.0 };
            egui::pos2(x_of(ms), y_of(g))
        })
        .collect();
    p.line_segment([egui::pos2(x_of(0.0), rect.top()), egui::pos2(x_of(0.0), rect.bottom())], Stroke::new(1.5, style::CUT));
    p.add(egui::Shape::line(pts, Stroke::new(2.0, style::ACCENT)));
    let font = FontId::proportional(10.0);
    p.text(egui::pos2(x_of(0.0) + 3.0, rect.top() + 2.0), egui::Align2::LEFT_TOP, "✂ Schnitt", font.clone(), style::CUT);
    p.text(rect.left_bottom() + egui::vec2(4.0, -2.0), egui::Align2::LEFT_BOTTOM, format!("−{span:.0} ms"), font.clone(), Color32::GRAY);
    p.text(rect.right_bottom() + egui::vec2(-4.0, -2.0), egui::Align2::RIGHT_BOTTOM, format!("+{span:.0} ms"), font, Color32::GRAY);
}
