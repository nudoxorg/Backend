//! Replayable raw-byte mutation corpus for every Wave 1 frame structural field.

mod fields;
mod fixture;
mod hostile;
mod mutation;

use bolero::check;

use fixture::CorpusError;

#[test]
fn bolero_combines_structural_byte_mutations() {
    check!().for_each(|input: &[u8]| {
        assert_eq!(mutation::structural_mutation_laws(input), Ok(()));
    });
}

#[test]
fn every_byte_value_has_exact_structural_provenance() -> Result<(), CorpusError> {
    for raw in u8::MIN..=u8::MAX {
        mutation::exhaustive_single_byte_mutation_laws(raw)?;
    }
    Ok(())
}

#[test]
fn hostile_ordering_truncation_and_correlated_boundaries_are_exact() -> Result<(), CorpusError> {
    hostile::ordering_laws()?;
    hostile::truncation_laws()?;
    hostile::correlated_boundary_laws()
}
