//! [`Producer`] implementation for the TypeScript/OXC tier.
//!
//! # tsz seam
//! The [`TsOracle`] sealed trait is the plug point for a future tsz tier.
//! Today only [`OwnedOracle`] (the OXC path) implements it. A tsz oracle
//! would implement `TsOracle` and be selected by constructing
//! `TypescriptProducer::<TszOracle>::new()`.
//!
//! The trait is sealed (private supertrait) so external crates cannot
//! implement it without touching this crate.

use nudox_ir::body::Language;

use nudox_producer::{PackageSource, Producer, ProducerError, ProducerId};
use nudox_ir::lower::Lowering;

use crate::{
    emit::lower_package,
    entry::discover_entry_points,
    extract::ModuleFacts,
    graph::build_and_extract,
    id::TsId,
};

// ── Sealed trait (tsz seam) ───────────────────────────────────────────────────

mod sealed {
    pub trait TsOracleSeal {}
}

/// Marker trait for TypeScript oracle implementations.
///
/// Currently only [`OwnedOracle`] (OXC) implements this. A future tsz oracle
/// would implement both this trait and the `Producer::Oracle` contract.
pub trait TsOracle: sealed::TsOracleSeal {
    fn modules(&self) -> &[ModuleFacts];
}

// ── OXC oracle ────────────────────────────────────────────────────────────────

/// The owned result of the OXC extraction pass.
///
/// All AST arenas have been dropped by the time this exists. It contains
/// only fully-owned `ModuleFacts` — no arena references.
pub struct OwnedOracle {
    pub(crate) modules: Vec<ModuleFacts>,
}

impl sealed::TsOracleSeal for OwnedOracle {}

impl TsOracle for OwnedOracle {
    fn modules(&self) -> &[ModuleFacts] {
        &self.modules
    }
}

// ── Producer ──────────────────────────────────────────────────────────────────

/// The TypeScript producer, parameterized over the oracle tier.
///
/// Use `TypescriptProducer::new()` for the default OXC tier.
/// A future tsz tier would be selected via `TypescriptProducer::<TszOracle>::new_tsz(...)`.
pub struct TypescriptProducer<O = OwnedOracle> {
    _marker: std::marker::PhantomData<O>,
}

impl TypescriptProducer<OwnedOracle> {
    /// Construct a new TypeScript producer backed by the OXC in-process tier.
    pub fn new() -> Self {
        TypescriptProducer { _marker: std::marker::PhantomData }
    }
}

impl Default for TypescriptProducer<OwnedOracle> {
    fn default() -> Self {
        Self::new()
    }
}

impl<O: TsOracle> Producer for TypescriptProducer<O>
where
    O: From<OwnedOracle>,
{
    type Id = TsId;
    type Oracle = O;

    const ID: ProducerId = ProducerId("typescript-oxc/1");
    const LANGUAGE: Language = Language::TypeScript;

    fn invoke(&self, src: &PackageSource) -> Result<O, ProducerError> {
        let entry_points = discover_entry_points(src.root())
            .map_err(|e| ProducerError::OracleSpawn {
                command: "oxc-entry-discovery".to_string(),
                reason: e.to_string(),
            })?;

        let modules = build_and_extract(&entry_points, src.root())
            .map_err(|e| ProducerError::OracleSpawn {
                command: "oxc-extract".to_string(),
                reason: e.to_string(),
            })?;

        Ok(OwnedOracle { modules }.into())
    }

    fn lower(
        &self,
        oracle: &O,
        out: &mut Lowering<TsId>,
    ) -> Result<(), ProducerError> {
        lower_package(oracle.modules(), out);
        Ok(())
    }
}

// Blanket: OwnedOracle converts to itself (the default tier).
impl From<OwnedOracle> for OwnedOracle {
    fn from(o: OwnedOracle) -> Self {
        o
    }
}
