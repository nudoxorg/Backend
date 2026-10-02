//! Owned Windows child capture. Windows 10 / Server 2016 or later is required
//! for creation-time JOB_LIST membership; unavailable attributes fail closed.
//! The Job has KILL_ON_JOB_CLOSE and never permits breakaway. Only NUL stdin
//! and the two pipe writers are inherited. No reader threads or PeekNamedPipe
//! are involved. Pipe reads, leader status, and cancellation are polled with
//! zero-wait APIs under one absolute deadline.
//!
//! The absolute budget controls preflight, the final check before process
//! creation, and every capture-poll turn. Native process/pipe creation,
//! security setup and ordinary filesystem probes cannot be preempted by this
//! synchronous adapter; an OS syscall can itself outlast the deadline. Cleanup
//! adds at most 100ms of application polling, with the same native-syscall
//! limitation. This is bounded polling and ownership, not a claim that Windows
//! guarantees a wall-clock bound for all kernel/filesystem operations.
//!
//! Cancellation is not completion: an OVERLAPPED and its buffer cannot be
//! freed until the kernel completes the request. Cleanup polls for at most
//! 100ms after closing the Job. In the exceptional case that the kernel does
//! not complete cancellation, process and pipe handles close, while the
//! affected operation and its completion port transfer to a fixed 32-slot
//! retirement pool. Later calls reclaim only after receiving the IOCP packet.
//! Exhaustion refuses new work before spawning. Cleanup reports delayed
//! retirement; ordinary completion and cancellation retain nothing.
#![allow(
    unsafe_code,
    reason = "audited Win32 process and overlapped I/O ownership boundary"
)]

use crate::child_output::{
    CaptureCommand, CaptureEnvironment, CaptureError, CaptureLimits, CapturedOutput, OutputStream,
};
use std::{
    cell::UnsafeCell,
    cmp::Ordering as CmpOrdering,
    ffi::{OsStr, OsString},
    io,
    marker::PhantomPinned,
    mem,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, RawHandle},
        process::ExitStatusExt,
    },
    path::PathBuf,
    pin::Pin,
    ptr,
    sync::atomic::{AtomicBool, AtomicPtr, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED,
        ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    Globalization::{CSTR_EQUAL, CSTR_GREATER_THAN, CSTR_LESS_THAN, CompareStringOrdinal},
    Security::SECURITY_ATTRIBUTES,
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, PIPE_ACCESS_INBOUND, ReadFile, WRITE_DAC,
    },
    System::{
        Environment::{FreeEnvironmentStringsW, GetEnvironmentStringsW},
        IO::{CancelIoEx, CreateIoCompletionPort, GetQueuedCompletionStatus, OVERLAPPED},
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        },
        Pipes::{
            ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_TYPE_BYTE, PIPE_WAIT,
        },
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
        },
    },
};

const CHUNK: usize = 8192;
const POLL: Duration = Duration::from_millis(1);
const CLEANUP: Duration = Duration::from_millis(100);

/// Sole owner of a real kernel handle; pseudo handles are never accepted.
struct Handle(HANDLE);
impl Handle {
    fn new(raw: HANDLE) -> io::Result<Self> {
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }
}
impl AsRawHandle for Handle {
    fn as_raw_handle(&self) -> RawHandle {
        self.0.cast()
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: this value uniquely owns a successfully opened, real handle.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new() -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: null requests the size for two attributes; bytes is writable.
        unsafe {
            InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let words = bytes.div_ceil(size_of::<usize>());
        let mut this = Self {
            storage: vec![0; words],
            initialized: false,
        };
        // SAFETY: the aligned storage covers the size returned by Windows.
        if unsafe { InitializeProcThreadAttributeList(this.as_mut_ptr(), 2, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }
        this.initialized = true;
        Ok(this)
    }
    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
    fn set(&mut self, attribute: u32, handles: &[HANDLE]) -> io::Result<()> {
        // SAFETY: callers keep these arrays and every referenced handle alive
        // until this list is deleted; Windows receives their exact byte size.
        if unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                attribute as usize,
                handles.as_ptr().cast(),
                size_of_val(handles),
                ptr::null_mut(),
                ptr::null(),
            )
        } == 0
        {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: one successful initialization, and backing storage lives.
            unsafe {
                DeleteProcThreadAttributeList(self.as_mut_ptr());
            }
        }
    }
}

// At most one request is in flight per port. The IOCP packet, including
// failed/cancelled packets, is the ownership proof for its stable allocation.
struct Operation {
    // Kernel writes continue after ReadFile returns. Interior mutability
    // prevents creating Rust aliases that forbid those writes. The pinned
    // allocation never moves until the actual IOCP ownership receipt arrives.
    overlapped: UnsafeCell<OVERLAPPED>,
    bytes: UnsafeCell<[u8; CHUNK]>,
    _pin: PhantomPinned,
}
impl Operation {
    fn new() -> Pin<Box<Self>> {
        Box::pin(Self {
            overlapped: UnsafeCell::new(OVERLAPPED::default()),
            bytes: UnsafeCell::new([0; CHUNK]),
            _pin: PhantomPinned,
        })
    }
    fn request(&self) -> *mut OVERLAPPED {
        self.overlapped.get()
    }
    fn buffer(&self) -> *mut u8 {
        self.bytes.get().cast()
    }
}
struct Retired {
    port: Handle,
    operation: Pin<Box<Operation>>,
}
const MAX_OPERATIONS: usize = 32;
const RESERVED: *mut Retired = ptr::without_provenance_mut(1);
static OPERATIONS: [AtomicPtr<Retired>; MAX_OPERATIONS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; MAX_OPERATIONS];

struct Slot(usize);
impl Slot {
    fn reserve() -> Result<Self, CaptureError> {
        sweep_retired();
        for (index, cell) in OPERATIONS.iter().enumerate() {
            if cell
                .compare_exchange(
                    ptr::null_mut(),
                    RESERVED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return Ok(Self(index));
            }
        }
        Err(CaptureError::Capacity {
            maximum: MAX_OPERATIONS,
        })
    }
    fn retire(self, retired: Retired) {
        OPERATIONS[self.0].store(Box::into_raw(Box::new(retired)), Ordering::Release);
        // Ownership moved into this exact slot; Slot's drop must not clear it.
        mem::forget(self);
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        OPERATIONS[self.0].store(ptr::null_mut(), Ordering::Release);
    }
}
fn sweep_retired() {
    for cell in &OPERATIONS {
        let pointer = cell.load(Ordering::Acquire);
        if pointer.is_null() || pointer == RESERVED {
            continue;
        }
        if cell
            .compare_exchange(pointer, RESERVED, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            continue;
        }
        // SAFETY: the successful CAS exclusively owns the initialized box that
        // retire published. Other callers cannot dereference or mutate it.
        let retired = unsafe { Box::from_raw(pointer) };
        let completion = completion(retired.port.0, retired.operation.request());
        if matches!(completion, Ok(Some(_))) || matches!(completion, Err((true, _))) {
            drop(retired); // IOCP certified the operation has actually completed.
            cell.store(ptr::null_mut(), Ordering::Release);
        } else {
            cell.store(Box::into_raw(retired), Ordering::Release);
        }
    }
}

// Bool on errors says whether this operation's completion was dequeued. A
// timeout has no packet and never authorizes freeing its storage.
fn completion(port: HANDLE, expected: *mut OVERLAPPED) -> Result<Option<usize>, (bool, io::Error)> {
    // GQCS associates even callers that time out with their port. Every call
    // explicitly switches to a private empty port, then closes that port;
    // hence no live caller stays associated with a capture/retirement port.
    // Allocate the detach owner BEFORE touching the target. If allocation
    // fails, there has been no new association or ownership receipt to lose.
    // SAFETY: INVALID_HANDLE_VALUE creates a new port without any file handle.
    let detach =
        Handle::new(unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, 1) })
            .map_err(|source| (false, source))?;
    let mut bytes = 0;
    let mut key = 0;
    let mut request = ptr::null_mut();
    // SAFETY: owned completion port, initialized writable out pointers, zero
    // timeout. Every submitted operation remains allocated until this packet.
    let success = unsafe { GetQueuedCompletionStatus(port, &mut bytes, &mut key, &mut request, 0) };
    // SAFETY: capture the target's error before the detach call overwrites it.
    let error = if success == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    let result = if request.is_null() {
        if success == 0 && error == WAIT_TIMEOUT {
            Ok(None)
        } else {
            Err((false, io::Error::from_raw_os_error(error as i32)))
        }
    } else if !ptr::eq(request, expected) {
        Err((false, io::Error::other("unexpected IOCP operation pointer")))
    } else if success != 0 {
        Ok(Some(bytes as usize))
    } else {
        Err((true, io::Error::from_raw_os_error(error as i32)))
    };

    let mut ignored_bytes = 0;
    let mut ignored_key = 0;
    let mut ignored_request = ptr::null_mut();
    // SAFETY: this private empty port has no files or posted packets. Calling
    // GQCS with valid out pointers and zero wait switches this thread's port
    // association; closing the new port then clears it (Microsoft IOCP rules).
    let detached = unsafe {
        GetQueuedCompletionStatus(
            detach.0,
            &mut ignored_bytes,
            &mut ignored_key,
            &mut ignored_request,
            0,
        )
    };
    // SAFETY: retrieve the detach API's error before closing its owned handle.
    let detach_error = if detached == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    drop(detach);
    if detached == 0 && ignored_request.is_null() && detach_error == WAIT_TIMEOUT {
        result
    } else {
        let completed = matches!(&result, Ok(Some(_)) | Err((true, _)));
        Err((
            completed,
            io::Error::other(format!(
                "unexpected private IOCP detach result: {detach_error}"
            )),
        ))
    }
}

struct Pipe {
    handle: Option<Handle>,
    port: Option<Handle>,
    operation: Option<Pin<Box<Operation>>>,
    slot: Option<Slot>,
    pending: bool,
    eof: bool,
}
impl Pipe {
    fn pair(slot: Slot) -> io::Result<(Self, Handle)> {
        let mut random = [0u8; 16];
        crate::win32::random::fill(&mut random)?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = wide(OsStr::new(&format!(
            r"\\.\pipe\backend-capture-{}-{token}",
            std::process::id()
        )))?;
        // Random first-instance names exclude substitution; remote clients are
        // denied and the owner DACL is installed before opening the writer.
        // SAFETY: terminated name, bounded buffers, noninheritable server.
        let handle = Handle::new(unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_INBOUND
                    | FILE_FLAG_OVERLAPPED
                    | FILE_FLAG_FIRST_PIPE_INSTANCE
                    | WRITE_DAC,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                CHUNK as u32,
                CHUNK as u32,
                0,
                ptr::null(),
            )
        })?;
        crate::win32::security::restrict_handle_to_current_user(&handle)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: 1,
        };
        // SAFETY: named local instance exists; only this writer is inherited.
        let writer = Handle::new(unsafe {
            CreateFileW(
                name.as_ptr(),
                windows_sys::Win32::Foundation::GENERIC_WRITE,
                0,
                &attributes,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                ptr::null_mut(),
            )
        })?;
        let operation = Operation::new();
        // The client is already connected, so ConnectNamedPipe has no pending
        // request on partial startup. Associate the port only AFTER connecting.
        // SAFETY: stable writable OVERLAPPED; the client was opened above.
        if unsafe { ConnectNamedPipe(handle.0, operation.request()) } == 0 {
            // SAFETY: immediately read the preceding error.
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_CONNECTED {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
        // Every completion poll detaches its thread before returning. A live
        // Pipe has one caller; a retired slot has one CAS owner, so concurrency
        // one cannot accumulate associations from earlier runnable callers.
        // SAFETY: associate this sole owned server handle with a new owned port.
        let port = Handle::new(unsafe { CreateIoCompletionPort(handle.0, ptr::null_mut(), 0, 1) })?;
        Ok((
            Self {
                handle: Some(handle),
                port: Some(port),
                operation: Some(operation),
                slot: Some(slot),
                pending: false,
                eof: false,
            },
            writer,
        ))
    }

    fn poll(
        &mut self,
        output: &mut Vec<u8>,
        maximum: usize,
        stream: OutputStream,
    ) -> Result<(), CaptureError> {
        if self.eof {
            return Ok(());
        }
        let op = self
            .operation
            .as_ref()
            .expect("operation present until cleanup");
        if !self.pending {
            // SAFETY: pending is false only before first submission or after
            // an actual completion packet (including failed packets). There
            // are no kernel or Rust accesses to this storage during reset.
            unsafe {
                op.request().write(OVERLAPPED::default());
            }
            let read_size = maximum
                .saturating_sub(output.len())
                .saturating_add(1)
                .min(CHUNK);
            // SAFETY: stable storage, at most one outstanding read, bounded
            // byte count; null event means every completion posts to this IOCP.
            let success = unsafe {
                ReadFile(
                    self.handle.as_ref().expect("live pipe").0,
                    op.buffer(),
                    read_size as u32,
                    ptr::null_mut(),
                    op.request(),
                )
            };
            if success == 0 {
                // SAFETY: immediately retrieve this ReadFile error.
                let error = unsafe { GetLastError() };
                if error == ERROR_BROKEN_PIPE {
                    self.eof = true;
                    return Ok(());
                }
                if error != ERROR_IO_PENDING {
                    return Err(CaptureError::Read {
                        stream,
                        source: io::Error::from_raw_os_error(error as i32),
                    });
                }
            }
            // Synchronous success ALSO queues a completion packet. No storage
            // reuse or buffer access is permitted until that packet is dequeued.
            self.pending = true;
        }
        let count = match completion(self.port.as_ref().expect("live IOCP").0, op.request()) {
            Ok(None) => return Ok(()),
            Ok(Some(count)) => {
                self.pending = false;
                count
            }
            Err((completed, source)) => {
                self.pending = !completed;
                if completed && source.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    self.eof = true;
                    return Ok(());
                }
                return Err(CaptureError::Read { stream, source });
            }
        };
        if count == 0 {
            self.eof = true;
            return Ok(());
        }
        if count > CHUNK {
            return Err(CaptureError::Read {
                stream,
                source: io::Error::other("invalid completion byte count"),
            });
        }
        let observed = output.len().saturating_add(count);
        if observed > maximum {
            return Err(CaptureError::OutputLimit {
                stream,
                observed,
                maximum,
            });
        }
        // SAFETY: the matching IOCP packet was dequeued above and pending
        // cleared. The kernel can no longer access either field. count was
        // checked against the allocation size before forming this shared slice.
        let bytes = unsafe { std::slice::from_raw_parts(op.buffer().cast_const(), count) };
        output.extend_from_slice(bytes);
        Ok(())
    }
    fn cancel(&mut self) -> io::Result<()> {
        if !self.pending {
            return Ok(());
        }
        let op = self
            .operation
            .as_ref()
            .expect("pending operation has storage");
        // SAFETY: cancel the one owned pending request without waiting.
        if unsafe { CancelIoEx(self.handle.as_ref().expect("live pipe").0, op.request()) } == 0 {
            // SAFETY: immediately retrieve this CancelIoEx error.
            let error = unsafe { GetLastError() };
            if error != ERROR_NOT_FOUND {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }
        Ok(())
    }
    fn observe_cancel(&mut self) -> io::Result<()> {
        if !self.pending {
            return Ok(());
        }
        let op = self
            .operation
            .as_ref()
            .expect("pending operation has storage");
        match completion(self.port.as_ref().expect("live IOCP").0, op.request()) {
            Ok(Some(_)) => {
                self.pending = false;
                Ok(())
            }
            Ok(None) => Ok(()),
            Err((completed, source)) => {
                self.pending = !completed;
                if completed
                    && matches!(source.raw_os_error(), Some(code) if
                    code == ERROR_OPERATION_ABORTED as i32 || code == ERROR_BROKEN_PIPE as i32)
                {
                    Ok(())
                } else {
                    Err(source)
                }
            }
        }
    }
}
impl Drop for Pipe {
    fn drop(&mut self) {
        if self.pending {
            let _ = self.cancel();
            drop(self.handle.take()); // cancel/close the server before retirement
            let retired = Retired {
                port: self.port.take().expect("pending port"),
                operation: self.operation.take().expect("pending storage"),
            };
            self.slot
                .take()
                .expect("reserved retirement slot")
                .retire(retired);
        }
        // Completed storage, ports, and slot permits drop normally. This also
        // keeps unwind memory-safe without an unbounded wait or allocation pool.
    }
}

fn budget(limits: CaptureLimits, cancelled: &AtomicBool) -> Result<(), CaptureError> {
    if cancelled.load(Ordering::Acquire) {
        Err(CaptureError::Cancelled)
    } else if Instant::now() >= limits.deadline {
        Err(CaptureError::Deadline)
    } else {
        Ok(())
    }
}

pub(crate) fn capture(
    command: &CaptureCommand,
    limits: CaptureLimits,
    cancelled: &AtomicBool,
) -> Result<CapturedOutput, CaptureError> {
    budget(limits, cancelled)?;
    let stdout_slot = Slot::reserve()?;
    let stderr_slot = Slot::reserve()?;
    let environment = environment_block(command).map_err(CaptureError::Spawn)?;
    let executable = resolve_program(command, &environment).map_err(CaptureError::Spawn)?;
    let application = wide(executable.as_os_str()).map_err(CaptureError::Spawn)?;
    let mut arguments =
        command_line(executable.as_os_str(), &command.args).map_err(CaptureError::Spawn)?;
    let cwd = command
        .cwd
        .as_ref()
        .map(|path| wide(path.as_os_str()))
        .transpose()
        .map_err(CaptureError::Spawn)?;
    // SAFETY: unnamed, noninheritable Job; handle owned immediately.
    let job = Handle::new(unsafe { CreateJobObjectW(ptr::null(), ptr::null()) })
        .map_err(CaptureError::Spawn)?;
    let mut policy = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    policy.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: exact initialized ABI structure, and real Job handle.
    if unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&policy as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&policy) as u32,
        )
    } == 0
    {
        return Err(CaptureError::Spawn(io::Error::last_os_error()));
    }
    let (mut stdout, stdout_writer) =
        Pipe::pair(stdout_slot).map_err(|source| CaptureError::Configure {
            stream: OutputStream::Stdout,
            source,
        })?;
    let (mut stderr, stderr_writer) =
        Pipe::pair(stderr_slot).map_err(|source| CaptureError::Configure {
            stream: OutputStream::Stderr,
            source,
        })?;
    let inheritable = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: ptr::null_mut(),
        bInheritHandle: 1,
    };
    let nul = wide(OsStr::new("NUL")).map_err(CaptureError::Spawn)?;
    // SAFETY: owned NUL handle, opened for read and deliberately inherited.
    let stdin = Handle::new(unsafe {
        CreateFileW(
            nul.as_ptr(),
            windows_sys::Win32::Foundation::GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &inheritable,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    })
    .map_err(CaptureError::Spawn)?;
    let inherited = [stdin.0, stdout_writer.0, stderr_writer.0];
    let jobs = [job.0];
    let mut attributes = Attributes::new().map_err(CaptureError::Spawn)?;
    attributes
        .set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &inherited)
        .map_err(CaptureError::Spawn)?;
    if let Err(source) = attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs) {
        // No assign-after-spawn fallback: unsupported birth containment means
        // no child is started. Keep actual operational errors distinguishable.
        if matches!(source.raw_os_error(), Some(50 | 87 | 120)) {
            return Err(CaptureError::Unsupported);
        }
        return Err(CaptureError::Spawn(source));
    }
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = stdin.0;
    startup.StartupInfo.hStdOutput = stdout_writer.0;
    startup.StartupInfo.hStdError = stderr_writer.0;
    startup.lpAttributeList = attributes.as_mut_ptr();
    let mut process = PROCESS_INFORMATION::default();
    budget(limits, cancelled)?;
    // SAFETY: all strings are terminated UTF16 without embedded NUL; the
    // command line is writable; environment is double terminated. Attributes
    // and their referenced arrays/handles outlive creation. JOB_LIST assigns
    // the Job before the initial thread runs; HANDLE_LIST limits inheritance.
    if unsafe {
        CreateProcessW(
            application.as_ptr(),
            arguments.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            1,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            environment.as_ptr().cast(),
            cwd.as_ref().map_or(ptr::null(), |value| value.as_ptr()),
            &startup.StartupInfo,
            &mut process,
        )
    } == 0
    {
        return Err(CaptureError::Spawn(io::Error::last_os_error()));
    }
    let leader = Handle::new(process.hProcess).map_err(CaptureError::Spawn)?;
    let initial_thread = Handle::new(process.hThread).map_err(CaptureError::Spawn)?;
    drop(attributes); // delete list before closing the referenced handles
    drop(initial_thread);
    drop(stdin);
    drop(stdout_writer);
    drop(stderr_writer);
    let mut job = Some(job);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut status = None;
    let result = (|| {
        loop {
            budget(limits, cancelled)?;
            stdout.poll(&mut out, limits.stdout_bytes, OutputStream::Stdout)?;
            stderr.poll(&mut err, limits.stderr_bytes, OutputStream::Stderr)?;
            if status.is_none() {
                // SAFETY: valid owned leader, zero timeout never blocks.
                match unsafe { WaitForSingleObject(leader.0, 0) } {
                    WAIT_OBJECT_0 => {
                        let mut code = 0;
                        // SAFETY: leader has signaled, so code is its exit status
                        // (even if it equals STILL_ACTIVE); writable out pointer.
                        if unsafe { GetExitCodeProcess(leader.0, &mut code) } == 0 {
                            return Err(CaptureError::Wait(io::Error::last_os_error()));
                        }
                        status = Some(std::process::ExitStatus::from_raw(code));
                        // Kill descendants even on successful leader exit, so
                        // inherited writers cannot extend the capture lifetime.
                        drop(job.take());
                    }
                    WAIT_TIMEOUT => {}
                    _ => return Err(CaptureError::Wait(io::Error::last_os_error())),
                }
            }
            if stdout.eof
                && stderr.eof
                && let Some(status) = status
            {
                return Ok(CapturedOutput {
                    status,
                    stdout: mem::take(&mut out),
                    stderr: mem::take(&mut err),
                });
            }
            thread::sleep(POLL.min(limits.deadline.saturating_duration_since(Instant::now())));
        }
    })();
    drop(job.take());
    let cleanup = finish_pipes(&mut stdout, &mut stderr);
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Err(primary), Err(source)) => Err(CaptureError::Cleanup {
            primary: Box::new(primary),
            source,
        }),
        (Ok(_), Err(source)) => Err(CaptureError::Cleanup {
            primary: Box::new(CaptureError::Wait(io::Error::other(
                "capture completion cleanup failed",
            ))),
            source,
        }),
    }
}

fn finish_pipes(stdout: &mut Pipe, stderr: &mut Pipe) -> io::Result<()> {
    let mut error = stdout.cancel().err();
    if let Err(source) = stderr.cancel() {
        error.get_or_insert(source);
    }
    sweep_retired();
    let deadline = Instant::now() + CLEANUP;
    loop {
        for pipe in [&mut *stdout, &mut *stderr] {
            if let Err(source) = pipe.observe_cancel() {
                error.get_or_insert(source);
            }
        }
        if !stdout.pending && !stderr.pending {
            return error.map_or(Ok(()), Err);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "overlapped cancellation did not complete; operation transferred to bounded retirement pool",
            ));
        }
        thread::sleep(POLL);
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = value.encode_wide().collect();
    if units.contains(&0) {
        return Err(invalid("Windows command contains embedded NUL"));
    }
    units.push(0);
    Ok(units)
}

// Implements the Microsoft CRT backslash/quote convention. Shell scripts are
// deliberately excluded; Cargo and rustc use this native executable contract.
fn command_line(program: &OsStr, args: &[OsString]) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    for value in std::iter::once(program).chain(args.iter().map(OsString::as_os_str)) {
        if !line.is_empty() {
            line.push(b' ' as u16);
        }
        let value = wide(value)?;
        line.push(b'"' as u16);
        let mut slashes = 0;
        for unit in value[..value.len() - 1].iter().copied() {
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            let count = if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            };
            line.extend(std::iter::repeat_n(b'\\' as u16, count));
            slashes = 0;
            line.push(unit);
        }
        line.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        line.push(b'"' as u16);
    }
    line.push(0);
    if line.len() > 32767 {
        return Err(invalid("Windows command exceeds CreateProcessW limit"));
    }
    Ok(line)
}

fn ordinal_lengths(left: usize, right: usize) -> io::Result<(i32, i32)> {
    Ok((
        i32::try_from(left).map_err(|_| invalid("Windows name length exceeds i32"))?,
        i32::try_from(right).map_err(|_| invalid("Windows name length exceeds i32"))?,
    ))
}

fn ordinal_result(result: i32, error: u32) -> io::Result<CmpOrdering> {
    match result {
        CSTR_LESS_THAN => Ok(CmpOrdering::Less),
        CSTR_EQUAL => Ok(CmpOrdering::Equal),
        CSTR_GREATER_THAN => Ok(CmpOrdering::Greater),
        0 => Err(io::Error::from_raw_os_error(error as i32)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid Windows ordinal comparison result",
        )),
    }
}

fn compare_names(left: &[u16], right: &[u16]) -> io::Result<CmpOrdering> {
    let (left_length, right_length) = ordinal_lengths(left.len(), right.len())?;
    // The empty-name order is unambiguous and avoids passing a dangling empty
    // slice pointer to a native API whose pointer contract expects a string.
    if left.is_empty() || right.is_empty() {
        return Ok(left.len().cmp(&right.len()));
    }
    // SAFETY: both readable UTF16 slices remain live through the call. Their
    // exact lengths were checked before narrowing, including unpaired UTF16.
    let result = unsafe {
        CompareStringOrdinal(left.as_ptr(), left_length, right.as_ptr(), right_length, 1)
    };
    // SAFETY: capture native failure immediately, without reclassifying it as
    // equality or allowing a partial environment mutation to reach the child.
    let error = if result == 0 {
        unsafe { GetLastError() }
    } else {
        0
    };
    ordinal_result(result, error)
}

type EnvironmentEntry = (Vec<u16>, Vec<u16>);
fn sort_environment(mut entries: Vec<EnvironmentEntry>) -> io::Result<Vec<EnvironmentEntry>> {
    // A fallible merge sort avoids pretending an OS comparison failure is
    // Equal inside a sort_by closure. Every comparison either orders the
    // entries or stops command preparation before any process is created.
    if entries.len() < 2 {
        return Ok(entries);
    }
    let right = entries.split_off(entries.len() / 2);
    let mut left = sort_environment(entries)?.into_iter().peekable();
    let mut right = sort_environment(right)?.into_iter().peekable();
    let mut ordered = Vec::with_capacity(left.len() + right.len());
    while let (Some(a), Some(b)) = (left.peek(), right.peek()) {
        if compare_names(&a.0, &b.0)? != CmpOrdering::Greater {
            ordered.push(left.next().expect("peeked left entry"));
        } else {
            ordered.push(right.next().expect("peeked right entry"));
        }
    }
    ordered.extend(left);
    ordered.extend(right);
    Ok(ordered)
}

fn environment_block(command: &CaptureCommand) -> io::Result<Vec<u16>> {
    let mut entries: Vec<EnvironmentEntry> = Vec::new();
    if matches!(command.environment, CaptureEnvironment::Inherit) {
        // GetEnvironmentStrings preserves Windows' hidden drive-directory
        // entries and ill-formed UTF16 rather than taking a lossy UTF8 detour.
        // SAFETY: API returns an owned double-terminated environment allocation.
        let raw = unsafe { GetEnvironmentStringsW() };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        struct Environment(*mut u16);
        impl Drop for Environment {
            fn drop(&mut self) {
                // SAFETY: exact pointer returned by GetEnvironmentStringsW.
                unsafe {
                    FreeEnvironmentStringsW(self.0);
                }
            }
        }
        let _owner = Environment(raw);
        let mut offset = 0;
        loop {
            // SAFETY: Windows guarantees readable entries through the final
            // empty entry. We walk one unit at a time within that allocation.
            if unsafe { *raw.add(offset) } == 0 {
                break;
            }
            let start = offset;
            while unsafe { *raw.add(offset) } != 0 {
                offset += 1;
            }
            // SAFETY: the preceding scan established exactly this entry length.
            let entry = unsafe { std::slice::from_raw_parts(raw.add(start), offset - start) };
            if let Some(separator) = entry
                .iter()
                .enumerate()
                .skip(1)
                .find_map(|(i, unit)| (*unit == b'=' as u16).then_some(i))
            {
                entries.push((entry[..separator].to_vec(), entry[separator + 1..].to_vec()));
            }
            offset += 1;
        }
    }
    for (name, value) in &command.overrides {
        let mut name = wide(name)?;
        name.pop();
        if name.is_empty() || name.contains(&(b'=' as u16)) {
            return Err(invalid("invalid Windows environment name"));
        }
        let mut retained = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            if compare_names(&key, &name)? != CmpOrdering::Equal {
                retained.push((key, value));
            }
        }
        entries = retained;
        if let Some(value) = value {
            let mut value = wide(value)?;
            value.pop();
            entries.push((name, value));
        }
    }
    let entries = sort_environment(entries)?;
    let mut block = Vec::new();
    for (name, value) in entries {
        block.extend(name);
        block.push(b'=' as u16);
        block.extend(value);
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn resolve_program(command: &CaptureCommand, environment: &[u16]) -> io::Result<PathBuf> {
    let mut program = PathBuf::from(&command.program);
    if program.as_os_str().is_empty() {
        return Err(invalid("empty Windows program"));
    }
    if program.extension().is_none() {
        program.set_extension("exe");
    }
    let exe: Vec<_> = OsStr::new("exe").encode_wide().collect();
    if let Some(extension) = program.extension() {
        let extension: Vec<_> = extension.encode_wide().collect();
        if compare_names(&extension, &exe)? != CmpOrdering::Equal {
            return Err(invalid(
                "bounded Windows capture requires a native .exe program",
            ));
        }
    }
    let cwd = command.cwd.clone().unwrap_or(std::env::current_dir()?);
    if program.is_absolute() {
        return Ok(program);
    }
    if program.components().count() > 1 {
        return Ok(cwd.join(program));
    }
    let path_name: Vec<_> = OsStr::new("PATH").encode_wide().collect();
    for entry in environment
        .split(|unit| *unit == 0)
        .filter(|entry| !entry.is_empty())
    {
        if let Some(separator) = entry.iter().position(|unit| *unit == b'=' as u16)
            && compare_names(&entry[..separator], &path_name)? == CmpOrdering::Equal
        {
            for directory in std::env::split_paths(&OsString::from_wide(&entry[separator + 1..])) {
                let candidate = if directory.is_absolute() {
                    directory.join(&program)
                } else {
                    cwd.join(directory).join(&program)
                };
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "native program not found in effective PATH",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        process::{Command, Stdio},
        sync::Mutex,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, OpenProcess, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, TerminateProcess,
    };

    // Serialize this module's process-wide capacity/handle-accounting tests.
    // This mutex is test-only, never part of the capture or retirement path.
    static TESTS: Mutex<()> = Mutex::new(());
    const FIXTURE: &str = "windows_child_output::tests::capture_fixture";

    fn command(mode: &str) -> CaptureCommand {
        CaptureCommand {
            program: std::env::current_exe()
                .expect("test executable")
                .into_os_string(),
            args: vec!["--exact".into(), FIXTURE.into(), "--nocapture".into()],
            cwd: None,
            environment: CaptureEnvironment::Inherit,
            overrides: vec![("BACKEND_CAPTURE_FIXTURE".into(), Some(mode.into()))],
        }
    }
    fn limits() -> CaptureLimits {
        CaptureLimits {
            deadline: Instant::now() + Duration::from_secs(5),
            stdout_bytes: 65536,
            stderr_bytes: 65536,
        }
    }
    fn occupied() -> usize {
        sweep_retired();
        OPERATIONS
            .iter()
            .filter(|slot| !slot.load(Ordering::Acquire).is_null())
            .count()
    }
    fn handles() -> u32 {
        let mut count = 0;
        // SAFETY: pseudo handle is used only for querying this process, not owned.
        assert_ne!(
            unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
            0
        );
        count
    }
    fn primary(error: &CaptureError) -> &CaptureError {
        match error {
            CaptureError::Cleanup { primary: error, .. } => primary(error),
            error => error,
        }
    }

    /// The directory the fixture process tree records PIDs and handshakes in.
    fn fixture_directory() -> PathBuf {
        PathBuf::from(std::env::var_os("BACKEND_CAPTURE_DIR").expect("fixture directory"))
    }

    /// Records this process's PID as `role`, for the supervising test to pin.
    fn record_pid(role: &str) {
        fs::write(
            fixture_directory().join(format!("{role}.pid")),
            std::process::id().to_string(),
        )
        .expect("record the PID");
    }

    /// Starts a fixture process in `mode` that inherits this one's standard handles.
    fn spawn_fixture(mode: &str) {
        Command::new(std::env::current_exe().expect("fixture executable"))
            .args(["--exact", FIXTURE, "--nocapture"])
            .env("BACKEND_CAPTURE_FIXTURE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start a fixture descendant");
    }

    /// Waits (bounded) for the supervising test to create `name` in the fixture directory.
    fn wait_for_file(name: &str) {
        let path = fixture_directory().join(name);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.exists() {
            assert!(Instant::now() < deadline, "{name} never appeared");
            thread::sleep(POLL);
        }
    }

    // Runs only in a spawned copy of this test executable. It uses the same
    // native process, argument, environment and standard-handle path as Cargo.
    // No installed shell, Python interpreter, or external fixture is required.
    #[test]
    fn capture_fixture() {
        let Ok(mode) = std::env::var("BACKEND_CAPTURE_FIXTURE") else {
            return;
        };
        match mode.as_str() {
            "dual" => {
                assert_eq!(io::stdin().read(&mut [0]).expect("NUL stdin"), 0);
                io::stdout().write_all(b"bounded stdout\n").expect("stdout");
                io::stderr().write_all(b"bounded stderr\n").expect("stderr");
            }
            "large-out" => {
                io::stdout().write_all(&[b'x'; CHUNK * 4]).expect("stdout");
            }
            "large-err" => {
                io::stderr().write_all(&[b'y'; CHUNK * 4]).expect("stderr");
            }
            "leak-audit" => {
                // The process-wide handle count is only meaningful when no other
                // thread opens or closes handles, so the accounting runs here, in
                // a process of its own, instead of beside the parallel tests.
                let cancelled = AtomicBool::new(false);
                capture(&command("dual"), limits(), &cancelled).expect("warm-up capture");
                assert_eq!(occupied(), 0);
                let before = handles();
                for _ in 0..8 {
                    capture(&command("dual"), limits(), &cancelled).expect("repeat capture");
                }
                assert_eq!(occupied(), 0);
                assert_eq!(
                    handles(),
                    before,
                    "capture must return all process, pipe, Job and IOCP handles"
                );
            }
            "sleep" => {
                if let Some(path) = std::env::var_os("BACKEND_CAPTURE_PID") {
                    fs::write(path, std::process::id().to_string()).expect("PID handshake");
                }
                thread::sleep(Duration::from_secs(30));
            }
            "descendant" => {
                let child = Command::new(std::env::current_exe().expect("fixture executable"))
                    .args(["--exact", FIXTURE, "--nocapture"])
                    .env("BACKEND_CAPTURE_FIXTURE", "sleep")
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .expect("descendant");
                if let Some(path) = std::env::var_os("BACKEND_CAPTURE_DESCENDANT") {
                    fs::write(path, child.id().to_string()).expect("descendant PID handshake");
                }
                // Leave an observation window for the parent test to acquire a
                // process handle; then exit with the descendant holding writers.
                thread::sleep(Duration::from_millis(300));
            }
            "tree" => {
                // The leader of a three-level tree: it records its PID, starts a child that
                // starts a grandchild, and then waits, holding the pipes the whole tree inherits.
                record_pid("leader");
                spawn_fixture("tree-child");
                thread::sleep(Duration::from_secs(30));
            }
            "tree-child" => {
                record_pid("child");
                spawn_fixture("sleep-recorded");
                thread::sleep(Duration::from_secs(30));
            }
            "sleep-recorded" => {
                record_pid("grandchild");
                thread::sleep(Duration::from_secs(30));
            }
            "flood" => {
                // Starts a descendant that outlives any output, then writes until the pipe closes.
                record_pid("leader");
                spawn_fixture("sleep-recorded");
                wait_for_file("grandchild.pid");
                wait_for_file("go");
                let mut stdout = io::stdout();
                while stdout.write_all(&[b'z'; 65536]).is_ok() {}
            }
            "crash" => {
                // Partial output, a descendant holding the pipes, then death without cleanup.
                io::stdout().write_all(b"partial output\n").expect("stdout");
                io::stdout().flush().expect("flush");
                spawn_fixture("sleep-recorded");
                wait_for_file("grandchild.pid");
                wait_for_file("go");
                std::process::exit(7);
            }
            "breakaway" => {
                // A descendant that tries to leave the Job, as a daemonizing tool would.
                use std::os::windows::process::CommandExt as _;
                const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
                record_pid("leader");
                let outcome = Command::new(std::env::current_exe().expect("fixture executable"))
                    .args(["--exact", FIXTURE, "--nocapture"])
                    .env("BACKEND_CAPTURE_FIXTURE", "sleep-recorded")
                    .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn();
                let verdict = match outcome {
                    Ok(_) => "spawned".to_owned(),
                    Err(error) => format!("refused:{}", error.raw_os_error().unwrap_or(-1)),
                };
                let directory = std::env::var_os("BACKEND_CAPTURE_DIR").expect("fixture directory");
                fs::write(PathBuf::from(directory).join("breakaway"), verdict).expect("verdict");
                thread::sleep(Duration::from_secs(30));
            }
            _ => panic!("unknown fixture mode"),
        }
    }

    #[test]
    fn native_capture_drains_both_streams_and_closes_handles() {
        let _serial = TESTS.lock().expect("test lock");
        let cancelled = AtomicBool::new(false);
        let output = capture(&command("dual"), limits(), &cancelled).expect("native capture");
        assert!(output.status.success());
        assert!(
            output
                .stdout
                .windows(b"bounded stdout".len())
                .any(|window| window == b"bounded stdout")
        );
        assert!(
            output
                .stderr
                .windows(b"bounded stderr".len())
                .any(|window| window == b"bounded stderr")
        );
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn repeated_capture_returns_every_process_pipe_job_and_iocp_handle() {
        let _serial = TESTS.lock().expect("test lock");
        let audit = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", FIXTURE, "--nocapture"])
            .env("BACKEND_CAPTURE_FIXTURE", "leak-audit")
            .stdin(Stdio::null())
            .output()
            .expect("run the isolated handle audit");
        assert!(
            audit.status.success(),
            "isolated handle audit failed:\n{}\n{}",
            String::from_utf8_lossy(&audit.stdout),
            String::from_utf8_lossy(&audit.stderr)
        );
        assert!(
            String::from_utf8_lossy(&audit.stdout).contains("capture_fixture ... ok"),
            "the audit fixture did not run: the filter matched no test"
        );
    }

    #[test]
    fn each_stream_enforces_its_own_ceiling_and_partial_start_releases_slots() {
        let _serial = TESTS.lock().expect("test lock");
        let cancelled = AtomicBool::new(false);
        for (mode, stream) in [
            ("large-out", OutputStream::Stdout),
            ("large-err", OutputStream::Stderr),
        ] {
            let mut bounds = limits();
            bounds.stdout_bytes = 1024;
            bounds.stderr_bytes = 1024;
            let error = capture(&command(mode), bounds, &cancelled).expect_err("stream ceiling");
            assert!(
                matches!(primary(&error), CaptureError::OutputLimit { stream: actual, observed, maximum }
                if *actual == stream && observed > maximum)
            );
            assert_eq!(
                occupied(),
                0,
                "completion releases the reserved operation slots"
            );
        }
        let mut missing = command("dual");
        missing.program = std::env::temp_dir()
            .join("backend-nonexistent-capture-program.exe")
            .into_os_string();
        let error = capture(&missing, limits(), &cancelled).expect_err("creation failure");
        assert!(matches!(primary(&error), CaptureError::Spawn(_)));
        assert_eq!(
            occupied(),
            0,
            "partial startup must not occupy retirement slots"
        );
    }

    #[test]
    fn deadline_and_cancellation_bound_pending_pipe_reads() {
        let _serial = TESTS.lock().expect("test lock");
        let cancelled = AtomicBool::new(false);
        let mut bounds = limits();
        bounds.deadline = Instant::now() + Duration::from_millis(50);
        let start = Instant::now();
        let error = capture(&command("sleep"), bounds, &cancelled).expect_err("deadline");
        assert!(matches!(primary(&error), CaptureError::Deadline));
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_eq!(occupied(), 0);
        thread::scope(|scope| {
            let cancel = &cancelled;
            scope.spawn(move || {
                thread::sleep(Duration::from_millis(50));
                cancel.store(true, Ordering::Release);
            });
            let start = Instant::now();
            let error = capture(&command("sleep"), limits(), &cancelled).expect_err("cancellation");
            assert!(matches!(primary(&error), CaptureError::Cancelled));
            assert!(start.elapsed() < Duration::from_secs(2));
        });
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn leader_exit_retires_descendant_inherited_writers() {
        let _serial = TESTS.lock().expect("test lock");
        let path = std::env::temp_dir().join(format!(
            "backend-capture-descendant-{}.pid",
            std::process::id()
        ));
        let _ = fs::remove_file(&path);
        let mut spec = command("descendant");
        spec.overrides.push((
            "BACKEND_CAPTURE_DESCENDANT".into(),
            Some(path.clone().into_os_string()),
        ));
        thread::scope(|scope| {
            let worker = scope.spawn(|| capture(&spec, limits(), &AtomicBool::new(false)));
            let deadline = Instant::now() + Duration::from_secs(3);
            let descendant = loop {
                if let Ok(contents) = fs::read_to_string(&path)
                    && let Ok(pid) = contents.parse::<u32>()
                {
                    // SAFETY: process handle pins the identity, preventing PID
                    // reuse from turning a later retirement check into a guess.
                    break Handle::new(unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) })
                        .expect("live descendant handle");
                }
                assert!(Instant::now() < deadline, "descendant handshake");
                thread::sleep(POLL);
            };
            let result = worker
                .join()
                .expect("capture worker")
                .expect("leader completion");
            assert!(result.status.success());
            // SAFETY: bounded wait on the witnessed descendant identity.
            assert_eq!(
                unsafe { WaitForSingleObject(descendant.0, 1000) },
                WAIT_OBJECT_0
            );
        });
        assert_eq!(occupied(), 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn fixed_operation_capacity_rejects_before_start_and_releases_permits() {
        let _serial = TESTS.lock().expect("test lock");
        assert_eq!(occupied(), 0);
        let permits: Vec<_> = (0..MAX_OPERATIONS)
            .map(|_| Slot::reserve().expect("reserve slot"))
            .collect();
        assert!(matches!(
            Slot::reserve(),
            Err(CaptureError::Capacity {
                maximum: MAX_OPERATIONS
            })
        ));
        let error = capture(&command("dual"), limits(), &AtomicBool::new(false))
            .expect_err("before-start capacity");
        assert!(matches!(
            error,
            CaptureError::Capacity {
                maximum: MAX_OPERATIONS
            }
        ));
        drop(permits);
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn pending_requests_transfer_to_fixed_slots_until_iocp_completion() {
        let _serial = TESTS.lock().expect("test lock");
        assert_eq!(occupied(), 0);
        let slot = Slot::reserve().expect("operation slot");
        let (mut pipe, writer) = Pipe::pair(slot).expect("owned pipe");
        pipe.poll(&mut Vec::new(), 1024, OutputStream::Stdout)
            .expect("start overlapped read");
        assert!(pipe.pending, "the connected writer supplied no bytes");
        drop(pipe); // close/cancel, then transfer stable request ownership
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(2);
        while occupied() != 0 {
            assert!(
                Instant::now() < deadline,
                "cancelled request never delivered its IOCP ownership receipt"
            );
            thread::sleep(POLL);
        }
        let permits: Vec<_> = (0..MAX_OPERATIONS)
            .map(|_| Slot::reserve().expect("reclaimed slot"))
            .collect();
        drop(permits);
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn successive_live_callers_do_not_throttle_a_later_completion_owner() {
        let _serial = TESTS.lock().expect("test lock");
        let (mut pipe, writer) = Pipe::pair(Slot::reserve().expect("slot")).expect("owned pipe");
        pipe.poll(&mut Vec::new(), 1024, OutputStream::Stdout)
            .expect("pending read");
        assert!(pipe.pending);
        // Only raw addresses cross threads. Stable UnsafeCell storage stays
        // owned by this pinned Pipe until the final matching packet arrives.
        let port = pipe.port.as_ref().expect("port").0 as usize;
        let request = pipe.operation.as_ref().expect("operation").request() as usize;
        let stop = AtomicBool::new(false);
        struct Stop<'a>(&'a AtomicBool);
        impl Drop for Stop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        thread::scope(|scope| {
            let _release_on_unwind = Stop(&stop);
            let (done, ready) = std::sync::mpsc::channel();
            for _ in 0..8 {
                let done = done.clone();
                let stop = &stop;
                scope.spawn(move || {
                    let result = completion(port as HANDLE, request as *mut OVERLAPPED);
                    done.send(matches!(result, Ok(None)))
                        .expect("report empty poll");
                    // Remain runnable; a blocking wait would hide an IOCP
                    // association leak by lowering its active-thread count.
                    while !stop.load(Ordering::Acquire) {
                        std::hint::spin_loop();
                    }
                });
                assert!(
                    ready
                        .recv_timeout(Duration::from_secs(2))
                        .expect("poll must not wait")
                );
            }
            let mut written = 0;
            // SAFETY: owned synchronous writer, valid three-byte input buffer.
            assert_ne!(
                unsafe {
                    windows_sys::Win32::Storage::FileSystem::WriteFile(
                        writer.0,
                        b"end".as_ptr(),
                        3,
                        &mut written,
                        ptr::null_mut(),
                    )
                },
                0
            );
            let receipt = scope
                .spawn(move || {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        match completion(port as HANDLE, request as *mut OVERLAPPED) {
                            Ok(Some(count)) => return count,
                            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
                            other => panic!("later caller could not dequeue: {other:?}"),
                        }
                    }
                })
                .join()
                .expect("later completion owner");
            assert_eq!(receipt, 3);
            pipe.pending = false; // the matching IOCP packet is the proof
        });
        drop(pipe);
        drop(writer);
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn ordinal_failure_and_length_overflow_never_mean_equal() {
        assert_eq!(
            ordinal_result(CSTR_EQUAL, 0).expect("equal"),
            CmpOrdering::Equal
        );
        assert_eq!(
            ordinal_result(0, 87)
                .expect_err("native failure")
                .raw_os_error(),
            Some(87)
        );
        assert_eq!(
            ordinal_result(99, 0).expect_err("invalid result").kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            ordinal_lengths(i32::MAX as usize + 1, 1)
                .expect_err("left overflow")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            ordinal_lengths(1, i32::MAX as usize + 1)
                .expect_err("right overflow")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            ordinal_lengths(i32::MAX as usize, 1).expect("checked boundary"),
            (i32::MAX, 1)
        );
        let a: Vec<_> = "Path".encode_utf16().collect();
        let b: Vec<_> = "PATH".encode_utf16().collect();
        assert_eq!(
            compare_names(&a, &b).expect("native case table"),
            CmpOrdering::Equal
        );
    }

    #[test]
    fn command_and_environment_preserve_utf16_and_explicit_clear() {
        let _serial = TESTS.lock().expect("test lock");
        let surrogate = OsString::from_wide(&[0xd800, b' ' as u16, b'"' as u16, b'\\' as u16]);
        let line = command_line(OsStr::new("cargo.exe"), &[surrogate]).expect("UTF16 quoting");
        let expected = [
            b'"' as u16,
            0xd800,
            b' ' as u16,
            b'\\' as u16,
            b'"' as u16,
            b'\\' as u16,
            b'\\' as u16,
            b'"' as u16,
            0,
        ];
        assert!(line.ends_with(&expected));
        let mut spec = command("dual");
        spec.environment = CaptureEnvironment::Clear;
        spec.overrides.clear();
        assert_eq!(
            environment_block(&spec).expect("empty environment"),
            vec![0, 0]
        );
        spec.overrides = vec![
            ("Path".into(), Some("first".into())),
            ("PATH".into(), Some("second".into())),
        ];
        let expected: Vec<_> = "PATH=second\0\0".encode_utf16().collect();
        assert_eq!(
            environment_block(&spec).expect("case insensitive environment"),
            expected
        );
        spec.overrides.push(("path".into(), None));
        assert_eq!(
            environment_block(&spec).expect("remove variable"),
            vec![0, 0]
        );
        assert!(wide(&OsString::from_wide(&[0])).is_err());
    }

    // ---- process-tree supervision --------------------------------------------------------

    /// The directory a fixture process tree records its PIDs in, and which test owns it.
    struct TreeDirectory(PathBuf);

    impl TreeDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "backend-capture-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock after epoch")
                    .as_nanos()
            ));
            fs::create_dir(&path).expect("tree directory");
            Self(path)
        }

        fn command(&self, mode: &str) -> CaptureCommand {
            let mut spec = command(mode);
            spec.overrides.push((
                "BACKEND_CAPTURE_DIR".into(),
                Some(self.0.clone().into_os_string()),
            ));
            spec
        }

        /// Lets a fixture that waits for the `go` file continue.
        fn release(&self) {
            fs::write(self.0.join("go"), b"go").expect("release the fixture");
        }

        /// Waits for process `role` to record its PID, then pins that exact process with a handle
        /// so a later liveness check cannot be fooled by PID reuse.
        fn witness(&self, role: &str) -> Witness {
            let path = self.0.join(format!("{role}.pid"));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(contents) = fs::read_to_string(&path)
                    && let Ok(pid) = contents.parse::<u32>()
                {
                    // SAFETY: opens a process by PID for waiting and termination only.
                    let handle =
                        unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, 0, pid) };
                    return Witness {
                        role: role.to_owned(),
                        handle: Handle::new(handle).expect("a live process handle"),
                    };
                }
                assert!(Instant::now() < deadline, "{role} never recorded its PID");
                thread::sleep(POLL);
            }
        }
    }

    impl Drop for TreeDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// One pinned process of a fixture tree.
    struct Witness {
        role: String,
        handle: Handle,
    }

    impl Witness {
        /// Whether the process has already exited.
        fn is_dead(&self) -> bool {
            // SAFETY: a zero-timeout wait on an owned process handle.
            unsafe { WaitForSingleObject(self.handle.0, 0) == WAIT_OBJECT_0 }
        }

        /// Waits (bounded) for the process to be gone, as capture must have made it.
        fn assert_dead(&self) {
            // SAFETY: bounded wait on an owned process handle.
            let waited = unsafe { WaitForSingleObject(self.handle.0, 2000) };
            assert_eq!(waited, WAIT_OBJECT_0, "{} survived the capture", self.role);
        }

        /// Ends the process abruptly from outside, with `code`.
        fn terminate(&self, code: u32) {
            // SAFETY: terminating an owned, pinned process handle.
            assert_ne!(unsafe { TerminateProcess(self.handle.0, code) }, 0);
        }
    }

    #[test]
    fn cancellation_ends_the_leader_and_every_descendant() {
        let _serial = TESTS.lock().expect("test lock");
        let tree = TreeDirectory::new("cancel");
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let capturing = scope.spawn(|| capture(&tree.command("tree"), limits(), &cancelled));
            let tree_processes = ["leader", "child", "grandchild"].map(|role| tree.witness(role));
            assert!(tree_processes.iter().all(|process| !process.is_dead()));
            let started = Instant::now();
            cancelled.store(true, Ordering::Release);
            let error = capturing
                .join()
                .expect("capture worker")
                .expect_err("cancelled");
            assert!(matches!(primary(&error), CaptureError::Cancelled));
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "cancellation is prompt"
            );
            for process in &tree_processes {
                process.assert_dead();
            }
        });
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn the_deadline_ends_the_leader_and_every_descendant() {
        let _serial = TESTS.lock().expect("test lock");
        let tree = TreeDirectory::new("deadline");
        let mut bounds = limits();
        bounds.deadline = Instant::now() + Duration::from_secs(4);
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let capturing = scope.spawn(|| capture(&tree.command("tree"), bounds, &cancelled));
            let tree_processes = ["leader", "child", "grandchild"].map(|role| tree.witness(role));
            let error = capturing
                .join()
                .expect("capture worker")
                .expect_err("deadline");
            assert!(matches!(primary(&error), CaptureError::Deadline));
            for process in &tree_processes {
                process.assert_dead();
            }
        });
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn exceeding_the_output_limit_ends_the_tree_even_while_the_leader_floods() {
        let _serial = TESTS.lock().expect("test lock");
        let tree = TreeDirectory::new("flood");
        let mut bounds = limits();
        bounds.stdout_bytes = 4096;
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let capturing = scope.spawn(|| capture(&tree.command("flood"), bounds, &cancelled));
            // Pin both processes while they are alive, then let the leader start flooding.
            let tree_processes = ["leader", "grandchild"].map(|role| tree.witness(role));
            let started = Instant::now();
            tree.release();
            let error = capturing
                .join()
                .expect("capture worker")
                .expect_err("flood");
            assert!(matches!(
                primary(&error),
                CaptureError::OutputLimit {
                    stream: OutputStream::Stdout,
                    ..
                }
            ));
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "the flood is cut off promptly"
            );
            for process in &tree_processes {
                process.assert_dead();
            }
        });
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn a_leader_killed_from_outside_reports_its_status_and_takes_the_tree_with_it() {
        let _serial = TESTS.lock().expect("test lock");
        let tree = TreeDirectory::new("killed");
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let capturing = scope.spawn(|| capture(&tree.command("tree"), limits(), &cancelled));
            let tree_processes = ["leader", "child", "grandchild"].map(|role| tree.witness(role));
            tree_processes[0].terminate(77);
            let output = capturing
                .join()
                .expect("capture worker")
                .expect("an externally killed leader is still a complete transaction");
            assert_eq!(output.status.code(), Some(77));
            for process in &tree_processes {
                process.assert_dead();
            }
        });
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn a_crashing_leader_keeps_its_partial_output_and_leaves_no_descendant() {
        let _serial = TESTS.lock().expect("test lock");
        let tree = TreeDirectory::new("crash");
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let capturing = scope.spawn(|| capture(&tree.command("crash"), limits(), &cancelled));
            let grandchild = tree.witness("grandchild");
            tree.release();
            let output = capturing
                .join()
                .expect("capture worker")
                .expect("a crash is a complete transaction");
            assert_eq!(output.status.code(), Some(7));
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("partial output"),
                "the output written before the crash is kept"
            );
            grandchild.assert_dead();
        });
        assert_eq!(occupied(), 0);
    }

    #[test]
    fn a_descendant_cannot_break_away_from_the_job() {
        let _serial = TESTS.lock().expect("test lock");
        let tree = TreeDirectory::new("breakaway");
        let cancelled = AtomicBool::new(false);
        thread::scope(|scope| {
            let capturing =
                scope.spawn(|| capture(&tree.command("breakaway"), limits(), &cancelled));
            let leader = tree.witness("leader");
            let verdict_path = tree.0.join("breakaway");
            let deadline = Instant::now() + Duration::from_secs(5);
            let verdict = loop {
                if let Ok(verdict) = fs::read_to_string(&verdict_path)
                    && !verdict.is_empty()
                {
                    break verdict;
                }
                assert!(
                    Instant::now() < deadline,
                    "the breakaway attempt never reported"
                );
                thread::sleep(POLL);
            };
            eprintln!("breakaway attempt: {verdict}");
            // Either the kernel refuses a breakaway from a Job that does not allow it, or the
            // process it started is inside the Job; in neither case may anything outlive capture.
            let escaped = (verdict == "spawned").then(|| tree.witness("grandchild"));
            cancelled.store(true, Ordering::Release);
            let error = capturing
                .join()
                .expect("capture worker")
                .expect_err("cancelled");
            assert!(matches!(primary(&error), CaptureError::Cancelled));
            leader.assert_dead();
            if let Some(escaped) = escaped {
                escaped.assert_dead();
            } else {
                assert!(verdict.starts_with("refused:"), "{verdict}");
            }
        });
        assert_eq!(occupied(), 0);
    }
}
