use std::hint::black_box;
use std::time::Instant;

#[inline(never)]
fn uncached(basis: &[u8; 32], overlays: usize) -> u64 {
    let mut sum = 0u64;
    for _ in 0..overlays {
        let mut value = [0u8; 32];
        value.copy_from_slice(basis);
        sum = sum.wrapping_add(u64::from(value[0]) + u64::from(value[31]));
        black_box(value);
    }
    black_box(sum)
}

#[inline(never)]
fn cached(basis: &[u8; 32], overlays: usize) -> u64 {
    let value = *basis;
    let mut sum = 0u64;
    for _ in 0..overlays {
        sum = sum.wrapping_add(u64::from(value[0]) + u64::from(value[31]));
        black_box(value);
    }
    black_box(sum)
}

fn main() {
    let basis = [0x5au8; 32];
    let iterations = 100_000usize;
    println!("overlays\trun\tuncached_ns\tcached_ns");
    for overlays in [0usize, 1, 4, 16, 32, 64, 128, 256, 1024, 4096, 16384] {
        for run in 0..7 {
            let start = Instant::now();
            let mut x = 0u64;
            for _ in 0..iterations { x ^= uncached(black_box(&basis), overlays); }
            let uncached_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            for _ in 0..iterations { x ^= cached(black_box(&basis), overlays); }
            let cached_ns = start.elapsed().as_nanos();
            black_box(x);
            println!("{overlays}\t{run}\t{uncached_ns}\t{cached_ns}");
        }
    }
}
