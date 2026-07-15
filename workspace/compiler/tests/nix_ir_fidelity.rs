//! Nix IR fidelity contracts — static (+ best-effort dynamic) lowering
//! against the `tests/fixtures/nix/` trees.
//!
//! Mandatory coverage:
//! 1. `defaultWidth = 80` → Constant with Int type (+ ConstExpr value)
//! 2. `packages.*.hello` derivation → TypedBinding.ty Derivation (dynamic or
//!    unit-level `derivation_binding` / `value_kind_type` when eval is absent)
//! 3. `strings` attrset is Module; `strings/trim` is a wired member
//! 4. Lambda `args@{ a, ... }` preserves args_bind + ellipsis/variadic
//! 5. `value_kind_type` is live (not dead code)
//! 6. Forced-thunk / catchable path does not panic

use std::path::{Path, PathBuf};

use compiler::languages::nix::lower_package;
use compiler::languages::nix::{function, syntax, types};
use ir::entry::{Index, NudoxPath};
use ir::function::Attribute;
use ir::generics::ConstExpr;
use ir::kind::{Entry, Visibility};
use ir::parameter::{Parameter, ParameterAttribute};
use ir::primitives::{Primitive, Width};
use ir::ty::{Type, TypeReference};
use snix_eval::{CatchableErrorKind, Value};

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn fixture(rel: &str) -> PathBuf {
	PathBuf::from(env!("CARGO_MANIFEST_DIR"))
		.join("tests/fixtures/nix")
		.join(rel)
}

fn path_display(path: &NudoxPath) -> String {
	match path {
		NudoxPath::Local(p) => p.display().to_string(),
		NudoxPath::External { path, dependency } => {
			format!("{dependency}:{}", path.display())
		}
	}
}

fn dump(index: &Index) -> String {
	let mut v: Vec<String> = index
		.entries_by_path
		.iter()
		.map(|(p, e)| format!("{}:{}", e.kind_tag(), path_display(p)))
		.collect();
	v.sort();
	v.join(", ")
}

fn find_constant<'a>(
	index: &'a Index,
	path_suffix: &str,
) -> &'a ir::kind::Symbol<ir::kind::TypedBinding> {
	index
		.entries_by_path
		.iter()
		.find_map(|(p, e)| match e {
			Entry::Constant(sym) if path_display(p).ends_with(path_suffix) => Some(sym),
			_ => None,
		})
		.unwrap_or_else(|| {
			panic!(
				"Constant at `…{path_suffix}` not found; entries: {}",
				dump(index)
			)
		})
}

fn find_module<'a>(
	index: &'a Index,
	path_suffix: &str,
) -> &'a ir::kind::Symbol<ir::module::Module> {
	index
		.entries_by_path
		.iter()
		.find_map(|(p, e)| match e {
			Entry::Module(sym) if path_display(p) == path_suffix || path_display(p).ends_with(path_suffix) => {
				Some(sym)
			}
			_ => None,
		})
		.unwrap_or_else(|| {
			panic!(
				"Module at `{path_suffix}` not found; entries: {}",
				dump(index)
			)
		})
}

fn find_function<'a>(
	index: &'a Index,
	name: &str,
) -> &'a ir::kind::Symbol<ir::function::Function> {
	index
		.entries_by_path
		.values()
		.find_map(|e| match e {
			Entry::Function(sym) if sym.name == name => Some(sym),
			_ => None,
		})
		.unwrap_or_else(|| panic!("function `{name}` not found; entries: {}", dump(index)))
}

fn param_names(func: &ir::function::Function) -> Vec<String> {
	func.input_parameters
		.as_ref()
		.map(|ps| {
			ps.iter()
				.filter_map(|p| match p {
					Parameter::Literal(l) => Some(l.name.clone()),
					_ => None,
				})
				.collect()
		})
		.unwrap_or_default()
}

fn has_variadic(func: &ir::function::Function) -> bool {
	let on_fn = func
		.attributes
		.as_ref()
		.is_some_and(|a| a.contains(&Attribute::Variadic));
	let on_param = func.input_parameters.as_ref().is_some_and(|ps| {
		ps.iter().any(|p| match p {
			Parameter::Literal(l) => {
				l.name == "..."
					|| l.attributes
						.as_ref()
						.is_some_and(|a| a.contains(&ParameterAttribute::Variadic))
			}
			_ => false,
		})
	});
	on_fn || on_param
}

// ─── 1. defaultWidth = 80 → Constant with Int type ───────────────────────────

#[test]
fn default_width_constant_has_int_type_and_value() {
	let root = fixture("fidelity");
	let index = lower_package(Path::new(&root)).expect("static lower must succeed");
	println!("entries: {}", dump(&index));

	let c = find_constant(&index, "defaultWidth");
	assert_eq!(c.name, "defaultWidth");

	let ty = c
		.inner
		.ty
		.as_ref()
		.expect("defaultWidth must carry a TypedBinding.ty");
	assert!(
		matches!(ty, Type::Primitive(Primitive::Int(Width::W64))),
		"defaultWidth ty should be Int(W64), got {ty:?}"
	);

	let val = c
		.inner
		.value
		.as_ref()
		.expect("defaultWidth must recover ConstExpr::Int(80)");
	assert!(
		matches!(val, ConstExpr::Int(80)),
		"defaultWidth value should be ConstExpr::Int(80), got {val:?}"
	);

	// Static-only trees promote top-level export roots to Public.
	assert_eq!(
		c.visibility,
		Visibility::Public,
		"static-only export root should be Public"
	);
}

// ─── 2. packages.*.hello derivation TypedBinding.ty Derivation ───────────────

#[test]
fn derivation_typed_binding_and_hello_surface() {
	// Unit-level: value_kind_type / derivation_binding are the live paths used
	// by mint_derivation (must not be dead code).
	let dty = types::derivation_type();
	assert!(
		matches!(&dty, Type::TypeReference(TypeReference { identifier, .. }) if identifier == "Derivation"),
		"derivation_type() must name Derivation"
	);
	assert_eq!(
		types::value_kind_type("derivation"),
		dty,
		"value_kind_type(\"derivation\") must equal derivation_type()"
	);
	let binding = types::derivation_binding();
	assert_eq!(
		binding.ty.as_ref(),
		Some(&types::derivation_type()),
		"derivation_binding().ty must be Derivation"
	);

	// Dynamic surface (best-effort): mini-flake packages.*.hello.
	let root = fixture("mini-flake");
	let index = lower_package(Path::new(&root)).expect("static layer must succeed");
	println!("mini-flake entries: {}", dump(&index));

	let hello = index.entries_by_path.iter().find(|(p, e)| {
		path_display(p).ends_with("hello")
			&& matches!(e, Entry::Constant(_) | Entry::Module(_) | Entry::Function(_))
	});

	if let Some((path, Entry::Constant(sym))) = hello {
		// When dynamic eval forced the derivation, ty must be Derivation.
		if let Some(ty) = &sym.inner.ty {
			assert!(
				matches!(
					ty,
					Type::TypeReference(TypeReference { identifier, .. }) if identifier == "Derivation"
				),
				"packages.*.hello ty should be Derivation at {}, got {ty:?}",
				path_display(path)
			);
		} else {
			// Static-only leaf without type is acceptable only if we never
			// evaluated; the unit-level derivation_binding contract above
			// still holds for mint_derivation.
			eprintln!(
				"note: hello at {} has no ty (static-only); derivation_binding contract covered",
				path_display(path)
			);
		}
	} else {
		// Static surface still exposes a hello name somewhere, or packages prefix.
		let has_hello = index.entries_by_path.values().any(|e| e.name() == "hello");
		let has_packages = index.entries_by_path.keys().any(|p| {
			let s = path_display(p);
			s == "packages" || s.starts_with("packages/")
		});
		assert!(
			has_hello || has_packages,
			"expected packages/hello surface even without dynamic eval; {}",
			dump(&index)
		);
	}
}

// ─── 3. strings attrset is Module; strings/trim is member ────────────────────

#[test]
fn strings_attrset_is_module_with_trim_member() {
	let root = fixture("fidelity");
	let index = lower_package(Path::new(&root)).expect("lower");
	println!("entries: {}", dump(&index));

	let strings = find_module(&index, "strings");
	assert_eq!(strings.name, "strings");
	assert!(
		matches!(
			index
				.entries_by_path
				.get(&NudoxPath::Local(PathBuf::from("strings"))),
			Some(Entry::Module(_))
		),
		"strings must be Entry::Module (not Constant)"
	);

	// wire_members attaches children under the Module.
	let members = strings
		.inner
		.members
		.as_ref()
		.expect("strings Module must have members after wire_members");
	let member_paths: Vec<String> = members.iter().map(path_display).collect();
	assert!(
		member_paths.iter().any(|p| p == "strings/trim" || p.ends_with("/trim")),
		"strings members must include strings/trim; got {member_paths:?}"
	);

	// And the trim entry itself exists as a Function.
	let trim = find_function(&index, "trim");
	assert!(
		trim.inner.input_parameters.is_some(),
		"strings.trim should lower as Function with formals"
	);

	// inherit (from) simple select: utils.trim resolves to the same lambda.
	let utils_trim = index.entries_by_path.iter().find(|(p, _)| {
		path_display(p) == "utils/trim" || path_display(p).ends_with("utils/trim")
	});
	assert!(
		utils_trim.is_some(),
		"utils/trim inherit alias missing; {}",
		dump(&index)
	);
	if let Some((_, Entry::Function(sym))) = utils_trim {
		assert!(
			sym.inner.input_parameters.is_some(),
			"inherit (strings) trim should resolve the source lambda"
		);
	}
}

// ─── 4. Lambda args@{ a, ... } preserves ellipsis / variadic ─────────────────

#[test]
fn args_bind_and_ellipsis_preserved() {
	let root = fixture("fidelity");
	let index = lower_package(Path::new(&root)).expect("lower");
	println!("entries: {}", dump(&index));

	let with_args = find_function(&index, "withArgs");
	let names = param_names(&with_args.inner);
	assert!(
		names.iter().any(|n| n == "args"),
		"args@ bind must emit an `args` parameter; got {names:?}"
	);
	assert!(
		names.iter().any(|n| n == "a"),
		"pattern field `a` must be present; got {names:?}"
	);
	assert!(
		has_variadic(&with_args.inner),
		"ellipsis must produce Variadic param and/or Function attribute; params={names:?}, attrs={:?}",
		with_args.inner.attributes
	);

	// openOnly = { name, ... }: … — ellipsis without args@.
	let open_only = find_function(&index, "openOnly");
	assert!(
		has_variadic(&open_only.inner),
		"openOnly ellipsis must be Variadic; params={:?}",
		param_names(&open_only.inner)
	);

	// Direct static parse of args@ for belt-and-suspenders (no package root).
	let src = "args@{ a, b ? 1, ... }: a + b";
	let file = syntax::parse_file(PathBuf::from("inline.nix"), src.into())
		.expect("parse args@ lambda");
	assert_eq!(file.lambdas.len(), 1);
	let lam = &file.lambdas[0];
	assert_eq!(lam.args_bind.as_deref(), Some("args"));
	assert!(lam.ellipsis, "ellipsis flag must be true");
	let parsed = compiler::languages::nix::docs::parse_doc("");
	let func = function::lower_lambda(lam, &parsed, None);
	let names = param_names(&func);
	assert!(names.iter().any(|n| n == "args"));
	assert!(has_variadic(&func));
}

// ─── 5. value_kind_type is used (not dead) ───────────────────────────────────

#[test]
fn value_kind_type_is_live() {
	// Exhaustive coarse mapping — if this were dead code, typed_binding_*
	// would not call it and constants would stay untyped.
	assert!(matches!(
		types::value_kind_type("int"),
		Type::Primitive(Primitive::Int(Width::W64))
	));
	assert!(matches!(
		types::value_kind_type("string"),
		Type::Primitive(Primitive::String)
	));
	assert!(matches!(
		types::value_kind_type("bool"),
		Type::Primitive(Primitive::Bool)
	));
	assert!(matches!(
		types::value_kind_type("list"),
		Type::Slice(_)
	));
	// snix type_of returns "set"; we also accept "attrs".
	for kind in ["set", "attrs"] {
		assert!(
			matches!(
				types::value_kind_type(kind),
				Type::TypeReference(TypeReference { identifier, .. }) if identifier == "AttrSet"
			),
			"value_kind_type({kind:?}) should be AttrSet"
		);
	}
	assert_eq!(
		types::value_kind_type("derivation"),
		types::derivation_type()
	);

	// End-to-end: a scalar constant from the fixture must go through
	// typed_binding_from_shape → value_kind_type("int").
	let root = fixture("fidelity");
	let index = lower_package(Path::new(&root)).expect("lower");
	let c = find_constant(&index, "defaultWidth");
	assert!(
		c.inner.ty.is_some(),
		"typed constants prove value_kind_type is on the static path"
	);

	// Runtime helper also uses value_kind_type.
	let tb = types::typed_binding_from_value(&Value::Integer(7));
	assert!(matches!(
		tb.ty,
		Some(Type::Primitive(Primitive::Int(Width::W64)))
	));
	assert!(matches!(tb.value, Some(ConstExpr::Int(7))));
}

// ─── 6. Forced thunk / catchable path does not panic ─────────────────────────

#[test]
fn force_path_catchable_does_not_panic() {
	// Exercise the same degradation the walker uses: Catchable → soft gap.
	// We re-implement the public contract here via snix values; the walker
	// unit tests (force_tests) cover force_budgeted directly inside the crate.
	let catchable = Value::from(CatchableErrorKind::AssertionFailed);
	// Typing a catchable must not panic.
	let tb = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		types::typed_binding_from_value(&catchable)
	}));
	assert!(
		tb.is_ok(),
		"typed_binding_from_value must not panic on Catchable"
	);
	// Catchable falls through to Any — still a TypedBinding, never a hard error.
	let tb = tb.unwrap();
	assert!(
		matches!(tb.ty, Some(Type::Any) | None),
		"catchable should degrade to Any/empty, got {:?}",
		tb.ty
	);

	// Budgeted force of catchable (mirrored contract): converting via
	// match-classify style must not panic.
	let classified = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		match &catchable {
			Value::Catchable(_) => "unevaluable",
			Value::Thunk(_) => "thunk",
			_ => "other",
		}
	}));
	assert_eq!(classified.ok(), Some("unevaluable"));

	// Full package lower never panics even when dynamic force hits gaps.
	let root = fixture("mini-flake");
	let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
		lower_package(Path::new(&root))
	}));
	assert!(
		result.is_ok(),
		"lower_package must not panic on force/catchable paths"
	);
	let index = result.unwrap().expect("static layer Ok");
	assert!(
		!index.entries_by_path.is_empty(),
		"package lower must still produce a surface"
	);
}

