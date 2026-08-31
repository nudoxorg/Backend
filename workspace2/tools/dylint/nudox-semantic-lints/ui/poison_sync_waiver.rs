#![allow(dead_code)]

use std::sync::Mutex;

#[allow(nudox_poison_sync_primitive)]
fn attempted_waiver() {
    let _mutex = Mutex::new(0_u8);
}

fn main() {}
