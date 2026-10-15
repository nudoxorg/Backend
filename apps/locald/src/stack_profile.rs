//! LOCAL PROFILING AID (never committed). `TSZ_STACK_PROFILE=<path>` starts a SIGPROF CPU-time
//! sampler (every 5 ms of process CPU, any thread). Stacks are written to `<path>.cpu` every 10 s
//! as `T <count> 0 <offset> <offset> ...` (offsets relative to the executable's load base).
#![allow(unsafe_code, clippy::all, clippy::pedantic, clippy::restriction)]
use std::cell::{Cell, UnsafeCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const RING: usize = 16384;
const DEPTH: usize = 64;

struct Ring(UnsafeCell<[[usize; DEPTH + 1]; RING]>);
unsafe impl Sync for Ring {}
static RING_BUF: Ring = Ring(UnsafeCell::new([[0; DEPTH + 1]; RING]));
static HEAD: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct Timeval { tv_sec: i64, tv_usec: i64 }
#[repr(C)]
struct Itimerval { interval: Timeval, value: Timeval }
unsafe extern "C" {
    fn setitimer(which: i32, new_value: *const Itimerval, old_value: *mut Itimerval) -> i32;
    fn signal(signum: i32, handler: usize) -> usize;
    fn _Unwind_Backtrace(
        trace: extern "C" fn(*mut std::ffi::c_void, *mut std::ffi::c_void) -> i32,
        arg: *mut std::ffi::c_void,
    ) -> i32;
    fn _Unwind_GetIP(ctx: *mut std::ffi::c_void) -> usize;
}

struct FrameSink<'a> { slot: &'a mut [usize; DEPTH + 1], len: usize }
extern "C" fn sink_cb(ctx: *mut std::ffi::c_void, arg: *mut std::ffi::c_void) -> i32 {
    let sink = unsafe { &mut *(arg as *mut FrameSink<'_>) };
    let ip = unsafe { _Unwind_GetIP(ctx) };
    if sink.len < DEPTH {
        sink.slot[1 + sink.len] = ip;
        sink.len += 1;
        0
    } else {
        5
    }
}

extern "C" fn on_sigprof(_signal: i32) {
    thread_local! { static IN_HANDLER: Cell<bool> = const { Cell::new(false) }; }
    if IN_HANDLER.try_with(|f| f.replace(true)).unwrap_or(true) {
        return;
    }
    let index = HEAD.fetch_add(1, Ordering::Relaxed) % RING;
    let slot = unsafe { &mut (*RING_BUF.0.get())[index] };
    slot[0] = 0;
    let mut sink = FrameSink { slot, len: 0 };
    unsafe { _Unwind_Backtrace(sink_cb, &mut sink as *mut FrameSink<'_> as *mut _) };
    let len = sink.len;
    sink.slot[0] = len;
    let _ = IN_HANDLER.try_with(|f| f.set(false));
}

fn exe_base() -> usize {
    let Some(exe) = std::fs::read_link("/proc/self/exe").ok() else { return 0 };
    let exe = exe.to_string_lossy().to_string();
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    for line in maps.lines() {
        if line.ends_with(&exe) {
            if let Some(range) = line.split_whitespace().next() {
                if let Some((start, _)) = range.split_once('-') {
                    return usize::from_str_radix(start, 16).unwrap_or(0);
                }
            }
        }
    }
    0
}

pub fn start() {
    let Ok(path) = std::env::var("TSZ_STACK_PROFILE") else { return };
    if path.is_empty() { return; }
    let out = format!("{path}.cpu.{}", std::process::id());
    std::thread::Builder::new()
        .name("cpu-profile-drain".into())
        .spawn(move || {
            let mut agg: HashMap<Vec<usize>, u64> = HashMap::new();
            let mut tail = 0usize;
            let mut last_write = Instant::now();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let head = HEAD.load(Ordering::Acquire);
                let from = if head.saturating_sub(tail) > RING { head - RING } else { tail };
                for n in from..head {
                    let slot = unsafe { &(*RING_BUF.0.get())[n % RING] };
                    let len = slot[0].min(DEPTH);
                    if len > 0 {
                        *agg.entry(slot[1..=len].to_vec()).or_default() += 1;
                    }
                }
                tail = head;
                if last_write.elapsed().as_secs() >= 20 {
                    last_write = Instant::now();
                    let base = exe_base();
                    let mut text = format!("BASE {base:x}\n");
                    for (stack, count) in &agg {
                        text.push_str(&format!("T {count} 0"));
                        for ip in stack {
                            text.push_str(&format!(" {:x}", ip.wrapping_sub(base)));
                        }
                        text.push('\n');
                    }
                    let _ = std::fs::write(&out, text);
                }
            }
        })
        .ok();
    let spec = Itimerval {
        interval: Timeval { tv_sec: 0, tv_usec: 5000 },
        value: Timeval { tv_sec: 0, tv_usec: 5000 },
    };
    unsafe {
        signal(27, on_sigprof as extern "C" fn(i32) as usize);
        setitimer(2, &spec, std::ptr::null_mut());
    }
}

unsafe extern "C" {
    fn dup2(oldfd: i32, newfd: i32) -> i32;
}

/// Redirects stderr (normally /dev/null for the owner) to `<path>.stderr.<pid>` so panics,
/// stack-overflow messages, and aborts are visible.
pub fn capture_stderr() {
    use std::os::fd::AsRawFd;
    let Ok(path) = std::env::var("TSZ_STACK_PROFILE") else { return };
    if path.is_empty() { return; }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{path}.stderr.{}", std::process::id()))
    {
        unsafe { dup2(file.as_raw_fd(), 2) };
        std::mem::forget(file);
    }
}
