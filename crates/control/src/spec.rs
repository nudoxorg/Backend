//! Immutable work specifications and exact work-key derivation.

use core::fmt;

use crate::ControlError;
use crate::ids::{
    AgentWorkKey, CellId, CellSchema, DependencyRoot, DependencySchema, Identity, InputRoot,
    InputSchema, ModelId, ModelSchema, RoleId, RoleSchema, ToolchainRoot, ToolchainSchema,
    WorkKeySchema, append_field, append_field_unchecked, append_identity,
    append_identity_unchecked, append_version, derive,
};

/// Maximum number of prerequisite work cells in one specification.
pub const MAX_DEPENDENCIES: usize = 1024;

/// Maximum number of bytes in one canonical work specification.
pub const MAX_WORK_SPEC_BYTES: usize = 128 * 1024;

/// Ordered effort classes understood by the scheduler and role contract.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Effort {
    /// Fast, low-cost planning or inspection.
    Low,
    /// Normal implementation effort.
    Medium,
    /// Extended implementation or repair effort.
    High,
    /// Extra context and verification budget.
    XHigh,
    /// Maximum standard role budget.
    Max,
    /// Explicit frontier/experimental budget.
    Ultra,
}

impl Effort {
    /// Parses the stable wire spelling.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Wire`] for an unknown effort class.
    pub fn parse(value: &str) -> Result<Self, ControlError> {
        match value {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::XHigh),
            "max" => Ok(Self::Max),
            "ultra" => Ok(Self::Ultra),
            _ => Err(ControlError::Wire("invalid effort class".to_owned())),
        }
    }

    /// Returns the stable wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }

    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
            Self::XHigh => 3,
            Self::Max => 4,
            Self::Ultra => 5,
        }
    }

    pub(crate) const fn from_tag(tag: u8) -> Result<Self, ControlError> {
        match tag {
            0 => Ok(Self::Low),
            1 => Ok(Self::Medium),
            2 => Ok(Self::High),
            3 => Ok(Self::XHigh),
            4 => Ok(Self::Max),
            5 => Ok(Self::Ultra),
            _ => Err(ControlError::Corrupt),
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A prerequisite key plus the canonical key preimage needed to re-admit it
/// after a crash. Dependencies are copied as small immutable byte slices and
/// never require opening the dependency's worktree or transcript.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkDependency {
    key: AgentWorkKey,
    material: Box<[u8]>,
}

impl WorkDependency {
    /// Creates a dependency from another immutable specification.
    ///
    /// # Errors
    ///
    /// Returns an identity-bound error if the dependency's canonical key
    /// material exceeds the admission bound.
    pub fn from_spec(spec: &WorkSpec) -> Result<Self, ControlError> {
        Ok(Self {
            key: spec.key(),
            material: spec.key_material()?.into_boxed_slice(),
        })
    }

    pub(crate) fn from_material(material: Vec<u8>) -> Result<Self, ControlError> {
        let key = derive::<WorkKeySchema>(&material)?;
        Ok(Self {
            key,
            material: material.into_boxed_slice(),
        })
    }

    /// Returns the prerequisite work key.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.key
    }

    pub(crate) fn material(&self) -> &[u8] {
        &self.material
    }
}

/// The complete immutable identity input for one agent run.
///
/// `AgentWorkKey` is derived from this object. It is impossible to mutate a
/// field after construction, and every root is included in the canonical
/// preimage. As a result, a receipt can be reused only when all production
/// inputs match exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkSpec {
    cell: Identity<CellSchema>,
    role: Identity<RoleSchema>,
    model: Identity<ModelSchema>,
    effort: Effort,
    toolchain: Identity<ToolchainSchema>,
    input: Identity<InputSchema>,
    dependencies: Box<[WorkDependency]>,
    dependency_root: Identity<DependencySchema>,
    key: AgentWorkKey,
}

impl WorkSpec {
    /// Constructs an immutable, canonical work specification from its
    /// retained canonical facet values.
    ///
    /// # Errors
    ///
    /// Returns an error when dependencies are oversized, duplicated, or
    /// produce a specification beyond the canonical wire bound.
    pub fn new(
        cell: Identity<CellSchema>,
        role: Identity<RoleSchema>,
        model: Identity<ModelSchema>,
        effort: Effort,
        toolchain: Identity<ToolchainSchema>,
        input: Identity<InputSchema>,
        dependencies: impl IntoIterator<Item = WorkDependency>,
    ) -> Result<Self, ControlError> {
        let mut dependencies = dependencies.into_iter().collect::<Vec<_>>();
        if dependencies.len() > MAX_DEPENDENCIES {
            return Err(ControlError::Bounds);
        }
        dependencies.sort_unstable_by_key(WorkDependency::key);
        if dependencies
            .windows(2)
            .any(|window| window[0].key() == window[1].key())
        {
            return Err(ControlError::NonCanonicalDependencies);
        }
        let dependency_root = dependency_root(&dependencies)?;
        let key = derive_key(
            cell.id(),
            role.id(),
            model.id(),
            effort,
            toolchain.id(),
            input.id(),
            dependency_root.id(),
        )?;
        let spec = Self {
            cell,
            role,
            model,
            effort,
            toolchain,
            input,
            dependencies: dependencies.into_boxed_slice(),
            dependency_root,
            key,
        };
        if spec.canonical_bytes()?.len() > MAX_WORK_SPEC_BYTES {
            return Err(ControlError::Bounds);
        }
        Ok(spec)
    }

    /// Creates a convenient specification from stable labels and root bytes.
    ///
    /// # Errors
    ///
    /// Returns an identity, effort, dependency, or canonical-size error.
    pub fn from_labels(
        cell: &str,
        role: &str,
        model: &str,
        effort: Effort,
        toolchain: &[u8],
        input: &[u8],
        dependencies: impl IntoIterator<Item = WorkDependency>,
    ) -> Result<Self, ControlError> {
        Self::new(
            Identity::from_label(cell)?,
            Identity::from_label(role)?,
            Identity::from_label(model)?,
            effort,
            Identity::from_bytes(toolchain.to_vec())?,
            Identity::from_bytes(input.to_vec())?,
            dependencies,
        )
    }

    /// Returns the complete immutable work key.
    #[must_use]
    pub const fn key(&self) -> AgentWorkKey {
        self.key
    }

    /// Returns the cell identity.
    #[must_use]
    pub const fn cell(&self) -> CellId {
        self.cell.id()
    }

    /// Returns the role contract identity.
    #[must_use]
    pub const fn role(&self) -> RoleId {
        self.role.id()
    }

    /// Returns the model class identity.
    #[must_use]
    pub const fn model(&self) -> ModelId {
        self.model.id()
    }

    /// Returns the requested effort class.
    #[must_use]
    pub const fn effort(&self) -> Effort {
        self.effort
    }

    /// Returns the exact immutable toolchain root.
    #[must_use]
    pub const fn toolchain(&self) -> ToolchainRoot {
        self.toolchain.id()
    }

    /// Returns the exact production input root.
    #[must_use]
    pub const fn input(&self) -> InputRoot {
        self.input.id()
    }

    /// Returns the sorted prerequisite key list.
    pub fn dependencies(&self) -> impl Iterator<Item = AgentWorkKey> + '_ {
        self.dependencies.iter().map(WorkDependency::key)
    }

    /// Returns the content identity of the prerequisite list.
    #[must_use]
    pub const fn dependency_root(&self) -> DependencyRoot {
        self.dependency_root.id()
    }

    /// Returns the canonical bytes for this specification.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] if the admitted specification exceeds
    /// its canonical size limit.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, ControlError> {
        let mut bytes = Vec::new();
        self.append_canonical(&mut bytes)?;
        Ok(bytes)
    }

    pub(crate) fn append_canonical(&self, output: &mut Vec<u8>) -> Result<(), ControlError> {
        output.push(1); // WorkSpec wire version.
        append_identity::<CellSchema>(output, &self.cell)?;
        append_identity::<RoleSchema>(output, &self.role)?;
        append_identity::<ModelSchema>(output, &self.model)?;
        output.push(self.effort.tag());
        append_identity::<ToolchainSchema>(output, &self.toolchain)?;
        append_identity::<InputSchema>(output, &self.input)?;
        append_identity::<DependencySchema>(output, &self.dependency_root)?;
        let count = u32::try_from(self.dependencies.len()).map_err(|_| ControlError::Bounds)?;
        output.extend_from_slice(&count.to_be_bytes());
        for dependency in &self.dependencies {
            append_version::<WorkKeySchema>(output, dependency.key());
            append_field(output, dependency.material())?;
        }
        if output.len() > MAX_WORK_SPEC_BYTES {
            return Err(ControlError::Bounds);
        }
        Ok(())
    }

    /// Appends the already admitted canonical representation. `WorkSpec::new`
    /// checks the same bounds before a specification enters a work record, so
    /// the relation encoder can remain total and cannot collapse an encoding
    /// error into an empty value.
    pub(crate) fn append_canonical_unchecked(&self, output: &mut Vec<u8>) {
        output.push(1);
        append_identity_unchecked::<CellSchema>(output, &self.cell);
        append_identity_unchecked::<RoleSchema>(output, &self.role);
        append_identity_unchecked::<ModelSchema>(output, &self.model);
        output.push(self.effort.tag());
        append_identity_unchecked::<ToolchainSchema>(output, &self.toolchain);
        append_identity_unchecked::<InputSchema>(output, &self.input);
        append_identity_unchecked::<DependencySchema>(output, &self.dependency_root);
        // `WorkSpec::new` admits at most MAX_DEPENDENCIES entries, so this
        // conversion is total for every value that can reach the relation.
        #[allow(clippy::cast_possible_truncation)]
        let count = self.dependencies.len() as u32;
        output.extend_from_slice(&count.to_be_bytes());
        for dependency in &self.dependencies {
            append_version::<WorkKeySchema>(output, dependency.key());
            append_field_unchecked(output, dependency.material());
        }
    }

    pub(crate) fn key_material(&self) -> Result<Vec<u8>, ControlError> {
        let mut bytes = Vec::with_capacity(7 * (1 + backend_version::ID_BYTES) + 16);
        bytes.extend_from_slice(b"agent-work-key-v1");
        append_version::<CellSchema>(&mut bytes, self.cell.id());
        append_version::<RoleSchema>(&mut bytes, self.role.id());
        append_version::<ModelSchema>(&mut bytes, self.model.id());
        bytes.push(self.effort.tag());
        append_version::<ToolchainSchema>(&mut bytes, self.toolchain.id());
        append_version::<InputSchema>(&mut bytes, self.input.id());
        append_version::<DependencySchema>(&mut bytes, self.dependency_root.id());
        if bytes.len() > MAX_WORK_SPEC_BYTES {
            return Err(ControlError::Bounds);
        }
        Ok(bytes)
    }
}

fn derive_key(
    cell: CellId,
    role: RoleId,
    model: ModelId,
    effort: Effort,
    toolchain: ToolchainRoot,
    input: InputRoot,
    dependency_root: DependencyRoot,
) -> Result<AgentWorkKey, ControlError> {
    let mut bytes = Vec::with_capacity(7 * (1 + backend_version::ID_BYTES) + 32);
    bytes.extend_from_slice(b"agent-work-key-v1");
    append_version::<CellSchema>(&mut bytes, cell);
    append_version::<RoleSchema>(&mut bytes, role);
    append_version::<ModelSchema>(&mut bytes, model);
    bytes.push(effort.tag());
    append_version::<ToolchainSchema>(&mut bytes, toolchain);
    append_version::<InputSchema>(&mut bytes, input);
    append_version::<DependencySchema>(&mut bytes, dependency_root);
    derive::<WorkKeySchema>(&bytes)
}

fn dependency_root(
    dependencies: &[WorkDependency],
) -> Result<Identity<DependencySchema>, ControlError> {
    let mut bytes = Vec::with_capacity(4 + dependencies.len() * backend_version::ID_BYTES);
    let count = u32::try_from(dependencies.len()).map_err(|_| ControlError::Bounds)?;
    bytes.extend_from_slice(&count.to_be_bytes());
    for dependency in dependencies {
        append_version::<WorkKeySchema>(&mut bytes, dependency.key());
    }
    Identity::<DependencySchema>::from_bytes(bytes)
}
