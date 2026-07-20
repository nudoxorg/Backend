use super::*;

pub fn build_package_1(b: &mut EntryBuilder, id: &mut IdGen) {
    // you would typically do something more sophisticated to track
    // the indices you might need to reference in the future, but
    // this is good enough for a simple test example
    let mut records = HashMap::new();

    b.create(id(), symbol("mod_1"), |b| {
        let idx = b.create(id(), symbol("record_1"), |b| {
            Record::builder().fields([]).build(b)
        });

        records.insert("mod_1/record_1", idx);

        let idx = b.create(id(), symbol("record_2"), |b| {
            let fields = ["field_1", "field_2", "field_3"]
                .map(|name| symbol(name))
                .map(|sym| b.create(id(), sym, |_| Field {}));

            Record::builder().fields(fields).build(b)
        });

        records.insert("mod_1/record_2", idx);

        Module
    });

    b.create(id(), symbol("mod_2"), |b| {
        let idx = b.create(id(), symbol("record_3"), |b| {
            let fields = ["field_1", "field_2", "field_3"]
                .map(|name| symbol(name))
                .map(|sym| b.create(id(), sym, |_| Field {}));

            Record::builder().fields(fields).build(b)
        });

        records.insert("mod_2/record_3", idx);

        Module
    });

    b.create_ref(
        id(),
        symbol("record_1_re_export"),
        records["mod_1/record_1"],
    );

    b.create_ref(
        id(),
        symbol("record_2_re_export"),
        records["mod_1/record_2"],
    );

    b.create_ref(
        id(),
        symbol("record_3_re_export"),
        records["mod_2/record_3"],
    );
}

pub fn expected_package_1(
    sym: &str,
    pkg: PackageId,
    id_to_idx: impl Fn(UniqueId<usize>) -> RawEntryIdx,
) -> (Vec<UniqueId<usize>>, Vec<Entry>) {
    let idx = |id: usize| id_to_idx(UniqueId::new(PackageId::clone(&pkg), id));

    let root = id_to_idx(UniqueId::root(PackageId::clone(&pkg)));

    #[rustfmt::skip]
    let entries = [
        entry(sym, n::root([0, 6, 11, 12, 13].map(idx)), Module),
	        entry("mod_1", n(root, [1, 2].map(idx)), Module),
		        entry("record_1", n::leaf(idx(0)), Record { fields: list![] }),
		        entry("record_2", n(idx(0), [3, 4, 5].map(idx)), Record {
		            fields: [3, 4, 5].map(idx).map(EntryIdx::typed).into(),
		        }),
			        entry("field_1", n::leaf(idx(2)), Field {}),
			        entry("field_2", n::leaf(idx(2)), Field {}),
			        entry("field_3", n::leaf(idx(2)), Field {}),
	        entry("mod_2", n(root, [idx(7)]), Module),
		        entry("record_3", n(idx(6), [8, 9, 10].map(idx)), Record {
		            fields: [8, 9, 10].map(idx).map(EntryIdx::typed).into(),
		        }),
			        entry("field_1", n::leaf(idx(7)), Field {}),
			        entry("field_2", n::leaf(idx(7)), Field {}),
			        entry("field_3", n::leaf(idx(7)), Field {}),
	        entry_ref("record_1_re_export", n::leaf(root), idx(1)),
	        entry_ref("record_2_re_export", n::leaf(root), idx(2)),
	        entry_ref("record_3_re_export", n::leaf(root), idx(7)),
    ];

    let ids = std::iter::once(UniqueId::root(PackageId::clone(&pkg)))
        .chain((0..entries.len() - 1).map(move |id| UniqueId::new(PackageId::clone(&pkg), id)))
        .collect();

    (ids, Vec::from(entries))
}
