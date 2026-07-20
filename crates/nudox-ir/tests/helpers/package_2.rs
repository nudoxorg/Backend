use super::*;

pub fn build_package_2(b: &mut EntryBuilder, id: &mut IdGen) {
    b.create(id(), symbol("m1"), |b| {
        b.create(id(), symbol("m2"), |b| {
            b.create(id(), symbol("m3"), |b| {
                b.create(id(), symbol("m4"), |_| Module);

                Module
            });

            Module
        });

        Module
    });
}

pub fn expected_package_2(
    sym: &str,
    pkg: PackageId,
    id_to_idx: impl Fn(UniqueId<usize>) -> RawEntryIdx,
) -> (Vec<UniqueId<usize>>, Vec<Entry>) {
    let idx = |id: usize| id_to_idx(UniqueId::new(PackageId::clone(&pkg), id));

    let root = id_to_idx(UniqueId::root(PackageId::clone(&pkg)));

    #[rustfmt::skip]
    let entries = [
        entry(sym, n::root([idx(0)]), Module),
	        entry("m1", n(root, [idx(1)]), Module),
		        entry("m2", n(idx(0), [idx(2)]), Module),
			        entry("m3", n(idx(1), [idx(3)]), Module),
				        entry("m4", n::leaf(idx(2)), Module),
    ];

    let ids = std::iter::once(UniqueId::root(PackageId::clone(&pkg)))
        .chain((0..entries.len() - 1).map(move |id| UniqueId::new(PackageId::clone(&pkg), id)))
        .collect();

    (ids, Vec::from(entries))
}
