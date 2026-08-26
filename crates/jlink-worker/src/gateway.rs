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
    ErrorCode, FaultDiagnostics, FirmwareImage, FlashRegion, JlinkError, ProgramAfter,
    TargetConnectionSpec, TargetInterface, TargetState, ValidationCheck, ValidationCheckKind,
    ValidationReport,
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
const DEVICE_AREA_COUNT: usize = 32;
const PROGRAM_CHUNK_BYTES: usize = 64 * 1024;
const PROGRAM_CHUNK_BYTES_U64: u64 = 64 * 1024;

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
type ReadMemFn = unsafe extern "C" fn(u32, u32, *mut u8) -> i32;
type WriteMemFn = unsafe extern "C" fn(u32, u32, *const u8) -> i32;
type DeviceGetIndexFn = unsafe extern "C" fn(*const c_char) -> i32;
type DeviceGetInfoFn = unsafe extern "C" fn(i32, *mut DeviceInfo) -> i32;
type BeginDownloadFn = unsafe extern "C" fn(u32);
type EndDownloadFn = unsafe extern "C" fn() -> i32;
type EraseChipFn = unsafe extern "C" fn() -> i32;
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
    read_mem: ReadMemFn,
    read_mem_u32: ReadMemU32Fn,
    write_mem: WriteMemFn,
    device_get_index: DeviceGetIndexFn,
    device_get_info: DeviceGetInfoFn,
    begin_download: BeginDownloadFn,
    end_download: EndDownloadFn,
    erase_chip: EraseChipFn,
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
            read_mem: load_symbol(module, b"JLINKARM_ReadMem\0")?,
            read_mem_u32: load_symbol(module, b"JLINKARM_ReadMemU32\0")?,
            write_mem: load_symbol(module, b"JLINKARM_WriteMem\0")?,
            device_get_index: load_symbol(module, b"JLINKARM_DEVICE_GetIndex\0")?,
            device_get_info: load_symbol(module, b"JLINKARM_DEVICE_GetInfo\0")?,
            begin_download: load_symbol(module, b"JLINKARM_BeginDownload\0")?,
            end_download: load_symbol(module, b"JLINKARM_EndDownload\0")?,
            erase_chip: load_symbol(module, b"JLINK_EraseChip\0")?,
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

    /// Reads authoritative Flash regions from the loaded J-Link device database.
    ///
    /// This call does not open a probe or access the target. The returned ranges
    /// are therefore suitable for completing all boundary checks before a Flash
    /// side effect begins.
    pub(crate) fn device_flash_regions(
        &mut self,
        device: &str,
    ) -> Result<Vec<FlashRegion>, JlinkError> {
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

    /// Programs every normalized image segment through J-Link's device algorithm.
    ///
    /// The caller must validate all segments against [`Self::device_flash_regions`]
    /// before invoking this method. Any failure after `BeginDownload` is reported
    /// as execution-uncertain because target side effects may already exist.
    pub(crate) fn program_image(&mut self, image: &FirmwareImage) -> Result<(), JlinkError> {
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
            (Ok(()), value) if value >= 0 => Ok(()),
            (Err(error), value) => Err(execution_uncertain_error(format!(
                "Flash 写入未能完成：{error}；JLINKARM_EndDownload 返回 {value}"
            ))),
            (Ok(()), value) => Err(execution_uncertain_error(format!(
                "JLINKARM_EndDownload 返回 {value}"
            ))),
        }
    }

    /// Erases all always-present device Flash banks through the J-Link algorithm.
    pub(crate) fn erase_chip(&mut self) -> Result<(), JlinkError> {
        // SAFETY: the connected target is uniquely owned by this gateway.
        let result = unsafe { (self.api.erase_chip)() };
        if result < 0 {
            return Err(execution_uncertain_error(format!(
                "JLINK_EraseChip 返回 {result}"
            )));
        }
        Ok(())
    }

    /// Erases one validated byte range using J-Link's Flash read-modify-write path.
    ///
    /// Writing erased bytes within a download transaction delegates sector erase
    /// and preservation of bytes outside the requested range to the selected
    /// device algorithm. Hardware evidence must confirm this behavior for each
    /// frozen DLL/device fingerprint before release.
    pub(crate) fn erase_range(&mut self, address: u64, length: u64) -> Result<(), JlinkError> {
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
            (Ok(()), value) if value >= 0 => Ok(()),
            (Err(error), value) => Err(execution_uncertain_error(format!(
                "范围擦除未能完成：{error}；JLINKARM_EndDownload 返回 {value}"
            ))),
            (Ok(()), value) => Err(execution_uncertain_error(format!(
                "范围擦除的 JLINKARM_EndDownload 返回 {value}"
            ))),
        }
    }

    /// Reads one complete target range for verification without truncation.
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
                JlinkError::new(ErrorCode::ValueInvalid, "校验读取偏移无法表示", false)
            })?;
            let current = address.checked_add(offset_u64).ok_or_else(|| {
                JlinkError::new(ErrorCode::ValueInvalid, "校验读取地址溢出", false)
            })?;
            let current = u32::try_from(current).map_err(|_| {
                JlinkError::new(
                    ErrorCode::FlashRangeInvalid,
                    "校验读取地址超出 Cortex-M 地址范围",
                    false,
                )
            })?;
            let count_u32 = u32::try_from(count).expect("fixed read chunk fits u32");
            // SAFETY: the output slice is writable for `count` bytes and the
            // connected target is uniquely owned by this gateway.
            let result =
                unsafe { (self.api.read_mem)(current, count_u32, output[offset..].as_mut_ptr()) };
            if result != i32::try_from(count).expect("fixed read chunk fits i32") {
                return Err(JlinkError::new(
                    ErrorCode::TargetConnectFailed,
                    format!("JLINKARM_ReadMem(0x{current:08X}, {count}) 返回 {result}"),
                    true,
                ));
            }
            offset += count;
        }
        Ok(output)
    }

    /// Applies the explicit successful post-program target state.
    pub(crate) fn apply_program_after(
        &mut self,
        after: ProgramAfter,
    ) -> Result<TargetState, JlinkError> {
        let (actual, expected) = match after {
            ProgramAfter::None => (self.observe_target_state()?, None),
            ProgramAfter::ResetHalt => {
                // SAFETY: reset and halt are serialized through the unique gateway.
                unsafe {
                    (self.api.reset)();
                    (self.api.halt)();
                }
                self.wait_until_halted()?;
                (self.observe_target_state()?, Some(TargetState::Halted))
            }
            ProgramAfter::ResetRun => (self.reset_run_and_observe()?, Some(TargetState::Running)),
        };
        if expected.is_some_and(|expected| actual != expected) {
            return Err(target_recovery_error(format!(
                "烧录后状态不符合请求：after={after:?}，实际={actual:?}"
            )));
        }
        Ok(actual)
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

fn execution_uncertain_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::ExecutionUncertain, message, false)
}

#[cfg(test)]
fn test_injection_error(message: impl Into<String>) -> JlinkError {
    JlinkError::new(ErrorCode::TargetRecoveryFailed, message, false)
}

#[cfg(test)]
mod tests {
    use std::{env, path::PathBuf};

    use jlink_domain::FlashRegion;

    use super::{DeviceInfo, DllGateway};

    #[test]
    fn frozen_x64_device_info_prefix_is_568_bytes() {
        assert_eq!(std::mem::size_of::<DeviceInfo>(), 568);
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
        }
    }
}
