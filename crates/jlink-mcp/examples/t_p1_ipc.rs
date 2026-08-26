//! Windows process integration suite for primary test T-P1-IPC.

use std::{
    env,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use jlink_domain::ErrorCode;
use jlink_mcp::worker_client::{WorkerLaunchSpec, attach_or_spawn};

const DLL_PATH: &str = r"C:\Program Files (x86)\SEGGER\JLink\JLink_x64.dll";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let worker = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("缺少 jlink-worker.exe 路径")?;
    if !worker.is_file() {
        return Err(format!("Worker 不存在：{}", worker.display()).into());
    }
    let directory = tempfile::tempdir()?;
    let probe_identity = format!("t-p1-ipc-{}", std::process::id());
    let spec = WorkerLaunchSpec {
        executable: worker.clone(),
        lease_root: directory.path().join("leases"),
        probe_identity: probe_identity.clone(),
        dll_path: PathBuf::from(DLL_PATH),
    };

    let barrier = Arc::new(Barrier::new(3));
    let launches = [spec.clone(), spec.clone()].map(|launch_spec| {
        let thread_barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            thread_barrier.wait();
            attach_or_spawn(&launch_spec)
        })
    });
    barrier.wait();
    let mut simultaneous = Vec::new();
    for launch in launches {
        let attachment = launch.join().map_err(|_| "并发启动线程异常退出")??;
        simultaneous.push(attachment);
    }
    let simultaneous_pid = simultaneous[0].status.worker_pid;
    let simultaneous_owner_count = simultaneous
        .iter()
        .filter(|attachment| attachment.spawned)
        .count();
    let simultaneous_consistent = simultaneous
        .iter()
        .all(|attachment| attachment.status.worker_pid == simultaneous_pid);
    simultaneous[0].client.disconnect()?;
    for attachment in &mut simultaneous {
        if let Some(child) = attachment.spawned_child_mut() {
            child.wait()?;
        }
    }
    if simultaneous_owner_count != 1 || !simultaneous_consistent {
        return Err(format!(
            "并发启动所有权错误：owner_count={simultaneous_owner_count}, consistent={simultaneous_consistent}"
        )
        .into());
    }

    let mut first = attach_or_spawn(&spec)?;
    if !first.spawned || !first.status.dll_loaded {
        return Err("首次调用未启动持有 DLL 的 Worker".into());
    }
    let authoritative_pid = first.status.worker_pid;
    let second = attach_or_spawn(&spec)?;
    if second.spawned || second.status.worker_pid != authoritative_pid {
        return Err("附着优先未复用同一 Worker".into());
    }

    let competing = Command::new(&worker)
        .arg("--lease-root")
        .arg(&spec.lease_root)
        .arg("--probe")
        .arg(&probe_identity)
        .arg("--dll")
        .arg(DLL_PATH)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    let competing_error = String::from_utf8_lossy(&competing.stderr);
    if competing.status.success() || !competing_error.contains(ErrorCode::ProbeBusy.as_str()) {
        return Err(format!("第二 Worker 未返回 PROBE_BUSY：{competing_error}").into());
    }

    let child = first.spawned_child_mut().ok_or("首次附着缺少子进程句柄")?;
    child.kill()?;
    child.wait()?;
    thread::sleep(Duration::from_millis(50));

    let mut after_crash = attach_or_spawn(&spec)?;
    if !after_crash.spawned || after_crash.status.worker_pid == authoritative_pid {
        return Err("Worker 崩溃后未释放探针租约".into());
    }
    after_crash.client.disconnect()?;
    after_crash
        .spawned_child_mut()
        .ok_or("崩溃恢复 Worker 缺少子进程句柄")?
        .wait()?;

    let mut after_disconnect = attach_or_spawn(&spec)?;
    if !after_disconnect.spawned {
        return Err("正常断开后探针租约仍被占用".into());
    }
    after_disconnect.client.disconnect()?;
    after_disconnect
        .spawned_child_mut()
        .ok_or("正常恢复 Worker 缺少子进程句柄")?
        .wait()?;

    println!("T-P1-IPC 通过：附着优先、单 Worker、探针互斥、崩溃与正常退出释放租约均符合预期");
    Ok(())
}
