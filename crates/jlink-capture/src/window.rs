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
    offset: usize,
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
            offset: 0,
        })
    }

    /// Sets the deterministic qualifying row or non-empty bucket offset.
    #[must_use]
    pub const fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
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

impl CaptureEventKind {
    /// Returns whether this event is acquisition-quality evidence.
    #[must_use]
    pub const fn is_quality(self) -> bool {
        matches!(
            self,
            Self::QualityBufferOverflow
                | Self::QualityShortFrame
                | Self::QualityFrameFormat
                | Self::QualitySampleInterval
                | Self::QualityClockRegression
        )
    }
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

/// Non-causal ordering relation between a host interval and a sample interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureTimeRelation {
    /// The complete uncertainty envelope ends before the sample interval.
    Before,
    /// The complete uncertainty envelope starts after the sample interval.
    After,
    /// The central host interval overlaps the sample interval.
    Overlaps,
    /// Mapping uncertainty prevents a reliable ordering conclusion.
    Indeterminate,
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
    /// Relation of this host event to the capture's complete sample range.
    pub sample_relation: CaptureTimeRelation,
    /// Persisted host-to-sample mapping uncertainty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping_uncertainty_us: Option<u64>,
}

/// One explicit non-causal relation between an event and a nearby sample change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEventChangeRelation {
    /// Stable event short ID.
    pub event: String,
    /// Stable changed leaf-series ID.
    pub series: String,
    /// Latest source time at which the prior value was observed.
    pub after_us: u64,
    /// First source time at which the changed value was observed.
    pub observed_by_us: u64,
    /// Conservative cross-clock relation, never a causality claim.
    pub relation: CaptureTimeRelation,
    /// Persisted mapping uncertainty used for the relation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping_uncertainty_us: Option<u64>,
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
    /// Explicit non-causal relations between the selected event and nearby changes.
    pub relations: Vec<CaptureEventChangeRelation>,
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
    series: Option<Vec<String>>,
    limit: usize,
) -> Result<CaptureAroundEvent, JlinkError> {
    around_event_page(snapshot, event_id, before_us, after_us, series, limit, 0)
}

/// Returns one cursor page for an event neighborhood.
///
/// # Errors
///
/// Returns the same stable errors as [`around_event`].
pub fn around_event_page(
    snapshot: &CaptureSnapshot,
    event_id: &str,
    before_us: u64,
    after_us: u64,
    series: Option<Vec<String>>,
    limit: usize,
    offset: usize,
) -> Result<CaptureAroundEvent, JlinkError> {
    require_completed(snapshot, "around_event")?;
    if !(1..=1_000).contains(&limit) {
        return Err(window_value_invalid("around_event.limit 必须为 1..1000"));
    }
    let event = capture_events(snapshot)
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
        &CaptureChangesQuery::new(series, Some(from_us), Some(to_us), Some(Vec::new()), limit)?
            .with_offset(offset),
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
    let relations = event_change_relations(
        &event,
        &nearby.changes,
        snapshot.status().quality.clock.mapping_error_us,
    );
    Ok(CaptureAroundEvent {
        event,
        window: CaptureAroundEventWindow { from_us, to_us },
        dictionary: nearby.dictionary,
        changes: nearby.changes,
        relations,
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
    let mut skipped = 0_usize;
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
            if skipped < query.offset {
                skipped += 1;
                previous = Some(decoded);
                continue;
            }
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
    let mut skipped = 0_usize;
    for (index, accumulator) in accumulators.into_iter().enumerate() {
        let Some(accumulator) = accumulator else {
            continue;
        };
        if skipped < query.offset {
            skipped += 1;
            continue;
        }
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

/// Returns all persisted device, quality, and recovery events in stable chronological order.
#[must_use]
pub fn capture_events(snapshot: &CaptureSnapshot) -> Vec<CaptureEvent> {
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
    let mapping_uncertainty_us = snapshot.status().quality.clock.mapping_error_us;
    let sample_range = capture_sample_range(snapshot).ok();
    pending
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let sample_relation =
                sample_range.map_or(CaptureTimeRelation::Indeterminate, |range| {
                    relate_host_to_sample(
                        item.start_us,
                        item.end_us,
                        range.0,
                        range.1,
                        mapping_uncertainty_us,
                    )
                });
            CaptureEvent {
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
                sample_relation,
                mapping_uncertainty_us,
            }
        })
        .collect()
}

/// Relates one immutable host-clock event to exact sample-change intervals.
#[must_use]
pub fn event_change_relations(
    event: &CaptureEvent,
    changes: &[CaptureChange],
    mapping_uncertainty_us: Option<u64>,
) -> Vec<CaptureEventChangeRelation> {
    changes
        .iter()
        .map(|change| CaptureEventChangeRelation {
            event: event.id.clone(),
            series: change.series.clone(),
            after_us: change.after_us,
            observed_by_us: change.observed_by_us,
            relation: relate_host_to_sample(
                event.start.us,
                event.end.us,
                change.after_us,
                change.observed_by_us,
                mapping_uncertainty_us,
            ),
            mapping_uncertainty_us,
        })
        .collect()
}

/// Relates one immutable event to an arbitrary sample-clock interval.
#[must_use]
pub const fn event_sample_relation(
    event: &CaptureEvent,
    sample_start_us: u64,
    sample_end_us: u64,
    mapping_uncertainty_us: Option<u64>,
) -> CaptureTimeRelation {
    relate_host_to_sample(
        event.start.us,
        event.end.us,
        sample_start_us,
        sample_end_us,
        mapping_uncertainty_us,
    )
}

const fn relate_host_to_sample(
    host_start_us: u64,
    host_end_us: u64,
    sample_start_us: u64,
    sample_end_us: u64,
    mapping_uncertainty_us: Option<u64>,
) -> CaptureTimeRelation {
    let Some(uncertainty) = mapping_uncertainty_us else {
        return CaptureTimeRelation::Indeterminate;
    };
    let earliest = host_start_us.saturating_sub(uncertainty);
    let latest = host_end_us.saturating_add(uncertainty);
    if latest < sample_start_us {
        CaptureTimeRelation::Before
    } else if earliest > sample_end_us {
        CaptureTimeRelation::After
    } else if host_end_us >= sample_start_us && host_start_us <= sample_end_us {
        CaptureTimeRelation::Overlaps
    } else {
        CaptureTimeRelation::Indeterminate
    }
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
