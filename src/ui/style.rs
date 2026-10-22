//! Colors and base style.

use crate::project::TrackKind;
use eframe::egui::{self, Color32, FontId, TextStyle};

pub const ACCENT: Color32 = Color32::from_rgb(255, 140, 50);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(150, 70, 20);
pub const CUT: Color32 = Color32::from_rgb(235, 80, 80);
pub const WARN: Color32 = Color32::from_rgb(240, 190, 60);
pub const SELECTION: Color32 = Color32::from_rgb(40, 90, 160);
pub const CURRENT: Color32 = Color32::from_rgb(200, 100, 30);
pub const SILENCE_BG: Color32 = Color32::from_rgb(45, 45, 52);
pub const SILENCE_TEXT: Color32 = Color32::from_rgb(140, 140, 160);
pub const BLOCK_SELECTED: Color32 = Color32::from_rgba_premultiplied(14, 8, 3, 14);

pub const MAIN_CLIP: Color32 = Color32::from_rgb(52, 101, 190);
pub const OVERLAY_CLIP: Color32 = Color32::from_rgb(132, 76, 200);
pub const AUDIO_CLIP: Color32 = Color32::from_rgb(40, 140, 100);
pub const PLAYHEAD: Color32 = Color32::from_rgb(255, 70, 70);

pub fn track_color(is_main: bool, kind: Option<TrackKind>) -> Color32 {
    match (is_main, kind) {
        (true, _) => MAIN_CLIP,
        (false, Some(TrackKind::Video)) => OVERLAY_CLIP,
        _ => AUDIO_CLIP,
    }
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::dark());
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(8.0, 4.0);
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Heading, FontId::proportional(18.0));
        s.visuals.selection.bg_fill = SELECTION;
        s.visuals.hyperlink_color = ACCENT;
    });
}
