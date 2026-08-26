use std::{
    marker::PhantomData,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    rc::Rc,
};

use jlink_domain::{ErrorCode, JlinkError};
use windows_sys::Win32::{
    Foundation::{FreeLibrary, GetLastError, HMODULE},
    System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LoadLibraryExW,
    },
};

/// The only owner allowed to hold the J-Link module and future function pointers.
///
/// The `Rc` marker intentionally keeps this value on one Worker thread. V1 DLL
/// calls are added as `&mut self` operations so two threads cannot enter the
/// same module through this boundary.
pub(crate) struct DllGateway {
    module: HMODULE,
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
        Ok(Self {
            module,
            _path: path.to_path_buf(),
            _single_thread: PhantomData,
        })
    }

    /// Reports whether this gateway currently owns a loaded module.
    pub(crate) const fn is_loaded(&self) -> bool {
        !self.module.is_null()
    }
}

impl Drop for DllGateway {
    fn drop(&mut self) {
        if !self.module.is_null() {
            // SAFETY: `module` was returned by LoadLibraryExW and is freed exactly once here.
            let _ = unsafe { FreeLibrary(self.module) };
        }
    }
}
