use std::{
    sync::{
        atomic::{AtomicU32, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use tablet_stream::Metrics;

#[derive(Debug, Clone)]
pub struct MetricsCounters {
    pub packets_processed: Arc<AtomicU64>,
    pub dropped: Arc<AtomicU64>,
    pub queue_depth: Arc<AtomicU32>,
}

impl MetricsCounters {
    pub fn new(dropped: Arc<AtomicU64>) -> Self {
        Self {
            packets_processed: Arc::new(AtomicU64::new(0)),
            dropped,
            queue_depth: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn increment_packets_processed(&self) {
        self.packets_processed.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increment_queue_depth(&self) {
        let _ = self
            .queue_depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |depth| {
                Some(depth.saturating_add(1))
            });
    }

    pub fn decrement_queue_depth(&self) {
        let _ = self
            .queue_depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |depth| {
                Some(depth.saturating_sub(1))
            });
    }

    pub fn queue_depth(&self) -> u32 {
        self.queue_depth.load(Ordering::Relaxed)
    }
}

#[derive(Debug)]
pub struct MetricsEmitter {
    counters: MetricsCounters,
    interval: Duration,
    last_emit: Instant,
    last_packets_processed: u64,
    last_dropped: u64,
    emitted: bool,
}

impl MetricsEmitter {
    pub fn new(counters: MetricsCounters, interval: Duration, now: Instant) -> Self {
        let last_packets_processed = counters.packets_processed.load(Ordering::Relaxed);
        let last_dropped = counters.dropped.load(Ordering::Relaxed);
        Self {
            counters,
            interval,
            last_emit: now,
            last_packets_processed,
            last_dropped,
            emitted: false,
        }
    }

    pub fn due(&self, now: Instant) -> bool {
        now.duration_since(self.last_emit) >= self.interval
    }

    pub fn has_emitted(&self) -> bool {
        self.emitted
    }

    pub fn dropped_since_last_emit(&self) -> u64 {
        self.counters
            .dropped
            .load(Ordering::Relaxed)
            .saturating_sub(self.last_dropped)
    }

    pub fn snapshot(
        &mut self,
        now: Instant,
        actual_rate_hz: u32,
        requested_rate_hz: u32,
        connected_clients: u32,
    ) -> Metrics {
        let packets = self.counters.packets_processed.load(Ordering::Relaxed);
        let dropped = self.counters.dropped.load(Ordering::Relaxed);
        let queue_depth = self.counters.queue_depth.load(Ordering::Relaxed);
        let elapsed = now.duration_since(self.last_emit);
        let packets_per_sec =
            packets_per_second(packets.saturating_sub(self.last_packets_processed), elapsed);

        self.last_emit = now;
        self.last_packets_processed = packets;
        self.last_dropped = dropped;
        self.emitted = true;

        Metrics {
            packets_per_sec,
            dropped,
            queue_depth,
            actual_rate_hz,
            requested_rate_hz,
            connected_clients,
        }
    }
}

pub fn packets_per_second(packet_delta: u64, elapsed: Duration) -> f64 {
    let seconds = elapsed.as_secs_f64();
    if seconds > 0.0 {
        packet_delta as f64 / seconds
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tablet_stream::{encode_payload, Format, StreamMessage, KIND_METRICS};

    #[test]
    fn packets_per_second_uses_elapsed_time() {
        let rate = packets_per_second(50, Duration::from_millis(250));
        assert!((rate - 200.0).abs() < f64::EPSILON);
    }

    #[test]
    fn snapshot_uses_deltas_and_current_counters() {
        let dropped = Arc::new(AtomicU64::new(0));
        let counters = MetricsCounters::new(Arc::clone(&dropped));
        let start = Instant::now();
        let mut emitter = MetricsEmitter::new(counters.clone(), Duration::from_secs(1), start);

        counters.packets_processed.store(25, Ordering::Relaxed);
        counters.queue_depth.store(7, Ordering::Relaxed);
        dropped.store(3, Ordering::Relaxed);

        let metrics = emitter.snapshot(start + Duration::from_millis(500), 190, 200, 2);

        assert_eq!(metrics.dropped, 3);
        assert_eq!(metrics.queue_depth, 7);
        assert_eq!(metrics.actual_rate_hz, 190);
        assert_eq!(metrics.requested_rate_hz, 200);
        assert_eq!(metrics.connected_clients, 2);
        assert!((metrics.packets_per_sec - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn metrics_snapshot_encodes_as_metrics_frame_kind() {
        let metrics = Metrics {
            packets_per_sec: 120.0,
            dropped: 1,
            queue_depth: 2,
            actual_rate_hz: 190,
            requested_rate_hz: 200,
            connected_clients: 1,
        };

        let (kind, payload) =
            encode_payload(&StreamMessage::Metrics(metrics), Format::Postcard).unwrap();

        assert_eq!(kind, KIND_METRICS);
        assert!(!payload.is_empty());
    }
}
