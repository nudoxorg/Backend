//! Checked storage demand from one borrowed, source-bound package image.

use super::*;
use crate::driver::lower::{ResourcePlan, TypeChildLane};
use std::collections::{HashMap, HashSet};

/// Counts each projected callable traversal, not unique signature roots.
/// The primary source extent is deliberately absent from allocation geometry.
pub(super) struct ImageDemand<'image> {
    image: GoImage<'image>,
    plan: ResourcePlan,
    names: HashSet<(&'image [u8], &'image [u8])>,
    atoms: HashMap<&'image [u8], u32>,
    atom_lists: HashSet<Vec<u32>>,
    type_lists: HashSet<Vec<u32>>,
    entity_lists: HashSet<Vec<u32>>,
    visited: Vec<usize>,
    epoch: usize,
}

impl<'image> ImageDemand<'image> {
    pub(super) fn measure(image: GoImage<'image>) -> Result<Self, GoCollectError> {
        let mut demand = Self {
            image,
            plan: ResourcePlan {
                facts: 0,
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
            atoms: HashMap::new(),
            atom_lists: HashSet::from([Vec::new()]),
            type_lists: HashSet::from([Vec::new()]),
            entity_lists: HashSet::from([Vec::new()]),
            visited: Vec::new(),
            epoch: 0,
        };
        demand
            .visited
            .try_reserve_exact(image.type_count())
            .map_err(|_| lane_terminal(0, 0, FactFault::TypeRowCapacity))?;
        demand.visited.resize(image.type_count(), 0);
        // Mirror the collector's declaration-first name availability. An
        // alias may only reference earlier aliases, while all named types
        // already own their backward nominal anchors.
        for row in image.declarations() {
            let row = row.map_err(GoCollectError::Image)?;
            if row.kind == DeclarationKind::Type {
                demand.fact()?;
                demand.names.insert((row.package, row.name));
            }
        }
        for row in image.declarations() {
            let row = row.map_err(GoCollectError::Image)?;
            if row.kind == DeclarationKind::Alias {
                demand.root(row.type_root)?;
                demand.fact()?;
                demand.names.insert((row.package, row.name));
            }
        }
        for (index, row) in image.declarations().enumerate() {
            let row = row.map_err(GoCollectError::Image)?;
            match row.kind {
                DeclarationKind::Type => demand.family(index, row.type_root)?,
                DeclarationKind::Function => {
                    demand.parameters(index, row.package)?;
                    demand.executable(row.type_root)?;
                    demand.names.insert((row.package, row.name));
                }
                DeclarationKind::Constant | DeclarationKind::Static => {
                    demand.root(row.type_root)?;
                    demand.fact()?;
                    if row.kind == DeclarationKind::Constant && !row.value.is_empty() {
                        demand.atom(row.value)?;
                    }
                    if !is_unbound_name(row.name) {
                        demand.names.insert((row.package, row.name));
                    }
                }
                DeclarationKind::Alias => {}
            }
        }
        for index in 0..image.unresolved_cgo_count() {
            let name = image.unresolved_cgo(index).map_err(GoCollectError::Image)?;
            let bare = name.rsplit(|byte| *byte == b'.').next().unwrap_or(name);
            if !image
                .declarations()
                .any(|row| row.is_ok_and(|row| row.name == bare))
            {
                demand.fact()?;
            }
        }
        for index in 0..image.constraint_count() {
            let row = image.constraint(index).map_err(GoCollectError::Image)?;
            demand.atom(row.constraint)?;
            for _ in 0..row.exported_count {
                demand.fact()?;
            }
        }
        demand.plan.occurrences = add(image.reference_count(), image.satisfaction_count())?;
        for index in 0..image.doc_count() {
            let row = image.doc(index).map_err(GoCollectError::Image)?;
            if row.owner_kind == DocOwner::Package {
                continue;
            }
            let text = row
                .text
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count();
            let breaks = row.text.iter().filter(|byte| **byte == b'\n').count();
            demand.plan.docs = add(demand.plan.docs, add(text, breaks)?)?;
        }
        demand.plan.extension_atoms = demand.atoms.len();
        demand.plan.ref_lists = demand
            .atom_lists
            .len()
            .max(demand.type_lists.len())
            .max(demand.entity_lists.len());
        Ok(demand)
    }

    pub(super) fn resource_plan(self) -> Result<ResourcePlan, GoCollectError> {
        let maximum = ResourcePlan::protocol_maximum();
        let p = self.plan;
        let fault = if p.facts > maximum.facts {
            Some(FactFault::Capacity)
        } else if p.product_children > maximum.product_children {
            Some(FactFault::ProductChildPoolCapacity {
                used: 0,
                requested: p.product_children,
                capacity: maximum.product_children,
            })
        } else if p.declared_type_children > maximum.declared_type_children {
            Some(FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Declared,
                used: 0,
                requested: p.declared_type_children,
                capacity: maximum.declared_type_children,
            })
        } else if p.anonymous_rows > maximum.anonymous_rows {
            Some(FactFault::TypeRowCapacity)
        } else if p.anonymous_type_children > maximum.anonymous_type_children {
            Some(FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Anonymous,
                used: 0,
                requested: p.anonymous_type_children,
                capacity: maximum.anonymous_type_children,
            })
        } else if p.occurrences > maximum.occurrences {
            Some(FactFault::OccurrenceCapacity)
        } else if p.docs > maximum.docs {
            Some(FactFault::DocCapacity)
        } else if p.extension_atoms > maximum.extension_atoms {
            Some(FactFault::ExtensionAtomCapacity)
        } else if p.type_parameters > maximum.type_parameters {
            Some(FactFault::TypeParameterCapacity)
        } else if p.type_parameter_bounds > maximum.type_parameter_bounds {
            Some(FactFault::TypeParameterBoundCapacity {
                requested: p.type_parameter_bounds,
                available: maximum.type_parameter_bounds,
            })
        } else if p.ref_lists > maximum.ref_lists {
            Some(FactFault::RefListCapacity)
        } else {
            None
        };
        if let Some(fault) = fault {
            return Err(lane_terminal(p.facts, 0, fault));
        }
        Ok(p)
    }

    fn fact(&mut self) -> Result<u32, GoCollectError> {
        let ordinal = go_u32(self.plan.facts, GoProjectionIndexPhase::FactOrdinal).map_err(
            |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
        )?;
        self.plan.facts = add(self.plan.facts, 1)?;
        Ok(ordinal)
    }

    fn atom(&mut self, bytes: &'image [u8]) -> Result<(), GoCollectError> {
        let next = go_u32(self.atoms.len(), GoProjectionIndexPhase::Atom).map_err(
            |(phase, observed)| terminal(ProjectionFault::IndexCapacity { phase, observed }),
        )?;
        let id = *self.atoms.entry(bytes).or_insert(next);
        self.atom_lists.insert(vec![id]);
        Ok(())
    }

    fn parameters(&mut self, owner: usize, package: &[u8]) -> Result<(), GoCollectError> {
        for index in 0..self.image.type_parameter_count() {
            let row = self
                .image
                .type_parameter(index)
                .map_err(GoCollectError::Image)?;
            if row.owner as usize != owner {
                continue;
            }
            self.plan.type_parameters = add(self.plan.type_parameters, 1)?;
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
                    self.plan.type_parameter_bounds = add(self.plan.type_parameter_bounds, 1)?;
                }
            }
        }
        Ok(())
    }

    fn family(&mut self, owner: usize, root: Option<u32>) -> Result<(), GoCollectError> {
        let declaration = self
            .image
            .declaration(owner)
            .map_err(GoCollectError::Image)?;
        self.parameters(owner, declaration.package)?;
        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut interface_methods = Vec::new();
        let mut names = HashSet::new();
        if let Some(root) = root {
            let row = self
                .image
                .type_row(index_of(root))
                .map_err(GoCollectError::Image)?;
            if matches!(row.kind, TypeRowKind::Struct | TypeRowKind::Interface) {
                for index in member_run(&row) {
                    let member = self.image.member(index).map_err(GoCollectError::Image)?;
                    if row.kind == TypeRowKind::Struct && member.kind == MemberKind::Field {
                        self.root(member.type_root)?;
                        fields.push(self.fact()?);
                    } else if row.kind == TypeRowKind::Interface
                        && member.kind == MemberKind::Method
                    {
                        interface_methods.push((self.executable(member.type_root)?, member.name));
                    }
                }
            }
        }
        for index in 0..self.image.method_count() {
            let method = self.image.method(index).map_err(GoCollectError::Image)?;
            if method.owner as usize != owner {
                continue;
            }
            self.plan.type_parameters = add(
                self.plan.type_parameters,
                blank_separated(method.receiver_type_params).len(),
            )?;
            methods.push(self.executable(method.type_root)?);
            names.insert(method.name);
        }
        for (ordinal, name) in interface_methods {
            methods.push(ordinal);
            names.insert(name);
        }
        for index in 0..self.image.method_set_count() {
            let method = self
                .image
                .method_set(index)
                .map_err(GoCollectError::Image)?;
            if root != Some(method.owner) || names.contains(method.name) {
                continue;
            }
            methods.push(self.executable(method.type_root)?);
            names.insert(method.name);
        }
        self.entity_lists.insert(fields);
        self.entity_lists.insert(methods);
        Ok(())
    }

    fn executable(&mut self, signature: Option<u32>) -> Result<u32, GoCollectError> {
        let mut params = Vec::new();
        let mut results = Vec::new();
        if let Some(signature) = signature {
            let row = self
                .image
                .type_row(index_of(signature))
                .map_err(GoCollectError::Image)?;
            for position in 0..row.children.1 as usize {
                let child = self
                    .image
                    .type_child(index_of(row.children.0) + position)
                    .map_err(GoCollectError::Image)?
                    .0;
                self.root(Some(child))?;
                let carrier = self.fact()?;
                if position < row.param_count as usize {
                    params.push(carrier);
                } else {
                    results.push(carrier);
                }
                self.plan.product_children = add(self.plan.product_children, 1)?;
                self.plan.declared_type_children = add(self.plan.declared_type_children, 1)?;
            }
        }
        self.type_lists.insert(params);
        self.type_lists.insert(results);
        self.fact()
    }

    fn root(&mut self, root: Option<u32>) -> Result<(), GoCollectError> {
        let Some(root) = root else {
            return Ok(());
        };
        self.epoch = add(self.epoch, 1)?;
        let row = self
            .image
            .type_row(index_of(root))
            .map_err(GoCollectError::Image)?;
        if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias) {
            if row.package.is_empty() || !self.names.contains(&(row.package, row.name)) {
                return Ok(());
            }
            if row.children.1 != 0 {
                self.plan.declared_type_children = add(self.plan.declared_type_children, 1)?;
            }
        }
        let children = self.children(&row, true)?;
        self.plan.declared_type_children = add(self.plan.declared_type_children, children.len())?;
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

    fn anonymous(&mut self, root: Option<u32>, depth: usize) -> Result<(), GoCollectError> {
        let Some(root) = root else {
            self.plan.anonymous_rows = add(self.plan.anonymous_rows, 1)?;
            return Ok(());
        };
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth { type_row: root }));
        }
        let slot = index_of(root);
        if self.visited[slot] == self.epoch {
            return Ok(());
        }
        let row = self.image.type_row(slot).map_err(GoCollectError::Image)?;
        let children = if matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias) {
            Vec::new()
        } else {
            self.children(&row, false)?
        };
        self.plan.anonymous_rows = add(self.plan.anonymous_rows, 1)?;
        self.plan.anonymous_type_children = add(self.plan.anonymous_type_children, children.len())?;
        for child in children {
            self.anonymous(child, depth - 1)?;
        }
        // Match the projector's completed-row memo: a back edge must still
        // reach the existing typed depth rejection rather than becoming a
        // fabricated previously projected coordinate.
        self.visited[slot] = self.epoch;
        Ok(())
    }

    fn children(
        &self,
        row: &backend_frontend_go::legacy::TypeRow<'image>,
        root: bool,
    ) -> Result<Vec<Option<u32>>, GoCollectError> {
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
                let member = self.image.member(index).map_err(GoCollectError::Image)?;
                if row.kind == TypeRowKind::Struct || member.kind == MemberKind::Method {
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
