//! T-P3-START read-only hardware preflight through the production Worker.

use std::{
    env,
    path::PathBuf,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use jlink_domain::{
    ConnectionState, TargetConnectionSpec, TargetInterface, TargetState, ValidationCheckKind,
};
use jlink_mcp::{
    config::{ConfigSource, ResolvedField, ResolvedJlink, validate_dll_identity},
    worker_client::{WorkerAttachment, WorkerLaunchSpec, attach_or_spawn},
};
use serde_json::json;

const DLL_PATH: &str = r"C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll";
const DLL_VERSION: &str = "6.98a";
const DLL_SHA256: &str = "D15D5A24DC86F135C0B1FAFEB89F0E577691B6A85F3A19C773B3E20D0B95BBE5";
const ELF_SHA256: &str = "F8ADB9A2B9BBFD26B469C66F2478EE6E22735302706B83509B2D4F2AE7F7738D";
const PROBE_SERIAL: u32 = 260_106_173;

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
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Err("Worker 未在 disconnect 后退出".into())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let worker = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("缺少 jlink-worker.exe 路径")?;
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
    let guard = AttachmentGuard::new(attach_or_spawn(&launch)?);
    let connected = guard.attachment().client.connect(&target)?;
    if connected.connection_state != ConnectionState::Connected
        || connected.target_state != TargetState::Running
    {
        return Err(format!(
            "连接预检未收口到 running：{:?}/{:?}",
            connected.connection_state, connected.target_state
        )
        .into());
    }
    let report = guard.attachment().client.validate(&target, None)?;
    let hss = report
        .checks
        .iter()
        .find(|check| check.kind == ValidationCheckKind::HssCapability)
        .ok_or("验证报告缺少 HSS capability")?;
    let background = report
        .checks
        .iter()
        .find(|check| check.kind == ValidationCheckKind::BackgroundAccess)
        .ok_or("验证报告缺少 background access")?;
    if !report.valid || !hss.passed || !background.passed {
        return Err(format!("HSS 启动预检未通过：{report:?}").into());
    }
    let notices = connected.recovery_notifications;
    let detail = hss.detail.clone();
    guard.shutdown()?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "status": "PASS",
            "dll_version": DLL_VERSION,
            "dll_sha256": DLL_SHA256,
            "elf_sha256": ELF_SHA256,
            "probe_serial": PROBE_SERIAL,
            "device": "S32K144",
            "interface": "SWD",
            "speed_khz": 4_000,
            "hss_capability": detail,
            "recovery_notifications": notices
        }))?
    );
    Ok(())
}
