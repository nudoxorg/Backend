use nudox_hydration::VerifiedGeneration;

fn hypothetical_witness(evidence: &String) -> VerifiedGeneration<'_, String> {
    let _ = evidence;
    loop {}
}

fn consume(witness: &VerifiedGeneration<'_, String>) {
    let _ = witness;
}

fn main() {
    let evidence = String::from("store snapshot");
    let witness = hypothetical_witness(&evidence);
    drop(evidence);
    consume(&witness);
}
