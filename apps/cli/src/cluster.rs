//! Owner-side commands for the local compiler peer allowlist.

use crate::options::{Format, Options};
use backend_engine::cluster_transport::{ClusterExecutionClass, EndpointId, ScopedClusterInvite};
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
        [help] if help == "help" => Ok(help_text().to_owned()),
        [] => Ok(help_text().to_owned()),
        _ => Err(usage(
            "cluster",
            "use `cluster owner init|show`, `cluster trust list|add|revoke`, or `cluster invite create`",
        )),
    }
}

pub(super) const fn help_text() -> &'static str {
    "backend cluster — local compiler owner and explicit worker trust\n\n\
     Usage: backend [OPTIONS] cluster owner init --bind IP:PORT --advertise IP:PORT\n\
            backend [OPTIONS] cluster owner show\n\
            backend [OPTIONS] cluster invite create --worker-peer HEX --worker-address IP:PORT --namespace HEX --recipe HEX --profile HEX --stage lower-ir --toolchain HEX --environment HEX --target-platform HEX [--ttl-seconds N]\n\
            backend [OPTIONS] cluster trust list\n\
            backend [OPTIONS] cluster trust add --peer HEX --address IP:PORT --namespace HEX --recipe HEX --profile HEX --stage parse|lower-ir --toolchain HEX --environment HEX --target-platform HEX\n\
            backend [OPTIONS] cluster trust revoke --peer HEX\n\n\
     Owner identity is stored owner-only under the selected locald data root in cluster-owner.v1.\n\
     Inviting a worker atomically records its exact peer and execution grant in compiler-worker-trust.v1 and prints the one-time import token.\n\
     Use --workspace PATH to select an isolated private data root; by default each project has a separate root under per-user app data.\n\
     The authenticated peer identity and every execution-scope field must match exactly."
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
