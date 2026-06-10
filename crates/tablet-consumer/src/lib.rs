//! `tablet-consumer` — shared, OS-agnostic stream-ingestion infrastructure for
//! WCAP consumers.
//!
//! Reads the WCAP stream a `tablet-cli` producer emits (stdin pipe, TCP, or a
//! Windows named pipe) on a background reader thread, surfaces connection state
//! and stream health, and hands decoded [`StreamMessage`](tablet_stream::StreamMessage)s
//! to the UI thread through a bounded, drop-oldest display buffer.
//!
//! Extracted from `tablet-ui` so multiple consumer applications (the
//! calibration UI and the MPE MIDI HUD) share one reader implementation rather
//! than duplicating it. The crate has **no UI dependency**; it depends only on
//! `tablet-core` and `tablet-stream`.

pub mod source;
pub mod spawn;

pub use source::{
    handle_message, ingest_reader, ConnectionStatus, IngestState, SharedSource, Source,
    SourceHandle, SourceState,
};
pub use spawn::{spawn_producer, SpawnOptions, SpawnedProducer};
