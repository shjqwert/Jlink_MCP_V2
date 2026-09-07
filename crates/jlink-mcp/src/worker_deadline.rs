//! Cancelable named-pipe I/O; a deadline covers the complete framed exchange.
use std::{
    io::{self, Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::{ERROR_IO_PENDING, WAIT_OBJECT_0},
    Storage::FileSystem::{ReadFile, WriteFile},
    System::{
        IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        Threading::{CreateEventW, WaitForSingleObject},
    },
};

pub(super) struct DeadlinePipe {
    handle: OwnedHandle,
    deadline: Instant,
    pub(super) timed_out: bool,
}

impl DeadlinePipe {
    pub(super) fn new(handle: OwnedHandle, deadline: Instant) -> Self {
        Self {
            handle,
            deadline,
            timed_out: false,
        }
    }

    fn transfer(&mut self, buffer: *mut u8, length: usize, writing: bool) -> io::Result<usize> {
        if length == 0 {
            return Ok(0);
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.timed_out = true;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Worker request deadline exceeded",
            ));
        }
        // SAFETY: null attributes/name create a private, initially unsignaled event.
        let raw_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if raw_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful event handle is uniquely owned here.
        let event = unsafe { OwnedHandle::from_raw_handle(raw_event) };
        // SAFETY: all-zero is a valid initial OVERLAPPED; the event stays alive below.
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event.as_raw_handle();
        let mut transferred = 0;
        let count = u32::try_from(length).map_err(|_| io::Error::other("IPC buffer too large"))?;
        // SAFETY: callers provide readable/writable buffers for count bytes. Both buffer
        // and OVERLAPPED remain alive until completion or confirmed cancellation.
        let result = unsafe {
            if writing {
                WriteFile(
                    self.handle.as_raw_handle(),
                    buffer.cast_const(),
                    count,
                    ptr::null_mut(),
                    &raw mut overlapped,
                )
            } else {
                ReadFile(
                    self.handle.as_raw_handle(),
                    buffer,
                    count,
                    ptr::null_mut(),
                    &raw mut overlapped,
                )
            }
        };
        if result == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING.cast_signed()) {
                return Err(error);
            }
            let milliseconds = u32::try_from(remaining.as_millis())
                .unwrap_or(u32::MAX - 1)
                .max(1);
            // SAFETY: event is the live event associated with this pending operation.
            let wait = unsafe { WaitForSingleObject(event.as_raw_handle(), milliseconds) };
            if wait != WAIT_OBJECT_0 {
                // SAFETY: cancel exactly this operation and drain its kernel completion
                // before returning, so no borrowed buffer can outlive the call.
                unsafe {
                    CancelIoEx(self.handle.as_raw_handle(), &raw const overlapped);
                    GetOverlappedResult(
                        self.handle.as_raw_handle(),
                        &raw const overlapped,
                        &raw mut transferred,
                        1,
                    );
                }
                self.timed_out = true;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Worker request deadline exceeded",
                ));
            }
        }
        // SAFETY: this operation completed, and transferred is writable.
        if unsafe {
            GetOverlappedResult(
                self.handle.as_raw_handle(),
                &raw const overlapped,
                &raw mut transferred,
                0,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(transferred as usize)
    }
}

impl Read for DeadlinePipe {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.transfer(buffer.as_mut_ptr(), buffer.len(), false)
    }
}
impl Write for DeadlinePipe {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.transfer(buffer.as_ptr().cast_mut(), buffer.len(), true)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
