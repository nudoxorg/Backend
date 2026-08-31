#![allow(dead_code)]

pub enum Presence {
    Present(usize),
    Missing,
}

pub struct PublishedFact {
    pub term: usize,
    pub document: usize,
    pub value: Presence,
}

impl PublishedFact {
    pub const fn new(term: usize, document: usize, score: usize) -> Self {
        Self {
            term,
            document,
            value: Presence::Present(score),
        }
    }
}

pub struct RenamedFact {
    pub term: usize,
    pub document: usize,
    pub value: usize,
}

impl RenamedFact {
    pub const fn new(term_input: usize, document_input: usize, score_input: usize) -> Self {
        Self {
            term: term_input,
            document: document_input,
            value: score_input,
        }
    }
}

pub struct ConvertedFact {
    pub term: usize,
    pub document: usize,
    pub score: usize,
}

impl From<(usize, usize, usize)> for ConvertedFact {
    fn from(input: (usize, usize, usize)) -> Self {
        Self {
            term: input.0,
            document: input.1,
            score: input.2,
        }
    }
}

pub struct TryConvertedFact {
    pub term: usize,
    pub document: usize,
    pub score: usize,
}

impl core::convert::TryFrom<(usize, usize, usize)> for TryConvertedFact {
    type Error = ();

    fn try_from(input: (usize, usize, usize)) -> Result<Self, Self::Error> {
        Ok(Self {
            term: input.0,
            document: input.1,
            score: input.2,
        })
    }
}

pub struct ValidatedFact {
    pub term: usize,
    pub document: usize,
    pub score: usize,
}

impl ValidatedFact {
    pub const fn new(term: usize, document: usize, score: usize) -> Self {
        let score = if score > 100 { 100 } else { score };
        Self {
            term,
            document,
            score,
        }
    }
}

pub struct SealedFact {
    pub term: usize,
    score: usize,
}

impl SealedFact {
    pub const fn new(term: usize, score: usize) -> Self {
        Self { term, score }
    }
}

pub struct CrateConstructedFact {
    pub value: usize,
}

impl CrateConstructedFact {
    pub(crate) const fn new(value: usize) -> Self {
        Self { value }
    }
}

pub struct UnsafeFact {
    pub value: usize,
}

impl UnsafeFact {
    pub const fn new(value: usize) -> Self {
        unsafe { Self { value } }
    }
}

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
