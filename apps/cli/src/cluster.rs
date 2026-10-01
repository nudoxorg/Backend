//! Owner-side commands for the local compiler peer allowlist.

use crate::options::{Format, Options};
use backend_engine::PackageReference;
use backend_engine::application::{
    CompilerPackageTargetV2, GoPackageAuthorityWitness, LocalCompilerCapabilityState,
    LocalCompilerExecutionIdentity, LocalCompilerHost,
};
use backend_engine::cluster_transport::{ClusterExecutionClass, EndpointId, ScopedClusterInvite};
use backend_library::interface::PackageUrl;
use backend_local_service::builtin::{
    ProductCompilerScope, ProductCompilerTargetKind, product_compiler_scope,
};
use backend_local_service::cluster_owner::{ClusterOwnerConfig, ClusterOwnerConfigError};
use backend_local_service::compiler_trust::{
    CompilerTrustError, TrustedCompilerWorkerGrant, TrustedCompilerWorkerPolicy,
};
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use backend_semantic::vocabulary::{LanguageProfile, Stage};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;

const TRUST_FILE: &str = "compiler-worker-trust.v1";
const OWNER_FILE: &str = "cluster-owner.v1";

/// Runs one owner-side cluster trust command without starting or contacting locald.
pub(super) fn run(words: &[String], options: &Options) -> Result<String, Fault> {
    match words {
        [trust, action, rest @ ..] if trust == "trust" => match action.as_str() {
            "list" => list(rest, options),
            "add" => add(rest, options),
            "revoke" => revoke(rest, options),
            _ => Err(usage("cluster trust", "choose list, add, or revoke")),
        },
        [owner, action, rest @ ..] if owner == "owner" => match action.as_str() {
            "init" => owner_init(rest, options),
            "show" => owner_show(rest, options),
            _ => Err(usage("cluster owner", "choose init or show")),
        },
        [invite, action, rest @ ..] if invite == "invite" && action == "create" => {
            invite_create(rest, options)
        }
        [scope, action, rest @ ..] if scope == "scope" => match action.as_str() {
            "show" => scope_show(rest, options),
            _ => Err(usage("cluster scope", "choose show")),
        },
        [help] if help == "help" => Ok(help_text().to_owned()),
        [] => Ok(help_text().to_owned()),
        _ => Err(usage(
            "cluster",
            "use `cluster owner init|show`, `cluster scope show`, `cluster trust list|add|revoke`, or `cluster invite create`",
        )),
    }
}

pub(super) const fn help_text() -> &'static str {
    "backend cluster — local compiler owner and explicit worker trust\n\n\
     Usage: backend [OPTIONS] cluster owner init --bind IP:PORT --advertise IP:PORT\n\
            backend [OPTIONS] cluster owner show\n\
            backend [OPTIONS] cluster scope show --package PACKAGE --profile PROFILE [--coordinate PKGURL] [--package-root PATH]\n\
            backend [OPTIONS] cluster invite create --worker-peer HEX --worker-address IP:PORT --namespace HEX --recipe HEX --profile HEX --stage lower-ir --toolchain HEX --environment HEX --target-platform HEX [--ttl-seconds N]\n\
            backend [OPTIONS] cluster trust list\n\
            backend [OPTIONS] cluster trust add --peer HEX --address IP:PORT --namespace HEX --recipe HEX --profile HEX --stage parse|lower-ir --toolchain HEX --environment HEX --target-platform HEX\n\
            backend [OPTIONS] cluster trust revoke --peer HEX\n\n\
     Owner identity is stored owner-only under the selected locald data root in cluster-owner.v1.\n\
     Inviting a worker atomically records its exact peer and execution grant in compiler-worker-trust.v1 and prints the one-time import token.\n\
     Use --workspace PATH to select an isolated private data root; by default each project has a separate root under per-user app data.\n\
     `cluster scope show` reports the exact namespace and admitted lower-ir compiler identity for a package/profile. A pinned package URL is its default compiler coordinate; local package labels use locald's package-coordinate resolver unless --coordinate is supplied. For Go, `--package-root` inspects module/workspace manifests and local filesystem directives; packages that depend on local workspace, replace, or use targets remain local-only until those inputs are in the transferable closure. Without a root, package eligibility requires project inspection. Go cgo package graphs are unsupported by the current closed oracle policy.\n\
     Scope inspection creates no runtime directories and starts no compiler owner; it performs bounded version/authority probes. Its identity matches locald when both admit the same compiler and authority configuration for the same target platform.\n\
     The authenticated peer identity and every execution-scope field must match exactly."
}

fn scope_show(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &["package", "profile", "coordinate", "package-root"],
        &["package", "profile"],
    )?;
    let package_text = required(&flags, "package")?;
    let package = PackageReference::parse(package_text.to_owned()).map_err(|_| {
        usage(
            "--package",
            "use a canonical pinned package URL or local package label",
        )
    })?;
    let profile_text = required(&flags, "profile")?;
    let profile = LanguageProfile::try_from(profile_text)
        .map_err(|_| usage("--profile", "use a canonical language profile spelling"))?;
    let coordinate = flags
        .get("coordinate")
        .map(|value| {
            PackageUrl::parse((*value).to_owned())
                .map_err(|error| usage("--coordinate", format!("invalid package URL: {error:?}")))
        })
        .transpose()?
        .or(match &package {
            PackageReference::Purl(_) => Some(
                PackageUrl::parse(package.as_str().to_owned()).map_err(|error| {
                    usage(
                        "--package",
                        format!("invalid pinned compiler coordinate: {error:?}"),
                    )
                })?,
            ),
            PackageReference::Local(_) => None,
        });
    if matches!(package, PackageReference::Purl(_)) && coordinate.is_none() {
        return Err(usage(
            "--package",
            "the pinned package URL could not be used as a compiler coordinate; supply --coordinate",
        ));
    }
    let scope = product_compiler_scope(package.clone(), profile, coordinate.as_ref())
        .map_err(|error| compiler_scope_fault(package.as_str(), &error.to_string()))?;
    let compiler_root = compiler_data_root(options)?.join("compiler");
    let capabilities = LocalCompilerHost::production_at(compiler_root.clone())
        .inspect_capabilities()
        .map_err(|error| {
            compiler_capability_fault(
                profile_text,
                &format!(
                    "could not inspect the production compiler capability at {}: {error}",
                    compiler_root.display()
                ),
            )
        })?;
    let capability = capabilities.for_profile(profile);
    if capability.state() != LocalCompilerCapabilityState::Ready {
        return Err(compiler_capability_fault(
            profile_text,
            &format!(
                "local compiler capability state is {:?}",
                capability.state()
            ),
        ));
    }
    let target = CompilerPackageTargetV2::for_package(scope.coordinate().clone());
    let identity = capability
        .execution_identity_for_unit(&target, profile, Stage::LowerIr)
        .filter(|identity| {
            identity.target() == target.target()
                && identity.profile() == profile
                && identity.stage() == Stage::LowerIr
        })
        .ok_or_else(|| {
            compiler_capability_fault(
                profile_text,
                "the ready capability did not admit a portable lower-ir invocation identity for this target",
            )
        })?;
    let placement = package_placement(profile, flags.get("package-root").copied())?;
    let report = ClusterScopeReport::new(package.as_str(), profile_text, &scope, identity)
        .with_placement(placement);
    match options.format() {
        Format::Json => serde_json::to_string_pretty(&report.json())
            .map(|mut value| {
                value.push('\n');
                value
            })
            .map_err(|error| storage_fault(&compiler_root, error.to_string())),
        Format::Human | Format::Markdown => Ok(report.human()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ClusterScopeReport {
    target_kind: &'static str,
    package: String,
    coordinate: String,
    namespace: [u8; 16],
    recipe: [u8; 32],
    profile: [u8; 2],
    profile_name: String,
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
    package_eligibility: &'static str,
    placement: &'static str,
    placement_reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PackagePlacement {
    eligibility: &'static str,
    placement: &'static str,
    reason: Option<&'static str>,
}

fn package_placement(
    profile: LanguageProfile,
    package_root: Option<&str>,
) -> Result<PackagePlacement, Fault> {
    let root = package_root.map(PathBuf::from);
    if root.is_some() && profile.language() != backend_semantic::vocabulary::Language::Go {
        return Err(usage(
            "--package-root",
            "project placement inspection is currently supported only for Go profiles",
        ));
    }
    if let Some(root) = root {
        let witness = GoPackageAuthorityWitness::capture(&root).map_err(|error| {
            usage(
                "--package-root",
                format!("could not inspect Go package authority inputs: {error}"),
            )
        })?;
        if witness.requires_local_execution() {
            return Ok(PackagePlacement {
                eligibility: "local-only",
                placement: "local-only",
                reason: witness.local_only_reason().map(|reason| reason.code()),
            });
        }
    }
    Ok(PackagePlacement {
        eligibility: "requires-project-inspection",
        placement: "requires-project-inspection",
        reason: None,
    })
}

impl ClusterScopeReport {
    fn new(
        package: &str,
        profile_name: &str,
        scope: &ProductCompilerScope,
        identity: LocalCompilerExecutionIdentity,
    ) -> Self {
        Self::from_invocation(
            package,
            profile_name,
            scope,
            identity.profile(),
            identity.invocation_recipe(),
        )
    }

    fn from_invocation(
        package: &str,
        profile_name: &str,
        scope: &ProductCompilerScope,
        profile: LanguageProfile,
        invocation: backend_engine::application::CompilerInvocationRecipeV2,
    ) -> Self {
        Self {
            target_kind: match scope.target_kind() {
                ProductCompilerTargetKind::PinnedRegistry => "pinned-registry",
                ProductCompilerTargetKind::Local => "local",
            },
            package: package.to_owned(),
            coordinate: scope.coordinate().as_str().to_owned(),
            namespace: scope.namespace_id(),
            recipe: *invocation.identity().as_ref(),
            profile: <[u8; 2]>::from(profile),
            profile_name: profile_name.to_owned(),
            toolchain: *invocation.toolchain().as_ref(),
            environment: invocation.environment(),
            target_platform: invocation.target_platform(),
            package_eligibility: "requires-project-inspection",
            placement: "requires-project-inspection",
            placement_reason: None,
        }
    }

    fn with_placement(mut self, placement: PackagePlacement) -> Self {
        self.package_eligibility = placement.eligibility;
        self.placement = placement.placement;
        self.placement_reason = placement.reason;
        self
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "target_kind": self.target_kind,
            "package": self.package,
            "coordinate": self.coordinate,
            "namespace": hex(&self.namespace),
            "recipe": hex(&self.recipe),
            "profile": hex(&self.profile),
            "profile_name": self.profile_name,
            "stage": "lower-ir",
            "toolchain": hex(&self.toolchain),
            "environment": hex(&self.environment),
            "target_platform": hex(&self.target_platform),
            "package_eligibility": self.package_eligibility,
            "placement": self.placement,
            "placement_reason": self.placement_reason,
        })
    }

    fn human(&self) -> String {
        format!(
            "Compiler scope for {} package {}\nCompiler coordinate: {}\nNamespace: {}\nProfile: {} ({})\nStage: lower-ir\nRecipe: {}\nToolchain: {}\nEnvironment: {}\nTarget platform: {}\nPackage eligibility: {}\nPlacement: {}\nPlacement reason: {}\n",
            self.target_kind,
            self.package,
            self.coordinate,
            hex(&self.namespace),
            self.profile_name,
            hex(&self.profile),
            hex(&self.recipe),
            hex(&self.toolchain),
            hex(&self.environment),
            hex(&self.target_platform),
            self.package_eligibility,
            self.placement,
            self.placement_reason.unwrap_or("none"),
        )
    }
}

fn owner_init(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags(rest, &["bind", "advertise"])?;
    let bind = socket_address(required(&flags, "bind")?, "--bind")?;
    let advertised = socket_address(required(&flags, "advertise")?, "--advertise")?;
    let path = owner_path(options)?;
    let owner = ClusterOwnerConfig::create(&path, bind, advertised)
        .map_err(|error| owner_fault(&path, error))?;
    Ok(format!(
        "Created local compiler owner {} at {}.\n",
        hex(owner.endpoint_id().as_bytes()),
        owner.advertised_address(),
    ))
}

fn owner_show(rest: &[String], options: &Options) -> Result<String, Fault> {
    if !rest.is_empty() {
        return Err(usage("cluster owner show", "takes no command options"));
    }
    let path = owner_path(options)?;
    let owner = ClusterOwnerConfig::load(&path).map_err(|error| owner_fault(&path, error))?;
    match options.format() {
        Format::Json => serde_json::to_string_pretty(&serde_json::json!({
            "endpoint": hex(owner.endpoint_id().as_bytes()),
            "bindAddress": owner.bind_address().to_string(),
            "advertisedAddress": owner.advertised_address().to_string(),
        }))
        .map(|mut value| {
            value.push('\n');
            value
        })
        .map_err(|error| storage_fault(&path, error.to_string())),
        Format::Human | Format::Markdown => Ok(format!(
            "Local compiler owner {}\nBind address: {}\nWorker address: {}\n",
            hex(owner.endpoint_id().as_bytes()),
            owner.bind_address(),
            owner.advertised_address(),
        )),
    }
}

fn invite_create(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &[
            "worker-peer",
            "worker-address",
            "namespace",
            "recipe",
            "profile",
            "stage",
            "toolchain",
            "environment",
            "target-platform",
            "ttl-seconds",
        ],
        &[
            "worker-peer",
            "worker-address",
            "namespace",
            "recipe",
            "profile",
            "stage",
            "toolchain",
            "environment",
            "target-platform",
        ],
    )?;
    let peer = endpoint_id(required(&flags, "worker-peer")?)?;
    let address = socket_address(required(&flags, "worker-address")?, "--worker-address")?;
    let namespace_id = fixed_hex::<16>(required(&flags, "namespace")?)?;
    let recipe = fixed_hex::<32>(required(&flags, "recipe")?)?;
    let profile_bytes = fixed_hex::<2>(required(&flags, "profile")?)?;
    let profile = LanguageProfile::try_from(profile_bytes)
        .map_err(|_| usage("--profile", "use a canonical two-byte profile code"))?;
    let stage = parse_stage(required(&flags, "stage")?)?;
    if stage != Stage::LowerIr {
        return Err(usage("--stage", "remote compiler invites require lower-ir"));
    }
    let toolchain = fixed_hex::<32>(required(&flags, "toolchain")?)?;
    let environment = fixed_hex::<32>(required(&flags, "environment")?)?;
    let target_platform = fixed_hex::<32>(required(&flags, "target-platform")?)?;
    let ttl_seconds = flags
        .get("ttl-seconds")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| usage("--ttl-seconds", "use an integer between 60 and 86400"))
        })
        .transpose()?
        .unwrap_or(900);
    if !(60..=86_400).contains(&ttl_seconds) {
        return Err(usage(
            "--ttl-seconds",
            "use an integer between 60 and 86400",
        ));
    }

    let owner_path = owner_path(options)?;
    let owner =
        ClusterOwnerConfig::load(&owner_path).map_err(|error| owner_fault(&owner_path, error))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| usage("cluster invite", "system clock predates Unix epoch"))?
        .as_millis();
    let now = u64::try_from(now).map_err(|_| usage("cluster invite", "system clock overflow"))?;
    let expiry = now
        .checked_add(ttl_seconds.saturating_mul(1000))
        .ok_or_else(|| usage("--ttl-seconds", "invite expiry overflow"))?;
    let invite = owner
        .invite(
            namespace_id,
            recipe,
            profile_bytes,
            u8::from(stage),
            toolchain,
            environment,
            target_platform,
            expiry,
        )
        .map_err(|error| owner_fault(&owner_path, error))?;
    if invite.execution_class() != ClusterExecutionClass::TrustedCoordinatorHostExecution {
        return Err(usage(
            "cluster invite",
            "only trusted host execution is supported",
        ));
    }
    let token = invite
        .encode_token()
        .map_err(|_| usage("cluster invite", "invite claims could not be encoded"))?;
    let grant = TrustedCompilerWorkerGrant::new(
        peer,
        address,
        namespace_id,
        recipe,
        profile,
        stage,
        toolchain,
        environment,
        target_platform,
    )
    .map_err(|error| trust_fault(&owner_path, error))?;
    let trust_path = trust_path(options)?;
    let mut policy = TrustedCompilerWorkerPolicy::load(&trust_path)
        .map_err(|error| trust_fault(&trust_path, error))?;
    policy
        .add(grant)
        .map_err(|error| trust_fault(&trust_path, error))?;
    policy
        .save_atomic(&trust_path)
        .map_err(|error| trust_fault(&trust_path, error))?;
    Ok(format!(
        "Worker {} is trusted for the exact execution scope until Unix millisecond {expiry}.\nImport this one-time token on the worker: {token}\nFingerprint: {}\n",
        hex(peer.as_bytes()),
        invite.fingerprint_hex(),
    ))
}

fn list(rest: &[String], options: &Options) -> Result<String, Fault> {
    if !rest.is_empty() {
        return Err(usage("cluster trust list", "takes no command options"));
    }
    let path = policy_path(options)?;
    let policy =
        TrustedCompilerWorkerPolicy::load(&path).map_err(|error| trust_fault(&path, error))?;
    match options.format() {
        Format::Json => serde_json::to_string_pretty(
            &policy.grants().iter().map(grant_json).collect::<Vec<_>>(),
        )
        .map(|mut value| {
            value.push('\n');
            value
        })
        .map_err(|error| storage_fault(&path, error.to_string())),
        Format::Human | Format::Markdown => {
            if policy.grants().is_empty() {
                return Ok("No trusted compiler peers are registered.\n".to_owned());
            }
            let mut output = String::new();
            for grant in policy.grants() {
                output.push_str(&format!(
                    "peer {} at {} — namespace {} recipe {} profile {} stage {} toolchain {} environment {} target-platform {}\n",
                    hex(grant.peer().as_bytes()),
                    grant.address(),
                    hex(&grant.namespace_id()),
                    hex(&grant.recipe()),
                    hex(&<[u8; 2]>::from(grant.profile())),
                    stage_name(grant.stage()),
                    hex(&grant.toolchain()),
                    hex(&grant.environment()),
                    hex(&grant.target_platform()),
                ));
            }
            Ok(output)
        }
    }
}

fn add(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags(
        rest,
        &[
            "peer",
            "address",
            "namespace",
            "recipe",
            "profile",
            "stage",
            "toolchain",
            "environment",
            "target-platform",
        ],
    )?;
    let peer = endpoint_id(required(&flags, "peer")?)?;
    let address = required(&flags, "address")?
        .parse::<SocketAddr>()
        .map_err(|_| usage("--address", "use a direct IP address and nonzero port"))?;
    let namespace_id = fixed_hex::<16>(required(&flags, "namespace")?)?;
    let recipe = fixed_hex::<32>(required(&flags, "recipe")?)?;
    let profile_bytes = fixed_hex::<2>(required(&flags, "profile")?)?;
    let profile = LanguageProfile::try_from(profile_bytes)
        .map_err(|_| usage("--profile", "use a canonical two-byte profile code"))?;
    let stage = parse_stage(required(&flags, "stage")?)?;
    let toolchain = fixed_hex::<32>(required(&flags, "toolchain")?)?;
    let environment = fixed_hex::<32>(required(&flags, "environment")?)?;
    let target_platform = fixed_hex::<32>(required(&flags, "target-platform")?)?;
    let path = policy_path(options)?;
    let grant = TrustedCompilerWorkerGrant::new(
        peer,
        address,
        namespace_id,
        recipe,
        profile,
        stage,
        toolchain,
        environment,
        target_platform,
    )
    .map_err(|error| trust_fault(&path, error))?;
    let mut policy =
        TrustedCompilerWorkerPolicy::load(&path).map_err(|error| trust_fault(&path, error))?;
    policy
        .add(grant)
        .map_err(|error| trust_fault(&path, error))?;
    policy
        .save_atomic(&path)
        .map_err(|error| trust_fault(&path, error))?;
    Ok(format!(
        "Registered compiler peer {} for the exact execution scope.\n",
        hex(peer.as_bytes())
    ))
}

fn revoke(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags(rest, &["peer"])?;
    let peer = endpoint_id(required(&flags, "peer")?)?;
    let path = policy_path(options)?;
    let mut policy =
        TrustedCompilerWorkerPolicy::load(&path).map_err(|error| trust_fault(&path, error))?;
    let removed = policy
        .revoke_peer(peer)
        .map_err(|error| trust_fault(&path, error))?;
    policy
        .save_atomic(&path)
        .map_err(|error| trust_fault(&path, error))?;
    Ok(format!(
        "Revoked compiler peer {}; removed {removed} execution grant(s).\n",
        hex(peer.as_bytes())
    ))
}

fn policy_path(options: &Options) -> Result<PathBuf, Fault> {
    let paths = backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map_err(|error| {
        Fault::new(
            FaultSlug::Endpoint,
            Operand::Path(options.workspace().or(options.project()).map_or_else(
                || ".".to_owned(),
                |path| path.to_string_lossy().into_owned(),
            )),
            Cause::new(CauseSlug::Unreachable, error.to_string()),
            Affordance::None,
        )
    })?;
    Ok(paths.data().join(TRUST_FILE))
}

fn owner_path(options: &Options) -> Result<PathBuf, Fault> {
    let paths = backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map_err(|error| workspace_fault(options, error.to_string()))?;
    Ok(paths.data().join(OWNER_FILE))
}

fn trust_path(options: &Options) -> Result<PathBuf, Fault> {
    policy_path(options)
}

fn compiler_data_root(options: &Options) -> Result<PathBuf, Fault> {
    backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map(|paths| paths.data().to_path_buf())
    .map_err(|error| workspace_fault(options, error.to_string()))
}

fn workspace_fault(options: &Options, message: String) -> Fault {
    Fault::new(
        FaultSlug::Endpoint,
        Operand::Path(options.workspace().or(options.project()).map_or_else(
            || ".".to_owned(),
            |path| path.to_string_lossy().into_owned(),
        )),
        Cause::new(CauseSlug::Unreachable, message),
        Affordance::None,
    )
}

fn owner_fault(path: &PathBuf, error: ClusterOwnerConfigError) -> Fault {
    storage_fault(path, error.to_string())
}

fn compiler_scope_fault(package: &str, message: &str) -> Fault {
    Fault::new(
        FaultSlug::Rejected,
        Operand::Text(package.to_owned()),
        Cause::new(CauseSlug::Unproven, message.to_owned()),
        Affordance::None,
    )
}

fn compiler_capability_fault(profile: &str, message: &str) -> Fault {
    Fault::new(
        FaultSlug::LaneUnavailable,
        Operand::Argument(format!("compiler capability for {profile}")),
        Cause::new(CauseSlug::Unconfigured, message.to_owned()),
        Affordance::None,
    )
}

fn parse_flags<'value>(
    words: &'value [String],
    allowed: &[&str],
) -> Result<BTreeMap<&'value str, &'value str>, Fault> {
    parse_flags_with_required(words, allowed, allowed)
}

fn parse_flags_with_required<'value>(
    words: &'value [String],
    allowed: &[&str],
    required: &[&str],
) -> Result<BTreeMap<&'value str, &'value str>, Fault> {
    let mut flags = BTreeMap::new();
    let mut at = 0;
    while at < words.len() {
        let name = words[at]
            .strip_prefix("--")
            .ok_or_else(|| usage("cluster trust", "all command fields use --name VALUE"))?;
        if !allowed.contains(&name) {
            return Err(usage(format!("--{name}"), "unknown cluster trust field"));
        }
        at += 1;
        let value = words
            .get(at)
            .ok_or_else(|| usage(format!("--{name}"), "a value is required"))?;
        if value.starts_with("--") || flags.insert(name, value.as_str()).is_some() {
            return Err(usage(
                format!("--{name}"),
                "field is repeated or has no value",
            ));
        }
        at += 1;
    }
    if required.iter().any(|name| !flags.contains_key(name)) {
        return Err(usage("cluster trust", "every listed field is required"));
    }
    Ok(flags)
}

fn required<'value>(
    flags: &'value BTreeMap<&str, &'value str>,
    name: &str,
) -> Result<&'value str, Fault> {
    flags
        .get(name)
        .copied()
        .ok_or_else(|| usage(format!("--{name}"), "a value is required"))
}

fn endpoint_id(value: &str) -> Result<EndpointId, Fault> {
    let bytes = fixed_hex::<32>(value)?;
    EndpointId::from_bytes(&bytes).map_err(|_| usage("--peer", "peer identity is invalid"))
}

fn socket_address(value: &str, operand: &str) -> Result<SocketAddr, Fault> {
    value
        .parse::<SocketAddr>()
        .map_err(|_| usage(operand, "use a direct IP address and nonzero port"))
}

fn fixed_hex<const N: usize>(value: &str) -> Result<[u8; N], Fault> {
    if value.len() != N.saturating_mul(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(usage(
            "cluster trust",
            format!("expected exactly {} hexadecimal bytes", N),
        ));
    }
    let mut bytes = [0; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        let offset = index.saturating_mul(2);
        let high = value.as_bytes()[offset];
        let low = value.as_bytes()[offset + 1];
        *byte = (hex_nibble(high)? << 4) | hex_nibble(low)?;
    }
    Ok(bytes)
}

fn hex_nibble(byte: u8) -> Result<u8, Fault> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(usage("cluster trust", "identity is not hexadecimal")),
    }
}

fn parse_stage(value: &str) -> Result<Stage, Fault> {
    match value {
        "parse" => Ok(Stage::Parse),
        "lower-ir" => Ok(Stage::LowerIr),
        _ => Err(usage("--stage", "choose parse or lower-ir")),
    }
}

const fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::Parse => "parse",
        Stage::LowerIr => "lower-ir",
    }
}

fn grant_json(grant: &TrustedCompilerWorkerGrant) -> serde_json::Value {
    serde_json::json!({
        "peer": hex(grant.peer().as_bytes()),
        "address": grant.address().to_string(),
        "namespace": hex(&grant.namespace_id()),
        "recipe": hex(&grant.recipe()),
        "profile": hex(&<[u8; 2]>::from(grant.profile())),
        "stage": stage_name(grant.stage()),
        "toolchain": hex(&grant.toolchain()),
        "environment": hex(&grant.environment()),
        "target_platform": hex(&grant.target_platform()),
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn trust_fault(path: &std::path::Path, error: CompilerTrustError) -> Fault {
    Fault::new(
        FaultSlug::Rejected,
        Operand::Path(path.to_string_lossy().into_owned()),
        Cause::new(CauseSlug::Refused, error.to_string()),
        Affordance::None,
    )
}

fn storage_fault(path: &std::path::Path, error: String) -> Fault {
    Fault::new(
        FaultSlug::Endpoint,
        Operand::Path(path.to_string_lossy().into_owned()),
        Cause::new(CauseSlug::Unreachable, error),
        Affordance::None,
    )
}

fn usage(operand: impl Into<String>, message: impl Into<String>) -> Fault {
    Fault::usage(operand, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::application::{CompilerInvocationRecipeV2, LocalRuntimeToolchain};
    use backend_engine::cluster_transport::{ClusterExecutionClass, SecretKey};
    use backend_local_service::compiler_trust::TrustedCompilerWorkerPolicy;
    use backend_semantic::vocabulary::{GoVersion, NativeTool, RustEdition};

    #[test]
    fn reported_scope_builds_worker_invite_and_wrong_environment_is_denied() {
        let package = PackageReference::parse("pkg:cargo/widget@1.2.3").expect("pinned package");
        let coordinate = PackageUrl::parse("pkg:cargo/widget@1.2.3".to_owned())
            .expect("pinned compiler coordinate");
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let scope = product_compiler_scope(package.clone(), profile, Some(&coordinate))
            .expect("production compiler scope");
        assert_eq!(
            scope.target_kind(),
            ProductCompilerTargetKind::PinnedRegistry
        );

        let runtime_toolchain = LocalRuntimeToolchain::resolved(
            NativeTool::Rustc,
            PathBuf::from("/test/toolchain/rustc"),
            b"rustc 1.0.0 test",
        )
        .expect("resolved compiler toolchain fixture");
        let invocation = CompilerInvocationRecipeV2::new(
            profile,
            Stage::LowerIr,
            NativeTool::Rustc,
            runtime_toolchain.identity.expect("toolchain identity"),
            [4; 32],
            [5; 32],
            [6; 32],
        )
        .expect("canonical compiler invocation");
        let report = ClusterScopeReport::from_invocation(
            package.as_str(),
            "rust-2024",
            &scope,
            profile,
            invocation,
        )
        .json();

        let namespace = fixed_hex::<16>(report["namespace"].as_str().expect("namespace hex"))
            .expect("namespace bytes");
        let recipe =
            fixed_hex::<32>(report["recipe"].as_str().expect("recipe hex")).expect("recipe bytes");
        let profile_bytes = fixed_hex::<2>(report["profile"].as_str().expect("profile hex"))
            .expect("profile bytes");
        let toolchain = fixed_hex::<32>(report["toolchain"].as_str().expect("toolchain hex"))
            .expect("toolchain bytes");
        let environment = fixed_hex::<32>(report["environment"].as_str().expect("environment hex"))
            .expect("environment bytes");
        let target_platform = fixed_hex::<32>(
            report["target_platform"]
                .as_str()
                .expect("target platform hex"),
        )
        .expect("target platform bytes");
        let coordinator_key = SecretKey::from_bytes(&[8; 32]);
        let coordinator_address = "127.0.0.1:4411".parse().expect("coordinator address");
        let invite = ScopedClusterInvite::new(
            coordinator_key.public(),
            coordinator_address,
            namespace,
            recipe,
            profile_bytes,
            u8::from(Stage::LowerIr),
            toolchain,
            environment,
            target_platform,
            1_800_000_000_000,
            ClusterExecutionClass::TrustedCoordinatorHostExecution,
        )
        .expect("worker invitation from reported tuple");
        let invite =
            ScopedClusterInvite::decode_token(&invite.encode_token().expect("encode invite"))
                .expect("decode invite");
        let worker = SecretKey::from_bytes(&[9; 32]).public();
        let worker_address = "127.0.0.1:4412".parse().expect("worker address");
        let grant = TrustedCompilerWorkerGrant::new(
            worker,
            worker_address,
            namespace,
            recipe,
            profile,
            Stage::LowerIr,
            toolchain,
            environment,
            target_platform,
        )
        .expect("worker grant from reported tuple");
        let mut policy = TrustedCompilerWorkerPolicy::default();
        policy.add(grant).expect("admit worker grant");
        assert!(policy.authorizes(
            worker,
            invite.namespace_id(),
            invite.recipe(),
            profile,
            Stage::LowerIr,
            invite.toolchain(),
            invite.environment(),
            invite.target_platform(),
        ));

        let mut wrong_environment = invite.environment();
        wrong_environment[0] ^= 1;
        assert!(!policy.authorizes(
            worker,
            invite.namespace_id(),
            invite.recipe(),
            profile,
            Stage::LowerIr,
            invite.toolchain(),
            wrong_environment,
            invite.target_platform(),
        ));
    }

    #[test]
    fn go_workspace_scope_is_reported_local_only_until_transferable() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-cluster-go-scope-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create package root");
        std::fs::write(root.join("go.work"), b"go 1.25\nuse ./module\n")
            .expect("write selected Go workspace");

        let placement = package_placement(
            LanguageProfile::Go(GoVersion::Go125),
            Some(root.to_str().expect("UTF-8 temp path")),
        )
        .expect("inspect Go workspace placement");
        assert_eq!(placement.eligibility, "local-only");
        assert_eq!(placement.placement, "local-only");
        assert_eq!(placement.reason, Some("go.workspace.local.v1"));

        let requires_project = package_placement(LanguageProfile::Go(GoVersion::Go125), None)
            .expect("scope without package root remains unclassified");
        assert_eq!(requires_project.eligibility, "requires-project-inspection");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn go_local_replace_scope_is_reported_local_only() {
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let parent = std::env::temp_dir().join(format!(
            "backend-cluster-go-replace-{}-{sequence}",
            std::process::id()
        ));
        let root = parent.join("consumer");
        let replacement = parent.join("shared");
        std::fs::create_dir_all(&root).expect("create consumer module");
        std::fs::create_dir_all(&replacement).expect("create replacement module");
        std::fs::write(
            root.join("go.mod"),
            b"module example.test/consumer\ngo 1.24\nreplace example.test/shared => ../shared\n",
        )
        .expect("write consumer manifest");
        std::fs::write(
            replacement.join("go.mod"),
            b"module example.test/shared\ngo 1.24\n",
        )
        .expect("write replacement manifest");
        std::fs::write(replacement.join("shared.go"), b"package shared\n")
            .expect("write replacement source");

        let placement = package_placement(
            LanguageProfile::Go(GoVersion::Go125),
            Some(root.to_str().expect("UTF-8 package path")),
        )
        .expect("inspect local replacement placement");
        assert_eq!(placement.eligibility, "local-only");
        assert_eq!(placement.placement, "local-only");
        assert_eq!(placement.reason, Some("go.module_replace.local.v1"));
        let _ = std::fs::remove_dir_all(parent);
    }
}
