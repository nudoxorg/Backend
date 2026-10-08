//! Runs a manual native authority check against an unmodified full Serde checkout.
//! Requires root, rustc, sysroot, cargo, and an exclusively owned Cargo home as arguments.
//! Checks the exact cross-file HIR handles twice, with a cold offline reopen.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use backend_frontend_rust::legacy::ra_ap_syntax::ast::HasName;
use backend_frontend_rust::legacy::{
    RustAnalysisControl, RustCargoMetadataPolicy, RustDefinition, RustFeatureControl,
    RustToolchain, RustWorkspace, SemanticKind, SourceByteLimit, SourceOrigin,
    ra_ap_hir::{AssocItem, ModuleDef, PathResolution},
    ra_ap_syntax::{AstNode, ast},
};
use backend_semantic::vocabulary::RustEdition;

fn main() -> Result<()> {
    let arguments = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    ensure!(
        arguments.len() == 5,
        "expected SERDE_ROOT RUSTC SYSROOT CARGO OWNED_CARGO_HOME"
    );
    let root = fs::canonicalize(&arguments[0])?;
    let toolchain = RustToolchain::from_paths_with_cargo(
        arguments[1].clone(),
        arguments[2].clone(),
        arguments[3].clone(),
        arguments[4].clone(),
    )?;
    let declaration_path = root.join("serde_core/src/ser/mod.rs");
    let caller_path = root.join("serde/src/private/ser.rs");
    let declaration_source = fs::read(&declaration_path)?;
    let caller_source = fs::read(&caller_path)?;
    ensure!(
        !root.join("Cargo.lock").exists(),
        "selected checkout must begin without Cargo.lock"
    );
    let cancelled = AtomicBool::new(false);
    for (phase, policy) in [
        ("initial-online", RustCargoMetadataPolicy::Online),
        ("cold-offline", RustCargoMetadataPolicy::Offline),
    ] {
        let started = Instant::now();
        let control = RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(1024 * 1024),
            deadline: Instant::now() + Duration::from_secs(240),
        };
        let workspace = RustWorkspace::open_with_features_and_metadata_policy(
            &root,
            &toolchain,
            RustEdition::Rust2021,
            RustFeatureControl::default(),
            policy,
            control,
        )
        .with_context(|| format!("{phase}: load full selected workspace"))?;
        let (serialize_trait, serialize_method, declaration_count, declaration_offset) = workspace.analyze_source(
            &declaration_path, &declaration_source, control, |authority| {
                let declarations = authority.declarations().collect::<Vec<_>>();
                let selected = declarations.iter().find(|declaration| {
                    matches!(declaration.definition, RustDefinition::Trait(_))
                        && authority.declaration_name(declaration).ok()
                            .and_then(|span| authority.source_at(span).ok()) == Some(b"Serialize")
                }).ok_or(backend_frontend_rust::legacy::RustAuthorityError::MissingSemanticFact { fact: SemanticKind::Trait })?;
                let RustDefinition::Trait(trait_) = selected.definition else {
                    return Err(backend_frontend_rust::legacy::RustAuthorityError::MissingSemanticFact { fact: SemanticKind::Trait });
                };
                let function = trait_.items(authority.database).into_iter().find_map(|item| match item {
                    AssocItem::Function(function) if function.name(authority.database).as_str() == "serialize" => Some(function),
                    _ => None,
                }).ok_or(backend_frontend_rust::legacy::RustAuthorityError::MissingSemanticFact { fact: SemanticKind::Function })?;
                Ok((trait_, function, declarations.len(), authority.declaration_name(selected)?.start))
            },
        ).with_context(|| format!("{phase}: selected Serialize declaration"))?;
        let facts = workspace.analyze_source(&caller_path, &caller_source, control, |authority| {
            let mut resolved_bounds = Vec::new();
            for path in authority.paths() {
                let span = authority.span(path.syntax())?;
                if authority.source_at(span)? == b"Serialize"
                    && matches!(authority.resolve_path(&path), Some((PathResolution::Def(ModuleDef::Trait(trait_)), _)) if trait_ == serialize_trait)
                {
                    resolved_bounds.push(span.start);
                }
            }
            let mut typed_calls = Vec::new();
            for call in authority.method_calls() {
                if call.target == Some(serialize_method)
                    && call.inferred.as_ref().is_some_and(|inferred| !inferred.original.contains_unknown())
                    && authority.definition_origin(serialize_method, SemanticKind::Function) == SourceOrigin::Foreign(SemanticKind::Function)
                    && call.syntax.syntax().ancestors().find_map(ast::Fn::cast)
                        .and_then(|function| function.name())
                        .is_some_and(|name| name.text() == "serialize_tagged_newtype")
                {
                    let span = authority.span(call.syntax.syntax())?;
                    typed_calls.push((span.start, String::from_utf8_lossy(authority.source_at(span)?).into_owned()));
                }
            }
            Ok((authority.declarations().count(), resolved_bounds, typed_calls))
        }).with_context(|| format!("{phase}: selected caller resolves exact foreign HIR handles"))?;
        ensure!(
            declaration_count > 0 && facts.0 > 0,
            "{phase}: declaration rows must be nonempty"
        );
        ensure!(
            !facts.1.is_empty(),
            "{phase}: no Serialize bound resolved to selected serde_core trait"
        );
        ensure!(
            facts
                .2
                .iter()
                .any(|(_, text)| text.starts_with("value.serialize(TaggedSerializer")),
            "{phase}: known value.serialize call did not resolve to selected trait method with inferred type"
        );
        ensure!(
            fs::read(&declaration_path)? == declaration_source
                && fs::read(&caller_path)? == caller_source,
            "{phase}: selected source changed"
        );
        ensure!(
            !root.join("Cargo.lock").exists(),
            "{phase}: product created Cargo.lock in selected checkout"
        );
        println!(
            "{}",
            serde_json::json!({
                "phase":phase, "elapsed_seconds":started.elapsed().as_secs_f64(),
                "expected_declaration":"serde_core/src/ser/mod.rs::Serialize", "declaration_offset":declaration_offset,
                "expected_caller":"serde/src/private/ser.rs::serialize_tagged_newtype", "expected_call":"value.serialize(TaggedSerializer",
                "declaration_count":declaration_count,"caller_declaration_count":facts.0,
                "resolved_exact_trait_bound_offsets":facts.1,"resolved_exact_typed_method_calls":facts.2,
                "project_lockfile_absent":true,"selected_source_unchanged":true,
            })
        );
        drop(workspace);
    }
    Ok(())
}
