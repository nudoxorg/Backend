//! [`CorpusResolver`] — the corpus-side cross-package LINK pass.
//!
//! # Why this exists
//!
//! Packages are produced and sealed in isolation, each one fed
//! [`nudox_ir::foreign::Unlinked`] as its [`ForeignResolver`] because no other
//! package's identity is available yet at that moment. Every cross-package
//! reference therefore survives sealing as a *named-but-unlinked*
//! `Ref::Foreign { key, target: None }`: the producer recorded everything it
//! could state about the target (`ForeignKey::path`, `origin`, `kind`) but
//! nothing ever turned that into a `StableRef`, because the answer depends on
//! a sibling package's own collision counts — information only that sibling's
//! own seal could produce (see `ir/model/src/foreign.rs`'s module docs).
//!
//! `CorpusResolver` is the second half: once the corpus has *loaded* the
//! sibling, its sealed declaration table is exactly what a `ForeignKey` needs
//! to be joined against. This module does not re-run sealing or mutate any
//! package — it is a read-only, sync view over whatever is already in the
//! corpus, handed to whoever wants to re-run `seal` with linking, or to a
//! query-time renderer that wants to resolve a dangling ref on demand.
//!
//! # Bridging sync and async
//!
//! [`ForeignResolver::resolve`] is a synchronous trait method (`seal` is sync
//! and single-threaded per package), but [`Corpus`] guards its package map
//! behind a `tokio::sync::RwLock`. [`Corpus::foreign_resolver`] takes the
//! async read lock exactly once, clones the `BTreeMap<PackageLineageId,
//! Arc<PackageView>>` (an O(packages) pointer-clone, not a deep copy — each
//! value is an `Arc`), and hands back a `CorpusResolver` that owns that
//! snapshot. Every `resolve` call after that is pure, lock-free, and
//! `.await`-free: a point-in-time view of the corpus at the moment
//! `foreign_resolver()` was called, exactly like [`Corpus::packages`] already
//! promises for its own callers.
//!
//! # Matching a `ForeignKey` against a sealed table
//!
//! A [`ForeignKey::path`] is written in the *target* ecosystem's own spelling
//! (`core::clone::Clone` for cargo, `java.util.List` for maven), so the join
//! key on this side must be styled the same way. This uses
//! [`monikers_styled`] — never [`nudox_ir::reflect::monikers`] or
//! [`PackageIndexes::path_of`](crate::store::package::PackageIndexes::path_of),
//! both of which are permanently dot-joined for reasons unrelated to this
//! resolver (see `reflect.rs`'s `PathStyle` docs) and would silently fail to
//! match every `::`-styled key a cargo or cpp producer emits.
//!
//! # Ambiguity
//!
//! Rust (and other ecosystems) keep separate type/value namespaces, so a
//! `trait Foo` and a `fn Foo` in the same module share one styled moniker
//! path. A resolver that returned the first match found would sometimes link
//! to the wrong one — a wrong link renders as a working hyperlink to the
//! wrong symbol, which [`Resolution::Ambiguous`]'s doc comment calls "worse
//! than no link". This resolver collects every match, and only picks a
//! winner when `ForeignKey::kind` narrows the set to exactly one.

use std::collections::BTreeMap;
use std::sync::Arc;

use nudox_ir::{
    change::{IntroId, PackageLineageId, StableRef},
    foreign::{ForeignKey, ForeignResolver, Resolution},
    reflect::{ExportPolicy, PathStyle, monikers_styled},
};

use crate::store::package::PackageView;

/// A point-in-time, sync snapshot of a [`Corpus`](crate::store::corpus::Corpus)
/// that can answer [`ForeignResolver::resolve`].
///
/// Built by [`Corpus::foreign_resolver`](crate::store::corpus::Corpus::foreign_resolver).
/// See the module docs for why this exists as a separate, owned snapshot
/// rather than a resolver that borrows the corpus directly.
#[derive(Debug, Default)]
pub struct CorpusResolver {
    packages: BTreeMap<PackageLineageId, Arc<PackageView>>,
}

impl CorpusResolver {
    /// Build a resolver from an already-collected package snapshot.
    ///
    /// `pub(crate)`: the only supported way to obtain one is
    /// [`Corpus::foreign_resolver`](crate::store::corpus::Corpus::foreign_resolver),
    /// which is what keeps "a resolver's view of the world" and "the corpus's
    /// view of the world" from being constructed inconsistently by two
    /// different call sites.
    pub(crate) fn from_snapshot(packages: BTreeMap<PackageLineageId, Arc<PackageView>>) -> Self {
        Self { packages }
    }
}

impl ForeignResolver for CorpusResolver {
    fn resolve(&self, key: &ForeignKey) -> Resolution {
        // Only a `Package` origin can ever link — a `Namespace` or `Universe`
        // origin means the producer itself could not name an owning package,
        // so there is nothing for this resolver to look up. Mirrors
        // `Unlinked`'s rule exactly (see `ir/model/src/foreign.rs`).
        let Some(lineage) = key.origin.lineage() else {
            return Resolution::Unplaceable;
        };

        let Some(package) = self.packages.get(lineage) else {
            return Resolution::PackageNotLoaded;
        };

        let style = PathStyle::for_ecosystem(key.origin.ecosystem().as_str());
        let table = package.view().table();

        // Collect every export whose styled moniker path matches the key's
        // path — not just the first — so that a same-path collision across
        // Rust's type/value namespaces (`trait Foo` + `fn Foo`) is visible as
        // an ambiguity instead of silently resolved to whichever happened to
        // iterate first.
        let matches: Vec<IntroId> = monikers_styled(table, ExportPolicy::PublicOnly, style)
            .filter(|(path, _intro)| path.as_str() == key.path.as_ref())
            .map(|(_path, intro)| intro)
            .collect();

        match matches.len() {
            0 => Resolution::PathNotFound {
                package: lineage.clone(),
            },
            1 => Resolution::Resolved(StableRef::new(lineage.clone(), matches[0])),
            _ => {
                // More than one export shares this styled path. Only the
                // key's stated `kind` may break the tie — never iteration
                // order, which is exactly the wrong-link hazard
                // `Resolution::Ambiguous`'s doc comment warns about.
                if let Some(wanted_kind) = key.kind {
                    let narrowed: Vec<IntroId> = matches
                        .iter()
                        .copied()
                        .filter(|&intro| {
                            table
                                .get(intro)
                                .and_then(|entry| entry.kind().discriminant())
                                == Some(wanted_kind)
                        })
                        .collect();
                    if narrowed.len() == 1 {
                        return Resolution::Resolved(StableRef::new(lineage.clone(), narrowed[0]));
                    }
                }
                Resolution::Ambiguous {
                    candidates: matches.len(),
                }
            }
        }
    }
}
