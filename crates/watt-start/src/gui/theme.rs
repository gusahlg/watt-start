//! Filament theme: cool graphite layers, a warm Volt accent and a cool Arc accent.
//!
//! Text styles only resize the built-in [`egui::TextStyle`] keys. A `Ui` created earlier in the
//! same pass has already cloned the previous [`egui::Style`], so a fresh `TextStyle::Name` would
//! be missing from it and panic on lookup. Weight comes from the `semibold` font family instead.

use std::sync::{Arc, OnceLock};

use egui::{Color32, FontFamily, FontId, TextStyle};

/// Window background and input fill.
pub const BG_CANVAS: Color32 = Color32::from_rgb(0x0B, 0x0D, 0x12);
/// Sidebar panel.
pub const BG_SIDEBAR: Color32 = Color32::from_rgb(0x10, 0x13, 0x1A);
/// Main area.
pub const BG_SURFACE: Color32 = Color32::from_rgb(0x15, 0x19, 0x22);
/// Cards, header card, dialogs.
pub const BG_CARD: Color32 = Color32::from_rgb(0x1B, 0x20, 0x2B);
/// Hovered rows, selected sidebar row, secondary buttons, pills, popups.
pub const BG_RAISED: Color32 = Color32::from_rgb(0x23, 0x29, 0x37);
/// Pressed fills, the active segment, tooltips.
pub const BG_OVERLAY: Color32 = Color32::from_rgb(0x2B, 0x32, 0x43);
/// Card edges. White at alpha 15 (about 6%).
pub const LINE_SOFT: Color32 = Color32::from_rgba_unmultiplied_const(255, 255, 255, 15);
/// Inputs, separators, secondary button stroke.
pub const LINE: Color32 = Color32::from_rgb(0x2D, 0x34, 0x44);
/// Hover strokes.
pub const LINE_STRONG: Color32 = Color32::from_rgb(0x3C, 0x45, 0x58);
/// Titles, row names, the active tab.
pub const TEXT_BRIGHT: Color32 = Color32::from_rgb(0xF4, 0xF6, 0xFA);
/// Body text.
pub const TEXT: Color32 = Color32::from_rgb(0xD3, 0xD8, 0xE2);
/// Meta and secondary text.
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8C, 0x95, 0xA8);
/// Placeholders and disabled text.
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x68, 0x71, 0x87);
/// Text on Volt.
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x15, 0x12, 0x0A);
/// Primary accent: Play, needs-build, forge heat.
pub const VOLT: Color32 = Color32::from_rgb(0xFF, 0xC2, 0x1A);
/// Primary button hover.
pub const VOLT_HOVER: Color32 = Color32::from_rgb(0xFF, 0xD0, 0x4D);
/// Primary button pressed.
pub const VOLT_PRESSED: Color32 = Color32::from_rgb(0xE8, 0xAE, 0x0A);
/// Volt at alpha 36. Selected and active tint fills.
pub const VOLT_TINT: Color32 = Color32::from_rgba_unmultiplied_const(0xFF, 0xC2, 0x1A, 36);
/// Running, links, focus rings, update badges.
pub const ARC: Color32 = Color32::from_rgb(0x3D, 0xD6, 0xFF);
/// Arc at alpha 36.
pub const ARC_TINT: Color32 = Color32::from_rgba_unmultiplied_const(0x3D, 0xD6, 0xFF, 36);
/// Ready and success.
pub const OK: Color32 = Color32::from_rgb(0x4C, 0xD3, 0x7D);
/// Doctor warnings.
pub const WARN: Color32 = Color32::from_rgb(0xFF, 0x8F, 0x3D);
/// Broken, failed, delete.
pub const BAD: Color32 = Color32::from_rgb(0xFF, 0x5D, 0x5D);
/// New, unknown, cancelled.
pub const NEUTRAL: Color32 = Color32::from_rgb(0x7C, 0x86, 0x98);
/// Forge Link phase ("white-hot").
pub const HOT: Color32 = Color32::from_rgb(0xFF, 0xF3, 0xC4);

/// The 10 instance accent colours (cube icon, header tint, sidebar selection bar).
///
/// Never used as a button fill.
pub const INSTANCE_PALETTE: [Color32; 10] = [
    Color32::from_rgb(0xFF, 0xC2, 0x1A), // Volt
    Color32::from_rgb(0x3D, 0xD6, 0xFF), // Arc
    Color32::from_rgb(0xFF, 0x7A, 0x59), // Ember
    Color32::from_rgb(0x5E, 0xDC, 0x8A), // Moss
    Color32::from_rgb(0x8F, 0x7B, 0xFF), // Ion
    Color32::from_rgb(0xFF, 0x6F, 0xA8), // Rose
    Color32::from_rgb(0x58, 0xB3, 0xFF), // Sky
    Color32::from_rgb(0xC9, 0xE2, 0x4A), // Lime
    Color32::from_rgb(0xFF, 0xA6, 0x4D), // Copper
    Color32::from_rgb(0x4F, 0xE0, 0xC8), // Teal
];

// transitional: removed once every screen uses the new tokens
/// Old name for [`VOLT`].
pub const WATT: Color32 = VOLT;
/// Old name for [`ARC`].
pub const CYAN: Color32 = ARC;
/// Old name for [`OK`].
pub const GOOD: Color32 = OK;
/// Old name for [`BG_CANVAS`].
#[allow(dead_code)] // transitional: still part of the alias set; no screen reads it yet
pub const BG_0: Color32 = BG_CANVAS;
/// Old name for [`BG_SURFACE`].
#[allow(dead_code)] // transitional: still part of the alias set; no screen reads it yet
pub const BG_1: Color32 = BG_SURFACE;
/// Old name for [`BG_CARD`].
#[allow(dead_code)] // transitional: still part of the alias set; no screen reads it yet
pub const BG_2: Color32 = BG_CARD;
/// Old name for [`BG_RAISED`].
pub const BG_3: Color32 = BG_RAISED;
/// Old name for [`TEXT_MUTED`].
#[allow(dead_code)] // transitional: still part of the alias set; no screen reads it yet
pub const MUTED: Color32 = TEXT_MUTED;
/// Old name for [`TEXT_BRIGHT`].
#[allow(dead_code)] // transitional: still part of the alias set; no screen reads it yet
pub const BRIGHT: Color32 = TEXT_BRIGHT;

/// Colours for one theme mode. Widgets read this so a light theme stays legible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Window background and input fill.
    pub bg_canvas: Color32,
    /// Sidebar panel.
    pub bg_sidebar: Color32,
    /// Main area.
    pub bg_surface: Color32,
    /// Cards and dialogs.
    pub bg_card: Color32,
    /// Raised rows, secondary buttons, pills.
    pub bg_raised: Color32,
    /// Pressed fills and the active segment.
    pub bg_overlay: Color32,
    /// Faint card edge.
    pub line_soft: Color32,
    /// Inputs, separators, secondary stroke.
    pub line: Color32,
    /// Hover stroke.
    pub line_strong: Color32,
    /// Titles and active labels.
    pub text_bright: Color32,
    /// Body text.
    pub text: Color32,
    /// Secondary text.
    pub text_muted: Color32,
    /// Placeholders and disabled text.
    pub text_faint: Color32,
    /// Text drawn on Volt.
    pub on_accent: Color32,
    /// Primary accent.
    pub volt: Color32,
    /// Primary hover.
    pub volt_hover: Color32,
    /// Primary pressed.
    pub volt_pressed: Color32,
    /// Volt tint fill.
    pub volt_tint: Color32,
    /// Live accent.
    pub arc: Color32,
    /// Arc tint fill.
    pub arc_tint: Color32,
    /// Success.
    pub ok: Color32,
    /// Warning.
    pub warn: Color32,
    /// Error.
    pub bad: Color32,
    /// Neutral status.
    pub neutral: Color32,
    /// Forge Link colour.
    pub hot: Color32,
    /// Pressed secondary fill (`#1C212C` in the dark theme).
    pub pressed_fill: Color32,
    /// Hover wash for list rows (white at 4% on dark, black at 4% on light).
    pub wash_hover: Color32,
    /// Pressed wash for list rows.
    pub wash_pressed: Color32,
    /// Ghost button hover fill.
    pub ghost_hover: Color32,
    /// Ghost button pressed fill.
    pub ghost_pressed: Color32,
}

/// The dark Filament palette (the `pub const` tokens).
pub fn dark_palette() -> Palette {
    Palette {
        bg_canvas: BG_CANVAS,
        bg_sidebar: BG_SIDEBAR,
        bg_surface: BG_SURFACE,
        bg_card: BG_CARD,
        bg_raised: BG_RAISED,
        bg_overlay: BG_OVERLAY,
        line_soft: LINE_SOFT,
        line: LINE,
        line_strong: LINE_STRONG,
        text_bright: TEXT_BRIGHT,
        text: TEXT,
        text_muted: TEXT_MUTED,
        text_faint: TEXT_FAINT,
        on_accent: ON_ACCENT,
        volt: VOLT,
        volt_hover: VOLT_HOVER,
        volt_pressed: VOLT_PRESSED,
        volt_tint: VOLT_TINT,
        arc: ARC,
        arc_tint: ARC_TINT,
        ok: OK,
        warn: WARN,
        bad: BAD,
        neutral: NEUTRAL,
        hot: HOT,
        pressed_fill: Color32::from_rgb(0x1C, 0x21, 0x2C),
        wash_hover: Color32::from_white_alpha(10),
        wash_pressed: Color32::from_white_alpha(20),
        ghost_hover: Color32::from_white_alpha(15),
        ghost_pressed: Color32::from_white_alpha(26),
    }
}

/// A basic light palette. Dark is the priority; this only has to stay legible.
pub fn light_palette() -> Palette {
    let volt = Color32::from_rgb(0xE0, 0xA8, 0x00);
    let arc = Color32::from_rgb(0x00, 0x98, 0xC7);
    Palette {
        bg_canvas: Color32::from_rgb(0xF4, 0xF5, 0xF8),
        bg_sidebar: Color32::from_rgb(0xEC, 0xEE, 0xF3),
        bg_surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        bg_card: Color32::from_rgb(0xF7, 0xF8, 0xFB),
        bg_raised: Color32::from_rgb(0xED, 0xEF, 0xF4),
        bg_overlay: Color32::from_rgb(0xE2, 0xE5, 0xEC),
        line_soft: Color32::from_rgb(0xE6, 0xE8, 0xEE),
        line: Color32::from_rgb(0xD7, 0xDB, 0xE3),
        line_strong: Color32::from_rgb(0xC4, 0xC9, 0xD4),
        text_bright: Color32::from_rgb(0x15, 0x18, 0x1F),
        text: Color32::from_rgb(0x30, 0x34, 0x3D),
        text_muted: Color32::from_rgb(0x6B, 0x72, 0x80),
        text_faint: Color32::from_rgb(0x9A, 0xA0, 0xAC),
        on_accent: ON_ACCENT,
        volt,
        volt_hover: Color32::from_rgb(0xF0, 0xBE, 0x2A),
        volt_pressed: Color32::from_rgb(0xC4, 0x92, 0x00),
        volt_tint: Color32::from_rgba_unmultiplied(volt.r(), volt.g(), volt.b(), 48),
        arc,
        arc_tint: Color32::from_rgba_unmultiplied(arc.r(), arc.g(), arc.b(), 40),
        ok: Color32::from_rgb(0x1E, 0x9E, 0x55),
        warn: Color32::from_rgb(0xD9, 0x77, 0x06),
        bad: Color32::from_rgb(0xDC, 0x26, 0x26),
        neutral: Color32::from_rgb(0x6B, 0x72, 0x80),
        hot: Color32::from_rgb(0xC2, 0x41, 0x0C),
        pressed_fill: Color32::from_rgb(0xD5, 0xD9, 0xE2),
        wash_hover: Color32::from_black_alpha(12),
        wash_pressed: Color32::from_black_alpha(20),
        ghost_hover: Color32::from_black_alpha(16),
        ghost_pressed: Color32::from_black_alpha(28),
    }
}

/// The palette most recently passed to [`apply`]. Dark until the first call.
pub fn palette(ctx: &egui::Context) -> Palette {
    ctx.data(|data| data.get_temp(id("palette")))
        .unwrap_or_else(dark_palette)
}

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
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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

/// Apply the dark or light theme. Fonts are registered once per [`egui::Context`]. Visuals are
/// rebuilt only when `dark`, `animations` or the current zoom factor changes.
///
/// Call this before any widget lays out text. [`egui::Context::set_fonts`] takes effect
/// immediately when nothing has been shaped yet this pass, which is the case at the start of
/// [`crate::gui::app::App::ui`].
pub fn apply(ctx: &egui::Context, dark: bool, animations: bool) {
    install_fonts(ctx);
    ctx.data_mut(|data| data.insert_temp(id("animations"), animations));

    let scale_bits = ctx.zoom_factor().to_bits();
    let key = (dark, animations, scale_bits);
    let already = ctx.data(|data| data.get_temp::<(bool, bool, u32)>(id("applied")));
    if already == Some(key) {
        return;
    }
    ctx.data_mut(|data| {
        data.insert_temp(id("applied"), key);
        data.insert_temp(
            id("palette"),
            if dark {
                dark_palette()
            } else {
                light_palette()
            },
        );
    });

    let p = if dark {
        dark_palette()
    } else {
        light_palette()
    };
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = p.bg_surface;
    visuals.window_fill = p.bg_card;
    visuals.extreme_bg_color = p.bg_canvas;
    visuals.faint_bg_color = p.bg_card;
    visuals.code_bg_color = p.bg_canvas;
    visuals.text_edit_bg_color = Some(p.bg_canvas);
    visuals.weak_text_color = Some(p.text_faint);
    visuals.hyperlink_color = p.arc;
    // `selection.stroke` is the colour of selected text, not an outline.
    visuals.selection.bg_fill = with_alpha(p.volt, 70);
    visuals.selection.stroke = egui::Stroke::new(1.0, p.text_bright);
    visuals.window_stroke = egui::Stroke::new(1.0, p.line);
    visuals.window_corner_radius = 16u8.into();
    visuals.menu_corner_radius = 8u8.into();
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 70 } else { 28 }),
    };
    visuals.window_shadow = visuals.popup_shadow;
    visuals.text_cursor.stroke = egui::Stroke::new(1.5, p.arc);

    let widget = |bg: Color32, stroke: Color32, fg: Color32| egui::style::WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: egui::Stroke::new(1.0, stroke),
        fg_stroke: egui::Stroke::new(1.0, fg),
        corner_radius: 8u8.into(),
        expansion: 0.0,
    };
    visuals.widgets.noninteractive = widget(p.bg_card, p.line, p.text);
    visuals.widgets.inactive = widget(p.bg_raised, p.line, p.text);
    visuals.widgets.hovered = widget(p.bg_overlay, p.line_strong, p.text_bright);
    visuals.widgets.active = widget(p.pressed_fill, p.line_strong, p.text_bright);
    visuals.widgets.open = widget(p.bg_overlay, p.line_strong, p.text_bright);

    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, visuals);
    let animation_time = if animations { 0.12 } else { 0.0 };
    ctx.style_mut_of(theme, |style| {
        style.animation_time = animation_time;
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 8.0);
        style.spacing.window_margin = egui::Margin::same(20);
        // Only re-size the built-in styles (never add a new `TextStyle::Name` key).
        style.text_styles.insert(
            TextStyle::Heading,
            FontId::new(20.0, FontFamily::Proportional),
        );
        style
            .text_styles
            .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
        style.text_styles.insert(
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        );
        style.text_styles.insert(
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        );
        style.text_styles.insert(
            TextStyle::Small,
            FontId::new(12.0, FontFamily::Proportional),
        );
    });
}

/// `base` seconds, or `0` when Settings > Animations is off.
///
/// [`apply`] stores the flag. Callers pass the result to `animate_*_with_time` so a disabled
/// animation snaps and does not request a repaint.
pub fn anim_secs(ctx: &egui::Context, base: f32) -> f32 {
    let on = ctx
        .data(|data| data.get_temp::<bool>(id("animations")))
        .unwrap_or(true);
    if on { base } else { 0.0 }
}

/// Inter Regular at `size` points.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Inter SemiBold at `size` points.
///
/// The family is registered by [`apply`] before the first label. [`egui::Context::set_fonts`]
/// applies immediately when no text has been shaped yet this pass.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, semibold_family())
}

/// Hack (egui's monospace) at `size` points.
#[allow(dead_code)] // public kit helper; screens pick it up in later phases
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Page and dialog title (20 semibold, bright).
pub fn title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(semibold(20.0))
        .color(TEXT_BRIGHT)
}

/// Instance title (28 semibold, bright).
#[allow(dead_code)] // transitional: screens paint palette text so a light theme stays legible
pub fn heading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(semibold(28.0))
        .color(TEXT_BRIGHT)
}

/// Card section heading (15 semibold, bright).
pub fn subheading(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(semibold(15.0))
        .color(TEXT_BRIGHT)
}

/// Muted small text (12 regular).
pub fn muted(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .font(regular(12.0))
        .color(TEXT_MUTED)
}

/// A small filled status dot, painted rather than drawn from a glyph.
pub fn dot(ui: &mut egui::Ui, color: Color32) -> egui::Response {
    let size = egui::vec2(8.0, 8.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), size.x / 2.0, color);
    response
}

fn semibold_family() -> FontFamily {
    static NAME: OnceLock<Arc<str>> = OnceLock::new();
    FontFamily::Name(Arc::clone(NAME.get_or_init(|| Arc::from("semibold"))))
}

fn install_fonts(ctx: &egui::Context) {
    if ctx
        .data(|data| data.get_temp::<bool>(id("fonts")))
        .unwrap_or(false)
    {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    fonts.font_data.insert(
        "Inter".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-Regular.ttf"
        ))),
    );
    fonts.font_data.insert(
        "InterSemiBold".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/Inter-SemiBold.ttf"
        ))),
    );
    if let Some(proportional) = fonts.families.get_mut(&FontFamily::Proportional) {
        proportional.insert(0, "Inter".to_owned());
    }
    let mut semi = Vec::with_capacity(1 + fallbacks.len());
    semi.push("InterSemiBold".to_owned());
    semi.extend(fallbacks);
    fonts
        .families
        .insert(FontFamily::Name(Arc::from("semibold")), semi);
    ctx.set_fonts(fonts);
    ctx.data_mut(|data| data.insert_temp(id("fonts"), true));
}

fn id(salt: &str) -> egui::Id {
    // Globally unique ctx-data key. `Id::new` is deprecated; `unique` is the same hash.
    egui::Id::unique(("filament", salt))
}

/// Same RGB, with `alpha` replacing the existing alpha.
pub fn with_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}
