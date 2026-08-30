#![allow(dead_code)]

pub struct PublicFact {
    pub bytes: usize,
}

impl PublicFact {
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    pub const fn bytes_of(fact: &Self) -> usize {
        fact.bytes
    }
}

macro_rules! local_accessor {
    ($name:ident) => {
        pub const fn $name(&self) -> usize {
            self.bytes
        }
    };
}

impl PublicFact {
    local_accessor!(expanded_bytes);
}

trait PublicFactView {
    fn bytes(&self) -> usize;
}

impl PublicFactView for PublicFact {
    fn bytes(&self) -> usize {
        self.bytes
    }
}

pub struct ProtectedFact {
    bytes: usize,
}

impl ProtectedFact {
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

fn main() {}
