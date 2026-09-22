use crate::config::Theme;
use eframe::egui::{self, Color32, FontFamily, FontId, Stroke, TextStyle};

pub const GREEN: Color32 = Color32::from_rgb(63, 140, 97);

pub fn install_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "Sora".into(),
        egui::FontData::from_static(include_bytes!("../assets/Sora.ttf")).into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Sora".into());
    context.set_fonts(fonts);
}

pub fn apply(context: &egui::Context, theme: Theme) {
    let dark = theme == Theme::Midnight;
    let background = if dark {
        Color32::from_rgb(11, 15, 13)
    } else {
        Color32::from_rgb(135, 219, 148)
    };
    let surface = if dark {
        Color32::from_rgb(18, 25, 22)
    } else {
        Color32::from_rgb(254, 254, 254)
    };
    let text = if dark {
        Color32::from_rgb(232, 240, 234)
    } else {
        Color32::from_rgb(37, 48, 42)
    };
    let muted = if dark {
        Color32::from_rgb(167, 181, 173)
    } else {
        Color32::from_rgb(75, 90, 80)
    };
    let border = if dark {
        Color32::from_rgb(31, 42, 35)
    } else {
        Color32::from_rgb(216, 224, 214)
    };
    let tint = if dark {
        Color32::from_rgb(31, 50, 41)
    } else {
        Color32::from_rgb(231, 243, 236)
    };
    let mut style = (*context.global_style()).clone();
    style.visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    style.visuals.override_text_color = Some(text);
    style.visuals.weak_text_color = Some(muted);
    style.visuals.panel_fill = background;
    style.visuals.window_fill = surface;
    style.visuals.extreme_bg_color = surface;
    style.visuals.faint_bg_color = tint;
    style.visuals.text_edit_bg_color = Some(surface);
    style.visuals.window_stroke = Stroke::new(1.0, border);
    style.visuals.window_corner_radius = 14.into();
    style.visuals.selection.bg_fill = tint;
    style.visuals.selection.stroke = Stroke::new(
        1.5,
        if dark {
            Color32::from_rgb(117, 168, 141)
        } else {
            GREEN
        },
    );
    style.visuals.hyperlink_color = if dark {
        Color32::from_rgb(117, 168, 141)
    } else {
        GREEN
    };
    style.visuals.warn_fg_color = if dark {
        Color32::from_rgb(244, 191, 117)
    } else {
        Color32::from_rgb(130, 76, 12)
    };
    style.visuals.error_fg_color = if dark {
        Color32::from_rgb(242, 132, 130)
    } else {
        Color32::from_rgb(169, 38, 38)
    };
    style.visuals.widgets.noninteractive.bg_fill = surface;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, border);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text);
    style.visuals.widgets.inactive.bg_fill = surface;
    style.visuals.widgets.inactive.weak_bg_fill = surface;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, text);
    style.visuals.widgets.hovered.bg_fill = tint;
    style.visuals.widgets.hovered.weak_bg_fill = tint;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, GREEN);
    style.visuals.widgets.active.bg_fill = tint;
    style.visuals.widgets.active.weak_bg_fill = tint;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.5, GREEN);
    style.visuals.widgets.inactive.corner_radius = 8.into();
    style.visuals.widgets.hovered.corner_radius = 8.into();
    style.visuals.widgets.active.corner_radius = 8.into();
    style.spacing.item_spacing = egui::vec2(12.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 9.0);
    style.spacing.interact_size = egui::vec2(40.0, 36.0);
    style.spacing.window_margin = 20.into();
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(28.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(14.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(12.0));
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::monospace(12.0));
    context.set_global_style(style);
}

pub fn paint_background(ui: &egui::Ui) {
    if ui.visuals().dark_mode {
        return;
    }
    let bounds = ui.max_rect().expand(28.0);
    let start = Color32::from_rgb(254, 254, 254);
    let end = Color32::from_rgb(135, 219, 148);
    let horizontal = bounds.width() * 0.5;
    let vertical = bounds.height() * 0.866_025_4;
    let extent = horizontal + vertical;
    let mut mesh = egui::Mesh::default();
    for (position, fraction) in [
        (bounds.left_top(), 0.0),
        (bounds.right_top(), horizontal / extent),
        (bounds.right_bottom(), 1.0),
        (bounds.left_bottom(), vertical / extent),
    ] {
        mesh.colored_vertex(position, start.lerp_to_gamma(end, fraction));
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}
