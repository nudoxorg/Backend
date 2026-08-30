use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_root::{BorrowedGenerationViewFacts, EntryKey, RootEntryCount, ValidatedLocality, ValidatedRoot, ValidatedRootFacts};

fn main() {
    let facts = ValidatedRootFacts { bytes: &[], id: GenerationId::from(ContentId::from([0; 32])), entry_count: RootEntryCount::from(0) };
    let _ = BorrowedGenerationViewFacts { id: facts.id, entry_count: facts.entry_count };
    let _ = EntryKey::from(1);
    let _ = core::marker::PhantomData::<ObjectDomain>;
}

fn rewrite(root: &mut ValidatedRoot<'_, ObjectDomain>) {
    root.entry_count = RootEntryCount::from(1);
}

fn rewrite_locality(locality: &mut ValidatedLocality<'_, ObjectDomain>) {
    locality.root_count = RootEntryCount::from(1);
}

