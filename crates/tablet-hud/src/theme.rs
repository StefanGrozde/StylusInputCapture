//! The HUD's visual identity: a dark hardware-panel look (think Ableton Push /
//! ROLI Dashboard) with a single cyan accent.
//!
//! Everything color-related lives here — `apply` restyles egui once at
//! startup, the named constants replace the colors previously inlined in
//! `app.rs`, and the small drawing helpers (`status_dot`, `meter_bar`,
//! `bipolar_bar`) are shared by the top bar and the meters strip.

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, Stroke, Vec2};

// ── Palette ─────────────────────────────────────────────────────────────────

/// The single accent color (selection, lit pads, meters).
pub const ACCENT: Color32 = Color32::from_rgb(64, 210, 224);
/// Text drawn *on top of* an accent-filled surface (lit pad labels).
pub const ACCENT_TEXT: Color32 = Color32::from_rgb(8, 26, 34);
/// Dimmed accent for secondary highlights (root pad labels, captions).
pub const ACCENT_DIM: Color32 = Color32::from_rgb(120, 185, 215);

/// Near-black panel/window background (~#101014).
pub const BG: Color32 = Color32::from_rgb(16, 16, 20);
/// Slightly lifted background for the surface canvas and meter troughs.
pub const BG_RAISED: Color32 = Color32::from_rgb(24, 24, 30);

/// Resting pad fill / stroke.
pub const PAD: Color32 = Color32::from_rgb(42, 44, 50);
pub const PAD_STROKE: Color32 = Color32::from_rgb(62, 65, 72);
/// Root-of-scale pad fill / stroke (cooler, bluer).
pub const PAD_ROOT: Color32 = Color32::from_rgb(38, 70, 110);
pub const PAD_ROOT_STROKE: Color32 = Color32::from_rgb(70, 120, 170);
/// Pads beyond the MIDI note range: dead, nearly invisible.
pub const PAD_DEAD: Color32 = Color32::from_rgb(18, 18, 20);
pub const PAD_DEAD_STROKE: Color32 = Color32::from_rgb(30, 30, 33);
/// Stroke around the sounding pad.
pub const PAD_ACTIVE_STROKE: Color32 = Color32::from_rgb(160, 230, 255);

/// Status colors for the connection pills and inline messages.
pub const STATUS_OK: Color32 = Color32::from_rgb(80, 210, 120);
pub const STATUS_WARN: Color32 = Color32::from_rgb(235, 180, 70);
pub const STATUS_ERR: Color32 = Color32::from_rgb(235, 90, 90);
/// A neutral gray dot for "not connected / idle".
pub const STATUS_IDLE: Color32 = Color32::from_rgb(110, 112, 120);

/// Fading pen trail on the surface (alpha applied per point).
pub const TRAIL: Color32 = Color32::from_rgb(80, 190, 255);
/// Pen cursor while pressing / while hovering.
pub const CURSOR_ACTIVE: Color32 = Color32::from_rgb(255, 220, 90);
pub const CURSOR_HOVER: Color32 = Color32::from_rgb(150, 150, 160);
/// Vibrato-active glow around the stylus (distinct from the cyan accent).
pub const VIBRATO: Color32 = Color32::from_rgb(255, 55, 55);

/// Fill of the sounding pad, brightness following pen pressure (the same
/// cyan ramp the old inline code used, now in the accent hue).
pub fn pad_active(pressure: f32) -> Color32 {
    let lum = 120.0 + 135.0 * pressure.clamp(0.0, 1.0);
    Color32::from_rgb(
        (lum * 0.45) as u8,
        (lum * 0.85) as u8,
        lum.min(255.0) as u8,
    )
}

// ── Style ───────────────────────────────────────────────────────────────────

/// Restyle egui for the controller look. Call once at startup, before the
/// first frame.
pub fn apply(ctx: &egui::Context) {
    ctx.global_style_mut(|style| {
        let v = &mut style.visuals;
        v.dark_mode = true;
        v.panel_fill = BG;
        v.window_fill = BG;
        v.extreme_bg_color = Color32::from_rgb(10, 10, 13);
        v.faint_bg_color = Color32::from_rgb(26, 26, 32);
        v.hyperlink_color = ACCENT;
        v.selection.bg_fill = ACCENT.linear_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, ACCENT);

        let radius = CornerRadius::same(6);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = radius;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(38, 39, 46));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(190, 192, 200));
        v.widgets.inactive.bg_fill = Color32::from_rgb(34, 35, 42);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 35, 42);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(50, 52, 60));
        v.widgets.hovered.bg_fill = Color32::from_rgb(46, 48, 58);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(46, 48, 58);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.linear_multiply(0.6));
        v.widgets.active.bg_fill = ACCENT.linear_multiply(0.45);
        v.widgets.active.weak_bg_fill = ACCENT.linear_multiply(0.45);
        v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        v.widgets.open.bg_fill = Color32::from_rgb(40, 42, 50);

        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(10.0, 4.0);
        style.spacing.slider_width = 140.0;
    });
}

// ── Drawing helpers ─────────────────────────────────────────────────────────

/// A colored ● + label, used as a connection-status pill in the top bar.
pub fn status_dot(ui: &mut egui::Ui, color: Color32, label: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 5.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), 4.0, color);
        ui.label(label);
    });
}

/// A unipolar meter bar: trough plus a fill spanning `frac` of the width.
pub fn meter_bar(painter: &egui::Painter, rect: Rect, frac: f32, color: Color32) {
    let radius = (rect.height() * 0.4).min(6.0);
    painter.rect_filled(rect, radius, BG_RAISED);
    let frac = frac.clamp(0.0, 1.0);
    if frac > 0.0 {
        let fill = Rect::from_min_size(
            rect.min,
            Vec2::new((rect.width() * frac).max(radius * 2.0), rect.height()),
        );
        painter.rect_filled(fill, radius, color);
    }
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, PAD_STROKE),
        egui::StrokeKind::Inside,
    );
}

/// A bipolar meter bar: the fill grows from the center toward either rail
/// (`frac` in `[-1, 1]`), with a faint center tick at neutral.
pub fn bipolar_bar(painter: &egui::Painter, rect: Rect, frac: f32, color: Color32) {
    let radius = (rect.height() * 0.4).min(6.0);
    painter.rect_filled(rect, radius, BG_RAISED);
    let frac = frac.clamp(-1.0, 1.0);
    let center_x = rect.center().x;
    let half = rect.width() / 2.0;
    if frac.abs() > 0.001 {
        let extent = half * frac.abs();
        let fill = if frac >= 0.0 {
            Rect::from_min_max(
                Pos2::new(center_x, rect.top()),
                Pos2::new(center_x + extent, rect.bottom()),
            )
        } else {
            Rect::from_min_max(
                Pos2::new(center_x - extent, rect.top()),
                Pos2::new(center_x, rect.bottom()),
            )
        };
        painter.rect_filled(fill, radius * 0.5, color);
    }
    // Center tick so "in tune" reads at a glance.
    painter.line_segment(
        [
            Pos2::new(center_x, rect.top() + 1.0),
            Pos2::new(center_x, rect.bottom() - 1.0),
        ],
        Stroke::new(1.0, PAD_STROKE),
    );
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, PAD_STROKE),
        egui::StrokeKind::Inside,
    );
}
