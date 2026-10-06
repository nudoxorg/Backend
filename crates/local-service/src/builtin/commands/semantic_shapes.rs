//! Bounded typed-shape projection over the already selected compiler image.
//!
//! This module owns no compiler parsing or type system. It follows the exact
//! symbol into a checked `SemanticImageView` and copies only typed IR facts
//! under the shared product response budget.

use super::super::view_build;
use super::super::{
    BuiltinModel, BuiltinModelError, BuiltinSemanticRelation, activate_semantic_publication,
    read_package_sources,
};
use super::semantic_query::for_package_publications;
use backend_engine::application::LocalCompilerClient;
use backend_engine::builtin::{ProductSemanticPublicationRecord, SemanticPublicationCoverage};
use backend_library::{
    CommandFailure, CommandReply, SemanticArrayShape, SemanticCallableCarrierBindings,
    SemanticCallableShape, SemanticDeclarationShape, SemanticImagePayloadBytes, SemanticLiteral,
    SemanticObjectMember, SemanticPropertyKey, SemanticShapeBatch, SemanticShapeEntry,
    SemanticShapeFact, SemanticShapeImageOrigin, SemanticShapeLanguageFact,
    SemanticShapeLanguageFacts, SemanticShapeMember, SemanticShapeRequest, SemanticShapeSelection,
    SemanticShapeSourceOrigin, SemanticShapeUnavailable, SemanticTypeElement, SemanticTypeExpr,
    SemanticTypeFact, SemanticTypeUnavailable, SymbolAddress,
};
use backend_semantic::ir::{
    ArrayShape, ConcreteType, DeclarationIdentity, ExternalTarget, ExternalTargetIdentity,
    FunctionVariadicForm, ItemKind, LanguageProfile, LiteralType, ObjectMember, PropertyKey,
    SemanticCoreReader, SemanticImageView, SemanticReader, SignatureCarrierBindingRole,
    SignatureCarrierBindingsObservation, SignatureCarrierRole, SignatureCarrierRoleObservation,
    TupleElementKind, TypeExpr, TypeId,
};
use std::collections::BTreeSet;

/// Hard cap on canonical declaration rows inspected while resolving one
/// selected semantic publication. Shape output budgets do not bound lookup
/// work, so a separate owner-controlled scan ceiling prevents an absent
/// selector from forcing an unbounded walk of every image.
const MAX_SEMANTIC_SHAPE_SCAN_ENTITIES: usize = 1_000_000;
/// Post-activation scan fence. Before payload materialization, the selected
/// loader checks metadata image count and encoded-byte total against the
/// per-generation residence admission caps; aggregate cached bytes have a
/// separate residence cap. This command's fence bounds lookup work if those
/// policies change.
const MAX_SEMANTIC_SHAPE_SCAN_IMAGES: usize = 4096;

struct ProjectionMeter {
    nodes: usize,
    bytes: usize,
    max_nodes: usize,
    max_bytes: usize,
    proof_symbols: BTreeSet<backend_engine::SymbolKey>,
    proof_packages: BTreeSet<backend_engine::PackageKey>,
    omitted_proof_symbols: BTreeSet<backend_engine::SymbolKey>,
}

impl ProjectionMeter {
    fn new(request: &SemanticShapeRequest) -> Self {
        Self {
            nodes: 0,
            bytes: 0,
            max_nodes: usize::from(request.budget().max_nodes()),
            max_bytes: usize::try_from(request.budget().max_bytes()).unwrap_or(usize::MAX),
            proof_symbols: BTreeSet::new(),
            proof_packages: BTreeSet::new(),
            omitted_proof_symbols: BTreeSet::new(),
        }
    }

    fn node(&mut self, bytes: usize) -> Result<(), SemanticShapeUnavailable> {
        self.nodes_bytes(1, bytes)
    }

    fn preflight_nodes_bytes(
        &self,
        nodes: usize,
        bytes: usize,
    ) -> Result<(usize, usize), SemanticShapeUnavailable> {
        let total_nodes = self
            .nodes
            .checked_add(nodes)
            .ok_or(SemanticShapeUnavailable::NodeBudget)?;
        let total_bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(SemanticShapeUnavailable::ByteBudget)?;
        if total_nodes > self.max_nodes {
            return Err(SemanticShapeUnavailable::NodeBudget);
        }
        if total_bytes > self.max_bytes {
            return Err(SemanticShapeUnavailable::ByteBudget);
        }
        Ok((total_nodes, total_bytes))
    }

    fn nodes_bytes(&mut self, nodes: usize, bytes: usize) -> Result<(), SemanticShapeUnavailable> {
        let (total_nodes, total_bytes) = self.preflight_nodes_bytes(nodes, bytes)?;
        self.nodes = total_nodes;
        self.bytes = total_bytes;
        Ok(())
    }

    fn bytes(&mut self, bytes: usize) -> Result<(), SemanticShapeUnavailable> {
        let total = self
            .bytes
            .checked_add(bytes)
            .ok_or(SemanticShapeUnavailable::ByteBudget)?;
        if total > self.max_bytes {
            return Err(SemanticShapeUnavailable::ByteBudget);
        }
        self.bytes = total;
        Ok(())
    }

    fn text(&mut self, length: usize) -> Result<(), SemanticShapeUnavailable> {
        self.bytes(length.saturating_mul(6).saturating_add(8))
    }

    /// Charges the exact row preimages that the response certificate will
    /// carry for a visible declaration reference. The response must keep each
    /// optional SymbolKey bounded together with the proof that admits it.
    fn visible_symbol_proof(
        &mut self,
        view: &backend_engine::ViewRoot,
        symbol: backend_engine::SymbolKey,
    ) -> bool {
        if self.proof_symbols.contains(&symbol) {
            return true;
        }
        if self.omitted_proof_symbols.contains(&symbol) {
            return false;
        }
        let Some(row) = view.row_ref(backend_engine::RowId::Symbol(symbol)) else {
            self.omitted_proof_symbols.insert(symbol);
            return false;
        };
        let identity_bytes = row
            .identity_preimage()
            .map_or(row.label.len(), |preimage| preimage.as_str().len());
        // A canonical preimage uses one row-identity claim plus the typed key
        // commitment; the fallback uses the row's readable key and row-ID
        // commitment as well, so reserve the larger fixed allowance.
        let mut charge = identity_bytes.saturating_add(768);
        let new_package = row
            .package
            .filter(|package| !self.proof_packages.contains(package));
        if let Some(package) = new_package
            && let Some(package_row) = view.row_ref(backend_engine::RowId::Package(package))
        {
            let package_identity_bytes = package_row
                .identity_preimage()
                .map_or(package_row.label.len(), |preimage| preimage.as_str().len());
            charge = charge.saturating_add(package_identity_bytes.saturating_add(512));
        }
        if self
            .bytes
            .checked_add(charge)
            .is_none_or(|total| total > self.max_bytes)
        {
            self.omitted_proof_symbols.insert(symbol);
            return false;
        }
        self.bytes += charge;
        if let Some(package) = new_package {
            self.proof_packages.insert(package);
        }
        self.proof_symbols.insert(symbol);
        true
    }
}

/// Resolves a root-pinned batch only after the selected publication has been
/// rebound to the exact current view and source-generation record.
pub(super) fn execute_semantic_shapes(
    daemon: &crate::Locald<
        BuiltinModel,
        super::super::BuiltinValidator,
        super::super::BuiltinAuthorityVerifier,
    >,
    compiler: &LocalCompilerClient,
    generations: &mut super::super::generation_residence::SemanticGenerationResidence,
    image_rows: &mut view_build::ImageRowResidence,
    semantic_authority: &super::super::semantic_authority::SemanticAuthority,
    published_roots: Option<([u8; 32], [u8; 32])>,
    request: &SemanticShapeRequest,
) -> Result<CommandReply, BuiltinModelError> {
    let library = daemon.engine().daemon().library();
    let view = library.view();
    let current = view.root();
    if !request.basis().matches(current) {
        return Ok(CommandReply::Failed(CommandFailure::WrongBasis {
            expected: current.into(),
            observed: request.basis(),
        }));
    }
    let profile =
        request.source().profile.profile().map_err(|error| {
            BuiltinModelError(format!("invalid semantic-shape profile: {error}"))
        })?;
    if request.source().coordinate.package_type().language() != profile.language()
        || !request.source().selected
        || !request.source().complete
    {
        return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
            "semantic shapes require one selected complete source generation".to_owned(),
        )));
    }
    let package = backend_engine::package_key(request.source().package.as_str());
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let snapshot_source_root = super::super::view_publish::source_root(&snapshot)?;
    let snapshot_semantic_root = super::super::view_publish::semantic_root(&snapshot)?;
    if published_roots != Some((snapshot_source_root, snapshot_semantic_root)) {
        return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
            "semantic shape view is detached from the current workspace snapshot".to_owned(),
        )));
    }
    let sources = read_package_sources(&snapshot, package)?;
    let relation = snapshot
        .relation::<BuiltinSemanticRelation>()
        .map_err(|error| BuiltinModelError(format!("open selected semantic relation: {error}")))?;
    let mut selected = None;
    for_package_publications(&relation, package, &sources, "shape", |key, record| {
        if key.coordinate().as_str() != request.source().coordinate.as_str()
            || key.profile() != profile
        {
            return Ok(());
        }
        if selected.is_some() {
            return Err(BuiltinModelError(
                "semantic shape source has multiple selected publications".to_owned(),
            ));
        }
        let ProductSemanticPublicationRecord::Published {
            coverage: SemanticPublicationCoverage::Complete,
            claim,
        } = record
        else {
            return Err(BuiltinModelError(
                "semantic shape source is not a complete publication".to_owned(),
            ));
        };
        let binding = claim.binding();
        let manifest = claim.manifest();
        let exact = request.source().generation.to_bytes() == *binding.identity.as_ref()
            && request.source().generation_root == *binding.generation.pinned_root.as_ref()
            && request.source().dependency_set == *binding.generation.dep_set.as_ref()
            && request.source().manifest == *manifest.identity.as_ref()
            && request.source().artifacts == manifest.fragment_count
            && request.source().semantic_bytes == manifest.byte_length
            && request.source().package == *key.package()
            && request.source().coordinate.as_str() == key.coordinate().as_str()
            && request.source().profile.profile().ok() == Some(key.profile());
        if !exact {
            return Err(BuiltinModelError(
                "semantic shape source differs from the selected publication".to_owned(),
            ));
        }
        selected = Some((key.clone(), *claim));
        Ok(())
    })?;
    let Some((key, claim)) = selected else {
        return Ok(CommandReply::Failed(CommandFailure::NotFound));
    };
    let freshness_key = super::super::semantic_authority::SelectedSemanticPublicationKey::new(&key)
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
    // Freshness is a current source-input observation, not part of the
    // immutable generation binding. Recompute it from this exact selected
    // key/claim read from the same immutable snapshot, while the owner handles
    // one command, and reject a request whose product record predates a source
    // observation. History status is
    // intentionally outside this shape authority: it is an asynchronous,
    // derived sidecar and is neither echoed nor triggered by a shape read.
    if semantic_authority.freshness(freshness_key, claim) != request.source().freshness {
        return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
            "semantic shape source freshness changed after selection".to_owned(),
        )));
    }

    // Every positive selector must already be a declaration row in this exact
    // view. Cross-package requests would bind an image to the wrong owner.
    let mut resolved = Vec::with_capacity(request.symbols().len());
    for address in request.symbols().iter().copied() {
        let Some(symbol) = address.resolve(view) else {
            resolved.push(None);
            continue;
        };
        let Some(row) = view.row_ref(backend_engine::RowId::Symbol(symbol)) else {
            resolved.push(None);
            continue;
        };
        if row.package != Some(package) {
            return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
                "all semantic shape symbols must belong to the selected source package".to_owned(),
            )));
        }
        resolved.push(Some(symbol));
    }

    let activated = activate_semantic_publication(compiler, &key, claim, generations, image_rows)?;
    if u32::try_from(activated.images().len()).ok() != Some(request.source().artifacts) {
        return Err(BuiltinModelError(
            "selected semantic image count differs from its checked manifest".to_owned(),
        ));
    }
    let selected_image_bytes = activated
        .images()
        .iter()
        .try_fold(0_u32, |total, image| {
            if image.authority.byte_len == 0 {
                return None;
            }
            total.checked_add(image.authority.byte_len)
        })
        .ok_or_else(|| {
            BuiltinModelError(
                "selected semantic image payload extent is empty or overflowed".to_owned(),
            )
        })?;
    let semantic_image_bytes = SemanticImagePayloadBytes::new(selected_image_bytes)
        .map_err(|error| BuiltinModelError(format!("admit selected image extent: {error}")))?;
    let selection_root = *snapshot.root().as_bytes();
    let selected_source = SemanticShapeSelection::from_selected(request.source())
        .map_err(|error| BuiltinModelError(format!("selected shape source admission: {error}")))?;
    let source_wire_bytes = serde_json::to_vec(&selected_source)
        .map_err(|error| BuiltinModelError(format!("encode semantic source witness: {error}")))?
        .len();
    let mut meter = ProjectionMeter::new(request);
    for symbol in resolved.iter().flatten().copied() {
        if !meter.visible_symbol_proof(view, symbol) {
            return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
                "semantic shape request exceeds its selected-symbol proof budget".to_owned(),
            )));
        }
    }
    // Reserve every output envelope before spending the caller's limits on
    // recursive facts. This keeps the mandatory source witness for an entry
    // admissible even when a later type edge is truncated to Unavailable.
    for symbol in &resolved {
        let origin_bytes = if symbol.is_some() {
            source_wire_bytes.saturating_add(768)
        } else {
            0
        };
        if meter.node(256usize.saturating_add(origin_bytes)).is_err() || meter.node(96).is_err() {
            return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
                "semantic shape request cannot fit its bounded result envelope".to_owned(),
            )));
        }
    }
    let mut entries = Vec::with_capacity(request.symbols().len());
    let requested_symbols = resolved.iter().flatten().copied().collect::<BTreeSet<_>>();
    // Open and scan each image once for the whole batch. The former inner
    // request loop reopened and linearly searched every image independently
    // for each symbol, multiplying compiler-entity traversal by batch size.
    // Keep exact image index, entity coordinate, and declaration identity
    // together until projection borrows from the same reopened image below.
    let mut opened_images = Vec::new();
    let mut locations = std::collections::BTreeMap::new();
    let mut scanned_entities = 0usize;
    if !requested_symbols.is_empty() {
        if activated.images().len() > MAX_SEMANTIC_SHAPE_SCAN_IMAGES {
            return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
                "selected semantic shape image set exceeds its bounded lookup work".to_owned(),
            )));
        }
        opened_images.reserve(activated.images().len());
        for (image_index, image_snapshot) in activated.images().iter().enumerate() {
            let image = image_snapshot.reopen().map_err(|error| {
                BuiltinModelError(format!("reopen selected semantic shape image: {error}"))
            })?;
            let entities = image.canonical_entities();
            let image_entity_count = entities.len();
            let remaining = MAX_SEMANTIC_SHAPE_SCAN_ENTITIES.saturating_sub(scanned_entities);
            for entity in entities.take(remaining) {
                scanned_entities += 1;
                let identity = entity.version.identity();
                let symbol = view_build::semantic_symbol(package, identity);
                if requested_symbols.contains(&symbol) {
                    locations
                        .entry(symbol)
                        .or_insert((image_index, entity.id, identity));
                    if locations.len() == requested_symbols.len() {
                        break;
                    }
                }
            }
            opened_images.push(image);
            if locations.len() == requested_symbols.len() {
                break;
            }
            if image_entity_count > remaining {
                return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
                    "selected semantic shape declaration scan exceeds its bounded lookup work"
                        .to_owned(),
                )));
            }
        }
    }
    for (address, symbol) in request.symbols().iter().copied().zip(resolved.into_iter()) {
        let Some(symbol) = symbol else {
            entries.push(SemanticShapeEntry {
                symbol: address,
                identity: None,
                origin: None,
                fact: SemanticShapeFact::Unavailable(SemanticShapeUnavailable::NotInView),
            });
            continue;
        };
        if let Some((image_index, entity_id, observed_identity)) = locations.get(&symbol).copied() {
            let image = opened_images.get(image_index).ok_or_else(|| {
                BuiltinModelError("selected semantic shape image scan lost its owner".to_owned())
            })?;
            let image_snapshot = activated.images().get(image_index).ok_or_else(|| {
                BuiltinModelError("selected semantic shape image authority disappeared".to_owned())
            })?;
            let entity = image.entity(entity_id).ok_or_else(|| {
                BuiltinModelError("selected semantic shape entity disappeared".to_owned())
            })?;
            if entity.version.identity() != observed_identity {
                return Err(BuiltinModelError(
                    "selected semantic shape identity changed within its immutable image"
                        .to_owned(),
                ));
            }
            let identity = identity_from_compiler(observed_identity);
            let origin = SemanticShapeSourceOrigin {
                source: selected_source.clone(),
                selection_root,
                semantic_image_bytes,
                image: Some(SemanticShapeImageOrigin {
                    image: image_snapshot.authority,
                    profile,
                }),
            };
            let fact = project_declaration(image, entity, package, view, profile, &mut meter);
            entries.push(SemanticShapeEntry {
                symbol: address,
                identity: Some(identity),
                origin: Some(origin),
                fact,
            });
        } else {
            let fallback_origin = Some(SemanticShapeSourceOrigin {
                source: selected_source.clone(),
                selection_root,
                semantic_image_bytes,
                image: None,
            });
            entries.push(SemanticShapeEntry {
                symbol: address,
                identity: None,
                origin: fallback_origin,
                fact: SemanticShapeFact::Unavailable(if activated.images().is_empty() {
                    SemanticShapeUnavailable::NoSelectedImage
                } else {
                    SemanticShapeUnavailable::MissingImageFact
                }),
            });
        }
    }
    let final_snapshot = daemon.engine().daemon().owner().snapshot();
    let final_source_root = super::super::view_publish::source_root(&final_snapshot)?;
    let final_semantic_root = super::super::view_publish::semantic_root(&final_snapshot)?;
    if (final_source_root, final_semantic_root) != (snapshot_source_root, snapshot_semantic_root)
        || published_roots != Some((final_source_root, final_semantic_root))
    {
        return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
            "workspace selection changed during semantic shape projection".to_owned(),
        )));
    }
    if semantic_authority.freshness(freshness_key, claim) != request.source().freshness {
        return Ok(CommandReply::Failed(CommandFailure::InvalidQuery(
            "semantic shape source freshness changed during projection".to_owned(),
        )));
    }
    let final_view_root = library.view().root();
    if !request.basis().matches(final_view_root) {
        return Ok(CommandReply::Failed(CommandFailure::WrongBasis {
            expected: final_view_root.into(),
            observed: request.basis(),
        }));
    }
    let batch = SemanticShapeBatch {
        basis: request.basis(),
        entries: entries.into_boxed_slice(),
    };
    batch.admit_against(request).map_err(|error| {
        BuiltinModelError(format!("semantic shape output failed admission: {error}"))
    })?;
    Ok(CommandReply::SemanticShapes(batch))
}

fn project_declaration(
    image: &SemanticImageView<'_>,
    entity: backend_semantic::ir::SemanticEntity,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    profile: LanguageProfile,
    meter: &mut ProjectionMeter,
) -> SemanticShapeFact {
    let result = (|| {
        meter.node(64)?;
        let language =
            project_language_facts(image, entity.id, entity.kind, profile, package, view, meter)?;
        let semantic_type = entity
            .semantic_type
            .and_then(|id| image.ty(id).map(|ty| (id, ty)));
        let function_type = semantic_type.as_ref().and_then(|(_, ty)| match ty {
            TypeExpr::Concrete(ConcreteType::Function {
                parameters,
                results,
                abi,
                variadic,
                unsafe_,
            }) => Some((*parameters, *results, *abi, *variadic, *unsafe_)),
            _ => None,
        });
        let go = image.go_extension(entity.id);
        let shape = match entity.kind {
            ItemKind::Function => {
                let callable =
                    if let Some((parameters, results, abi, variadic, unsafe_)) = function_type {
                        project_callable(
                            image, entity.id, parameters, results, abi, variadic, unsafe_, package,
                            view, meter,
                        )?
                    } else if let Some(go) = go {
                        project_type_list_callable(
                            image,
                            entity.id,
                            go.signature,
                            package,
                            view,
                            meter,
                        )?
                    } else if let Some((type_id, _)) = semantic_type.as_ref() {
                        SemanticDeclarationShape::Typed(project_type_id(
                            image,
                            *type_id,
                            package,
                            view,
                            meter,
                            &mut BTreeSet::new(),
                            0,
                        )?)
                    } else {
                        SemanticDeclarationShape::Typed(SemanticTypeFact::Unavailable(
                            SemanticTypeUnavailable::MissingTypeCoordinate,
                        ))
                    };
                callable
            }
            ItemKind::Record
            | ItemKind::Trait
            | ItemKind::Enum
            | ItemKind::Module
            | ItemKind::Namespace
            | ItemKind::Implementation
                if image
                    .entity_list(entity.members)
                    .is_some_and(|mut members| members.next().is_some())
                    || matches!(
                        entity.kind,
                        ItemKind::Record
                            | ItemKind::Trait
                            | ItemKind::Enum
                            | ItemKind::Module
                            | ItemKind::Namespace
                    ) =>
            {
                SemanticDeclarationShape::Aggregate(project_members(
                    image,
                    entity.members,
                    package,
                    view,
                    profile,
                    meter,
                )?)
            }
            _ => match semantic_type.as_ref() {
                Some((type_id, _)) => SemanticDeclarationShape::Typed(project_type_id(
                    image,
                    *type_id,
                    package,
                    view,
                    meter,
                    &mut BTreeSet::new(),
                    0,
                )?),
                None => SemanticDeclarationShape::Typed(SemanticTypeFact::Unavailable(
                    SemanticTypeUnavailable::MissingTypeCoordinate,
                )),
            },
        };
        Ok(SemanticShapeFact::Available { shape, language })
    })();
    result.unwrap_or_else(SemanticShapeFact::Unavailable)
}

fn project_language_facts(
    image: &SemanticImageView<'_>,
    entity: backend_semantic::ir::EntityId,
    kind: ItemKind,
    profile: LanguageProfile,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
) -> Result<SemanticShapeLanguageFacts, SemanticShapeUnavailable> {
    Ok(match profile {
        LanguageProfile::Rust(_) => match image.rust_extension(entity) {
            Some(facts) => SemanticShapeLanguageFacts::Partial {
                profile,
                facts: SemanticShapeLanguageFact::RustOwnership(facts.ownership),
            },
            None => SemanticShapeLanguageFacts::Unavailable { profile },
        },
        LanguageProfile::Go(_) => match image.go_extension(entity) {
            Some(facts) => SemanticShapeLanguageFacts::Partial {
                profile,
                facts: SemanticShapeLanguageFact::GoVariadic(facts.signature.variadic),
            },
            None => SemanticShapeLanguageFacts::Unavailable { profile },
        },
        LanguageProfile::Python(_) => {
            python_language_facts(kind, image.python_extension(entity), profile)
        }
        LanguageProfile::CSharp(_) => match image.csharp_extension(entity) {
            Some(facts) => SemanticShapeLanguageFacts::Partial {
                profile,
                facts: SemanticShapeLanguageFact::CSharp {
                    nullability: facts.nullability,
                    reference_kind: facts.reference_kind,
                    is_async: facts.effects.is_async,
                    is_iterator: facts.effects.is_iterator,
                    is_extension: facts.effects.is_extension,
                },
            },
            None => SemanticShapeLanguageFacts::Unavailable { profile },
        },
        LanguageProfile::TypeScript(_) => {
            let Some(facts) = image.typescript_extension(entity) else {
                return Ok(SemanticShapeLanguageFacts::Unavailable { profile });
            };
            SemanticShapeLanguageFacts::Partial {
                profile,
                facts: SemanticShapeLanguageFact::TypeScript {
                    declared: facts
                        .declared
                        .map(|ty| {
                            project_type_id(
                                image,
                                ty,
                                package,
                                view,
                                meter,
                                &mut BTreeSet::new(),
                                0,
                            )
                            .map(Box::new)
                        })
                        .transpose()?,
                    observed: facts
                        .observed
                        .map(|ty| {
                            project_type_id(
                                image,
                                ty,
                                package,
                                view,
                                meter,
                                &mut BTreeSet::new(),
                                0,
                            )
                            .map(Box::new)
                        })
                        .transpose()?,
                },
            }
        }
        LanguageProfile::Java(_) => {
            let Some(facts) = image.java_extension(entity) else {
                return Ok(SemanticShapeLanguageFacts::Unavailable { profile });
            };
            let mut throws = Vec::new();
            let Some(types) = image.types(facts.throws) else {
                return Err(SemanticShapeUnavailable::MissingImageFact);
            };
            for ty in types {
                throws.push(project_type_id(
                    image,
                    ty,
                    package,
                    view,
                    meter,
                    &mut BTreeSet::new(),
                    0,
                )?);
            }
            let mut annotations = Vec::new();
            let Some(atoms) = image.atom_list(facts.annotations) else {
                return Err(SemanticShapeUnavailable::MissingImageFact);
            };
            for atom in atoms {
                meter.node(48)?;
                annotations.push(atom_text(image, atom, meter)?);
            }
            SemanticShapeLanguageFacts::Partial {
                profile,
                facts: SemanticShapeLanguageFact::Java {
                    throws: throws.into_boxed_slice(),
                    annotations: annotations.into_boxed_slice(),
                },
            }
        }
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) => match image.clang_extension(entity) {
            Some(facts) => SemanticShapeLanguageFacts::Partial {
                profile,
                facts: SemanticShapeLanguageFact::Clang {
                    is_const: facts.qualifiers.is_const,
                    is_volatile: facts.qualifiers.is_volatile,
                    is_restrict: facts.qualifiers.is_restrict,
                    size_bits: facts.layout.size_bits,
                    align_bits: facts.layout.align_bits,
                },
            },
            None => SemanticShapeLanguageFacts::Unavailable { profile },
        },
    })
}

fn project_members(
    image: &SemanticImageView<'_>,
    members: backend_semantic::ir::EntityListId,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    profile: LanguageProfile,
    meter: &mut ProjectionMeter,
) -> Result<Box<[SemanticShapeMember]>, SemanticShapeUnavailable> {
    let Some(member_ids) = image.entity_list(members) else {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    };
    let mut result = Vec::new();
    for member_id in member_ids {
        let Some(member) = image.entity(member_id) else {
            return Err(SemanticShapeUnavailable::MissingImageFact);
        };
        let name = atom_text(image, member.name, meter)?;
        // `atom_text` already charged the member spelling.
        meter.node(160)?;
        meter.node(64)?;
        let language =
            project_language_facts(image, member.id, member.kind, profile, package, view, meter)?;
        let ty = match member.semantic_type {
            Some(id) => project_type_id(image, id, package, view, meter, &mut BTreeSet::new(), 0)?,
            None => {
                meter.node(96)?;
                SemanticTypeFact::Unavailable(SemanticTypeUnavailable::MissingTypeCoordinate)
            }
        };
        result.push(SemanticShapeMember {
            identity: identity_from_compiler(member.version.identity()),
            name,
            kind: member.kind,
            ty,
            language,
        });
    }
    Ok(result.into_boxed_slice())
}

fn python_language_facts(
    kind: ItemKind,
    facts: Option<backend_semantic::ir::PythonFacts>,
    profile: LanguageProfile,
) -> SemanticShapeLanguageFacts {
    match facts {
        Some(facts) if kind == ItemKind::Parameter => SemanticShapeLanguageFacts::Partial {
            profile,
            facts: SemanticShapeLanguageFact::PythonParameter {
                kind: facts.parameter_kind,
                confidence: facts.dynamic_confidence,
            },
        },
        Some(_) => SemanticShapeLanguageFacts::CommonOnly { profile },
        None => SemanticShapeLanguageFacts::Unavailable { profile },
    }
}

fn project_callable_carrier_bindings(
    image: &SemanticImageView<'_>,
    owner: backend_semantic::ir::EntityId,
    parameter_count: usize,
    result_count: usize,
    meter: &mut ProjectionMeter,
) -> Result<SemanticCallableCarrierBindings, SemanticShapeUnavailable> {
    let observation = image
        .signature_carrier_bindings(owner)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
    let SignatureCarrierBindingsObservation::Captured(mut bindings) = observation else {
        return Ok(SemanticCallableCarrierBindings::Unavailable);
    };

    let parameter_count_u32 =
        u32::try_from(parameter_count).map_err(|_| SemanticShapeUnavailable::MissingImageFact)?;
    let result_count_u32 =
        u32::try_from(result_count).map_err(|_| SemanticShapeUnavailable::MissingImageFact)?;
    let expected_count = parameter_count
        .checked_add(result_count)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
    parameter_count_u32
        .checked_add(result_count_u32)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
    if bindings.len() != expected_count {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    }

    let identity_bytes = expected_count
        .checked_mul(backend_library::SEMANTIC_SHAPE_CARRIER_IDENTITY_BYTES)
        .ok_or(SemanticShapeUnavailable::ByteBudget)?;
    meter.preflight_nodes_bytes(expected_count, identity_bytes)?;

    let mut parameters = Vec::new();
    parameters
        .try_reserve_exact(parameter_count)
        .map_err(|_| SemanticShapeUnavailable::ByteBudget)?;
    let mut results = Vec::new();
    results
        .try_reserve_exact(result_count)
        .map_err(|_| SemanticShapeUnavailable::ByteBudget)?;

    for position in 0..parameter_count_u32 {
        let binding = bindings
            .next()
            .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
        let identity = callable_carrier_identity(
            image,
            owner,
            binding,
            SignatureCarrierBindingRole::Parameter,
            position,
        )?;
        parameters.push(identity);
    }
    for position in 0..result_count_u32 {
        let binding = bindings
            .next()
            .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
        let identity = callable_carrier_identity(
            image,
            owner,
            binding,
            SignatureCarrierBindingRole::Result,
            position,
        )?;
        results.push(identity);
    }
    if bindings.next().is_some() {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    }
    meter.nodes_bytes(expected_count, identity_bytes)?;

    Ok(SemanticCallableCarrierBindings::Captured {
        parameters: parameters.into_boxed_slice(),
        results: results.into_boxed_slice(),
    })
}

fn callable_carrier_identity(
    image: &SemanticImageView<'_>,
    owner: backend_semantic::ir::EntityId,
    binding: backend_semantic::ir::SignatureCarrierBinding,
    expected_role: SignatureCarrierBindingRole,
    expected_position: u32,
) -> Result<backend_library::SemanticDeclarationIdentity, SemanticShapeUnavailable> {
    if binding.owner != owner
        || binding.role != expected_role
        || binding.position != expected_position
    {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    }
    let target = image
        .entity(binding.carrier)
        .filter(|entity| entity.kind == ItemKind::Parameter)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
    let observed_role = image
        .signature_carrier_role(binding.carrier)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
    let role_matches = matches!(
        (expected_role, observed_role),
        (
            SignatureCarrierBindingRole::Parameter,
            SignatureCarrierRoleObservation::Captured(
                SignatureCarrierRole::Input | SignatureCarrierRole::Both
            )
        ) | (
            SignatureCarrierBindingRole::Result,
            SignatureCarrierRoleObservation::Captured(
                SignatureCarrierRole::Result | SignatureCarrierRole::Both
            )
        )
    );
    if !role_matches {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    }
    Ok(identity_from_compiler(target.version.identity()))
}

fn project_callable(
    image: &SemanticImageView<'_>,
    owner: backend_semantic::ir::EntityId,
    parameters: backend_semantic::ir::TupleElementListId,
    results: backend_semantic::ir::TupleElementListId,
    abi: Option<backend_semantic::ir::AtomId>,
    variadic: FunctionVariadicForm,
    unsafe_: bool,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
) -> Result<SemanticDeclarationShape, SemanticShapeUnavailable> {
    let parameter_count = image
        .tuple_elements(parameters)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?
        .len();
    let result_count = image
        .tuple_elements(results)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?
        .len();
    let carrier_bindings =
        project_callable_carrier_bindings(image, owner, parameter_count, result_count, meter)?;
    let parameters = tuple_elements_with_context(
        image,
        parameters,
        package,
        view,
        meter,
        &mut BTreeSet::new(),
        0,
    )?;
    let results = tuple_elements_with_context(
        image,
        results,
        package,
        view,
        meter,
        &mut BTreeSet::new(),
        0,
    )?;
    let abi = abi.map(|atom| atom_text(image, atom, meter)).transpose()?;
    Ok(SemanticDeclarationShape::Callable(SemanticCallableShape {
        parameters,
        results,
        carrier_bindings,
        abi,
        variadic,
        unsafe_,
    }))
}

fn project_type_list_callable(
    image: &SemanticImageView<'_>,
    owner: backend_semantic::ir::EntityId,
    signature: backend_semantic::ir::GoSignature,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
) -> Result<SemanticDeclarationShape, SemanticShapeUnavailable> {
    let parameter_count = image
        .types(signature.parameters)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?
        .len();
    let result_count = image
        .types(signature.results)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?
        .len();
    let carrier_bindings =
        project_callable_carrier_bindings(image, owner, parameter_count, result_count, meter)?;
    let project = |list, meter: &mut ProjectionMeter| {
        let Some(types) = image.types(list) else {
            return Err(SemanticShapeUnavailable::MissingImageFact);
        };
        let mut elements = Vec::new();
        for ty in types {
            meter.node(48)?;
            elements.push(SemanticTypeElement {
                label: None,
                kind: TupleElementKind::Required,
                ty: project_type_id(image, ty, package, view, meter, &mut BTreeSet::new(), 0)?,
            });
        }
        Ok(elements.into_boxed_slice())
    };
    let parameters = project(signature.parameters, meter)?;
    let results = project(signature.results, meter)?;
    Ok(SemanticDeclarationShape::Callable(SemanticCallableShape {
        parameters,
        results,
        carrier_bindings,
        abi: None,
        variadic: if signature.variadic {
            FunctionVariadicForm::TypedLast
        } else {
            FunctionVariadicForm::None
        },
        unsafe_: false,
    }))
}

fn project_type_id(
    image: &SemanticImageView<'_>,
    id: TypeId,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
    stack: &mut BTreeSet<TypeId>,
    depth: usize,
) -> Result<SemanticTypeFact, SemanticShapeUnavailable> {
    // The fact wrapper itself is one admitted node. The type expression, if
    // present, is charged separately by `project_type_expr`.
    meter.node(96)?;
    // Keep the bounded Unavailable marker itself at an admitted depth. If we
    // descended one more level and only then stopped, the marker would sit at
    // depth MAX + 1 and wire admission would reject the whole result.
    if depth >= backend_library::MAX_SEMANTIC_SHAPE_DEPTH {
        return Ok(SemanticTypeFact::Unavailable(
            SemanticTypeUnavailable::DepthBudget,
        ));
    }
    if !stack.insert(id) {
        return Ok(SemanticTypeFact::Unavailable(
            SemanticTypeUnavailable::StructuralCycle,
        ));
    }
    let Some(expression) = image.ty(id) else {
        stack.remove(&id);
        return Ok(SemanticTypeFact::Unavailable(
            SemanticTypeUnavailable::MissingImageFact,
        ));
    };
    let result = project_type_expr(image, expression, package, view, meter, stack, depth);
    stack.remove(&id);
    result
}

fn project_type_expr(
    image: &SemanticImageView<'_>,
    expression: TypeExpr,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
    stack: &mut BTreeSet<TypeId>,
    depth: usize,
) -> Result<SemanticTypeFact, SemanticShapeUnavailable> {
    if depth > backend_library::MAX_SEMANTIC_SHAPE_DEPTH {
        return Ok(SemanticTypeFact::Unavailable(
            SemanticTypeUnavailable::DepthBudget,
        ));
    }
    let expression_bytes = match expression {
        TypeExpr::Concrete(ConcreteType::Nominal(_)) => 384,
        TypeExpr::Concrete(ConcreteType::External(_)) => 224,
        _ => 64,
    };
    meter.node(expression_bytes)?;
    match expression {
        TypeExpr::Unknown(unknown) => Ok(SemanticTypeFact::Unknown {
            reason: unknown.reason,
            spelling: unknown
                .spelling
                .map(|atom| atom_text(image, atom, meter))
                .transpose()?,
        }),
        TypeExpr::Computed(computed) => Ok(SemanticTypeFact::Unsupported {
            tag: computed.tag(),
        }),
        TypeExpr::Concrete(concrete) => {
            use ConcreteType as C;
            let expression = match concrete {
                C::Builtin(value) => SemanticTypeExpr::Builtin(value),
                C::Literal(value) => match value {
                    LiteralType::String(atom) => SemanticTypeExpr::Literal(
                        SemanticLiteral::String(atom_text(image, atom, meter)?),
                    ),
                    LiteralType::Number(atom) => SemanticTypeExpr::Literal(
                        SemanticLiteral::Number(atom_text(image, atom, meter)?),
                    ),
                    LiteralType::BigInt(atom) => SemanticTypeExpr::Literal(
                        SemanticLiteral::BigInt(atom_text(image, atom, meter)?),
                    ),
                    LiteralType::Boolean(value) => {
                        SemanticTypeExpr::Literal(SemanticLiteral::Boolean(value))
                    }
                    LiteralType::Null => SemanticTypeExpr::Literal(SemanticLiteral::Null),
                    LiteralType::Undefined => SemanticTypeExpr::Literal(SemanticLiteral::Undefined),
                },
                C::Nominal(entity_id) => {
                    let Some(target) = image.entity(entity_id) else {
                        return Ok(SemanticTypeFact::Unavailable(
                            SemanticTypeUnavailable::MissingImageFact,
                        ));
                    };
                    let identity = target.version.identity();
                    let symbol = view_build::semantic_symbol(package, identity);
                    let symbol = view
                        .row_ref(backend_engine::RowId::Symbol(symbol))
                        .filter(|_| meter.visible_symbol_proof(view, symbol))
                        .map(|_| SymbolAddress::selected(symbol));
                    SemanticTypeExpr::Nominal {
                        declaration: identity_from_compiler(identity),
                        symbol,
                    }
                }
                C::External(external) => {
                    let identity = ExternalTargetIdentity::capture(image, external)
                        .map_err(|_| SemanticShapeUnavailable::MissingImageFact)?;
                    let display = image.external(external).and_then(|target| match target {
                        ExternalTarget::Foreign(target) => Some(target.display),
                        ExternalTarget::FragmentEntity { display, .. } => Some(display),
                        ExternalTarget::Stable { .. } => None,
                    });
                    SemanticTypeExpr::External {
                        identity: *identity.as_bytes(),
                        display: display
                            .map(|atom| atom_text(image, atom, meter))
                            .transpose()?,
                    }
                }
                C::Parameter(atom) => SemanticTypeExpr::Parameter(atom_text(image, atom, meter)?),
                C::Applied {
                    constructor,
                    arguments,
                } => {
                    let constructor = Box::new(project_type_id(
                        image,
                        constructor,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?);
                    let arguments = project_type_list(
                        image,
                        arguments,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?;
                    SemanticTypeExpr::Applied {
                        constructor,
                        arguments,
                    }
                }
                C::Tuple(elements) => SemanticTypeExpr::Tuple(tuple_elements_with_context(
                    image,
                    elements,
                    package,
                    view,
                    meter,
                    stack,
                    depth + 1,
                )?),
                C::Object(members) => SemanticTypeExpr::Object(object_members(
                    image,
                    members,
                    package,
                    view,
                    meter,
                    stack,
                    depth + 1,
                )?),
                C::Function {
                    parameters,
                    results,
                    abi,
                    variadic,
                    unsafe_,
                } => {
                    let parameters = tuple_elements_with_context(
                        image,
                        parameters,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?;
                    let results = tuple_elements_with_context(
                        image,
                        results,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?;
                    let abi = abi.map(|atom| atom_text(image, atom, meter)).transpose()?;
                    SemanticTypeExpr::Function(Box::new(SemanticCallableShape {
                        parameters,
                        results,
                        carrier_bindings: SemanticCallableCarrierBindings::Unavailable,
                        abi,
                        variadic,
                        unsafe_,
                    }))
                }
                C::Reference {
                    target,
                    mutability,
                    lifetime,
                } => SemanticTypeExpr::Reference {
                    target: Box::new(project_type_id(
                        image,
                        target,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?),
                    mutable: matches!(mutability, backend_semantic::ir::Mutability::Mutable),
                    lifetime: lifetime
                        .map(|atom| atom_text(image, atom, meter))
                        .transpose()?,
                },
                C::Pointer { target, mutability } => SemanticTypeExpr::Pointer {
                    target: Box::new(project_type_id(
                        image,
                        target,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?),
                    mutable: matches!(mutability, backend_semantic::ir::Mutability::Mutable),
                },
                C::Slice(target) => SemanticTypeExpr::Slice(Box::new(project_type_id(
                    image,
                    target,
                    package,
                    view,
                    meter,
                    stack,
                    depth + 1,
                )?)),
                C::Array { element, shape } => {
                    let shape = match shape {
                        ArrayShape::Sequence => SemanticArrayShape::Sequence,
                        ArrayShape::Rectangular { rank } => {
                            SemanticArrayShape::Rectangular { rank }
                        }
                        ArrayShape::FixedValue { length } => {
                            SemanticArrayShape::FixedValue { length }
                        }
                        ArrayShape::ConstExpression(atom) => {
                            SemanticArrayShape::ConstExpression(atom_text(image, atom, meter)?)
                        }
                        ArrayShape::Incomplete => SemanticArrayShape::Incomplete,
                    };
                    SemanticTypeExpr::Array {
                        element: Box::new(project_type_id(
                            image,
                            element,
                            package,
                            view,
                            meter,
                            stack,
                            depth + 1,
                        )?),
                        shape,
                    }
                }
                C::Optional(target) => SemanticTypeExpr::Optional(Box::new(project_type_id(
                    image,
                    target,
                    package,
                    view,
                    meter,
                    stack,
                    depth + 1,
                )?)),
                C::Union(types) => SemanticTypeExpr::Union(project_type_list(
                    image,
                    types,
                    package,
                    view,
                    meter,
                    stack,
                    depth + 1,
                )?),
                C::Intersection(types) => SemanticTypeExpr::Intersection(project_type_list(
                    image,
                    types,
                    package,
                    view,
                    meter,
                    stack,
                    depth + 1,
                )?),
                C::Map { key, value } => SemanticTypeExpr::Map {
                    key: Box::new(project_type_id(
                        image,
                        key,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?),
                    value: Box::new(project_type_id(
                        image,
                        value,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?),
                },
                C::Channel { direction, element } => SemanticTypeExpr::Channel {
                    direction,
                    element: Box::new(project_type_id(
                        image,
                        element,
                        package,
                        view,
                        meter,
                        stack,
                        depth + 1,
                    )?),
                },
                other => SemanticTypeExpr::Unsupported { tag: other.tag() },
            };
            Ok(SemanticTypeFact::Known(expression))
        }
    }
}

fn project_type_list(
    image: &SemanticImageView<'_>,
    list: backend_semantic::ir::TypeListId,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
    stack: &mut BTreeSet<TypeId>,
    depth: usize,
) -> Result<Box<[SemanticTypeFact]>, SemanticShapeUnavailable> {
    let Some(types) = image.types(list) else {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    };
    let mut result = Vec::new();
    for ty in types {
        result.push(project_type_id(
            image, ty, package, view, meter, stack, depth,
        )?);
    }
    Ok(result.into_boxed_slice())
}

fn tuple_elements_with_context(
    image: &SemanticImageView<'_>,
    list: backend_semantic::ir::TupleElementListId,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
    stack: &mut BTreeSet<TypeId>,
    depth: usize,
) -> Result<Box<[SemanticTypeElement]>, SemanticShapeUnavailable> {
    let Some(elements) = image.tuple_elements(list) else {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    };
    let mut result = Vec::new();
    for element in elements {
        let label = element
            .label
            .map(|atom| atom_text(image, atom, meter))
            .transpose()?;
        meter.node(48)?;
        let ty = project_type_id(image, element.ty, package, view, meter, stack, depth)?;
        result.push(SemanticTypeElement {
            label,
            kind: element.kind,
            ty,
        });
    }
    Ok(result.into_boxed_slice())
}

fn object_members(
    image: &SemanticImageView<'_>,
    list: backend_semantic::ir::ObjectMemberListId,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
    stack: &mut BTreeSet<TypeId>,
    depth: usize,
) -> Result<Box<[SemanticObjectMember]>, SemanticShapeUnavailable> {
    let Some(members) = image.object_members(list) else {
        return Err(SemanticShapeUnavailable::MissingImageFact);
    };
    let mut result = Vec::new();
    for member in members {
        let output = match member {
            ObjectMember::Property {
                key,
                ty,
                optional,
                readonly,
            } => SemanticObjectMember::Property {
                key: property_key(image, key, package, view, meter, stack, depth)?,
                ty: project_type_id(image, ty, package, view, meter, stack, depth)?,
                optional,
                readonly,
            },
            ObjectMember::Method {
                key,
                signature,
                optional,
            } => SemanticObjectMember::Method {
                key: property_key(image, key, package, view, meter, stack, depth)?,
                signature: project_type_id(image, signature, package, view, meter, stack, depth)?,
                optional,
            },
            ObjectMember::Index {
                parameter,
                key,
                value,
                readonly,
            } => SemanticObjectMember::Index {
                parameter: atom_text(image, parameter, meter)?,
                key: project_type_id(image, key, package, view, meter, stack, depth)?,
                value: project_type_id(image, value, package, view, meter, stack, depth)?,
                readonly,
            },
            ObjectMember::Call(signature) => SemanticObjectMember::Call(project_type_id(
                image, signature, package, view, meter, stack, depth,
            )?),
            ObjectMember::Construct(signature) => SemanticObjectMember::Construct(project_type_id(
                image, signature, package, view, meter, stack, depth,
            )?),
        };
        meter.node(96)?;
        result.push(output);
    }
    Ok(result.into_boxed_slice())
}

fn property_key(
    image: &SemanticImageView<'_>,
    key: PropertyKey,
    package: backend_engine::PackageKey,
    view: &backend_engine::ViewRoot,
    meter: &mut ProjectionMeter,
    stack: &mut BTreeSet<TypeId>,
    depth: usize,
) -> Result<SemanticPropertyKey, SemanticShapeUnavailable> {
    Ok(match key {
        PropertyKey::Named(atom) => SemanticPropertyKey::Named(atom_text(image, atom, meter)?),
        PropertyKey::Private(atom) => SemanticPropertyKey::Private(atom_text(image, atom, meter)?),
        PropertyKey::Numeric(atom) => SemanticPropertyKey::Numeric(atom_text(image, atom, meter)?),
        PropertyKey::Computed(ty) => SemanticPropertyKey::Computed(project_type_id(
            image, ty, package, view, meter, stack, depth,
        )?),
    })
}

fn atom_text(
    image: &SemanticImageView<'_>,
    atom: backend_semantic::ir::AtomId,
    meter: &mut ProjectionMeter,
) -> Result<backend_library::SourceAtomText, SemanticShapeUnavailable> {
    let bytes = image
        .atom(atom)
        .ok_or(SemanticShapeUnavailable::MissingImageFact)?;
    if bytes.len() > backend_library::MAX_PRODUCT_TEXT_BYTES {
        return Err(SemanticShapeUnavailable::ByteBudget);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SemanticShapeUnavailable::InvalidText)?;
    meter.text(text.len())?;
    backend_library::SourceAtomText::new(text).map_err(|_| SemanticShapeUnavailable::InvalidText)
}

fn identity_from_compiler(
    identity: DeclarationIdentity,
) -> backend_library::SemanticDeclarationIdentity {
    backend_library::SemanticDeclarationIdentity {
        family: *identity.family.as_bytes(),
        variant: *identity.variant.as_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ItemKind, ProjectionMeter, SemanticCallableCarrierBindings, SemanticDeclarationShape,
        SemanticTypeExpr, atom_text, python_language_facts,
    };
    use backend_semantic::{
        ir::{
            AtomListId, BorrowedTree, ConcreteType, Confidence, CorePayloadHash,
            DeclarationFamilyId, EntityAuthorityFacts, EntityId, EntityVersion, FactAvailability,
            FunctionVariadicForm, IrBuilder, LiteralType, ObjectMember, ParentageAuthority,
            PropertyKey, PythonFacts, PythonParameterKind, SemanticImageView, SemanticReader,
            SignatureCarrierOwnerInput, TreeItemInput, TypeExpr, VariantFingerprint, Visibility,
            encode_full_semantic_image, full_semantic_image_len,
        },
        vocabulary::{LanguageProfile, PythonVersion, RustEdition},
    };
    use std::collections::BTreeSet;

    #[test]
    fn python_parameter_convention_is_not_projected_from_a_function_row() {
        let profile = LanguageProfile::Python(PythonVersion::Python312);
        let facts = PythonFacts {
            decorators: AtomListId::new(0),
            parameter_kind: PythonParameterKind::KeywordOnly,
            dynamic_confidence: Confidence::Compiler,
        };

        assert_eq!(
            python_language_facts(ItemKind::Function, Some(facts), profile),
            backend_library::SemanticShapeLanguageFacts::CommonOnly { profile },
        );
        assert_eq!(
            python_language_facts(ItemKind::Parameter, Some(facts), profile),
            backend_library::SemanticShapeLanguageFacts::Partial {
                profile,
                facts: backend_library::SemanticShapeLanguageFact::PythonParameter {
                    kind: PythonParameterKind::KeywordOnly,
                    confidence: Confidence::Compiler,
                },
            },
        );
        assert_eq!(
            python_language_facts(ItemKind::Parameter, None, profile),
            backend_library::SemanticShapeLanguageFacts::Unavailable { profile },
        );
    }

    #[test]
    fn owned_callable_binding_is_captured_empty_but_nested_function_is_unavailable() {
        let mut builder = IrBuilder::new();
        let parameters = builder
            .intern_tuple_elements(&[])
            .expect("empty function parameter list interns");
        let results = builder
            .intern_tuple_elements(&[])
            .expect("empty function result list interns");
        let function_type = builder
            .intern_type(TypeExpr::Concrete(ConcreteType::Function {
                parameters,
                results,
                abi: None,
                variadic: FunctionVariadicForm::None,
                unsafe_: false,
            }))
            .expect("function type interns");
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw([61; 16]),
            variant: VariantFingerprint::from_raw([62; 16]),
            core_payload: CorePayloadHash::from_raw([63; 16]),
        };
        let legacy_version = EntityVersion {
            family: DeclarationFamilyId::from_raw([64; 16]),
            variant: VariantFingerprint::from_raw([65; 16]),
            core_payload: CorePayloadHash::from_raw([66; 16]),
        };
        let item = TreeItemInput {
            name: b"zero",
            kind: ItemKind::Function,
            visibility: Visibility::Private,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                semantic_type: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: Some(function_type),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        };
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[version, legacy_version],
                items: &[
                    item,
                    TreeItemInput {
                        name: b"unavailable",
                        ..item
                    },
                ],
                links: &[],
            })
            .expect("function owner enters the image");
        builder
            .capture_signature_carrier_bindings(
                &[
                    SignatureCarrierOwnerInput::captured(EntityId::new(0), 0, 0),
                    SignatureCarrierOwnerInput::unavailable(EntityId::new(1)),
                ],
                &[],
            )
            .expect("capture distinguishes known-empty and unavailable owners");
        let ir = builder.finish().expect("compiler image finalizes");
        let image_length = full_semantic_image_len(&ir).expect("image length is bounded");
        let mut bytes = vec![0; image_length];
        encode_full_semantic_image(&ir, &mut bytes).expect("function image encodes");
        let image = SemanticImageView::reopen(&bytes).expect("encoded image reopens");

        let package = backend_engine::package_key("semantic-callable-carrier-test");
        let basis = backend_engine::Basis::new(
            backend_engine::view_state_root(&[]),
            backend_engine::object_version(b"semantic-callable-carrier-test"),
        );
        let frontier =
            backend_engine::Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0);
        let view = backend_engine::ViewRoot::new_incomplete(
            backend_engine::view_key(b"semantic-callable-carrier-test"),
            basis,
            frontier,
            vec![],
            vec![],
        )
        .expect("empty projection view is coherent");
        let mut meter = ProjectionMeter {
            nodes: 0,
            bytes: 0,
            max_nodes: backend_library::MAX_SEMANTIC_SHAPE_NODES,
            max_bytes: backend_library::MAX_SEMANTIC_SHAPE_BYTES,
            proof_symbols: BTreeSet::new(),
            proof_packages: BTreeSet::new(),
            omitted_proof_symbols: BTreeSet::new(),
        };
        let owner = image.entity(EntityId::new(0)).expect("owner entity exists");
        let fact = super::project_declaration(
            &image,
            owner,
            package,
            &view,
            LanguageProfile::Rust(RustEdition::Rust2021),
            &mut meter,
        );
        let backend_library::SemanticShapeFact::Available { shape, .. } = fact else {
            panic!("owner projection retains the complete callable");
        };
        let SemanticDeclarationShape::Callable(callable) = shape else {
            panic!("Function owner projects to a callable shape");
        };
        assert_eq!(
            callable.carrier_bindings,
            SemanticCallableCarrierBindings::Captured {
                parameters: Box::new([]),
                results: Box::new([]),
            }
        );

        let unavailable_owner = image
            .entity(EntityId::new(1))
            .expect("second function owner exists");
        let unavailable_fact = super::project_declaration(
            &image,
            unavailable_owner,
            package,
            &view,
            LanguageProfile::Rust(RustEdition::Rust2021),
            &mut meter,
        );
        let backend_library::SemanticShapeFact::Available {
            shape: SemanticDeclarationShape::Callable(unavailable_callable),
            ..
        } = unavailable_fact
        else {
            panic!("unavailable capture preserves the callable's type shape");
        };
        assert_eq!(
            unavailable_callable.carrier_bindings,
            SemanticCallableCarrierBindings::Unavailable,
            "an unproven owner does not become captured-empty"
        );

        let nested = super::project_type_id(
            &image,
            function_type,
            package,
            &view,
            &mut meter,
            &mut BTreeSet::new(),
            0,
        )
        .expect("nested function type projects");
        let backend_library::SemanticTypeFact::Known(SemanticTypeExpr::Function(nested)) = nested
        else {
            panic!("function type projects to a nested callable expression");
        };
        assert_eq!(
            nested.carrier_bindings,
            SemanticCallableCarrierBindings::Unavailable,
            "a structural function type has no direct declaration owner"
        );

        assert_eq!(
            super::project_callable_carrier_bindings(&image, EntityId::new(99), 0, 0, &mut meter,),
            Err(backend_library::SemanticShapeUnavailable::MissingImageFact),
            "an invalid owner cannot be reported as unavailable capture"
        );
    }

    #[test]
    fn image_atom_projection_preserves_empty_space_nul_unicode_and_padding() {
        let spellings: [&[u8]; 5] = [b"", b" ", b"\0", "λ雪".as_bytes(), b" padded "];
        let expected = ["", " ", "\0", "λ雪", " padded "];
        let mut builder = IrBuilder::new();
        let mut atoms = Vec::with_capacity(spellings.len());
        let mut literal_types = Vec::with_capacity(spellings.len());
        for spelling in spellings {
            let atom = builder
                .intern_atom(spelling)
                .expect("exact compiler atom interns");
            atoms.push(atom);
            literal_types.push(
                builder
                    .intern_type(TypeExpr::Concrete(ConcreteType::Literal(
                        LiteralType::String(atom),
                    )))
                    .expect("literal type interns"),
            );
        }
        let keys = [
            PropertyKey::Named(atoms[0]),
            PropertyKey::Private(atoms[1]),
            PropertyKey::Numeric(atoms[2]),
            PropertyKey::Named(atoms[3]),
            PropertyKey::Private(atoms[4]),
        ];
        let members = keys
            .into_iter()
            .zip(literal_types.iter().copied())
            .map(|(key, ty)| ObjectMember::Property {
                key,
                ty,
                optional: false,
                readonly: false,
            })
            .collect::<Vec<_>>();
        let member_list = builder
            .intern_object_members(&members)
            .expect("object member list interns");
        let object_type = builder
            .intern_type(TypeExpr::Concrete(ConcreteType::Object(member_list)))
            .expect("object type interns");
        let version = EntityVersion {
            family: DeclarationFamilyId::from_raw([1; 16]),
            variant: VariantFingerprint::from_raw([2; 16]),
            core_payload: CorePayloadHash::from_raw([3; 16]),
        };
        let item = TreeItemInput {
            name: b"shape",
            kind: ItemKind::Record,
            visibility: Visibility::Private,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                semantic_type: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: Some(object_type),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        };
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &[version],
                items: &[item],
                links: &[],
            })
            .expect("typed compiler declaration enters image");
        let ir = builder.finish().expect("complete semantic image builds");
        let image_length = full_semantic_image_len(&ir).expect("image length is admitted");
        let mut bytes = vec![0; image_length];
        encode_full_semantic_image(&ir, &mut bytes).expect("complete image encodes");
        let image = SemanticImageView::reopen(&bytes).expect("encoded image reopens");

        let mut meter = ProjectionMeter {
            nodes: 0,
            bytes: 0,
            max_nodes: backend_library::MAX_SEMANTIC_SHAPE_NODES,
            max_bytes: backend_library::MAX_SEMANTIC_SHAPE_BYTES,
            proof_symbols: BTreeSet::new(),
            proof_packages: BTreeSet::new(),
            omitted_proof_symbols: BTreeSet::new(),
        };
        let entity = image
            .entity(EntityId::new(0))
            .expect("image preserves the compiler declaration");
        let object_type = entity
            .semantic_type
            .expect("declaration retains its typed shape");
        let Some(TypeExpr::Concrete(ConcreteType::Object(member_list))) = image.ty(object_type)
        else {
            panic!("image preserves object type");
        };
        let members = image
            .object_members(member_list)
            .expect("image preserves object members")
            .collect::<Vec<_>>();
        assert_eq!(
            members.len(),
            expected.len(),
            "all five source members survive"
        );

        let source_atom = |spelling| {
            backend_engine::SourceAtomText::new(spelling).expect("expected source atom is bounded")
        };
        let expected_keys = [
            backend_engine::SemanticPropertyKey::Named(source_atom("")),
            backend_engine::SemanticPropertyKey::Private(source_atom(" ")),
            backend_engine::SemanticPropertyKey::Numeric(source_atom("\0")),
            backend_engine::SemanticPropertyKey::Named(source_atom("λ雪")),
            backend_engine::SemanticPropertyKey::Private(source_atom(" padded ")),
        ];
        for ((member, expected_key), expected_value) in
            members.into_iter().zip(expected_keys.iter()).zip(expected)
        {
            let ObjectMember::Property { key, ty, .. } = member else {
                panic!("image preserves exact static property key variant");
            };
            let projected_key = match key {
                PropertyKey::Named(atom) => backend_engine::SemanticPropertyKey::Named(
                    atom_text(&image, atom, &mut meter).expect("exact named property text"),
                ),
                PropertyKey::Private(atom) => backend_engine::SemanticPropertyKey::Private(
                    atom_text(&image, atom, &mut meter).expect("exact private property text"),
                ),
                PropertyKey::Numeric(atom) => backend_engine::SemanticPropertyKey::Numeric(
                    atom_text(&image, atom, &mut meter).expect("exact numeric property text"),
                ),
                PropertyKey::Computed(_) => panic!("image preserves static property key"),
            };
            assert_eq!(&projected_key, expected_key);
            let Some(TypeExpr::Concrete(ConcreteType::Literal(LiteralType::String(value_atom)))) =
                image.ty(ty)
            else {
                panic!("image preserves the property's string literal type");
            };
            let projected_value =
                atom_text(&image, value_atom, &mut meter).expect("exact literal text");
            assert_eq!(projected_value.as_str(), expected_value);
        }

        let expected_fact =
            backend_engine::SemanticTypeFact::Known(backend_engine::SemanticTypeExpr::Object(
                expected_keys
                    .iter()
                    .cloned()
                    .zip(expected)
                    .map(
                        |(key, value)| backend_engine::SemanticObjectMember::Property {
                            key,
                            ty: backend_engine::SemanticTypeFact::Known(
                                backend_engine::SemanticTypeExpr::Literal(
                                    backend_engine::SemanticLiteral::String(source_atom(value)),
                                ),
                            ),
                            optional: false,
                            readonly: false,
                        },
                    )
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ));
        let root = backend_engine::view_state_root(&[]);
        let basis = backend_engine::Basis::new(root, backend_engine::object_version(b"shape-test"));
        let frontier =
            backend_engine::Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0);
        let view = backend_engine::ViewRoot::new_incomplete(
            backend_engine::view_key(b"semantic-shapes-test"),
            basis,
            frontier,
            vec![],
            vec![],
        )
        .expect("projection receives a coherent empty view root");
        let projected_fact = super::project_type_id(
            &image,
            object_type,
            backend_engine::package_key("semantic-shapes-test"),
            &view,
            &mut meter,
            &mut BTreeSet::new(),
            0,
        )
        .expect("production type projector preserves the reopened object");
        assert_eq!(projected_fact, expected_fact);
    }
}
