use std::{
    ffi::{CStr, CString, c_char},
    marker::PhantomData,
    mem,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    rc::Rc,
    thread,
    time::{Duration, Instant},
};

use jlink_domain::{
    ErrorCode, FaultDiagnostics, JlinkError, TargetConnectionSpec, TargetInterface, TargetState,
    ValidationCheck, ValidationCheckKind, ValidationReport,
};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, GetLastError, HMODULE},
    System::LibraryLoader::{
        GetProcAddress, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR,
        LoadLibraryExW,
    },
};

const RECOVERY_TIMEOUT: Duration = Duration::from_secs(2);
const RUNNING_STABILITY_WINDOW: Duration = Duration::from_millis(100);
const TARGET_POLL_INTERVAL: Duration = Duration::from_millis(10);
const ICSR: u32 = 0xE000_ED04;
const CFSR: u32 = 0xE000_ED28;
const HFSR: u32 = 0xE000_ED2C;
const DFSR: u32 = 0xE000_ED30;
#[cfg(test)]
const DEMCR: u32 = 0xE000_EDFC;
#[cfg(test)]
const DEMCR_VC_HARDERR: u32 = 1 << 10;
#[cfg(test)]
const TEST_HARDFAULT_PC: u32 = 0xE000_0000;

type OpenFn = unsafe extern "C" fn() -> i32;
type CloseFn = unsafe extern "C" fn();
type ExecCommandFn = unsafe extern "C" fn(*const c_char, *mut c_char, i32) -> i32;
type SelectTifFn = unsafe extern "C" fn(i32) -> i32;
type SetSpeedFn = unsafe extern "C" fn(i32);
type ConnectFn = unsafe extern "C" fn() -> i32;
type SelectProbeFn = unsafe extern "C" fn(u32) -> i32;
type GetU32Fn = unsafe extern "C" fn() -> u32;
type GetI32Fn = unsafe extern "C" fn() -> i32;
type VoidFn = unsafe extern "C" fn();
type ReadRegFn = unsafe extern "C" fn(i32) -> u32;
type ReadMemU32Fn = unsafe extern "C" fn(u32, u32, *mut u32, *mut u8) -> i32;
#[cfg(test)]
type WriteRegFn = unsafe extern "C" fn(i32, u32) -> u8;
#[cfg(test)]
type WriteU32Fn = unsafe extern "C" fn(u32, u32) -> i32;
type HssGetCapsFn = unsafe extern "C" fn(*mut HssCaps) -> i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct HssCaps {
    max_blocks: u32,
    max_frequency_hz: u32,
    flags: u32,
    reserved: [u32; 5],
}

struct Api {
    open: OpenFn,
    close: CloseFn,
    exec_command: ExecCommandFn,
    select_tif: SelectTifFn,
    set_speed: SetSpeedFn,
    connect: ConnectFn,
    select_probe: SelectProbeFn,
    get_serial: GetU32Fn,
    get_target_id: GetU32Fn,
    get_dll_version: GetI32Fn,
    is_connected: GetI32Fn,
    is_halted: GetI32Fn,
    halt: VoidFn,
    go: VoidFn,
    reset: VoidFn,
    read_reg: ReadRegFn,
    read_mem_u32: ReadMemU32Fn,
    #[cfg(test)]
    write_reg: WriteRegFn,
    #[cfg(test)]
    write_u32: WriteU32Fn,
    hss_get_caps: HssGetCapsFn,
}

impl Api {
    fn load(module: HMODULE) -> Result<Self, JlinkError> {
        Ok(Self {
            open: load_symbol(module, b"JLINKARM_Open\0")?,
            close: load_symbol(module, b"JLINKARM_Close\0")?,
            exec_command: load_symbol(module, b"JLINKARM_ExecCommand\0")?,
            select_tif: load_symbol(module, b"JLINKARM_TIF_Select\0")?,
            set_speed: load_symbol(module, b"JLINKARM_SetSpeed\0")?,
            connect: load_symbol(module, b"JLINKARM_Connect\0")?,
            select_probe: load_symbol(module, b"JLINKARM_EMU_SelectByUSBSN\0")?,
            get_serial: load_symbol(module, b"JLINKARM_GetSN\0")?,
            get_target_id: load_symbol(module, b"JLINKARM_GetId\0")?,
            get_dll_version: load_symbol(module, b"JLINKARM_GetDLLVersion\0")?,
            is_connected: load_symbol(module, b"JLINKARM_IsConnected\0")?,
            is_halted: load_symbol(module, b"JLINKARM_IsHalted\0")?,
            halt: load_symbol(module, b"JLINKARM_Halt\0")?,
            go: load_symbol(module, b"JLINKARM_Go\0")?,
            reset: load_symbol(module, b"JLINKARM_Reset\0")?,
            read_reg: load_symbol(module, b"JLINKARM_ReadReg\0")?,
            read_mem_u32: load_symbol(module, b"JLINKARM_ReadMemU32\0")?,
            #[cfg(test)]
            write_reg: load_symbol(module, b"JLINKARM_WriteReg\0")?,
            #[cfg(test)]
            write_u32: load_symbol(module, b"JLINKARM_WriteU32\0")?,
            hss_get_caps: load_symbol(module, b"JLINK_HSS_GetCaps\0")?,
        })
    }
}

fn load_symbol<T: Copy>(module: HMODULE, name: &'static [u8]) -> Result<T, JlinkError> {
    // SAFETY: `module` is live and `name` is a static NUL-terminated export name.
    let symbol = unsafe { GetProcAddress(module, name.as_ptr()) }.ok_or_else(|| {
        let display = CStr::from_bytes_with_nul(name)
            .expect("static export names are NUL terminated")
            .to_string_lossy();
        JlinkError::new(
            ErrorCode::DllExportMissing,
            format!("J-Link DLL 缺少必要导出：{display}"),
            false,
        )
    })?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&symbol));
    // SAFETY: each call site supplies the ABI exercised by the frozen 6.98a evidence.
    Ok(unsafe { mem::transmute_copy(&symbol) })
}

pub(crate) struct TargetObservation {
    pub(crate) target_id: u32,
    pub(crate) target_state: TargetState,
}

/// The only owner allowed to hold the J-Link module and future function pointers.
///
/// The `Rc` marker intentionally keeps this value on one Worker thread. V1 DLL
/// calls are added as `&mut self` operations so two threads cannot enter the
/// same module through this boundary.
pub(crate) struct DllGateway {
    module: HMODULE,
    api: Api,
    opened: bool,
    connected_spec: Option<TargetConnectionSpec>,
    target_id: Option<u32>,
    _path: PathBuf,
    _single_thread: PhantomData<Rc<()>>,
}

impl DllGateway {
    /// Loads one already identity-validated x64 DLL with restricted search paths.
    pub(crate) fn load(path: &Path) -> Result<Self, JlinkError> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated and remains alive for the synchronous call.
        let module = unsafe {
            LoadLibraryExW(
                wide.as_ptr(),
                ptr::null_mut(),
                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
            )
        };
        if module.is_null() {
            // SAFETY: GetLastError has no preconditions and is read immediately after failure.
            let code = unsafe { GetLastError() };
            return Err(JlinkError::new(
                ErrorCode::DllLoadFailed,
                format!(
                    "无法加载 J-Link DLL（Windows 错误 {code}）：{}",
                    path.display()
                ),
                false,
            ));
        }
        let api = match Api::load(module) {
            Ok(api) => api,
            Err(error) => {
                // SAFETY: `module` is still uniquely owned on this error path.
                let _ = unsafe { FreeLibrary(module) };
                return Err(error);
            }
        };
        Ok(Self {
            module,
            api,
            opened: false,
            connected_spec: None,
            target_id: None,
            _path: path.to_path_buf(),
            _single_thread: PhantomData,
        })
    }

    /// Reports whether this gateway currently owns a loaded module.
    pub(crate) const fn is_loaded(&self) -> bool {
        !self.module.is_null()
    }

    /// Opens the configured probe and target without applying recovery policy.
    pub(crate) fn open_target(
        &mut self,
        spec: &TargetConnectionSpec,
    ) -> Result<TargetObservation, JlinkError> {
        spec.validate()?;
        if self.opened {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "Worker 已持有一个活动目标，切换前必须断开",
                true,
            ));
        }
        self.exec_command("SuppressGUI = 1")?;
        // SAFETY: the unique gateway serializes calls using the frozen 6.98a ABI.
        let selected = unsafe { (self.api.select_probe)(spec.probe_serial()) };
        if selected < 0 {
            return Err(target_connection_error(format!(
                "无法选择探针 {}：JLINKARM_EMU_SelectByUSBSN 返回 {selected}",
                spec.probe_serial()
            )));
        }
        // SAFETY: the unique gateway serializes calls using the frozen 6.98a ABI.
        let opened = unsafe { (self.api.open)() };
        if opened < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_Open 返回 {opened}"
            )));
        }
        self.opened = true;
        let result = self.configure_open_target(spec);
        if result.is_err() {
            self.close_target();
        }
        result
    }

    fn configure_open_target(
        &mut self,
        spec: &TargetConnectionSpec,
    ) -> Result<TargetObservation, JlinkError> {
        self.exec_command("SetRestartOnClose = 0")?;
        self.exec_command("SetSkipDebugDeInit = 1")?;
        self.exec_command(&format!("device = {}", spec.device()))?;
        let interface = match spec.interface() {
            TargetInterface::Swd => 1,
            TargetInterface::Jtag => 0,
        };
        // SAFETY: the unique gateway serializes calls using the frozen 6.98a ABI.
        let selected = unsafe { (self.api.select_tif)(interface) };
        if selected < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_TIF_Select 返回 {selected}"
            )));
        }
        let speed = i32::try_from(spec.speed_khz()).map_err(|_| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "target.speed_khz 超出 J-Link 接口范围",
                false,
            )
        })?;
        // SAFETY: the unique gateway serializes calls using the frozen 6.98a ABI.
        unsafe { (self.api.set_speed)(speed) };
        // SAFETY: the unique gateway serializes calls using the frozen 6.98a ABI.
        let connected = unsafe { (self.api.connect)() };
        if connected < 0 || !self.is_connected()? {
            return Err(target_connection_error(format!(
                "JLINKARM_Connect 返回 {connected}"
            )));
        }
        // SAFETY: the target connection is active and the getter has no arguments.
        let actual_serial = unsafe { (self.api.get_serial)() };
        if actual_serial != spec.probe_serial() {
            return Err(target_connection_error(format!(
                "探针身份不匹配：期望 {}，实际 {actual_serial}",
                spec.probe_serial()
            )));
        }
        // SAFETY: the target connection is active and the getter has no arguments.
        let target_id = unsafe { (self.api.get_target_id)() };
        if target_id == 0 {
            return Err(target_connection_error("JLINKARM_GetId 返回零"));
        }
        let target_state = self.observe_target_state()?;
        self.connected_spec = Some(spec.clone());
        self.target_id = Some(target_id);
        Ok(TargetObservation {
            target_id,
            target_state,
        })
    }

    /// Closes the target and clears all gateway-local session facts.
    pub(crate) fn close_target(&mut self) {
        if self.opened {
            // SAFETY: the matching open handle is owned by this gateway.
            unsafe { (self.api.close)() };
        }
        self.opened = false;
        self.connected_spec = None;
        self.target_id = None;
    }

    /// Returns whether the DLL still reports the target connection as active.
    pub(crate) fn is_connected(&self) -> Result<bool, JlinkError> {
        // SAFETY: the getter has no arguments and is serialized by the gateway.
        let connected = unsafe { (self.api.is_connected)() };
        if connected < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_IsConnected 返回 {connected}"
            )));
        }
        Ok(connected > 0)
    }

    /// Observes running, halted, or `HardFault` without changing target state.
    pub(crate) fn observe_target_state(&self) -> Result<TargetState, JlinkError> {
        if !self.opened || !self.is_connected()? {
            return Ok(TargetState::Unknown);
        }
        // SAFETY: the target connection is active and the getter has no arguments.
        let halted = unsafe { (self.api.is_halted)() };
        if halted < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_IsHalted 返回 {halted}"
            )));
        }
        let ipsr = self.read_word(ICSR)? & 0x1ff;
        if ipsr == 3 {
            Ok(TargetState::HardFault)
        } else if halted > 0 {
            Ok(TargetState::Halted)
        } else {
            Ok(TargetState::Running)
        }
    }

    /// Injects a real Cortex-M `HardFault` without modifying target Flash.
    ///
    /// This entry point only exists in test builds. It preserves the current
    /// debug exception control value for mandatory cleanup and uses the ARM
    /// frozen DLL register API to change PC while the connected core is halted.
    /// Any failed injection attempts an immediate safe restoration.
    #[cfg(test)]
    pub(crate) fn inject_hardfault_for_test(&mut self) -> Result<u32, JlinkError> {
        if self.observe_target_state()? != TargetState::Running {
            return Err(test_injection_error(
                "HardFault 注入要求同一 gateway 会话中的目标已稳定运行",
            ));
        }
        let original_demcr = self.read_word(DEMCR)?;
        let injection = self.perform_hardfault_injection(original_demcr);
        if let Err(error) = injection {
            return match self.finish_hardfault_injection_for_test(original_demcr) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(test_injection_error(format!(
                    "HardFault 注入失败且安全恢复失败：{error}；{cleanup_error}"
                ))),
            };
        }
        Ok(original_demcr)
    }

    /// Restores debug exception control and leaves the target stably running.
    ///
    /// The cleanup attempts both restoration and run recovery even when one
    /// operation fails so an ignored hardware test cannot silently strand the
    /// target in a debug exception.
    #[cfg(test)]
    pub(crate) fn finish_hardfault_injection_for_test(
        &mut self,
        original_demcr: u32,
    ) -> Result<(), JlinkError> {
        let restore_result = self.write_test_word(DEMCR, original_demcr);
        let running_result = match self.observe_target_state() {
            Ok(TargetState::Running) => Ok(TargetState::Running),
            Ok(_) | Err(_) => self.reset_run_and_observe(),
        };
        match (restore_result, running_result) {
            (Ok(()), Ok(TargetState::Running)) => Ok(()),
            (restore, running) => Err(test_injection_error(format!(
                "测试清理未能确认安全状态：DEMCR={restore:?}，target={running:?}"
            ))),
        }
    }

    #[cfg(test)]
    fn perform_hardfault_injection(&mut self, original_demcr: u32) -> Result<(), JlinkError> {
        // SAFETY: the unique test gateway owns the connected target and the
        // frozen 6.98a no-argument Halt ABI was exercised in F0-A.
        unsafe { (self.api.halt)() };
        self.wait_until_halted()?;
        self.write_test_word(DEMCR, original_demcr | DEMCR_VC_HARDERR)?;
        // SAFETY: the frozen 6.98a export disassembly confirms two 32-bit
        // arguments and a byte return. Register 15 is the Cortex-M PC.
        let write_result = unsafe { (self.api.write_reg)(15, TEST_HARDFAULT_PC) };
        // SAFETY: register 15 is the Cortex-M PC in the frozen read ABI.
        let pc = unsafe { (self.api.read_reg)(15) };
        if pc != TEST_HARDFAULT_PC {
            return Err(test_injection_error(format!(
                "测试 PC 写入不一致：期望 0x{TEST_HARDFAULT_PC:08X}，实际 0x{pc:08X}，WriteReg 返回 {write_result}"
            )));
        }

        // SAFETY: the gateway is the unique owner and the core is halted.
        unsafe { (self.api.go)() };
        let started = Instant::now();
        loop {
            match self.observe_target_state()? {
                TargetState::HardFault => return Ok(()),
                TargetState::Running | TargetState::Halted | TargetState::Unknown => {}
            }
            if started.elapsed() >= RECOVERY_TIMEOUT {
                return Err(test_injection_error("目标未在两秒内进入可观察的 HardFault"));
            }
            thread::sleep(TARGET_POLL_INTERVAL);
        }
    }

    fn wait_until_halted(&self) -> Result<(), JlinkError> {
        let started = Instant::now();
        loop {
            // SAFETY: the target is connected and the getter is serialized.
            let halted = unsafe { (self.api.is_halted)() };
            if halted > 0 {
                return Ok(());
            }
            if halted < 0 {
                return Err(target_recovery_error(format!(
                    "JLINKARM_IsHalted 返回 {halted}"
                )));
            }
            if started.elapsed() >= RECOVERY_TIMEOUT {
                return Err(target_recovery_error("目标未在两秒内暂停"));
            }
            thread::sleep(TARGET_POLL_INTERVAL);
        }
    }

    #[cfg(test)]
    fn write_test_word(&self, address: u32, value: u32) -> Result<(), JlinkError> {
        // SAFETY: the write is serialized and limited to Cortex-M debug PPB
        // registers by the test-only caller; the ABI was exercised in F0-A.
        let result = unsafe { (self.api.write_u32)(address, value) };
        if result < 0 {
            return Err(test_injection_error(format!(
                "JLINKARM_WriteU32(0x{address:08X}) 返回 {result}"
            )));
        }
        Ok(())
    }

    /// Halts the connected target and returns the resulting observed state.
    pub(crate) fn halt_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        // SAFETY: the target connection is active and the call is serialized.
        unsafe { (self.api.halt)() };
        self.wait_until_halted()?;
        self.observe_target_state()
    }

    /// Resumes a halted target and observes the stable final state.
    pub(crate) fn resume_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        // SAFETY: the target connection is active and the call is serialized.
        unsafe { (self.api.go)() };
        self.wait_for_stable_state()
    }

    /// Resets and starts the target, then observes the stable final state.
    pub(crate) fn reset_run_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        // SAFETY: the target connection is active and calls are serialized.
        unsafe {
            (self.api.reset)();
            (self.api.go)();
        }
        self.wait_for_stable_state()
    }

    fn wait_for_stable_state(&self) -> Result<TargetState, JlinkError> {
        let started = Instant::now();
        let mut running_since = None;
        let mut latest = TargetState::Unknown;
        while started.elapsed() < RECOVERY_TIMEOUT {
            latest = self.observe_target_state()?;
            match latest {
                TargetState::Running => {
                    let stable_since = running_since.get_or_insert_with(Instant::now);
                    if stable_since.elapsed() >= RUNNING_STABILITY_WINDOW {
                        return Ok(TargetState::Running);
                    }
                }
                TargetState::HardFault => return Ok(TargetState::HardFault),
                TargetState::Halted | TargetState::Unknown => running_since = None,
            }
            thread::sleep(TARGET_POLL_INTERVAL);
        }
        Ok(latest)
    }

    /// Captures all readable Cortex-M diagnostics after a recovery failure.
    pub(crate) fn fault_diagnostics(&self) -> FaultDiagnostics {
        let mut diagnostics = FaultDiagnostics {
            // SAFETY: register 15 is the Cortex-M program counter in the frozen ABI.
            pc: Some(unsafe { (self.api.read_reg)(15) }),
            ..FaultDiagnostics::default()
        };
        for (name, address, target) in [
            ("ipsr", ICSR, &mut diagnostics.ipsr),
            ("cfsr", CFSR, &mut diagnostics.cfsr),
            ("hfsr", HFSR, &mut diagnostics.hfsr),
            ("dfsr", DFSR, &mut diagnostics.dfsr),
        ] {
            match self.read_word(address) {
                Ok(value) => *target = Some(if name == "ipsr" { value & 0x1ff } else { value }),
                Err(_) => diagnostics.unavailable.push(name.to_owned()),
            }
        }
        diagnostics
    }

    /// Performs the fresh DLL, export, probe, target, access, and HSS checklist.
    pub(crate) fn validation_report(&self, validation_runs: u64) -> ValidationReport {
        let spec = self
            .connected_spec
            .as_ref()
            .expect("validation requires an open target");
        let target_state_result = self.observe_target_state();
        let target_state = target_state_result
            .as_ref()
            .copied()
            .unwrap_or(TargetState::Unknown);
        let target_id = self.target_id;
        // SAFETY: the DLL getter has no arguments and does not touch the target.
        let dll_version = unsafe { (self.api.get_dll_version)() };
        // SAFETY: the target connection is active and the getter has no arguments.
        let actual_serial = unsafe { (self.api.get_serial)() };
        let mut checks = vec![
            passed_check(
                ValidationCheckKind::DllIdentity,
                format!("已加载冻结 DLL，API 版本 {dll_version}"),
            ),
            passed_check(
                ValidationCheckKind::RequiredExports,
                "2.5 所需导出已由唯一 gateway 解析",
            ),
            check(
                ValidationCheckKind::ProbeIdentity,
                actual_serial == spec.probe_serial(),
                format!("期望 {}，实际 {actual_serial}", spec.probe_serial()),
                "检查 probe.serial、USB 连接和探针占用状态",
            ),
            match target_state_result {
                Ok(_) => check(
                    ValidationCheckKind::TargetIdentity,
                    target_id.is_some_and(|value| value != 0),
                    format!("device={}，target_id={target_id:?}", spec.device()),
                    "检查目标供电、器件型号和 SWD/JTAG 接线",
                ),
                Err(error) => failed_check(
                    ValidationCheckKind::TargetIdentity,
                    error.to_string(),
                    "检查目标供电、器件型号、接口配置和调试链路",
                ),
            },
            passed_check(
                ValidationCheckKind::Interface,
                format!("{:?} {} kHz", spec.interface(), spec.speed_khz()),
            ),
        ];
        checks.push(match self.read_word(ICSR) {
            Ok(value) => passed_check(
                ValidationCheckKind::BackgroundAccess,
                format!("运行态读取 ICSR 成功：0x{value:08X}"),
            ),
            Err(error) => failed_check(
                ValidationCheckKind::BackgroundAccess,
                error.to_string(),
                "确认目标正在运行且支持后台内存访问",
            ),
        });
        let mut caps = HssCaps::default();
        // SAFETY: `caps` is writable and the frozen 6.98a ABI was verified in F0-A.
        let hss_result = unsafe { (self.api.hss_get_caps)(&raw mut caps) };
        checks.push(check(
            ValidationCheckKind::HssCapability,
            hss_result >= 0 && caps.max_blocks > 0 && caps.max_frequency_hz > 0,
            format!(
                "return={hss_result}，max_blocks={}，max_frequency_hz={}，flags={}",
                caps.max_blocks, caps.max_frequency_hz, caps.flags
            ),
            "确认使用冻结的 J-Link 6.98a DLL、已连接目标和支持 HSS 的探针",
        ));
        ValidationReport {
            valid: checks.iter().all(|item| item.passed),
            checks,
            target_state,
            target_id,
            validation_runs,
            recovery_notifications: Vec::new(),
        }
    }

    fn read_word(&self, address: u32) -> Result<u32, JlinkError> {
        let mut value = 0_u32;
        let mut status = 0_u8;
        // SAFETY: both output pointers are valid for one 32-bit word.
        let count = unsafe { (self.api.read_mem_u32)(address, 1, &raw mut value, &raw mut status) };
        if count != 1 || status != 0 {
            return Err(target_connection_error(format!(
                "读取 0x{address:08X} 失败：count={count}，status={status}"
            )));
        }
        Ok(value)
    }

    fn exec_command(&self, command: &str) -> Result<String, JlinkError> {
        let command = CString::new(command).map_err(|_| {
            JlinkError::new(ErrorCode::ConfigInvalid, "J-Link 命令包含 NUL 字符", false)
        })?;
        let mut output = [0_i8; 512];
        let output_len = i32::try_from(output.len()).expect("fixed output buffer fits i32");
        // SAFETY: pointers remain valid and the zeroed output buffer is fully sized.
        let result =
            unsafe { (self.api.exec_command)(command.as_ptr(), output.as_mut_ptr(), output_len) };
        if result < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_ExecCommand 返回 {result}"
            )));
        }
        // SAFETY: the output buffer was zeroed and supplied with its full length.
        Ok(unsafe { CStr::from_ptr(output.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }
}

impl Drop for DllGateway {
    fn drop(&mut self) {
        self.close_target();
        if !self.module.is_null() {
            // SAFETY: `module` was returned by LoadLibraryExW and is freed exactly once here.
            let _ = unsafe { FreeLibrary(self.module) };
        }
    }
}

fn passed_check(kind: ValidationCheckKind, detail: impl Into<String>) -> ValidationCheck {
    ValidationCheck {
        kind,
        passed: true,
        detail: detail.into(),
        recommendation: None,
    }
}

fn failed_check(
    kind: ValidationCheckKind,
    detail: impl Into<String>,
    recommendation: impl Into<String>,
) -> ValidationCheck {
    ValidationCheck {
        kind,
        passed: false,
        detail: detail.into(),
        recommendation: Some(recommendation.into()),
    }
}

fn check(
    kind: ValidationCheckKind,
    passed: bool,
    detail: impl Into<String>,
    recommendation: impl Into<String>,
) -> ValidationCheck {
    if passed {
        passed_check(kind, detail)
    } else {
        failed_check(kind, detail, recommendation)
    }
}

fn target_connection_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetConnectFailed, message, true)
}

fn target_recovery_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
}

#[cfg(test)]
fn test_injection_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
}
