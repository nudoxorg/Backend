//! Emits deterministic tab-separated evidence for the representation laboratory.

#[global_allocator]
static ALLOCATOR: nudox_layout_lab::experiments::TrackingAllocator =
    nudox_layout_lab::experiments::TrackingAllocator::new();

fn main() {
    nudox_layout_lab::experiments::run(&ALLOCATOR);
}
