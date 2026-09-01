use std::collections::BTreeMap;

use jlink_domain::{
    ErrorCode, HssDataIntegrity, HssEvidenceKind, HssQualityEvidence, HssQualitySummary,
    HssRunState, JlinkError, normalize_hss_timestamp_us,
};
use serde::Serialize;

use crate::CaptureSnapshot;

/// Low-redundancy navigation counts for one top-level HSS selector.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureVariableOverview {
    /// Stable short identifier used by subsequent query views.
    pub series: String,
    /// Whether values are typed by DWARF or only by explicit raw-address metadata.
    pub evidence: HssEvidenceKind,
    /// Number of complete persisted samples.
    pub samples: u64,
    /// Number of adjacent samples whose selected top-level bytes differ.
    pub changes: u64,
}

/// Quality facts emitted only when integrity is limited or evidence is abnormal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureOverviewQuality {
    /// Integrity assessed independently from capture lifecycle.
    pub integrity: HssDataIntegrity,
    /// Persisted acquisition evidence without empty event categories.
    #[serde(flatten)]
    pub acquisition: HssQualitySummary,
}

/// Deterministic overview derived from one verified immutable capture snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureOverview {
    /// Stable capture identity.
    pub capture_id: String,
    /// Inclusive beginning of the available source-time range.
    pub from_us: u64,
    /// Exclusive end of the available source-time range.
    pub to_us: u64,
    /// First-use mapping from stable short IDs to complete DWARF paths.
    pub dictionary: BTreeMap<String, String>,
    /// Top-level navigation counts in submitted selector order.
    pub variables: Vec<CaptureVariableOverview>,
    /// Persisted write, recovery, and quality event occurrence count.
    pub events: u64,
    /// Present only for abnormal or explicitly limited quality evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<CaptureOverviewQuality>,
}

/// Builds a low-redundancy overview from a fully verified immutable capture.
///
/// # Errors
///
/// Returns [`ErrorCode::FrameInvalid`] when persisted payload boundaries,
/// sample counts, source-time evidence, or variable slices disagree with the
/// capture's self-description.
pub fn overview(snapshot: &CaptureSnapshot) -> Result<CaptureOverview, JlinkError> {
    if snapshot.status().state != HssRunState::Completed {
        return Err(JlinkError::new(
            ErrorCode::OperationConflict,
            "overview 只接受生命周期为 completed 的不可变 capture；请先查询 status",
            false,
        )
        .with_detail("capture_id", serde_json::json!(snapshot.capture_id()))
        .with_detail("state", serde_json::json!(snapshot.status().state)));
    }
    let payload = snapshot.read_verified_payload()?;
    let batch = snapshot.plan().frame_layout().parse(&payload)?;
    if !batch.incomplete_tail.is_empty() {
        return Err(query_invalid("完成 capture 包含非完整 HSS 记录尾部"));
    }

    let samples = u64::try_from(batch.frames.len())
        .map_err(|_| query_invalid("完成 capture 样本数量无法表示为 u64"))?;
    let status = snapshot.status();
    if samples != status.complete_records || samples != status.quality.actual_samples {
        return Err(query_invalid("完成 capture 的样本计数与终态清单不一致")
            .with_detail("payload_samples", serde_json::json!(samples))
            .with_detail(
                "complete_records",
                serde_json::json!(status.complete_records),
            )
            .with_detail(
                "quality_actual_samples",
                serde_json::json!(status.quality.actual_samples),
            ));
    }

    let (from_us, to_us) = capture_range(snapshot, &batch.frames)?;
    let mut dictionary = BTreeMap::new();
    let mut variables = Vec::with_capacity(snapshot.plan().variables().len());
    for (index, variable) in snapshot.plan().variables().iter().enumerate() {
        let series = format!("s{index}");
        dictionary.insert(series.clone(), variable.series_label());
        let offset = usize::try_from(variable.sample_offset())
            .map_err(|_| query_invalid("HSS 变量 sample_offset 无法表示为 usize"))?;
        let size = usize::try_from(variable.byte_size())
            .map_err(|_| query_invalid("HSS 变量 byte_size 无法表示为 usize"))?;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| query_invalid("HSS 变量样本范围溢出"))?;
        let mut changes = 0_u64;
        for pair in batch.frames.windows(2) {
            let before = pair[0]
                .sample
                .get(offset..end)
                .ok_or_else(|| query_invalid("HSS 变量范围超出前一完整样本"))?;
            let after = pair[1]
                .sample
                .get(offset..end)
                .ok_or_else(|| query_invalid("HSS 变量范围超出后一完整样本"))?;
            if before != after {
                changes = changes
                    .checked_add(1)
                    .ok_or_else(|| query_invalid("HSS 变量变化计数溢出"))?;
            }
        }
        if let Some(frame) = batch.frames.first() {
            frame
                .sample
                .get(offset..end)
                .ok_or_else(|| query_invalid("HSS 变量范围超出完整样本"))?;
        }
        variables.push(CaptureVariableOverview {
            series,
            evidence: variable.evidence_kind(),
            samples,
            changes,
        });
    }

    Ok(CaptureOverview {
        capture_id: snapshot.capture_id().to_owned(),
        from_us,
        to_us,
        dictionary,
        variables,
        events: event_count(snapshot)?,
        quality: quality_for_overview(snapshot),
    })
}

fn capture_range(
    snapshot: &CaptureSnapshot,
    frames: &[jlink_domain::HssRawFrame<'_>],
) -> Result<(u64, u64), JlinkError> {
    let clock = &snapshot.status().quality.clock;
    let range = match (frames.first(), frames.last()) {
        (None, None) => (0, 0),
        (Some(first), Some(last)) => {
            let from_us = normalize_hss_timestamp_us(first.timestamp_raw);
            let last_us = normalize_hss_timestamp_us(last.timestamp_raw);
            let to_us = last_us
                .checked_add(u64::from(clock.source_resolution_us))
                .ok_or_else(|| query_invalid("HSS 采集结束边界溢出"))?;
            (from_us, to_us)
        }
        _ => unreachable!("a frame slice cannot have only one boundary"),
    };
    if clock.first_timestamp_us
        != frames
            .first()
            .map(|frame| normalize_hss_timestamp_us(frame.timestamp_raw))
        || clock.last_timestamp_us
            != frames
                .last()
                .map(|frame| normalize_hss_timestamp_us(frame.timestamp_raw))
    {
        return Err(query_invalid("HSS payload 时间边界与终态质量证据不一致"));
    }
    Ok(range)
}

fn event_count(snapshot: &CaptureSnapshot) -> Result<u64, JlinkError> {
    let quality_events =
        snapshot
            .status()
            .quality
            .events
            .iter()
            .try_fold(0_u64, |count, event| {
                count
                    .checked_add(event.occurrences)
                    .ok_or_else(|| query_invalid("HSS 质量事件计数溢出"))
            })?;
    u64::try_from(snapshot.status().writes.len())
        .ok()
        .and_then(|writes| quality_events.checked_add(writes))
        .and_then(|count| {
            u64::try_from(snapshot.status().recovery_notifications.len())
                .ok()
                .and_then(|recoveries| count.checked_add(recoveries))
        })
        .ok_or_else(|| query_invalid("HSS 时间线事件计数溢出"))
}

fn quality_for_overview(snapshot: &CaptureSnapshot) -> Option<CaptureOverviewQuality> {
    let quality = &snapshot.status().quality;
    let requested_rate_millihz = u64::from(quality.requested_rate_hz).saturating_mul(1_000);
    let abnormal_or_limited = snapshot.status().integrity != HssDataIntegrity::Complete
        || quality.actual_samples != quality.expected_samples
        || quality.actual_rate_millihz != Some(requested_rate_millihz)
        || quality.intervals.collisions > 0
        || quality.intervals.gap_events > 0
        || quality.intervals.gap_slots > 0
        || quality.intervals.regressions > 0
        || !quality.events.is_empty()
        || match quality.loss.evidence {
            HssQualityEvidence::Confirmed => quality.loss.lost_samples != Some(0),
            HssQualityEvidence::Suspected | HssQualityEvidence::Unknown => true,
        }
        || match quality.overflow.evidence {
            HssQualityEvidence::Confirmed => quality.overflow.events != Some(0),
            HssQualityEvidence::Suspected | HssQualityEvidence::Unknown => true,
        };
    abnormal_or_limited.then(|| CaptureOverviewQuality {
        integrity: snapshot.status().integrity,
        acquisition: quality.clone(),
    })
}

fn query_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::FrameInvalid, message, false)
}
