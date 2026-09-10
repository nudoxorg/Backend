//! Defines projector behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the projector invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The one projector: a borrowed semantic reader on one side, owned document values on the other.

use compiler_ir::{
    EntityId, ExternalId, ExternalTarget, ForeignTargetOrigin, LinkKind, LinkTarget, SemanticReader,
    SemanticImageAuthority, Visibility,
};
use compiler_ir_vocabulary::EntityKind;
use compiler_vocabulary::Language;
use interface_identity::{ContentKey, PackageCoordinate, SymbolPath};

use crate::{
    Census, Count, Direction, ExternalRef, ForeignOrigin, MemberGroup, MemberRow, MissingPool, Name,
    Outline, OutlineNode, Page, PageTruncation, ProjectionError, ProjectionLimits, RelationGroup,
    RelationRole, RelationRow, ReverseLinks, SignatureDialect, SourceLocation, Symbol, Target, Text,
    address::walk_address,
    prose::project_prose,
};

/// The language one image proved, when its authority names one.
///
/// A shared-authority image proved no single language, so the caller supplies the ecosystem's
/// language instead of this module inventing one.
#[must_use]
pub fn image_language<Reader: SemanticReader + ?Sized>(reader: &Reader) -> Option<Language> {
    match reader.image_facts().authority {
        SemanticImageAuthority::Shared => None,
        SemanticImageAuthority::Language(profile) => Some(profile.language()),
    }
}

/// Projects one reopened semantic image into owned document values.
///
/// The projector borrows the reader for its whole life and never hands a borrow outward: every
/// method returns owned data, which is what makes the closure-scoped image API in the library
/// possible without leaking a view.
pub struct Projector<'image, Reader: SemanticReader + ?Sized> {
    pub(crate) reader: &'image Reader,
    pub(crate) package: PackageCoordinate,
    pub(crate) dialect: SignatureDialect,
}

impl<'image, Reader: SemanticReader + ?Sized> Projector<'image, Reader> {
    /// Binds a projector to one image and the package that owns it.
    #[must_use]
    pub const fn new(
        reader: &'image Reader,
        package: PackageCoordinate,
        language: Language,
    ) -> Self {
        Self {
            reader,
            package,
            dialect: SignatureDialect::of(language),
        }
    }

    /// The source language every signature on this image is spelled in.
    #[must_use]
    pub const fn language(&self) -> Language {
        self.dialect.language()
    }

    /// The package this image belongs to.
    #[must_use]
    pub const fn package(&self) -> &PackageCoordinate {
        &self.package
    }

    /// One declaration's name exactly as the image spells it.
    #[must_use]
    pub fn name(&self, entity: EntityId) -> Option<Name> {
        let row = self.reader.entity(entity)?;
        self.reader.atom(row.name).map(Name::displayable)
    }

    /// Spells one entity's identity header without projecting its page.
    ///
    /// # Errors
    ///
    /// Returns the exact missing coordinate, missing name atom, or depth overflow.
    pub fn symbol(&self, entity: EntityId) -> Result<Symbol, ProjectionError> {
        let row = self
            .reader
            .entity(entity)
            .ok_or(ProjectionError::MissingEntity { entity })?;
        let name = self
            .reader
            .atom(row.name)
            .map(Name::displayable)
            .ok_or(ProjectionError::MissingPool {
                entity,
                pool: MissingPool::Name,
            })?;
        let walk = walk_address(self.reader, &self.package, entity)?;
        Ok(Symbol {
            address: walk.address,
            entity,
            name,
            kind: row.kind,
            visibility: row.visibility,
        })
    }

    /// Declaration counts over every canonical entity in the image.
    #[must_use]
    pub fn census(&self) -> Census {
        let mut census = Census::default();
        for row in self.reader.canonical_entities() {
            let documented = self
                .reader
                .docs(row.docs)
                .is_some_and(|fragments| fragments.len() != 0);
            census.record(row.kind, row.visibility == Visibility::Public, documented);
        }
        census
    }

    /// Every canonical declaration's coordinate, name bytes, and kind, in image order.
    ///
    /// This is the exact input the lexical projection needs, handed over without allocating a
    /// second copy of the image's name plane.
    pub fn names(&self) -> impl Iterator<Item = (EntityId, &'image [u8], EntityKind)> + '_ {
        self.reader
            .canonical_entities()
            .filter_map(move |row| Some((row.id, self.reader.atom(row.name)?, row.kind)))
    }

    /// The entity one proven key names, when the image holds it.
    #[must_use]
    pub fn entity_by_key(&self, key: ContentKey) -> Option<EntityId> {
        self.reader
            .entity_by_identity(key.identity())
            .map(|row| row.id)
    }

    /// Every declaration whose root-relative path matches.
    ///
    /// A kind qualifier the caller supplied filters; one it omitted does not. Two overload
    /// instances that differ only by variant fingerprint both come back, each with its own exact
    /// key, so a caller reports ambiguity rather than silently picking one.
    #[must_use]
    pub fn resolve_path(&self, path: &SymbolPath) -> Box<[Symbol]> {
        if path.is_root() {
            return Box::new([]);
        }
        let mut matches = Vec::new();
        for row in self.reader.canonical_entities() {
            let Ok(symbol) = self.symbol(row.id) else {
                continue;
            };
            if path_matches(path, symbol.address.as_address().path.segments()) {
                matches.push(symbol);
            }
        }
        matches.sort_by(|left, right| {
            left.address
                .to_string()
                .cmp(&right.address.to_string())
        });
        matches.into_boxed_slice()
    }

    /// Projects the whole containment tree.
    ///
    /// # Errors
    ///
    /// Returns the exact entity whose row or name atom the image lacked.
    pub fn outline(&self) -> Result<Outline, ProjectionError> {
        let mut roots = Vec::new();
        let mut children: Vec<(EntityId, EntityId)> = Vec::new();
        for row in self.reader.canonical_entities() {
            match row.parent {
                Some(parent) if parent != row.id => children.push((parent, row.id)),
                _ => roots.push(row.id),
            }
        }
        children.sort_unstable_by_key(|(parent, child)| (parent.index(), child.index()));
        let mut nodes = Vec::with_capacity(roots.len());
        for root in roots {
            nodes.push(self.outline_node(root, &children, MAX_OUTLINE_DEPTH)?);
        }
        Ok(Outline {
            package: self.package.clone(),
            roots: nodes.into_boxed_slice(),
            census: self.census(),
        })
    }

    fn outline_node(
        &self,
        entity: EntityId,
        children: &[(EntityId, EntityId)],
        depth: u8,
    ) -> Result<OutlineNode, ProjectionError> {
        let symbol = self.symbol(entity)?;
        let summary = self.summary(entity);
        let mut nodes = Vec::new();
        if let Some(next) = depth.checked_sub(1).filter(|remaining| *remaining != 0) {
            let start = children.partition_point(|(parent, _)| parent.index() < entity.index());
            for (parent, child) in children.iter().skip(start) {
                if *parent != entity {
                    break;
                }
                nodes.push(self.outline_node(*child, children, next)?);
            }
        }
        Ok(OutlineNode {
            symbol,
            summary,
            children: nodes.into_boxed_slice(),
        })
    }

    pub(crate) fn summary(&self, entity: EntityId) -> Option<Text> {
        let row = self.reader.entity(entity)?;
        project_prose(self, entity, row.docs, ProjectionLimits::default().prose_bytes)
            .ok()?
            .summary()
    }

    /// Projects one complete declaration page.
    ///
    /// # Errors
    ///
    /// Returns the exact missing coordinate or the prose overrun; member and relation budgets are
    /// reported through [`Page::truncation`] rather than as a failure.
    pub fn page(
        &self,
        entity: EntityId,
        limits: ProjectionLimits,
        reverse: &ReverseLinks,
    ) -> Result<Page, ProjectionError> {
        let row = self
            .reader
            .entity(entity)
            .ok_or(ProjectionError::MissingEntity { entity })?;
        let walk = walk_address(self.reader, &self.package, entity)?;
        let mut crumbs = Vec::with_capacity(walk.ancestors.len());
        for ancestor in &walk.ancestors {
            crumbs.push(self.symbol(*ancestor)?);
        }
        let (signature, signature_truncation) = self.signature(entity, limits.signature_tokens);
        let prose = project_prose(self, entity, row.docs, limits.prose_bytes)?;
        let (members, member_truncation) = self.members(entity, limits.members)?;
        let (relations, relation_truncation) = self.relations(entity, limits.relations, reverse)?;
        Ok(Page {
            symbol: self.symbol(entity)?,
            language: self.language(),
            crumbs: crumbs.into_boxed_slice(),
            signature,
            prose,
            members,
            relations,
            source: self.source(entity),
            attributes: self.attributes(entity),
            truncation: PageTruncation {
                members: member_truncation,
                relations: relation_truncation,
                signature_tokens: signature_truncation,
            },
        })
    }

    fn source(&self, entity: EntityId) -> Option<SourceLocation> {
        let row = self.reader.entity(entity)?;
        let span = row.source?;
        let file = self.reader.atom(span.file()).map(Name::displayable)?;
        Some(SourceLocation {
            file: Text::new(file.as_str()),
            span: crate::ByteSpan::new(span.start(), span.end())?,
        })
    }

    fn attributes(&self, entity: EntityId) -> Box<[Text]> {
        let Some(row) = self.reader.entity(entity) else {
            return Box::new([]);
        };
        let Some(atoms) = self.reader.atom_list(row.attributes) else {
            return Box::new([]);
        };
        atoms
            .filter_map(|atom| self.reader.atom(atom))
            .map(|bytes| Text::new(Name::displayable(bytes).as_str()))
            .collect()
    }

    fn members(
        &self,
        entity: EntityId,
        budget: Count,
    ) -> Result<(Box<[MemberGroup]>, Option<Count>), ProjectionError> {
        let Some(row) = self.reader.entity(entity) else {
            return Err(ProjectionError::MissingEntity { entity });
        };
        let Some(members) = self.reader.entity_list(row.members) else {
            return Ok((Box::new([]), None));
        };
        let admitted = usize::try_from(budget.0).unwrap_or(usize::MAX);
        let mut rows: Vec<(EntityKind, MemberRow)> = Vec::new();
        let mut refused = 0_u32;
        for member in members {
            let Some(member_row) = self.reader.entity(member) else {
                return Err(ProjectionError::MissingEntity { entity: member });
            };
            if member_row.parent != Some(entity) {
                return Err(ProjectionError::ParentageMismatch { entity, member });
            }
            if rows.len() >= admitted {
                refused = refused.saturating_add(1);
                continue;
            }
            let (signature, _) = self.signature(member, Count(MEMBER_SIGNATURE_TOKENS));
            rows.push((
                member_row.kind,
                MemberRow {
                    symbol: self.symbol(member)?,
                    signature,
                    summary: self.summary(member),
                },
            ));
        }
        let mut groups = Vec::new();
        for kind in EntityKind::ALL {
            let of_kind: Vec<MemberRow> = rows
                .iter()
                .filter(|(member_kind, _)| *member_kind == kind)
                .map(|(_, member)| member.clone())
                .collect();
            if !of_kind.is_empty() {
                groups.push(MemberGroup {
                    kind,
                    rows: of_kind.into_boxed_slice(),
                });
            }
        }
        Ok((
            groups.into_boxed_slice(),
            (refused != 0).then_some(Count(refused)),
        ))
    }

    fn relations(
        &self,
        entity: EntityId,
        budget: Count,
        reverse: &ReverseLinks,
    ) -> Result<(Box<[RelationGroup]>, Option<Count>), ProjectionError> {
        let admitted = usize::try_from(budget.0).unwrap_or(usize::MAX);
        let mut rows: Vec<(RelationRole, RelationRow)> = Vec::new();
        let mut refused = 0_u32;
        for (_, link) in self.reader.links_from(entity) {
            if rows.len() >= admitted {
                refused = refused.saturating_add(1);
                continue;
            }
            let Some(target) = self.target(link.target) else {
                continue;
            };
            rows.push((
                RelationRole {
                    kind: link.kind,
                    direction: Direction::Outgoing,
                },
                RelationRow {
                    target,
                    confidence: link.confidence,
                },
            ));
        }
        for incoming in reverse.incoming(entity) {
            if rows.len() >= admitted {
                refused = refused.saturating_add(1);
                continue;
            }
            rows.push((
                RelationRole {
                    kind: incoming.kind,
                    direction: Direction::Incoming,
                },
                RelationRow {
                    target: Target::Local(self.symbol(incoming.from)?),
                    confidence: incoming.confidence,
                },
            ));
        }
        Ok((
            group_relations(&rows),
            (refused != 0).then_some(Count(refused)),
        ))
    }

    pub(crate) fn target(&self, target: LinkTarget) -> Option<Target> {
        match target {
            LinkTarget::Local(entity) => self.symbol(entity).ok().map(Target::Local),
            LinkTarget::External(external) => Some(self.external(external)),
        }
    }

    pub(crate) fn external(&self, external: ExternalId) -> Target {
        let Some(resolved) = self.reader.external(external) else {
            return Target::Unresolved(Text::new("unresolved external"));
        };
        match resolved {
            ExternalTarget::Foreign(foreign) => {
                let display = self.atom_text(foreign.display);
                let path = self.atom_text(foreign.path);
                Target::External(ExternalRef {
                    display,
                    path,
                    origin: Some(self.origin(foreign.origin)),
                    kind: foreign.kind,
                })
            }
            ExternalTarget::FragmentEntity { display, .. } => {
                Target::Unresolved(self.atom_text(display))
            }
            ExternalTarget::Stable { target } => Target::Unresolved(Text::new(format!(
                "#{}",
                ContentKey::new(target.declaration)
            ))),
        }
    }

    fn origin(&self, origin: ForeignTargetOrigin) -> ForeignOrigin {
        match origin {
            ForeignTargetOrigin::Package { ecosystem, package } => ForeignOrigin {
                ecosystem: self.atom_text(ecosystem),
                package: Some(self.atom_text(package)),
            },
            ForeignTargetOrigin::Namespace {
                ecosystem,
                namespace,
            } => ForeignOrigin {
                ecosystem: self.atom_text(ecosystem),
                package: Some(self.atom_text(namespace)),
            },
            ForeignTargetOrigin::Universe { ecosystem }
            | ForeignTargetOrigin::Unspecified { ecosystem } => ForeignOrigin {
                ecosystem: self.atom_text(ecosystem),
                package: None,
            },
        }
    }

    pub(crate) fn atom_text(&self, atom: compiler_ir::AtomId) -> Text {
        self.reader
            .atom(atom)
            .map_or_else(Text::default, |bytes| {
                Text::new(Name::displayable(bytes).as_str())
            })
    }
}

/// Deepest containment nesting one outline renders.
const MAX_OUTLINE_DEPTH: u8 = 64;
/// Signature tokens one member row renders; a row is a glance, not a page.
const MEMBER_SIGNATURE_TOKENS: u32 = 64;

/// Whether a requested path matches a proven one, honouring only the kinds the caller supplied.
fn path_matches(
    requested: &SymbolPath,
    proven: &[interface_identity::PathSegment],
) -> bool {
    let wanted = requested.segments();
    if wanted.len() != proven.len() {
        return false;
    }
    wanted.iter().zip(proven.iter()).all(|(want, have)| {
        want.name == have.name && want.kind.is_none_or(|kind| Some(kind) == have.kind)
    })
}

/// Groups relation rows by role in canonical link order, outgoing before incoming.
fn group_relations(rows: &[(RelationRole, RelationRow)]) -> Box<[RelationGroup]> {
    let mut roles: Vec<RelationRole> = rows.iter().map(|(role, _)| *role).collect();
    roles.sort_unstable_by_key(|role| (link_order(role.kind), direction_order(role.direction)));
    roles.dedup();
    roles
        .into_iter()
        .map(|role| RelationGroup {
            role,
            rows: rows
                .iter()
                .filter(|(row_role, _)| *row_role == role)
                .map(|(_, row)| row.clone())
                .collect(),
        })
        .collect()
}

const fn link_order(kind: LinkKind) -> u8 {
    match kind {
        LinkKind::Calls => 0,
        LinkKind::MethodCall => 1,
        LinkKind::TypeReference => 2,
        LinkKind::Reads => 3,
        LinkKind::Writes => 4,
        LinkKind::Imports => 5,
        LinkKind::Implements => 6,
        LinkKind::Overrides => 7,
        LinkKind::Reexports => 8,
        LinkKind::Inherits => 9,
        LinkKind::Documents => 10,
    }
}

const fn direction_order(direction: Direction) -> u8 {
    match direction {
        Direction::Outgoing => 0,
        Direction::Incoming => 1,
    }
}
