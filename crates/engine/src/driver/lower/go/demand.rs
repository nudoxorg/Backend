//! Checked storage demand from one borrowed, source-bound package image.

use super::*;
use crate::driver::lower::{ResourcePlan, TypeChildLane};
use crate::driver::types::WorkPermit;
use std::collections::HashSet;

/// Counts each projected callable traversal, not unique signature roots.
/// The primary source extent is deliberately absent from allocation geometry.
pub(super) struct ImageDemand<'image, 'cancel> {
    image: GoImage<'image>,
    plan: ResourcePlan,
    names: HashSet<(&'image [u8], &'image [u8])>,
    declarations: HashSet<&'image [u8]>,
    atoms: HashSet<&'image [u8]>,
    type_lists: usize,
    entity_lists: usize,
    visited: HashSet<u32>,
    permit: Option<WorkPermit<'cancel>>,
}

impl<'image, 'cancel> ImageDemand<'image, 'cancel> {
    pub(super) fn measure(
        image: GoImage<'image>,
        permit: Option<WorkPermit<'cancel>>,
    ) -> Result<Self, DemandError> {
        let mut demand = Self {
            image,
            plan: ResourcePlan {
                facts: 0,
                name_spans: 0,
                product_children: 0,
                declared_type_children: 0,
                occurrences: 0,
                docs: 0,
                extension_atoms: 0,
                type_parameters: 0,
                type_parameter_bounds: 0,
                free_predicates: 0,
                ref_lists: 0,
                anonymous_rows: 0,
                anonymous_type_children: 0,
                computed_rows: 0,
                computed_type_children: 0,
            },
            names: HashSet::new(),
            declarations: HashSet::new(),
            atoms: HashSet::new(),
            type_lists: 1,
            entity_lists: 1,
            visited: HashSet::new(),
            permit,
        };
        demand.checkpoint()?;
        if image.declaration_count() > ResourcePlan::protocol_maximum().facts {
            return Err(lane_terminal(
                ResourcePlan::protocol_maximum().facts,
                0,
                FactFault::Capacity,
            )
            .into());
        }
        // Mirror the collector's declaration-first name availability. An
        // alias may only reference earlier aliases, while all named types
        // already own their backward nominal anchors.
        for row in image.declarations() {
            demand.checkpoint()?;
            let row = row.map_err(GoCollectError::Image)?;
            if !demand.declarations.contains(row.name) {
                demand
                    .declarations
                    .try_reserve(1)
                    .map_err(|_| lane_terminal(demand.plan.facts, 0, FactFault::Capacity))?;
                demand.declarations.insert(row.name);
            }
            if row.kind == DeclarationKind::Type {
                demand.fact()?;
                demand.name(row.package, row.name)?;
            }
        }
        for row in image.declarations() {
            demand.checkpoint()?;
            let row = row.map_err(GoCollectError::Image)?;
            if row.kind == DeclarationKind::Alias {
                demand.root(row.type_root)?;
                demand.fact()?;
                demand.name(row.package, row.name)?;
            }
        }
        for (index, row) in image.declarations().enumerate() {
            demand.checkpoint()?;
            let row = row.map_err(GoCollectError::Image)?;
            match row.kind {
                DeclarationKind::Type => demand.family(index, row.type_root)?,
                DeclarationKind::Function => {
                    demand.parameters(index, row.package)?;
                    demand.executable(row.type_root)?;
                    demand.name(row.package, row.name)?;
                }
                DeclarationKind::Constant | DeclarationKind::Static => {
                    demand.root(row.type_root)?;
                    demand.fact()?;
                    if row.kind == DeclarationKind::Constant && !row.value.is_empty() {
                        demand.atom(row.value)?;
                    }
                    if !is_unbound_name(row.name) {
                        demand.name(row.package, row.name)?;
                    }
                }
                DeclarationKind::Alias => {}
            }
        }
        for index in 0..image.unresolved_cgo_count() {
            demand.checkpoint()?;
            let name = image.unresolved_cgo(index).map_err(GoCollectError::Image)?;
            let bare = name.rsplit(|byte| *byte == b'.').next().unwrap_or(name);
            if !demand.declarations.contains(bare) {
                demand.fact()?;
            }
        }
        for index in 0..image.constraint_count() {
            demand.checkpoint()?;
            let row = image.constraint(index).map_err(GoCollectError::Image)?;
            demand.atom(row.constraint)?;
            for _ in 0..row.exported_count {
                demand.fact()?;
            }
        }
        grow(
            &mut demand.plan.occurrences,
            add(image.reference_count(), image.satisfaction_count())?,
            ResourcePlan::protocol_maximum().occurrences,
            FactFault::OccurrenceCapacity,
        )?;
        for index in 0..image.doc_count() {
            demand.checkpoint()?;
            let row = image.doc(index).map_err(GoCollectError::Image)?;
            if row.owner_kind == DocOwner::Package {
                continue;
            }
            let mut text = false;
            for chunk in row.text.chunks(4096) {
                demand.checkpoint()?;
                for byte in chunk {
                    if *byte == b'\n' {
                        if text {
                            grow(
                                &mut demand.plan.docs,
                                1,
                                ResourcePlan::protocol_maximum().docs,
                                FactFault::DocCapacity,
                            )?;
                        }
                        grow(
                            &mut demand.plan.docs,
                            1,
                            ResourcePlan::protocol_maximum().docs,
                            FactFault::DocCapacity,
                        )?;
                        text = false;
                    } else {
                        text = true;
                    }
                }
            }
            if text {
                grow(
                    &mut demand.plan.docs,
                    1,
                    ResourcePlan::protocol_maximum().docs,
                    FactFault::DocCapacity,
                )?;
            }
        }
        demand.plan.ref_lists = demand
            .type_lists
            .max(demand.entity_lists)
            .max(demand.atoms.len() + 1);
        demand.checkpoint()?;
        Ok(demand)
    }

    pub(super) fn resource_plan(self) -> Result<ResourcePlan, DemandError> {
        self.checkpoint()?;
        Ok(self.plan)
    }

    fn checkpoint(&self) -> Result<(), DemandError> {
        if let Some(permit) = self.permit {
            permit.checkpoint().map_err(DemandError::Stopped)?;
        }
        Ok(())
    }

    fn name(&mut self, package: &'image [u8], name: &'image [u8]) -> Result<(), DemandError> {
        self.checkpoint()?;
        if !self.names.contains(&(package, name)) {
            self.names
                .try_reserve(1)
                .map_err(|_| lane_terminal(self.plan.facts, 0, FactFault::Capacity))?;
            self.names.insert((package, name));
        }
        Ok(())
    }

    fn fact(&mut self) -> Result<u32, DemandError> {
        let ordinal = go_u32(self.plan.facts, GoProjectionIndexPhase::FactOrdinal).map_err(
            |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
        )?;
        self.checkpoint()?;
        grow(
            &mut self.plan.facts,
            1,
            ResourcePlan::protocol_maximum().facts,
            FactFault::Capacity,
        )?;
        Ok(ordinal)
    }

    fn atom(&mut self, bytes: &'image [u8]) -> Result<(), DemandError> {
        self.checkpoint()?;
        if !self.atoms.contains(bytes) {
            grow(
                &mut self.plan.extension_atoms,
                1,
                ResourcePlan::protocol_maximum().extension_atoms,
                FactFault::ExtensionAtomCapacity,
            )?;
            if self.atoms.len() + 2 > ResourcePlan::protocol_maximum().ref_lists {
                return Err(lane_terminal(self.plan.facts, 0, FactFault::RefListCapacity).into());
            }
            self.atoms
                .try_reserve(1)
                .map_err(|_| lane_terminal(self.plan.facts, 0, FactFault::ExtensionAtomCapacity))?;
            self.atoms.insert(bytes);
        }
        Ok(())
    }

    fn parameters(&mut self, owner: usize, package: &[u8]) -> Result<(), DemandError> {
        for index in owner_run(
            self.image.type_parameter_count(),
            owner as u32,
            |position| self.image.type_parameter(position).map(|row| row.owner),
        )? {
            self.checkpoint()?;
            let row = self
                .image
                .type_parameter(index)
                .map_err(GoCollectError::Image)?;
            if row.owner as usize != owner {
                continue;
            }
            grow(
                &mut self.plan.type_parameters,
                1,
                ResourcePlan::protocol_maximum().type_parameters,
                FactFault::TypeParameterCapacity,
            )?;
            if let Some(root) = row.constraint {
                let root = self
                    .image
                    .type_row(index_of(root))
                    .map_err(GoCollectError::Image)?;
                if matches!(root.kind, TypeRowKind::Named | TypeRowKind::Alias)
                    && root.children.1 == 0
                    && root.package == package
                    && self.names.contains(&(root.package, root.name))
                {
                    grow(
                        &mut self.plan.type_parameter_bounds,
                        1,
                        ResourcePlan::protocol_maximum().type_parameter_bounds,
                        FactFault::TypeParameterBoundCapacity {
                            requested: 1,
                            available: 0,
                        },
                    )?;
                }
            }
        }
        Ok(())
    }

    fn family(&mut self, owner: usize, root: Option<u32>) -> Result<(), DemandError> {
        let declaration = self
            .image
            .declaration(owner)
            .map_err(GoCollectError::Image)?;
        self.parameters(owner, declaration.package)?;
        let mut fields = 0usize;
        let mut methods = 0usize;
        let mut names = HashSet::new();
        if let Some(root) = root {
            let row = self
                .image
                .type_row(index_of(root))
                .map_err(GoCollectError::Image)?;
            if matches!(row.kind, TypeRowKind::Struct | TypeRowKind::Interface) {
                for index in member_run(&row) {
                    self.checkpoint()?;
                    let member = self.image.member(index).map_err(GoCollectError::Image)?;
                    if row.kind == TypeRowKind::Struct && member.kind == MemberKind::Field {
                        self.root(member.type_root)?;
                        self.fact()?;
                        fields += 1;
                        self.width(fields)?;
                    } else if row.kind == TypeRowKind::Interface
                        && member.kind == MemberKind::Method
                    {
                        self.executable(member.type_root)?;
                        methods += 1;
                        self.width(methods)?;
                        names.try_reserve(1).map_err(|_| {
                            lane_terminal(self.plan.facts, 0, FactFault::RefListCapacity)
                        })?;
                        names.insert(member.name);
                    }
                }
            }
        }
        for index in owner_run(self.image.method_count(), owner as u32, |position| {
            self.image.method(position).map(|row| row.owner)
        })? {
            self.checkpoint()?;
            let method = self.image.method(index).map_err(GoCollectError::Image)?;
            if method.owner as usize != owner {
                continue;
            }
            for _ in blank_separated(method.receiver_type_params) {
                self.checkpoint()?;
                grow(
                    &mut self.plan.type_parameters,
                    1,
                    ResourcePlan::protocol_maximum().type_parameters,
                    FactFault::TypeParameterCapacity,
                )?;
            }
            self.executable(method.type_root)?;
            methods += 1;
            self.width(methods)?;
            names
                .try_reserve(1)
                .map_err(|_| lane_terminal(self.plan.facts, 0, FactFault::RefListCapacity))?;
            names.insert(method.name);
        }
        let run = match root {
            Some(owner) => owner_run(self.image.method_set_count(), owner, |position| {
                self.image.method_set(position).map(|row| row.owner)
            })?,
            None => 0..0,
        };
        for index in run {
            self.checkpoint()?;
            let method = self
                .image
                .method_set(index)
                .map_err(GoCollectError::Image)?;
            if root != Some(method.owner) || names.contains(method.name) {
                continue;
            }
            self.executable(method.type_root)?;
            methods += 1;
            self.width(methods)?;
            names
                .try_reserve(1)
                .map_err(|_| lane_terminal(self.plan.facts, 0, FactFault::RefListCapacity))?;
            names.insert(method.name);
        }
        self.list(false, fields)?;
        self.list(false, methods)?;
        Ok(())
    }

    fn executable(&mut self, signature: Option<u32>) -> Result<u32, DemandError> {
        let mut params = 0usize;
        let mut results = 0usize;
        if let Some(signature) = signature {
            let row = self
                .image
                .type_row(index_of(signature))
                .map_err(GoCollectError::Image)?;
            if row.children.1 as usize > crate::driver::lower::MAX_FACT_CHILDREN {
                return Err(lane_terminal(self.plan.facts, 0, FactFault::ChildCapacity).into());
            }
            for position in 0..row.children.1 as usize {
                let child = self
                    .image
                    .type_child(index_of(row.children.0) + position)
                    .map_err(GoCollectError::Image)?
                    .0;
                self.root(Some(child))?;
                self.fact()?;
                if position < row.param_count as usize {
                    params += 1;
                } else {
                    results += 1;
                }
                grow(
                    &mut self.plan.product_children,
                    1,
                    ResourcePlan::protocol_maximum().product_children,
                    FactFault::ProductChildPoolCapacity {
                        used: 0,
                        requested: 1,
                        capacity: ResourcePlan::protocol_maximum().product_children,
                    },
                )?;
                grow(
                    &mut self.plan.declared_type_children,
                    1,
                    ResourcePlan::protocol_maximum().declared_type_children,
                    FactFault::TypeChildPoolCapacity {
                        lane: TypeChildLane::Declared,
                        used: 0,
                        requested: 1,
                        capacity: ResourcePlan::protocol_maximum().declared_type_children,
                    },
                )?;
            }
        }
        self.list(true, params)?;
        self.list(true, results)?;
        self.fact()
    }

    fn root(&mut self, root: Option<u32>) -> Result<(), DemandError> {
        let Some(root) = root else {
            return Ok(());
        };
        self.checkpoint()?;
        self.visited.clear();
        let row = self
            .image
            .type_row(index_of(root))
            .map_err(GoCollectError::Image)?;
        if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias) {
            if row.package.is_empty() || !self.names.contains(&(row.package, row.name)) {
                return Ok(());
            }
            if row.children.1 != 0 {
                grow(
                    &mut self.plan.declared_type_children,
                    1,
                    ResourcePlan::protocol_maximum().declared_type_children,
                    FactFault::TypeChildPoolCapacity {
                        lane: TypeChildLane::Declared,
                        used: 0,
                        requested: 1,
                        capacity: ResourcePlan::protocol_maximum().declared_type_children,
                    },
                )?;
            }
        }
        let children = self.children(&row, true)?;
        grow(
            &mut self.plan.declared_type_children,
            children.len(),
            ResourcePlan::protocol_maximum().declared_type_children,
            FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Declared,
                used: 0,
                requested: children.len(),
                capacity: ResourcePlan::protocol_maximum().declared_type_children,
            },
        )?;
        for child in children {
            if let Some(child) = child {
                let row = self
                    .image
                    .type_row(index_of(child))
                    .map_err(GoCollectError::Image)?;
                if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
                    && !row.package.is_empty()
                    && row.children.1 == 0
                    && self.names.contains(&(row.package, row.name))
                {
                    continue;
                }
            }
            self.anonymous(child, DEPTH_LIMIT - 1)?;
        }
        Ok(())
    }

    fn anonymous(&mut self, root: Option<u32>, depth: usize) -> Result<(), DemandError> {
        let Some(root) = root else {
            grow(
                &mut self.plan.anonymous_rows,
                1,
                ResourcePlan::protocol_maximum().anonymous_rows,
                FactFault::TypeRowCapacity,
            )?;
            return Ok(());
        };
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth { type_row: root }).into());
        }
        let slot = index_of(root);
        self.checkpoint()?;
        if self.visited.contains(&root) {
            return Ok(());
        }
        let row = self.image.type_row(slot).map_err(GoCollectError::Image)?;
        let children = if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias) {
            Vec::new()
        } else {
            self.children(&row, false)?
        };
        grow(
            &mut self.plan.anonymous_rows,
            1,
            ResourcePlan::protocol_maximum().anonymous_rows,
            FactFault::TypeRowCapacity,
        )?;
        grow(
            &mut self.plan.anonymous_type_children,
            children.len(),
            ResourcePlan::protocol_maximum().anonymous_type_children,
            FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Anonymous,
                used: 0,
                requested: children.len(),
                capacity: ResourcePlan::protocol_maximum().anonymous_type_children,
            },
        )?;
        for child in children {
            self.anonymous(child, depth - 1)?;
        }
        // Match the projector's completed-row memo: a back edge must still
        // reach the existing typed depth rejection rather than becoming a
        // fabricated previously projected coordinate.
        self.visited
            .try_reserve(1)
            .map_err(|_| lane_terminal(self.plan.facts, 0, FactFault::TypeRowCapacity))?;
        self.visited.insert(root);
        Ok(())
    }

    // Every nonempty callable/owner list contains newly emitted, disjoint
    // fact ordinals. Empty lists alone share coordinate zero; no list copy
    // or interning table is needed to prove their exact demand.
    fn width(&self, width: usize) -> Result<(), DemandError> {
        self.checkpoint()?;
        if width > MAX_REF_LIST_ELEMENTS {
            return Err(lane_terminal(self.plan.facts, 0, FactFault::RefListElements).into());
        }
        Ok(())
    }

    fn list(&mut self, types: bool, width: usize) -> Result<(), DemandError> {
        self.width(width)?;
        if width != 0 {
            let lists = if types {
                &mut self.type_lists
            } else {
                &mut self.entity_lists
            };
            grow(
                lists,
                1,
                ResourcePlan::protocol_maximum().ref_lists,
                FactFault::RefListCapacity,
            )?;
        }
        Ok(())
    }

    fn child_slot(&self, children: &mut Vec<Option<u32>>) -> Result<(), DemandError> {
        self.checkpoint()?;
        if children.len() == crate::driver::lower::MAX_TYPE_CHILDREN {
            return Err(lane_terminal(self.plan.facts, 0, FactFault::TypeChildCapacity).into());
        }
        children
            .try_reserve(1)
            .map_err(|_| lane_terminal(self.plan.facts, 0, FactFault::TypeChildCapacity))?;
        Ok(())
    }

    fn children(
        &self,
        row: &backend_frontend_go::legacy::TypeRow<'image>,
        root: bool,
    ) -> Result<Vec<Option<u32>>, DemandError> {
        let mut children = Vec::new();
        if !matches!(
            row.kind,
            TypeRowKind::Basic
                | TypeRowKind::TypeParam
                | TypeRowKind::Invalid
                | TypeRowKind::Struct
        ) {
            for offset in
                index_of(row.children.0)..index_of(row.children.0) + row.children.1 as usize
            {
                self.child_slot(&mut children)?;
                children.push(Some(
                    self.image
                        .type_child(offset)
                        .map_err(GoCollectError::Image)?
                        .0,
                ));
            }
        }
        if matches!(row.kind, TypeRowKind::Struct | TypeRowKind::Interface) {
            for index in member_run(row) {
                self.checkpoint()?;
                let member = self.image.member(index).map_err(GoCollectError::Image)?;
                if row.kind == TypeRowKind::Struct || member.kind == MemberKind::Method {
                    self.child_slot(&mut children)?;
                    children.push(member.type_root);
                }
            }
        }
        // Named rows reached inside anonymous graphs remain unknown leaves.
        if !root && matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias) {
            children.clear();
        }
        Ok(children)
    }
}

fn add(left: usize, right: usize) -> Result<usize, GoCollectError> {
    left.checked_add(right).ok_or_else(|| {
        terminal(ProjectionFault::IndexCapacity {
            phase: GoProjectionIndexPhase::ImageRow,
            observed: u64::MAX,
        })
    })
}

// Refuse before mutating demand or allocating its indexing state. These
// maxima are the original wire limits, not primary-source estimates.
fn grow(
    used: &mut usize,
    requested: usize,
    maximum: usize,
    fault: FactFault,
) -> Result<(), DemandError> {
    let next = add(*used, requested)?;
    if next > maximum {
        let fault = match fault {
            FactFault::ProductChildPoolCapacity { .. } => FactFault::ProductChildPoolCapacity {
                used: *used,
                requested,
                capacity: maximum,
            },
            FactFault::TypeChildPoolCapacity { lane, .. } => FactFault::TypeChildPoolCapacity {
                lane,
                used: *used,
                requested,
                capacity: maximum,
            },
            FactFault::TypeParameterBoundCapacity { .. } => FactFault::TypeParameterBoundCapacity {
                requested,
                available: maximum - *used,
            },
            other => other,
        };
        return Err(lane_terminal(*used, 0, fault).into());
    }
    *used = next;
    Ok(())
}

#[cfg(test)]
pub(super) fn assert_control_boundaries(image: &[u8]) {
    use crate::driver::types::{CompileControl, WorkStopped};
    use backend_semantic::ir::SourceIdentity;
    use backend_version::{ContentId, SourceFactDomain};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let cancelled = AtomicBool::new(false);
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"demand-control"),
        byte_len: 14,
    };
    let permit = WorkPermit::new(
        source,
        CompileControl {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: &cancelled,
        },
    );
    let mut measured = ImageDemand::measure(
        GoImage::open(image).expect("validated control"),
        Some(permit),
    )
    .expect("completed initial traversals");
    let signature = (0..measured.image.type_count())
        .find(|index| {
            measured
                .image
                .type_row(*index)
                .is_ok_and(|row| row.kind == TypeRowKind::Func)
        })
        .expect("real shared signature control") as u32;
    let prefix = measured.plan;
    cancelled.store(true, Ordering::Release);
    assert!(matches!(
        measured.executable(Some(signature)),
        Err(DemandError::Stopped(WorkStopped::Cancelled))
    ));
    assert_eq!(
        measured.plan, prefix,
        "cancellation between traversals must preserve demand prefix"
    );
    cancelled.store(false, Ordering::Release);
    measured.permit = Some(WorkPermit::new(
        source,
        CompileControl {
            deadline: Instant::now() - Duration::from_secs(1),
            cancelled: &cancelled,
        },
    ));
    assert!(matches!(
        measured.root(Some(signature)),
        Err(DemandError::Stopped(WorkStopped::Deadline))
    ));
    assert_eq!(measured.plan, prefix);
    measured.permit = Some(permit);
    measured.plan.facts = ResourcePlan::protocol_maximum().facts;
    let capacities = (
        measured.names.capacity(),
        measured.declarations.capacity(),
        measured.visited.capacity(),
    );
    assert!(matches!(measured.fact(), Err(DemandError::Projection(_))));
    assert_eq!(measured.plan.facts, ResourcePlan::protocol_maximum().facts);
    assert_eq!(
        capacities,
        (
            measured.names.capacity(),
            measured.declarations.capacity(),
            measured.visited.capacity()
        ),
        "cumulative fact rejection must precede indexing allocation"
    );
    measured.plan.anonymous_rows = ResourcePlan::protocol_maximum().anonymous_rows;
    assert!(matches!(
        measured.anonymous(None, DEPTH_LIMIT),
        Err(DemandError::Projection(_))
    ));
    assert_eq!(
        measured.plan.anonymous_rows,
        ResourcePlan::protocol_maximum().anonymous_rows
    );
    measured.type_lists = ResourcePlan::protocol_maximum().ref_lists;
    assert!(matches!(
        measured.list(true, 1),
        Err(DemandError::Projection(_))
    ));
    assert_eq!(
        measured.type_lists,
        ResourcePlan::protocol_maximum().ref_lists
    );
}
