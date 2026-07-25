//! Sealing — the two-pass that assigns every arena entry its content-addressed
//! [`IntroId`] and materializes a [`PristineIntroTable`].
//!
//! Build-time entries are addressed by arena-local [`EntryIndex`]; sealing
//! lowers that to the durable, cross-generation [`IntroId`] identity by hashing
//! each declaration's `(package, kind, ancestor-path, leaf-name,
//! disambiguator)`. The disambiguator is selected per §4.3 and — unlike
//! `workspace/ir` — its skeleton payload includes generics/wheres/negativity,
//! so distinct overloads and impls never collide.
//!
//! `seal` consumes the package: sealing moves each [`Entry`] into the table
//! (nudox-ir's `Entry` is intentionally not `Clone`), which is the natural
//! once-per-generation lifecycle.

use std::{collections::HashMap, hash::Hash};

use crate::{
    apply::PristineIntroTable,
    change::{IntroId, PackageLineageId},
    entry::{Entry, EntryInner},
    index::{Ref, UntypedEntryIndex},
    intro::{Disambiguator, bootstrap_intro_id},
    kind::{Kind, KindDiscriminant},
    kinds::{Param, Type},
    skeleton::{function_signature_skeleton, trait_impl_skeleton},
    visitor::Visitor,
};

use super::IrPackage;

/// The base collision key: entries sharing `(kind, ancestor-path, leaf-name)`
/// need a disambiguator to stay distinct.
type BaseKey = (u16, Vec<String>, String);

impl<Id: Eq + Hash> IrPackage<Id> {
    /// Seal this package under `lineage`, minting an [`IntroId`] for every
    /// entry and returning the materialized [`PristineIntroTable`].
    /// Consumes `self`.
    ///
    /// `lineage` is the production package identity (ecosystem + name); it is a
    /// parameter for now — the arena's internal `PackageId` is dev-only and
    /// will be replaced by the lineage id when the registry is rewired.
    pub fn seal(self, lineage: &PackageLineageId) -> PristineIntroTable {
        // Resolution indices over the arena's export-addressed entries.
        let by_idx: HashMap<UntypedEntryIndex, &Entry> =
            self.entries.iter().map(|(idx, e)| (*idx, e)).collect();
        let pos_of: HashMap<UntypedEntryIndex, usize> = self
            .entries
            .iter()
            .enumerate()
            .map(|(i, (idx, _))| (*idx, i))
            .collect();

        let n = self.entries.len();

        // ── Pass 0: ancestor segments, leaf name, kind, parent-idx per entry ──
        let mut segs: Vec<Vec<String>> = Vec::with_capacity(n);
        let mut names: Vec<String> = Vec::with_capacity(n);
        let mut discs: Vec<KindDiscriminant> = Vec::with_capacity(n);
        let mut parent_idxs: Vec<Option<UntypedEntryIndex>> = Vec::with_capacity(n);
        for (_, e) in &self.entries {
            let mut chain = Vec::new();
            let mut cur = e.parent().and_then(|r| r.as_local());
            while let Some(pidx) = cur {
                match by_idx.get(&pidx) {
                    Some(pe) => {
                        chain.push(pe.sym().name.clone());
                        cur = pe.parent().and_then(|r| r.as_local());
                    }
                    None => break,
                }
            }
            chain.reverse();
            segs.push(chain);
            names.push(e.sym().name.clone());
            discs.push(match e.kind() {
                EntryInner::Owned(k) => k.discriminant(),
                // A within-arena forwarding alias is a re-export.
                EntryInner::Reference(_) => KindDiscriminant::Reexport,
            });
            parent_idxs.push(e.parent().and_then(|r| r.as_local()));
        }

        // ── Pass 1: count collisions on the base key ──────────────────────────
        let mut counts: HashMap<BaseKey, u32> = HashMap::new();
        for i in 0..n {
            let key = (discs[i].as_u16(), segs[i].clone(), names[i].clone());
            *counts.entry(key).or_insert(0) += 1;
        }

        // ── Pass 2: select disambiguator + mint IntroId per entry ─────────────
        let mut intros: Vec<IntroId> = Vec::with_capacity(n);
        for i in 0..n {
            let (_, e) = &self.entries[i];
            let count = *counts
                .get(&(discs[i].as_u16(), segs[i].clone(), names[i].clone()))
                .unwrap_or(&1);

            let disamb = match e.kind() {
                // ≥2 functions at the same key: the full signature skeleton.
                EntryInner::Owned(Kind::Function(f)) if count >= 2 => {
                    let inputs = resolve_param_tys(&f.input_params, &by_idx);
                    let outputs = resolve_param_tys(&f.output_params, &by_idx);
                    let skel =
                        function_signature_skeleton(&inputs, &outputs, &f.generics, &f.wheres);
                    Disambiguator::FnOverload(skel.into_boxed_slice())
                }
                // Every impl: the (trait, self, generics, wheres, negative, blanket)
                // skeleton — unconditionally, since impls share the `"impl"` name.
                EntryInner::Owned(Kind::Impl(im)) => {
                    let skel = trait_impl_skeleton(
                        im.of.as_ref(),
                        &im.self_ty,
                        &im.generics,
                        &im.wheres,
                        im.flags.negative,
                        im.flags.blanket,
                    );
                    Disambiguator::TraitImpl(skel.into_boxed_slice())
                }
                // Any other same-key collision: fall back to the source span.
                _ if count >= 2 => {
                    let span = &e.sym().span;
                    Disambiguator::Span {
                        start: span.start,
                        end: span.end,
                    }
                }
                // Unique: signature-stable None.
                _ => Disambiguator::None,
            };

            let seg_refs: Vec<&str> = segs[i].iter().map(String::as_str).collect();
            intros.push(bootstrap_intro_id(
                lineage, discs[i], &seg_refs, &names[i], &disamb,
            ));
        }

        // Resolve parent IntroIds from the captured parent indices (owned data —
        // no borrow of `self.entries`, so pass 3 may consume it).
        let parents: Vec<Option<IntroId>> = parent_idxs
            .iter()
            .map(|opt| opt.and_then(|pidx| pos_of.get(&pidx)).map(|&pi| intros[pi]))
            .collect();

        // Every arena index → its minted IntroId, for lowering in-body refs.
        let intro_of: HashMap<UntypedEntryIndex, IntroId> =
            pos_of.iter().map(|(idx, &i)| (*idx, intros[i])).collect();

        drop(by_idx); // end the borrow of `self.entries` before moving it

        // ── Pass 3: lower every in-body `Local` ref → `Intro`, then move in ───
        // One `visit_mut` per entry rewrites EVERY reference at once — kind-body
        // refs (fields/params/variants), `Type` nominals, and the `Node` tree
        // edges — making the table fully content-addressed (self-contained).
        let mut table = PristineIntroTable::new();
        for (i, (_, mut entry)) in self.entries.into_iter().enumerate() {
            entry.visit_mut(&|r| {
                if let Ref::Local(idx) = r
                    && let Some(&intro) = intro_of.get(idx)
                {
                    *r = Ref::Intro(intro);
                }
            });
            table.insert_live(intros[i], entry, parents[i]);
        }
        table
    }
}

/// Resolve each param handle to its declared type (if any) for the signature
/// skeleton. An unresolvable or type-less param contributes `None`.
fn resolve_param_tys(
    params: &[Ref<Param>],
    by_idx: &HashMap<UntypedEntryIndex, &Entry>,
) -> Vec<Option<Type>> {
    params
        .iter()
        .map(|pref| {
            pref.as_local()
                .and_then(|idx| by_idx.get(&idx.raw()))
                .and_then(|e| match e.kind() {
                    EntryInner::Owned(Kind::Param(p)) => p.ty.clone(),
                    _ => None,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::{
        build::*,
        change::{EcosystemId, PackageName},
        entry::EntryInner,
        kind::Kind,
        test_helpers::{id_gen, sym},
    };

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("demo"))
    }

    /// End-to-end: seal a package and confirm every entry is materialized with
    /// a parent edge that matches the tree.
    #[test]
    fn seal_materializes_the_tree() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            root.create(id(), sym("Point"), |mut rec| {
                let x = rec.create(id(), sym("x"), |_| {
                    Field::builder().key(FieldKey::Named).ty(Type::I32).build()
                });
                Record::builder().fields([x]).build()
            });
        });

        let table = pkg.seal(&lineage());
        // root module + Point record + x field = 3 entries.
        assert_eq!(table.len(), 3);
        let roots = table
            .iter()
            .filter(|(id, _)| table.parent_of(*id).is_none())
            .count();
        assert_eq!(roots, 1, "exactly one root (the module)");
    }

    /// The collision fix, end-to-end: two overloaded functions differing only
    /// by a generic bound must seal to DISTINCT IntroIds (they would
    /// collide under workspace/ir's skeleton).
    #[test]
    fn overloads_seal_to_distinct_intros() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            for bound in [Type::Any, Type::Never] {
                root.create(id(), sym("f"), |_| {
                    Function::builder()
                        .generics([GenericParam::Type {
                            name: "T".to_owned(),
                            bounds: [bound].into(),
                            default: None,
                        }])
                        .build()
                });
            }
        });

        let table = pkg.seal(&lineage());
        // 1 module + 2 functions = 3 unique intros (no collision).
        assert_eq!(table.len(), 3, "no IntroId collision among the overloads");
    }

    /// A unique-named function seals with the signature-stable `None`
    /// disambiguator, so its id is stable across a param-type change.
    #[test]
    fn unique_fn_id_is_signature_stable() {
        let mint = |ret: Type| {
            let mut id = id_gen();
            let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
                root.create(id(), sym("solo"), |mut f| {
                    let p = f.create(id(), sym("out"), |_| Param::builder().ty(ret).build());
                    Function::builder().output_params([p]).build()
                });
            });
            let table = pkg.seal(&lineage());
            table
                .iter()
                .find(|(_, e)| e.sym().name == "solo")
                .map(|(i, _)| i)
                .unwrap()
        };
        assert_eq!(
            mint(Type::I32),
            mint(Type::I64),
            "a unique-named fn keeps its IntroId under a return-type change"
        );
    }

    /// Phase-1 proof: after seal, a record's field reference is LOWERED from an
    /// arena-local index to a content-addressed `Ref::Intro` — the table is
    /// self-contained (one `visit_mut` rewrote every ref).
    #[test]
    fn seal_lowers_in_body_refs_to_intro() {
        let mut id = id_gen();
        let pkg = IrPackage::build(PackageId::path("demo"), sym("root"), |mut root| {
            root.create(id(), sym("Point"), |mut rec| {
                let x = rec.create(id(), sym("x"), |_| {
                    Field::builder().key(FieldKey::Named).ty(Type::I32).build()
                });
                Record::builder().fields([x]).build()
            });
        });

        let table = pkg.seal(&lineage());
        let (_, rec) = table.iter().find(|(_, e)| e.sym().name == "Point").unwrap();

        match rec.kind() {
            EntryInner::Owned(Kind::Record(r)) => {
                assert_eq!(r.fields.len(), 1);
                match &r.fields[0] {
                    Ref::Intro(fi) => {
                        assert!(table.contains(*fi), "field intro resolves in the table");
                        assert_eq!(table.get(*fi).unwrap().sym().name, "x");
                    }
                    other => panic!("field ref must be lowered to Intro, got {other:?}"),
                }
            }
            other => panic!("expected Record, got {other:?}"),
        }
    }
}
