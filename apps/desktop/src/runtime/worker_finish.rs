//! Send-only worker ownership after irreversible close admission.
//! The UI keeps one tracked finish task until every owned worker has returned.

#[derive(Default)]
pub(crate) struct WorkerFinish(Vec<std::thread::JoinHandle<()>>);
impl WorkerFinish {
    pub(crate) fn push(&mut self, worker: Option<std::thread::JoinHandle<()>>) {
        if let Some(worker) = worker {
            self.0.push(worker);
        }
    }
    pub(crate) fn extend(&mut self, other: Self) {
        self.0.extend(other.0);
    }
    pub(crate) fn from_workers(workers: Vec<std::thread::JoinHandle<()>>) -> Self {
        Self(workers)
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub(crate) async fn wait(self, executor: gpui::BackgroundExecutor) {
        // Poll completion without blocking an executor thread on an arbitrary
        // reader. Join only after the OS reports that worker has returned.
        while self.0.iter().any(|worker| !worker.is_finished()) {
            executor.timer(std::time::Duration::from_millis(25)).await;
        }
        for worker in self.0 {
            if worker.join().is_err() {
                eprintln!("backend-desktop: a closing worker stopped unexpectedly");
            }
        }
    }
}
