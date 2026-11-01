//! Colours, fonts and the shared look of rows and bars.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontFamily, FontId, Stroke, TextStyle, Vec2,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};
use sizequilt::category::CATEGORIES;

pub const BG: Color32 = Color32::from_rgb(14, 17, 22);
pub const PANEL: Color32 = Color32::from_rgb(21, 26, 33);
pub const RAISED: Color32 = Color32::from_rgb(30, 37, 46);
pub const HOVER: Color32 = Color32::from_rgb(38, 46, 57);
pub const LINE: Color32 = Color32::from_rgb(45, 54, 66);
pub const TEXT: Color32 = Color32::from_rgb(230, 234, 240);
pub const MUTED: Color32 = Color32::from_rgb(139, 150, 165);
pub const ACCENT: Color32 = Color32::from_rgb(45, 212, 191);
pub const ACCENT_DIM: Color32 = Color32::from_rgb(20, 74, 70);
pub const WARNING: Color32 = Color32::from_rgb(240, 190, 66);
pub const DANGER: Color32 = Color32::from_rgb(229, 72, 96);

/// The colour of category `index`.
pub fn category_color(index: usize) -> Color32 {
    let [r, g, b] = CATEGORIES[index].rgb;
    Color32::from_rgb(r, g, b)
}

/// Applies the app's fonts and visuals.
pub fn apply(ctx: &egui::Context) {
    // Segoe UI for the look of Windows; the CJK font so any file name reads.
    let fonts = std::env::var_os("SystemRoot")
        .map(|root| std::path::PathBuf::from(root).join("Fonts"));
    if let Some(folder) = fonts {
        for (name, file, priority) in [
            ("segoe", "segoeui.ttf", FontPriority::Highest),
            ("yahei", "msyh.ttc", FontPriority::Lowest),
        ] {
            if let Ok(bytes) = std::fs::read(folder.join(file)) {
                ctx.add_font(FontInsert::new(
                    name,
                    FontData::from_owned(bytes),
                    vec![InsertFontFamily {
                        family: FontFamily::Proportional,
                        priority,
                    }],
                ));
            }
        }
    }

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = PANEL;
    visuals.window_fill = PANEL;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.extreme_bg_color = BG;
    visuals.faint_bg_color = RAISED;
    visuals.selection.bg_fill = ACCENT_DIM;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.hyperlink_color = ACCENT;
    visuals.override_text_color = Some(TEXT);
    let radius = CornerRadius::same(5);
    for w in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.inactive.weak_bg_fill = RAISED;
    visuals.widgets.inactive.bg_fill = RAISED;
    visuals.widgets.hovered.weak_bg_fill = HOVER;
    visuals.widgets.hovered.bg_fill = HOVER;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, LINE);
    visuals.widgets.active.weak_bg_fill = ACCENT_DIM;
    visuals.widgets.active.bg_fill = ACCENT_DIM;
    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(6.0, 4.0);
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.text_styles.insert(TextStyle::Body, FontId::proportional(13.5));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(13.5));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(11.5));
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(24.0));
    });
}

/// The window icon: the same quilt the file itself carries.
pub fn icon() -> Arc<egui::IconData> {
    const SIZE: usize = 64;
    Arc::new(egui::IconData {
        rgba: sizequilt::icon::pixels(SIZE),
        width: SIZE as u32,
        height: SIZE as u32,
    })
}
