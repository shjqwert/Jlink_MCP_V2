//! Capture storage and deterministic queries for J-Link MCP V2.

mod query;
mod store;

pub use query::{CaptureOverview, CaptureOverviewQuality, CaptureVariableOverview, overview};
pub use store::{
    CaptureEstimate, CapturePhase, CaptureRecovery, CaptureSnapshot, CaptureStore, CaptureWriter,
    DEFAULT_CAPTURE_MAX_BYTES,
};
