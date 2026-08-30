use std::{convert::Infallible, hint::black_box, sync::{Arc, atomic::{AtomicU64, AtomicUsize, Ordering}}, thread, time::Instant};
use nudox_runtime::{AtomicAccounting, BoundedWork, ByteBudget, ByteQuantum, RemoteRuntime, RetainedBytes};

#[derive(Debug)]
struct Work { bytes: Vec<u8>, checksum: u64 }
impl Work { fn new(size: usize, seq: usize) -> Self { Self { bytes: vec![(seq as u8).wrapping_mul(31); size], checksum: 0 } } }
impl BoundedWork for Work { fn retained_bytes(&self) -> RetainedBytes { self.bytes.len().into() } }

fn run<P: Default + nudox_runtime::Accounting + Sync>(producers: usize, payload: usize, n: usize) -> (u128, usize, usize, nudox_runtime::RuntimeMetrics) {
    let budget = ByteBudget::new(RetainedBytes::from(64 * 1024 * 1024), ByteQuantum::try_from(1).unwrap()).unwrap();
    let mut runtime: RemoteRuntime<u64, Work, Infallible, P> = RemoteRuntime::new(64, budget).unwrap();
    let (admission, mut owner) = runtime.split();
    let accepted = Arc::new(AtomicUsize::new(0));
    let rejected = Arc::new(AtomicUsize::new(0));
    let completed = Arc::new(AtomicUsize::new(0));
    let checksum = Arc::new(AtomicU64::new(0));
    let start = Instant::now();
    thread::scope(|scope| {
        for producer in 0..producers {
            let a = admission.clone(); let accepted = Arc::clone(&accepted); let rejected = Arc::clone(&rejected);
            scope.spawn(move || {
                for i in 0..(n / producers) {
                    let mut work = Work::new(payload, producer * n + i);
                    loop {
                        match a.admit(0, work) {
                            Ok(_) => { accepted.fetch_add(1, Ordering::Relaxed); break; }
                            Err(nudox_runtime::AdmissionError::Rejected { rejected: r }) => { work = r.work; rejected.fetch_add(1, Ordering::Relaxed); thread::yield_now(); }
                            Err(other) => panic!("unexpected admission error: {other:?}"),
                        }
                    }
                }
            });
        }
        while completed.load(Ordering::Acquire) < n {
            if owner.execute_next(&0, |w| { let mut x = 0u64; for &b in &w.bytes { x = x.wrapping_add(b as u64); } w.checksum = x; Ok::<(), Infallible>(()) }).unwrap() == nudox_runtime::OwnerProgress::Idle { thread::yield_now(); }
            while let Some(_event) = owner.poll_terminal().unwrap() { checksum.fetch_add(1, Ordering::Relaxed); completed.fetch_add(1, Ordering::Release); }
        }
        while owner.poll_terminal().unwrap().is_some() {}
    });
    let elapsed = start.elapsed().as_nanos();
    let metrics = runtime.metrics();
    (elapsed, checksum.load(Ordering::Relaxed) as usize, rejected.load(Ordering::Relaxed), metrics)
}

fn main() {
    println!("policy,producers,payload,n,median_ns,completed,checked_out,reserved,terminal,rejected_slots,rejected_bytes,checksum");
    for policy in ["default", "atomic"] { for &producers in &[1,2,4,8] { for &payload in &[0,64,4096] {
        let mut times = Vec::new(); let mut last = None;
        for _ in 0..5 { let r = if policy == "default" { run::<()>(producers,payload,20_000) } else { run::<AtomicAccounting>(producers,payload,20_000) }; times.push(r.0); last = Some(r); }
        times.sort_unstable(); let r = last.unwrap();
        println!("{policy},{producers},{payload},20000,{}, {},{},{},{},{},{},{}", times[2], 20_000, r.3.checked_out, r.3.reserved_bytes, r.3.terminal_occupied, r.2, 0, r.1);
        black_box(r);
    }}}
}
