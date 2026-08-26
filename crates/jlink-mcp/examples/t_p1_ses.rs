//! S32K144 hardware integration suite for primary test T-P1-SES.

use std::{
    env,
    path::PathBuf,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use jlink_domain::{
    ConnectionState, ErrorCode, RecoveryNotification, TargetConnectionSpec, TargetInterface,
    TargetState, ValidationAfter, ValidationCheckKind,
};
use jlink_mcp::{
    config::{ConfigSource, ResolvedField, ResolvedJlink, validate_dll_identity},
    worker_client::{WorkerAttachment, WorkerLaunchSpec, attach_or_spawn},
};
use serde_json::json;

const DLL_PATH: &str = r"C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll";
const DLL_VERSION: &str = "6.98a";
const DLL_SHA256: &str = "D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5";
const ELF_SHA256: &str = "3EB79013870DBB6F9B6ADC929C3B43D8D30C4FF35D69A4D2D39A78643526EFEF";
const PROBE_SERIAL: u32 = 260_106_173;
const TARGET_ID: u32 = 0x2BA0_1477;
const EXPECTED_CHECKS: [ValidationCheckKind; 7] = [
    ValidationCheckKind::DllIdentity,
    ValidationCheckKind::RequiredExports,
    ValidationCheckKind::ProbeIdentity,
    ValidationCheckKind::TargetIdentity,
    ValidationCheckKind::Interface,
    ValidationCheckKind::BackgroundAccess,
    ValidationCheckKind::HssCapability,
];

struct AttachmentGuard(Option<WorkerAttachment>);

impl AttachmentGuard {
    fn new(attachment: WorkerAttachment) -> Self {
        Self(Some(attachment))
    }

    fn attachment(&self) -> &WorkerAttachment {
        self.0.as_ref().expect("attachment guard is active")
    }

    fn shutdown(mut self) -> Result<(), Box<dyn std::error::Error>> {
        let mut attachment = self.0.take().expect("attachment guard is active");
        attachment.client.disconnect()?;
        if let Some(child) = attachment.spawned_child_mut() {
            wait_for_exit(child)?;
        }
        Ok(())
    }
}

impl Drop for AttachmentGuard {
    fn drop(&mut self) {
        if let Some(attachment) = &mut self.0 {
            let _ = attachment.client.disconnect();
            if let Some(child) = attachment.spawned_child_mut()
                && child.try_wait().ok().flatten().is_none()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn wait_for_exit(child: &mut Child) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err("Worker 未在断开后两秒内退出".into());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn verify_halted_recovery(
    launch: &WorkerLaunchSpec,
    target: &TargetConnectionSpec,
) -> Result<(), Box<dyn std::error::Error>> {
    let first = AttachmentGuard::new(attach_or_spawn(launch)?);
    let initial = &first.attachment().status;
    if initial.connection_state != ConnectionState::Disconnected
        || initial.validation_cached
        || initial.validation_runs != 0
    {
        return Err("Worker 初始会话状态不干净".into());
    }

    let connected = first.attachment().client.connect(target)?;
    if connected.connection_state != ConnectionState::Connected
        || connected.target_state != TargetState::Running
        || connected.target_id != Some(TARGET_ID)
        || connected.validation_runs != 1
        || !connected.validation_cached
        || connected.recovery_notifications != [RecoveryNotification::ResumedFromHalt]
    {
        return Err(format!("halted 自动恢复结果不符合预期：{connected:?}").into());
    }
    let cached = first.attachment().client.status()?;
    if cached.validation_runs != connected.validation_runs
        || cached.target_state != TargetState::Running
    {
        return Err("status 触发了额外验证或改变了目标状态".into());
    }

    let connected_after_error = first
        .attachment()
        .client
        .validate(target, Some(ValidationAfter::Run))
        .expect_err("活动连接 validate 必须拒绝 after");
    if connected_after_error.code != ErrorCode::OperationConflict {
        return Err(format!("活动连接 validate 未拒绝 after：{connected_after_error}").into());
    }

    let connected_validation = first.attachment().client.validate(target, None)?;
    if !connected_validation.valid
        || connected_validation.target_state != TargetState::Running
        || connected_validation.target_id != Some(TARGET_ID)
        || connected_validation.validation_runs != 2
        || connected_validation
            .checks
            .iter()
            .map(|check| check.kind)
            .ne(EXPECTED_CHECKS)
    {
        return Err(format!("显式验证报告不完整：{connected_validation:?}").into());
    }

    let changed_elf = TargetConnectionSpec::new(
        "S32K144",
        TargetInterface::Swd,
        4_000,
        Some(PROBE_SERIAL),
        Some("A".repeat(64)),
    )?;
    let changed_error = first
        .attachment()
        .client
        .validate(&changed_elf, None)
        .expect_err("活动连接不得复用变化后的 ELF 身份");
    if changed_error.code != ErrorCode::OperationConflict {
        return Err(format!("ELF 变化未使验证缓存失效：{changed_error}").into());
    }
    let second_connect = first
        .attachment()
        .client
        .connect(target)
        .expect_err("单 Worker 不能建立第二个活动目标");
    if second_connect.code != ErrorCode::InvalidStateTransition {
        return Err(format!("第二活动目标未被拒绝：{second_connect}").into());
    }
    first.shutdown()?;
    Ok(())
}

fn verify_disconnected_validation(
    launch: &WorkerLaunchSpec,
    target: &TargetConnectionSpec,
) -> Result<(), Box<dyn std::error::Error>> {
    let second = AttachmentGuard::new(attach_or_spawn(launch)?);
    let missing_after = second
        .attachment()
        .client
        .validate(target, None)
        .expect_err("断开状态 validate 必须要求 after");
    if missing_after.code != ErrorCode::ConfigInvalid {
        return Err(format!("断开状态 validate 未拒绝缺失 after：{missing_after}").into());
    }

    let halted_validation = second
        .attachment()
        .client
        .validate(target, Some(ValidationAfter::Halt))?;
    if !halted_validation.valid
        || halted_validation.target_state != TargetState::Halted
        || halted_validation.target_id != Some(TARGET_ID)
        || halted_validation.validation_runs != 1
        || halted_validation.recovery_notifications != [RecoveryNotification::ResumedFromHalt]
        || halted_validation
            .checks
            .iter()
            .map(|check| check.kind)
            .ne(EXPECTED_CHECKS)
    {
        return Err(format!("断开 validate after=halt 结果错误：{halted_validation:?}").into());
    }
    let halted_status = second.attachment().client.status()?;
    if halted_status.connection_state != ConnectionState::Disconnected
        || halted_status.validation_cached
        || halted_status.target_state != TargetState::Halted
    {
        return Err(format!("after=halt 未保留显式终态：{halted_status:?}").into());
    }

    let running_validation = second
        .attachment()
        .client
        .validate(target, Some(ValidationAfter::Run))?;
    if !running_validation.valid
        || running_validation.target_state != TargetState::Running
        || running_validation.target_id != Some(TARGET_ID)
        || running_validation.validation_runs != 2
        || running_validation.recovery_notifications != [RecoveryNotification::ResumedFromHalt]
    {
        return Err(format!("断开 validate after=run 结果错误：{running_validation:?}").into());
    }
    let final_status = second.attachment().client.status()?;
    if final_status.connection_state != ConnectionState::Disconnected
        || final_status.validation_cached
        || final_status.target_state != TargetState::Running
    {
        return Err(format!("after=run 未保留显式终态：{final_status:?}").into());
    }
    second.shutdown()?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let worker = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("缺少 jlink-worker.exe 路径")?;
    let scenario = env::args()
        .nth(2)
        .ok_or("缺少测试场景：halted 或 validate")?;
    if !worker.is_file() {
        return Err(format!("Worker 不存在：{}", worker.display()).into());
    }
    validate_dll_identity(&ResolvedJlink {
        dll_path: ResolvedField {
            value: PathBuf::from(DLL_PATH),
            source: ConfigSource::Project,
        },
        version: ResolvedField {
            value: DLL_VERSION.to_owned(),
            source: ConfigSource::Project,
        },
        sha256: ResolvedField {
            value: DLL_SHA256.to_owned(),
            source: ConfigSource::Project,
        },
    })?;
    let target = TargetConnectionSpec::new(
        "S32K144",
        TargetInterface::Swd,
        4_000,
        Some(PROBE_SERIAL),
        Some(ELF_SHA256.to_owned()),
    )?;
    let directory = tempfile::tempdir()?;
    let launch = WorkerLaunchSpec {
        executable: worker,
        lease_root: directory.path().join("leases"),
        probe_identity: PROBE_SERIAL.to_string(),
        dll_path: PathBuf::from(DLL_PATH),
    };

    let recovery = match scenario.as_str() {
        "halted" => {
            verify_halted_recovery(&launch, &target)?;
            "resumed_from_halt"
        }
        "validate" => {
            verify_disconnected_validation(&launch, &target)?;
            "explicit_after"
        }
        _ => return Err(format!("未知测试场景：{scenario}").into()),
    };

    println!(
        "{}",
        serde_json::to_string(&json!({
            "status": "PASS",
            "scenario": scenario,
            "dll_version": DLL_VERSION,
            "dll_sha256": DLL_SHA256,
            "elf_sha256": ELF_SHA256,
            "probe_serial": PROBE_SERIAL,
            "device": "S32K144",
            "interface": "SWD",
            "speed_khz": 4_000,
            "target_id": format!("0x{TARGET_ID:08X}"),
            "recovery": recovery,
            "validation_checks": EXPECTED_CHECKS,
            "final_target_state": "running"
        }))?
    );
    Ok(())
}
