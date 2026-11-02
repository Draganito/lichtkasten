use eframe::egui::{self, Color32, CornerRadius, Frame, Margin, Shadow, Stroke, Ui};

/// Warm black from the Mimizan Lab launcher.
pub const BG: Color32 = Color32::from_rgb(0x12, 0x0F, 0x0A);
pub const VOID: Color32 = Color32::BLACK;
pub const SURFACE: Color32 = Color32::from_rgb(0x1A, 0x17, 0x12);
pub const INK: Color32 = Color32::from_rgb(0xE6, 0xE6, 0xE6);
pub const MUTED: Color32 = Color32::from_rgb(0x6E, 0x6A, 0x64);
pub const CHIP: Color32 = Color32::from_rgb(0x2A, 0x27, 0x22);
pub const HAIRLINE: Color32 = Color32::from_rgba_premultiplied(40, 36, 30, 40);
pub const HAIRLINE_STRONG: Color32 = Color32::from_rgba_premultiplied(90, 84, 76, 90);
/// Warnfarbe für unumkehrbare Aktionen (Papierkorb leeren).
pub const DANGER: Color32 = Color32::from_rgb(0xD9, 0x6C, 0x5A);

pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.override_text_color = Some(INK);
    visuals.panel_fill = VOID;
    visuals.window_fill = BG;
    visuals.extreme_bg_color = VOID;
    visuals.faint_bg_color = SURFACE;
    visuals.code_bg_color = SURFACE;
    visuals.hyperlink_color = INK;
    visuals.warn_fg_color = Color32::from_rgb(0xE0, 0xB0, 0x80);
    visuals.error_fg_color = Color32::from_rgb(0xFF, 0x80, 0x80);
    visuals.selection.bg_fill = CHIP;
    visuals.selection.stroke = Stroke::NONE;
    visuals.slider_trailing_fill = true;
    visuals.window_stroke = Stroke::new(1.0, HAIRLINE);
    visuals.window_shadow = Shadow::NONE;
    visuals.window_corner_radius = CornerRadius::same(8);
    visuals.menu_corner_radius = CornerRadius::same(8);
    visuals.popup_shadow = Shadow::NONE;
    visuals.widgets.noninteractive = egui::style::WidgetVisuals {
        bg_fill: Color32::TRANSPARENT,
        weak_bg_fill: Color32::TRANSPARENT,
        bg_stroke: Stroke::NONE,
        corner_radius: CornerRadius::same(6),
        fg_stroke: Stroke::new(1.0, MUTED),
        expansion: 0.0,
    };
    let idle = egui::style::WidgetVisuals {
        bg_fill: Color32::TRANSPARENT,
        weak_bg_fill: Color32::TRANSPARENT,
        bg_stroke: Stroke::NONE,
        corner_radius: CornerRadius::same(6),
        fg_stroke: Stroke::new(1.0, INK),
        expansion: 0.0,
    };
    visuals.widgets.inactive = idle;
    visuals.widgets.hovered = egui::style::WidgetVisuals {
        bg_fill: CHIP,
        weak_bg_fill: CHIP,
        bg_stroke: Stroke::NONE,
        corner_radius: CornerRadius::same(6),
        fg_stroke: Stroke::new(1.0, Color32::WHITE),
        expansion: 0.0,
    };
    visuals.widgets.active = visuals.widgets.hovered;
    visuals.widgets.open = visuals.widgets.hovered;
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.button_padding = egui::vec2(10.0, 4.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.interact_size.y = 22.0;
    });
}

pub fn chrome(inner: Margin) -> Frame {
    Frame::NONE.fill(BG).inner_margin(inner)
}

pub fn side() -> Frame {
    Frame::NONE.fill(BG).inner_margin(Margin::symmetric(16, 14))
}

pub fn hairline_bottom(ui: &Ui) {
    let rect = ui.max_rect();
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        Stroke::new(1.0, HAIRLINE),
    );
}

pub fn hairline_top(ui: &Ui) {
    let rect = ui.max_rect();
    ui.painter()
        .hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, HAIRLINE));
}

pub fn section(ui: &mut Ui, title: &str) {
    ui.label(egui::RichText::new(title).small().color(MUTED));
}
