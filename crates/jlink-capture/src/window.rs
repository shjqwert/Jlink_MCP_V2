use std::{cmp::Ordering, collections::BTreeMap};

use jlink_domain::{
    ErrorCode, HssQualityEvent, HssQualityEventKind, HssRecoveryNotification, HssRunState,
    HssWriteKind, HssWriteResult, JlinkError, compare_numeric_typed_values,
    normalize_hss_timestamp_us,
};
use serde::Serialize;
use serde_json::Value;

use crate::{
    CaptureChange, CaptureChangesQuery, CaptureSnapshot, changes,
    changes::{
        SeriesDescriptor, decode_frame, normalize_series_selection, resolve_series, series_catalog,
        validate_complete_frames,
    },
};

/// Explicit projection selected by one immutable window query.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureWindowMode {
    /// Preserve every source row, including repeated values.
    Raw,
    /// Preserve only rows where at least one selected value changed.
    Transitions,
    /// Return minimum and maximum values for at most `points` fixed time buckets.
    MinMax {
        /// Number of fixed buckets spanning the requested range.
        points: usize,
    },
    /// Return first and last values for at most `points` fixed time buckets.
    FirstLast {
        /// Number of fixed buckets spanning the requested range.
        points: usize,
    },
}

/// Validated request for one deterministic sample-clock window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureWindowQuery {
    series: Vec<String>,
    from_us: u64,
    to_us: u64,
    mode: CaptureWindowMode,
    limit: usize,
}

impl CaptureWindowQuery {
    /// Validates leaf selection, half-open time range, explicit mode, and result limit.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCode::ValueInvalid`] for empty/duplicate series, an empty
    /// range, a limit outside 1..1000, or aggregate points outside 1..1000.
    pub fn new(
        series: Vec<String>,
        from_us: u64,
        to_us: u64,
        mode: CaptureWindowMode,
        limit: usize,
    ) -> Result<Self, JlinkError> {
        if from_us >= to_us {
            return Err(window_value_invalid(
                "window 时间范围必须满足 from_us < to_us",
            ));
        }
        if !(1..=1_000).contains(&limit) {
            return Err(window_value_invalid("window.limit 必须为 1..1000"));
        }
        if let CaptureWindowMode::MinMax { points } | CaptureWindowMode::FirstLast { points } = mode
            && !(1..=1_000).contains(&points)
        {
            return Err(window_value_invalid("window.points 必须为 1..1000"));
        }
        Ok(Self {
            series: normalize_series_selection(series)?,
            from_us,
            to_us,
            mode,
            limit,
        })
    }
}

/// Explicit clock domain attached to query rows and events.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureClock {
    /// Normalized source sample clock.
    Sample,
    /// Worker monotonic time since capture start.
    Host,
}

/// Complete rectangular rows returned by `raw` or `transitions` mode.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureWindowRows {
    /// Explicit sample clock domain.
    pub clock: CaptureClock,
    /// Stable series-to-leaf-path mapping.
    pub dictionary: BTreeMap<String, String>,
    /// Source timestamps in persisted record order.
    pub time_us: Vec<u64>,
    /// Complete selected values aligned with `time_us`.
    pub values: BTreeMap<String, Vec<Value>>,
    /// Whether more qualifying rows exist after this bounded result.
    pub truncated: bool,
}

/// One non-empty fixed time bucket for an explicit aggregate mode.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureWindowBucket {
    /// Inclusive bucket start in the sample clock.
    pub from_us: u64,
    /// Exclusive bucket end in the sample clock.
    pub to_us: u64,
    /// Per-series `[min,max]` or `[first,last]` pair.
    pub values: BTreeMap<String, [Value; 2]>,
}

/// Non-empty fixed buckets returned by an explicit aggregate mode.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureWindowBuckets {
    /// Explicit sample clock domain.
    pub clock: CaptureClock,
    /// Stable series-to-leaf-path mapping.
    pub dictionary: BTreeMap<String, String>,
    /// Non-empty buckets in requested-range order.
    pub buckets: Vec<CaptureWindowBucket>,
    /// Whether more non-empty buckets exist after this bounded result.
    pub truncated: bool,
}

/// Closed result shape selected only by the explicit window mode.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum CaptureWindow {
    /// Raw or transition rows.
    Rows(CaptureWindowRows),
    /// Explicit aggregation buckets.
    Buckets(CaptureWindowBuckets),
}

/// Stable kind retained for an immutable capture event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEventKind {
    /// A serialized target write of an intentionally generic retained kind.
    TargetWrite,
    /// Raw memory or MMIO write.
    MemoryWrite,
    /// DWARF-resolved typed variable write.
    VariableWrite,
    /// Confirmed or suspected target buffer overflow evidence.
    QualityBufferOverflow,
    /// Short DLL read evidence.
    QualityShortFrame,
    /// Non-integral frame read evidence.
    QualityFrameFormat,
    /// Source sample interval deviation evidence.
    QualitySampleInterval,
    /// Source clock regression evidence.
    QualityClockRegression,
    /// Internal Stop completed after an acquisition failure.
    RecoveryStopCompletedAfterFailure,
    /// Valid partial data was retained after failure.
    RecoveryPartialDataRetained,
    /// Startup recovery classified an interrupted capture as aborted.
    RecoveryAbortedCapture,
}

/// Stable outcome available only for target-write events.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEventOutcome {
    /// Target write and requested verification succeeded.
    Succeeded,
    /// Target write returned a stable error.
    Failed,
}

/// One event endpoint in an explicit clock domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEventTime {
    /// Host or sample clock domain.
    pub clock: CaptureClock,
    /// Microseconds since the corresponding capture clock origin.
    pub us: u64,
}

/// One immutable event selected by a stable short event ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEvent {
    /// Stable short ID assigned by chronological persisted order.
    pub id: String,
    /// Closed retained event category.
    pub kind: CaptureEventKind,
    /// Inclusive event start.
    pub start: CaptureEventTime,
    /// Inclusive event end.
    pub end: CaptureEventTime,
    /// Opaque IPC request correlation for target writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Target-write outcome when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CaptureEventOutcome>,
    /// Stable target-write failure code when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
}

/// Event, reusable sample-window bounds, nearby changes, and overlapping quality evidence.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureAroundEvent {
    /// Selected immutable event.
    pub event: CaptureEvent,
    /// Sample-clock bounds reusable with `window`.
    pub window: CaptureAroundEventWindow,
    /// First-use dictionary for nearby changed series.
    pub dictionary: BTreeMap<String, String>,
    /// Nearby exact changes without raw waveform duplication.
    pub changes: Vec<CaptureChange>,
    /// Quality evidence whose host interval overlaps the requested neighborhood.
    pub quality: Vec<HssQualityEvent>,
    /// Whether more nearby changes exist after this bounded result.
    pub truncated: bool,
}

/// Half-open sample bounds returned by `around_event` for reuse by `window`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureAroundEventWindow {
    /// Inclusive normalized source time.
    pub from_us: u64,
    /// Exclusive normalized source time.
    pub to_us: u64,
}

/// Reads one complete raw, transition, or explicit aggregate sample window.
///
/// # Errors
///
/// Returns a stable state, frame, path, type, or value error when the immutable
/// snapshot cannot satisfy the exact requested projection.
pub fn window(
    snapshot: &CaptureSnapshot,
    query: &CaptureWindowQuery,
) -> Result<CaptureWindow, JlinkError> {
    require_completed(snapshot, "window")?;
    let payload = snapshot.read_verified_payload()?;
    let batch = snapshot.plan().frame_layout().parse(&payload)?;
    validate_complete_frames(snapshot, &batch)?;
    let catalog = series_catalog(snapshot)?;
    let selected = resolve_series(&catalog, Some(&query.series))?;
    match query.mode {
        CaptureWindowMode::Raw | CaptureWindowMode::Transitions => Ok(CaptureWindow::Rows(
            collect_rows(snapshot, &batch.frames, &catalog, &selected, query)?,
        )),
        CaptureWindowMode::MinMax { points } => Ok(CaptureWindow::Buckets(collect_buckets(
            snapshot,
            &batch.frames,
            &catalog,
            &selected,
            query,
            points,
            true,
        )?)),
        CaptureWindowMode::FirstLast { points } => Ok(CaptureWindow::Buckets(collect_buckets(
            snapshot,
            &batch.frames,
            &catalog,
            &selected,
            query,
            points,
            false,
        )?)),
    }
}

/// Returns one event neighborhood and sample bounds reusable by `window`.
///
/// # Errors
///
/// Returns a stable state or value error when the event is absent, the capture
/// has no sample range, or its host-to-sample uncertainty is unavailable.
pub fn around_event(
    snapshot: &CaptureSnapshot,
    event_id: &str,
    before_us: u64,
    after_us: u64,
    limit: usize,
) -> Result<CaptureAroundEvent, JlinkError> {
    require_completed(snapshot, "around_event")?;
    if !(1..=1_000).contains(&limit) {
        return Err(window_value_invalid("around_event.limit 必须为 1..1000"));
    }
    let event = event_catalog(snapshot)
        .into_iter()
        .find(|candidate| candidate.id == event_id)
        .ok_or_else(|| {
            window_value_invalid("around_event.event_id 不存在于不可变 capture")
                .with_detail("event_id", serde_json::json!(event_id))
        })?;
    let mapping_error = snapshot
        .status()
        .quality
        .clock
        .mapping_error_us
        .ok_or_else(|| window_value_invalid("capture 缺少 host 到 sample 的映射误差边界"))?;
    let host_from = event.start.us.saturating_sub(before_us);
    let host_to = event.end.us.saturating_add(after_us);
    let (sample_first, sample_end) = capture_sample_range(snapshot)?;
    let from_us = host_from.saturating_sub(mapping_error).max(sample_first);
    let to_us = host_to
        .saturating_add(mapping_error)
        .saturating_add(u64::from(
            snapshot.status().quality.clock.source_resolution_us,
        ))
        .min(sample_end);
    if from_us >= to_us {
        return Err(window_value_invalid(
            "事件邻域与不可变 capture 的 sample 范围不相交",
        ));
    }
    let nearby = changes(
        snapshot,
        &CaptureChangesQuery::new(None, Some(from_us), Some(to_us), Some(Vec::new()), limit)?,
    )?;
    let quality = snapshot
        .status()
        .quality
        .events
        .iter()
        .filter(|item| {
            item.last_host_elapsed_us >= host_from && item.first_host_elapsed_us <= host_to
        })
        .cloned()
        .collect();
    Ok(CaptureAroundEvent {
        event,
        window: CaptureAroundEventWindow { from_us, to_us },
        dictionary: nearby.dictionary,
        changes: nearby.changes,
        quality,
        truncated: nearby.truncated,
    })
}

fn collect_rows(
    snapshot: &CaptureSnapshot,
    frames: &[jlink_domain::HssRawFrame<'_>],
    catalog: &[SeriesDescriptor],
    selected: &[usize],
    query: &CaptureWindowQuery,
) -> Result<CaptureWindowRows, JlinkError> {
    let mut time_us = Vec::new();
    let mut values = empty_values(catalog, selected);
    let mut previous: Option<Vec<Value>> = None;
    let mut truncated = false;
    for frame in frames {
        let decoded = decode_frame(snapshot.plan().variables(), frame.sample)?;
        let timestamp_us = normalize_hss_timestamp_us(frame.timestamp_raw);
        let in_range = timestamp_us >= query.from_us && timestamp_us < query.to_us;
        let transition = if let Some(prior) = &previous {
            let mut changed = false;
            for index in selected {
                if catalog[*index].value(prior)? != catalog[*index].value(&decoded)? {
                    changed = true;
                    break;
                }
            }
            changed
        } else {
            false
        };
        let include = in_range
            && match query.mode {
                CaptureWindowMode::Raw => true,
                CaptureWindowMode::Transitions => transition,
                CaptureWindowMode::MinMax { .. } | CaptureWindowMode::FirstLast { .. } => false,
            };
        if include {
            if time_us.len() == query.limit {
                truncated = true;
                break;
            }
            time_us.push(timestamp_us);
            append_selected_values(&mut values, catalog, selected, &decoded)?;
        }
        previous = Some(decoded);
    }
    Ok(CaptureWindowRows {
        clock: CaptureClock::Sample,
        dictionary: selected_dictionary(catalog, selected),
        time_us,
        values,
        truncated,
    })
}

fn collect_buckets(
    snapshot: &CaptureSnapshot,
    frames: &[jlink_domain::HssRawFrame<'_>],
    catalog: &[SeriesDescriptor],
    selected: &[usize],
    query: &CaptureWindowQuery,
    points: usize,
    min_max: bool,
) -> Result<CaptureWindowBuckets, JlinkError> {
    if min_max && selected.iter().any(|index| !catalog[*index].numeric) {
        return Err(window_value_invalid("window min_max 只能应用于数值叶序列"));
    }
    let mut accumulators: Vec<Option<BucketAccumulator>> = vec![None; points];
    for frame in frames {
        let timestamp_us = normalize_hss_timestamp_us(frame.timestamp_raw);
        if timestamp_us < query.from_us || timestamp_us >= query.to_us {
            continue;
        }
        let decoded = decode_frame(snapshot.plan().variables(), frame.sample)?;
        let index = bucket_index(timestamp_us, query.from_us, query.to_us, points)?;
        match &mut accumulators[index] {
            Some(accumulator) => accumulator.observe(catalog, selected, &decoded, min_max)?,
            slot @ None => {
                *slot = Some(BucketAccumulator::new(catalog, selected, &decoded)?);
            }
        }
    }
    let mut buckets = Vec::new();
    let mut truncated = false;
    for (index, accumulator) in accumulators.into_iter().enumerate() {
        let Some(accumulator) = accumulator else {
            continue;
        };
        if buckets.len() == query.limit {
            truncated = true;
            break;
        }
        let (from_us, to_us) = bucket_bounds(query.from_us, query.to_us, points, index)?;
        buckets.push(CaptureWindowBucket {
            from_us,
            to_us,
            values: accumulator.into_values(catalog, selected, min_max),
        });
    }
    Ok(CaptureWindowBuckets {
        clock: CaptureClock::Sample,
        dictionary: selected_dictionary(catalog, selected),
        buckets,
        truncated,
    })
}

#[derive(Clone, Debug)]
struct BucketAccumulator {
    first: Vec<Value>,
    last: Vec<Value>,
    minimum: Vec<Value>,
    maximum: Vec<Value>,
}

impl BucketAccumulator {
    fn new(
        catalog: &[SeriesDescriptor],
        selected: &[usize],
        decoded: &[Value],
    ) -> Result<Self, JlinkError> {
        let values = selected
            .iter()
            .map(|index| catalog[*index].value(decoded).cloned())
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            first: values.clone(),
            last: values.clone(),
            minimum: values.clone(),
            maximum: values,
        })
    }

    fn observe(
        &mut self,
        catalog: &[SeriesDescriptor],
        selected: &[usize],
        decoded: &[Value],
        min_max: bool,
    ) -> Result<(), JlinkError> {
        for (position, index) in selected.iter().enumerate() {
            let value = catalog[*index].value(decoded)?.clone();
            if min_max {
                if compare_numeric_typed_values(&value, &self.minimum[position])? == Ordering::Less
                {
                    self.minimum[position] = value.clone();
                }
                if compare_numeric_typed_values(&value, &self.maximum[position])?
                    == Ordering::Greater
                {
                    self.maximum[position] = value.clone();
                }
            }
            self.last[position] = value;
        }
        Ok(())
    }

    fn into_values(
        self,
        catalog: &[SeriesDescriptor],
        selected: &[usize],
        min_max: bool,
    ) -> BTreeMap<String, [Value; 2]> {
        selected
            .iter()
            .enumerate()
            .map(|(position, index)| {
                let pair = if min_max {
                    [
                        self.minimum[position].clone(),
                        self.maximum[position].clone(),
                    ]
                } else {
                    [self.first[position].clone(), self.last[position].clone()]
                };
                (catalog[*index].id.clone(), pair)
            })
            .collect()
    }
}

fn empty_values(catalog: &[SeriesDescriptor], selected: &[usize]) -> BTreeMap<String, Vec<Value>> {
    selected
        .iter()
        .map(|index| (catalog[*index].id.clone(), Vec::new()))
        .collect()
}

fn append_selected_values(
    values: &mut BTreeMap<String, Vec<Value>>,
    catalog: &[SeriesDescriptor],
    selected: &[usize],
    decoded: &[Value],
) -> Result<(), JlinkError> {
    for index in selected {
        values
            .get_mut(&catalog[*index].id)
            .expect("selected values map contains every requested series")
            .push(catalog[*index].value(decoded)?.clone());
    }
    Ok(())
}

fn selected_dictionary(
    catalog: &[SeriesDescriptor],
    selected: &[usize],
) -> BTreeMap<String, String> {
    selected
        .iter()
        .map(|index| (catalog[*index].id.clone(), catalog[*index].path.clone()))
        .collect()
}

fn bucket_index(
    timestamp_us: u64,
    from_us: u64,
    to_us: u64,
    points: usize,
) -> Result<usize, JlinkError> {
    let offset = u128::from(timestamp_us - from_us);
    let span = u128::from(to_us - from_us);
    let points = u128::try_from(points)
        .map_err(|_| window_value_invalid("window.points 无法表示为 u128"))?;
    usize::try_from(offset * points / span)
        .map_err(|_| window_value_invalid("window bucket 索引无法表示为 usize"))
}

fn bucket_bounds(
    from_us: u64,
    to_us: u64,
    points: usize,
    index: usize,
) -> Result<(u64, u64), JlinkError> {
    let span = u128::from(to_us - from_us);
    let points_u128 = u128::try_from(points)
        .map_err(|_| window_value_invalid("window.points 无法表示为 u128"))?;
    let index_u128 = u128::try_from(index)
        .map_err(|_| window_value_invalid("window bucket 索引无法表示为 u128"))?;
    let base = u128::from(from_us);
    let start = base + span * index_u128 / points_u128;
    let end = base + span * (index_u128 + 1) / points_u128;
    Ok((
        u64::try_from(start).map_err(|_| window_value_invalid("window bucket 起点溢出"))?,
        u64::try_from(end).map_err(|_| window_value_invalid("window bucket 终点溢出"))?,
    ))
}

fn event_catalog(snapshot: &CaptureSnapshot) -> Vec<CaptureEvent> {
    let mut pending = Vec::new();
    for (index, write) in snapshot.status().writes.iter().enumerate() {
        let (outcome, error_code) = match write.result {
            HssWriteResult::Succeeded => (CaptureEventOutcome::Succeeded, None),
            HssWriteResult::Failed { code } => (CaptureEventOutcome::Failed, Some(code)),
        };
        pending.push(PendingEvent {
            start_us: write.started_at_us,
            end_us: write.completed_at_us,
            rank: 0,
            source_index: index,
            kind: match write.kind {
                HssWriteKind::TargetWrite => CaptureEventKind::TargetWrite,
                HssWriteKind::MemoryWrite => CaptureEventKind::MemoryWrite,
                HssWriteKind::VariableWrite => CaptureEventKind::VariableWrite,
            },
            request_id: Some(write.request_id.clone()),
            outcome: Some(outcome),
            error_code,
        });
    }
    for (index, event) in snapshot.status().quality.events.iter().enumerate() {
        pending.push(PendingEvent {
            start_us: event.first_host_elapsed_us,
            end_us: event.last_host_elapsed_us,
            rank: 1,
            source_index: index,
            kind: quality_event_kind(event.kind),
            request_id: None,
            outcome: None,
            error_code: None,
        });
    }
    for (index, notification) in snapshot.status().recovery_notifications.iter().enumerate() {
        pending.push(PendingEvent {
            start_us: snapshot.status().elapsed_us,
            end_us: snapshot.status().elapsed_us,
            rank: 2,
            source_index: index,
            kind: recovery_event_kind(notification),
            request_id: None,
            outcome: None,
            error_code: None,
        });
    }
    pending.sort_by_key(|item| (item.start_us, item.end_us, item.rank, item.source_index));
    pending
        .into_iter()
        .enumerate()
        .map(|(index, item)| CaptureEvent {
            id: format!("e{index}"),
            kind: item.kind,
            start: CaptureEventTime {
                clock: CaptureClock::Host,
                us: item.start_us,
            },
            end: CaptureEventTime {
                clock: CaptureClock::Host,
                us: item.end_us,
            },
            request_id: item.request_id,
            outcome: item.outcome,
            error_code: item.error_code,
        })
        .collect()
}

struct PendingEvent {
    start_us: u64,
    end_us: u64,
    rank: u8,
    source_index: usize,
    kind: CaptureEventKind,
    request_id: Option<String>,
    outcome: Option<CaptureEventOutcome>,
    error_code: Option<ErrorCode>,
}

const fn quality_event_kind(kind: HssQualityEventKind) -> CaptureEventKind {
    match kind {
        HssQualityEventKind::BufferOverflow => CaptureEventKind::QualityBufferOverflow,
        HssQualityEventKind::ShortFrame => CaptureEventKind::QualityShortFrame,
        HssQualityEventKind::FrameFormat => CaptureEventKind::QualityFrameFormat,
        HssQualityEventKind::SampleInterval => CaptureEventKind::QualitySampleInterval,
        HssQualityEventKind::ClockRegression => CaptureEventKind::QualityClockRegression,
    }
}

const fn recovery_event_kind(notification: &HssRecoveryNotification) -> CaptureEventKind {
    match notification {
        HssRecoveryNotification::StopCompletedAfterFailure => {
            CaptureEventKind::RecoveryStopCompletedAfterFailure
        }
        HssRecoveryNotification::PartialDataRetained { .. } => {
            CaptureEventKind::RecoveryPartialDataRetained
        }
        HssRecoveryNotification::AbortedCaptureRecovered => {
            CaptureEventKind::RecoveryAbortedCapture
        }
    }
}

fn capture_sample_range(snapshot: &CaptureSnapshot) -> Result<(u64, u64), JlinkError> {
    let clock = &snapshot.status().quality.clock;
    let first = clock
        .first_timestamp_us
        .ok_or_else(|| window_value_invalid("capture 没有完整 sample 起点"))?;
    let last = clock
        .last_timestamp_us
        .ok_or_else(|| window_value_invalid("capture 没有完整 sample 终点"))?;
    Ok((
        first,
        last.saturating_add(u64::from(clock.source_resolution_us)),
    ))
}

fn require_completed(snapshot: &CaptureSnapshot, view: &str) -> Result<(), JlinkError> {
    if snapshot.status().state == HssRunState::Completed {
        Ok(())
    } else {
        Err(JlinkError::new(
            ErrorCode::OperationConflict,
            format!("{view} 只接受生命周期为 completed 的不可变 capture；请先查询 status"),
            false,
        ))
    }
}

fn window_value_invalid(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ValueInvalid, message, false)
}
