use std::{
    ffi::{CStr, CString, c_char, c_void},
    marker::PhantomData,
    mem,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    rc::Rc,
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};

use jlink_domain::{
    CoreRegister, DeviceMemoryMap, ErrorCode, FaultDiagnostics, FirmwareImage, FlashRegion,
    HssCapabilities, JlinkError, MemoryRegion, MemoryRegionKind, ProgramAfter,
    TargetConnectionSpec, TargetInterface, TargetState, ValidationCheck, ValidationCheckEvidence,
    ValidationCheckKind, ValidationReport, validate_write_count,
};
use serde_json::json;
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
const DEVICE_AREA_COUNT: usize = 32;
const MAX_REGISTER_COUNT: usize = 256;
const PROGRAM_CHUNK_BYTES: usize = 64 * 1024;
const PROGRAM_CHUNK_BYTES_U64: u64 = 64 * 1024;
const DLL_DIAGNOSTIC_BYTES: usize = 8 * 1024;

type DllLogFn = unsafe extern "C" fn(*const c_char);
type OpenExFn = unsafe extern "C" fn(Option<DllLogFn>, Option<DllLogFn>) -> *const c_char;
type CloseFn = unsafe extern "C" fn();
type ExecCommandFn = unsafe extern "C" fn(*const c_char, *mut c_char, i32) -> i32;
type SelectTifFn = unsafe extern "C" fn(i32) -> i32;
type SetSpeedFn = unsafe extern "C" fn(i32);
type ConnectFn = unsafe extern "C" fn() -> i32;
type SelectProbeFn = unsafe extern "C" fn(u32) -> i32;
type GetU32Fn = unsafe extern "C" fn() -> u32;
type GetCharFn = unsafe extern "C" fn() -> i8;
type HaltFn = unsafe extern "C" fn() -> i8;
type GoFn = unsafe extern "C" fn();
type ResetFn = unsafe extern "C" fn() -> i32;
type GetRegisterListFn = unsafe extern "C" fn(*mut u32, i32) -> i32;
type GetRegisterNameFn = unsafe extern "C" fn(i32) -> *const c_char;
type ReadRegFn = unsafe extern "C" fn(i32) -> u32;
type ReadRegsFn = unsafe extern "C" fn(*const u32, *mut u32, *mut u8, u32) -> i32;
type WriteRegFn = unsafe extern "C" fn(i32, u32) -> i8;
type StepFn = unsafe extern "C" fn() -> i8;
type ReadMemU32Fn = unsafe extern "C" fn(u32, u32, *mut u32, *mut u8) -> i32;
type ReadMemFn = unsafe extern "C" fn(u32, u32, *mut u8) -> i32;
type WriteMemFn = unsafe extern "C" fn(u32, u32, *const u8) -> i32;
type DeviceGetIndexFn = unsafe extern "C" fn(*const c_char) -> i32;
type DeviceGetInfoFn = unsafe extern "C" fn(i32, *mut DeviceInfo) -> i32;
type BeginDownloadFn = unsafe extern "C" fn(u32);
type EndDownloadFn = unsafe extern "C" fn() -> i32;
type EraseChipFn = unsafe extern "C" fn() -> i32;
#[cfg(test)]
type WriteU32Fn = unsafe extern "C" fn(u32, u32) -> i32;
type HssGetCapsFn = unsafe extern "C" fn(*mut HssCaps) -> i32;
type HssStartFn = unsafe extern "C" fn(*mut HssBlock, i32, i32, i32) -> i32;
type HssReadFn = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type HssStopFn = unsafe extern "C" fn() -> i32;

struct DllDiagnosticBuffer {
    bytes: [u8; DLL_DIAGNOSTIC_BYTES],
    len: usize,
    truncated: bool,
}

impl DllDiagnosticBuffer {
    const fn new() -> Self {
        Self {
            bytes: [0; DLL_DIAGNOSTIC_BYTES],
            len: 0,
            truncated: false,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
    }

    fn append(&mut self, label: &[u8], message: &[u8]) {
        if message.is_empty() {
            return;
        }
        if self.len != 0 {
            self.copy_bytes(b"\n");
        }
        self.copy_bytes(label);
        self.copy_bytes(message);
    }

    fn copy_bytes(&mut self, source: &[u8]) {
        let remaining = self.bytes.len().saturating_sub(self.len);
        let count = remaining.min(source.len());
        self.bytes[self.len..self.len + count].copy_from_slice(&source[..count]);
        self.len += count;
        self.truncated |= count != source.len();
    }

    fn take(&mut self) -> Option<String> {
        if self.len == 0 && !self.truncated {
            return None;
        }
        let mut output = String::from_utf8_lossy(&self.bytes[..self.len]).into_owned();
        if self.truncated {
            output.push_str("\n[diagnostics truncated]");
        }
        self.clear();
        Some(output)
    }
}

static DLL_DIAGNOSTICS: Mutex<DllDiagnosticBuffer> = Mutex::new(DllDiagnosticBuffer::new());

unsafe extern "C" fn dll_log_callback(message: *const c_char) {
    capture_dll_diagnostic(b"log: ", message);
}

unsafe extern "C" fn dll_error_callback(message: *const c_char) {
    capture_dll_diagnostic(b"error: ", message);
}

fn capture_dll_diagnostic(label: &[u8], message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: the frozen callback ABI supplies a NUL-terminated string valid
    // for the duration of this callback.
    let message = unsafe { CStr::from_ptr(message) }.to_bytes();
    DLL_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .append(label, message);
}

fn reset_dll_diagnostics() {
    DLL_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

fn take_dll_diagnostics() -> Option<String> {
    DLL_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

fn attach_dll_diagnostics(mut error: JlinkError) -> JlinkError {
    if let Some(diagnostics) = take_dll_diagnostics() {
        error = error.with_detail("dll_diagnostics", serde_json::json!(diagnostics));
    }
    error
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct HssBlock {
    address: u32,
    byte_count: u32,
    flags: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct HssCaps {
    max_blocks: u32,
    max_frequency_hz: u32,
    flags: u32,
    reserved: [u32; 5],
}

struct HssApi {
    get_caps: Option<HssGetCapsFn>,
    start: Option<HssStartFn>,
    read: Option<HssReadFn>,
    stop: Option<HssStopFn>,
}

impl HssApi {
    fn load(module: HMODULE) -> Self {
        Self {
            get_caps: load_optional_symbol(module, b"JLINK_HSS_GetCaps\0"),
            start: load_optional_symbol(module, b"JLINK_HSS_Start\0"),
            read: load_optional_symbol(module, b"JLINK_HSS_Read\0"),
            stop: load_optional_symbol(module, b"JLINK_HSS_Stop\0"),
        }
    }

    fn missing_exports(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.get_caps.is_none() {
            missing.push("JLINK_HSS_GetCaps");
        }
        if self.start.is_none() {
            missing.push("JLINK_HSS_Start");
        }
        if self.read.is_none() {
            missing.push("JLINK_HSS_Read");
        }
        if self.stop.is_none() {
            missing.push("JLINK_HSS_Stop");
        }
        missing
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct DeviceArea {
    address: u32,
    size: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct DeviceInfo {
    size_of_struct: u32,
    name: *const c_char,
    core_id: u32,
    flash_address: u32,
    ram_address: u32,
    endian_mode: u8,
    flash_size: u32,
    ram_size: u32,
    manufacturer: *const c_char,
    flash_areas: [DeviceArea; DEVICE_AREA_COUNT],
    ram_areas: [DeviceArea; DEVICE_AREA_COUNT],
    core: u32,
}

impl Default for DeviceInfo {
    fn default() -> Self {
        Self {
            size_of_struct: u32::try_from(mem::size_of::<Self>())
                .expect("J-Link device info ABI fits u32"),
            name: ptr::null(),
            core_id: 0,
            flash_address: 0,
            ram_address: 0,
            endian_mode: 0,
            flash_size: 0,
            ram_size: 0,
            manufacturer: ptr::null(),
            flash_areas: [DeviceArea::default(); DEVICE_AREA_COUNT],
            ram_areas: [DeviceArea::default(); DEVICE_AREA_COUNT],
            core: 0,
        }
    }
}

struct Api {
    open_ex: OpenExFn,
    close: CloseFn,
    exec_command: ExecCommandFn,
    select_tif: SelectTifFn,
    set_speed: SetSpeedFn,
    connect: ConnectFn,
    select_probe: SelectProbeFn,
    get_serial: GetU32Fn,
    get_target_id: GetU32Fn,
    get_dll_version: GetU32Fn,
    is_connected: GetCharFn,
    is_halted: GetCharFn,
    halt: HaltFn,
    go: GoFn,
    reset: ResetFn,
    get_register_list: GetRegisterListFn,
    get_register_name: GetRegisterNameFn,
    read_reg: ReadRegFn,
    read_regs: ReadRegsFn,
    write_reg: WriteRegFn,
    step: StepFn,
    read_mem: ReadMemFn,
    read_mem_u32: ReadMemU32Fn,
    write_mem: WriteMemFn,
    device_get_index: DeviceGetIndexFn,
    device_get_info: DeviceGetInfoFn,
    begin_download: BeginDownloadFn,
    end_download: EndDownloadFn,
    erase_chip: EraseChipFn,
    #[cfg(test)]
    write_u32: WriteU32Fn,
    hss: HssApi,
}

impl Api {
    fn load(module: HMODULE) -> Result<Self, JlinkError> {
        Ok(Self {
            open_ex: load_symbol(module, b"JLINKARM_OpenEx\0")?,
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
            get_register_list: load_symbol(module, b"JLINKARM_GetRegisterList\0")?,
            get_register_name: load_symbol(module, b"JLINKARM_GetRegisterName\0")?,
            read_reg: load_symbol(module, b"JLINKARM_ReadReg\0")?,
            read_regs: load_symbol(module, b"JLINKARM_ReadRegs\0")?,
            write_reg: load_symbol(module, b"JLINKARM_WriteReg\0")?,
            step: load_symbol(module, b"JLINKARM_Step\0")?,
            read_mem: load_symbol(module, b"JLINKARM_ReadMem\0")?,
            read_mem_u32: load_symbol(module, b"JLINKARM_ReadMemU32\0")?,
            write_mem: load_symbol(module, b"JLINKARM_WriteMem\0")?,
            device_get_index: load_symbol(module, b"JLINKARM_DEVICE_GetIndex\0")?,
            device_get_info: load_symbol(module, b"JLINKARM_DEVICE_GetInfo\0")?,
            begin_download: load_symbol(module, b"JLINKARM_BeginDownload\0")?,
            end_download: load_symbol(module, b"JLINKARM_EndDownload\0")?,
            erase_chip: load_symbol(module, b"JLINK_EraseChip\0")?,
            #[cfg(test)]
            write_u32: load_symbol(module, b"JLINKARM_WriteU32\0")?,
            hss: HssApi::load(module),
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

fn load_optional_symbol<T: Copy>(module: HMODULE, name: &'static [u8]) -> Option<T> {
    // SAFETY: `module` is live and `name` is a static NUL-terminated export name.
    let symbol = unsafe { GetProcAddress(module, name.as_ptr()) }?;
    debug_assert_eq!(mem::size_of::<T>(), mem::size_of_val(&symbol));
    // SAFETY: each HSS signature is frozen by F0-A and T-P3-ABI. Absence remains
    // local to HSS capability instead of preventing ordinary DLL use.
    Some(unsafe { mem::transmute_copy(&symbol) })
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
    hss_started: bool,
    path: PathBuf,
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
            hss_started: false,
            path: path.to_path_buf(),
            _single_thread: PhantomData,
        })
    }

    /// Reports whether this gateway currently owns a loaded module.
    pub(crate) const fn is_loaded(&self) -> bool {
        !self.module.is_null()
    }

    /// Reads and validates the exact HSS capability set needed before Start.
    pub(crate) fn hss_capabilities(&self) -> Result<HssCapabilities, JlinkError> {
        let missing_exports = self.api.hss.missing_exports();
        if !missing_exports.is_empty() {
            return Err(JlinkError::new(
                ErrorCode::DllExportMissing,
                "当前 DLL 缺少必要 HSS 导出，未启动采集",
                false,
            )
            .with_detail("missing_exports", json!(missing_exports))
            .with_detail("dll_path", json!(self.path.display().to_string()))
            .with_detail(
                "recommendation",
                json!("使用身份已冻结且包含 GetCaps/Start/Read/Stop 的 J-Link 6.98a DLL"),
            ));
        }
        let mut caps = HssCaps::default();
        let get_caps = self
            .api
            .hss
            .get_caps
            .expect("complete HSS export set contains GetCaps");
        // SAFETY: `caps` is writable and the frozen 6.98a ABI was verified in F0-A.
        let result = unsafe { get_caps(&raw mut caps) };
        if result < 0 {
            return Err(JlinkError::new(
                ErrorCode::HssUnsupported,
                format!("JLINK_HSS_GetCaps 返回失败状态 {result}"),
                false,
            )
            .with_detail("dll_path", json!(self.path.display().to_string())));
        }
        HssCapabilities::frozen_698a(
            caps.max_blocks,
            caps.max_frequency_hz,
            caps.flags,
            caps.reserved,
        )
    }

    /// Starts one validated HSS plan through the frozen 6.98a ABI.
    pub(crate) fn start_hss(
        &mut self,
        plan: &jlink_domain::HssStartPlan,
    ) -> Result<(), JlinkError> {
        if self.hss_started {
            return Err(JlinkError::new(
                ErrorCode::OperationConflict,
                "DLL 已持有一个活动 HSS 采集",
                true,
            ));
        }
        let mut blocks = plan
            .variables()
            .iter()
            .map(|variable| {
                let access = variable.access_plan();
                Ok(HssBlock {
                    address: u32::try_from(access.address()).map_err(|_| {
                        JlinkError::new(
                            ErrorCode::HssUnsupported,
                            "HSS 变量地址超出冻结 32-bit ABI",
                            false,
                        )
                    })?,
                    byte_count: u32::try_from(access.byte_size()).map_err(|_| {
                        JlinkError::new(
                            ErrorCode::HssUnsupported,
                            "HSS 变量长度超出冻结 32-bit ABI",
                            false,
                        )
                    })?,
                    flags: jlink_domain::HSS_BLOCK_FLAGS_DEFAULT,
                    reserved: 0,
                })
            })
            .collect::<Result<Vec<_>, JlinkError>>()?;
        let block_count = i32::try_from(blocks.len()).map_err(|_| {
            JlinkError::new(ErrorCode::HssUnsupported, "HSS block 数量无法表示", false)
        })?;
        let period_us = i32::try_from(plan.period_us()).map_err(|_| {
            JlinkError::new(ErrorCode::HssUnsupported, "HSS period_us 无法表示", false)
        })?;
        let start = self
            .api
            .hss
            .start
            .expect("HSS preflight requires the complete export set");
        // SAFETY: block layout and call signature are frozen by F0-A; the unique
        // gateway owns the live target and serializes every DLL call.
        let result = unsafe {
            start(
                blocks.as_mut_ptr(),
                block_count,
                period_us,
                jlink_domain::HSS_START_FLAGS_698A_MAINLINE,
            )
        };
        if result < 0 {
            return Err(hss_start_error(result));
        }
        self.hss_started = true;
        Ok(())
    }

    /// Drains currently buffered HSS bytes, including the frozen zero-return case.
    pub(crate) fn read_hss(
        &mut self,
        buffer: &mut [u8],
        record_bytes: usize,
    ) -> Result<usize, JlinkError> {
        const SENTINEL: u8 = 0xA5;
        if record_bytes == 0 || record_bytes > buffer.len() {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                "HSS 读取缓冲区小于一个完整帧",
                false,
            ));
        }
        buffer.fill(SENTINEL);
        let read = self
            .api
            .hss
            .read
            .expect("HSS preflight requires the complete export set");
        // SAFETY: `buffer` is writable for its declared length and the ABI is frozen by F0-A.
        let result = unsafe {
            read(
                buffer.as_mut_ptr().cast::<c_void>(),
                u32::try_from(buffer.len()).expect("fixed HSS buffer fits u32"),
            )
        };
        if result < 0 {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                format!("JLINK_HSS_Read 返回失败状态 {result}"),
                false,
            ));
        }
        let returned = usize::try_from(result)
            .map_err(|_| JlinkError::new(ErrorCode::FrameInvalid, "HSS 返回长度无法表示", false))?;
        if returned > buffer.len() {
            return Err(JlinkError::new(
                ErrorCode::FrameInvalid,
                "JLINK_HSS_Read 返回长度超过缓冲区",
                false,
            ));
        }
        if returned == 0 && buffer[..record_bytes].iter().any(|byte| *byte != SENTINEL) {
            Ok(record_bytes)
        } else {
            Ok(returned)
        }
    }

    /// Stops the active HSS stream exactly once before tail drain.
    pub(crate) fn stop_hss(&mut self) -> Result<(), JlinkError> {
        if !self.hss_started {
            return Err(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "DLL 没有可停止的活动 HSS 采集",
                false,
            ));
        }
        let stop = self
            .api
            .hss
            .stop
            .expect("HSS preflight requires the complete export set");
        // Mark consumed before interpreting the result so cleanup never retries a
        // failed Stop against an uncertain DLL state.
        self.hss_started = false;
        // SAFETY: the unique gateway owns the matching successful Start call.
        let result = unsafe { stop() };
        if result < 0 {
            return Err(JlinkError::new(
                ErrorCode::TargetRecoveryFailed,
                format!("JLINK_HSS_Stop 返回失败状态 {result}"),
                false,
            ));
        }
        Ok(())
    }

    /// Reads authoritative Flash regions from the loaded J-Link device database.
    ///
    /// This call does not open a probe or access the target. The returned ranges
    /// are therefore suitable for completing all boundary checks before a Flash
    /// side effect begins.
    pub(crate) fn device_flash_regions(
        &mut self,
        device: &str,
    ) -> Result<Vec<FlashRegion>, JlinkError> {
        let info = self.device_info(device)?;
        let regions = info
            .flash_areas
            .iter()
            .take_while(|area| area.size != 0)
            .map(|area| FlashRegion::new(u64::from(area.address), u64::from(area.size)))
            .collect::<Result<Vec<_>, _>>()?;
        if regions.is_empty() {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "J-Link 设备数据库没有提供可验证的 Flash 区域",
                false,
            ));
        }
        Ok(regions)
    }

    /// Reads authoritative Flash and RAM regions for ordinary access classification.
    pub(crate) fn device_memory_map(
        &mut self,
        device: &str,
    ) -> Result<DeviceMemoryMap, JlinkError> {
        let info = self.device_info(device)?;
        let mut regions = Vec::new();
        regions.extend(
            info.flash_areas
                .iter()
                .take_while(|area| area.size != 0)
                .map(|area| {
                    MemoryRegion::new(
                        u64::from(area.address),
                        u64::from(area.size),
                        MemoryRegionKind::Flash,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        regions.extend(
            info.ram_areas
                .iter()
                .take_while(|area| area.size != 0)
                .map(|area| {
                    MemoryRegion::new(
                        u64::from(area.address),
                        u64::from(area.size),
                        MemoryRegionKind::Ram,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
        if !regions
            .iter()
            .any(|region| region.kind() == MemoryRegionKind::Flash)
            || !regions
                .iter()
                .any(|region| region.kind() == MemoryRegionKind::Ram)
        {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "J-Link 设备数据库没有同时提供 Flash 和 RAM 区域",
                false,
            ));
        }
        DeviceMemoryMap::new(regions)
    }

    fn device_info(&mut self, device: &str) -> Result<DeviceInfo, JlinkError> {
        let device = CString::new(device).map_err(|_| {
            JlinkError::new(
                ErrorCode::ConfigInvalid,
                "target.device 包含 NUL 字符",
                false,
            )
        })?;
        // SAFETY: `device` is NUL-terminated and this no-target DLL call is
        // serialized by the unique gateway.
        let index = unsafe { (self.api.device_get_index)(device.as_ptr()) };
        if index < 0 {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "J-Link 设备数据库中不存在配置的 target.device",
                false,
            ));
        }
        let mut info = DeviceInfo::default();
        // SAFETY: the size-versioned structure matches the frozen x64 ABI and
        // remains writable for the synchronous call.
        let result = unsafe { (self.api.device_get_info)(index, &raw mut info) };
        if result != 0 {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                format!("JLINKARM_DEVICE_GetInfo 返回 {result}"),
                false,
            ));
        }
        Ok(info)
    }

    /// Programs every normalized image segment through J-Link's device algorithm.
    ///
    /// The caller must validate all segments against [`Self::device_flash_regions`]
    /// before invoking this method. Any failure after `BeginDownload` is reported
    /// as execution-uncertain because target side effects may already exist.
    pub(crate) fn program_image(&mut self, image: &FirmwareImage) -> Result<(), JlinkError> {
        reset_dll_diagnostics();
        self.prepare_flash_operation()
            .map_err(attach_dll_diagnostics)?;
        // SAFETY: the connected target is uniquely owned and flags=0 selects the
        // frozen default download behavior.
        unsafe { (self.api.begin_download)(0) };
        let write_result = image
            .segments()
            .iter()
            .try_for_each(|segment| self.write_download_bytes(segment.address(), segment.data()));
        // SAFETY: every BeginDownload path is paired with exactly one EndDownload.
        let end_result = unsafe { (self.api.end_download)() };
        match (write_result, end_result) {
            (Ok(()), value) if value >= 0 => {
                let _ = take_dll_diagnostics();
                Ok(())
            }
            (Err(error), value) => Err(attach_dll_diagnostics(execution_uncertain_error(format!(
                "Flash 写入未能完成：{error}；JLINKARM_EndDownload 返回 {value}"
            )))),
            (Ok(()), value) => Err(attach_dll_diagnostics(execution_uncertain_error(format!(
                "JLINKARM_EndDownload 返回 {value}"
            )))),
        }
    }

    /// Erases all always-present device Flash banks through the J-Link algorithm.
    pub(crate) fn erase_chip(&mut self) -> Result<(), JlinkError> {
        reset_dll_diagnostics();
        self.prepare_flash_operation()
            .map_err(attach_dll_diagnostics)?;
        // SAFETY: the connected target is uniquely owned by this gateway.
        let result = unsafe { (self.api.erase_chip)() };
        if result < 0 {
            return Err(attach_dll_diagnostics(execution_uncertain_error(format!(
                "JLINK_EraseChip 返回 {result}"
            ))));
        }
        let _ = take_dll_diagnostics();
        Ok(())
    }

    /// Erases one validated byte range using J-Link's Flash read-modify-write path.
    ///
    /// Writing erased bytes within a download transaction delegates sector erase
    /// and preservation of bytes outside the requested range to the selected
    /// device algorithm. Hardware evidence must confirm this behavior for each
    /// frozen DLL/device fingerprint before release.
    pub(crate) fn erase_range(&mut self, address: u64, length: u64) -> Result<(), JlinkError> {
        reset_dll_diagnostics();
        self.prepare_flash_operation()
            .map_err(attach_dll_diagnostics)?;
        // SAFETY: the connected target is uniquely owned and flags=0 selects the
        // frozen default download behavior.
        unsafe { (self.api.begin_download)(0) };
        let erased = vec![0xff_u8; PROGRAM_CHUNK_BYTES];
        let mut remaining = length;
        let mut current = address;
        let write_result: Result<(), JlinkError> = (|| {
            while remaining > 0 {
                let count = usize::try_from(remaining.min(PROGRAM_CHUNK_BYTES_U64))
                    .map_err(|_| execution_uncertain_error("范围擦除块长度无法表示为 usize"))?;
                self.write_download_bytes(current, &erased[..count])?;
                let count = u64::try_from(count)
                    .map_err(|_| execution_uncertain_error("范围擦除块长度无法表示为 u64"))?;
                current = current
                    .checked_add(count)
                    .ok_or_else(|| execution_uncertain_error("范围擦除地址溢出"))?;
                remaining -= count;
            }
            Ok(())
        })();
        // SAFETY: every BeginDownload path is paired with exactly one EndDownload.
        let end_result = unsafe { (self.api.end_download)() };
        match (write_result, end_result) {
            (Ok(()), value) if value >= 0 => {
                let _ = take_dll_diagnostics();
                Ok(())
            }
            (Err(error), value) => Err(attach_dll_diagnostics(execution_uncertain_error(format!(
                "范围擦除未能完成：{error}；JLINKARM_EndDownload 返回 {value}"
            )))),
            (Ok(()), value) => Err(attach_dll_diagnostics(execution_uncertain_error(format!(
                "范围擦除的 JLINKARM_EndDownload 返回 {value}"
            )))),
        }
    }

    /// Reads one complete target range without truncation.
    pub(crate) fn read_bytes(
        &mut self,
        address: u64,
        length: usize,
    ) -> Result<Vec<u8>, JlinkError> {
        let mut output = vec![0_u8; length];
        let mut offset = 0_usize;
        while offset < length {
            let count = (length - offset).min(PROGRAM_CHUNK_BYTES);
            let offset_u64 = u64::try_from(offset).map_err(|_| {
                JlinkError::new(ErrorCode::ValueInvalid, "目标读取偏移无法表示", false)
            })?;
            let current = address.checked_add(offset_u64).ok_or_else(|| {
                JlinkError::new(ErrorCode::AddressOutOfRange, "目标读取地址溢出", false)
            })?;
            let current = u32::try_from(current).map_err(|_| {
                JlinkError::new(
                    ErrorCode::AddressOutOfRange,
                    "目标读取地址超出 Cortex-M 地址范围",
                    false,
                )
            })?;
            let count_u32 = u32::try_from(count).expect("fixed read chunk fits u32");
            // SAFETY: the output slice is writable for `count` bytes and the
            // connected target is uniquely owned by this gateway.
            let result =
                unsafe { (self.api.read_mem)(current, count_u32, output[offset..].as_mut_ptr()) };
            validate_read_mem_result(current, count, result)?;
            offset += count;
        }
        Ok(output)
    }

    /// Writes one complete ordinary RAM or MMIO range and rejects short writes.
    pub(crate) fn write_bytes(&mut self, address: u64, bytes: &[u8]) -> Result<(), JlinkError> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let count = (bytes.len() - offset).min(PROGRAM_CHUNK_BYTES);
            let offset_u64 = u64::try_from(offset).map_err(|_| {
                JlinkError::new(ErrorCode::ValueInvalid, "内存写入偏移无法表示", false)
            })?;
            let current = address.checked_add(offset_u64).ok_or_else(|| {
                JlinkError::new(ErrorCode::AddressOutOfRange, "内存写入地址溢出", false)
            })?;
            let current_u32 = u32::try_from(current).map_err(|_| {
                JlinkError::new(
                    ErrorCode::AddressOutOfRange,
                    "内存写入地址超出 Cortex-M 地址范围",
                    false,
                )
            })?;
            let count_u32 = u32::try_from(count).expect("fixed write chunk fits u32");
            // SAFETY: the input slice is readable for `count` bytes and the
            // connected target is uniquely owned by this gateway.
            let result =
                unsafe { (self.api.write_mem)(current_u32, count_u32, bytes[offset..].as_ptr()) };
            if let Err(mut error) = validate_write_count(current, count, result) {
                let actual_in_chunk = usize::try_from(result.max(0)).unwrap_or(0).min(count);
                let actual_total = offset.saturating_add(actual_in_chunk);
                error = error
                    .with_detail("requested_length", serde_json::json!(bytes.len()))
                    .with_detail("actual_length", serde_json::json!(actual_total));
                return Err(error);
            }
            offset += count;
        }
        Ok(())
    }

    /// Applies the explicit successful post-program target state.
    pub(crate) fn apply_program_after(
        &mut self,
        after: ProgramAfter,
    ) -> Result<TargetState, JlinkError> {
        let (actual, expected) = match after {
            ProgramAfter::None => (self.observe_target_state()?, None),
            ProgramAfter::ResetHalt => (self.reset_halt_and_observe()?, Some(TargetState::Halted)),
            ProgramAfter::ResetRun => (self.reset_run_and_observe()?, Some(TargetState::Running)),
        };
        if expected.is_some_and(|expected| actual != expected) {
            return Err(target_recovery_error(format!(
                "烧录后状态不符合请求：after={after:?}，实际={actual:?}"
            )));
        }
        Ok(actual)
    }

    fn prepare_flash_operation(&mut self) -> Result<(), JlinkError> {
        if self.connected_spec.is_none() || !self.is_connected()? {
            return Err(target_connection_error(
                "Flash 操作要求同一 DLL 会话已确认具体器件选择和目标连接",
            ));
        }
        self.reset_halt_for_flash()
    }

    /// Establishes the deterministic halted boundary required around Flash work.
    pub(crate) fn reset_halt_for_flash(&mut self) -> Result<(), JlinkError> {
        require_flash_reset_halted(self.reset_halt_and_observe()?)
    }

    fn write_download_bytes(&mut self, address: u64, bytes: &[u8]) -> Result<(), JlinkError> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let count = (bytes.len() - offset).min(PROGRAM_CHUNK_BYTES);
            let offset_u64 = u64::try_from(offset)
                .map_err(|_| execution_uncertain_error("Flash 写入偏移无法表示"))?;
            let current = address
                .checked_add(offset_u64)
                .ok_or_else(|| execution_uncertain_error("Flash 写入地址溢出"))?;
            let current = u32::try_from(current)
                .map_err(|_| execution_uncertain_error("Flash 写入地址超出 u32"))?;
            let count_u32 = u32::try_from(count).expect("fixed write chunk fits u32");
            // SAFETY: the input slice is readable for `count` bytes and the
            // download transaction is uniquely owned by this gateway.
            let result =
                unsafe { (self.api.write_mem)(current, count_u32, bytes[offset..].as_ptr()) };
            if result != i32::try_from(count).expect("fixed write chunk fits i32") {
                return Err(execution_uncertain_error(format!(
                    "JLINKARM_WriteMem(0x{current:08X}, {count}) 返回 {result}"
                )));
            }
            offset += count;
        }
        Ok(())
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
        reset_dll_diagnostics();
        // SAFETY: both callbacks match the frozen 6.98a callback ABI and only
        // append bytes to a fixed-capacity diagnostic buffer without DLL reentry.
        let open_error =
            unsafe { (self.api.open_ex)(Some(dll_log_callback), Some(dll_error_callback)) };
        if !open_error.is_null() {
            // SAFETY: a non-null OpenEx result is a DLL-owned NUL-terminated
            // error string valid at least until the synchronous call returns.
            let message = unsafe { CStr::from_ptr(open_error) }.to_string_lossy();
            return Err(attach_dll_diagnostics(target_connection_error(format!(
                "JLINKARM_OpenEx 失败：{message}"
            ))));
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
        if self.hss_started
            && let Err(error) = self.stop_hss()
        {
            eprintln!("HSS 安全停止失败，跳过后续目标 DLL 调用：{error}");
            self.opened = false;
            self.connected_spec = None;
            self.target_id = None;
            return;
        }
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
        classify_observed_target_state(halted > 0, || self.read_word(ICSR))
    }

    /// Returns the register indices and names reported by the active target.
    pub(crate) fn register_entries(&self) -> Result<Vec<(u32, String)>, JlinkError> {
        if !self.opened || !self.is_connected()? {
            return Err(target_connection_error("读取寄存器目录要求已建立目标连接"));
        }
        let mut indices = [0_u32; MAX_REGISTER_COUNT];
        let capacity = i32::try_from(indices.len()).expect("register capacity fits i32");
        // SAFETY: the buffer is writable for `capacity` indices and the active
        // target owns the register catalog for this serialized DLL session.
        let count = unsafe { (self.api.get_register_list)(indices.as_mut_ptr(), capacity) };
        if count < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_GetRegisterList 返回 {count}"
            )));
        }
        let count = usize::try_from(count).expect("non-negative register count fits usize");
        if count > indices.len() {
            return Err(target_connection_error(format!(
                "J-Link 寄存器数量 {count} 超过固定上限 {}",
                indices.len()
            )));
        }
        indices[..count]
            .iter()
            .copied()
            .map(|index| {
                let name_index = i32::try_from(index).map_err(|_| {
                    target_connection_error(format!(
                        "J-Link 寄存器索引 {index} 超出冻结名称接口范围"
                    ))
                })?;
                // SAFETY: `index` came from the active target's register list.
                let name = unsafe { (self.api.get_register_name)(name_index) };
                if name.is_null() {
                    return Err(target_connection_error(format!(
                        "JLINKARM_GetRegisterName({index}) 返回空指针"
                    )));
                }
                // SAFETY: J-Link owns the NUL-terminated name for the loaded DLL lifetime.
                let name = unsafe { CStr::from_ptr(name) }
                    .to_string_lossy()
                    .into_owned();
                if name.is_empty() {
                    return Err(target_connection_error(format!(
                        "JLINKARM_GetRegisterName({index}) 返回空名称"
                    )));
                }
                Ok((index, name))
            })
            .collect()
    }

    fn register_index(&self, register: CoreRegister) -> Result<u32, JlinkError> {
        self.register_entries()?
            .into_iter()
            .find_map(|(index, name)| (name == register.jlink_name()).then_some(index))
            .ok_or_else(|| {
                JlinkError::new(
                    ErrorCode::RegisterNotFound,
                    format!("当前目标不支持核心寄存器 {}", register.canonical_name()),
                    false,
                )
                .with_detail("register", serde_json::json!(register.canonical_name()))
            })
    }

    /// Reads one target-supported canonical core register without truncation.
    pub(crate) fn read_register(&mut self, register: CoreRegister) -> Result<u32, JlinkError> {
        let index = self.register_index(register)?;
        let mut value = 0_u32;
        let mut status = u8::MAX;
        // SAFETY: all buffers hold one element and the unique gateway owns the
        // active target session for the complete call.
        let result =
            unsafe { (self.api.read_regs)(&raw const index, &raw mut value, &raw mut status, 1) };
        if result < 0 {
            return Err(target_connection_error(format!(
                "JLINKARM_ReadRegs({}) 返回 {result}",
                register.canonical_name()
            )));
        }
        if status != 0 {
            let target_state = self.observe_target_state().map_err(|error| {
                error
                    .with_detail("register", serde_json::json!(register.canonical_name()))
                    .with_detail("dll_status", serde_json::json!(status))
            })?;
            return Err(classify_register_read_failure(
                register,
                status,
                target_state,
            ));
        }
        Ok(value)
    }

    /// Writes one target-supported canonical core register.
    pub(crate) fn write_register(
        &mut self,
        register: CoreRegister,
        value: u32,
    ) -> Result<(), JlinkError> {
        register.ensure_writable()?;
        let index = self.register_index(register)?;
        let write_index = i32::try_from(index).map_err(|_| {
            target_connection_error(format!("J-Link 寄存器索引 {index} 超出冻结写入接口范围"))
        })?;
        // SAFETY: the register index came from the active target catalog and
        // the unique gateway serializes the frozen two-argument ABI.
        let status = unsafe { (self.api.write_reg)(write_index, value) };
        if status != 0 {
            return Err(execution_uncertain_error(format!(
                "JLINKARM_WriteReg({}, 0x{value:08X}) 返回 {status}，寄存器可能已改变",
                register.canonical_name()
            ))
            .with_detail("register", serde_json::json!(register.canonical_name()))
            .with_detail("dll_status", serde_json::json!(status)));
        }
        Ok(())
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
        let halt_status = unsafe { (self.api.halt)() };
        self.wait_until_halted().map_err(|error| {
            error.with_detail("dll_halt_status", serde_json::json!(halt_status))
        })?;
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
        let halt_status = unsafe { (self.api.halt)() };
        self.wait_until_halted().map_err(|error| {
            error.with_detail("dll_halt_status", serde_json::json!(halt_status))
        })?;
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
        reset_dll_diagnostics();
        // SAFETY: the target connection is active and the reset call is serialized.
        let reset_status = unsafe { (self.api.reset)() };
        if reset_status < 0 {
            return Err(attach_dll_diagnostics(target_recovery_error(format!(
                "JLINKARM_Reset 返回 {reset_status}"
            ))));
        }
        // SAFETY: the successful reset left the uniquely owned target ready to run.
        unsafe { (self.api.go)() };
        self.wait_for_stable_state()
    }

    /// Resets and explicitly leaves the target halted.
    pub(crate) fn reset_halt_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        reset_dll_diagnostics();
        // SAFETY: the target connection is active and the reset call is serialized.
        let reset_status = unsafe { (self.api.reset)() };
        if reset_status < 0 {
            return Err(attach_dll_diagnostics(target_recovery_error(format!(
                "JLINKARM_Reset 返回 {reset_status}"
            ))));
        }
        // SAFETY: the successful reset and halt are serialized through the gateway.
        let halt_status = unsafe { (self.api.halt)() };
        self.wait_until_halted().map_err(|error| {
            error
                .with_detail("dll_reset_status", serde_json::json!(reset_status))
                .with_detail("dll_halt_status", serde_json::json!(halt_status))
        })?;
        self.observe_target_state()
    }

    /// Executes exactly one instruction from an already halted target.
    pub(crate) fn step_and_observe(&mut self) -> Result<TargetState, JlinkError> {
        let before = self.observe_target_state()?;
        if before != TargetState::Halted {
            return Err(JlinkError::new(
                ErrorCode::InvalidStateTransition,
                "step 要求目标已经 halted；请先显式调用 halt",
                true,
            )
            .with_detail("expected", serde_json::json!("halted"))
            .with_detail("actual", serde_json::json!(before)));
        }
        // SAFETY: the target is confirmed halted and the no-argument ABI is
        // serialized by the unique gateway.
        let status = unsafe { (self.api.step)() };
        let after = self.observe_target_state()?;
        if status != 0 || after != TargetState::Halted {
            let cleanup = if after == TargetState::Halted {
                Ok(after)
            } else {
                self.halt_and_observe()
            };
            return Err(execution_uncertain_error(format!(
                "JLINKARM_Step 返回 {status}，step 后状态为 {after:?}，安全暂停结果为 {cleanup:?}"
            ))
            .with_detail("dll_status", serde_json::json!(status))
            .with_detail("observed_state", serde_json::json!(after)));
        }
        Ok(after)
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

    /// Performs dynamic checks and reuses only explicitly supplied fingerprint-bound evidence.
    pub(crate) fn validation_report(
        &self,
        validation_runs: u64,
        reusable_checks: Option<&[ValidationCheck]>,
        running_background_access: Option<&ValidationCheck>,
    ) -> ValidationReport {
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
        let mut checks = vec![
            reusable_check(reusable_checks, ValidationCheckKind::DllIdentity).unwrap_or_else(
                || {
                    // SAFETY: the DLL getter has no arguments and does not touch the target.
                    let dll_version = unsafe { (self.api.get_dll_version)() };
                    passed_check(
                        ValidationCheckKind::DllIdentity,
                        format!("已加载冻结 DLL，API 版本 {dll_version}"),
                    )
                },
            ),
            reusable_check(reusable_checks, ValidationCheckKind::RequiredExports).unwrap_or_else(
                || {
                    passed_check(
                        ValidationCheckKind::RequiredExports,
                        "2.5 所需导出已由唯一 gateway 解析",
                    )
                },
            ),
            reusable_check(reusable_checks, ValidationCheckKind::ProbeIdentity).unwrap_or_else(
                || {
                    // SAFETY: the target connection is active and the getter has no arguments.
                    let actual_serial = unsafe { (self.api.get_serial)() };
                    check(
                        ValidationCheckKind::ProbeIdentity,
                        actual_serial == spec.probe_serial(),
                        format!("期望 {}，实际 {actual_serial}", spec.probe_serial()),
                        "检查 probe.serial、USB 连接和探针占用状态",
                    )
                },
            ),
            match target_state_result {
                Ok(_) => reusable_check(reusable_checks, ValidationCheckKind::TargetIdentity)
                    .unwrap_or_else(|| {
                        check(
                            ValidationCheckKind::TargetIdentity,
                            target_id.is_some_and(|value| value != 0),
                            format!("device={}，target_id={target_id:?}", spec.device()),
                            "检查目标供电、器件型号和 SWD/JTAG 接线",
                        )
                    }),
                Err(error) => failed_check(
                    ValidationCheckKind::TargetIdentity,
                    error.to_string(),
                    "检查目标供电、器件型号、接口配置和调试链路",
                ),
            },
            reusable_check(reusable_checks, ValidationCheckKind::Interface).unwrap_or_else(|| {
                passed_check(
                    ValidationCheckKind::Interface,
                    format!("{:?} {} kHz", spec.interface(), spec.speed_khz()),
                )
            }),
        ];
        checks.push(self.background_access_check(target_state, running_background_access));
        checks.push(self.hss_validation_check(reusable_checks));
        ValidationReport {
            valid: checks.iter().all(|item| item.passed),
            checks,
            target_state,
            target_id,
            validation_runs,
            recovery_notifications: Vec::new(),
        }
    }

    fn background_access_check(
        &self,
        target_state: TargetState,
        running_background_access: Option<&ValidationCheck>,
    ) -> ValidationCheck {
        match target_state {
            TargetState::Running => match self.read_word(ICSR) {
                Ok(value) => passed_check(
                    ValidationCheckKind::BackgroundAccess,
                    format!("本次在 running 状态执行 ICSR 读取成功：0x{value:08X}"),
                ),
                Err(error) => failed_check(
                    ValidationCheckKind::BackgroundAccess,
                    error.to_string(),
                    "确认目标正在运行且支持后台内存访问",
                ),
            },
            TargetState::Halted | TargetState::HardFault => running_background_access
                .filter(|check| check.passed)
                .map_or_else(
                    || {
                        failed_check(
                            ValidationCheckKind::BackgroundAccess,
                            format!(
                                "目标当前为 {target_state:?}；没有同一连接中已成功的运行态后台访问证据"
                            ),
                            "先恢复 running 并执行 validate，再显式 halt",
                        )
                    },
                    |check| {
                        let mut reused = check.clone();
                        reused.evidence = ValidationCheckEvidence::Reused;
                        reused.detail = format!(
                            "目标当前为 {target_state:?}；复用同一连接中已成功的运行态证据：{}",
                            check.detail
                        );
                        reused
                    },
                ),
            TargetState::Unknown => failed_check(
                ValidationCheckKind::BackgroundAccess,
                "目标状态 unknown，不能执行或复用运行态后台访问检查",
                "重新建立可信目标连接后再验证",
            ),
        }
    }

    fn hss_validation_check(&self, reusable_checks: Option<&[ValidationCheck]>) -> ValidationCheck {
        if let Some(check) = reusable_check(reusable_checks, ValidationCheckKind::HssCapability) {
            return check;
        }
        match self.hss_capabilities() {
            Ok(caps) => passed_check(
                ValidationCheckKind::HssCapability,
                format!(
                    "max_blocks={}，max_frequency_hz={}，source_timestamp={} Hz/{} us，monotonic={}",
                    caps.max_blocks(),
                    caps.max_frequency_hz(),
                    caps.source_timestamp_frequency_hz(),
                    caps.source_timestamp_resolution_us(),
                    caps.source_timestamp_monotonic()
                ),
            ),
            Err(error) => failed_check(
                ValidationCheckKind::HssCapability,
                error.to_string(),
                "使用包含 GetCaps/Start/Read/Stop 的冻结 J-Link DLL；普通调试能力不受影响",
            ),
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

    fn exec_command(&self, command: &str) -> Result<(), JlinkError> {
        let command = CString::new(command).map_err(|_| {
            JlinkError::new(ErrorCode::ConfigInvalid, "J-Link 命令包含 NUL 字符", false)
        })?;
        let mut output = [0_i8; 512];
        let output_len = i32::try_from(output.len() - 1).expect("fixed output buffer fits i32");
        reset_dll_diagnostics();
        // SAFETY: pointers remain valid and one trailing zero byte is withheld so
        // the returned error output is always NUL-terminated for local parsing.
        let result =
            unsafe { (self.api.exec_command)(command.as_ptr(), output.as_mut_ptr(), output_len) };
        // SAFETY: the final byte remains zero even if the DLL fills its declared buffer.
        let output = unsafe { CStr::from_ptr(output.as_ptr()) }.to_string_lossy();
        let validation =
            validate_exec_command_result(command.to_string_lossy().as_ref(), result, &output);
        match validation {
            Ok(()) => {
                let _ = take_dll_diagnostics();
                Ok(())
            }
            Err(error) => Err(attach_dll_diagnostics(error)),
        }
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

/// Classifies one halt observation without reading Cortex-M PPB state while running.
///
/// J-Link 6.98a may reject ICSR reads from a running S32K144. A caught
/// `HardFault` is distinguished only after the DLL reports the core halted.
fn classify_observed_target_state(
    halted: bool,
    read_icsr: impl FnOnce() -> Result<u32, JlinkError>,
) -> Result<TargetState, JlinkError> {
    if !halted {
        return Ok(TargetState::Running);
    }
    if read_icsr()? & 0x1ff == 3 {
        Ok(TargetState::HardFault)
    } else {
        Ok(TargetState::Halted)
    }
}

fn classify_register_read_failure(
    register: CoreRegister,
    dll_status: u8,
    target_state: TargetState,
) -> JlinkError {
    let (code, message) = if target_state == TargetState::Running {
        (
            ErrorCode::InvalidStateTransition,
            format!(
                "目标运行时不能读取核心寄存器 {}；请先显式 halt",
                register.canonical_name()
            ),
        )
    } else {
        (
            ErrorCode::TargetConnectFailed,
            format!(
                "目标处于 {target_state:?} 时读取核心寄存器 {} 失败",
                register.canonical_name()
            ),
        )
    };
    let mut error = JlinkError::new(code, message, true)
        .with_detail("register", serde_json::json!(register.canonical_name()))
        .with_detail("dll_status", serde_json::json!(dll_status))
        .with_detail("target_state", serde_json::json!(target_state));
    if target_state == TargetState::Running {
        error = error.with_detail(
            "recommendation",
            serde_json::json!("先调用 jlink_control.halt，再读取核心寄存器"),
        );
    }
    error
}

fn hss_start_error(status: i32) -> JlinkError {
    JlinkError::new(
        ErrorCode::HssStartFailed,
        format!("JLINK_HSS_Start 返回失败状态 {status}"),
        true,
    )
}

/// Requires the reset-halt preparation established for the frozen Flash algorithm.
fn require_flash_reset_halted(state: TargetState) -> Result<(), JlinkError> {
    if state == TargetState::Halted {
        Ok(())
    } else {
        Err(target_recovery_error(format!(
            "Flash 操作前未能确认目标 halted：实际={state:?}"
        )))
    }
}

fn reusable_check(
    checks: Option<&[ValidationCheck]>,
    kind: ValidationCheckKind,
) -> Option<ValidationCheck> {
    checks?
        .iter()
        .find(|check| check.kind == kind)
        .map(|check| {
            let mut reused = check.clone();
            reused.evidence = ValidationCheckEvidence::Reused;
            reused
        })
}

fn passed_check(kind: ValidationCheckKind, detail: impl Into<String>) -> ValidationCheck {
    ValidationCheck {
        kind,
        passed: true,
        evidence: ValidationCheckEvidence::Executed,
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
        evidence: ValidationCheckEvidence::Executed,
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

/// Interprets the frozen 6.98a byte-oriented memory-read status.
///
/// `JLINKARM_ReadMem` returns zero only after its internal transfer completed
/// the requested byte count. Unlike the typed memory-read exports, its return
/// value is a status and must not be compared with the requested length.
fn validate_read_mem_result(address: u32, length: usize, result: i32) -> Result<(), JlinkError> {
    if result == 0 {
        return Ok(());
    }
    Err(target_connection_error(format!(
        "JLINKARM_ReadMem(0x{address:08X}, {length}) 返回错误状态 {result}"
    ))
    .with_detail("address", serde_json::json!(format!("0x{address:08X}")))
    .with_detail("requested_length", serde_json::json!(length))
    .with_detail("dll_status", serde_json::json!(result)))
}

fn target_connection_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetConnectFailed, message, true)
}

fn validate_exec_command_result(
    command: &str,
    result: i32,
    error_output: &str,
) -> Result<(), JlinkError> {
    let error_output = error_output.trim();
    if result >= 0 && error_output.is_empty() {
        return Ok(());
    }
    let mut error =
        target_connection_error(format!("JLINKARM_ExecCommand({command}) 返回 {result}"));
    if !error_output.is_empty() {
        error = error.with_detail("dll_error_output", serde_json::json!(error_output));
    }
    Err(error)
}

fn target_recovery_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
}

fn execution_uncertain_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ExecutionUncertain, message, false)
}

#[cfg(test)]
fn test_injection_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, env, ffi::c_void, mem, path::PathBuf};

    use jlink_domain::{
        CoreRegister, ErrorCode, FlashRegion, JlinkError, MemoryRange, MemoryRegionKind,
        TargetConnectionSpec, TargetInterface, TargetState, ValidationCheckEvidence,
        ValidationCheckKind,
    };

    use super::{
        DLL_DIAGNOSTIC_BYTES, DeviceInfo, DllDiagnosticBuffer, DllGateway, HssApi, HssBlock,
        HssCaps, classify_observed_target_state, classify_register_read_failure, hss_start_error,
        passed_check, require_flash_reset_halted, reusable_check, validate_exec_command_result,
        validate_read_mem_result,
    };

    unsafe extern "C" fn hss_get_caps_stub(_caps: *mut HssCaps) -> i32 {
        0
    }

    unsafe extern "C" fn hss_start_stub(
        _blocks: *mut HssBlock,
        _block_count: i32,
        _period_us: i32,
        _flags: i32,
    ) -> i32 {
        0
    }

    unsafe extern "C" fn hss_read_stub(_buffer: *mut c_void, _buffer_bytes: u32) -> i32 {
        0
    }

    unsafe extern "C" fn hss_stop_stub() -> i32 {
        0
    }

    #[test]
    fn frozen_x64_device_info_prefix_is_568_bytes() {
        assert_eq!(std::mem::size_of::<DeviceInfo>(), 568);
    }

    #[test]
    fn hss_start_failure_is_retryable_by_public_contract() {
        let error = hss_start_error(-1);
        assert_eq!(error.code, ErrorCode::HssStartFailed);
        assert!(error.retryable);
    }

    #[test]
    fn t_p3_abi_matches_frozen_hss_structures_and_function_signatures() {
        assert_eq!(mem::size_of::<HssBlock>(), 16);
        assert_eq!(mem::align_of::<HssBlock>(), 4);
        assert_eq!(mem::offset_of!(HssBlock, address), 0);
        assert_eq!(mem::offset_of!(HssBlock, byte_count), 4);
        assert_eq!(mem::offset_of!(HssBlock, flags), 8);
        assert_eq!(mem::offset_of!(HssBlock, reserved), 12);

        assert_eq!(mem::size_of::<HssCaps>(), 32);
        assert_eq!(mem::align_of::<HssCaps>(), 4);
        assert_eq!(mem::offset_of!(HssCaps, max_blocks), 0);
        assert_eq!(mem::offset_of!(HssCaps, max_frequency_hz), 4);
        assert_eq!(mem::offset_of!(HssCaps, flags), 8);
        assert_eq!(mem::offset_of!(HssCaps, reserved), 12);

        let api = HssApi {
            get_caps: Some(hss_get_caps_stub),
            start: Some(hss_start_stub),
            read: Some(hss_read_stub),
            stop: Some(hss_stop_stub),
        };
        assert!(api.missing_exports().is_empty());
    }

    #[test]
    fn t_p3_abi_reports_only_missing_hss_exports() {
        let api = HssApi {
            get_caps: Some(hss_get_caps_stub),
            start: None,
            read: None,
            stop: Some(hss_stop_stub),
        };
        assert_eq!(api.missing_exports(), ["JLINK_HSS_Start", "JLINK_HSS_Read"]);
    }

    #[test]
    fn exec_command_requires_empty_error_output() {
        validate_exec_command_result("device = S32K144", 0, "")
            .expect("empty error output is accepted");

        let rejected =
            validate_exec_command_result("device = MissingDevice", 0, "Unknown device selected.")
                .expect_err("DLL error output must reject the command");
        assert_eq!(rejected.code, ErrorCode::TargetConnectFailed);
        assert_eq!(
            rejected
                .details
                .as_ref()
                .and_then(|details| details.get("dll_error_output")),
            Some(&serde_json::json!("Unknown device selected."))
        );

        let failed = validate_exec_command_result("device = S32K144", -1, "")
            .expect_err("negative return code must reject the command");
        assert_eq!(failed.code, ErrorCode::TargetConnectFailed);
    }

    #[test]
    fn running_observation_skips_icsr_but_halted_hardfault_reads_it() {
        let read_attempted = Cell::new(false);
        let running = classify_observed_target_state(false, || {
            read_attempted.set(true);
            Ok(3)
        })
        .expect("running state does not require ICSR");
        assert_eq!(running, TargetState::Running);
        assert!(!read_attempted.get());

        let hardfault = classify_observed_target_state(true, || Ok(3))
            .expect("halted HardFault is classified from ICSR");
        assert_eq!(hardfault, TargetState::HardFault);
    }

    #[test]
    fn register_item_failure_distinguishes_running_from_halted_access() {
        let running = classify_register_read_failure(CoreRegister::Pc, 255, TargetState::Running);
        assert_eq!(running.code, ErrorCode::InvalidStateTransition);
        assert!(running.retryable);
        let running_details = running.details.expect("running details");
        assert_eq!(
            running_details["target_state"],
            serde_json::json!("running")
        );

        let halted = classify_register_read_failure(CoreRegister::Pc, 255, TargetState::Halted);
        assert_eq!(halted.code, ErrorCode::TargetConnectFailed);
        assert!(halted.retryable);
        let halted_details = halted.details.expect("halted details");
        assert_eq!(halted_details["target_state"], serde_json::json!("halted"));
    }

    #[test]
    fn validation_check_provenance_changes_only_when_evidence_is_reused() {
        let executed = passed_check(ValidationCheckKind::DllIdentity, "frozen DLL");
        assert_eq!(executed.evidence, ValidationCheckEvidence::Executed);

        let reused = reusable_check(Some(std::slice::from_ref(&executed)), executed.kind)
            .expect("same-fingerprint evidence");
        assert_eq!(reused.evidence, ValidationCheckEvidence::Reused);
        assert_eq!(reused.detail, executed.detail);
    }

    #[test]
    fn flash_operation_requires_confirmed_reset_halted_state() {
        require_flash_reset_halted(TargetState::Halted)
            .expect("reset-halted target is ready for Flash");
        for state in [
            TargetState::Running,
            TargetState::HardFault,
            TargetState::Unknown,
        ] {
            let error =
                require_flash_reset_halted(state).expect_err("non-halted state is rejected");
            assert_eq!(error.code, ErrorCode::TargetRecoveryFailed);
        }
    }

    #[test]
    fn generic_read_mem_uses_zero_success_status() {
        validate_read_mem_result(0, 784, 0).expect("zero is the frozen success status");

        let error =
            validate_read_mem_result(0, 784, 1).expect_err("non-zero status must reject the read");
        assert_eq!(error.code, ErrorCode::TargetConnectFailed);
        let details = error.details.expect("read failure details");
        assert_eq!(
            details.get("requested_length"),
            Some(&serde_json::json!(784))
        );
        assert_eq!(details.get("dll_status"), Some(&serde_json::json!(1)));
        assert!(!details.contains_key("actual_length"));
    }

    #[test]
    fn dll_diagnostics_are_bounded_and_cleared_after_take() {
        let mut diagnostics = DllDiagnosticBuffer::new();
        diagnostics.append(b"error: ", &vec![b'x'; DLL_DIAGNOSTIC_BYTES * 2]);
        assert_eq!(diagnostics.len, DLL_DIAGNOSTIC_BYTES);
        assert!(diagnostics.truncated);

        let output = diagnostics.take().expect("bounded diagnostic output");
        assert!(output.ends_with("[diagnostics truncated]"));
        assert_eq!(diagnostics.len, 0);
        assert!(!diagnostics.truncated);
    }

    #[test]
    #[ignore = "requires the explicitly fingerprinted J-Link 6.98a DLL"]
    fn t_p3_abi_frozen_dll_has_required_hss_exports() {
        let path = PathBuf::from(
            env::var("JLINK_MCP_T_P3_ABI_DLL")
                .expect("JLINK_MCP_T_P3_ABI_DLL must name the frozen DLL"),
        );
        let gateway = DllGateway::load(&path).expect("load frozen DLL");
        assert!(gateway.api.hss.missing_exports().is_empty());
    }

    #[test]
    #[ignore = "requires the explicitly fingerprinted J-Link 6.98a DLL"]
    fn frozen_dll_reports_non_empty_device_flash_regions() {
        let path = PathBuf::from(
            env::var("JLINK_MCP_T_P2_PRG_DLL")
                .expect("JLINK_MCP_T_P2_PRG_DLL must name the frozen DLL"),
        );
        let device = env::var("JLINK_MCP_T_P2_PRG_DEVICE")
            .expect("JLINK_MCP_T_P2_PRG_DEVICE must name the configured device");
        let mut gateway = DllGateway::load(&path).expect("load frozen DLL");
        let regions = gateway
            .device_flash_regions(&device)
            .expect("device Flash regions");
        println!("device={device}; regions={regions:?}");
        assert!(!regions.is_empty());
        assert!(regions.iter().all(|region| region.length() > 0));
        if device == "S32K144" {
            assert_eq!(
                regions,
                [
                    FlashRegion::new(0x0000_0000, 0x0008_0000).expect("program Flash"),
                    FlashRegion::new(0x1000_0000, 0x0001_0000).expect("data Flash"),
                ]
            );
            let memory_map = gateway
                .device_memory_map(&device)
                .expect("device Flash/RAM memory map");
            assert_eq!(
                memory_map
                    .classify(MemoryRange::raw(0x2000_0000, 4).expect("SRAM range"))
                    .expect("classified SRAM"),
                MemoryRegionKind::Ram
            );
            assert_eq!(
                memory_map
                    .classify(MemoryRange::raw(0x4000_0000, 4).expect("MMIO range"))
                    .expect("classified MMIO"),
                MemoryRegionKind::Mmio
            );
        }
    }

    #[test]
    #[ignore = "requires the explicitly fingerprinted J-Link 6.98a DLL and S32K144 target"]
    fn hardware_core_register_and_control_round_trip() -> Result<(), JlinkError> {
        let path = PathBuf::from(
            env::var("JLINK_MCP_T_P2_CTL_DLL")
                .expect("JLINK_MCP_T_P2_CTL_DLL must name the frozen DLL"),
        );
        let device = env::var("JLINK_MCP_T_P2_CTL_DEVICE")
            .expect("JLINK_MCP_T_P2_CTL_DEVICE must name the configured device");
        let serial = env::var("JLINK_MCP_T_P2_CTL_PROBE")
            .expect("JLINK_MCP_T_P2_CTL_PROBE must name the configured probe")
            .parse::<u32>()
            .expect("probe serial is u32");
        let spec =
            TargetConnectionSpec::new(device, TargetInterface::Swd, 4_000, Some(serial), None)
                .expect("target spec");
        let mut gateway = DllGateway::load(&path).expect("load frozen DLL");
        gateway.open_target(&spec)?;
        let mut original_r0 = None;
        let result = (|| {
            if gateway.resume_and_observe()? != TargetState::Running {
                return Err(hardware_control_error("测试前目标未稳定运行"));
            }
            let running_step = gateway
                .step_and_observe()
                .expect_err("running step must be rejected before JLINKARM_Step");
            if running_step.code != ErrorCode::InvalidStateTransition
                || gateway.observe_target_state()? != TargetState::Running
            {
                return Err(hardware_control_error(
                    "运行中 step 未保持目标运行或错误码不正确",
                ));
            }

            let entries = gateway.register_entries()?;
            for register in CoreRegister::ALL {
                if !entries.iter().any(|entry| entry.1 == register.jlink_name()) {
                    return Err(JlinkError::new(
                        ErrorCode::RegisterNotFound,
                        format!("S32K144 目录缺少 V1 寄存器 {}", register.canonical_name()),
                        false,
                    ));
                }
            }
            if gateway.halt_and_observe()? != TargetState::Halted {
                return Err(hardware_control_error("halt 未收口到 halted"));
            }

            let saved_r0 = gateway.read_register(CoreRegister::R0)?;
            original_r0 = Some(saved_r0);
            let changed_r0 = saved_r0 ^ 0xA5A5_5A5A;
            gateway.write_register(CoreRegister::R0, changed_r0)?;
            if gateway.read_register(CoreRegister::R0)? != changed_r0 {
                return Err(hardware_control_error("R0 写入后读取不一致"));
            }
            gateway.write_register(CoreRegister::R0, saved_r0)?;
            if gateway.read_register(CoreRegister::R0)? != saved_r0 {
                return Err(hardware_control_error("R0 原值恢复后读取不一致"));
            }
            original_r0 = None;

            let pc_before = gateway.read_register(CoreRegister::Pc)?;
            if gateway.step_and_observe()? != TargetState::Halted {
                return Err(hardware_control_error("step 后目标未保持 halted"));
            }
            let pc_after = gateway.read_register(CoreRegister::Pc)?;
            if pc_after == pc_before {
                return Err(hardware_control_error(format!(
                    "step 后 PC 未变化：0x{pc_before:08X}"
                )));
            }
            if gateway.reset_halt_and_observe()? != TargetState::Halted {
                return Err(hardware_control_error("reset after=halt 未收口到 halted"));
            }
            if gateway.reset_run_and_observe()? != TargetState::Running {
                return Err(hardware_control_error("reset after=run 未收口到 running"));
            }
            if gateway.halt_and_observe()? != TargetState::Halted
                || gateway.resume_and_observe()? != TargetState::Running
            {
                return Err(hardware_control_error("halt/resume 未完成状态往返"));
            }
            Ok(())
        })();
        let cleanup = (|| {
            if let Some(saved_r0) = original_r0 {
                gateway.halt_and_observe()?;
                gateway.write_register(CoreRegister::R0, saved_r0)?;
                if gateway.read_register(CoreRegister::R0)? != saved_r0 {
                    return Err(hardware_control_error("失败清理未能恢复 R0 原值"));
                }
            }
            if gateway.observe_target_state()? != TargetState::Running
                && gateway.reset_run_and_observe()? != TargetState::Running
            {
                return Err(hardware_control_error("失败清理未能恢复 CPU 运行"));
            }
            Ok(())
        })();
        gateway.close_target();
        cleanup?;
        result
    }

    fn hardware_control_error(message: impl Into<String>) -> JlinkError {
        JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
    }
}
