pub struct SealedFact {
    bytes: usize,
}

#[allow(nudox_redundant_public_accessor)]
impl SealedFact {
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}

fn main() {}
