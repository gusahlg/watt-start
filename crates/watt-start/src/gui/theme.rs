//! Colours and typography: a deep blue-black dark theme with an amber ("watt") primary accent
//! and an electric cyan secondary, plus the per-instance accent palette.

use egui::{Color32, FontFamily, FontId, TextStyle};

/// The primary accent ("watt": amber/orange).
pub const WATT: Color32 = Color32::from_rgb(0xff, 0xa4, 0x2e);
/// The secondary accent (electric cyan).
pub const CYAN: Color32 = Color32::from_rgb(0x3d, 0xe7, 0xe0);
/// Success green.
pub const GOOD: Color32 = Color32::from_rgb(0x4c, 0xd9, 0x7c);
/// Warning amber (reused for "needs build").
pub const WARN: Color32 = Color32::from_rgb(0xff, 0xb0, 0x4a);
/// Error red.
pub const BAD: Color32 = Color32::from_rgb(0xf0, 0x5a, 0x5a);

/// Deep blue-black backgrounds.
pub const BG_0: Color32 = Color32::from_rgb(0x0a, 0x0c, 0x14); // window background
pub const BG_1: Color32 = Color32::from_rgb(0x10, 0x13, 0x1f); // panels
pub const BG_2: Color32 = Color32::from_rgb(0x17, 0x1b, 0x2a); // cards, rows
pub const BG_3: Color32 = Color32::from_rgb(0x20, 0x25, 0x38); // hovered rows, inputs

/// Muted foreground for secondary text.
pub const MUTED: Color32 = Color32::from_rgb(0x8a, 0x92, 0xad);

/// The ~10-colour palette instance accents are assigned from by hashing the instance name.
pub const INSTANCE_PALETTE: [Color32; 10] = [
    WATT,
    CYAN,
    Color32::from_rgb(0xe0, 0x6c, 0xf0), // orchid
    Color32::from_rgb(0x6c, 0x8c, 0xf0), // periwinkle
    GOOD,
    Color32::from_rgb(0xf0, 0x6c, 0x8c), // rose
    Color32::from_rgb(0xf0, 0xd4, 0x6c), // gold
    Color32::from_rgb(0x6c, 0xf0, 0xc4), // mint
    Color32::from_rgb(0xb0, 0x8c, 0xf0), // violet
    Color32::from_rgb(0xf0, 0x9a, 0x6c), // coral
];

/// A stable accent colour for an instance without one set in its metadata.
pub fn accent_for(name: &str) -> Color32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    INSTANCE_PALETTE[(hash as usize) % INSTANCE_PALETTE.len()]
}

/// Parse `meta.color` (`#rrggbb`), falling back to [`accent_for`].
pub fn accent_for_meta(name: &str, color: Option<&str>) -> Color32 {
    color
        .and_then(parse_hex_color)
        .unwrap_or_else(|| accent_for(name))
}

/// Parse `#rrggbb`.
pub fn parse_hex_color(s: &str) -> Option<Color32> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

/// To `#rrggbb`.
pub fn to_hex_color(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

/// Apply the dark (or light) theme, bigger text styles and generous spacing to `ctx`.
pub fn apply(ctx: &egui::Context, dark: bool) {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    if dark {
        visuals.panel_fill = BG_1;
        visuals.window_fill = BG_1;
        visuals.extreme_bg_color = BG_0;
        visuals.faint_bg_color = BG_2;
        visuals.widgets.noninteractive.bg_fill = BG_2;
        visuals.widgets.inactive.bg_fill = BG_2;
        visuals.widgets.hovered.bg_fill = BG_3;
        visuals.widgets.active.bg_fill = BG_3;
        visuals.selection.bg_fill = WATT.linear_multiply(0.55);
        visuals.selection.stroke = egui::Stroke::new(1.0, WATT);
        visuals.hyperlink_color = CYAN;
    }
    visuals.window_corner_radius = 10u8.into();
    visuals.menu_corner_radius = 8u8.into();
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = 8u8.into();
    }
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, visuals);
    ctx.style_mut_of(theme, |style| {
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(14);
        // Only re-size the built-in styles (never add a new `TextStyle::Name` key): a `Ui`
        // created earlier in the same pass has already cloned the *previous* `Style`, so a
        // fresh key would be missing from it and panic on lookup, while resizing an existing
        // key just renders with the old size for the rest of that one pass.
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(26.0, FontFamily::Proportional),
        );
        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(14.5, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.5, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        );
    });
}

/// Near-white, for text that must read as the brightest thing on the page (a big heading, the
/// instance title): the default foreground is a working grey, legible but not a focal point.
pub const BRIGHT: Color32 = Color32::from_rgb(0xf5, 0xf7, 0xfc);

/// A "Title"-sized [`egui::RichText`] (between [`TextStyle::Heading`] and [`TextStyle::Body`]),
/// bright and bold: a page-level heading ("Settings", "Welcome to watt-start").
pub fn title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(FontId::new(20.0, FontFamily::Proportional))
        .color(BRIGHT)
        .strong()
}

/// The instance page's own title: bigger than [`title`] and just as bright, so it is
/// unmistakably the primary heading of the page (the dir name under it stays small and muted).
pub fn heading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(FontId::new(28.0, FontFamily::Proportional))
        .color(BRIGHT)
        .strong()
}

/// A "Subheading"-sized [`egui::RichText`].
pub fn subheading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).font(FontId::new(16.0, FontFamily::Proportional))
}

/// Muted small text.
pub fn muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).color(MUTED).small()
}

/// A small filled status dot, painted rather than drawn from a glyph (status/bullet symbols
/// are not guaranteed to be in every installed font, including the minimal one headless/test
/// rendering uses).
pub fn dot(ui: &mut egui::Ui, color: egui::Color32) -> egui::Response {
    let size = egui::vec2(8.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size.x / 2.0, color);
    response
}
