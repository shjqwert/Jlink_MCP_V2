//! Capture storage and deterministic queries for J-Link MCP V2.

mod changes;
mod cursor;
mod query;
mod store;
mod window;

pub use changes::{CaptureChange, CaptureChanges, CaptureChangesQuery, CaptureRuleMatch, changes};
pub use cursor::{CaptureCursor, decode_cursor, encode_cursor};
pub use query::{CaptureOverview, CaptureOverviewQuality, CaptureVariableOverview, overview};
pub use store::{
    CaptureEstimate, CapturePhase, CaptureRecovery, CaptureSnapshot, CaptureStore, CaptureWriter,
    DEFAULT_CAPTURE_MAX_BYTES,
};
pub use window::{
    CaptureAroundEvent, CaptureAroundEventWindow, CaptureClock, CaptureEvent,
    CaptureEventChangeRelation, CaptureEventKind, CaptureEventOutcome, CaptureEventTime,
    CaptureTimeRelation, CaptureWindow, CaptureWindowBucket, CaptureWindowBuckets,
    CaptureWindowMode, CaptureWindowQuery, CaptureWindowRows, around_event, around_event_page,
    capture_events, event_change_relations, event_sample_relation, window,
};
