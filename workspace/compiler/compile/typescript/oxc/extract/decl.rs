//! Declarations → `ir::kind::Entry` (+ member entries). The integrator of the
//! extract layer: dispatches a grouped [`SymbolGroup`] to the per-kind helpers
//! and assembles [`FactEntry`]s carrying provisional paths + type refs.
//!
//! See OXC-PORT-SPEC.md §1 for the exact per-declaration IR shapes to reproduce.
//! LEAF FILE — fill the `todo!()` bodies.

use oxc_ast::ast::{
	Class, Function, TSEnumDeclaration, TSInterfaceDeclaration, TSModuleDeclaration,
	TSTypeAliasDeclaration, VariableDeclaration,
};

use ir::{
	function::Function as IrFunction,
	protocols::TraitDef,
	record::{Record, SumVariant},
	ty::Type,
};

use super::{Extractor, FactEntry, Result, SymbolGroup};

impl<'a> Extractor<'a> {
	/// Lower one grouped symbol into its IR entries: the primary entry plus any
	/// member entries (class methods/constructor, namespace elements). Applies
	/// the group's name / visibility / documentation and, for functions, folds
	/// non-primary declarations into `overloads`.
	pub(crate) fn lower_symbol(&mut self, group: &SymbolGroup<'a>) -> Result<Vec<FactEntry>> {
		let _ = group;
		todo!("decl.rs: SymbolGroup → Vec<FactEntry> (primary + members)")
	}

	/// Lower a class into its `Record` plus separated constructor/method member
	/// entries (member paths recorded on the record). Constructor
	/// parameter-properties (`accessibility`/`readonly` on a ctor param) are
	/// detected here — constructor-only, no synthetic class field.
	pub(crate) fn lower_class(
		&mut self,
		name: &str,
		cls: &Class<'a>,
	) -> Result<(Record, Vec<FactEntry>)> {
		let _ = (name, cls);
		todo!("decl.rs: Class → (Record, member entries)")
	}

	/// Lower an interface into an `ir::protocols::TraitDef` (methods, call/index
	/// signatures synthesized as `__call*`/`__index*` methods, extends → super
	/// traits, properties).
	pub(crate) fn lower_interface(
		&mut self,
		name: &str,
		iface: &TSInterfaceDeclaration<'a>,
	) -> Result<TraitDef> {
		let _ = (name, iface);
		todo!("decl.rs: TSInterfaceDeclaration → TraitDef")
	}

	/// Lower an enum into its `SumVariant`s (member name → unit variant; carries
	/// `r#const` forward as additive capability).
	pub(crate) fn lower_enum(&mut self, en: &TSEnumDeclaration<'a>) -> Result<Vec<SumVariant>> {
		let _ = en;
		todo!("decl.rs: TSEnumDeclaration → Vec<SumVariant>")
	}

	/// Lower a namespace / ambient module into member entries (recursively
	/// lowering nested elements under the `parent.child` module path).
	pub(crate) fn lower_namespace(
		&mut self,
		name: &str,
		ns: &TSModuleDeclaration<'a>,
	) -> Result<Vec<FactEntry>> {
		let _ = (name, ns);
		todo!("decl.rs: TSModuleDeclaration → member entries")
	}

	/// Lower a variable declaration into per-binding entries (destructuring
	/// fan-out; `Const` → `Entry::Constant`, else `Entry::Variable`; type via
	/// annotation → referenced-symbol → initializer inference).
	pub(crate) fn lower_variable(&mut self, decl: &VariableDeclaration<'a>) -> Result<Vec<FactEntry>> {
		let _ = decl;
		todo!("decl.rs: VariableDeclaration → entries")
	}

	/// Lower a type alias's RHS type.
	pub(crate) fn lower_type_alias(&mut self, alias: &TSTypeAliasDeclaration<'a>) -> Result<Type> {
		let _ = alias;
		todo!("decl.rs: TSTypeAliasDeclaration → Type")
	}

	/// Collect non-primary function declarations in a group as overloads.
	pub(crate) fn function_overloads(
		&mut self,
		group: &SymbolGroup<'a>,
		primary: &Function<'a>,
	) -> Result<Option<Vec<IrFunction>>> {
		let _ = (group, primary);
		todo!("decl.rs: overload grouping")
	}
}
