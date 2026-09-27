//! Single entry for every discovered harness.
//!
//! `FUZZ_TARGET` selects `targets/<id>/`. Supervised runners are
//! `.#continuous-fuzz.bins.<id>`, which set that variable before exec.

fn main() {
    let known: Vec<&str> = backend_fuzz::targets()
        .iter()
        .map(|target| target.name)
        .collect();
    let joined = known.join(", ");
    let Ok(name) = std::env::var("FUZZ_TARGET") else {
        eprintln!(
            "FUZZ_TARGET is unset. Known targets: {joined}. Supervised runs use .#continuous-fuzz.bins.<id>."
        );
        std::process::exit(2);
    };
    let Some(target) = backend_fuzz::target(&name) else {
        eprintln!("unknown fuzz target {name}. Known targets: {joined}.");
        std::process::exit(2);
    };
    backend_fuzz::drive(target.max_len, target.exercise);
}
