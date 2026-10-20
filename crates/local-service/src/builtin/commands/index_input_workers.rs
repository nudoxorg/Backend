//! Owned scan/acquisition threads survive owner phase changes until kernel exit.
//!
//! One index job can acquire and then scan. Two retained handles cover those
//! stages; another launch refuses capacity until finished tails are reaped.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

struct Worker {
    cancelled: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

const MAX_WORKERS: usize = 2;

pub(super) struct IndexInputWorkers {
    workers: Vec<Worker>,
    closed: bool,
}

impl Default for IndexInputWorkers {
    fn default() -> Self {
        Self {
            workers: Vec::with_capacity(MAX_WORKERS),
            closed: false,
        }
    }
}

impl IndexInputWorkers {
    pub(super) fn reserve(&mut self) -> Result<(), &'static str> {
        if self.closed {
            return Err("index input owner is closed");
        }
        self.reap_finished();
        if self.workers.len() >= MAX_WORKERS {
            return Err("index input workers are still retiring; retry");
        }
        Ok(())
    }

    pub(super) fn retain(&mut self, handle: JoinHandle<()>, cancelled: Arc<AtomicBool>) {
        // The owner is single-threaded and reserves before spawning, so no
        // other launch can consume the slot between reserve and transfer.
        assert!(
            self.workers.len() < MAX_WORKERS && !self.closed,
            "reserved index input worker capacity"
        );
        self.workers.push(Worker { cancelled, handle });
    }

    pub(super) fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].handle.is_finished() {
                let worker = self.workers.swap_remove(index);
                join(worker);
            } else {
                index += 1;
            }
        }
    }

    /// Completed handles alone are not activity; a retained live tail is.
    pub(super) fn has_inflight_work(&self) -> bool {
        self.workers
            .iter()
            .any(|worker| !worker.handle.is_finished())
    }

    pub(super) fn request_close(&mut self) {
        self.closed = true;
        for worker in &self.workers {
            worker.cancelled.store(true, Ordering::Release);
        }
    }

    /// The adapter first drops acquisition/scan receivers, unblocking bounded
    /// terminal sends. No owner or worker mutex is held across these joins.
    pub(super) fn close(&mut self) {
        self.request_close();
        for worker in self.workers.drain(..) {
            join(worker);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.workers.len()
    }
}

impl Drop for IndexInputWorkers {
    fn drop(&mut self) {
        self.close();
    }
}

fn join(worker: Worker) {
    if worker.handle.join().is_err() {
        // A disconnected result is already a typed failure to the operation
        // consumer. Reaping must not invent a success or panic during Drop.
        eprintln!("locald index input worker panicked during retirement");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_input_threads_bound_launches_and_are_joined_after_broadcast() {
        let mut workers = IndexInputWorkers::default();
        assert!(!workers.has_inflight_work());
        let mut controls = Vec::new();
        let mut releases = Vec::new();
        let exited = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for _ in 0..2 {
            workers.reserve().expect("bounded input slot");
            let control = Arc::new(AtomicBool::new(false));
            let stopped = Arc::clone(&control);
            let exited = Arc::clone(&exited);
            let (release, gate) = std::sync::mpsc::sync_channel(1);
            let handle = std::thread::spawn(move || {
                let _ = gate.recv();
                assert!(stopped.load(Ordering::Acquire));
                exited.fetch_add(1, Ordering::Release);
            });
            workers.retain(handle, Arc::clone(&control));
            controls.push(control);
            releases.push(release);
        }
        assert!(
            workers.has_inflight_work(),
            "actual gated input threads are live"
        );
        assert!(workers.reserve().is_err(), "no detached overflow thread");
        assert!(
            controls
                .iter()
                .all(|control| !control.load(Ordering::Acquire))
        );
        workers.request_close();
        assert!(
            controls
                .iter()
                .all(|control| control.load(Ordering::Acquire))
        );
        assert_eq!(
            exited.load(Ordering::Acquire),
            0,
            "signalling performs no join"
        );
        for release in releases {
            let _ = release.send(());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while workers.has_inflight_work() && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            !workers.has_inflight_work(),
            "finished retained handles are not activity"
        );
        assert_eq!(workers.len(), 2, "the idle witness does not reap or join");
        workers.close();
        assert_eq!(exited.load(Ordering::Acquire), 2);
        assert_eq!(workers.len(), 0);
        assert!(
            !workers.has_inflight_work(),
            "joined work no longer keeps an owner live"
        );
        workers.close();
        assert!(
            workers.reserve().is_err(),
            "a closed owner cannot admit input work"
        );
    }
}
