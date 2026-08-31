#![allow(dead_code)]

pub struct PublicFact {
    pub bytes: usize,
}

pub struct TupleFact(pub usize);

impl TupleFact {
    pub const fn value(&self) -> &usize {
        &self.0
    }
}

pub struct InnerFact {
    pub value: usize,
}

impl InnerFact {
    pub const fn adjusted(&self, amount: usize) -> usize {
        self.value + amount
    }
}

pub struct OuterFact {
    inner: InnerFact,
}

impl OuterFact {
    pub const fn adjusted(&self, amount: usize) -> usize {
        self.inner.adjusted(amount)
    }

    pub const fn adjusted_more(&self, amount: usize) -> usize {
        self.inner.adjusted(amount + 1)
    }
}

impl PublicFact {
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    pub const fn into_bytes(self) -> usize {
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

struct CrateFact {
    bytes: usize,
}

impl CrateFact {
    pub(crate) const fn bytes(&self) -> usize {
        self.bytes
    }
}

fn main() {}
