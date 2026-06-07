use std::{collections::VecDeque, time::Instant};

use eframe::egui;
use egui_plot::{Bar, BarChart, Plot};
use tablet_core::{PenSample, ToolKind};
use tablet_stream::Metrics;

use crate::{panels::trace::TraceHistory, source::SourceState};

const JITTER_BUCKETS: usize = 24;

#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub min: f64,
    pub max: f64,
    pub counts: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EventLogEntry {
    Proximity {
        in_range: bool,
        tool_serial: u64,
    },
    ToolChanged {
        serial: u32,
        from: ToolKind,
        to: ToolKind,
        tool_serial: u64,
    },
}

impl EventLogEntry {
    pub fn label(&self) -> String {
        match self {
            Self::Proximity {
                in_range,
                tool_serial,
            } => {
                let state = if *in_range { "in" } else { "out" };
                format!("proximity {state}  tool_serial={tool_serial}")
            }
            Self::ToolChanged {
                serial,
                from,
                to,
                tool_serial,
            } => format!(
                "tool change @ serial {serial}: {from:?} -> {to:?}  tool_serial={tool_serial}"
            ),
        }
    }
}

pub fn jitter_deltas_ms(samples: &[PenSample]) -> Vec<f64> {
    samples
        .windows(2)
        .filter_map(|pair| {
            let delta = pair[1].t_capture_ns.checked_sub(pair[0].t_capture_ns)?;
            Some(delta as f64 / 1_000_000.0)
        })
        .collect()
}

pub fn histogram(values: &[f64], bucket_count: usize) -> Histogram {
    let bucket_count = bucket_count.max(1);
    let finite: Vec<_> = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect();
    if finite.is_empty() {
        return Histogram {
            min: 0.0,
            max: 1.0,
            counts: vec![0; bucket_count],
        };
    }

    let min = finite.iter().copied().fold(f64::INFINITY, f64::min);
    let max = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut counts = vec![0; bucket_count];

    if (max - min).abs() <= f64::EPSILON {
        counts[0] = finite.len();
        return Histogram { min, max, counts };
    }

    let width = (max - min) / bucket_count as f64;
    for value in finite {
        let bucket = ((value - min) / width).floor() as usize;
        counts[bucket.min(bucket_count - 1)] += 1;
    }

    Histogram { min, max, counts }
}

pub fn detect_tool_change(
    previous: Option<&PenSample>,
    current: &PenSample,
) -> Option<EventLogEntry> {
    let previous = previous?;
    (previous.tool != current.tool).then_some(EventLogEntry::ToolChanged {
        serial: current.serial,
        from: previous.tool,
        to: current.tool,
        tool_serial: current.tool_serial,
    })
}

pub fn proximity_event(in_range: bool, tool_serial: u64) -> EventLogEntry {
    EventLogEntry::Proximity {
        in_range,
        tool_serial,
    }
}

pub fn draw_telemetry_panel(
    ui: &mut egui::Ui,
    metrics: Option<&Metrics>,
    source_state: &SourceState,
    history: &TraceHistory,
    event_log: &VecDeque<EventLogEntry>,
    latest_sample_received_at: Option<Instant>,
) {
    ui.vertical(|ui| {
        ui.heading("telemetry");

        egui::Grid::new("telemetry_values")
            .num_columns(2)
            .spacing([16.0, 4.0])
            .striped(true)
            .show(ui, |ui| {
                if let Some(metrics) = metrics {
                    value_row(ui, "packets/s", format!("{:.2}", metrics.packets_per_sec));
                    value_row(ui, "producer dropped", metrics.dropped.to_string());
                    value_row(ui, "queue depth", metrics.queue_depth.to_string());
                    value_row(
                        ui,
                        "rate actual/requested",
                        format!(
                            "{} / {} Hz",
                            metrics.actual_rate_hz, metrics.requested_rate_hz
                        ),
                    );
                    value_row(
                        ui,
                        "connected clients",
                        metrics.connected_clients.to_string(),
                    );
                } else {
                    value_row(ui, "metrics", "waiting".to_owned());
                }

                value_row(ui, "serial gaps", source_state.serial_gaps.to_string());
                value_row(
                    ui,
                    "display dropped",
                    source_state.display_dropped.to_string(),
                );
                value_row(
                    ui,
                    "latency proxy",
                    latest_sample_received_at
                        .map(|instant| {
                            format!(
                                "{:.1} ms since UI receive",
                                instant.elapsed().as_secs_f64() * 1000.0
                            )
                        })
                        .unwrap_or_else(|| "waiting".to_owned()),
                );
            });

        ui.separator();
        draw_jitter_histogram(ui, history);

        ui.separator();
        ui.label("events");
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .max_height(110.0)
            .show(ui, |ui| {
                if event_log.is_empty() {
                    ui.label("no events");
                } else {
                    for event in event_log {
                        ui.monospace(event.label());
                    }
                }
            });
    });
}

fn value_row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.monospace(value);
    ui.end_row();
}

fn draw_jitter_histogram(ui: &mut egui::Ui, history: &TraceHistory) {
    let samples: Vec<_> = history.iter().map(|point| point.processed.raw).collect();
    let deltas = jitter_deltas_ms(&samples);
    let histogram = histogram(&deltas, JITTER_BUCKETS);
    let span = (histogram.max - histogram.min).abs();
    let bucket_width = if span <= f64::EPSILON {
        1.0
    } else {
        span / histogram.counts.len() as f64
    };
    let bars: Vec<_> = histogram
        .counts
        .iter()
        .enumerate()
        .map(|(idx, count)| {
            let center = if span <= f64::EPSILON {
                histogram.min
            } else {
                histogram.min + (idx as f64 + 0.5) * bucket_width
            };
            Bar::new(center, *count as f64).width(bucket_width * 0.9)
        })
        .collect();

    ui.horizontal(|ui| {
        ui.label("jitter interval");
        ui.monospace(format!("{:.3}..{:.3} ms", histogram.min, histogram.max));
    });

    Plot::new("jitter_histogram")
        .height(95.0)
        .include_y(0.0)
        .show(ui, |plot_ui| {
            plot_ui.bar_chart(BarChart::new("capture delta ms", bars).width(bucket_width * 0.9));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_core::ToolKind;

    fn sample(serial: u32, t_capture_ns: u64, tool: ToolKind) -> PenSample {
        PenSample {
            t_capture_ns,
            t_device_ms: serial,
            serial,
            x_raw: 0,
            y_raw: 0,
            z_raw: 0,
            x_norm: 0.0,
            y_norm: 0.0,
            pressure_raw: 0,
            pressure_norm: 0.0,
            tangent_pressure_raw: None,
            azimuth_deci_deg: None,
            altitude_deci_deg: None,
            twist_deci_deg: None,
            tilt_x_deg: None,
            tilt_y_deg: None,
            rotation_deci_deg: None,
            buttons: 0,
            tool,
            tool_serial: 42,
            in_proximity: true,
            status: 0,
        }
    }

    #[test]
    fn jitter_deltas_use_consecutive_capture_timestamps() {
        let samples = [
            sample(1, 1_000, ToolKind::Pen),
            sample(2, 2_001_000, ToolKind::Pen),
            sample(3, 5_001_000, ToolKind::Pen),
        ];

        assert_eq!(jitter_deltas_ms(&samples), vec![2.0, 3.0]);
    }

    #[test]
    fn histogram_places_upper_edge_in_last_bucket() {
        let histogram = histogram(&[1.0, 2.0, 3.0], 2);

        assert_eq!(histogram.counts, vec![1, 2]);
    }

    #[test]
    fn tool_change_is_detected_between_samples() {
        let previous = sample(1, 1, ToolKind::Pen);
        let current = sample(2, 2, ToolKind::Eraser);

        assert_eq!(
            detect_tool_change(Some(&previous), &current),
            Some(EventLogEntry::ToolChanged {
                serial: 2,
                from: ToolKind::Pen,
                to: ToolKind::Eraser,
                tool_serial: 42,
            })
        );
    }

    #[test]
    fn unchanged_tool_does_not_emit_event() {
        let previous = sample(1, 1, ToolKind::Pen);
        let current = sample(2, 2, ToolKind::Pen);

        assert_eq!(detect_tool_change(Some(&previous), &current), None);
    }

    #[test]
    fn proximity_event_records_direction_and_tool_serial() {
        assert_eq!(
            proximity_event(false, 77),
            EventLogEntry::Proximity {
                in_range: false,
                tool_serial: 77,
            }
        );
    }
}
