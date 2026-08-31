#![allow(dead_code)]

use std::sync::{Condvar, Mutex, RwLock, atomic::AtomicBool, mpsc::sync_channel};

struct PoisoningFields {
    mutex: Mutex<u8>,
    readers: RwLock<u8>,
    wake: Condvar,
}

fn inferred_constructor() {
    let mutex = Mutex::new(0_u8);
    drop(mutex.lock());
}

fn allowed_coordination() {
    let _ready = AtomicBool::new(false);
    let (_send, _receive) = sync_channel::<u8>(1);
}

fn main() {}
