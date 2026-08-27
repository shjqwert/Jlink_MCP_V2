use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use jlink_domain::{
    ErrorCode, HssCaptureReservation, HssDrainTiming, HssReservationOutcome, HssRunSnapshot,
    HssRunState, HssStartPlan, HssStartRegistry, HssWriteResult, HssWriteTiming, JlinkError,
};
use serde_json::json;

use crate::gateway::DllGateway;

const READ_BUFFER_BYTES: usize = 64 * 1024;
const DRAIN_INTERVAL: Duration = Duration::from_millis(1);
const TAIL_DRAIN_TIMEOUT: Duration = Duration::from_millis(500);
const TAIL_EMPTY_READS: u32 = 20;

pub(crate) trait HssIo {
    fn start_hss(&mut self, plan: &HssStartPlan) -> Result<(), JlinkError>;
    fn read_hss(&mut self, buffer: &mut [u8], record_bytes: usize) -> Result<usize, JlinkError>;
    fn stop_hss(&mut self) -> Result<(), JlinkError>;
}

impl HssIo for DllGateway {
    fn start_hss(&mut self, plan: &HssStartPlan) -> Result<(), JlinkError> {
        Self::start_hss(self, plan)
    }

    fn read_hss(&mut self, buffer: &mut [u8], record_bytes: usize) -> Result<usize, JlinkError> {
        Self::read_hss(self, buffer, record_bytes)
    }

    fn stop_hss(&mut self) -> Result<(), JlinkError> {
        Self::stop_hss(self)
    }
}

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
    state: HssRunState,
    tail_started: Option<Instant>,
    consecutive_empty_tail_reads: u32,
    buffer: Vec<u8>,
    raw_bytes: Vec<u8>,
    incomplete_tail: Vec<u8>,
    complete_records: u64,
    drain: HssDrainTiming,
    writes: Vec<HssWriteTiming>,
    pending_write_impact: Option<usize>,
}

struct CompletedCapture {
    snapshot: HssRunSnapshot,
    _plan: HssStartPlan,
    _raw_bytes: Vec<u8>,
}

/// Worker-owned fixed-duration scheduler; all methods run on the DLL thread.
pub(crate) struct HssCoordinator {
    registry: HssStartRegistry,
    active: Option<ActiveCapture>,
    completed: BTreeMap<String, CompletedCapture>,
}

impl HssCoordinator {
    pub(crate) const fn new() -> Self {
        Self {
            registry: HssStartRegistry::new(),
            active: None,
            completed: BTreeMap::new(),
        }
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
        plan: HssStartPlan,
        io: &mut I,
        preflight: F,
    ) -> Result<HssStartOutcome, JlinkError>
    where
        I: HssIo,
        F: FnOnce(&mut I) -> Result<(), JlinkError>,
    {
        let reservation = match self.registry.reserve(probe_identity, &plan)? {
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
        if let Err(error) = preflight(io).and_then(|()| io.start_hss(&plan)) {
            self.registry.rollback_created(&plan, &reservation);
            return Err(error);
        }
        let started = Instant::now();
        let deadline = started + Duration::from_secs(u64::from(plan.duration_s()));
        let capture_id = reservation.capture_id().to_owned();
        self.active = Some(ActiveCapture {
            reservation,
            plan,
            started,
            deadline,
            state: HssRunState::Running,
            tail_started: None,
            consecutive_empty_tail_reads: 0,
            buffer: vec![0; READ_BUFFER_BYTES],
            raw_bytes: Vec::new(),
            incomplete_tail: Vec::new(),
            complete_records: 0,
            drain: HssDrainTiming::default(),
            writes: Vec::new(),
            pending_write_impact: None,
        });
        Ok(HssStartOutcome {
            snapshot: self.status(&capture_id, started)?,
            started_new: true,
        })
    }

    /// Drains once before any dispatch and performs automatic Stop/tail drain.
    pub(crate) fn advance<I: HssIo>(&mut self, io: &mut I) -> Result<bool, JlinkError> {
        self.advance_at(io, Instant::now())
    }

    fn advance_at<I: HssIo>(&mut self, io: &mut I, now: Instant) -> Result<bool, JlinkError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(false);
        };
        if active.state == HssRunState::Running {
            if let Err(error) = drain_once(active, io, now) {
                let cleanup = io.stop_hss();
                return Err(error.with_detail(
                    "hss_stop_cleanup",
                    match cleanup {
                        Ok(()) => json!({ "completed": true }),
                        Err(cleanup_error) => json!({
                            "completed": false,
                            "code": cleanup_error.code,
                            "message": cleanup_error.message,
                        }),
                    },
                ));
            }
            if now < active.deadline {
                return Ok(false);
            }
            io.stop_hss()?;
            active.state = HssRunState::Stopping;
            active.tail_started = Some(now);
            return Ok(false);
        }

        let tail_started = active
            .tail_started
            .expect("stopping capture has a tail-drain start");
        let read = drain_once(active, io, now)?;
        if read == 0 {
            active.consecutive_empty_tail_reads += 1;
        } else {
            active.consecutive_empty_tail_reads = 0;
        }
        if active.consecutive_empty_tail_reads >= TAIL_EMPTY_READS {
            return self.complete(now);
        }
        if now.saturating_duration_since(tail_started) >= TAIL_DRAIN_TIMEOUT {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                "HSS Stop 后 500 ms 内未达到 20 次连续空排空",
                false,
            ));
        }
        Ok(false)
    }

    fn complete(&mut self, now: Instant) -> Result<bool, JlinkError> {
        let active = self
            .active
            .take()
            .expect("completion requires active capture");
        if !active.incomplete_tail.is_empty() {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                "HSS 尾排空后仍存在不完整帧",
                false,
            )
            .with_detail("incomplete_bytes", json!(active.incomplete_tail.len())));
        }
        let capture_id = active.reservation.capture_id().to_owned();
        let snapshot = snapshot(&active, HssRunState::Completed, now);
        self.completed.insert(
            capture_id,
            CompletedCapture {
                snapshot,
                _plan: active.plan,
                _raw_bytes: active.raw_bytes,
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
            return Ok(snapshot(active, active.state, now));
        }
        self.completed
            .get(capture_id)
            .map(|capture| capture.snapshot.clone())
            .ok_or_else(|| {
                JlinkError::new(ErrorCode::ValueInvalid, "Worker 找不到 capture_id", false)
                    .with_detail("capture_id", json!(capture_id))
            })
    }

    pub(crate) fn begin_write(
        &mut self,
        request_id: &str,
        requested_at: Instant,
        started_at: Instant,
    ) -> Result<Option<HssWriteToken>, JlinkError> {
        let Some(active) = self.active.as_mut() else {
            return Ok(None);
        };
        if active.state != HssRunState::Running {
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

fn drain_once<I: HssIo>(
    active: &mut ActiveCapture,
    io: &mut I,
    call_started: Instant,
) -> Result<usize, JlinkError> {
    let record_bytes = usize::try_from(active.plan.frame_layout().record_bytes())
        .map_err(|_| JlinkError::new(ErrorCode::FrameInvalid, "HSS 帧长度无法表示", false))?;
    let read = io.read_hss(&mut active.buffer, record_bytes)?;
    let completed = Instant::now();
    let elapsed = duration_us(completed.saturating_duration_since(call_started));
    active.drain.calls += 1;
    active.drain.total_us = active.drain.total_us.saturating_add(elapsed);
    active.drain.max_us = active.drain.max_us.max(elapsed);
    if read > active.buffer.len() {
        return Err(JlinkError::new(
            ErrorCode::FrameInvalid,
            "HSS 排空长度超过读取缓冲区",
            false,
        ));
    }
    active.raw_bytes.extend_from_slice(&active.buffer[..read]);
    active
        .incomplete_tail
        .extend_from_slice(&active.buffer[..read]);
    let complete_bytes = active.incomplete_tail.len() / record_bytes * record_bytes;
    active.complete_records = active
        .complete_records
        .saturating_add(u64::try_from(complete_bytes / record_bytes).unwrap_or(u64::MAX));
    let tail = active.incomplete_tail.split_off(complete_bytes);
    active.incomplete_tail = tail;
    if let Some(index) = active.pending_write_impact.take() {
        active.writes[index].samples_after_next_drain = Some(active.complete_records);
    }
    Ok(read)
}

fn snapshot(active: &ActiveCapture, state: HssRunState, now: Instant) -> HssRunSnapshot {
    HssRunSnapshot {
        capture_id: active.reservation.capture_id().to_owned(),
        state,
        elapsed_us: instant_offset_us(active.started, now),
        complete_records: active.complete_records,
        drain: active.drain,
        writes: active.writes.clone(),
    }
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
        AccessLayout, AccessPlan, ErrorCode, FirmwareIdentityPlan, HssReturnWhen, HssRunState,
        HssStartPlan, HssWriteResult, ScalarEncoding, VariableSelector,
    };
    use serde_json::json;

    use super::{HssCoordinator, HssIo, Instant, JlinkError};

    struct ScriptedHss {
        reads: VecDeque<Result<Vec<u8>, JlinkError>>,
        calls: Vec<&'static str>,
    }

    impl ScriptedHss {
        fn healthy(reads: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                reads: reads.into_iter().map(Ok).collect(),
                calls: Vec::new(),
            }
        }
    }

    impl HssIo for ScriptedHss {
        fn start_hss(&mut self, _plan: &HssStartPlan) -> Result<(), JlinkError> {
            self.calls.push("start");
            Ok(())
        }

        fn read_hss(
            &mut self,
            buffer: &mut [u8],
            _record_bytes: usize,
        ) -> Result<usize, JlinkError> {
            self.calls.push("drain");
            let bytes = self.reads.pop_front().unwrap_or_else(|| Ok(Vec::new()))?;
            buffer[..bytes.len()].copy_from_slice(&bytes);
            Ok(bytes.len())
        }

        fn stop_hss(&mut self) -> Result<(), JlinkError> {
            self.calls.push("stop");
            Ok(())
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

    #[test]
    fn t_p3_run_prioritizes_drain_records_failed_write_and_completes_tail() {
        let first_record = [1_u32.to_le_bytes(), 10_u32.to_le_bytes()].concat();
        let second_record = [2_u32.to_le_bytes(), 20_u32.to_le_bytes()].concat();
        let mut io = ScriptedHss::healthy([first_record, second_record]);
        let mut coordinator = HssCoordinator::new();
        let outcome = coordinator
            .start("260106173", start_plan(), &mut io, |_| Ok(()))
            .expect("capture starts");
        let capture_id = outcome.snapshot.capture_id;
        let started = coordinator.active.as_ref().expect("active capture").started;

        coordinator
            .advance_at(&mut io, started + Duration::from_millis(100))
            .expect("priority drain before write");
        let token = coordinator
            .begin_write(
                "write-1",
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
    }

    #[test]
    fn live_drain_failure_attempts_exactly_one_stop_and_does_not_complete() {
        let mut io = ScriptedHss {
            reads: VecDeque::from([Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                "read failed",
                false,
            ))]),
            calls: Vec::new(),
        };
        let mut coordinator = HssCoordinator::new();
        coordinator
            .start("260106173", start_plan(), &mut io, |_| Ok(()))
            .expect("capture starts");
        let error = coordinator
            .advance(&mut io)
            .expect_err("read failure stops the batch");
        assert_eq!(error.code, ErrorCode::FrameInvalid);
        assert_eq!(io.calls, ["start", "drain", "stop"]);
    }
}
