use std::{
    ffi::c_void,
    fs::File,
    mem,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr,
};

use jlink_domain::{ErrorCode, JlinkError};
use windows_sys::{
    Win32::{
        Foundation::{
            ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE,
            LocalFree,
        },
        Security::{
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SDDL_REVISION_1,
            },
            SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::PIPE_ACCESS_DUPLEX,
        System::{
            Pipes::{
                ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_TYPE_BYTE, PIPE_WAIT,
            },
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
    core::PWSTR,
};

/// Byte-mode named-pipe server that keeps one instance available for attach.
pub(crate) struct PipeServer {
    name: Vec<u16>,
    pending: OwnedHandle,
}

impl PipeServer {
    /// Validates and stores a stable local pipe name.
    pub(crate) fn new(name: &str) -> Result<Self, JlinkError> {
        if !name.starts_with(r"\\.\pipe\jlink-mcp-v1-") || name.contains('\0') {
            return Err(JlinkError::new(
                ErrorCode::ConfigInvalid,
                "Worker 管道名称不符合 V1 本机端点规则",
                false,
            ));
        }
        let name: Vec<u16> = Path::new(name)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let pending = create_instance(&name)?;
        Ok(Self { name, pending })
    }

    /// Waits for a client and replaces the accepted instance before dispatch.
    pub(crate) fn accept(&mut self) -> Result<File, JlinkError> {
        let raw = self.pending.as_raw_handle();
        // SAFETY: the handle is a synchronous named-pipe server and no OVERLAPPED is used.
        let connected = unsafe { ConnectNamedPipe(raw, ptr::null_mut()) };
        if connected == 0 {
            // SAFETY: GetLastError is read immediately after ConnectNamedPipe failed.
            let code = unsafe { GetLastError() };
            if code != ERROR_PIPE_CONNECTED {
                return Err(windows_pipe_error("无法接受 Worker 命名管道连接", code));
            }
        }
        let replacement = create_instance(&self.name)?;
        let connected = mem::replace(&mut self.pending, replacement);
        Ok(File::from(connected))
    }
}

fn create_instance(name: &[u16]) -> Result<OwnedHandle, JlinkError> {
    let mut security = CurrentUserSecurity::new()?;
    let attributes = security.attributes();
    // SAFETY: the pipe name is NUL-terminated, `attributes` and its descriptor
    // remain alive for the call, and the returned handle is uniquely owned.
    let raw = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            2,
            64 * 1024,
            64 * 1024,
            5_000,
            &raw const attributes,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(last_pipe_error("无法创建 Worker 命名管道"));
    }
    // SAFETY: `raw` is a unique valid handle returned by CreateNamedPipeW.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

struct CurrentUserSecurity {
    descriptor: *mut c_void,
}

impl CurrentUserSecurity {
    /// Builds a protected DACL granting full access only to SYSTEM and the caller SID.
    fn new() -> Result<Self, JlinkError> {
        let sid = current_user_sid_string()?;
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor = ptr::null_mut::<c_void>();
        // SAFETY: `sddl` is NUL-terminated and `descriptor` is a valid out parameter.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 || descriptor.is_null() {
            return Err(last_pipe_error("无法创建当前用户管道安全描述符"));
        }
        Ok(Self { descriptor })
    }

    fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(mem::size_of::<SECURITY_ATTRIBUTES>())
                .expect("SECURITY_ATTRIBUTES size fits u32"),
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        }
    }
}

impl Drop for CurrentUserSecurity {
    fn drop(&mut self) {
        if !self.descriptor.is_null() {
            // SAFETY: the descriptor was allocated by LocalAlloc through the conversion API.
            let _ = unsafe { LocalFree(self.descriptor) };
        }
    }
}

/// Reads the current process token SID and converts it to an SDDL SID string.
fn current_user_sid_string() -> Result<String, JlinkError> {
    let mut raw_token = ptr::null_mut();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle and `raw_token` is writable.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut raw_token) };
    if opened == 0 || raw_token.is_null() {
        return Err(last_pipe_error("无法读取当前用户令牌"));
    }
    // SAFETY: `raw_token` is a unique valid token handle returned by OpenProcessToken.
    let token = unsafe { OwnedHandle::from_raw_handle(raw_token) };
    let mut required = 0_u32;
    // SAFETY: the null buffer/zero length call requests the required token buffer size.
    let first = unsafe {
        windows_sys::Win32::Security::GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &raw mut required,
        )
    };
    // SAFETY: GetLastError is read immediately after the sizing call.
    let sizing_error = unsafe { GetLastError() };
    if first != 0 || sizing_error != ERROR_INSUFFICIENT_BUFFER || required == 0 {
        return Err(windows_pipe_error(
            "无法确定当前用户 SID 缓冲区",
            sizing_error,
        ));
    }
    let mut buffer = vec![0_u8; required as usize];
    // SAFETY: `buffer` is writable for `required` bytes and all out parameters are valid.
    let loaded = unsafe {
        windows_sys::Win32::Security::GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            required,
            &raw mut required,
        )
    };
    if loaded == 0 || buffer.len() < mem::size_of::<TOKEN_USER>() {
        return Err(last_pipe_error("无法读取当前用户 SID"));
    }
    // SAFETY: GetTokenInformation wrote a TOKEN_USER at the start of `buffer`;
    // read_unaligned avoids assuming the byte vector's alignment.
    let token_user = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<TOKEN_USER>()) };
    let mut sid_text: PWSTR = ptr::null_mut();
    // SAFETY: the SID points inside the live token buffer and `sid_text` is writable.
    let converted = unsafe { ConvertSidToStringSidW(token_user.User.Sid, &raw mut sid_text) };
    if converted == 0 || sid_text.is_null() {
        return Err(last_pipe_error("无法格式化当前用户 SID"));
    }
    let mut length = 0_usize;
    // SAFETY: ConvertSidToStringSidW returned a NUL-terminated LocalAlloc string.
    unsafe {
        while *sid_text.add(length) != 0 {
            length += 1;
        }
    }
    // SAFETY: the preceding scan found the terminating NUL within the API allocation.
    let sid = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid_text, length) });
    // SAFETY: `sid_text` was allocated by LocalAlloc and is freed exactly once here.
    let _ = unsafe { LocalFree(sid_text.cast::<c_void>()) };
    Ok(sid)
}

fn last_pipe_error(context: &str) -> JlinkError {
    // SAFETY: GetLastError has no preconditions and is called at the failure site.
    windows_pipe_error(context, unsafe { GetLastError() })
}

fn windows_pipe_error(context: &str, code: u32) -> JlinkError {
    JlinkError::new(
        ErrorCode::WorkerUnavailable,
        format!("{context}（Windows 错误 {code}）"),
        true,
    )
}
