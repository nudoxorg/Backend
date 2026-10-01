//! Old-oracle parity fixture for Rust declarations and documentation.

/// Computes a value through a typed service.
pub trait Service<T> {
    type Output;
    fn run(&self, value: T) -> Self::Output;
}

pub struct Worker;
pub enum Event { Started, Finished }
pub fn execute(worker: &impl Service<String>) -> String { worker.run(String::new()) }
