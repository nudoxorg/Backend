//! Acquire → Seal phase typestate (DAEMON-PLAN §2.1).
//!
//! The acquire/seal boundary *is* the security model: acquisition may reach the
//! network for hash-pinned fetches; sealed compute may not. This module makes
//! that boundary a type, not a comment — so "sealed job with the network on" is
//! **unrepresentable**, not merely rejected at runtime.
//!
//! ```text
//! Job<Acquiring>  net: caller's choice (FOD fetches)
//!      │ seal()
//!      ▼
//! Job<Sealed>     net: statically Off — no method can set it On
//! ```
//!
//! The trick: [`Job<Sealed>`] stores a [`SealedBudget`], whose network field is
//! the zero-variant [`NetOff`] marker instead of [`NetGrant`]. There is no
//! constructor, setter, or `From` that yields a `SealedBudget` with the network
//! on, so the compiler rejects it before any runtime check runs.

pub mod cancel;
pub mod worker;

use std::marker::PhantomData;
use std::path::PathBuf;

use heart::JobKey;

use crate::budget::limits::Limits;
use crate::budget::{CapabilityBudget, FsGrant, NetGrant, ThreatTier};
use crate::seal::SealedInput;
use crate::spec::Env;

/// Phase marker: acquisition may use the network (FOD-only fetches).
#[derive(Debug, Clone, Copy)]
pub struct Acquiring;

/// Phase marker: sealed compute — network is statically off.
#[derive(Debug, Clone, Copy)]
pub struct Sealed;

/// Sealed for [`Acquiring`] and [`Sealed`]; not extensible downstream.
mod sealed {
    pub trait Phase {}
    impl Phase for super::Acquiring {}
    impl Phase for super::Sealed {}
}

/// A phase of a compute job: [`Acquiring`] (net-capable) or [`Sealed`] (net-off).
///
/// The phase is a type parameter, so the acquire→seal transition is enforced by
/// the type system rather than by a runtime flag.
#[derive(Debug, Clone)]
pub struct Job<P: sealed::Phase> {
    key: JobKey,
    root: PathBuf,
    fs: FsGrant,
    env: Env,
    resources: Limits,
    /// Only meaningful in [`Acquiring`]; [`Job::seal`] discards it (forces off).
    net: NetGrant,
    _phase: PhantomData<P>,
}

impl Job<Acquiring> {
    /// Start an acquiring job. Network posture is the caller's choice here
    /// (FOD fetches are allowed to reach hash-pinned mirrors).
    pub fn acquiring(
        key: JobKey,
        root: impl Into<PathBuf>,
        fs: FsGrant,
        env: Env,
        resources: Limits,
        net: NetGrant,
    ) -> Self {
        Self {
            key,
            root: root.into(),
            fs,
            env,
            resources,
            net,
            _phase: PhantomData,
        }
    }

    /// Network posture during acquisition.
    pub fn net(&self) -> &NetGrant {
        &self.net
    }

    /// Transition to the sealed phase, clamping resources to `tier` and forcing
    /// the network **off**.
    ///
    /// This is the only constructor of a [`Job<Sealed>`]. The acquiring `net`
    /// grant — whatever it was — is dropped: a sealed job cannot carry net-on.
    pub fn seal(self, tier: ThreatTier) -> Job<Sealed> {
        Job {
            key: self.key,
            root: self.root,
            fs: self.fs,
            env: self.env,
            resources: tier.clamp(self.resources),
            net: tier.net_default(), // always NetGrant::Off
            _phase: PhantomData,
        }
    }
}

impl Job<Sealed> {
    /// The content-addressed job identity.
    pub fn key(&self) -> JobKey {
        self.key
    }

    /// Package root.
    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// The sealed budget. Its network field is [`NetOff`]: there is no value of
    /// this type that carries the network on.
    pub fn budget(&self) -> SealedBudget {
        SealedBudget {
            fs: self.fs.clone(),
            net: NetOff,
            env: self.env.clone(),
            resources: self.resources,
        }
    }

    /// Project into a [`SealedInput`] for `run_producer`.
    ///
    /// The projected [`CapabilityBudget`] necessarily has `net: NetGrant::Off`,
    /// because a [`SealedBudget`] cannot express anything else.
    pub fn into_input(self) -> SealedInput {
        let key = self.key;
        let root = self.root.clone();
        SealedInput::new(key, root, self.budget().into_capability_budget())
    }
}

/// Uninhabitable "network on" — a sealed budget's net field.
///
/// A unit struct with no other variant: the only network state a sealed budget
/// can name is *off*. Trying to seal with the network on is not a runtime error,
/// it is a type error, because there is no `NetOff` value that means "on".
///
/// A [`Job<Sealed>`] exposes its budget only as a [`SealedBudget`], whose `net`
/// field is this `NetOff`. Assigning `NetGrant::On` to it does not compile:
///
/// ```compile_fail
/// use sandbox::{Acquiring, Job, NetGrant, ThreatTier, FsGrant, Env};
/// use sandbox::ProducerProfile;
/// use heart::JobKey;
///
/// let key = JobKey::derive(b"t", b"", b"root", b"");
/// let job = Job::acquiring(
///     key, "/pkg",
///     FsGrant::scratch("/tmp"),
///     Env::empty(),
///     ProducerProfile::Nix.limits(),
///     NetGrant::permissive(),
/// );
/// let mut budget = job.seal(ThreatTier::Hostile).budget();
/// // `budget.net` has type `NetOff`; `NetGrant` is a different type.
/// budget.net = NetGrant::permissive(); // mismatched types — does not compile
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetOff;

impl From<NetOff> for NetGrant {
    fn from(_: NetOff) -> Self {
        NetGrant::Off
    }
}

/// A [`CapabilityBudget`] whose network is statically off.
///
/// Mirrors [`CapabilityBudget`] field for field, except `net: NetOff` instead of
/// `net: NetGrant`. This is what a [`Job<Sealed>`] hands out; converting to the
/// dynamic [`CapabilityBudget`] can only ever produce `NetGrant::Off`.
#[derive(Debug, Clone)]
pub struct SealedBudget {
    /// Filesystem grant.
    pub fs: FsGrant,
    /// Statically-off network (no other value exists).
    pub net: NetOff,
    /// Env allowlist.
    pub env: Env,
    /// Resource ceilings (already tier-clamped).
    pub resources: Limits,
}

impl SealedBudget {
    /// Lower into the dynamic [`CapabilityBudget`]; `net` is always `Off`.
    pub fn into_capability_budget(self) -> CapabilityBudget {
        CapabilityBudget::new(self.fs, self.net.into(), self.env, self.resources)
    }
}

/// Witness that a production-grade cage backs the forge (SV-4).
///
/// `Compile` implementations over untrusted sources must demand a `VmForge`
/// in their construction path: it is obtainable only from a cage whose
/// [`capabilities`](crate::cage::Cage::capabilities) report
/// `production_grade` (today: [`crate::cage::smolvm::SmolvmCage`]), never from
/// [`crate::cage::DevPassthrough`] and never in-process. Holding one is
/// therefore a static proof that untrusted code will execute behind the
/// hardware-virtualization boundary.
///
/// The only constructor is [`VmForge::over`]; the field is private, so a
/// witness cannot be forged by literal construction:
///
/// ```compile_fail
/// use sandbox::{VmForge, CageId};
///
/// // The `cage` field is private — struct-literal construction is a
/// // compile error, not a runtime check.
/// let forged = VmForge { cage: CageId("dev-passthrough") };
/// ```
#[derive(Debug, Clone)]
pub struct VmForge {
    cage: crate::cage::CageId,
}

impl VmForge {
    /// Obtain a witness over `cage`, refusing non-production-grade cages.
    pub fn over<C: crate::cage::Cage + ?Sized>(cage: &C) -> Result<Self, crate::error::CageError> {
        let caps = cage.capabilities();
        if caps.production_grade {
            Ok(Self { cage: cage.id() })
        } else {
            Err(crate::error::CageError::Denied {
                reason: format!(
                    "VmForge requires a production-grade cage; `{}` is not",
                    cage.id()
                ),
            })
        }
    }

    /// The cage this witness was taken over.
    pub fn cage_id(&self) -> &crate::cage::CageId {
        &self.cage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::profiles::ProducerProfile;

    fn scratch_fs() -> FsGrant {
        FsGrant::scratch(std::env::temp_dir())
    }

    #[test]
    fn seal_forces_network_off_regardless_of_acquiring_net() {
        let key = JobKey::derive(b"t", b"", b"root", b"");
        let job = Job::acquiring(
            key,
            "/pkg",
            scratch_fs(),
            Env::empty(),
            ProducerProfile::Nix.base_limits(),
            NetGrant::permissive(), // acquiring may have net on
        );
        assert!(matches!(job.net(), NetGrant::On(_)));

        let sealed = job.seal(ThreatTier::Hostile);
        let budget = sealed.budget();
        // The sealed budget's net is the NetOff marker, which lowers to Off.
        let net: NetGrant = budget.net.into();
        assert_eq!(net, NetGrant::Off);
        assert_eq!(
            budget.into_capability_budget().net,
            NetGrant::Off,
            "a sealed job can never carry net-on"
        );
    }

    #[test]
    fn vm_forge_refuses_non_production_cages() {
        let dev = crate::cage::DevPassthrough::try_new(crate::cage::Policy::Development)
            .expect("dev cage");
        let err = VmForge::over(&dev).expect_err("dev passthrough is not production-grade");
        assert!(matches!(err, crate::error::CageError::Denied { .. }));
    }

    #[test]
    fn seal_clamps_hostile_limits() {
        let key = JobKey::derive(b"t", b"", b"root", b"");
        // Rust profile is 6 GiB / 15 min — far above the Hostile ceiling.
        let job = Job::acquiring(
            key,
            "/pkg",
            scratch_fs(),
            Env::empty(),
            ProducerProfile::Rust.base_limits(),
            NetGrant::Off,
        );
        let sealed = job.seal(ThreatTier::Hostile);
        let b = sealed.budget();
        assert!(
            b.resources.mem_bytes.get() <= 2 * 1024 * 1024 * 1024,
            "hostile must clamp mem to its ceiling"
        );
        assert!(b.resources.wall.as_secs() <= 5 * 60);
    }
}
