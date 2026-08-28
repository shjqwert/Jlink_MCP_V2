use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

use jlink_capture::{CapturePhase, CaptureRecovery, CaptureSnapshot, CaptureStore, CaptureWriter};
use jlink_domain::{
    ErrorCode, HssCaptureReservation, HssCaptureState, HssDrainTiming, HssQualitySummary,
    HssQualityTracker, HssRecoveryNotification, HssReservationOutcome, HssRunSnapshot, HssRunState,
    HssStartPlan, HssStartRegistry, HssWriteKind, HssWriteResult, HssWriteTiming, JlinkError,
    TargetConnectionSpec,
};
use serde_json::json;

use crate::gateway::DllGateway;

const READ_BUFFER_BYTES: usize = 64 * 1024;
const DRAIN_INTERVAL: Duration = Duration::from_millis(1);
const TAIL_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const TAIL_EMPTY_READS: u32 = 20;

pub(crate) trait HssIo {
    fn start_hss(&mut self, plan: &HssStartPlan) -> Result<(), JlinkError>;
    fn read_hss(
        &mut self,
        buffer: &mut [u8],
        record_bytes: usize,
    ) -> Result<HssReadOutcome, JlinkError>;
    fn stop_hss(&mut self) -> Result<(), JlinkError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HssReadOutcome {
    bytes: usize,
    overflow_confirmed: bool,
}

impl HssIo for DllGateway {
    fn start_hss(&mut self, plan: &HssStartPlan) -> Result<(), JlinkError> {
        Self::start_hss(self, plan)
    }

    fn read_hss(
        &mut self,
        buffer: &mut [u8],
        record_bytes: usize,
    ) -> Result<HssReadOutcome, JlinkError> {
        Self::read_hss(self, buffer, record_bytes).map(|bytes| HssReadOutcome {
            bytes,
            // Frozen 6.98a exposes no independent overflow signal or counter.
            overflow_confirmed: false,
        })
    }

    fn stop_hss(&mut self) -> Result<(), JlinkError> {
        Self::stop_hss(self)
    }
}

#[derive(Debug)]
pub(crate) struct HssStartOutcome {
    pub(crate) snapshot: HssRunSnapshot,
    pub(crate) started_new: bool,
}

pub(crate) struct HssWriteToken {
    index: usize,
}

struct ActiveCapture {
    reservation: HssCaptureReservation,
    plan: HssStartPlan,
    started: Instant,
    deadline: Instant,
    status: HssCaptureState,
    tail_started: Option<Instant>,
    consecutive_empty_tail_reads: u32,
    buffer: Vec<u8>,
    writer: Option<CaptureWriter>,
    incomplete_tail: Vec<u8>,
    complete_records: u64,
    drain: HssDrainTiming,
    quality: HssQualityTracker,
    writes: Vec<HssWriteTiming>,
    pending_write_impact: Option<usize>,
}

struct TerminalCapture {
    snapshot: HssRunSnapshot,
    _plan: Option<HssStartPlan>,
    _store: Option<CaptureSnapshot>,
    _failure: Option<JlinkError>,
}

/// Worker-owned fixed-duration scheduler; all methods run on the DLL thread.
pub(crate) struct HssCoordinator {
    store: CaptureStore,
    registry: HssStartRegistry,
    retired_keys: BTreeMap<String, String>,
    active: Option<ActiveCapture>,
    terminal: BTreeMap<String, TerminalCapture>,
}

impl HssCoordinator {
    pub(crate) fn open(root: impl Into<PathBuf>, probe_identity: &str) -> Result<Self, JlinkError> {
        let store = CaptureStore::open(root)?;
        let recoveries = store.recover_partials()?;
        let mut registry = HssStartRegistry::new();
        let mut retired_keys = BTreeMap::new();
        let mut terminal = BTreeMap::new();
        for snapshot in store.completed_snapshots()? {
            let reservation =
                registry.reserve(probe_identity, snapshot.target(), snapshot.plan())?;
            validate_recovered_capture_id(&reservation, snapshot.capture_id())?;
            retired_keys.insert(
                snapshot.plan().capture_key().to_owned(),
                snapshot.capture_id().to_owned(),
            );
            terminal.insert(
                snapshot.capture_id().to_owned(),
                TerminalCapture {
                    snapshot: snapshot.status().clone(),
                    _plan: Some(snapshot.plan().clone()),
                    _store: Some(snapshot),
                    _failure: None,
                },
            );
        }
        for recovery in recoveries {
            match recovery {
                CaptureRecovery::Published(_) => {}
                CaptureRecovery::Aborted {
                    capture_id,
                    plan,
                    target,
                    status,
                    ..
                } => {
                    match (&target, &plan) {
                        (Some(target), Some(plan)) => {
                            let reservation = registry.reserve(probe_identity, target, plan)?;
                            validate_recovered_capture_id(&reservation, &capture_id)?;
                            retired_keys.insert(plan.capture_key().to_owned(), capture_id.clone());
                        }
                        (None, None) => {}
                        _ => {
                            return Err(JlinkError::new(
                                ErrorCode::FrameInvalid,
                                "恢复 capture 的目标身份与启动计划必须同时存在",
                                false,
                            )
                            .with_detail("capture_id", json!(capture_id)));
                        }
                    }
                    if terminal
                        .insert(
                            capture_id,
                            TerminalCapture {
                                snapshot: status,
                                _plan: plan,
                                _store: None,
                                _failure: None,
                            },
                        )
                        .is_some()
                    {
                        return Err(JlinkError::new(
                            ErrorCode::FrameInvalid,
                            "同一 capture_id 同时存在完成和部分恢复事实",
                            false,
                        ));
                    }
                }
            }
        }
        Ok(Self {
            store,
            registry: HssStartRegistry::new(),
            retired_keys,
            active: None,
            terminal,
        })
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub(crate) const fn next_wait() -> Duration {
        DRAIN_INTERVAL
    }

    pub(crate) fn start<I, F>(
        &mut self,
        probe_identity: &str,
        target: &TargetConnectionSpec,
        plan: HssStartPlan,
        capture_max_bytes: u64,
        io: &mut I,
        preflight: F,
    ) -> Result<HssStartOutcome, JlinkError>
    where
        I: HssIo,
        F: FnOnce(&mut I) -> Result<(), JlinkError>,
    {
        if let Some(capture_id) = self.retired_keys.get(plan.capture_key()) {
            return Err(JlinkError::new(
                ErrorCode::CaptureKeyConflict,
                "capture_key 属于上一 MCP/Worker 生命周期，新的采集必须使用新键",
                false,
            )
            .with_detail("capture_id", json!(capture_id)));
        }
        let reservation = match self.registry.reserve(probe_identity, target, &plan)? {
            HssReservationOutcome::Existing(reservation) => {
                let snapshot = self.status(reservation.capture_id(), Instant::now())?;
                return Ok(HssStartOutcome {
                    snapshot,
                    started_new: false,
                });
            }
            HssReservationOutcome::Created(reservation) => reservation,
        };
        if self.active.is_some() {
            self.registry.rollback_created(&plan, &reservation);
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "同一 Worker 同时只允许一个活动 HSS 采集",
                true,
            ));
        }
        if let Err(error) = preflight(io) {
            self.registry.rollback_created(&plan, &reservation);
            return Err(error);
        }
        let capture_id = reservation.capture_id().to_owned();
        let writer = match self
            .store
            .create_writer(&capture_id, target, &plan, capture_max_bytes)
        {
            Ok(writer) => writer,
            Err(error) => {
                self.registry.rollback_created(&plan, &reservation);
                return Err(error);
            }
        };
        let start_called = Instant::now();
        let start_result = io.start_hss(&plan);
        let started = Instant::now();
        if let Err(error) = start_result {
            return Err(self.record_start_failure(&capture_id, plan, writer, error));
        }
        let deadline = started + Duration::from_secs(u64::from(plan.duration_s()));
        let mut status = HssCaptureState::starting();
        status
            .mark_running()
            .expect("a successful Start transitions starting to running");
        let quality = HssQualityTracker::new(
            &plan,
            duration_us(started.saturating_duration_since(start_called)),
        );
        self.active = Some(ActiveCapture {
            reservation,
            plan,
            started,
            deadline,
            status,
            tail_started: None,
            consecutive_empty_tail_reads: 0,
            buffer: vec![0; READ_BUFFER_BYTES],
            writer: Some(writer),
            incomplete_tail: Vec::new(),
            complete_records: 0,
            drain: HssDrainTiming::default(),
            quality,
            writes: Vec::new(),
            pending_write_impact: None,
        });
        Ok(HssStartOutcome {
            snapshot: self.status(&capture_id, started)?,
            started_new: true,
        })
    }

    fn record_start_failure(
        &mut self,
        capture_id: &str,
        plan: HssStartPlan,
        writer: CaptureWriter,
        error: JlinkError,
    ) -> JlinkError {
        let mut status = HssCaptureState::starting();
        status
            .mark_failed(error.code, false, Vec::new())
            .expect("a controlled Start failure can terminate starting");
        let snapshot = HssRunSnapshot {
            capture_id: capture_id.to_owned(),
            state: status.lifecycle(),
            integrity: status.integrity(),
            elapsed_us: 0,
            complete_records: 0,
            drain: HssDrainTiming::default(),
            quality: HssQualitySummary::default(),
            writes: Vec::new(),
            failure_code: status.failure_code(),
            partial_available: false,
            reason: None,
            recoverable: None,
            recovery_notifications: Vec::new(),
        };
        let store_result = writer.finish(&snapshot);
        self.terminal.insert(
            capture_id.to_owned(),
            TerminalCapture {
                snapshot,
                _plan: Some(plan),
                _store: store_result.as_ref().ok().cloned(),
                _failure: Some(error.clone()),
            },
        );
        let mut error = error
            .with_detail("capture_id", json!(capture_id))
            .with_detail("state", json!(HssRunState::Failed))
            .with_detail("partial_available", json!(false));
        if let Err(store_error) = store_result {
            error = error.with_detail("capture_store_publish", json!(store_error.to_string()));
        }
        error
    }

    /// Drains once before any dispatch and performs automatic Stop/tail drain.
    pub(crate) fn advance<I: HssIo>(&mut self, io: &mut I) -> Result<bool, JlinkError> {
        self.advance_at(io, Instant::now())
    }

    pub(crate) fn shutdown<I: HssIo>(&mut self, io: &mut I) -> Result<bool, JlinkError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(false);
        };
        let stopped_at = Instant::now();
        if active.status.lifecycle() == HssRunState::Running {
            io.stop_hss()?;
            active
                .status
                .mark_stopping()
                .expect("running capture enters stopping after successful shutdown Stop");
            active.tail_started = Some(stopped_at);
        }

        loop {
            let now = Instant::now();
            let active = self
                .active
                .as_mut()
                .expect("shutdown retains the active capture until terminal persistence");
            let read = match drain_once(active, io, now) {
                Ok(read) => read,
                Err(error) => {
                    let reported = error.clone();
                    self.fail(error, now, true)?;
                    return Err(reported);
                }
            };
            if read == 0 {
                active.consecutive_empty_tail_reads += 1;
            } else {
                active.consecutive_empty_tail_reads = 0;
            }
            if active.consecutive_empty_tail_reads >= TAIL_EMPTY_READS {
                return self.fail(
                    JlinkError::new(
                        ErrorCode::WorkerUnavailable,
                        "MCP 正常关闭在固定截止时间前停止了 HSS",
                        false,
                    ),
                    now,
                    true,
                );
            }
            if now.saturating_duration_since(stopped_at) >= TAIL_DRAIN_TIMEOUT {
                let error = JlinkError::new(
                    ErrorCode::FrameInvalid,
                    "MCP 正常关闭后的 HSS 尾排空未在 500 ms 内收敛",
                    false,
                );
                self.fail(error.clone(), now, true)?;
                return Err(error);
            }
            std::thread::sleep(DRAIN_INTERVAL);
        }
    }

    fn advance_at<I: HssIo>(&mut self, io: &mut I, now: Instant) -> Result<bool, JlinkError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(false);
        };
        if active.status.lifecycle() == HssRunState::Running {
            if let Err(error) = drain_once(active, io, now) {
                let cleanup = io.stop_hss();
                return match cleanup {
                    Ok(()) => self.fail(error, now, true),
                    Err(cleanup_error) => Err(error.with_detail(
                        "hss_stop_cleanup",
                        json!({
                            "completed": false,
                            "code": cleanup_error.code,
                            "message": cleanup_error.message,
                        }),
                    )),
                };
            }
            if now < active.deadline {
                return Ok(false);
            }
            io.stop_hss()?;
            active
                .status
                .mark_stopping()
                .expect("running capture enters stopping after successful Stop");
            active.tail_started = Some(now);
            return Ok(false);
        }

        let tail_started = active
            .tail_started
            .expect("stopping capture has a tail-drain start");
        let read = match drain_once(active, io, now) {
            Ok(read) => read,
            Err(error) => return self.fail(error, now, true),
        };
        if read == 0 {
            active.consecutive_empty_tail_reads += 1;
        } else {
            active.consecutive_empty_tail_reads = 0;
        }
        if active.consecutive_empty_tail_reads >= TAIL_EMPTY_READS {
            return self.complete(now);
        }
        if now.saturating_duration_since(tail_started) >= TAIL_DRAIN_TIMEOUT {
            return self.fail(
                JlinkError::new(
                    ErrorCode::FrameInvalid,
                    "HSS Stop 后 500 ms 内未达到 20 次连续空排空",
                    false,
                ),
                now,
                true,
            );
        }
        Ok(false)
    }

    fn complete(&mut self, now: Instant) -> Result<bool, JlinkError> {
        let mut active = self
            .active
            .take()
            .expect("completion requires active capture");
        let integrity = active.quality.integrity(active.incomplete_tail.len());
        let mut completed_status = active.status.clone();
        completed_status
            .mark_completed(integrity)
            .expect("tail completion follows stopping");
        let capture_id = active.reservation.capture_id().to_owned();
        let snapshot = snapshot_with_status(&active, &completed_status, now);
        let partial_available = active
            .writer
            .as_ref()
            .is_some_and(|writer| writer.payload_bytes() > 0);
        let plan = active.plan.clone();
        let writer = active.writer.take().expect("active capture owns a writer");
        match writer.finish(&snapshot) {
            Ok(store) => {
                self.terminal.insert(
                    capture_id,
                    TerminalCapture {
                        snapshot,
                        _plan: Some(plan),
                        _store: Some(store),
                        _failure: None,
                    },
                );
            }
            Err(error) => {
                let failed =
                    failed_snapshot_after_store_error(&active, error.code, partial_available, now)?;
                self.terminal.insert(
                    capture_id,
                    TerminalCapture {
                        snapshot: failed,
                        _plan: Some(plan),
                        _store: None,
                        _failure: Some(error),
                    },
                );
            }
        }
        Ok(true)
    }

    fn fail(
        &mut self,
        error: JlinkError,
        now: Instant,
        stop_completed: bool,
    ) -> Result<bool, JlinkError> {
        let mut active = self.active.take().expect("failure requires active capture");
        let partial_available = active
            .writer
            .as_ref()
            .is_some_and(|writer| writer.payload_bytes() > 0);
        let mut notifications = Vec::new();
        if stop_completed {
            notifications.push(HssRecoveryNotification::StopCompletedAfterFailure);
        }
        if partial_available {
            notifications.push(HssRecoveryNotification::PartialDataRetained {
                complete_records: active.complete_records,
                trailing_bytes: u64::try_from(active.incomplete_tail.len()).unwrap_or(u64::MAX),
            });
        }
        active
            .status
            .mark_failed(error.code, partial_available, notifications)?;
        let capture_id = active.reservation.capture_id().to_owned();
        let snapshot = snapshot(&active, now);
        let plan = active.plan.clone();
        let writer = active.writer.take().expect("active capture owns a writer");
        let store_result = writer.finish(&snapshot);
        let error = match &store_result {
            Ok(_) => error,
            Err(store_error) => {
                error.with_detail("capture_store_publish", json!(store_error.to_string()))
            }
        };
        self.terminal.insert(
            capture_id,
            TerminalCapture {
                snapshot,
                _plan: Some(plan),
                _store: store_result.ok(),
                _failure: Some(error),
            },
        );
        Ok(true)
    }

    pub(crate) fn status(
        &self,
        capture_id: &str,
        now: Instant,
    ) -> Result<HssRunSnapshot, JlinkError> {
        if let Some(active) = &self.active
            && active.reservation.capture_id() == capture_id
        {
            return Ok(snapshot(active, now));
        }
        self.terminal
            .get(capture_id)
            .map(|capture| capture.snapshot.clone())
            .ok_or_else(|| {
                JlinkError::new(ErrorCode::ValueInvalid, "Worker 找不到 capture_id", false)
                    .with_detail("capture_id", json!(capture_id))
            })
    }

    pub(crate) fn status_by_key(
        &self,
        capture_key: &str,
        now: Instant,
    ) -> Result<HssRunSnapshot, JlinkError> {
        let capture_id = self
            .registry
            .capture_id_for_key(capture_key)
            .ok_or_else(|| {
                JlinkError::new(ErrorCode::ValueInvalid, "Worker 找不到 capture_key", false)
                    .with_detail("capture_key", json!(capture_key))
            })?;
        self.status(capture_id, now)
    }

    pub(crate) fn begin_write(
        &mut self,
        request_id: &str,
        kind: HssWriteKind,
        requested_at: Instant,
        started_at: Instant,
    ) -> Result<Option<HssWriteToken>, JlinkError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(None);
        };
        if active.status.lifecycle() != HssRunState::Running {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "HSS 尾排空期间不能交错目标写入",
                true,
            ));
        }
        if active.pending_write_impact.is_some() {
            return Err(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "上一笔 HSS 交错写入尚未完成后续排空",
                false,
            ));
        }
        let index = active.writes.len();
        active.writes.push(HssWriteTiming {
            request_id: request_id.to_owned(),
            kind,
            requested_at_us: instant_offset_us(active.started, requested_at),
            started_at_us: instant_offset_us(active.started, started_at),
            completed_at_us: instant_offset_us(active.started, started_at),
            result: HssWriteResult::Succeeded,
            samples_before: active.complete_records,
            samples_after_next_drain: None,
        });
        Ok(Some(HssWriteToken { index }))
    }

    pub(crate) fn finish_write(
        &mut self,
        token: Option<HssWriteToken>,
        completed_at: Instant,
        result: Result<(), ErrorCode>,
    ) {
        let Some(token) = token else {
            return;
        };
        let active = self
            .active
            .as_mut()
            .expect("write token requires active capture");
        let event = active
            .writes
            .get_mut(token.index)
            .expect("write token belongs to this capture");
        event.completed_at_us = instant_offset_us(active.started, completed_at);
        event.result = match result {
            Ok(()) => HssWriteResult::Succeeded,
            Err(code) => HssWriteResult::Failed { code },
        };
        active.pending_write_impact = Some(token.index);
    }
}

fn validate_recovered_capture_id(
    reservation: &HssReservationOutcome,
    stored_capture_id: &str,
) -> Result<(), JlinkError> {
    let reservation = match reservation {
        HssReservationOutcome::Created(reservation)
        | HssReservationOutcome::Existing(reservation) => reservation,
    };
    if reservation.capture_id() == stored_capture_id {
        Ok(())
    } else {
        Err(JlinkError::new(
            ErrorCode::FrameInvalid,
            "持久化 capture 身份与探针、键和请求指纹不一致",
            false,
        )
        .with_detail("stored_capture_id", json!(stored_capture_id))
        .with_detail("expected_capture_id", json!(reservation.capture_id())))
    }
}

fn drain_once<I: HssIo>(
    active: &mut ActiveCapture,
    io: &mut I,
    call_started: Instant,
) -> Result<usize, JlinkError> {
    let record_bytes = usize::try_from(active.plan.frame_layout().record_bytes())
        .map_err(|_| JlinkError::new(ErrorCode::FrameInvalid, "HSS 帧长度无法表示", false))?;
    let outcome = match io.read_hss(&mut active.buffer, record_bytes) {
        Ok(outcome) => outcome,
        Err(error) => {
            if error.code == ErrorCode::FrameInvalid {
                active.quality.record_frame_format_error(
                    instant_offset_us(active.started, Instant::now()),
                    active.complete_records,
                );
            }
            return Err(error);
        }
    };
    let read = outcome.bytes;
    let completed = Instant::now();
    let elapsed = duration_us(completed.saturating_duration_since(call_started));
    active.drain.calls += 1;
    active.drain.total_us = active.drain.total_us.saturating_add(elapsed);
    active.drain.max_us = active.drain.max_us.max(elapsed);
    if read > active.buffer.len() {
        active.quality.record_frame_format_error(
            instant_offset_us(active.started, completed),
            active.complete_records,
        );
        return Err(JlinkError::new(
            ErrorCode::FrameInvalid,
            "HSS 排空长度超过读取缓冲区",
            false,
        ));
    }
    let host_elapsed_us = instant_offset_us(active.started, completed);
    active
        .quality
        .observe_read_shape(read, record_bytes, host_elapsed_us, active.complete_records);
    if outcome.overflow_confirmed {
        active.quality.record_confirmed_overflow(
            host_elapsed_us,
            active.complete_records,
            active
                .complete_records
                .saturating_add(u64::try_from(read / record_bytes).unwrap_or(u64::MAX)),
        );
    }
    let phase = if active.status.lifecycle() == HssRunState::Running {
        CapturePhase::Live
    } else {
        CapturePhase::Tail
    };
    active
        .writer
        .as_mut()
        .expect("active capture owns a writer")
        .append(host_elapsed_us, phase, &active.buffer[..read])?;
    active
        .incomplete_tail
        .extend_from_slice(&active.buffer[..read]);
    let complete_bytes = active.incomplete_tail.len() / record_bytes * record_bytes;
    active.complete_records = active.quality.observe_complete_records(
        active.plan.frame_layout(),
        &active.incomplete_tail[..complete_bytes],
        host_elapsed_us,
    )?;
    let tail = active.incomplete_tail.split_off(complete_bytes);
    active.incomplete_tail = tail;
    if let Some(index) = active.pending_write_impact.take() {
        active.writes[index].samples_after_next_drain = Some(active.complete_records);
    }
    Ok(read)
}

fn snapshot(active: &ActiveCapture, now: Instant) -> HssRunSnapshot {
    snapshot_with_status(active, &active.status, now)
}

fn snapshot_with_status(
    active: &ActiveCapture,
    status: &HssCaptureState,
    now: Instant,
) -> HssRunSnapshot {
    let terminal_tail_bytes = matches!(
        status.lifecycle(),
        HssRunState::Completed | HssRunState::Failed | HssRunState::Aborted
    )
    .then_some(active.incomplete_tail.len())
    .unwrap_or_default();
    HssRunSnapshot {
        capture_id: active.reservation.capture_id().to_owned(),
        state: status.lifecycle(),
        integrity: status.integrity(),
        elapsed_us: instant_offset_us(active.started, now),
        complete_records: active.complete_records,
        drain: active.drain,
        quality: active.quality.summary(terminal_tail_bytes),
        writes: active.writes.clone(),
        failure_code: status.failure_code(),
        partial_available: status.partial_available(),
        reason: status.reason().map(str::to_owned),
        recoverable: status.recoverable(),
        recovery_notifications: status.recovery_notifications().to_vec(),
    }
}

fn failed_snapshot_after_store_error(
    active: &ActiveCapture,
    code: ErrorCode,
    partial_available: bool,
    now: Instant,
) -> Result<HssRunSnapshot, JlinkError> {
    let mut status = active.status.clone();
    let notifications = partial_available
        .then(|| HssRecoveryNotification::PartialDataRetained {
            complete_records: active.complete_records,
            trailing_bytes: u64::try_from(active.incomplete_tail.len()).unwrap_or(u64::MAX),
        })
        .into_iter()
        .collect();
    status.mark_failed(code, partial_available, notifications)?;
    Ok(snapshot_with_status(active, &status, now))
}

fn instant_offset_us(start: Instant, value: Instant) -> u64 {
    duration_us(value.checked_duration_since(start).unwrap_or_default())
}

fn duration_us(duration: Duration) -> u64 {
    u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, time::Duration};

    use jlink_domain::{
        AccessLayout, AccessPlan, ErrorCode, FirmwareIdentityPlan, HssClockMappingMethod,
        HssDataIntegrity, HssNormalizedTimeUnit, HssQualityBasis, HssQualityEventKind,
        HssQualityEvidence, HssRecoveryNotification, HssReturnWhen, HssRunSnapshot, HssRunState,
        HssSourceTimeUnit, HssStartPlan, HssWriteKind, HssWriteResult, ScalarEncoding,
        TargetConnectionSpec, VariableSelector,
    };
    use serde_json::json;
    use tempfile::{TempDir, tempdir};

    use super::{HssCoordinator, HssIo, HssReadOutcome, Instant, JlinkError};

    const TEST_CAPTURE_MAX_BYTES: u64 = 16 * 1024 * 1024;

    struct ScriptedHss {
        start_error: Option<JlinkError>,
        stop_error: Option<JlinkError>,
        reads: VecDeque<Result<(Vec<u8>, bool), JlinkError>>,
        calls: Vec<&'static str>,
    }

    impl ScriptedHss {
        fn healthy(reads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                start_error: None,
                stop_error: None,
                reads: reads.into_iter().map(|bytes| Ok((bytes, false))).collect(),
                calls: Vec::new(),
            }
        }
    }

    impl HssIo for ScriptedHss {
        fn start_hss(&mut self, _plan: &HssStartPlan) -> Result<(), JlinkError> {
            self.calls.push("start");
            match self.start_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn read_hss(
            &mut self,
            buffer: &mut [u8],
            _record_bytes: usize,
        ) -> Result<HssReadOutcome, JlinkError> {
            self.calls.push("drain");
            let (bytes, overflow_confirmed) = self
                .reads
                .pop_front()
                .unwrap_or_else(|| Ok((Vec::new(), false)))?;
            buffer[..bytes.len()].copy_from_slice(&bytes);
            Ok(HssReadOutcome {
                bytes: bytes.len(),
                overflow_confirmed,
            })
        }

        fn stop_hss(&mut self) -> Result<(), JlinkError> {
            self.calls.push("stop");
            match self.stop_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    fn start_plan() -> HssStartPlan {
        let firmware: FirmwareIdentityPlan = serde_json::from_value(json!({
            "elf_sha256": "11".repeat(32),
            "segments": [{
                "address": 0,
                "length": 4,
                "sha256": "22".repeat(32)
            }]
        }))
        .expect("firmware fixture");
        let access = AccessPlan::new(
            "11".repeat(32),
            VariableSelector::new("fixture", None).expect("selector"),
            0x2000_0000,
            4,
            None,
            false,
            AccessLayout::Scalar {
                name: "uint32_t".to_owned(),
                byte_size: 4,
                encoding: ScalarEncoding::Unsigned,
            },
        );
        HssStartPlan::new(
            "run-fixture",
            1,
            1_000,
            HssReturnWhen::Started,
            vec![access],
            Vec::new(),
            firmware,
        )
        .expect("start plan")
    }

    fn target() -> TargetConnectionSpec {
        TargetConnectionSpec::new(
            "S32K144",
            jlink_domain::TargetInterface::Swd,
            4_000,
            Some(260_106_173),
            None,
        )
        .expect("target fixture")
    }

    fn open_coordinator() -> (TempDir, HssCoordinator) {
        let root = tempdir().expect("capture store root");
        let coordinator =
            HssCoordinator::open(root.path(), "260106173").expect("capture store opens");
        (root, coordinator)
    }

    fn record(timestamp_ms: u32, value: u32) -> Vec<u8> {
        [timestamp_ms.to_le_bytes(), value.to_le_bytes()].concat()
    }

    fn complete_capture(
        coordinator: &mut HssCoordinator,
        io: &mut ScriptedHss,
        capture_id: &str,
    ) -> HssRunSnapshot {
        let started = coordinator.active.as_ref().expect("active capture").started;
        coordinator
            .advance_at(io, started + Duration::from_secs(1))
            .expect("deadline drain and Stop");
        for index in 1..=100 {
            if coordinator
                .advance_at(
                    io,
                    started + Duration::from_secs(1) + Duration::from_millis(index),
                )
                .expect("tail drain")
            {
                return coordinator
                    .status(capture_id, Instant::now())
                    .expect("terminal quality snapshot");
            }
        }
        panic!("tail drain did not complete")
    }

    fn quality_capture(reads: impl IntoIterator<Item = (Vec<u8>, bool)>) -> HssRunSnapshot {
        let (_root, mut coordinator) = open_coordinator();
        let mut io = ScriptedHss {
            start_error: None,
            stop_error: None,
            reads: reads.into_iter().map(Ok).collect(),
            calls: Vec::new(),
        };
        let capture = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("quality capture starts");
        complete_capture(&mut coordinator, &mut io, &capture.snapshot.capture_id)
    }

    #[test]
    fn t_p3_quality_reports_rate_loss_overflow_and_millisecond_clock_evidence() {
        let snapshot = quality_capture([(record(12_345, 1), false), (record(12_346, 2), false)]);
        assert_eq!(snapshot.state, HssRunState::Completed);
        assert_eq!(snapshot.quality.requested_rate_hz, 1_000);
        assert_eq!(snapshot.quality.actual_samples, 2);
        assert_eq!(snapshot.quality.actual_rate_millihz, Some(1_000_000));
        assert_eq!(snapshot.quality.loss.evidence, HssQualityEvidence::Unknown);
        assert_eq!(snapshot.quality.loss.lost_samples, None);
        assert_eq!(
            snapshot.quality.overflow.evidence,
            HssQualityEvidence::Unknown
        );
        assert_eq!(snapshot.quality.overflow.events, None);
        assert_eq!(
            snapshot.quality.clock.source_unit,
            HssSourceTimeUnit::Milliseconds
        );
        assert_eq!(snapshot.quality.clock.source_resolution_us, 1_000);
        assert_eq!(
            snapshot.quality.clock.normalized_unit,
            HssNormalizedTimeUnit::Microseconds
        );
        assert_eq!(
            snapshot.quality.clock.mapping_method,
            HssClockMappingMethod::CaptureStartCallBound
        );
        assert!(snapshot.quality.clock.mapping_error_us.unwrap() >= 1_000);
        assert_eq!(snapshot.quality.clock.first_timestamp_us, Some(12_345_000));
        assert_eq!(snapshot.quality.clock.last_timestamp_us, Some(12_346_000));

        let snapshot = quality_capture([
            (record(0, 1), false),
            (record(2, 2), false),
            (record(4, 3), false),
        ]);
        assert_eq!(snapshot.state, HssRunState::Completed);
        assert_eq!(snapshot.quality.actual_rate_millihz, Some(500_000));
        assert_eq!(snapshot.quality.intervals.gap_events, 2);
        assert_eq!(snapshot.quality.intervals.gap_slots, 2);
        assert_eq!(
            snapshot.quality.loss.basis,
            HssQualityBasis::SourceTimestampGap
        );
        assert_eq!(
            snapshot.quality.loss.evidence,
            HssQualityEvidence::Suspected
        );
        assert_eq!(snapshot.integrity, HssDataIntegrity::Degraded);

        let snapshot = quality_capture([(record(0, 1), true)]);
        assert_eq!(snapshot.state, HssRunState::Completed);
        assert_eq!(
            snapshot.quality.overflow.evidence,
            HssQualityEvidence::Confirmed
        );
        assert_eq!(snapshot.quality.overflow.events, Some(1));
        assert_eq!(
            snapshot.quality.loss.evidence,
            HssQualityEvidence::Confirmed
        );
        assert_eq!(snapshot.integrity, HssDataIntegrity::Degraded);
        assert!(snapshot.quality.events.iter().any(|event| {
            event.kind == HssQualityEventKind::BufferOverflow
                && event.evidence == HssQualityEvidence::Confirmed
        }));

        let bytes = record(0, 1);
        let snapshot =
            quality_capture([(bytes[..3].to_vec(), false), (bytes[3..].to_vec(), false)]);
        assert_eq!(snapshot.state, HssRunState::Completed);
        assert_eq!(snapshot.complete_records, 1);
        assert_eq!(
            snapshot.quality.loss.basis,
            HssQualityBasis::ShortOrMalformedRead
        );
        assert_eq!(snapshot.quality.loss.lost_samples, None);
        assert!(snapshot.quality.events.iter().any(|event| {
            event.kind == HssQualityEventKind::ShortFrame && event.occurrences == 2
        }));
    }

    #[test]
    fn t_p3_run_prioritizes_drain_records_failed_write_and_completes_tail() {
        let first_record = [1_u32.to_le_bytes(), 10_u32.to_le_bytes()].concat();
        let second_record = [2_u32.to_le_bytes(), 20_u32.to_le_bytes()].concat();
        let mut io = ScriptedHss::healthy([first_record, second_record]);
        let (store_root, mut coordinator) = open_coordinator();
        let outcome = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        let capture_id = outcome.snapshot.capture_id;
        let started = coordinator.active.as_ref().expect("active capture").started;

        coordinator
            .advance_at(&mut io, started + Duration::from_millis(100))
            .expect("priority drain before write");
        let token = coordinator
            .begin_write(
                "write-1",
                HssWriteKind::MemoryWrite,
                started + Duration::from_millis(101),
                started + Duration::from_millis(102),
            )
            .expect("write accepted");
        io.calls.push("write");
        coordinator.finish_write(
            token,
            started + Duration::from_millis(103),
            Err(ErrorCode::VerifyFailed),
        );
        coordinator
            .advance_at(&mut io, started + Duration::from_millis(104))
            .expect("immediate post-write drain");
        assert_eq!(
            coordinator
                .status(&capture_id, started)
                .unwrap()
                .complete_records,
            2
        );

        coordinator
            .advance_at(&mut io, started + Duration::from_secs(1))
            .expect("deadline performs internal stop");
        for index in 1..=20 {
            let completed = coordinator
                .advance_at(
                    &mut io,
                    started + Duration::from_secs(1) + Duration::from_millis(index),
                )
                .expect("tail drain");
            assert_eq!(completed, index == 20);
        }

        let snapshot = coordinator.status(&capture_id, Instant::now()).unwrap();
        assert_eq!(snapshot.state, HssRunState::Completed);
        assert_eq!(snapshot.complete_records, 2);
        assert_eq!(snapshot.writes.len(), 1);
        assert_eq!(snapshot.writes[0].kind, HssWriteKind::MemoryWrite);
        assert_eq!(snapshot.writes[0].samples_before, 1);
        assert_eq!(snapshot.writes[0].samples_after_next_drain, Some(2));
        assert_eq!(
            snapshot.writes[0].result,
            HssWriteResult::Failed {
                code: ErrorCode::VerifyFailed
            }
        );
        assert_eq!(&io.calls[..4], ["start", "drain", "write", "drain"]);
        assert_eq!(io.calls.iter().filter(|call| **call == "stop").count(), 1);
        assert!(snapshot.drain.calls >= 23);
        assert!(
            store_root
                .path()
                .join(format!("capture-{capture_id}.capture"))
                .is_file()
        );
    }

    #[test]
    fn t_p3_recover_keeps_completed_capture_by_id_but_retires_its_key_after_restart() {
        let plan = start_plan();
        let (store_root, mut coordinator) = open_coordinator();
        let mut io = ScriptedHss::healthy([record(1, 7)]);
        let started = coordinator
            .start(
                "260106173",
                &target(),
                plan.clone(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        let capture_id = started.snapshot.capture_id;
        let completed = complete_capture(&mut coordinator, &mut io, &capture_id);
        assert_eq!(completed.state, HssRunState::Completed);
        drop(coordinator);

        let mut recovered = HssCoordinator::open(store_root.path(), "260106173")
            .expect("completed capture index restores");
        let by_id = recovered
            .status(&capture_id, Instant::now())
            .expect("immutable completed capture remains queryable by ID");
        assert_eq!(by_id.state, HssRunState::Completed);
        assert_eq!(
            recovered
                .status_by_key(plan.capture_key(), Instant::now())
                .expect_err("capture key does not cross Worker lifecycles")
                .code,
            ErrorCode::ValueInvalid
        );

        let mut second_io = ScriptedHss::healthy([]);
        let error = recovered
            .start(
                "260106173",
                &target(),
                plan,
                TEST_CAPTURE_MAX_BYTES,
                &mut second_io,
                |_| panic!("retired key must fail before hardware preflight"),
            )
            .expect_err("same key cannot start in a new Worker lifecycle");
        assert_eq!(error.code, ErrorCode::CaptureKeyConflict);
        assert!(
            second_io.calls.is_empty(),
            "retired key cannot call HSS Start"
        );
    }

    #[test]
    fn t_p3_recover_graceful_shutdown_stops_drains_and_persists_failed_capture() {
        let (store_root, mut coordinator) = open_coordinator();
        let mut io = ScriptedHss::healthy([record(1, 7)]);
        let outcome = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        let capture_id = outcome.snapshot.capture_id;

        assert!(
            coordinator
                .shutdown(&mut io)
                .expect("graceful shutdown reaches a persisted terminal state")
        );
        let snapshot = coordinator
            .status(&capture_id, Instant::now())
            .expect("shutdown capture remains queryable in the owning Worker");
        assert_eq!(snapshot.state, HssRunState::Failed);
        assert_eq!(snapshot.failure_code, Some(ErrorCode::WorkerUnavailable));
        assert!(snapshot.partial_available);
        assert_eq!(io.calls[0..2], ["start", "stop"]);
        assert_eq!(io.calls.iter().filter(|call| **call == "stop").count(), 1);
        assert!(
            store_root
                .path()
                .join(format!("capture-{capture_id}.capture"))
                .is_file()
        );
    }

    #[test]
    fn t_p3_recover_shutdown_reports_tail_drain_failure_after_persisting_it() {
        let (store_root, mut coordinator) = open_coordinator();
        let mut io = ScriptedHss::healthy([]);
        let outcome = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        let capture_id = outcome.snapshot.capture_id;
        io.reads.push_back(Err(JlinkError::new(
            ErrorCode::FrameInvalid,
            "tail drain fixture failure",
            false,
        )));

        let error = coordinator
            .shutdown(&mut io)
            .expect_err("tail drain failure must reach the owning MCP");
        assert_eq!(error.code, ErrorCode::FrameInvalid);
        let snapshot = coordinator
            .status(&capture_id, Instant::now())
            .expect("failed capture remains queryable");
        assert_eq!(snapshot.state, HssRunState::Failed);
        assert_ne!(snapshot.state, HssRunState::Completed);
        assert!(
            store_root
                .path()
                .join(format!("capture-{capture_id}.capture"))
                .is_file()
        );
    }

    #[test]
    fn live_drain_failure_stops_once_and_retains_a_failed_capture() {
        let mut io = ScriptedHss {
            start_error: None,
            stop_error: None,
            reads: VecDeque::from([
                Ok(([1_u32.to_le_bytes(), 10_u32.to_le_bytes()].concat(), false)),
                Err(JlinkError::new(
                    ErrorCode::FrameInvalid,
                    "read failed",
                    false,
                )),
            ]),
            calls: Vec::new(),
        };
        let (_store_root, mut coordinator) = open_coordinator();
        let outcome = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        coordinator.advance(&mut io).expect("first record retained");
        assert!(
            coordinator
                .advance(&mut io)
                .expect("controlled read failure reaches a terminal state")
        );
        let snapshot = coordinator
            .status(&outcome.snapshot.capture_id, Instant::now())
            .expect("failed capture remains queryable");
        assert_eq!(snapshot.state, HssRunState::Failed);
        assert_eq!(snapshot.integrity, HssDataIntegrity::Unknown);
        assert_eq!(snapshot.failure_code, Some(ErrorCode::FrameInvalid));
        assert!(snapshot.partial_available);
        assert_eq!(
            snapshot.recovery_notifications,
            [
                HssRecoveryNotification::StopCompletedAfterFailure,
                HssRecoveryNotification::PartialDataRetained {
                    complete_records: 1,
                    trailing_bytes: 0
                }
            ]
        );
        assert_eq!(io.calls, ["start", "drain", "drain", "stop"]);
    }

    #[test]
    fn controlled_start_failure_is_queryable_and_idempotent_without_partial_data() {
        let plan = start_plan();
        let mut io = ScriptedHss {
            start_error: Some(JlinkError::new(
                ErrorCode::HssStartFailed,
                "start failed",
                true,
            )),
            stop_error: None,
            reads: VecDeque::new(),
            calls: Vec::new(),
        };
        let (store_root, mut coordinator) = open_coordinator();
        let error = coordinator
            .start(
                "260106173",
                &target(),
                plan.clone(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect_err("DLL Start failure remains a tool error");
        let capture_id = error
            .details
            .as_ref()
            .and_then(|details| details.get("capture_id"))
            .and_then(serde_json::Value::as_str)
            .expect("failed capture identity");
        let failed = coordinator
            .status(capture_id, Instant::now())
            .expect("failed start is retained");
        assert_eq!(failed.state, HssRunState::Failed);
        assert_eq!(failed.failure_code, Some(ErrorCode::HssStartFailed));
        assert!(!failed.partial_available);
        assert!(
            store_root
                .path()
                .join(format!("capture-{capture_id}.capture"))
                .is_file()
        );

        let recovered = coordinator
            .start(
                "260106173",
                &target(),
                plan,
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("same key and request recover the failed identity");
        assert!(!recovered.started_new);
        assert_eq!(recovered.snapshot.capture_id, failed.capture_id);
        assert_eq!(io.calls, ["start"]);
    }

    #[test]
    fn unconfirmed_stop_is_fatal_and_not_relabelled_as_controlled_failed() {
        let mut io = ScriptedHss {
            start_error: None,
            stop_error: Some(JlinkError::new(
                ErrorCode::TargetRecoveryFailed,
                "stop failed",
                false,
            )),
            reads: VecDeque::new(),
            calls: Vec::new(),
        };
        let (store_root, mut coordinator) = open_coordinator();
        let outcome = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        let started = coordinator.active.as_ref().expect("active capture").started;
        let error = coordinator
            .advance_at(&mut io, started + Duration::from_secs(1))
            .expect_err("unconfirmed Stop must terminate the Worker batch");
        assert_eq!(error.code, ErrorCode::TargetRecoveryFailed);
        assert!(coordinator.is_active());
        assert_eq!(io.calls, ["start", "drain", "stop"]);
        let capture_id = outcome.snapshot.capture_id;
        drop(coordinator);
        let recovered =
            HssCoordinator::open(store_root.path(), "260106173").expect("partial recovery scan");
        let snapshot = recovered
            .status(&capture_id, Instant::now())
            .expect("aborted partial remains queryable after restart");
        assert_eq!(snapshot.state, HssRunState::Aborted);
        assert_eq!(snapshot.integrity, HssDataIntegrity::Unknown);
        let retired_key = recovered
            .status_by_key("run-fixture", Instant::now())
            .expect_err("aborted capture key is retired after restart");
        assert_eq!(retired_key.code, ErrorCode::ValueInvalid);
    }

    #[test]
    fn incomplete_tail_completes_with_degraded_integrity_and_is_not_discarded() {
        let mut bytes = [1_u32.to_le_bytes(), 10_u32.to_le_bytes()].concat();
        bytes.push(0xAA);
        let mut io = ScriptedHss::healthy([bytes]);
        let (_store_root, mut coordinator) = open_coordinator();
        let outcome = coordinator
            .start(
                "260106173",
                &target(),
                start_plan(),
                TEST_CAPTURE_MAX_BYTES,
                &mut io,
                |_| Ok(()),
            )
            .expect("capture starts");
        let started = coordinator.active.as_ref().expect("active capture").started;
        coordinator
            .advance_at(&mut io, started + Duration::from_secs(1))
            .expect("deadline stop");
        for index in 1..=20 {
            coordinator
                .advance_at(
                    &mut io,
                    started + Duration::from_secs(1) + Duration::from_millis(index),
                )
                .expect("tail drain");
        }
        let snapshot = coordinator
            .status(&outcome.snapshot.capture_id, Instant::now())
            .expect("degraded capture remains queryable");
        assert_eq!(snapshot.state, HssRunState::Completed);
        assert_eq!(snapshot.integrity, HssDataIntegrity::Degraded);
        assert_eq!(snapshot.complete_records, 1);
        assert_eq!(snapshot.failure_code, None);
        assert!(!snapshot.partial_available);
    }
}
