// #[path = "./resolver.rs"]
// mod resolver;

// #[path = "./helpers/package_1.rs"]
// mod package_1;

// #[path = "./helpers/package_2.rs"]
// mod package_2;

// mod shared_test_helpers {
//     pub fn entry_ref(name: &str, node: Node, idx: RawEntryIdx) -> Entry {
//         Entry::reference(symbol(name), node, idx)
//     }

//     include!("../shared_test_helpers.rs");
// }

// use std::collections::HashMap;

// pub use nudox_ir::registry::{EntryIdx, Registry};

// pub use self::{package_1::*, package_2::*, resolver::*,
// shared_test_helpers::*};

// pub type EntryBuilder = nudox_ir::registry::EntryBuilder<ExampleResolver>;

// pub fn build_package(b: &mut EntryBuilder, build: fn(&mut EntryBuilder, &mut
// IdGen)) {     build(b, &mut {
//         let mut id = 0;
//         move || {
//             id += 1;
//             id - 1
//         }
//     })
// }

// pub type IdGen = dyn FnMut() -> usize;

// pub fn pkg(n: usize) -> PackageId {
//     PackageId::path(format!("/package/{n}"))
// }

// pub async fn async_to_sync_iter<F: Future>(
//     it: impl Iterator<Item = F>,
// ) -> impl Iterator<Item = F::Output> {
//     let mut items = Vec::new();
//     for item in it {
//         items.push(item.await);
//     }
//     items.into_iter()
// }
