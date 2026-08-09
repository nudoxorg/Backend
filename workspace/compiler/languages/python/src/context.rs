//! The pyrefly semantic tier: real type inference layered over the ruff
//! syntactic front end.
//!
//! # Why this is an enrichment pass, not a second front end
//!
//! pyrefly answers exactly one question the syntactic tier cannot: *what type
//! does this declaration have when the source did not say?* It is markedly
//! worse at everything else — it has no docstrings attached to types (pyrefly's
//! own lowering had to re-parse the AST to get them), it cannot distinguish a
//! `@overload` group from its implementation without the AST, and its export
//! table is a solved-value view that silently folds decorated functions into
//! their wrapper's type.
//!
//! So this module does not replace [`crate::syntax::build_oracle`]; it runs it,
//! then *fills the holes*. The merge is deliberately monotone (see
//! [`merge_type`]): a written annotation always wins over an inferred one, so
//! turning the `pyrefly` feature on can only ever add resolved types, never
//! change one the author wrote. That property is what makes the feature safe to
//! flip on a per-package basis.
//!
//! # What it is worth (measured 2026-08-08, eleven pypi corpus packages)
//!
//! Over 16,025 annotation positions (every `Option<TypeData>` slot the
//! syntactic tier emits; `self`/`cls` excluded, since a receiver is not an
//! annotation), the pyrefly tier gives a resolvable type to **4,607 positions
//! (28.7%)** that otherwise lower to `Type::Any` or to nothing at all. Split by
//! *why* the syntactic tier could not resolve them:
//!
//! | ruff outcome                             | gained | reachable syntactically? |
//! |------------------------------------------|--------|--------------------------|
//! | unannotated (`ty: None`)                 |  2,650 | no, not by any means     |
//! | nominal written unqualified (`-> Group`) |  1,892 | mostly — see below       |
//! | explicit `Any`                           |     65 | (never overwritten)      |
//!
//! The middle row is the *weak* part of the case: matching a bare name against
//! the package's own fully-qualified ids — pure string work, no checker —
//! reaches up to 1,707 of those 1,892. So pyrefly's irreducible contribution is
//! the first row: **2,650 positions (16.5%) that no syntactic front end can
//! ever supply.**
//!
//! It concentrates exactly where it matters most. On the five corpus packages
//! that carry *zero* type annotations (six, pyyaml, python-dateutil, requests,
//! attrs) the syntactic tier resolves 0 of 2,975 positions and this tier
//! resolves 1,528 — 51.4%, from nothing.
//!
//! # Cost
//!
//! Inference runs in-process against pyrefly's bundled typeshed with no
//! network, no interpreter, and no `site-packages`: an `env -i` run with no
//! `HOME` and no `python` on `PATH` produces identical output. It costs
//! 250 ms – 1.1 s per corpus package against the syntactic tier's 0 – 30 ms,
//! and 77 entries in the workspace lock file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nudox_producer::{PackageSource, ProducerError};

use pyrefly::binding::binding::{KeyClassField, KeyExport};
use pyrefly::commands::config_finder::{
    ConfigConfigurer, ConfigConfigurerWrapper, default_config_finder,
};
use pyrefly::state::require::Require;
use pyrefly::state::state::State;
use pyrefly_config::base::{InferReturnTypes, Preset};
use pyrefly_config::config::ConfigFile;
use pyrefly_config::finder::{ConfigError, ConfigFinder};
use pyrefly_util::arc_id::ArcId;
use pyrefly_build::handle::Handle;
use pyrefly_python::module_name::ModuleName;
use pyrefly_python::module_path::ModulePath;
use pyrefly_python::sys_info::SysInfo;
use pyrefly_types::callable::{Callable, Param, Params};
use pyrefly_types::types::{Forallable, Type as PyType};
use pyrefly_util::thread_pool::ThreadCount;

use crate::oracle::{ClassData, FunctionData, ItemBody, ItemData, PythonOracle, TypeData};

/// Which slot of a declaration an inferred type belongs to.
///
/// This exists instead of a bare `String` suffix so the two sides of the join
/// (the pyrefly walk that *writes* keys and the oracle walk that *reads* them)
/// cannot drift apart by a typo — the only way to spell a key is to construct
/// one of these.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Slot {
    /// A callable's return type.
    Return,
    /// A named parameter of a callable.
    Param(String),
    /// A constant, attribute, or alias target.
    Value,
}

/// A fully-qualified declaration id plus the slot within it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SlotKey {
    /// Dotted path, the same scheme `PythonId` uses (`pkg.mod.Cls.method`).
    id: String,
    slot: Slot,
}

impl SlotKey {
    fn ret(id: impl Into<String>) -> Self {
        SlotKey { id: id.into(), slot: Slot::Return }
    }
    fn param(id: impl Into<String>, name: impl Into<String>) -> Self {
        SlotKey { id: id.into(), slot: Slot::Param(name.into()) }
    }
    fn value(id: impl Into<String>) -> Self {
        SlotKey { id: id.into(), slot: Slot::Value }
    }
}

type Inferred = HashMap<SlotKey, TypeData>;

/// Build a [`PythonOracle`] with inferred types filled in.
///
/// The syntactic tier is authoritative for *structure* (which modules exist,
/// which items they declare, docstrings, decorators, overload grouping) and for
/// any type the author actually wrote. pyrefly contributes only to slots the
/// syntactic tier left unresolvable.
///
/// A pyrefly failure is not fatal: the syntactic oracle is returned unchanged,
/// with a warning. That mirrors `syntax.rs`'s own discipline — only a failure
/// to read the package root itself is a hard error, because only that means the
/// producer was handed something it cannot see at all.
pub fn invoke_oracle(src: &PackageSource) -> Result<PythonOracle, ProducerError> {
    let mut oracle = crate::syntax::build_oracle(src)?;
    let inferred = infer(src.root());
    if inferred.is_empty() {
        tracing::warn!(
            root = %src.root().display(),
            "pyrefly produced no solved types; falling back to the syntactic tier alone"
        );
        return Ok(oracle);
    }
    let filled = apply(&mut oracle, &inferred);
    tracing::info!(
        root = %src.root().display(),
        inferred_slots = inferred.len(),
        filled_slots = filled,
        "pyrefly semantic tier applied"
    );
    Ok(oracle)
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Turn pyrefly's inference all the way up.
///
/// **Without this the feature is worth roughly a third of what it should be**,
/// and silently so. A corpus package has no `pyrefly.toml`, so pyrefly treats
/// it as an unconfigured project and applies the `Basic` preset, which sets
/// `check_unannotated_defs = false` and `infer_return_types = Never`. Under
/// that policy a function with no annotations gets an `Any` return *by
/// decision*, not because inference failed — which is the right call for a
/// linter (do not nag about code the author never typed) and precisely wrong
/// for a documentation index, whose entire interest is in the code the author
/// never typed.
///
/// Measured on the eleven pypi corpus packages: 4,607 of 16,025 annotation
/// positions gain a real type with this override, against 3,247 without it.
///
/// `Preset::Off` silences every diagnostic while leaving behaviour flags
/// alone — this producer reads types and has no use for error reporting — and
/// setting `preset` at all is also what stops pyrefly's unconfigured resolver
/// from replacing the whole config (and these two flags with it).
fn inference_config_finder() -> ConfigFinder {
    struct InferEverything;

    impl ConfigConfigurer for InferEverything {
        fn configure(
            &self,
            _root: Option<&Path>,
            mut config: ConfigFile,
            _errors: Vec<ConfigError>,
        ) -> (ArcId<ConfigFile>, Vec<ConfigError>) {
            config.preset = Some(Preset::Off);
            config.root.check_unannotated_defs = Some(true);
            config.root.infer_return_types = Some(InferReturnTypes::Checked);
            config.configure();
            (ArcId::new(config), Vec::new())
        }
    }

    let wrapper: ConfigConfigurerWrapper = Arc::new(|_inner| Arc::new(InferEverything));
    default_config_finder(Some(wrapper))
}

// ---------------------------------------------------------------------------
// pyrefly side: solve the package, harvest declaration types
// ---------------------------------------------------------------------------

/// Type-check every module under `root` in one committed transaction and
/// harvest the solved type of every export and class attribute.
///
/// The single committed transaction is load-bearing: running the modules
/// together is what lets `from .core import Context` resolve across files, and
/// *committing* is what makes the solved answers survive into the read
/// transaction below. A throwaway transaction drops its updated modules on
/// `Drop`, leaving an `Any`-only view — which looks exactly like success.
fn infer(root: &Path) -> Inferred {
    let sys_info = SysInfo::default();
    let handles: Vec<(String, Handle)> = discover(root)
        .into_iter()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, path)| {
            let handle = Handle::new(
                ModuleName::from_str(&name),
                ModulePath::filesystem(path),
                sys_info,
            );
            (name, handle)
        })
        .collect();

    let mut out = Inferred::new();
    if handles.is_empty() {
        return out;
    }

    let state = State::new(inference_config_finder(), ThreadCount::default());
    let all: Vec<Handle> = handles.iter().map(|(_, h)| h.clone()).collect();
    let mut tx = state.new_committable_transaction(Require::Everything, None);
    tx.as_mut().run(&all, Require::Everything, None);
    state.commit_transaction(tx, None);

    let read = state.transaction();
    for (module, handle) in &handles {
        let (Some(bindings), Some(answers)) = (read.get_bindings(handle), read.get_answers(handle))
        else {
            continue;
        };
        for idx in bindings.keys::<KeyExport>() {
            let name = bindings.idx_to_key(idx).0.to_string();
            let Some(ty) = answers.get_idx(idx) else {
                continue;
            };
            let id = format!("{module}.{name}");
            record(&mut out, &id, &ty);

            let PyType::ClassDef(class) = &*ty else {
                continue;
            };
            let Some(fields) = bindings.get_class_fields(class.index()) else {
                continue;
            };
            for field in fields.fields() {
                let fidx = bindings.key_to_idx(&KeyClassField(class.index(), Clone::clone(field)));
                let Some(solved) = answers.get_idx(fidx) else {
                    continue;
                };
                record(&mut out, &format!("{id}.{field}"), &solved.ty());
            }
        }
    }
    out
}

/// Record every annotation slot a single declaration owns.
fn record(out: &mut Inferred, id: &str, ty: &PyType) {
    let Some(callable) = callable_of(ty) else {
        out.insert(SlotKey::value(id), convert(ty));
        return;
    };
    out.insert(SlotKey::ret(id), convert(&callable.ret));
    let (Params::List(list) | Params::Partial(list)) = &callable.params else {
        return;
    };
    for param in list.items() {
        let (name, ty) = match param {
            Param::PosOnly(n, t, _) => (n.as_ref().map(|n| n.to_string()), t),
            Param::Pos(n, t, _) => (Some(n.to_string()), t),
            Param::Varargs(n, t) => (n.as_ref().map(|n| n.to_string()), t),
            Param::KwOnly(n, t, _) => (Some(n.to_string()), t),
            Param::Kwargs(n, t) => (n.as_ref().map(|n| n.to_string()), t),
        };
        if let Some(name) = name {
            out.insert(SlotKey::param(id, name), convert(ty));
        }
    }
}

/// The callable signature behind a solved type, unwrapping the quantification
/// and binding wrappers pyrefly puts around one.
///
/// `None` means the declaration is not callable, i.e. it is a value and its
/// solved type *is* its annotation.
fn callable_of(ty: &PyType) -> Option<Callable> {
    match ty {
        PyType::Function(f) => Some(f.signature.clone()),
        PyType::Callable(c) => Some((**c).clone()),
        PyType::BoundMethod(bm) => callable_of(&bm.func.clone().as_type()),
        PyType::Forall(fa) => match &fa.body {
            Forallable::Function(f) => Some(f.signature.clone()),
            Forallable::Callable(c) => Some(c.clone()),
            _ => None,
        },
        // An overload group's slots are keyed by the base id; the syntactic
        // tier already split the branches, and only the first signature can be
        // attributed without re-deriving that split, so take it.
        PyType::Overload(o) => callable_of(&o.signatures.first().clone().as_type()),
        _ => None,
    }
}

/// Strip pyrefly's `@line:col-col` location suffix from a qualified name.
///
/// A Python dotted name never contains `@`, so truncating there drops only the
/// location. `types.rs::lower_nominal` does the same on the way out; doing it
/// here too keeps the ids comparable to the syntactic tier's.
fn strip_loc(name: &str) -> &str {
    match name.find('@') {
        Some(at) => &name[..at],
        None => name,
    }
}

/// Convert a solved pyrefly type into the producer's owned [`TypeData`].
///
/// Anything with no `TypeData` counterpart degrades to `Nominal` carrying
/// pyrefly's own spelling rather than to `Any`, so the link phase still has a
/// name to work with instead of the collapse this whole module exists to undo.
fn convert(ty: &PyType) -> TypeData {
    match ty {
        PyType::Any(_) => TypeData::Any,
        PyType::Never(_) => TypeData::Never,
        PyType::None => TypeData::NoneType,
        PyType::Union(u) => TypeData::Union(u.members.iter().map(convert).collect()),
        PyType::ClassType(ct) => {
            let base = TypeData::Nominal(strip_loc(&format!("{}", ct.qname())).to_owned());
            let args: Vec<TypeData> = ct.targs().as_slice().iter().map(convert).collect();
            if args.is_empty() {
                base
            } else {
                TypeData::Apply { base: Box::new(base), args }
            }
        }
        PyType::ClassDef(c) => TypeData::Apply {
            base: Box::new(TypeData::Nominal("typing.Type".to_owned())),
            args: vec![TypeData::Nominal(strip_loc(&format!("{}", c.qname())).to_owned())],
        },
        PyType::Type(inner) => TypeData::Apply {
            base: Box::new(TypeData::Nominal("typing.Type".to_owned())),
            args: vec![convert(inner)],
        },
        PyType::Tuple(_) => TypeData::Nominal("builtins.tuple".to_owned()),
        PyType::TypedDict(td) | PyType::PartialTypedDict(td) => {
            TypeData::Nominal(strip_loc(&format!("{}", td.name())).to_owned())
        }
        PyType::Literal(lit) => TypeData::Nominal(literal_runtime_class(&lit.value)),
        PyType::LiteralString(_) => TypeData::Nominal("builtins.str".to_owned()),
        PyType::TypeVar(tv) => TypeData::TypeVar(tv.qname().id().to_string()),
        PyType::ParamSpec(p) => TypeData::TypeVar(p.qname().id().to_string()),
        PyType::TypeVarTuple(t) => TypeData::TypeVar(t.qname().id().to_string()),
        PyType::Quantified(q) | PyType::QuantifiedValue(q) => TypeData::TypeVar(q.name().to_string()),
        PyType::SelfType(_) => TypeData::SelfType,
        PyType::Module(m) => TypeData::Nominal(format!("{m}")),
        PyType::Forall(_)
        | PyType::Function(_)
        | PyType::Callable(_)
        | PyType::Overload(_)
        | PyType::BoundMethod(_) => TypeData::Nominal("typing.Callable".to_owned()),
        PyType::Annotated(inner, meta) => TypeData::Annotated {
            inner: Box::new(convert(inner)),
            metadata: meta.iter().map(|m| format!("{m}")).collect(),
        },
        PyType::Unpack(inner) => convert(inner),
        other => TypeData::Nominal(strip_loc(&format!("{other}")).to_owned()),
    }
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

/// Fill unresolvable slots in `oracle` from `inferred`; returns how many were
/// filled.
fn apply(oracle: &mut PythonOracle, inferred: &Inferred) -> usize {
    let mut filled = 0;
    for module in &mut oracle.modules {
        let items = std::mem::take(&mut module.items);
        module.items = items
            .into_iter()
            .map(|item| apply_item(item, inferred, &mut filled))
            .collect();
    }
    filled
}

fn apply_item(mut item: ItemData, inferred: &Inferred, filled: &mut usize) -> ItemData {
    let id = item.id.0.clone();
    item.body = match item.body {
        ItemBody::Module => ItemBody::Module,
        ItemBody::Function(f) => ItemBody::Function(apply_fn(&id, f, inferred, filled)),
        ItemBody::Overloaded(fs) => ItemBody::Overloaded(
            fs.into_iter().map(|f| apply_fn(&id, f, inferred, filled)).collect(),
        ),
        ItemBody::Const(mut c) => {
            merge_type(&mut c.ty, inferred.get(&SlotKey::value(&id)), filled);
            ItemBody::Const(c)
        }
        ItemBody::Alias(mut a) => {
            merge_type(&mut a.target, inferred.get(&SlotKey::value(&id)), filled);
            ItemBody::Alias(a)
        }
        ItemBody::Class(c) => ItemBody::Class(apply_class(&id, c, inferred, filled)),
    };
    item
}

fn apply_class(id: &str, mut c: ClassData, inferred: &Inferred, filled: &mut usize) -> ClassData {
    for field in &mut c.fields {
        let key = SlotKey::value(format!("{id}.{}", field.name));
        merge_type(&mut field.ty, inferred.get(&key), filled);
    }
    c.methods = c.methods.into_iter().map(|m| apply_item(m, inferred, filled)).collect();
    c.nested = c.nested.into_iter().map(|n| apply_item(n, inferred, filled)).collect();
    c
}

fn apply_fn(id: &str, mut f: FunctionData, inferred: &Inferred, filled: &mut usize) -> FunctionData {
    merge_type(&mut f.return_ty, inferred.get(&SlotKey::ret(id)), filled);
    for param in &mut f.params {
        let key = SlotKey::param(id, param.name.clone());
        merge_type(&mut param.ty, inferred.get(&key), filled);
    }
    f
}

/// The merge rule, in one place: **a written annotation always wins.**
///
/// Two cases let inference through, and only two:
///
/// 1. The slot is empty. The author wrote nothing, so there is nothing to
///    override; anything but `Any` is an improvement.
/// 2. The slot holds a bare, unqualified nominal (`-> Group`, `-> "Context"`)
///    and inference resolved *the same name* to a qualified one. This is a
///    qualification fix, not a retype: `types.rs::lower_nominal` matches
///    `known_ids` on fully-qualified strings, so an unqualified name can never
///    resolve and always collapses to `Type::Any`. Requiring the short names to
///    agree is what keeps this from silently substituting a different type.
///
/// Everything else — including an explicit `Any`, which is a statement of
/// intent and not a gap — is left exactly as the author wrote it. That makes
/// the feature monotone: enabling it cannot change an existing lowering, only
/// add to it.
fn merge_type(slot: &mut Option<TypeData>, inferred: Option<&TypeData>, filled: &mut usize) {
    let Some(inferred) = inferred else { return };
    if matches!(inferred, TypeData::Any) {
        return;
    }
    match slot {
        None => {
            *slot = Some(inferred.clone());
            *filled += 1;
        }
        Some(TypeData::Nominal(written)) if !written.contains('.') => {
            if principal_short_name(inferred).as_deref() == Some(written.as_str()) {
                *slot = Some(inferred.clone());
                *filled += 1;
            }
        }
        Some(_) => {}
    }
}

/// The last dotted segment of a type's head nominal, if it has one.
///
/// `click.core.Group` → `Group`; `dict[str, Context]` → `dict`. Used only to
/// confirm that a qualification fix is talking about the same name.
fn principal_short_name(ty: &TypeData) -> Option<String> {
    match ty {
        TypeData::Nominal(n) => {
            Some(strip_loc(n).rsplit('.').next().unwrap_or(n).to_owned())
        }
        TypeData::Apply { base, .. } => principal_short_name(base),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Module discovery
// ---------------------------------------------------------------------------

/// Discover `(dotted module name, file)` pairs under `root`.
///
/// This must agree with `syntax.rs`'s own file walk and `module_dotted_name`,
/// because the dotted name is the *join key* between the two tiers: a
/// disagreement silently degrades this module to a no-op rather than failing.
/// `module_names_agree_with_the_syntactic_tier` below is what holds them
/// together — it is the reason this duplication is safe rather than merely
/// convenient.
fn discover(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if path.is_dir() {
                if is_excluded_dir(&name) {
                    continue;
                }
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("py") {
                out.push((module_dotted_name(&path), path));
            }
        }
    }
    out.sort();
    out
}

fn is_excluded_dir(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "__pycache__"
            | ".tox"
            | ".nox"
            | ".venv"
            | "venv"
            | "node_modules"
            | "build"
            | "dist"
            | ".mypy_cache"
            | ".pytest_cache"
            | ".ruff_cache"
            | ".eggs"
    ) || name.ends_with(".egg-info")
}

/// A directory contributes a dotted segment iff it contains `__init__.py` —
/// Python's own definition of "this directory is a package". Climbing stops at
/// the first ancestor without one, which is the sdist's import root whatever it
/// is called (`src/`, `lib/`, or the sdist root itself).
fn module_dotted_name(file: &Path) -> String {
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut segments: Vec<String> = Vec::new();
    if stem != "__init__" && !stem.is_empty() {
        segments.push(stem.to_owned());
    }
    let mut cur = file.parent().map(Path::to_path_buf);
    while let Some(dir) = cur {
        if !dir.join("__init__.py").is_file() {
            break;
        }
        let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
            break;
        };
        segments.push(name.to_owned());
        cur = dir.parent().map(Path::to_path_buf);
    }
    segments.reverse();
    segments.join(".")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg = dir.path().join("shapes");
        let sub = pkg.join("plane");
        std::fs::create_dir_all(&sub).expect("mkdir");
        std::fs::write(pkg.join("__init__.py"), "from .plane.point import Point\n").unwrap();
        std::fs::write(sub.join("__init__.py"), "").unwrap();
        std::fs::write(
            sub.join("point.py"),
            "class Point:\n\
             \x20   def __init__(self, x, y):\n\
             \x20       self.x = x\n\
             \x20       self.y = y\n\n\
             \x20   def label(self):\n\
             \x20       return \"point\"\n\n\
             ORIGIN = Point(0, 0)\n\n\
             def midpoint(a, b):\n\
             \x20   return Point(0, 0)\n",
        )
        .unwrap();
        dir
    }

    /// The two tiers join on the dotted module name. If this module's walk and
    /// `syntax.rs`'s ever disagree, every lookup misses and the feature turns
    /// into an expensive no-op that still *looks* like it worked — so assert
    /// the agreement directly rather than trusting two copies of a rule.
    #[test]
    fn module_names_agree_with_the_syntactic_tier() {
        let dir = fixture();
        let src = PackageSource::new(dir.path(), "shapes", "0.1.0");
        let oracle = crate::syntax::build_oracle(&src).expect("syntactic oracle");

        let mut syntactic: Vec<String> = oracle.modules.iter().map(|m| m.name.clone()).collect();
        let mut semantic: Vec<String> = discover(dir.path())
            .into_iter()
            .map(|(name, _)| name)
            .filter(|n| !n.is_empty())
            .collect();
        syntactic.sort();
        semantic.sort();
        semantic.dedup();

        assert_eq!(
            syntactic, semantic,
            "the semantic tier must discover exactly the modules the syntactic tier names"
        );
        assert!(
            syntactic.contains(&"shapes.plane.point".to_owned()),
            "the dotted name must climb `__init__.py` ancestors; got {syntactic:?}"
        );
    }

    /// The whole point of the tier: a function with no annotations anywhere
    /// must come back carrying the type pyrefly solved for it. `midpoint` has
    /// no `->` and returns a locally-defined class, so a syntactic front end
    /// can only ever say "nothing here"; the assertion is on the resolved
    /// *name*, which is what `lower_nominal` needs to mint a real Ref.
    #[test]
    fn an_unannotated_return_gains_the_inferred_class() {
        let dir = fixture();
        let src = PackageSource::new(dir.path(), "shapes", "0.1.0");

        let bare = crate::syntax::build_oracle(&src).expect("syntactic oracle");
        assert!(
            find_return(&bare, "shapes.plane.point.midpoint").is_none(),
            "precondition: the syntactic tier cannot know this return type"
        );

        let enriched = invoke_oracle(&src).expect("pyrefly oracle");
        let ret = find_return(&enriched, "shapes.plane.point.midpoint")
            .expect("the pyrefly tier must supply a return type where the source wrote none");
        assert_eq!(
            principal_short_name(&ret).as_deref(),
            Some("Point"),
            "expected the locally-defined class, got {ret:?}"
        );
    }

    /// Monotonicity: a written annotation is never replaced. `label` returns a
    /// `str` by inference; if the source had said otherwise the source would
    /// win. Here the check is the weaker but load-bearing half — enabling the
    /// feature must not drop or corrupt anything the syntactic tier already
    /// produced.
    #[test]
    fn enrichment_never_removes_a_syntactic_declaration() {
        let dir = fixture();
        let src = PackageSource::new(dir.path(), "shapes", "0.1.0");
        let bare = crate::syntax::build_oracle(&src).expect("syntactic oracle");
        let enriched = invoke_oracle(&src).expect("pyrefly oracle");

        let names = |o: &PythonOracle| {
            let mut v: Vec<String> = Vec::new();
            for m in &o.modules {
                v.push(m.name.clone());
                for i in &m.items {
                    v.push(i.id.0.clone());
                }
            }
            v.sort();
            v
        };
        assert_eq!(
            names(&bare),
            names(&enriched),
            "the semantic tier may only add types, never change the declaration set"
        );
    }

    fn find_return(oracle: &PythonOracle, id: &str) -> Option<TypeData> {
        for m in &oracle.modules {
            for item in &m.items {
                if item.id.0 == id
                    && let ItemBody::Function(f) = &item.body
                {
                    return f.return_ty.clone();
                }
            }
        }
        None
    }
}

/// The runtime class behind a literal type.
///
/// `Literal['x']` is a `str` as far as a reader of documentation is concerned;
/// keeping the literal spelling would put a value where a type belongs.
fn literal_runtime_class(lit: &pyrefly_types::literal::Lit) -> String {
    use pyrefly_types::literal::Lit;
    match lit {
        Lit::Str(_) => "builtins.str".to_owned(),
        Lit::Int(_) => "builtins.int".to_owned(),
        Lit::Bool(_) => "builtins.bool".to_owned(),
        Lit::Bytes(_) => "builtins.bytes".to_owned(),
        Lit::Enum(e) => strip_loc(&format!("{}", e.class.qname())).to_owned(),
    }
}
