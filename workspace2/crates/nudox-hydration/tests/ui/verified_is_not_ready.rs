use nudox_hydration::{ReadyGeneration, VerifiedGeneration};

fn require_ready(_: ReadyGeneration) {}

fn verified() -> VerifiedGeneration {
    loop {}
}

fn main() {
    require_ready(verified());
}
