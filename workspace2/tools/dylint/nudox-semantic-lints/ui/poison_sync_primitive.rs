#![allow(dead_code)]

use std::sync::{Condvar, Mutex, OnceLock, RwLock, atomic::AtomicBool, mpsc::sync_channel};

// These look like supported paths but resolve to local definitions and must remain clean.
mod loom {
    pub mod sync {
        pub mod condvar {
            pub struct Condvar;
        }

        pub mod mutex {
            pub struct Mutex;
        }

        pub mod rwlock {
            pub struct RwLock;
        }
    }
}

mod tokio {
    pub mod sync {
        pub struct Mutex;
        pub struct RwLock;
        pub struct Notify;
    }
}

mod parking_lot {
    pub struct Mutex;
    pub struct RwLock;
}

type MutexAlias = Mutex<u8>;
type RwLockAlias = RwLock<u8>;
type CondvarAlias = Condvar;

struct PoisoningFields {
    mutex: Mutex<u8>,
    readers: RwLock<u8>,
    wake: Condvar,
}

fn inferred_constructor() {
    let mutex = Mutex::new(0_u8);
    drop(mutex.lock());
}

fn unused_inferred_constructor() {
    let _mutex = Mutex::new(0_u8);
}

fn consume<T>() {}

fn generic_type_use() {
    consume::<Mutex<u8>>();
}

fn inferred_alias_constructor() {
    let mutex = MutexAlias::new(0_u8);
    drop(mutex.lock());
}

fn alias_methods(mutex: &MutexAlias, readers: &RwLockAlias, wake: &CondvarAlias) {
    drop(mutex.lock());
    drop(readers.read());
    let _ = wake.wait_timeout(mutex.lock().unwrap(), std::time::Duration::ZERO);
}

fn allowed_lookalikes() {
    let _loom_mutex = loom::sync::mutex::Mutex;
    let _loom_readers = loom::sync::rwlock::RwLock;
    let _loom_wake = loom::sync::condvar::Condvar;
    let _tokio_mutex = tokio::sync::Mutex;
    let _tokio_readers = tokio::sync::RwLock;
    let _notify = tokio::sync::Notify;
    let _parking_mutex = parking_lot::Mutex;
    let _parking_readers = parking_lot::RwLock;
    let _once: OnceLock<u8> = OnceLock::new();
}

fn allowed_coordination() {
    let _ready = AtomicBool::new(false);
    let (_send, _receive) = sync_channel::<u8>(1);
}

fn main() {}
