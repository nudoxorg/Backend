use std::{
    hint::black_box,
    sync::{Arc, Barrier, atomic::{AtomicUsize, Ordering}},
    thread,
    time::{Duration, Instant},
};

const OPERATIONS_PER_THREAD: usize = 1_000_000;
const SAMPLES: usize = 9;

#[derive(Clone, Copy)]
enum Policy {
    AcqRel,
    Relaxed,
}

impl Policy {
    const fn name(self) -> &'static str {
        match self {
            Self::AcqRel => "acqrel",
            Self::Relaxed => "relaxed",
        }
    }

    const fn load(self) -> Ordering {
        match self {
            Self::AcqRel => Ordering::Acquire,
            Self::Relaxed => Ordering::Relaxed,
        }
    }

    const fn success(self) -> Ordering {
        match self {
            Self::AcqRel => Ordering::AcqRel,
            Self::Relaxed => Ordering::Relaxed,
        }
    }

    const fn failure(self) -> Ordering {
        self.load()
    }

    const fn release(self) -> Ordering {
        match self {
            Self::AcqRel => Ordering::Release,
            Self::Relaxed => Ordering::Relaxed,
        }
    }
}

struct CreditPool {
    available: AtomicUsize,
    capacity: usize,
    policy: Policy,
}

impl CreditPool {
    fn new(capacity: usize, policy: Policy) -> Self {
        Self { available: AtomicUsize::new(capacity), capacity, policy }
    }

    fn reserve_one(&self) {
        let mut observed = self.available.load(self.policy.load());
        loop {
            if observed == 0 {
                std::hint::spin_loop();
                observed = self.available.load(self.policy.load());
                continue;
            }
            match self.available.compare_exchange_weak(
                observed,
                observed - 1,
                self.policy.success(),
                self.policy.failure(),
            ) {
                Ok(_) => return,
                Err(actual) => observed = actual,
            }
        }
    }

    fn release_one(&self) {
        let previous = self.available.fetch_add(1, self.policy.release());
        assert!(previous < self.capacity);
    }
}

fn sample(threads: usize, capacity: usize, policy: Policy) -> Duration {
    let pool = Arc::new(CreditPool::new(capacity, policy));
    let barrier = Arc::new(Barrier::new(threads + 1));
    let start = thread::scope(|scope| {
        for _ in 0..threads {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            scope.spawn(move || {
                barrier.wait();
                for operation in 0..OPERATIONS_PER_THREAD {
                    pool.reserve_one();
                    black_box(operation);
                    pool.release_one();
                }
            });
        }
        barrier.wait();
        let start = Instant::now();
        start
    });
    let elapsed = start.elapsed();
    assert_eq!(pool.available.load(Ordering::Relaxed), capacity);
    elapsed
}

fn median(mut values: Vec<Duration>) -> Duration {
    values.sort_unstable();
    values[values.len() / 2]
}

fn main() {
    let available = thread::available_parallelism().map_or(1, usize::from);
    println!("format\tcredit-ordering-v1");
    println!("available_parallelism\t{available}");
    println!("columns\tpolicy\tthreads\tcapacity\toperations\tmedian_ns\toperations_per_second");
    for threads in [1, 2, 4, 8].into_iter().filter(|threads| *threads <= available) {
        for capacity in [1, threads] {
            for policy in [Policy::AcqRel, Policy::Relaxed] {
                let _warmup = sample(threads, capacity, policy);
                let elapsed = median(
                    (0..SAMPLES)
                        .map(|_| sample(threads, capacity, policy))
                        .collect(),
                );
                let operations = threads * OPERATIONS_PER_THREAD;
                let rate = operations as f64 / elapsed.as_secs_f64();
                println!(
                    "result\t{}\t{threads}\t{capacity}\t{operations}\t{}\t{rate:.0}",
                    policy.name(),
                    elapsed.as_nanos(),
                );
            }
        }
    }
}
