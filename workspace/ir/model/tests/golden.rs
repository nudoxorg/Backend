// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Golden byte-pin harness for nudox-ir
//
// PURPOSE
// -------
// This file gates the cutover from `workspace/ir` (~300 call sites) to
// `nudox-ir`. If any of these pins fails, either:
//   (a) you changed the IntroId preimage layout intentionally → bump
//       `change::FORMAT_VERSION`, re-run the regeneration command below, paste
//       the new values in, and update the comment on the relevant const; or
//   (b) you changed something accidentally → investigate before merging.
//
// REGENERATION
// ------------
// When pins legitimately change (e.g. the skeleton encoding evolves), run:
//
//   UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden
//
// The test will print freshly-computed values to stdout in paste-ready form and
// then **fail**, so CI cannot accidentally leave UPDATE_GOLDEN set.
//
// After pasting the new values, run without UPDATE_GOLDEN to confirm the pins
// pass.
//
// PIN VALUES
// ----------
// The pins below are live values, generated against the sealed fixture. A pin
// set to the sentinel "<REGENERATE>" is NOT a guard — it means someone added a
// pin without running the command above.
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

use std::path::PathBuf;

use triomphe::Arc;

use nudox_ir::{
    apply::PristineIntroTable,
    build::{
        EcosystemId, GenericParam, ImplFlags, IrPackage, PackageId, PackageLineageId, PackageName,
        Visibility, WherePred,
    },
    change::{IntroId, StableRef},
    entry::{AttrTok, CfgExpr, Deprecation, DocLink, Symbol},
    foreign::{ForeignKey, Unlinked},
    index::Ref,
    kinds::{
        Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, Impl, Module, Param,
        Record, RecordForm, Static, Trait, Variant, VariantForm, ty::Type,
    },
};

// ── Helpers ────────────────────────────────────────────────────────────────

/// Minimal symbol suitable for most fixture entries.
fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// A symbol with every field that is accessible from an integration test
/// populated. This exercises: non-trivial span, non-empty aliases, non-empty
/// documentation, a non-default visibility, and a non-empty source path.
///
/// Every optional field is populated, so the pins catch a change to any of
/// them. (`Deprecation` and `DocLink` were originally unreachable from an
/// integration test — they are fields of the public `Symbol` but were not
/// re-exported. That export gap was fixed rather than worked around, which is
/// why this fixture can be complete.)
fn rich_sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Crate,
        documentation: "Does something important. See also other_fn.".to_owned(),
        // A literal path — NOT env!("CARGO_MANIFEST_DIR") — so it is
        // identical on every machine and run.
        source: PathBuf::from("crates/golden-fixture/src/lib.rs"),
        span: 42..137,
        aliases: Box::new(["golden_fn_alias".to_owned(), "gfn".to_owned()]),
        deprecation: Some(Deprecation {
            note: Some("use `other_fn` instead".to_owned()),
            since: Some("1.2.0".to_owned()),
        }),
        doc_links: Box::new([DocLink {
            target: "crate::other_fn".to_owned(),
            label: Some("other_fn".to_owned()),
        }]),
        attrs: Box::new([
            AttrTok {
                token: "must_use".to_owned(),
                arg: None,
            },
            AttrTok {
                token: "repr".to_owned(),
                arg: Some("C".to_owned()),
            },
        ]),
        // A nested predicate, so the recursive encoding is pinned too.
        cfg: Some(CfgExpr::All(Box::new([
            CfgExpr::Feature("serde".to_owned()),
            CfgExpr::Not(Box::new(CfgExpr::TargetOs("windows".to_owned()))),
        ]))),
    }
}

/// The package lineage used by the entire fixture.
fn lineage() -> PackageLineageId {
    PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new("golden-fixture"),
    )
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// The canonical fixture
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Build the rich fixture package, seal it, and return the table.
///
/// The fixture deliberately exercises the full encoding surface:
///
/// - Nested modules
/// - A record with named fields
/// - A tuple record with positional fields
/// - A generic record with a type-param that has a default
/// - An enum with variants (including a struct-form variant)
/// - A trait
/// - Two impls: one inherent (with generics + where-clause) and one negative
///   blanket impl — these are individually pinned because they are the entries
///   most likely to collide under a disambiguator bug
/// - An overloaded function pair: same name, same parent, different signatures
///   — exercises the `FnOverload` disambiguator path
/// - A re-export via `create_ref` (EntryInner::Reference)
/// - A const and a static
/// - A `Symbol` with all fields accessible from an integration test populated
///   (see `rich_sym`)
///
/// Every string is a literal. Nothing derives from the environment.
fn build_fixture() -> PristineIntroTable {
    // Stable, monotone id generator — same order on every run.
    let mut next_id = 0usize;
    let mut id = move || {
        next_id += 1;
        next_id
    };

    let pkg = IrPackage::build(
        PackageId::path("golden-fixture"),
        sym("golden_fixture"),
        |mut root| {
            // ── inner module ──────────────────────────────────────────────
            root.create(id(), sym("shapes"), |mut m| {
                // Named-field record: `struct Point { x: i32, y: i32 }`
                m.create(id(), sym("Point"), |mut rec| {
                    let x = rec.create(id(), sym("x"), |_| {
                        Field::builder().key(FieldKey::Named).ty(Type::I32).build()
                    });
                    let y = rec.create(id(), sym("y"), |_| {
                        Field::builder()
                            .key(FieldKey::Named)
                            .ty(Type::I32)
                            .attributes([FieldAttribute::Mutable])
                            .build()
                    });
                    Record::builder().fields([x, y]).build()
                });

                // Tuple record: `struct Pair(i32, i64)` — positional fields
                m.create(id(), sym("Pair"), |mut rec| {
                    let a = rec.create(id(), sym("0"), |_| {
                        Field::builder()
                            .key(FieldKey::Positional(0))
                            .ty(Type::I32)
                            .build()
                    });
                    let b = rec.create(id(), sym("1"), |_| {
                        Field::builder()
                            .key(FieldKey::Positional(1))
                            .ty(Type::I64)
                            .build()
                    });
                    Record::builder()
                        .form(RecordForm::Tuple)
                        .fields([a, b])
                        .build()
                });

                // Generic record with a type-param default:
                // `struct Container<T: Any = i32> { value: T }`
                m.create(id(), sym("Container"), |mut rec| {
                    let v = rec.create(id(), sym("value"), |_| {
                        Field::builder().key(FieldKey::Named).ty(Type::Any).build()
                    });
                    Record::builder()
                        .fields([v])
                        .generics([GenericParam::Type {
                            name: "T".to_owned(),
                            bounds: [Type::Any].into(),
                            default: Some(Type::I32),
                            variance: None,
                        }])
                        .build()
                });

                // Enum: `enum Color { Red, Green, Blue { label: bool } }`
                // Blue is a struct-form variant for extra variant-form coverage.
                m.create(id(), sym("Color"), |mut en| {
                    let red = en.create(id(), sym("Red"), |_| Variant::builder().build());
                    let green = en.create(id(), sym("Green"), |_| Variant::builder().build());
                    let blue = en.create(id(), sym("Blue"), |_| {
                        Variant::builder().form(VariantForm::Struct).build()
                    });
                    Enum::builder().variants([red, green, blue]).build()
                });

                Module
            });

            // ── trait ─────────────────────────────────────────────────────
            // `pub trait Drawable: Any { … }`
            // Generics are included so the trait exercises that field path.
            root.create(id(), sym("Drawable"), |_| {
                Trait::builder()
                    .supers([Type::Any])
                    .generics([GenericParam::Type {
                        name: "Output".to_owned(),
                        bounds: [].into(),
                        default: None,
                        variance: None,
                    }])
                    .build()
            });

            // ── inherent impl with generics + where-clause ─────────────────
            // `impl<T> Container<T> where Self: Any { … }`
            // self_ty = Type::Any as a primitive stand-in (nominal types
            // cannot be resolved before sealing; using Any keeps this
            // deterministic without depending on insertion order).
            root.create(id(), sym("impl"), |_| {
                Impl::builder()
                    .self_ty(Type::Any)
                    .generics([GenericParam::Type {
                        name: "T".to_owned(),
                        bounds: [Type::Any].into(),
                        default: None,
                        variance: None,
                    }])
                    .wheres([WherePred {
                        target: Type::SelfType,
                        bounds: [Type::Any].into(),
                    }])
                    .build()
            });

            // ── negative blanket impl ──────────────────────────────────────
            // `impl !Drawable for T` — negative=true, blanket=true.
            // Same leaf name ("impl") as the entry above: the seal pass must
            // assign DISTINCT IntroIds via the TraitImpl disambiguator.
            root.create(id(), sym("impl"), |_| {
                Impl::builder()
                    .of(Type::Any)
                    .self_ty(Type::SelfType)
                    .flags(ImplFlags {
                        negative: true,
                        blanket: true,
                    })
                    .build()
            });

            // ── overloaded function pair ───────────────────────────────────
            // `fn draw(x: i32)` and `fn draw(x: i64)` — same name, same
            // parent path. The seal pass must assign DISTINCT IntroIds via the
            // FnOverload disambiguator (signature skeleton differs: i32 vs i64).
            //
            // The first overload gets the rich_sym (non-trivial span, aliases,
            // documentation, Crate visibility) to exercise every Symbol field
            // that is constructible from an integration test.
            root.create(id(), rich_sym("draw"), |mut f| {
                let p = f.create(id(), sym("x"), |_| Param::builder().ty(Type::I32).build());
                Function::builder()
                    .input_params([p])
                    .modifiers([FnModifier::Unsafe])
                    .build()
            });

            root.create(id(), sym("draw"), |mut f| {
                let p = f.create(id(), sym("x"), |_| Param::builder().ty(Type::I64).build());
                Function::builder()
                    .input_params([p])
                    .modifiers([FnModifier::Async])
                    .build()
            });

            // ── re-export ─────────────────────────────────────────────────
            // A re-export entry (EntryInner::Reference). We point at a
            // Foreign ref with a deterministic sentinel intro so the fixture
            // does not depend on the insertion order of the `draw` entries.
            //
            // The key is what is hashed; the resolved target deliberately is
            // not, so this pin also witnesses that a linked and an unlinked
            // reference to the same target hash identically.
            {
                let foreign_ref: Ref<Function> = Ref::Foreign {
                    key: Arc::new(ForeignKey::in_package(
                        lineage(),
                        "demo::render",
                        "render",
                    )),
                    target: Some(StableRef::new(lineage(), IntroId::from_raw([0xd4; 32]))),
                };
                root.create_ref(id(), sym("render"), foreign_ref);
            }

            // ── const + static ────────────────────────────────────────────
            root.create(id(), sym("MAX_SIZE"), |_| {
                Const::builder().ty(Type::U64).build()
            });

            root.create(id(), sym("INSTANCE_COUNT"), |_| {
                Static::builder().ty(Type::I32).mutable(true).build()
            });
        },
    );

    pkg.seal(&lineage(), &Unlinked).table
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Pin constants
//
// Regenerate all of these with:
//   UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Sorted hex list of every IntroId minted by the fixture, newline-joined.
/// Protects: the complete identity surface — any addition/removal/rename of a
/// fixture entry will be caught here, as will any change to the preimage
/// layout.
/// Regenerated after adding `Option<Variance>` to `GenericParam::Type`: the
/// inherent `impl<T>` block's TraitImpl skeleton now includes the `0x00`
/// variance byte, shifting its IntroId from `392cad...` to `b590aea5...`.
/// All `draw` overload and non-generic-parametric entry IntroIds are unchanged.
const GOLDEN_ALL_INTROS: &str = "0db8e8727ceffd1424c3656928bc328d45ad4e676151a98af72d92d51a6aa234
1ecafde6ce9a38a6b9f81fac9818d0fc1394f8a86bce38915a4d9da4ded3f13a
31d07cc13bc7b9c954a36f3272d16f278cbaa084dd74fea8f570483bcf4437bd
34ad74b14409309951ec1beb0cf7ef83dcb19d4729c7bb8c19ed7130ae52e8fd
415ff0ee5aebafd620c63bddf6b3c93fdec19047be64c2dfdffb8ac5c0942e26
464aa98d0046ff2d7925ae908b3d96d81caf45eb6dfcbfcbe55841c56429b930
5637e6cdce5893c15e0570432bd8feb18a4168e580fdfab0b5e4b427180b56d5
595f509906af9691d389991c85ad317aba7e804030bd9382cdcc20531234806c
5bd0258a2cde2c68306a8e5f45cfa24f306923392a2d0046f2fbb37ab6c3e457
673e9f36f04000791f0949523f1eedc37366895d72135a410ecfb966c253d71f
7900e5fbf119f34766b266cc766ab2e2127d1bf17896160086c83c84a0adfafc
79521b7add8030ace46e0681f07b377c7e0ad76a7e219decb1c58259f67e4f00
7d7c21a284d5c46d76253816a2d79b8ff147d52bb88af729dbbc9ac9902c3933
80c6f136155f3a506215f6ed9ad915721109e5d4e48a7b8b83d4990f9fbcb359
81b7b7ffbc5a4aef2bdc91032294ed56e2a32ac60d1a4bcf21d2ca7aa4da558d
87d7416585b01f74750fdb030a7f74c05abe2c245b090f89d28e08467a8275d9
89f0c4ea5f7e3449cbb07048f29f9c2a03d1367c284b87eff54f78c3ac1c9259
8ce16cd5acd2174ab0e6c81d626362a32560d4b3f2e86fe8633af7fe005c7eb2
9d41eb994a7d4e5813007bc72c27611a6ba4a6713a0aad197a53e2b0ee47c226
a0290c53b52d20c7eb692a6c099b32c0f903515c1e7cd2f8d792a664635e3c16
b0d0aa9008b66d2a518cf7fd336e4118cf0b0530555b60973d78bc1a53f619fe
c8c41dd8c8c41ee6ae105535793b90f7a30a397c56719095f2574beee870466b
cb1d5e986b78efe73097f556612a0a097b071cd8379f1ca761f4efc9ee3a9e5d
f5d2bd6554182544e5ffb01f7f5300fe212c66b7c79b01339d45e74e5ca48780";

/// BLAKE3 hex of the JSON serialization of the sorted (intro_hex, entry_json)
/// pairs. We use JSON (via the existing `serde_json` dev-dep) rather than
/// postcard because postcard is not a workspace dependency. A digest is used
/// rather than embedding kilobytes of JSON, which would make diffs unreadable.
/// Protects: the on-wire encoding of every entry kind and every field value.
const GOLDEN_ENTRIES_B3: &str = "ec166c07aba18016a059836e7056fff24617e92db8b8b4f1ba527e2e07e10663";

/// IntroId of the sorted-first `draw` overload.
/// Which overload this is depends on how their BLAKE3 digests sort; run
/// UPDATE_GOLDEN=1 to find out. What matters is that both pins are stable.
/// Protects: the FnOverload disambiguator for one of the two i32/i64 overloads.
const GOLDEN_DRAW_FIRST: &str = "5bd0258a2cde2c68306a8e5f45cfa24f306923392a2d0046f2fbb37ab6c3e457";

/// IntroId of the sorted-second `draw` overload.
/// Protects: the FnOverload disambiguator for the other i32/i64 overload.
const GOLDEN_DRAW_SECOND: &str = "8ce16cd5acd2174ab0e6c81d626362a32560d4b3f2e86fe8633af7fe005c7eb2";

/// IntroId of the sorted-first `impl` entry.
/// Protects: the TraitImpl disambiguator for one of the two impl variants.
/// Regenerated: the inherent impl's skeleton changed (variance byte added to
/// GenericParam::Type encoding), producing a new IntroId `b590aea5...`. The
/// negative blanket impl has no generics so its IntroId `8010fb61...` is stable.
const GOLDEN_IMPL_FIRST: &str = "673e9f36f04000791f0949523f1eedc37366895d72135a410ecfb966c253d71f";

/// IntroId of the sorted-second `impl` entry.
/// Protects: the TraitImpl disambiguator — specifically that `negative=true`
/// and `blanket=true` produce a distinct skeleton from the inherent impl.
/// Regenerated: see GOLDEN_IMPL_FIRST note above.
const GOLDEN_IMPL_SECOND: &str = "80c6f136155f3a506215f6ed9ad915721109e5d4e48a7b8b83d4990f9fbcb359";

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Helpers shared by the regeneration path and the assertion path
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

fn compute_all_intros(table: &PristineIntroTable) -> String {
    let mut hexes: Vec<String> = table.iter().map(|(id, _)| id.to_hex()).collect();
    hexes.sort();
    hexes.join("\n")
}

/// Serialize all (intro_hex, entry_json) pairs sorted by intro hex, then
/// BLAKE3-hash the resulting JSON.
///
/// # Serializer choice: JSON
///
/// `postcard` is not a workspace dependency.  The `serde_json` dev-dep already
/// present in `Cargo.toml` is the only available deterministic serializer.
///
/// # Why a digest
///
/// The raw JSON of a rich fixture is several kilobytes; embedding it in source
/// would make every diff unreadable. A 64-hex BLAKE3 digest is compact and
/// still content-addresses the full serialization.
///
/// # The `EntryIndex::Serialize` trap
///
/// `EntryIndex::serialize` **errors** for indices created with
/// `EntryIndex::resolved()` (the MSB sentinel is clear — see `index.rs` lines
/// 154–168). Those are build-time arena-local indices (`Ref::Local`).
///
/// After `seal()`, the pass-3 visitor rewrites every `Ref::Local` inside each
/// entry to `Ref::Intro` (same-package) or leaves it as `Ref::Foreign`
/// (cross-package). So in the sealed table no `Ref::Local` should survive, and
/// JSON serialization of every entry must succeed.
///
/// The `.expect()` call in `entries_json_round_trip` below will catch it if
/// any `Ref::Local` did survive sealing.
fn compute_entries_b3(table: &PristineIntroTable) -> String {
    let mut pairs: Vec<(String, String)> = table
        .iter()
        .map(|(id, entry)| {
            let json = serde_json::to_string(entry).expect(
                "entry must serialize — all Local refs should be lowered to Intro by seal()",
            );
            (id.to_hex(), json)
        })
        .collect();
    // Sort by intro hex so the order is deterministic regardless of HashMap
    // iteration order (PristineIntroTable uses std::collections::HashMap).
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    let combined = serde_json::to_string(&pairs).expect("Vec<(String,String)> must serialize");

    // blake3 is a [dependencies] entry of nudox-ir, so it is available to the
    // integration test via the crate's link unit.  It is also added to
    // [dev-dependencies] in Cargo.toml so the `blake3` name can be used
    // directly here.
    let digest = blake3::hash(combined.as_bytes());
    digest.to_hex().to_string()
}

/// Return all IntroIds whose entry has the given `name`, sorted for
/// deterministic pin assignment.
fn find_intros_by_name(table: &PristineIntroTable, name: &str) -> Vec<String> {
    let mut hexes: Vec<String> = table
        .iter()
        .filter(|(_, e)| e.sym().name == name)
        .map(|(id, _)| id.to_hex())
        .collect();
    hexes.sort();
    hexes
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Primary golden pin: the sorted list of minted IntroIds.
///
/// Failure means: an entry was added, removed, renamed, or the preimage layout
/// changed.  Intentional change → bump FORMAT_VERSION, regenerate.
/// Accidental change → investigate before merging.
#[test]
fn golden_intro_list() {
    let table = build_fixture();
    let actual = compute_all_intros(&table);

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        println!("\n=== UPDATE_GOLDEN: GOLDEN_ALL_INTROS ===\n{actual}\n");
        panic!(
            "UPDATE_GOLDEN is set — fresh values printed above; \
             paste into GOLDEN_ALL_INTROS and re-run without UPDATE_GOLDEN"
        );
    }

    assert_eq!(
        actual, GOLDEN_ALL_INTROS,
        "IntroId list changed.\n\
         Intentional format change → bump FORMAT_VERSION + regenerate.\n\
         Accidental change → investigate before merging.\n\
         Regeneration: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
}

/// Entry encoding pin: BLAKE3 of the sorted JSON serialization of every entry.
///
/// Failure means: the serde representation of some entry Kind or Symbol field
/// changed.  Intentional encoding change → bump FORMAT_VERSION + regenerate.
#[test]
fn golden_entry_encoding() {
    let table = build_fixture();
    let actual = compute_entries_b3(&table);

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        println!("\n=== UPDATE_GOLDEN: GOLDEN_ENTRIES_B3 ===\n{actual}\n");
        panic!(
            "UPDATE_GOLDEN is set — fresh value printed above; \
             paste into GOLDEN_ENTRIES_B3 and re-run without UPDATE_GOLDEN"
        );
    }

    assert_eq!(
        actual, GOLDEN_ENTRIES_B3,
        "Entry encoding digest changed.\n\
         Intentional encoding change → bump FORMAT_VERSION + regenerate.\n\
         Accidental change → investigate the serde impls before merging.\n\
         Regeneration: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
}

/// Named pin: the two `draw` overloads must seal to DISTINCT IntroIds, and
/// those ids must be stable across runs.
///
/// Failure means: the FnOverload disambiguator skeleton encoding changed, OR
/// the two overloads got the same IntroId (a collision regression).
#[test]
fn golden_overload_intros() {
    let table = build_fixture();
    let draws = find_intros_by_name(&table, "draw");

    assert_eq!(
        draws.len(),
        2,
        "expected exactly 2 'draw' entries (one per overload); got {}.  \
         The fixture may have drifted — update build_fixture().",
        draws.len()
    );

    assert_ne!(
        draws[0], draws[1],
        "the two `draw` overloads sealed to the SAME IntroId — \
         the FnOverload disambiguator is broken or the skeleton encoding \
         collapsed a distinction it should preserve"
    );

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        println!("\n=== UPDATE_GOLDEN: GOLDEN_DRAW_FIRST ===\n{}\n", draws[0]);
        println!("=== UPDATE_GOLDEN: GOLDEN_DRAW_SECOND ===\n{}\n", draws[1]);
        panic!(
            "UPDATE_GOLDEN is set — fresh values printed above; \
             paste into GOLDEN_DRAW_FIRST/SECOND and re-run without UPDATE_GOLDEN"
        );
    }

    assert_eq!(
        draws[0], GOLDEN_DRAW_FIRST,
        "sorted-first draw overload IntroId changed — FnOverload skeleton regression?\n\
         Regeneration: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
    assert_eq!(
        draws[1], GOLDEN_DRAW_SECOND,
        "sorted-second draw overload IntroId changed — FnOverload skeleton regression?\n\
         Regeneration: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
}

/// Named pin: the two `impl` entries must seal to DISTINCT IntroIds, and
/// those ids must be stable across runs.
///
/// Failure means: the TraitImpl disambiguator skeleton encoding changed, OR the
/// negativity/blanket flags no longer separate the two impls (collision).
#[test]
fn golden_impl_intros() {
    let table = build_fixture();
    let impls = find_intros_by_name(&table, "impl");

    assert_eq!(
        impls.len(),
        2,
        "expected exactly 2 'impl' entries; got {}.  \
         The fixture may have drifted — update build_fixture().",
        impls.len()
    );

    assert_ne!(
        impls[0], impls[1],
        "the two `impl` entries sealed to the SAME IntroId — \
         the TraitImpl disambiguator no longer folds in negativity/blanket flags, \
         OR the generics/wheres skeleton collapsed a distinction it should preserve"
    );

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        println!("\n=== UPDATE_GOLDEN: GOLDEN_IMPL_FIRST ===\n{}\n", impls[0]);
        println!("=== UPDATE_GOLDEN: GOLDEN_IMPL_SECOND ===\n{}\n", impls[1]);
        panic!(
            "UPDATE_GOLDEN is set — fresh values printed above; \
             paste into GOLDEN_IMPL_FIRST/SECOND and re-run without UPDATE_GOLDEN"
        );
    }

    assert_eq!(
        impls[0], GOLDEN_IMPL_FIRST,
        "sorted-first impl IntroId changed — TraitImpl skeleton regression?\n\
         Regeneration: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
    assert_eq!(
        impls[1], GOLDEN_IMPL_SECOND,
        "sorted-second impl IntroId changed — TraitImpl skeleton regression?\n\
         Regeneration: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
}

/// Structural sanity: the fixture must have the expected entry count.
///
/// This is not a format pin but a fixture-drift detector: if someone edits
/// `build_fixture()` without updating the golden pins, this fires before the
/// (currently placeholder) pins do.
///
/// Entry inventory:
///   root module              (1)
///   shapes module            (1)
///     Point record           (1) + x field (1) + y field (1)         = 3
///     Pair record            (1) + "0" field (1) + "1" field (1)     = 3
///     Container record       (1) + value field (1)                   = 2
///     Color enum             (1) + Red (1) + Green (1) + Blue (1)    = 4
///   Drawable trait           (1)
///   inherent impl            (1)
///   negative blanket impl    (1)
///   draw/i32 fn              (1) + x param (1)                       = 2
///   draw/i64 fn              (1) + x param (1)                       = 2
///   render re-export         (1)
///   MAX_SIZE const           (1)
///   INSTANCE_COUNT static    (1)
///   ─────────────────────────────────────────────────────────────────────
///   Total                   24
#[test]
fn fixture_entry_count() {
    let table = build_fixture();
    assert_eq!(
        table.len(),
        24,
        "fixture entry count changed — update the inventory comment above, \
         then regenerate: UPDATE_GOLDEN=1 cargo test -p nudox-ir --test golden"
    );
}

/// Determinism guard: building and sealing the fixture twice must yield
/// identical sorted IntroId sets.
///
/// This catches any HashMap-iteration-order leak inside `build_fixture()`
/// itself (e.g. a string constructed from a non-deterministic source).
#[test]
fn fixture_is_deterministic() {
    let t1 = build_fixture();
    let t2 = build_fixture();

    let s1 = compute_all_intros(&t1);
    let s2 = compute_all_intros(&t2);

    assert_eq!(
        s1, s2,
        "two calls to build_fixture() produced different IntroId sets — \
         the fixture has a non-determinism bug (HashMap iteration order leaking \
         into a constructed string, a timestamp, etc.)"
    );
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Round-trip test
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// JSON round-trip for every sealed `Entry`.
///
/// `Entry` derives `Serialize`/`Deserialize`. After `seal()`, the pass-3
/// visitor rewrites every `Ref::Local` to `Ref::Intro` (or leaves
/// `Ref::Foreign`). `Ref::Intro` and `Ref::Foreign` serialize correctly.
///
/// # The `EntryIndex::Serialize` trap
///
/// `EntryIndex::Serialize` **errors** for any index created with
/// `EntryIndex::resolved()` — those carry a clear MSB and the serializer
/// returns `Err("tried to serialize an EntryIdx that is not marked for
/// serialization")` (see `index.rs` lines 154–168).  A `Ref::Local` surviving
/// the seal pass would contain such an index and would cause the `.expect()`
/// below to fire with that error message.  This is the correct behavior: if
/// that assertion fires, it means the seal pass has a bug.
///
/// # Why not a `PristineIntroTable` round-trip?
///
/// `PristineIntroTable` is NOT serializable: its `StoredEntry` wrapper is
/// private (`apply.rs` line 22) and not serde-derived, and the outer
/// `HashMap` iteration order is non-deterministic.  Individual `Entry` values
/// are serializable; we round-trip those instead.
#[test]
fn entries_json_round_trip() {
    let table = build_fixture();

    for (intro, entry) in table.iter() {
        let json = serde_json::to_string(entry).unwrap_or_else(|e| {
            panic!(
                "entry '{}' (intro {}) failed to serialize: {e}\n\
                 If a Ref::Local survived sealing, the EntryIndex serializer \
                 will error here — check that seal()'s pass-3 visitor ran on \
                 this entry.",
                entry.sym().name,
                intro.to_hex()
            )
        });

        let back: nudox_ir::entry::Entry = serde_json::from_str(&json).unwrap_or_else(|e| {
            panic!(
                "entry '{}' (intro {}) failed to deserialize: {e}",
                entry.sym().name,
                intro.to_hex()
            )
        });

        assert_eq!(
            entry,
            &back,
            "entry '{}' (intro {}) did not survive a JSON round-trip — \
             the serde representation is lossy for this entry kind",
            entry.sym().name,
            intro.to_hex()
        );
    }
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Companion: print all fresh values at once (used by UPDATE_GOLDEN path)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// When `UPDATE_GOLDEN=1` is set, this test prints ALL fresh pin values in one
/// consolidated block and then fails, making it easy to copy-paste the entire
/// set at once.  It is a no-op when UPDATE_GOLDEN is not set.
#[test]
fn update_golden_print_all() {
    if std::env::var("UPDATE_GOLDEN").is_err() {
        return;
    }

    let table = build_fixture();
    let all_intros = compute_all_intros(&table);
    let entries_b3 = compute_entries_b3(&table);
    let draws = find_intros_by_name(&table, "draw");
    let impls = find_intros_by_name(&table, "impl");

    println!();
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║  UPDATE_GOLDEN — paste these into the const declarations  ║");
    println!("╠═══════════════════════════════════════════════════════════╣");
    println!();
    println!("// GOLDEN_ALL_INTROS:");
    println!("{all_intros}");
    println!();
    println!("// GOLDEN_ENTRIES_B3:");
    println!("{entries_b3}");
    println!();
    println!("// GOLDEN_DRAW_FIRST:");
    println!(
        "{}",
        draws.first().map(String::as_str).unwrap_or("(missing)")
    );
    println!();
    println!("// GOLDEN_DRAW_SECOND:");
    println!(
        "{}",
        draws.get(1).map(String::as_str).unwrap_or("(missing)")
    );
    println!();
    println!("// GOLDEN_IMPL_FIRST:");
    println!(
        "{}",
        impls.first().map(String::as_str).unwrap_or("(missing)")
    );
    println!();
    println!("// GOLDEN_IMPL_SECOND:");
    println!(
        "{}",
        impls.get(1).map(String::as_str).unwrap_or("(missing)")
    );
    println!();
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();

    panic!(
        "UPDATE_GOLDEN is set — see stdout above for fresh pin values.  \
         Paste them, then re-run without UPDATE_GOLDEN."
    );
}
