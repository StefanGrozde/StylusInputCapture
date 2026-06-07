use std::collections::VecDeque;

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, PenSample};
use tablet_process::{ProcessedSample, TargetSpace};

const DEFAULT_TRACE_CAPACITY: usize = 2048;
const TRACE_MIN_HEIGHT: f32 = 320.0;
const PANEL_PADDING: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldPoint {
    pub x: f64,
    pub y: f64,
}

impl WorldPoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldBounds {
    pub min: WorldPoint,
    pub max: WorldPoint,
}

impl WorldBounds {
    pub fn new(min: WorldPoint, max: WorldPoint) -> Self {
        Self { min, max }
    }

    fn width(self) -> f64 {
        (self.max.x - self.min.x).abs().max(f64::EPSILON)
    }

    fn height(self) -> f64 {
        (self.max.y - self.min.y).abs().max(f64::EPSILON)
    }
}

#[derive(Clone, Debug)]
pub struct TracePoint {
    pub processed: ProcessedSample,
    pub raw_mapped: WorldPoint,
}

impl TracePoint {
    pub fn processed_point(&self) -> WorldPoint {
        WorldPoint::new(
            self.processed.x_filtered.unwrap_or(self.processed.x),
            self.processed.y_filtered.unwrap_or(self.processed.y),
        )
    }
}

#[derive(Clone, Debug)]
pub struct TraceHistory {
    capacity: usize,
    points: VecDeque<TracePoint>,
}

impl TraceHistory {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            points: VecDeque::with_capacity(capacity.max(1)),
        }
    }

    pub fn append(&mut self, point: TracePoint) {
        if self.points.len() == self.capacity {
            self.points.pop_front();
        }
        self.points.push_back(point);
    }

    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    pub fn latest(&self) -> Option<&TracePoint> {
        self.points.back()
    }

    pub fn iter(&self) -> impl Iterator<Item = &TracePoint> {
        self.points.iter()
    }
}

impl Default for TraceHistory {
    fn default() -> Self {
        Self::new(DEFAULT_TRACE_CAPACITY)
    }
}

pub fn raw_mapped_point(
    sample: &PenSample,
    caps: &DeviceCapabilities,
    target: &TargetSpace,
) -> WorldPoint {
    match target {
        TargetSpace::Normalized => WorldPoint::new(sample.x_norm, sample.y_norm),
        TargetSpace::ScreenPixels { w, h } => {
            WorldPoint::new(sample.x_norm * f64::from(*w), sample.y_norm * f64::from(*h))
        }
        TargetSpace::Millimeters => WorldPoint::new(
            raw_axis_to_mm(sample.x_raw as f64, &caps.x),
            raw_axis_to_mm(sample.y_raw as f64, &caps.y),
        ),
    }
}

pub fn target_bounds(target: &TargetSpace, caps: Option<&DeviceCapabilities>) -> WorldBounds {
    match target {
        TargetSpace::Normalized => {
            WorldBounds::new(WorldPoint::new(0.0, 0.0), WorldPoint::new(1.0, 1.0))
        }
        TargetSpace::ScreenPixels { w, h } => WorldBounds::new(
            WorldPoint::new(0.0, 0.0),
            WorldPoint::new(f64::from(*w).max(1.0), f64::from(*h).max(1.0)),
        ),
        TargetSpace::Millimeters => {
            if let Some(caps) = caps {
                WorldBounds::new(
                    WorldPoint::new(
                        raw_axis_to_mm(caps.x.min as f64, &caps.x),
                        raw_axis_to_mm(caps.y.min as f64, &caps.y),
                    ),
                    WorldPoint::new(
                        raw_axis_to_mm(caps.x.max as f64, &caps.x),
                        raw_axis_to_mm(caps.y.max as f64, &caps.y),
                    ),
                )
            } else {
                WorldBounds::new(WorldPoint::new(0.0, 0.0), WorldPoint::new(1.0, 1.0))
            }
        }
    }
}

pub fn world_to_panel(point: WorldPoint, bounds: WorldBounds, rect: Rect) -> Pos2 {
    let usable = rect.shrink(PANEL_PADDING);
    let world_w = bounds.width() as f32;
    let world_h = bounds.height() as f32;
    let scale = (usable.width() / world_w).min(usable.height() / world_h);
    let drawn_w = world_w * scale;
    let drawn_h = world_h * scale;
    let left = usable.left() + (usable.width() - drawn_w) * 0.5;
    let top = usable.top() + (usable.height() - drawn_h) * 0.5;
    let x = left + ((point.x - bounds.min.x) as f32) * scale;
    let y = top + ((point.y - bounds.min.y) as f32) * scale;
    Pos2::new(x, y)
}

pub fn draw_trace_panel(
    ui: &mut egui::Ui,
    history: &TraceHistory,
    target: &TargetSpace,
    caps: Option<&DeviceCapabilities>,
    show_raw_overlay: &mut bool,
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.heading("trace");
            ui.add_space(8.0);
            ui.checkbox(show_raw_overlay, "raw overlay");
            ui.add_space(8.0);
            ui.weak(format!("{} pts", history.len()));
        });

        let available = ui.available_size_before_wrap();
        let desired = Vec2::new(available.x.max(240.0), available.y.max(TRACE_MIN_HEIGHT));
        let (rect, response) = ui.allocate_exact_size(desired, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        painter.rect_stroke(
            rect,
            0.0,
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Inside,
        );

        let bounds = target_bounds(target, caps);
        draw_grid(
            &painter,
            rect,
            bounds,
            ui.visuals().widgets.noninteractive.bg_stroke.color,
        );

        if history.is_empty() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "waiting for samples",
                FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
            return;
        }

        if *show_raw_overlay {
            draw_path(
                &painter,
                history
                    .iter()
                    .map(|p| (p.raw_mapped, p.processed.active, 0.25)),
                bounds,
                rect,
                Color32::from_rgb(130, 145, 155),
                true,
            );
        }

        draw_path(
            &painter,
            history.iter().map(|p| {
                (
                    p.processed_point(),
                    p.processed.active,
                    p.processed.pressure,
                )
            }),
            bounds,
            rect,
            Color32::from_rgb(80, 190, 255),
            false,
        );

        if let Some(latest) = history.latest() {
            draw_latest(ui, &painter, latest, bounds, rect, response.hover_pos());
        }
    });
}

fn draw_grid(painter: &egui::Painter, rect: Rect, bounds: WorldBounds, color: Color32) {
    let grid_color = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 55);
    for i in 0..=4 {
        let t = f64::from(i) / 4.0;
        let x = bounds.min.x + bounds.width() * t;
        let a = world_to_panel(WorldPoint::new(x, bounds.min.y), bounds, rect);
        let b = world_to_panel(WorldPoint::new(x, bounds.max.y), bounds, rect);
        painter.line_segment([a, b], Stroke::new(1.0, grid_color));

        let y = bounds.min.y + bounds.height() * t;
        let a = world_to_panel(WorldPoint::new(bounds.min.x, y), bounds, rect);
        let b = world_to_panel(WorldPoint::new(bounds.max.x, y), bounds, rect);
        painter.line_segment([a, b], Stroke::new(1.0, grid_color));
    }
}

fn draw_path<I>(
    painter: &egui::Painter,
    points: I,
    bounds: WorldBounds,
    rect: Rect,
    base_color: Color32,
    force_thin: bool,
) where
    I: IntoIterator<Item = (WorldPoint, bool, f64)>,
{
    let points: Vec<_> = points.into_iter().collect();
    let segment_count = points.len().saturating_sub(1).max(1);
    for (idx, pair) in points.windows(2).enumerate() {
        let (a_world, a_active, a_pressure) = pair[0];
        let (b_world, b_active, b_pressure) = pair[1];
        let age = (idx + 1) as f32 / segment_count as f32;
        let alpha = (35.0 + 220.0 * age).round() as u8;
        let color =
            Color32::from_rgba_unmultiplied(base_color.r(), base_color.g(), base_color.b(), alpha);
        let pressure = ((a_pressure + b_pressure) * 0.5).clamp(0.0, 1.0) as f32;
        let active = a_active && b_active;
        let width = if force_thin || !active {
            1.0
        } else {
            1.25 + pressure * 5.0
        };
        let a = world_to_panel(a_world, bounds, rect);
        let b = world_to_panel(b_world, bounds, rect);
        if active && !force_thin {
            painter.line_segment([a, b], Stroke::new(width, color));
        } else {
            dashed_segment(painter, a, b, Stroke::new(width, color));
        }
    }
}

fn draw_latest(
    ui: &egui::Ui,
    painter: &egui::Painter,
    latest: &TracePoint,
    bounds: WorldBounds,
    rect: Rect,
    hover_pos: Option<Pos2>,
) {
    let point = world_to_panel(latest.processed_point(), bounds, rect);
    let cross = Color32::from_rgba_unmultiplied(245, 245, 245, 180);
    painter.line_segment(
        [
            Pos2::new(rect.left(), point.y),
            Pos2::new(rect.right(), point.y),
        ],
        Stroke::new(1.0, cross),
    );
    painter.line_segment(
        [
            Pos2::new(point.x, rect.top()),
            Pos2::new(point.x, rect.bottom()),
        ],
        Stroke::new(1.0, cross),
    );
    painter.circle_filled(point, 3.5, Color32::WHITE);

    if let (Some(tx), Some(ty)) = (latest.processed.tilt_x, latest.processed.tilt_y) {
        let tilt = Vec2::new(tx as f32, ty as f32);
        let len = tilt.length();
        if len > f32::EPSILON {
            let vector = tilt / len * (12.0 + len.min(60.0) * 0.45);
            painter.line_segment(
                [point, point + vector],
                Stroke::new(2.0, Color32::from_rgb(255, 214, 102)),
            );
        }
    } else if let (Some(azimuth), Some(altitude)) = (
        latest.processed.raw.azimuth_deci_deg,
        latest.processed.raw.altitude_deci_deg,
    ) {
        let radians = (azimuth as f32 / 10.0).to_radians();
        let altitude_degrees = altitude as f32 / 10.0;
        let len = 10.0 + ((90.0 - altitude_degrees).clamp(0.0, 90.0) / 90.0) * 28.0;
        let vector = Vec2::angled(radians) * len;
        painter.line_segment(
            [point, point + vector],
            Stroke::new(2.0, Color32::from_rgb(255, 214, 102)),
        );
    }

    if let Some(twist) = latest.processed.twist.or_else(|| {
        latest
            .processed
            .raw
            .rotation_deci_deg
            .map(|rotation| rotation as f64 / 10.0)
    }) {
        let center = point + Vec2::new(18.0, -18.0);
        let radius = 9.0;
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.0, Color32::from_rgb(200, 210, 220)),
        );
        let hand = Vec2::angled((twist as f32).to_radians()) * radius;
        painter.line_segment([center, center + hand], Stroke::new(1.5, Color32::WHITE));
    }

    let raw = latest.processed.raw;
    let text = format!(
        "raw: {}, {}  p:{}\ntarget: {:.3}, {:.3}  p:{:.2}",
        raw.x_raw,
        raw.y_raw,
        raw.pressure_raw,
        latest.processed.x,
        latest.processed.y,
        latest.processed.pressure
    );
    let anchor = hover_pos
        .map(|pos| pos + Vec2::new(12.0, 12.0))
        .unwrap_or_else(|| rect.left_top() + Vec2::splat(12.0));
    painter.text(
        anchor,
        Align2::LEFT_TOP,
        text,
        FontId::monospace(12.0),
        ui.visuals().text_color(),
    );
}

fn dashed_segment(painter: &egui::Painter, a: Pos2, b: Pos2, stroke: Stroke) {
    let delta = b - a;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = delta / length;
    let dash = 7.0;
    let gap = 5.0;
    let mut offset = 0.0;
    while offset < length {
        let end = (offset + dash).min(length);
        painter.line_segment([a + direction * offset, a + direction * end], stroke);
        offset += dash + gap;
    }
}

fn raw_axis_to_mm(raw: f64, axis: &AxisInfo) -> f64 {
    if axis.resolution <= 0.0 {
        return raw;
    }
    match axis.unit {
        AxisUnit::Inch => raw / axis.resolution * 25.4,
        AxisUnit::Centimeter => raw / axis.resolution * 10.0,
        AxisUnit::Degree | AxisUnit::None => raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::{AxisInfo, AxisUnit, DeviceCapabilities, ToolKind};

    fn sample(serial: u32) -> ProcessedSample {
        ProcessedSample {
            raw: PenSample {
                t_capture_ns: u64::from(serial),
                t_device_ms: serial,
                serial,
                x_raw: serial as i32,
                y_raw: serial as i32,
                z_raw: 0,
                x_norm: f64::from(serial),
                y_norm: f64::from(serial),
                pressure_raw: serial,
                pressure_norm: 0.5,
                tangent_pressure_raw: None,
                azimuth_deci_deg: None,
                altitude_deci_deg: None,
                twist_deci_deg: None,
                tilt_x_deg: None,
                tilt_y_deg: None,
                rotation_deci_deg: None,
                buttons: 0,
                tool: ToolKind::Pen,
                tool_serial: 0,
                in_proximity: true,
                status: 0,
            },
            x: f64::from(serial),
            y: f64::from(serial),
            x_filtered: None,
            y_filtered: None,
            pressure: 0.5,
            active: true,
            tilt_x: None,
            tilt_y: None,
            twist: None,
            out_of_range: false,
        }
    }

    fn caps() -> DeviceCapabilities {
        let axis = AxisInfo {
            min: 0,
            max: 1000,
            resolution: 100.0,
            unit: AxisUnit::Inch,
            supported: true,
        };
        DeviceCapabilities {
            device_name: "test".to_owned(),
            driver_version: "1".to_owned(),
            x: axis,
            y: axis,
            z: axis,
            pressure: axis,
            tangent_pressure: axis,
            azimuth: axis,
            altitude: axis,
            twist: axis,
            rotation: axis,
            max_packet_rate_hz: 200,
            queue_size: 128,
        }
    }

    #[test]
    fn trace_history_evicts_oldest_at_capacity() {
        let mut history = TraceHistory::new(3);
        for serial in 1..=5 {
            history.append(TracePoint {
                processed: sample(serial),
                raw_mapped: WorldPoint::new(f64::from(serial), f64::from(serial)),
            });
        }

        let serials: Vec<_> = history
            .iter()
            .map(|point| point.processed.raw.serial)
            .collect();
        assert_eq!(serials, vec![3, 4, 5]);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn world_to_panel_scales_and_centers_preserving_aspect() {
        let bounds = WorldBounds::new(WorldPoint::new(0.0, 0.0), WorldPoint::new(100.0, 50.0));
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(220.0, 120.0));

        let top_left = world_to_panel(WorldPoint::new(0.0, 0.0), bounds, rect);
        let bottom_right = world_to_panel(WorldPoint::new(100.0, 50.0), bounds, rect);

        assert!((top_left.x - 18.0).abs() < 1e-5, "{top_left:?}");
        assert!((top_left.y - 14.0).abs() < 1e-5, "{top_left:?}");
        assert!((bottom_right.x - 202.0).abs() < 1e-5, "{bottom_right:?}");
        assert!((bottom_right.y - 106.0).abs() < 1e-5, "{bottom_right:?}");
    }

    #[test]
    fn raw_mapped_screen_pixels_uses_normalized_sample() {
        let mut raw = sample(1).raw;
        raw.x_norm = 0.25;
        raw.y_norm = 0.5;
        let point = raw_mapped_point(&raw, &caps(), &TargetSpace::ScreenPixels { w: 800, h: 600 });

        assert_eq!(point, WorldPoint::new(200.0, 300.0));
    }
}
