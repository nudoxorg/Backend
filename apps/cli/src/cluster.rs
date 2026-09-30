//! Owner-side commands for the local compiler peer allowlist.

use crate::options::{Format, Options};
use backend_client::{LocalSemanticIndexClient, RemoteIndexCommandTransport, Session};
use backend_engine::application::{
    CompilerPackageTargetV2, GoPackageAuthorityWitness, LocalCompilerCapabilityState,
    LocalCompilerExecutionIdentity, LocalCompilerHost,
};
use backend_engine::cluster_transport::{
    ClusterExecutionClass, EndpointId, RemoteIndexCapability, RemoteIndexCapabilityClaims,
    RemoteIndexPermission, RemoteIndexProductScope, RemoteIndexQueryOperation,
    RemoteIndexSemanticSelection, ScopedClusterInvite, SecretKey, remote_index_now,
};
use backend_engine::{
    IndexSearchCursor, PackageReference, ProductText, SurfaceCommand, SurfaceReply,
};
use backend_library::interface::PackageUrl;
use backend_local_service::builtin::{
    ProductCompilerScope, ProductCompilerTargetKind, RemoteIndexUsage, product_compiler_scope,
};
use backend_local_service::cluster_owner::{ClusterOwnerConfig, ClusterOwnerConfigError};
use backend_local_service::compiler_trust::{
    CompilerTrustError, TrustedCompilerWorkerGrant, TrustedCompilerWorkerPolicy,
};
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
use backend_replication::SemanticTargetKey;
use backend_semantic::vocabulary::{LanguageProfile, Stage};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

const TRUST_FILE: &str = "compiler-worker-trust.v1";
const OWNER_FILE: &str = "cluster-owner.v1";
const REMOTE_CLIENT_FILE: &str = "remote-index-client.v1";
const REMOTE_CLIENT_MAGIC: &[u8; 8] = b"BKRICL01";
const REMOTE_CAPABILITY_MAGIC: &[u8; 8] = b"BKRICP01";

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
            "grant" => match rest {
                [action, args @ ..] if action == "list" => owner_grant_list(args, options),
                [action, args @ ..] if action == "revoke" => owner_grant_revoke(args, options),
                [kind, action, args @ ..] if action == "create" => match kind.as_str() {
                    "product" => owner_grant_product(args, options),
                    "semantic" => owner_grant_semantic(args, options),
                    _ => Err(usage("cluster owner grant", "choose product or semantic")),
                },
                _ => Err(usage(
                    "cluster owner grant",
                    "use product|semantic create, list, or revoke",
                )),
            },
            _ => Err(usage("cluster owner", "choose init or show")),
        },
        [client, action, rest @ ..] if client == "client" => match action.as_str() {
            "init" => client_init(rest, options),
            "connect" => client_connect(rest, options),
            "query" => client_query(rest, options),
            "semantic-catalog" => client_semantic_catalog(rest, options),
            _ => Err(usage(
                "cluster client",
                "choose init, connect, query, or semantic-catalog",
            )),
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
            "use `cluster owner init|show|grant`, `cluster client init|connect|query|semantic-catalog`, `cluster scope show`, `cluster trust list|add|revoke`, or `cluster invite create`",
        )),
    }
}

pub(super) const fn help_text() -> &'static str {
    "backend cluster — local compiler owner and explicit worker trust\n\n\
     Usage: backend [OPTIONS] cluster owner init --bind IP:PORT --advertise IP:PORT\n\
            backend [OPTIONS] cluster owner show\n\
            backend [OPTIONS] cluster owner grant list\n\
            backend [OPTIONS] cluster owner grant revoke --grant-id HEX\n\
            backend [OPTIONS] cluster owner grant product create --client-peer HEX --capability-file PATH [--operations search,names,index-search,...]\n\
            backend [OPTIONS] cluster owner grant semantic create --client-peer HEX --capability-file PATH --package PACKAGE --coordinate PKGURL --profile PROFILE\n\
            backend [OPTIONS] cluster client init [--key-file PATH]\n\
            backend [OPTIONS] cluster client connect [--key-file PATH] --owner-peer HEX --owner-address IP:PORT --capability-file PATH\n\
            backend [OPTIONS] cluster client query [--key-file PATH] --owner-peer HEX --owner-address IP:PORT --capability-file PATH --operation search|names|document|source|outline|graph|related|index-search --value TEXT [--limit N] [--cursor TOKEN]\n\
            backend [OPTIONS] cluster client semantic-catalog --key-file PATH --owner-peer HEX --owner-address IP:PORT --capability-file PATH\n\
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

fn owner_grant_product(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &[
            "client-peer",
            "capability-file",
            "operations",
            "ttl-seconds",
            "request-budget",
            "byte-budget",
        ],
        &["client-peer", "capability-file"],
    )?;
    let owner_path = owner_path(options)?;
    let owner =
        ClusterOwnerConfig::load(&owner_path).map_err(|error| owner_fault(&owner_path, error))?;
    let client = endpoint_id(required(&flags, "client-peer")?)?;
    let paths = workspace_paths(options)?;
    let mut product_session = Session::connect(paths.endpoint()).map_err(client_fault)?;
    let revision = product_session.revision().map_err(client_fault)?;
    let operations = parse_product_operations(flags.get("operations").copied())?;
    let index_search_snapshot = if operations.contains(&RemoteIndexQueryOperation::IndexSearch) {
        let page = product_session
            .surface(SurfaceCommand::IndexSearch {
                query: ProductText::from_static("__remote-index-capability-snapshot__"),
                limit: 1,
                cursor: None,
            })
            .map_err(client_fault)?;
        let SurfaceReply::IndexSearchPage(page) = page else {
            return Err(usage(
                "cluster owner grant product",
                "local service did not return the typed index-search snapshot",
            ));
        };
        let confirmed_page = product_session
            .surface(SurfaceCommand::IndexSearch {
                query: ProductText::from_static("__remote-index-capability-snapshot__"),
                limit: 1,
                cursor: None,
            })
            .map_err(client_fault)?;
        let SurfaceReply::IndexSearchPage(confirmed_page) = confirmed_page else {
            return Err(usage(
                "cluster owner grant product",
                "local service did not confirm the typed index-search snapshot",
            ));
        };
        let confirmed_revision = product_session.revision().map_err(client_fault)?;
        if confirmed_revision.root != revision.root {
            return Err(usage(
                "cluster owner grant product",
                "product root changed while capturing the search snapshot; retry the grant",
            ));
        }
        if confirmed_page.snapshot != page.snapshot {
            return Err(usage(
                "cluster owner grant product",
                "index-search snapshot changed while capturing the grant; retry the grant",
            ));
        }
        Some(page.snapshot)
    } else {
        None
    };
    let (issued_at, expires_at, request_budget, byte_budget) = capability_limits(&flags)?;
    let claims = RemoteIndexCapabilityClaims {
        version: 2,
        server: owner.endpoint_id(),
        client,
        grant_id: fresh_grant_id(),
        issued_at_unix_ms: issued_at,
        expires_at_unix_ms: expires_at,
        request_budget,
        byte_budget,
        permissions: vec![RemoteIndexPermission::ProductRead],
        product: Some(RemoteIndexProductScope {
            view_root: *revision.root.as_bytes(),
            operations: operations.clone(),
            index_search_snapshot,
        }),
        semantic: None,
    };
    let capability = owner
        .issue_remote_index_capability(claims, issued_at)
        .map_err(|error| storage_fault(&owner_path, error.to_string()))?;
    let usage_path = remote_usage_path(options)?;
    let usage = RemoteIndexUsage::open(&usage_path, owner.endpoint_id())
        .map_err(|error| storage_fault(&usage_path, error.to_string()))?;
    usage
        .register_capability(&capability, issued_at)
        .map_err(|error| storage_fault(&usage_path, error.to_string()))?;
    let path = PathBuf::from(required(&flags, "capability-file")?);
    if let Err(error) = write_remote_capability(&path, &capability) {
        let _ = usage.revoke(capability.grant_id());
        return Err(error);
    }
    Ok(format!(
        "Issued product read grant {} for client {} at view root {}.\nAllowed operations: {}\nIndex-search snapshot: {}\nCapability file: {}\nExpires at Unix millisecond {}.\n",
        hex(&capability.grant_id()),
        hex(client.as_bytes()),
        hex(&revision.root.as_bytes()[..]),
        operations
            .iter()
            .map(product_operation_name)
            .collect::<Vec<_>>()
            .join(", "),
        index_search_snapshot.map_or_else(|| "not granted".to_owned(), |snapshot| hex(&snapshot)),
        path.display(),
        expires_at,
    ))
}

fn owner_grant_semantic(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &[
            "client-peer",
            "capability-file",
            "package",
            "coordinate",
            "profile",
            "ttl-seconds",
            "request-budget",
            "byte-budget",
        ],
        &[
            "client-peer",
            "capability-file",
            "package",
            "coordinate",
            "profile",
        ],
    )?;
    let profile_text = required(&flags, "profile")?;
    let profile = LanguageProfile::try_from(profile_text)
        .map_err(|_| usage("--profile", "use a canonical language profile spelling"))?;
    let target = SemanticTargetKey::new(
        required(&flags, "package")?.to_owned(),
        required(&flags, "coordinate")?.to_owned(),
        profile,
    )
    .map_err(|error| usage("cluster owner grant semantic", error.to_string()))?;
    let paths = workspace_paths(options)?;
    let mut semantic = LocalSemanticIndexClient::connect(paths.endpoint(), target.clone())
        .map_err(client_fault)?;
    let snapshot = semantic.fetch_selected_catalog().map_err(client_fault)?;
    let stamp = snapshot.selected_stamp();
    let owner_path = owner_path(options)?;
    let owner =
        ClusterOwnerConfig::load(&owner_path).map_err(|error| owner_fault(&owner_path, error))?;
    let client = endpoint_id(required(&flags, "client-peer")?)?;
    let (issued_at, expires_at, request_budget, byte_budget) = capability_limits(&flags)?;
    let claims = RemoteIndexCapabilityClaims {
        version: 2,
        server: owner.endpoint_id(),
        client,
        grant_id: fresh_grant_id(),
        issued_at_unix_ms: issued_at,
        expires_at_unix_ms: expires_at,
        request_budget,
        byte_budget,
        permissions: vec![RemoteIndexPermission::SemanticHydration],
        product: None,
        semantic: Some(RemoteIndexSemanticSelection {
            package: target.package().to_owned(),
            coordinate: target.coordinate().to_owned(),
            profile: <[u8; 2]>::from(target.profile()),
            namespace: *stamp.namespace(),
            source_coordinate: *stamp.source_coordinate(),
            selection_revision: stamp.selection_revision(),
            selected_root: *stamp.selected_root(),
            closure_id: *stamp.closure_id(),
            catalog_root: *stamp.catalog_root().as_bytes(),
        }),
    };
    let capability = owner
        .issue_remote_index_capability(claims, issued_at)
        .map_err(|error| storage_fault(&owner_path, error.to_string()))?;
    let usage_path = remote_usage_path(options)?;
    let usage = RemoteIndexUsage::open(&usage_path, owner.endpoint_id())
        .map_err(|error| storage_fault(&usage_path, error.to_string()))?;
    usage
        .register_capability(&capability, issued_at)
        .map_err(|error| storage_fault(&usage_path, error.to_string()))?;
    let path = PathBuf::from(required(&flags, "capability-file")?);
    if let Err(error) = write_remote_capability(&path, &capability) {
        let _ = usage.revoke(capability.grant_id());
        return Err(error);
    }
    Ok(format!(
        "Issued semantic hydration grant {} for client {}.\nTarget: {} at {} (profile {}).\nSelected revision: {}; root {}; catalog root {}.\nCapability file: {}\nExpires at Unix millisecond {}.\n",
        hex(&capability.grant_id()),
        hex(client.as_bytes()),
        target.package(),
        target.coordinate(),
        profile_text,
        stamp.selection_revision(),
        hex(stamp.selected_root()),
        hex(stamp.catalog_root().as_bytes()),
        path.display(),
        expires_at,
    ))
}

fn owner_grant_list(rest: &[String], options: &Options) -> Result<String, Fault> {
    if !rest.is_empty() {
        return Err(usage(
            "cluster owner grant list",
            "takes no command options",
        ));
    }
    let owner_path = owner_path(options)?;
    let owner =
        ClusterOwnerConfig::load(&owner_path).map_err(|error| owner_fault(&owner_path, error))?;
    let path = remote_usage_path(options)?;
    let grants = RemoteIndexUsage::open(&path, owner.endpoint_id())
        .and_then(|usage| usage.list())
        .map_err(|error| storage_fault(&path, error.to_string()))?;
    match options.format() {
        Format::Json => {
            let values = grants
                .iter()
                .map(|grant| {
                    let product = grant.product.as_ref().map(|scope| {
                        serde_json::json!({
                            "viewRoot": hex(&scope.view_root),
                            "operations": scope.operations.iter().map(product_operation_name).collect::<Vec<_>>(),
                            "indexSearchSnapshot": scope.index_search_snapshot.map(|snapshot| hex(&snapshot)),
                        })
                    });
                    let semantic = grant.semantic.as_ref().map(|scope| {
                        serde_json::json!({
                            "package": scope.package,
                            "coordinate": scope.coordinate,
                            "profile": hex(&scope.profile),
                            "selectionRevision": scope.selection_revision,
                            "selectedRoot": hex(&scope.selected_root),
                            "catalogRoot": hex(&scope.catalog_root),
                        })
                    });
                    serde_json::json!({
                        "grantId": hex(&grant.grant_id),
                        "clientPeer": hex(grant.client.as_bytes()),
                        "expiresAtUnixMs": grant.expires_at_unix_ms,
                        "revoked": grant.revoked,
                        "requests": grant.requests,
                        "requestBudget": grant.request_budget,
                        "responseBytes": grant.response_bytes,
                        "byteBudget": grant.byte_budget,
                        "product": product,
                        "semantic": semantic,
                    })
                })
                .collect::<Vec<_>>();
            serde_json::to_string_pretty(&values)
                .map(|mut value| {
                    value.push('\n');
                    value
                })
                .map_err(|error| storage_fault(&path, error.to_string()))
        }
        Format::Human | Format::Markdown => {
            if grants.is_empty() {
                return Ok("No active remote index grants.\n".to_owned());
            }
            let mut output = String::new();
            for grant in grants {
                let state = if grant.revoked { "revoked" } else { "active" };
                output.push_str(&format!(
                    "grant {} — {} — client {} — expires {} — requests {}/{} bytes {}/{}\n",
                    hex(&grant.grant_id),
                    state,
                    hex(grant.client.as_bytes()),
                    grant.expires_at_unix_ms,
                    grant.requests,
                    grant.request_budget,
                    grant.response_bytes,
                    grant.byte_budget,
                ));
                if let Some(scope) = grant.product {
                    output.push_str(&format!(
                        "  product root {}; operations {}; index-search snapshot {}\n",
                        hex(&scope.view_root),
                        scope
                            .operations
                            .iter()
                            .map(product_operation_name)
                            .collect::<Vec<_>>()
                            .join(", "),
                        scope
                            .index_search_snapshot
                            .map_or_else(|| "not granted".to_owned(), |snapshot| hex(&snapshot)),
                    ));
                }
                if let Some(scope) = grant.semantic {
                    output.push_str(&format!(
                        "  semantic target {} at {} profile {}; selection {} root {} catalog {}\n",
                        scope.package,
                        scope.coordinate,
                        hex(&scope.profile),
                        scope.selection_revision,
                        hex(&scope.selected_root),
                        hex(&scope.catalog_root),
                    ));
                }
            }
            Ok(output)
        }
    }
}

fn owner_grant_revoke(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags(rest, &["grant-id"])?;
    let grant_id = fixed_hex::<16>(required(&flags, "grant-id")?)?;
    let owner_path = owner_path(options)?;
    let owner =
        ClusterOwnerConfig::load(&owner_path).map_err(|error| owner_fault(&owner_path, error))?;
    let path = remote_usage_path(options)?;
    let usage = RemoteIndexUsage::open(&path, owner.endpoint_id())
        .map_err(|error| storage_fault(&path, error.to_string()))?;
    let changed = usage
        .revoke(grant_id)
        .map_err(|error| storage_fault(&path, error.to_string()))?;
    Ok(if changed {
        format!(
            "Revoked remote index grant {}. New requests and unadmitted results are blocked; an earlier admitted response may finish.\n",
            hex(&grant_id)
        )
    } else {
        format!(
            "Remote index grant {} was already revoked.\n",
            hex(&grant_id)
        )
    })
}

fn client_init(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(rest, &["key-file"], &[])?;
    let path = if let Some(path) = flags.get("key-file") {
        PathBuf::from(*path)
    } else {
        let paths = workspace_paths(options)?;
        paths
            .initialize_data_directory()
            .map_err(|error| workspace_fault(options, error.to_string()))?;
        paths.data().join(REMOTE_CLIENT_FILE)
    };
    let secret = SecretKey::generate();
    let mut bytes = Vec::with_capacity(REMOTE_CLIENT_MAGIC.len() + 32);
    bytes.extend_from_slice(REMOTE_CLIENT_MAGIC);
    bytes.extend_from_slice(&secret.to_bytes());
    write_private_new(&path, &bytes)?;
    Ok(format!(
        "Created private remote-index client identity {}.\nKey file: {}\nThe private key stays in this file and is not printed.\n",
        hex(secret.public().as_bytes()),
        path.display(),
    ))
}

fn client_connect(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &["key-file", "owner-peer", "owner-address", "capability-file"],
        &["owner-peer", "owner-address", "capability-file"],
    )?;
    let key_path = flags
        .get("key-file")
        .map_or_else(|| client_key_path(options), |path| Ok(PathBuf::from(*path)))?;
    let secret = load_client_secret(&key_path)?;
    let capability_path = PathBuf::from(required(&flags, "capability-file")?);
    let capability = load_remote_capability(&capability_path)?;
    let owner = endpoint_id(required(&flags, "owner-peer")?)?;
    let address = socket_address(required(&flags, "owner-address")?, "--owner-address")?;
    capability
        .verify(
            owner,
            secret.public(),
            remote_index_now().map_err(|error| {
                usage(
                    "cluster client connect",
                    format!("system clock unavailable: {error}"),
                )
            })?,
        )
        .map_err(|error| usage("--capability-file", error.to_string()))?;
    if capability.claims.product.is_some() {
        let transport =
            RemoteIndexCommandTransport::connect(secret, owner, address, capability.clone())
                .map_err(client_fault)?;
        let mut session = Session::from_transport(
            PathBuf::from(format!("iroh://{}@{}", hex(owner.as_bytes()), address)),
            transport,
        );
        let revision = session.revision().map_err(client_fault)?;
        if capability
            .claims
            .product
            .as_ref()
            .is_some_and(|scope| scope.view_root != *revision.root.as_bytes())
        {
            return Err(usage(
                "--capability-file",
                "product capability became stale while connecting; issue a new grant",
            ));
        }
        let index_search_snapshot = if let Some(expected) = capability
            .claims
            .product
            .as_ref()
            .and_then(|scope| scope.index_search_snapshot)
        {
            let page = session
                .surface(SurfaceCommand::IndexSearch {
                    query: ProductText::from_static("__remote-index-capability-snapshot__"),
                    limit: 1,
                    cursor: None,
                })
                .map_err(client_fault)?;
            let SurfaceReply::IndexSearchPage(page) = page else {
                return Err(usage(
                    "--capability-file",
                    "remote service did not return the typed index-search snapshot",
                ));
            };
            if page.snapshot != expected {
                return Err(usage(
                    "--capability-file",
                    "index-search capability became stale while connecting; issue a new grant",
                ));
            }
            hex(&expected)
        } else {
            "not granted".to_owned()
        };
        return Ok(format!(
            "Connected to remote index {} at {}.\nAuthorized product root: {}\nRemote index-search snapshot: {}\nOperations: {}\nGrant: {}\n",
            hex(owner.as_bytes()),
            address,
            hex(&revision.root.as_bytes()[..]),
            index_search_snapshot,
            capability
                .claims
                .product
                .as_ref()
                .map_or_else(String::new, |scope| scope
                    .operations
                    .iter()
                    .map(product_operation_name)
                    .collect::<Vec<_>>()
                    .join(", ")),
            hex(&capability.grant_id()),
        ));
    }
    let scope = capability.claims.semantic.as_ref().ok_or_else(|| {
        usage(
            "--capability-file",
            "capability has no supported remote read scope",
        )
    })?;
    let profile = LanguageProfile::try_from(scope.profile)
        .map_err(|_| usage("--capability-file", "semantic profile is invalid"))?;
    let target = SemanticTargetKey::new(scope.package.clone(), scope.coordinate.clone(), profile)
        .map_err(|error| usage("--capability-file", error.to_string()))?;
    let mut semantic = LocalSemanticIndexClient::connect_remote(
        secret,
        owner,
        address,
        capability.clone(),
        target,
    )
    .map_err(client_fault)?;
    let snapshot = semantic.fetch_selected_catalog().map_err(client_fault)?;
    if snapshot.selected_stamp().selection_revision() != scope.selection_revision
        || snapshot.selected_root() != &scope.selected_root
        || snapshot.selected_stamp().namespace() != &scope.namespace
        || snapshot.selected_stamp().source_coordinate() != &scope.source_coordinate
        || snapshot.selected_stamp().closure_id() != &scope.closure_id
        || snapshot.selected_stamp().catalog_root().as_bytes() != &scope.catalog_root
    {
        return Err(usage(
            "--capability-file",
            "semantic selection changed; issue a new grant",
        ));
    }
    Ok(format!(
        "Connected to remote index {} at {}.\nAuthorized semantic target: {} at {} (profile {:?}).\nSelected revision: {}; root {}; catalog root {}.\nGrant: {}\n",
        hex(owner.as_bytes()),
        address,
        scope.package,
        scope.coordinate,
        profile,
        scope.selection_revision,
        hex(&scope.selected_root),
        hex(&scope.catalog_root),
        hex(&capability.grant_id()),
    ))
}

fn client_query(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &[
            "key-file",
            "owner-peer",
            "owner-address",
            "capability-file",
            "operation",
            "value",
            "limit",
            "cursor",
        ],
        &[
            "owner-peer",
            "owner-address",
            "capability-file",
            "operation",
            "value",
        ],
    )?;
    let key_path = flags
        .get("key-file")
        .map_or_else(|| client_key_path(options), |path| Ok(PathBuf::from(*path)))?;
    let secret = load_client_secret(&key_path)?;
    let capability_path = PathBuf::from(required(&flags, "capability-file")?);
    let capability = load_remote_capability(&capability_path)?;
    let owner = endpoint_id(required(&flags, "owner-peer")?)?;
    let address = socket_address(required(&flags, "owner-address")?, "--owner-address")?;
    let scope = capability.claims.product.as_ref().ok_or_else(|| {
        usage(
            "--capability-file",
            "product query needs a product read capability",
        )
    })?;
    let operation = parse_product_operation(required(&flags, "operation")?)?;
    if !scope.operations.contains(&operation) {
        return Err(usage(
            "--operation",
            "operation is not included in this signed capability",
        ));
    }
    let limit = flags
        .get("limit")
        .map(|value| {
            value
                .parse::<u16>()
                .map_err(|_| usage("--limit", "use an integer from 1 to 65535"))
        })
        .transpose()?
        .unwrap_or(20);
    if operation != RemoteIndexQueryOperation::IndexSearch && flags.contains_key("cursor") {
        return Err(usage(
            "--cursor",
            "continuation cursors are supported only for index-search",
        ));
    }
    let transport = RemoteIndexCommandTransport::connect(secret, owner, address, capability)
        .map_err(client_fault)?;
    let mut session = Session::from_transport(
        PathBuf::from(format!("iroh://{}@{}", hex(owner.as_bytes()), address)),
        transport,
    );
    let value = required(&flags, "value")?;
    let rendered = if operation == RemoteIndexQueryOperation::IndexSearch {
        let cursor = flags
            .get("cursor")
            .map(|value| {
                IndexSearchCursor::new((*value).to_owned())
                    .map_err(|error| usage("--cursor", error.to_string()))
            })
            .transpose()?;
        let query = ProductText::new(value.to_owned())
            .map_err(|error| usage("--value", error.to_string()))?;
        let reply = session
            .surface(SurfaceCommand::IndexSearch {
                query,
                limit,
                cursor,
            })
            .map_err(client_fault)?;
        serde_json::to_string_pretty(&reply)
    } else {
        let reply = match operation {
            RemoteIndexQueryOperation::Search => session.search(value, limit),
            RemoteIndexQueryOperation::Names => session.names(value, limit),
            RemoteIndexQueryOperation::Document => session.document(value),
            RemoteIndexQueryOperation::Source => session.source(value),
            RemoteIndexQueryOperation::Outline => session.outline(value),
            RemoteIndexQueryOperation::Graph => session.graph(value),
            RemoteIndexQueryOperation::Related => session.related(value),
            RemoteIndexQueryOperation::IndexSearch => unreachable!("handled above"),
        }
        .map_err(client_fault)?;
        serde_json::to_string_pretty(&reply)
    }
    .map(|mut value| {
        value.push('\n');
        value
    })
    .map_err(|error| storage_fault(&capability_path, error.to_string()))?;
    Ok(rendered)
}

fn client_semantic_catalog(rest: &[String], options: &Options) -> Result<String, Fault> {
    let flags = parse_flags_with_required(
        rest,
        &["key-file", "owner-peer", "owner-address", "capability-file"],
        &["owner-peer", "owner-address", "capability-file"],
    )?;
    let key_path = flags
        .get("key-file")
        .map_or_else(|| client_key_path(options), |path| Ok(PathBuf::from(*path)))?;
    let secret = load_client_secret(&key_path)?;
    let capability_path = PathBuf::from(required(&flags, "capability-file")?);
    let capability = load_remote_capability(&capability_path)?;
    let owner = endpoint_id(required(&flags, "owner-peer")?)?;
    let address = socket_address(required(&flags, "owner-address")?, "--owner-address")?;
    let scope = capability.claims.semantic.as_ref().ok_or_else(|| {
        usage(
            "--capability-file",
            "semantic catalog needs a semantic hydration capability",
        )
    })?;
    let profile = LanguageProfile::try_from(scope.profile)
        .map_err(|_| usage("--capability-file", "semantic profile is invalid"))?;
    let target = SemanticTargetKey::new(scope.package.clone(), scope.coordinate.clone(), profile)
        .map_err(|error| usage("--capability-file", error.to_string()))?;
    let mut semantic = LocalSemanticIndexClient::connect_remote(
        secret,
        owner,
        address,
        capability.clone(),
        target,
    )
    .map_err(client_fault)?;
    let snapshot = semantic.fetch_selected_catalog().map_err(client_fault)?;
    let stamp = snapshot.selected_stamp();
    let image_ordinals = snapshot
        .catalog()
        .entries()
        .iter()
        .map(|entry| entry.image().artifact_ordinal())
        .collect::<Vec<_>>();
    Ok(format!(
        "Remote semantic catalog admitted.\nTarget: {} at {} (profile {:?}).\nSelected revision: {}; root {}; catalog root {}.\nCatalog images: {}\nImage ordinals: {}\n",
        scope.package,
        scope.coordinate,
        profile,
        stamp.selection_revision(),
        hex(stamp.selected_root()),
        hex(stamp.catalog_root().as_bytes()),
        image_ordinals.len(),
        image_ordinals
            .iter()
            .map(|ordinal| ordinal.to_string())
            .collect::<Vec<_>>()
            .join(", "),
    ))
}

fn parse_product_operations(value: Option<&str>) -> Result<Vec<RemoteIndexQueryOperation>, Fault> {
    let mut operations = value
        .unwrap_or("search,names,document,source,outline,graph,related")
        .split(',')
        .map(parse_product_operation)
        .collect::<Result<Vec<_>, _>>()?;
    operations.sort_unstable();
    operations.dedup();
    if operations.is_empty() {
        return Err(usage(
            "--operations",
            "include at least one query operation",
        ));
    }
    Ok(operations)
}

fn parse_product_operation(value: &str) -> Result<RemoteIndexQueryOperation, Fault> {
    match value {
        "search" => Ok(RemoteIndexQueryOperation::Search),
        "names" => Ok(RemoteIndexQueryOperation::Names),
        "document" => Ok(RemoteIndexQueryOperation::Document),
        "source" => Ok(RemoteIndexQueryOperation::Source),
        "outline" => Ok(RemoteIndexQueryOperation::Outline),
        "graph" => Ok(RemoteIndexQueryOperation::Graph),
        "related" => Ok(RemoteIndexQueryOperation::Related),
        "index-search" => Ok(RemoteIndexQueryOperation::IndexSearch),
        _ => Err(usage(
            "--operations",
            "choose search, names, document, source, outline, graph, related, or index-search",
        )),
    }
}

fn product_operation_name(operation: &RemoteIndexQueryOperation) -> &'static str {
    match operation {
        RemoteIndexQueryOperation::Search => "search",
        RemoteIndexQueryOperation::Names => "names",
        RemoteIndexQueryOperation::Document => "document",
        RemoteIndexQueryOperation::Source => "source",
        RemoteIndexQueryOperation::Outline => "outline",
        RemoteIndexQueryOperation::Graph => "graph",
        RemoteIndexQueryOperation::Related => "related",
        RemoteIndexQueryOperation::IndexSearch => "index-search",
    }
}

fn capability_limits(flags: &BTreeMap<&str, &str>) -> Result<(u64, u64, u32, u64), Fault> {
    let issued_at = remote_index_now().map_err(|error| {
        usage(
            "cluster owner grant",
            format!("system clock unavailable: {error}"),
        )
    })?;
    let ttl_seconds = flags
        .get("ttl-seconds")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| usage("--ttl-seconds", "use an integer from 60 to 2592000"))
        })
        .transpose()?
        .unwrap_or(3600);
    if !(60..=30 * 24 * 60 * 60).contains(&ttl_seconds) {
        return Err(usage("--ttl-seconds", "use an integer from 60 to 2592000"));
    }
    let expires_at = issued_at
        .checked_add(ttl_seconds.saturating_mul(1000))
        .ok_or_else(|| usage("--ttl-seconds", "expiry overflow"))?;
    let request_budget = flags
        .get("request-budget")
        .map(|value| {
            value
                .parse::<u32>()
                .map_err(|_| usage("--request-budget", "use a positive request count"))
        })
        .transpose()?
        .unwrap_or(10_000);
    let byte_budget = flags
        .get("byte-budget")
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| usage("--byte-budget", "use a positive byte count"))
        })
        .transpose()?
        .unwrap_or(256 * 1024 * 1024);
    if request_budget == 0 || byte_budget == 0 {
        return Err(usage(
            "cluster owner grant",
            "request and byte budgets must be positive",
        ));
    }
    Ok((issued_at, expires_at, request_budget, byte_budget))
}

fn fresh_grant_id() -> [u8; 16] {
    let random = SecretKey::generate().to_bytes();
    let mut grant_id = [0; 16];
    grant_id.copy_from_slice(&random[..16]);
    grant_id
}

fn workspace_paths(options: &Options) -> Result<backend_runtime::WorkspacePaths, Fault> {
    backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map_err(|error| workspace_fault(options, error.to_string()))
}

fn client_key_path(options: &Options) -> Result<PathBuf, Fault> {
    Ok(workspace_paths(options)?.data().join(REMOTE_CLIENT_FILE))
}

fn write_remote_capability(
    path: &std::path::Path,
    capability: &RemoteIndexCapability,
) -> Result<(), Fault> {
    let encoded = capability
        .encode()
        .map_err(|error| storage_fault(path, error.to_string()))?;
    let mut bytes = Vec::with_capacity(REMOTE_CAPABILITY_MAGIC.len() + encoded.len());
    bytes.extend_from_slice(REMOTE_CAPABILITY_MAGIC);
    bytes.extend_from_slice(&encoded);
    write_private_new(path, &bytes)
}

pub(crate) fn load_remote_capability(
    path: &std::path::Path,
) -> Result<RemoteIndexCapability, Fault> {
    let bytes = read_bounded_file(
        path,
        backend_engine::cluster_transport::MAX_REMOTE_INDEX_AUTH_BYTES
            + REMOTE_CAPABILITY_MAGIC.len(),
    )?;
    let payload = bytes
        .strip_prefix(REMOTE_CAPABILITY_MAGIC.as_slice())
        .ok_or_else(|| storage_fault(path, "invalid capability file header".to_owned()))?;
    RemoteIndexCapability::decode(payload).map_err(|error| storage_fault(path, error.to_string()))
}

pub(crate) fn load_client_secret(path: &std::path::Path) -> Result<SecretKey, Fault> {
    let bytes = read_bounded_file(path, REMOTE_CLIENT_MAGIC.len() + 32)?;
    if bytes.len() != REMOTE_CLIENT_MAGIC.len() + 32
        || bytes.get(..REMOTE_CLIENT_MAGIC.len()) != Some(REMOTE_CLIENT_MAGIC.as_slice())
    {
        return Err(storage_fault(path, "invalid client key file".to_owned()));
    }
    let secret: [u8; 32] = bytes[REMOTE_CLIENT_MAGIC.len()..]
        .try_into()
        .map_err(|_| storage_fault(path, "invalid client key length".to_owned()))?;
    Ok(SecretKey::from_bytes(&secret))
}

fn read_bounded_file(path: &std::path::Path, maximum: usize) -> Result<Vec<u8>, Fault> {
    // Open and validate one private handle. A separate metadata check followed
    // by File::open would permit a final-component symlink replacement race.
    let mut file = backend_platform::durable::open_private_read(path)
        .map_err(|error| storage_fault(path, error.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|error| storage_fault(path, error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum as u64 {
        return Err(storage_fault(
            path,
            "file type or size is invalid".to_owned(),
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| storage_fault(path, error.to_string()))?;
    if bytes.len() > maximum {
        return Err(storage_fault(
            path,
            "file exceeded its size bound".to_owned(),
        ));
    }
    Ok(bytes)
}

fn write_private_new(path: &std::path::Path, bytes: &[u8]) -> Result<(), Fault> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    backend_platform::durable::ensure_private_directory(parent)
        .map_err(|error| storage_fault(parent, error.to_string()))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| storage_fault(path, error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| storage_fault(path, error.to_string()))?;
    file.sync_all()
        .map_err(|error| storage_fault(path, error.to_string()))?;
    #[cfg(unix)]
    {
        let mut permissions = file
            .metadata()
            .map_err(|error| storage_fault(path, error.to_string()))?
            .permissions();
        permissions.set_mode(0o600);
        file.set_permissions(permissions)
            .map_err(|error| storage_fault(path, error.to_string()))?;
    }
    Ok(())
}

fn client_fault(error: impl std::fmt::Display) -> Fault {
    Fault::new(
        FaultSlug::Endpoint,
        Operand::Text("remote index".to_owned()),
        Cause::new(CauseSlug::Unreachable, error.to_string()),
        Affordance::None,
    )
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

fn remote_usage_path(options: &Options) -> Result<PathBuf, Fault> {
    let owner_path = owner_path(options)?;
    let parent = owner_path
        .parent()
        .ok_or_else(|| storage_fault(&owner_path, "owner path has no parent".to_owned()))?;
    Ok(parent.join("remote-index-grants.v1"))
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

pub(crate) fn endpoint_id(value: &str) -> Result<EndpointId, Fault> {
    let bytes = fixed_hex::<32>(value)?;
    EndpointId::from_bytes(&bytes).map_err(|_| usage("--peer", "peer identity is invalid"))
}

pub(crate) fn socket_address(value: &str, operand: &str) -> Result<SocketAddr, Fault> {
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

    #[cfg(unix)]
    #[test]
    fn remote_client_key_and_capability_reads_reject_final_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::time::{SystemTime, UNIX_EPOCH};

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "backend-remote-client-private-read-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create private test directory");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("set private directory mode");

        let target = root.join("private-state");
        let mut key_bytes = REMOTE_CLIENT_MAGIC.to_vec();
        key_bytes.extend_from_slice(&[11; 32]);
        std::fs::write(&target, key_bytes).expect("write private key fixture");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600))
            .expect("set private file mode");
        assert!(load_client_secret(&target).is_ok());

        let key_link = root.join("client-key-link");
        symlink(&target, &key_link).expect("link client key fixture");
        assert!(load_client_secret(&key_link).is_err());

        let capability_link = root.join("capability-link");
        symlink(&target, &capability_link).expect("link capability fixture");
        assert!(read_bounded_file(&capability_link, 128).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

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
