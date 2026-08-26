use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use libloading::Library;
use serde::Serialize;

const RESUME_TIMEOUT: Duration = Duration::from_secs(2);
const RUNNING_STABILITY_WINDOW: Duration = Duration::from_millis(100);
const RUNNING_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
type ReadMemFn = unsafe extern "C" fn(u32, u32, *mut c_void) -> i32;
type ReadMemU32Fn = unsafe extern "C" fn(u32, u32, *mut u32, *mut u8) -> i32;
type WriteMemFn = unsafe extern "C" fn(u32, u32, *const c_void) -> i32;
type WriteU32Fn = unsafe extern "C" fn(u32, u32) -> i32;
type HssGetCapsFn = unsafe extern "C" fn(*mut HssCaps) -> i32;
type HssStartFn = unsafe extern "C" fn(*mut HssBlock, i32, i32, i32) -> i32;
type HssReadFn = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type HssStopFn = unsafe extern "C" fn() -> i32;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct HssBlock {
    pub(crate) address: u32,
    pub(crate) byte_count: u32,
    pub(crate) flags: u32,
    pub(crate) reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub(crate) struct HssCaps {
    pub(crate) max_blocks: u32,
    pub(crate) max_frequency_hz: u32,
    pub(crate) flags: u32,
    pub(crate) reserved: [u32; 5],
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectEvidence {
    pub(crate) dll_path: PathBuf,
    pub(crate) dll_version: i32,
    pub(crate) probe_serial: u32,
    pub(crate) selected_serial_return_code: i32,
    pub(crate) open_return_code: i32,
    pub(crate) connect_return_code: i32,
    pub(crate) target_id: u32,
    pub(crate) was_halted_after_connect: bool,
    pub(crate) resumed_after_connect: bool,
    pub(crate) resume_stability_elapsed_ms: Option<u64>,
    pub(crate) device_output: String,
}

struct Api {
    _library: Library,
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
    is_halted: GetI32Fn,
    halt: VoidFn,
    go: VoidFn,
    _read_mem: ReadMemFn,
    read_mem_u32: ReadMemU32Fn,
    write_mem: WriteMemFn,
    write_u32: WriteU32Fn,
    hss_get_caps: HssGetCapsFn,
    hss_start: HssStartFn,
    hss_read: HssReadFn,
    hss_stop: HssStopFn,
}

impl Api {
    fn load(path: &Path) -> Result<Self> {
        // SAFETY: The library identity is checked by the caller and remains owned by
        // this structure for at least as long as every copied function pointer.
        let library = unsafe { Library::new(path) }
            .with_context(|| format!("failed to load {}", path.display()))?;
        Ok(Self {
            open: load_symbol(&library, b"JLINKARM_Open\0")?,
            close: load_symbol(&library, b"JLINKARM_Close\0")?,
            exec_command: load_symbol(&library, b"JLINKARM_ExecCommand\0")?,
            select_tif: load_symbol(&library, b"JLINKARM_TIF_Select\0")?,
            set_speed: load_symbol(&library, b"JLINKARM_SetSpeed\0")?,
            connect: load_symbol(&library, b"JLINKARM_Connect\0")?,
            select_probe: load_symbol(&library, b"JLINKARM_EMU_SelectByUSBSN\0")?,
            get_serial: load_symbol(&library, b"JLINKARM_GetSN\0")?,
            get_target_id: load_symbol(&library, b"JLINKARM_GetId\0")?,
            get_dll_version: load_symbol(&library, b"JLINKARM_GetDLLVersion\0")?,
            is_halted: load_symbol(&library, b"JLINKARM_IsHalted\0")?,
            halt: load_symbol(&library, b"JLINKARM_Halt\0")?,
            go: load_symbol(&library, b"JLINKARM_Go\0")?,
            _read_mem: load_symbol(&library, b"JLINKARM_ReadMem\0")?,
            read_mem_u32: load_symbol(&library, b"JLINKARM_ReadMemU32\0")?,
            write_mem: load_symbol(&library, b"JLINKARM_WriteMem\0")?,
            write_u32: load_symbol(&library, b"JLINKARM_WriteU32\0")?,
            hss_get_caps: load_symbol(&library, b"JLINK_HSS_GetCaps\0")?,
            hss_start: load_symbol(&library, b"JLINK_HSS_Start\0")?,
            hss_read: load_symbol(&library, b"JLINK_HSS_Read\0")?,
            hss_stop: load_symbol(&library, b"JLINK_HSS_Stop\0")?,
            _library: library,
        })
    }
}

fn load_symbol<T: Copy>(library: &Library, name: &'static [u8]) -> Result<T> {
    // SAFETY: Every requested symbol uses the candidate ABI already exercised by
    // the legacy helper. A failure to resolve is returned before target access.
    let symbol = unsafe { library.get::<T>(name) }.with_context(|| {
        format!(
            "missing DLL export {}",
            CStr::from_bytes_with_nul(name).unwrap().to_string_lossy()
        )
    })?;
    Ok(*symbol)
}

pub(crate) struct JlinkSession {
    api: Api,
    opened: bool,
    pub(crate) evidence: ConnectEvidence,
}

impl JlinkSession {
    /// Opens one exact probe and connects to one concrete target.
    pub(crate) fn connect(
        dll_path: &Path,
        device: &str,
        interface: &str,
        speed_khz: i32,
        probe_serial: u32,
    ) -> Result<Self> {
        ensure!(speed_khz > 0, "speed must be positive");
        let interface_id = match interface {
            "SWD" => 1,
            "JTAG" => 0,
            _ => bail!("interface must be SWD or JTAG"),
        };
        let api = Api::load(dll_path)?;
        // SAFETY: Function pointers were resolved from the loaded x64 DLL and use
        // the candidate signatures isolated to this experiment process.
        let dll_version = unsafe { (api.get_dll_version)() };
        ensure!(
            dll_version > 0,
            "JLINKARM_GetDLLVersion returned {dll_version}"
        );
        let suppress_gui = CString::new("SuppressGUI = 1")?;
        let mut suppress_output = [0_i8; 512];
        let suppress_return_code = unsafe {
            (api.exec_command)(
                suppress_gui.as_ptr(),
                suppress_output.as_mut_ptr(),
                suppress_output.len() as i32,
            )
        };
        ensure!(
            suppress_return_code >= 0,
            "pre-open JLINKARM_ExecCommand(SuppressGUI) returned {suppress_return_code}"
        );
        let selected_serial_return_code = unsafe { (api.select_probe)(probe_serial) };
        ensure!(
            selected_serial_return_code >= 0,
            "JLINKARM_EMU_SelectByUSBSN returned {selected_serial_return_code}"
        );
        let open_return_code = unsafe { (api.open)() };
        ensure!(
            open_return_code >= 0,
            "JLINKARM_Open returned {open_return_code}"
        );

        let mut session = Self {
            api,
            opened: true,
            evidence: ConnectEvidence {
                dll_path: dll_path.to_path_buf(),
                dll_version,
                probe_serial,
                selected_serial_return_code,
                open_return_code,
                connect_return_code: -1,
                target_id: 0,
                was_halted_after_connect: false,
                resumed_after_connect: false,
                resume_stability_elapsed_ms: None,
                device_output: String::new(),
            },
        };

        session.exec_expect_success("SetRestartOnClose = 0")?;
        session.exec_expect_success("SetSkipDebugDeInit = 1")?;
        session.evidence.device_output =
            session.exec_expect_success(&format!("device = {device}"))?;
        let tif_return_code = unsafe { (session.api.select_tif)(interface_id) };
        ensure!(
            tif_return_code >= 0,
            "JLINKARM_TIF_Select returned {tif_return_code}"
        );
        unsafe { (session.api.set_speed)(speed_khz) };
        let connect_return_code = unsafe { (session.api.connect)() };
        ensure!(
            connect_return_code >= 0,
            "JLINKARM_Connect returned {connect_return_code}"
        );
        session.evidence.connect_return_code = connect_return_code;
        let actual_serial = unsafe { (session.api.get_serial)() };
        ensure!(
            actual_serial == probe_serial,
            "selected probe {probe_serial}, connected probe {actual_serial}"
        );
        session.evidence.target_id = unsafe { (session.api.get_target_id)() };
        session.evidence.was_halted_after_connect = session.is_halted()?;
        if session.evidence.was_halted_after_connect {
            unsafe { (session.api.go)() };
            session.evidence.resume_stability_elapsed_ms =
                Some(session.wait_until_stably_running()?);
            session.evidence.resumed_after_connect = true;
        }
        Ok(session)
    }

    /// Calls the candidate HSS capability API on the connected target.
    pub(crate) fn get_caps(&self) -> Result<(i32, HssCaps)> {
        let mut caps = HssCaps::default();
        let return_code = unsafe { (self.api.hss_get_caps)(&mut caps) };
        ensure!(return_code >= 0, "JLINK_HSS_GetCaps returned {return_code}");
        ensure!(
            caps.max_blocks > 0,
            "JLINK_HSS_GetCaps returned zero maxBlocks"
        );
        ensure!(
            caps.max_frequency_hz > 0,
            "JLINK_HSS_GetCaps returned zero maxFrequencyHz"
        );
        Ok((return_code, caps))
    }

    /// Starts one HSS stream with the supplied fixed memory blocks.
    pub(crate) fn start_hss(
        &self,
        blocks: &mut [HssBlock],
        rate_hz: u32,
        flags: u32,
    ) -> Result<i32> {
        ensure!(!blocks.is_empty(), "at least one HSS block is required");
        ensure!(rate_hz > 0, "rate must be positive");
        let period_us = (1_000_000_u32 + rate_hz / 2) / rate_hz;
        let return_code = unsafe {
            (self.api.hss_start)(
                blocks.as_mut_ptr(),
                i32::try_from(blocks.len())?,
                i32::try_from(period_us)?,
                i32::try_from(flags)?,
            )
        };
        ensure!(return_code >= 0, "JLINK_HSS_Start returned {return_code}");
        Ok(return_code)
    }

    /// Reads currently buffered HSS bytes into the supplied sentinel buffer.
    pub(crate) fn read_hss(&self, buffer: &mut [u8]) -> Result<i32> {
        let return_code = unsafe {
            (self.api.hss_read)(
                buffer.as_mut_ptr().cast::<c_void>(),
                u32::try_from(buffer.len())?,
            )
        };
        ensure!(return_code >= 0, "JLINK_HSS_Read returned {return_code}");
        ensure!(
            usize::try_from(return_code)? <= buffer.len(),
            "JLINK_HSS_Read returned more bytes than the buffer"
        );
        Ok(return_code)
    }

    /// Stops the active HSS stream.
    pub(crate) fn stop_hss(&self) -> Result<i32> {
        let return_code = unsafe { (self.api.hss_stop)() };
        ensure!(return_code >= 0, "JLINK_HSS_Stop returned {return_code}");
        Ok(return_code)
    }

    /// Reads one little-endian 32-bit RAM word.
    pub(crate) fn read_u32(&self, address: u32) -> Result<u32> {
        let mut value = 0_u32;
        let mut status = 0_u8;
        let return_code = unsafe { (self.api.read_mem_u32)(address, 1, &mut value, &mut status) };
        ensure!(
            return_code == 1 && status == 0,
            "JLINKARM_ReadMemU32 returned count={return_code}, status={status}"
        );
        Ok(value)
    }

    /// Writes and verifies one little-endian 32-bit RAM word.
    pub(crate) fn write_u32(&self, address: u32, value: u32) -> Result<i32> {
        let bytes = value.to_le_bytes();
        let return_code = unsafe {
            (self.api.write_mem)(address, bytes.len() as u32, bytes.as_ptr().cast::<c_void>())
        };
        ensure!(return_code >= 0, "JLINKARM_WriteMem returned {return_code}");
        ensure!(
            self.read_u32(address)? == value,
            "RAM readback mismatch at 0x{address:08X}"
        );
        Ok(return_code)
    }

    /// Writes through the SDK's scalar 32-bit entry point and verifies the value.
    pub(crate) fn write_u32_direct(&self, address: u32, value: u32) -> Result<i32> {
        let return_code = unsafe { (self.api.write_u32)(address, value) };
        ensure!(return_code >= 0, "JLINKARM_WriteU32 returned {return_code}");
        ensure!(
            self.read_u32(address)? == value,
            "RAM readback mismatch at 0x{address:08X}"
        );
        Ok(return_code)
    }

    /// Returns whether the target is currently halted.
    pub(crate) fn is_halted(&self) -> Result<bool> {
        let value = unsafe { (self.api.is_halted)() };
        ensure!(value >= 0, "JLINKARM_IsHalted returned {value}");
        Ok(value > 0)
    }

    /// Resumes the target when it is halted and verifies the final running state.
    pub(crate) fn ensure_running(&self) -> Result<bool> {
        if !self.is_halted()? {
            return Ok(false);
        }
        unsafe { (self.api.go)() };
        self.wait_until_stably_running()?;
        Ok(true)
    }

    /// Halts the target and enables full debug de-initialization for a clean reconnect.
    pub(crate) fn prepare_for_reconnect(&self) -> Result<()> {
        unsafe { (self.api.halt)() };
        let started = Instant::now();
        while !self.is_halted()? {
            ensure!(
                started.elapsed() < RESUME_TIMEOUT,
                "target did not halt before DLL reconnect"
            );
            thread::sleep(RUNNING_POLL_INTERVAL);
        }
        self.exec_expect_success("SetSkipDebugDeInit = 0")?;
        Ok(())
    }

    fn wait_until_stably_running(&self) -> Result<u64> {
        let started = Instant::now();
        let mut running_since = None;
        loop {
            if self.is_halted()? {
                running_since = None;
            } else {
                let stable_since = running_since.get_or_insert_with(Instant::now);
                if stable_since.elapsed() >= RUNNING_STABILITY_WINDOW {
                    return Ok(u64::try_from(started.elapsed().as_millis())?);
                }
            }
            ensure!(
                started.elapsed() < RESUME_TIMEOUT,
                "target did not remain running for {} ms within {} ms after JLINKARM_Go",
                RUNNING_STABILITY_WINDOW.as_millis(),
                RESUME_TIMEOUT.as_millis()
            );
            thread::sleep(RUNNING_POLL_INTERVAL);
        }
    }

    fn exec_expect_success(&self, command: &str) -> Result<String> {
        let command = CString::new(command).context("J-Link command contains an interior NUL")?;
        let mut output = [0_i8; 512];
        let return_code = unsafe {
            (self.api.exec_command)(command.as_ptr(), output.as_mut_ptr(), output.len() as i32)
        };
        ensure!(
            return_code >= 0,
            "JLINKARM_ExecCommand returned {return_code}"
        );
        // SAFETY: The output buffer is zero-initialized and the DLL receives its
        // complete size, so it remains NUL terminated even for an empty response.
        Ok(unsafe { CStr::from_ptr(output.as_ptr()) }
            .to_string_lossy()
            .into_owned())
    }
}

impl Drop for JlinkSession {
    fn drop(&mut self) {
        if self.opened {
            unsafe { (self.api.close)() };
            self.opened = false;
        }
    }
}

/// Loads the full candidate symbol set without opening a probe.
pub(crate) fn preflight(dll_path: &Path) -> Result<i32> {
    let api = Api::load(dll_path)?;
    let version = unsafe { (api.get_dll_version)() };
    ensure!(version > 0, "JLINKARM_GetDLLVersion returned {version}");
    Ok(version)
}
