use nudox_id::ObjectDomain;
use nudox_root::{RootEntryCount, ValidatedLocality, ValidatedRoot};

fn main() {
}

fn rewrite(root: &mut ValidatedRoot<'_, ObjectDomain>) {
    root.entry_count = RootEntryCount::from(1);
}

fn rewrite_locality(locality: &mut ValidatedLocality<'_, ObjectDomain>) {
    locality.root_count = RootEntryCount::from(1);
}
