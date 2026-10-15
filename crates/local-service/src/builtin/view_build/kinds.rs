//! What a compiler-backed declaration is called, and which written
//! declaration it is.
//!
//! The semantic entity vocabulary is deliberately small: a callable is a
//! `Function` whether it is free or a member, every nominal record is a
//! `Record` whether the language spells it `class` or `struct`, and a Python
//! import binding is an `Alias`. A reader sees the *product* vocabulary
//! (`method`, `class`, `struct`, `interface`, `property`, `import`, ...), and
//! the structural lane already speaks it, so the two lanes have to agree on
//! one word per declaration. This module is the single place that derives the
//! product word from what the image proves:
//!
//! * the entity's kind and the kind of the entity that holds it (a callable
//!   held by a type is a method),
//! * the language's own spelling for a record or a contract (Python, TypeScript,
//!   Java and C# write `class`; Rust, Go and C write `struct`; Rust writes
//!   `trait` where every other language writes `interface`),
//! * the facts the language extension retained (a Python `@property`
//!   decorator, whether a Go record has fields), and
//! * the written declaration it pairs with, when the file's structural twin is
//!   known: the grammar's own node name is the authority on a declaration's
//!   spelling, so a paired declaration takes its word.

use super::identity::{declaration_family, declaration_kind};
use backend_compile::{Container, DeclarationKind, SourceDeclaration};
use backend_engine::application::{DocumentationEntity, DocumentationSession};
use backend_semantic::ir::{DeclarationIdentity, ItemKind, SemanticReader};
use backend_semantic::vocabulary::{Language, LanguageProfile};
use std::collections::BTreeMap;

/// The entity that holds a declaration, as far as its kind is concerned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Holder<'a> {
    /// What kind of declaration holds this one.
    pub(super) kind: ItemKind,
    /// The holder's name, when it has one.
    pub(super) name: &'a [u8],
}

/// Returns the declaration kind a compiler-backed entity is shown as.
///
/// `holder` is the entity's lexical parent. `property` says the language
/// extension proved a Python `@property` decorator; `fields` says a Go record
/// has struct fields.
pub(super) fn entity_kind(
    profile: LanguageProfile,
    kind: ItemKind,
    name: &[u8],
    holder: Option<Holder<'_>>,
    property: bool,
    fields: bool,
) -> DeclarationKind {
    let language = Language::from(profile);
    match kind {
        ItemKind::Function => callable_kind(language, name, holder, property),
        ItemKind::Record => record_kind(profile, fields),
        ItemKind::Trait => match language {
            Language::Rust => DeclarationKind::Trait,
            _ => DeclarationKind::Interface,
        },
        ItemKind::Variant => DeclarationKind::Variant,
        ItemKind::Static if language == Language::Python => std::str::from_utf8(name)
            .map_or(DeclarationKind::Variable, |name| {
                backend_compile::python_binding_kind(name)
            }),
        other => declaration_kind(other),
    }
}

/// A callable held by a type is a method (a constructor, where the language
/// names constructors after their type); anything else is a function.
fn callable_kind(
    language: Language,
    name: &[u8],
    holder: Option<Holder<'_>>,
    property: bool,
) -> DeclarationKind {
    let Some(holder) = holder.filter(|holder| holds_members(holder.kind)) else {
        return DeclarationKind::Function;
    };
    if property && language == Language::Python {
        return DeclarationKind::Property;
    }
    if matches!(
        language,
        Language::Java | Language::CSharp | Language::Clang
    ) && !name.is_empty()
        && name == holder.name
    {
        return DeclarationKind::Constructor;
    }
    DeclarationKind::Method
}

/// Returns whether declarations of this kind own members.
const fn holds_members(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::Record
            | ItemKind::Trait
            | ItemKind::Enum
            | ItemKind::Alias
            | ItemKind::Implementation
    )
}

/// The product word for a nominal record in one language.
fn record_kind(profile: LanguageProfile, fields: bool) -> DeclarationKind {
    match profile {
        LanguageProfile::Python(_)
        | LanguageProfile::TypeScript(_)
        | LanguageProfile::Java(_)
        | LanguageProfile::CSharp(_)
        | LanguageProfile::Cxx(_) => DeclarationKind::Class,
        LanguageProfile::Rust(_) | LanguageProfile::C(_) => DeclarationKind::Struct,
        // Go has one record entity for every defined type. A struct has
        // fields; a type defined over another type (`type Level int`) is a
        // plain type.
        LanguageProfile::Go(_) if fields => DeclarationKind::Struct,
        LanguageProfile::Go(_) => DeclarationKind::Type,
    }
}

/// Returns the declaration kind of one entity of an image, reading its holder
/// and language facts from the image itself.
///
/// # Errors
/// Returns the reader's message when the holder is dangling.
pub(super) fn image_entity_kind<Reader: SemanticReader + ?Sized>(
    profile: LanguageProfile,
    reader: &Reader,
    session: &DocumentationSession<'_, Reader>,
    entity: &DocumentationEntity<'_, Reader>,
) -> Result<DeclarationKind, String> {
    let name = entity.name.named_bytes().unwrap_or_default();
    let holder = match entity.entity.parent {
        Some(parent) => {
            let parent = session
                .entity(parent)
                .map_err(|error| format!("project semantic parent: {error}"))?;
            Some((
                parent.entity.kind,
                parent.name.named_bytes().unwrap_or_default(),
            ))
        }
        None => None,
    };
    let holder = holder.map(|(kind, name)| Holder { kind, name });
    let language = Language::from(profile);
    let property = entity.entity.kind == ItemKind::Function
        && language == Language::Python
        && holder.is_some_and(|holder| holds_members(holder.kind))
        && has_property_decorator(reader, entity.entity.id);
    let fields = entity.entity.kind == ItemKind::Record
        && language == Language::Go
        && has_go_fields(reader, entity.entity.id);
    Ok(entity_kind(
        profile,
        entity.entity.kind,
        name,
        holder,
        property,
        fields,
    ))
}

fn has_property_decorator<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: backend_semantic::ir::EntityId,
) -> bool {
    let Some(facts) = reader.python_extension(entity) else {
        return false;
    };
    let Some(decorators) = reader.atom_list(facts.decorators) else {
        return false;
    };
    decorators.into_iter().any(|atom| {
        reader
            .atom(atom)
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .is_some_and(backend_compile::is_property_decorator)
    })
}

fn has_go_fields<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: backend_semantic::ir::EntityId,
) -> bool {
    let Some(facts) = reader.go_extension(entity) else {
        return false;
    };
    reader
        .entity_list(facts.fields)
        .is_some_and(|fields| fields.into_iter().next().is_some())
}

/// Returns the kind a declaration takes once its written twin is known.
///
/// The grammar's own node name settles what was written, so a paired site's
/// kind replaces the derived one when both name the same sort of declaration;
/// an import binding is the one pairing across sorts, because the image
/// records it as an alias of the imported name.
pub(super) fn kind_with_site(
    derived: DeclarationKind,
    item: ItemKind,
    site: DeclarationKind,
) -> DeclarationKind {
    if item == ItemKind::Alias && site == DeclarationKind::Import {
        return DeclarationKind::Import;
    }
    if declaration_family(derived) == declaration_family(site)
        && site != DeclarationKind::Unknown
        && site != DeclarationKind::Import
    {
        return site;
    }
    derived
}

/// One entity of an image, reduced to what pairing it with a written
/// declaration needs.
#[derive(Clone, Debug)]
pub(super) struct PairingEntity {
    /// Stable identity of the entity.
    pub(super) identity: DeclarationIdentity,
    /// Source-written name bytes.
    pub(super) name: Vec<u8>,
    /// The name of the entity that holds this one; empty at module level.
    pub(super) holder: Vec<u8>,
    /// The entity's kind.
    pub(super) item: ItemKind,
    /// The product kind derived from the image alone.
    pub(super) kind: DeclarationKind,
    /// First byte of the entity's source span, when captured.
    pub(super) start: Option<u32>,
}

const CALLABLE: u8 = 0;
const TYPE: u8 = 1;
const VALUE: u8 = 2;
const MODULE: u8 = 3;
const IMPORT: u8 = 4;
const MACRO: u8 = 5;

/// Which written declarations an entity of this sort can be: the sort it is
/// shown as first, then the sorts it can also be written as.
fn pairing_families(item: ItemKind, kind: DeclarationKind, language: Language) -> &'static [u8] {
    match (item, kind) {
        // A Python `@property` is written as a function and tagged a property.
        (ItemKind::Function, DeclarationKind::Property) if language == Language::Python => {
            &[VALUE, CALLABLE]
        }
        // An import binding is an alias of the imported name.
        (ItemKind::Alias, _) => &[TYPE, IMPORT],
        _ => match declaration_family(kind) {
            0 => &[CALLABLE],
            1 => &[TYPE],
            2 => &[VALUE],
            3 => &[MODULE],
            4 => &[IMPORT],
            5 => &[MACRO],
            _ => &[],
        },
    }
}

/// The name of the declaration a written declaration sits in; empty at
/// module level.
fn container_name(declaration: &SourceDeclaration) -> &str {
    match declaration.container() {
        Container::Module => "",
        Container::Binding { .. } => "",
        Container::Enclosing { name, .. } | Container::Attached { type_name: name } => name,
    }
}

/// Pairs every entity with the written declaration of the same name it is.
///
/// A semantic image records each declaration's byte span but not its line,
/// and the view does not retain whole source files, so a declaration is
/// paired by what both lanes know: its name and sort of declaration, in
/// source order. Pairing is exact when both lanes saw the same declarations of
/// a name, and refuses rather than guesses when they did not. Two cases are
/// decidable without a line and are resolved here:
///
/// * a Python later definition of the same name in the same scope is shadowed
///   by the first one (`if`/`else` and `try`/`except` branches define a name
///   twice), and the image keeps only the first, so the image's entities pair
///   with the earliest written declarations of that scope;
/// * a declaration written in a different sort than the image records it
///   (`@property`, an import alias) is paired after the exact sort is.
pub(super) fn pair_sites<'a>(
    language: Language,
    entities: &[PairingEntity],
    declarations: &[&'a SourceDeclaration],
) -> BTreeMap<DeclarationIdentity, &'a SourceDeclaration> {
    let mut sites = BTreeMap::new();
    if declarations.is_empty() || entities.is_empty() {
        return sites;
    }
    // Python scopes a name by what holds it; every other language pairs a
    // name across the file, because its image's holder of a member is not a
    // name the structural lane spells the same way (an `impl` block).
    let scoped = language == Language::Python;
    let mut taken = vec![false; declarations.len()];
    let mut written: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (index, declaration) in declarations.iter().enumerate() {
        let scope = if scoped {
            container_name(declaration)
        } else {
            ""
        };
        written
            .entry((declaration.name(), scope))
            .or_default()
            .push(index);
    }
    for indices in written.values_mut() {
        indices.sort_by_key(|index| declarations[*index].line());
    }
    let mut grouped: BTreeMap<(&[u8], &[u8]), Vec<&PairingEntity>> = BTreeMap::new();
    for entity in entities {
        let scope: &[u8] = if scoped {
            entity.holder.as_slice()
        } else {
            &[]
        };
        grouped
            .entry((entity.name.as_slice(), scope))
            .or_default()
            .push(entity);
    }
    for ((name, scope), mut group) in grouped {
        let (Ok(name), Ok(scope)) = (std::str::from_utf8(name), std::str::from_utf8(scope)) else {
            continue;
        };
        let Some(candidates) = written.get(&(name, scope)) else {
            continue;
        };
        group.sort_by_key(|entity| entity.start);
        // The exact sort first, then the cross-sort pairings.
        for exact in [true, false] {
            for family in [CALLABLE, TYPE, VALUE, MODULE, IMPORT, MACRO] {
                let mut wanted = group
                    .iter()
                    .filter(|entity| !sites.contains_key(&entity.identity))
                    .filter(|entity| {
                        let families = pairing_families(entity.item, entity.kind, language);
                        if exact {
                            families.first() == Some(&family)
                        } else {
                            families.first() != Some(&family) && families.contains(&family)
                        }
                    })
                    .copied()
                    .collect::<Vec<_>>();
                if wanted.is_empty() {
                    continue;
                }
                let open = candidates
                    .iter()
                    .copied()
                    .filter(|index| !taken[*index])
                    .filter(|index| declaration_family(declarations[*index].kind()) == family)
                    .collect::<Vec<_>>();
                // A Go package image also carries sibling declarations with no
                // primary-file span. Preserve the current native projection's
                // rule: only discard them when captured sites account for the
                // entire structural family in these exact source bytes.
                if wanted
                    .iter()
                    .filter(|entity| entity.start.is_some())
                    .count()
                    == open.len()
                {
                    wanted.retain(|entity| entity.start.is_some());
                }
                let usable = if open.len() == wanted.len() {
                    open.as_slice()
                } else if scoped && exact && open.len() > wanted.len() {
                    // The image keeps the first of a redefined name.
                    open.get(..wanted.len()).unwrap_or(&[])
                } else {
                    continue;
                };
                // Several of one name pair by order, which needs every span.
                if wanted.len() > 1 && wanted.iter().any(|entity| entity.start.is_none()) {
                    continue;
                }
                for (entity, index) in wanted.iter().zip(usable) {
                    taken[*index] = true;
                    sites.insert(entity.identity, declarations[*index]);
                }
            }
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use backend_semantic::ir::ItemKind;
    use backend_semantic::vocabulary::{
        CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, PythonVersion, RustEdition,
        TypeScriptSource,
    };

    const RUST: LanguageProfile = LanguageProfile::Rust(RustEdition::Rust2024);
    const PYTHON: LanguageProfile = LanguageProfile::Python(PythonVersion::Python314);
    const GO: LanguageProfile = LanguageProfile::Go(GoVersion::Go125);
    const TYPESCRIPT: LanguageProfile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    const JAVA: LanguageProfile = LanguageProfile::Java(JavaRelease::Java25);
    const CSHARP: LanguageProfile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
    const C: LanguageProfile = LanguageProfile::C(CStandard::C23);
    const CXX: LanguageProfile = LanguageProfile::Cxx(CxxStandard::Cxx23);

    fn holder(kind: ItemKind, name: &[u8]) -> Option<Holder<'_>> {
        Some(Holder { kind, name })
    }

    #[test]
    fn a_callable_held_by_a_type_is_a_method_everywhere() {
        for profile in [RUST, PYTHON, GO, TYPESCRIPT, JAVA, CSHARP, CXX] {
            for owner in [
                ItemKind::Record,
                ItemKind::Trait,
                ItemKind::Enum,
                ItemKind::Alias,
                ItemKind::Implementation,
            ] {
                assert_eq!(
                    entity_kind(
                        profile,
                        ItemKind::Function,
                        b"run",
                        holder(owner, b"Worker"),
                        false,
                        false
                    ),
                    DeclarationKind::Method,
                    "{profile:?} held by {owner:?}"
                );
            }
        }
    }

    #[test]
    fn a_callable_held_by_nothing_or_by_a_scope_is_a_function() {
        for held in [
            None,
            holder(ItemKind::Module, b"util"),
            holder(ItemKind::Namespace, b"util"),
            // A function nested in a function stays a function.
            holder(ItemKind::Function, b"outer"),
        ] {
            assert_eq!(
                entity_kind(PYTHON, ItemKind::Function, b"helper", held, false, false),
                DeclarationKind::Function
            );
        }
    }

    #[test]
    fn a_property_decorator_makes_a_python_member_a_property() {
        let held = holder(ItemKind::Record, b"Worker");
        assert_eq!(
            entity_kind(PYTHON, ItemKind::Function, b"size", held, true, false),
            DeclarationKind::Property
        );
        // The decorator only means something to a type's member.
        assert_eq!(
            entity_kind(PYTHON, ItemKind::Function, b"size", None, true, false),
            DeclarationKind::Function
        );
    }

    #[test]
    fn a_member_named_for_its_type_is_a_constructor_where_the_language_says_so() {
        let held = holder(ItemKind::Record, b"Worker");
        for profile in [JAVA, CSHARP, CXX] {
            assert_eq!(
                entity_kind(profile, ItemKind::Function, b"Worker", held, false, false),
                DeclarationKind::Constructor
            );
        }
        for profile in [PYTHON, RUST, GO, TYPESCRIPT] {
            assert_eq!(
                entity_kind(profile, ItemKind::Function, b"Worker", held, false, false),
                DeclarationKind::Method
            );
        }
    }

    #[test]
    fn a_record_is_a_class_or_a_struct_by_the_language_that_wrote_it() {
        for profile in [PYTHON, TYPESCRIPT, JAVA, CSHARP, CXX] {
            assert_eq!(
                entity_kind(profile, ItemKind::Record, b"Worker", None, false, false),
                DeclarationKind::Class,
                "{profile:?}"
            );
        }
        for profile in [RUST, C] {
            assert_eq!(
                entity_kind(profile, ItemKind::Record, b"Worker", None, false, false),
                DeclarationKind::Struct,
                "{profile:?}"
            );
        }
        // Go has one record entity: fields make it a struct, otherwise it is
        // a type defined over another type.
        assert_eq!(
            entity_kind(GO, ItemKind::Record, b"Color", None, false, true),
            DeclarationKind::Struct
        );
        assert_eq!(
            entity_kind(GO, ItemKind::Record, b"Level", None, false, false),
            DeclarationKind::Type
        );
    }

    #[test]
    fn a_contract_is_a_trait_in_rust_and_an_interface_elsewhere() {
        assert_eq!(
            entity_kind(RUST, ItemKind::Trait, b"Service", None, false, false),
            DeclarationKind::Trait
        );
        for profile in [GO, TYPESCRIPT, JAVA, CSHARP, PYTHON] {
            assert_eq!(
                entity_kind(profile, ItemKind::Trait, b"Service", None, false, false),
                DeclarationKind::Interface,
                "{profile:?}"
            );
        }
    }

    #[test]
    fn an_enum_variant_is_a_variant_and_not_a_constant() {
        for profile in [RUST, TYPESCRIPT, JAVA, CSHARP] {
            assert_eq!(
                entity_kind(profile, ItemKind::Variant, b"Started", None, false, false),
                DeclarationKind::Variant
            );
        }
        assert_eq!(
            entity_kind(RUST, ItemKind::Constant, b"LIMIT", None, false, false),
            DeclarationKind::Constant
        );
    }

    #[test]
    fn a_python_binding_is_a_constant_only_when_it_is_spelled_like_one() {
        assert_eq!(
            entity_kind(PYTHON, ItemKind::Static, b"LIMIT", None, false, false),
            DeclarationKind::Constant
        );
        assert_eq!(
            entity_kind(
                PYTHON,
                ItemKind::Static,
                b"_CACHE_SIZE2",
                None,
                false,
                false
            ),
            DeclarationKind::Constant
        );
        for name in [&b"counter"[..], b"__version__", b"Registry", b"_", b"__"] {
            assert_eq!(
                entity_kind(PYTHON, ItemKind::Static, name, None, false, false),
                DeclarationKind::Variable,
                "{}",
                String::from_utf8_lossy(name)
            );
        }
        assert_eq!(
            entity_kind(GO, ItemKind::Static, b"Registry", None, false, false),
            DeclarationKind::Variable
        );
    }

    #[test]
    fn a_written_twin_settles_the_spelling_the_grammar_decided() {
        // The grammar's own node name is the authority on what was written.
        assert_eq!(
            kind_with_site(
                DeclarationKind::Class,
                ItemKind::Record,
                DeclarationKind::Struct
            ),
            DeclarationKind::Struct
        );
        assert_eq!(
            kind_with_site(
                DeclarationKind::Type,
                ItemKind::Record,
                DeclarationKind::Struct
            ),
            DeclarationKind::Struct
        );
        assert_eq!(
            kind_with_site(
                DeclarationKind::Method,
                ItemKind::Function,
                DeclarationKind::Constructor
            ),
            DeclarationKind::Constructor
        );
        // An import binding is the one pairing across sorts.
        assert_eq!(
            kind_with_site(
                DeclarationKind::Type,
                ItemKind::Alias,
                DeclarationKind::Import
            ),
            DeclarationKind::Import
        );
        // A declaration of another sort never changes what the image proves.
        assert_eq!(
            kind_with_site(
                DeclarationKind::Method,
                ItemKind::Function,
                DeclarationKind::Variable
            ),
            DeclarationKind::Method
        );
        assert_eq!(
            kind_with_site(
                DeclarationKind::Function,
                ItemKind::Function,
                DeclarationKind::Import
            ),
            DeclarationKind::Function
        );
    }

    fn identity(seed: u8) -> DeclarationIdentity {
        DeclarationIdentity {
            family: backend_semantic::ir::DeclarationFamilyId::from_raw([seed; 16]),
            variant: backend_semantic::ir::VariantFingerprint::from_raw([seed; 16]),
        }
    }

    fn entity(
        seed: u8,
        name: &str,
        holder: &str,
        item: ItemKind,
        kind: DeclarationKind,
        start: u32,
    ) -> PairingEntity {
        PairingEntity {
            identity: identity(seed),
            name: name.as_bytes().to_vec(),
            holder: holder.as_bytes().to_vec(),
            item,
            kind,
            start: Some(start),
        }
    }

    fn written(
        name: &str,
        kind: DeclarationKind,
        line: u32,
        container: Container,
    ) -> SourceDeclaration {
        SourceDeclaration::at_path("m.py", name, kind, line, "", "")
            .expect("declaration")
            .with_container(container)
    }

    fn paired(sites: &BTreeMap<DeclarationIdentity, &SourceDeclaration>, seed: u8) -> Option<u32> {
        sites.get(&identity(seed)).map(|site| site.line())
    }

    #[test]
    fn a_name_both_lanes_saw_equally_pairs_in_source_order() {
        let declarations = [
            written("run", DeclarationKind::Method, 10, Container::Module),
            written("run", DeclarationKind::Method, 30, Container::Module),
        ];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let entities = [
            entity(
                2,
                "run",
                "",
                ItemKind::Function,
                DeclarationKind::Method,
                900,
            ),
            entity(
                1,
                "run",
                "",
                ItemKind::Function,
                DeclarationKind::Method,
                100,
            ),
        ];
        let sites = pair_sites(Language::Go, &entities, &borrowed);
        assert_eq!(paired(&sites, 1), Some(10));
        assert_eq!(paired(&sites, 2), Some(30));
    }

    #[test]
    fn an_unequal_count_outside_python_pairs_nothing_rather_than_guessing() {
        let declarations = [
            written("sys", DeclarationKind::Function, 4, Container::Module),
            written("sys", DeclarationKind::Function, 9, Container::Module),
        ];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let entities = [entity(
            1,
            "sys",
            "",
            ItemKind::Function,
            DeclarationKind::Function,
            40,
        )];
        assert!(pair_sites(Language::Rust, &entities, &borrowed).is_empty());
    }

    #[test]
    fn a_python_name_defined_in_both_branches_pairs_with_the_first_definition() {
        let declarations = [
            written("where", DeclarationKind::Function, 21, Container::Module),
            written("where", DeclarationKind::Function, 56, Container::Module),
            written("contents", DeclarationKind::Function, 48, Container::Module),
            written("contents", DeclarationKind::Function, 72, Container::Module),
        ];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let entities = [
            entity(
                1,
                "where",
                "",
                ItemKind::Function,
                DeclarationKind::Function,
                300,
            ),
            entity(
                2,
                "contents",
                "",
                ItemKind::Function,
                DeclarationKind::Function,
                900,
            ),
        ];
        let sites = pair_sites(Language::Python, &entities, &borrowed);
        assert_eq!(paired(&sites, 1), Some(21));
        assert_eq!(paired(&sites, 2), Some(48));
    }

    #[test]
    fn a_python_redefinition_is_scoped_to_the_class_that_holds_it() {
        let in_a = Container::enclosing("A", std::num::NonZeroU32::MIN);
        let in_b = Container::enclosing("B", std::num::NonZeroU32::MIN);
        let declarations = [
            written("dump", DeclarationKind::Method, 5, in_a),
            written("dump", DeclarationKind::Method, 15, in_b.clone()),
            written("dump", DeclarationKind::Method, 25, in_b),
        ];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let entities = [
            entity(
                1,
                "dump",
                "A",
                ItemKind::Function,
                DeclarationKind::Method,
                100,
            ),
            entity(
                2,
                "dump",
                "B",
                ItemKind::Function,
                DeclarationKind::Method,
                500,
            ),
        ];
        let sites = pair_sites(Language::Python, &entities, &borrowed);
        assert_eq!(paired(&sites, 1), Some(5));
        assert_eq!(paired(&sites, 2), Some(15));
    }

    #[test]
    fn a_python_property_pairs_with_the_property_it_was_written_as() {
        let in_class = Container::enclosing("Connection", std::num::NonZeroU32::MIN);
        let declarations = [written("states", DeclarationKind::Property, 194, in_class)];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let entities = [entity(
            1,
            "states",
            "Connection",
            ItemKind::Function,
            DeclarationKind::Property,
            7,
        )];
        let sites = pair_sites(Language::Python, &entities, &borrowed);
        assert_eq!(paired(&sites, 1), Some(194));
    }

    #[test]
    fn a_python_import_pairs_with_the_import_the_alias_came_from() {
        let declarations = [written(
            "where",
            DeclarationKind::Import,
            1,
            Container::Module,
        )];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let entities = [entity(
            1,
            "where",
            "",
            ItemKind::Alias,
            DeclarationKind::Type,
            20,
        )];
        let sites = pair_sites(Language::Python, &entities, &borrowed);
        assert_eq!(paired(&sites, 1), Some(1));
        let site = sites.get(&identity(1)).expect("paired import");
        assert_eq!(
            kind_with_site(DeclarationKind::Type, ItemKind::Alias, site.kind()),
            DeclarationKind::Import
        );
    }

    #[test]
    fn several_entities_of_one_name_need_every_span_to_pair() {
        let declarations = [
            written("run", DeclarationKind::Function, 3, Container::Module),
            written("run", DeclarationKind::Function, 8, Container::Module),
        ];
        let borrowed = declarations.iter().collect::<Vec<_>>();
        let mut entities = vec![
            entity(
                1,
                "run",
                "",
                ItemKind::Function,
                DeclarationKind::Function,
                10,
            ),
            entity(
                2,
                "run",
                "",
                ItemKind::Function,
                DeclarationKind::Function,
                90,
            ),
        ];
        entities[1].start = None;
        assert!(pair_sites(Language::Go, &entities, &borrowed).is_empty());
    }
}
