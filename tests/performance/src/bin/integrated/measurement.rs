use super::*;

pub(super) struct ResourceSampler {
    stop: Arc<AtomicBool>,
    peak_rss_bytes: Arc<AtomicU64>,
    cpu_time_ns: Arc<AtomicU64>,
    peak_fd_count: Arc<AtomicU64>,
    handle: Option<thread::JoinHandle<()>>,
}

impl ResourceSampler {
    pub(super) fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak_rss_bytes = Arc::new(AtomicU64::new(0));
        let cpu_time_ns = Arc::new(AtomicU64::new(0));
        let peak_fd_count = Arc::new(AtomicU64::new(0));
        let thread_stop = Arc::clone(&stop);
        let thread_peak_rss = Arc::clone(&peak_rss_bytes);
        let thread_cpu = Arc::clone(&cpu_time_ns);
        let thread_peak_fds = Arc::clone(&peak_fd_count);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::Relaxed) {
                if let Some((rss, cpu, fds)) = process_resources() {
                    thread_peak_rss.fetch_max(rss, Ordering::Relaxed);
                    thread_cpu.fetch_max(cpu, Ordering::Relaxed);
                    thread_peak_fds.fetch_max(fds, Ordering::Relaxed);
                }
                thread::sleep(Duration::from_millis(25));
            }
        });
        Self {
            stop,
            peak_rss_bytes,
            cpu_time_ns,
            peak_fd_count,
            handle: Some(handle),
        }
    }

    pub(super) fn finish(mut self) -> (Option<u64>, Option<u128>, Option<u64>) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        let rss = self.peak_rss_bytes.load(Ordering::Relaxed);
        let cpu = self.cpu_time_ns.load(Ordering::Relaxed);
        let fds = self.peak_fd_count.load(Ordering::Relaxed);
        (
            Some(rss).filter(|value| *value != 0),
            Some(u128::from(cpu)).filter(|value| *value != 0),
            Some(fds).filter(|value| *value != 0),
        )
    }
}

fn process_resources() -> Option<(u64, u64, u64)> {
    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
        let pid = std::process::id().to_string();
        let output = Command::new("ps")
            .args(["-p", &pid, "-o", "rss=,time="])
            .output()
            .ok()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut fields = stdout.split_whitespace();
        let rss_kib = fields.next()?.parse::<u64>().ok()?;
        let cpu = fields.next().and_then(parse_cpu_time_ns).unwrap_or(0);
        let fd_root = if cfg!(target_os = "macos") {
            "/dev/fd"
        } else {
            "/proc/self/fd"
        };
        let fds = fs::read_dir(fd_root)
            .ok()
            .map(|entries| entries.flatten().count() as u64)
            .unwrap_or(0);
        Some((rss_kib.saturating_mul(1024), cpu, fds))
    } else {
        None
    }
}

fn parse_cpu_time_ns(value: &str) -> Option<u64> {
    let (minutes, seconds) = value.split_once(':')?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
    let minutes = minutes.parse::<u64>().ok()?;
    let seconds = seconds.parse::<u64>().ok()?;
    let mut fraction = fraction
        .bytes()
        .take(9)
        .filter(|byte| byte.is_ascii_digit())
        .collect::<Vec<_>>();
    if fraction.is_empty() {
        fraction.push(b'0');
    }
    while fraction.len() < 9 {
        fraction.push(b'0');
    }
    let fraction = std::str::from_utf8(&fraction).ok()?.parse::<u64>().ok()?;
    Some(
        (minutes.saturating_mul(60).saturating_add(seconds))
            .saturating_mul(1_000_000_000)
            .saturating_add(fraction),
    )
}
