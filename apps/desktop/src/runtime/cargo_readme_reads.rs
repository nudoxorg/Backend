//! Exact owner README and relative-link reads. No client filesystem lookup.

use super::reads::{ReadContext, check, failure, rehydrate_cargo_source_authority, shape};
use crate::core::{ErrorValue, FaultCode};
use crate::model::browse::{BrowseValue, CargoReadmeDocument, CargoReadmeKey, CargoReadmeModel, CargoReadmeState};
use crate::model::pages::{CargoSourceKey, CargoSourcePage, PageValue, ReadFailure, SourceOrigin, SourceText};
use crate::navigation::CargoSourceTarget;
use backend_library::{CargoPackageReadmeFailureV1, CargoPackageReadmeLinkFailureV1, CargoPackageReadmeLinkRequestV1, CargoPackageReadmeLinkResultV1, CargoPackageReadmeOriginV1, CargoPackageReadmeRequestV1, CargoPackageReadmeResultV1, SurfaceCommand, SurfaceReply};
use backend_present::Engine;
use std::sync::Arc;

pub(crate) fn compose(engine: &mut dyn Engine, key: &CargoReadmeKey, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
    check(context.cancel)?;
    let mut result = request_readme(engine, key)?;
    check(context.cancel)?;
    // This closed reply is only a cold-cache hint. It admits no content,
    // absence or action; the fresh Tree must prove the full saved selector.
    if result.has_admissible_shape() && matches!(&result,
        CargoPackageReadmeResultV1::Unavailable { package: Some(package), request_binding: None,
            reason: CargoPackageReadmeFailureV1::AuthorityUnavailable } if package == key.package.reference())
    {
        rehydrate_cargo_source_authority(engine, &key.context, &key.package, context.cancel)?;
        check(context.cancel)?;
        result = request_readme(engine, key)?;
        check(context.cancel)?;
    }
    let page = readme_page(key, result)?;
    check(context.cancel)?;
    Ok(page)
}

fn request_readme(engine: &mut dyn Engine, key: &CargoReadmeKey) -> Result<CargoPackageReadmeResultV1, ReadFailure> {
    let request = CargoPackageReadmeRequestV1::from_tree(key.package.reference().clone(), key.context.request_binding());
    if !request.has_admissible_shape() { return Err(shape("Cargo README selector")); }
    match engine.surface(SurfaceCommand::CargoPackageReadme { request }).map_err(|error| failure(&error))? {
        SurfaceReply::CargoPackageReadme(result) => Ok(result),
        _ => Err(shape("Cargo README")),
    }
}

fn readme_page(key: &CargoReadmeKey, result: CargoPackageReadmeResultV1) -> Result<PageValue, ReadFailure> {
    if !result.has_admissible_shape() { return Err(shape("Cargo README proof")); }
    let binding = key.context.request_binding();
    let exact = match &result {
        CargoPackageReadmeResultV1::Read { package, request_binding, .. }
        | CargoPackageReadmeResultV1::Absent { package, request_binding, .. } => package == key.package.reference() && *request_binding == binding,
        CargoPackageReadmeResultV1::Stale { package, request_binding } => package == key.package.reference() && *request_binding == Some(binding),
        CargoPackageReadmeResultV1::Unavailable { package, request_binding, .. } => package.as_ref() == Some(key.package.reference()) && *request_binding == Some(binding),
    };
    if !exact { return Err(shape("Cargo README address mismatch")); }
    let origin = CargoPackageReadmeOriginV1::from_result(&result);
    let (state, source_revision) = match result {
        CargoPackageReadmeResultV1::Read { authority, readme, .. } => {
            let origin = origin.ok_or_else(|| shape("Cargo README origin"))?;
            (CargoReadmeState::Read(Arc::new(CargoReadmeDocument::prepare(origin, Arc::from(readme.contents)))), authority.source_revision())
        }
        CargoPackageReadmeResultV1::Absent { authority, reason, .. } => (CargoReadmeState::Absent(reason), authority.source_revision()),
        CargoPackageReadmeResultV1::Stale { .. } => return Err(missing("The Cargo README observation changed. Reopen its current Library tree.")),
        CargoPackageReadmeResultV1::Unavailable { reason, .. } => return Err(readme_failure(reason)),
    };
    Ok(PageValue::Browse(BrowseValue::CargoReadme(Arc::new(CargoReadmeModel {
        package: key.package.clone(), request_binding: binding, source_revision, state,
    }))))
}

pub(crate) fn compose_link(engine: &mut dyn Engine, key: &CargoSourceKey, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
    let CargoSourceTarget::ReadmeLink(link) = &key.target else { return Err(shape("Cargo README link scope")); };
    if link.origin().package != *key.package.reference() || link.origin().request_binding != key.context.request_binding() {
        return Err(shape("Cargo README link selector"));
    }
    check(context.cancel)?;
    let mut result = request_link(engine, link)?;
    check(context.cancel)?;
    if result.has_admissible_shape() && matches!(&result,
        CargoPackageReadmeLinkResultV1::Unavailable { origin: Some(origin), reason: CargoPackageReadmeLinkFailureV1::ObservationUnavailable }
            if origin == link.origin())
    {
        rehydrate_cargo_source_authority(engine, &key.context, &key.package, context.cancel)?;
        check(context.cancel)?;
        result = request_link(engine, link)?;
        check(context.cancel)?;
    }
    let page = link_page(key, result)?;
    check(context.cancel)?;
    Ok(page)
}

fn request_link(engine: &mut dyn Engine, link: &crate::navigation::CargoReadmeLinkAddress) -> Result<CargoPackageReadmeLinkResultV1, ReadFailure> {
    let request = CargoPackageReadmeLinkRequestV1 { origin: link.origin().clone(), href: link.href().to_owned() };
    if !request.has_admissible_shape() { return Err(shape("Cargo README link selector")); }
    match engine.surface(SurfaceCommand::CargoPackageReadmeLink { request }).map_err(|error| failure(&error))? {
        SurfaceReply::CargoPackageReadmeLink(result) => Ok(result),
        _ => Err(shape("Cargo README link")),
    }
}

fn link_page(key: &CargoSourceKey, result: CargoPackageReadmeLinkResultV1) -> Result<PageValue, ReadFailure> {
    let CargoSourceTarget::ReadmeLink(link) = &key.target else { return Err(shape("Cargo README link scope")); };
    if !result.has_admissible_shape() { return Err(shape("Cargo README target proof")); }
    let exact = match &result {
        CargoPackageReadmeLinkResultV1::Read { origin, root_scope, path, fragment, .. } => origin == link.origin()
            && *root_scope == link.origin().root_scope && path.as_str() == link.path().as_str()
            && fragment.as_deref() == link.fragment(),
        CargoPackageReadmeLinkResultV1::Stale { origin } => origin == link.origin(),
        CargoPackageReadmeLinkResultV1::Unavailable { origin, .. } => origin.as_ref() == Some(link.origin()),
        CargoPackageReadmeLinkResultV1::Anchor { .. } => false,
    };
    if !exact { return Err(shape("Cargo README target address mismatch")); }
    match result {
        CargoPackageReadmeLinkResultV1::Read { authority, contents, content_digest, .. } => {
            let source = SourceText::new(Arc::from(contents), 1, SourceOrigin::LocalFile, true).map_err(|_| shape("Cargo README target line range"))?;
            Ok(PageValue::CargoSource(CargoSourcePage { package: key.package.clone(), request_binding: key.context.request_binding(),
                target: key.target.clone(), source, content_digest, source_revision: authority.source_revision() }))
        }
        CargoPackageReadmeLinkResultV1::Stale { .. } => Err(missing("The README origin changed. Reopen the package's current README before following this link.")),
        CargoPackageReadmeLinkResultV1::Unavailable { reason, .. } => {
            let message = match reason {
                CargoPackageReadmeLinkFailureV1::InvalidRequest => "This README link address is invalid.",
                CargoPackageReadmeLinkFailureV1::StaleOrigin => "The README origin changed. Reopen its current package page.",
                CargoPackageReadmeLinkFailureV1::NotRelative => "This README target is not a supported relative file.",
                CargoPackageReadmeLinkFailureV1::OutsideScope => "This README target escapes its package or workspace root.",
                CargoPackageReadmeLinkFailureV1::UnsupportedFileKind => "This README target's file kind is unavailable in the source reader.",
                CargoPackageReadmeLinkFailureV1::TargetUnavailable => "The README target is absent, linked or unreadable under its exact root.",
                CargoPackageReadmeLinkFailureV1::TargetTooLarge => "The README target exceeds the bounded source reader.",
                CargoPackageReadmeLinkFailureV1::NotUtf8Text => "The README target is not bounded UTF-8 text.",
                CargoPackageReadmeLinkFailureV1::ObservationUnavailable => "The README source observation could not be revalidated. Reopen its current Library tree.",
            };
            Err(missing(message))
        }
        CargoPackageReadmeLinkResultV1::Anchor { .. } => Err(shape("Cargo README file target")),
    }
}

fn missing(message: &'static str) -> ReadFailure { ReadFailure::Fault(ErrorValue::new(FaultCode::Missing, message)) }

fn readme_failure(reason: CargoPackageReadmeFailureV1) -> ReadFailure {
    use CargoPackageReadmeFailureV1 as Reason;
    let message = match reason {
        Reason::InvalidPackageReference => "This Cargo README package address is invalid.",
        Reason::AuthorityUnavailable => "This package has no current Cargo README receipt. Reopen its Library tree.",
        Reason::SourceObservationUnavailable => "The Cargo README source observation could not be revalidated.",
        Reason::PackageRootUnavailable => "The admitted Cargo package folder is unavailable.",
        Reason::InvalidReadmePath => "The manifest's README path is invalid.",
        Reason::ReadmeOutsideAuthorizedRoot => "The selected README escapes its exact owner-held root.",
        Reason::WorkspaceReadmeUnresolved => "The workspace-inherited README could not be resolved from the admitted manifests.",
        Reason::PackageManifestUnavailable => "The exact package manifest could not be read safely.",
        Reason::PackageManifestTooLarge => "The package manifest exceeds the README admission bound.",
        Reason::PackageManifestMalformed => "The package manifest has an invalid README declaration.",
        Reason::SelectedFileUnavailable => "The selected README is absent, linked or unreadable.",
        Reason::ContentTooLarge => "The selected README exceeds the bounded reader; no prefix was returned.",
        Reason::NotUtf8Text => "The selected README is not bounded UTF-8 Markdown.",
    };
    missing(message)
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;
    use backend_library::{CargoPackageReadmeRootScopeV1, CargoPackageReadmeSelectionV1, CargoPackageReadmeV1, CargoPackageSourceAuthorityStateV1, CargoPackageSourcePathV1, CargoPackageSourceSemanticStatusV1};
    use crate::core::LocalProjectId;
    use crate::model::pages::PackageRef;
    use crate::navigation::{CargoBrowseContext, CargoReadmeLinkAddress};
    use std::path::Path;

    /// Immutable shape-valid producer fixture, not a live owner observation.
    pub(crate) fn fixture() -> (backend_library::browse::ProjectTree, CargoReadmeKey, CargoPackageReadmeResultV1) {
        use backend_advisory::{AdvisoryAuthority, normalize_package};
        use backend_library::browse::{ProjectTreeRequestBindingV1, build_tree, metadata_input_with_stable_source_witness};
        const METADATA: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/library/browse/fixtures/tree-2026-09-27/metadata.json"));
        let input = metadata_input_with_stable_source_witness(METADATA, "aarch64-apple-darwin", None, [7; 32]).expect("metadata witness fixture");
        let advisories = AdvisoryAuthority::new(1);
        let mut tree = build_tree(&input, &|name, version| advisories.observe(&normalize_package("cargo", name).expect("identity"), version, false, false, 0, false));
        let project = LocalProjectId::new("/fixture/workspace/member").expect("requested member");
        let binding = ProjectTreeRequestBindingV1::for_paths(Path::new(project.service_coordinate().expect("coordinate")), &tree.root).expect("full owner fixture binding");
        tree.request_binding = Some(binding);
        assert!(tree.has_admissible_shape());
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &tree.package("serde", "1.0.219").expect("package").source_authority else { panic!("exact source receipt"); };
        let reference = authority.package_reference().expect("source-qualified package");
        let contents: Box<str> = "# Guide\n\n[Local code](../src/lib.rs#L7)\n\n[On this page](#guide)\n\n[Outside](../../outside.rs)\n".into();
        let readme = CargoPackageReadmeV1 { root_scope: CargoPackageReadmeRootScopeV1::EffectiveWorkspace,
            path: CargoPackageSourcePathV1::new("docs/README.md").expect("relative README"), selection: CargoPackageReadmeSelectionV1::WorkspaceInherited,
            content_digest: *blake3::hash(contents.as_bytes()).as_bytes(), contents, semantic: CargoPackageSourceSemanticStatusV1::NotIndexed };
        let result = CargoPackageReadmeResultV1::Read { package: reference.clone(), authority: authority.clone(), request_binding: binding, readme };
        assert!(result.has_admissible_shape());
        let key = CargoReadmeKey { context: CargoBrowseContext::from_binding_address(project, binding).expect("address"), package: PackageRef::from_reference(reference) };
        (tree, key, result)
    }

    pub(crate) fn fixture_value() -> (CargoReadmeKey, PageValue) {
        let (_, key, result) = fixture();
        let value = readme_page(&key, result).expect("validated owner projection");
        (key, value)
    }

    #[test]
    fn exact_owner_readme_and_absence_do_not_borrow_missing_or_other_package_bindings() {
        let (_, key, result) = fixture();
        let PageValue::Browse(BrowseValue::CargoReadme(model)) = readme_page(&key, result.clone()).expect("exact README") else { panic!("README model"); };
        let CargoReadmeState::Read(document) = &model.state else { panic!("read"); };
        assert_eq!(document.origin.root_scope, CargoPackageReadmeRootScopeV1::EffectiveWorkspace);
        assert!(matches!(document.destination("#guide"), crate::model::browse::CargoReadmeDestination::Anchor(_)));
        assert!(matches!(document.destination("../../outside.rs"), crate::model::browse::CargoReadmeDestination::Unavailable(_)));
        let CargoPackageReadmeResultV1::Read { authority, .. } = result else { panic!("fixture read"); };
        let absence = CargoPackageReadmeResultV1::Absent { package: key.package.reference().clone(), authority, request_binding: key.context.request_binding(), reason: backend_library::CargoPackageReadmeAbsenceV1::ManifestDisabled };
        assert!(matches!(readme_page(&key, absence).expect("exact manifest absence"), PageValue::Browse(BrowseValue::CargoReadme(model)) if matches!(&model.state, CargoReadmeState::Absent(backend_library::CargoPackageReadmeAbsenceV1::ManifestDisabled))));
        let mut other = key.context.request_binding(); other.effective_workspace_root_digest = [8; 32];
        for result in [
            CargoPackageReadmeResultV1::Stale { package: key.package.reference().clone(), request_binding: None },
            CargoPackageReadmeResultV1::Stale { package: key.package.reference().clone(), request_binding: Some(other) },
            CargoPackageReadmeResultV1::Unavailable { package: Some(key.package.reference().clone()), request_binding: None, reason: CargoPackageReadmeFailureV1::SelectedFileUnavailable },
            CargoPackageReadmeResultV1::Unavailable { package: None, request_binding: Some(key.context.request_binding()), reason: CargoPackageReadmeFailureV1::AuthorityUnavailable },
        ] {
            assert!(matches!(readme_page(&key, result), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Protocol));
        }
        assert!(matches!(readme_page(&key, CargoPackageReadmeResultV1::Stale { package: key.package.reference().clone(), request_binding: Some(key.context.request_binding()) }), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Missing));
    }

    #[test]
    fn inherited_link_bytes_keep_workspace_scope_and_exact_origin_path_fragment() {
        let (_, readme_key, result) = fixture();
        let origin = CargoPackageReadmeOriginV1::from_result(&result).expect("origin");
        let link = CargoReadmeLinkAddress::new(origin.clone(), "../src/lib.rs#L7").expect("relative workspace target");
        assert_eq!(link.path().as_str(), "src/lib.rs"); assert_eq!(link.source_line(), Some(7));
        let key = CargoSourceKey { context: readme_key.context.clone(), package: readme_key.package.clone(), target: CargoSourceTarget::ReadmeLink(link.clone()) };
        let CargoPackageReadmeResultV1::Read { authority, .. } = result else { panic!("fixture read"); };
        let contents: Box<str> = "pub fn workspace_target() {}\n".into();
        let read = CargoPackageReadmeLinkResultV1::Read { origin: origin.clone(), authority, root_scope: origin.root_scope,
            path: CargoPackageSourcePathV1::new("src/lib.rs").expect("target path"), fragment: Some("L7".into()),
            content_digest: *blake3::hash(contents.as_bytes()).as_bytes(), contents, semantic: CargoPackageSourceSemanticStatusV1::NotIndexed };
        let PageValue::CargoSource(page) = link_page(&key, read.clone()).expect("exact workspace bytes") else { panic!("source page"); };
        assert_eq!(page.target, key.target); assert_eq!(page.source.coverage(), crate::model::pages::SourceCoverage::Unverified);
        let package_file = CargoSourceKey { target: CargoSourceTarget::PackageFile(link.path().clone()), ..key.clone() };
        assert_ne!(key, package_file, "identical relative spelling cannot collapse package and inherited workspace reads");
        for mutate in 0..4 {
            let mut forged = read.clone();
            if let CargoPackageReadmeLinkResultV1::Read { origin, root_scope, path, fragment, .. } = &mut forged {
                match mutate {
                    0 => *root_scope = CargoPackageReadmeRootScopeV1::Package,
                    1 => *path = CargoPackageSourcePathV1::new("src/other.rs").expect("another path"),
                    2 => *fragment = Some("L8".into()),
                    _ => origin.content_digest = [9; 32],
                }
            }
            assert!(matches!(link_page(&key, forged), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Protocol));
        }
        assert!(matches!(link_page(&key, CargoPackageReadmeLinkResultV1::Unavailable { origin: None, reason: CargoPackageReadmeLinkFailureV1::ObservationUnavailable }), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Protocol));
        assert!(matches!(link_page(&key, CargoPackageReadmeLinkResultV1::Stale { origin }), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Missing));
    }

    struct Cold {
        tree: backend_library::browse::ProjectTree,
        final_reply: CargoPackageReadmeResultV1,
        requests: Vec<CargoPackageReadmeRequestV1>,
        trees: usize,
    }
    impl Engine for Cold {
        fn revision(&mut self) -> Result<backend_library::ViewStateRoot, backend_client::ClientError> { Err(backend_client::ClientError::Protocol("unused".into())) }
        fn health(&mut self) -> Result<backend_library::HealthReport, backend_client::ClientError> { Err(backend_client::ClientError::Protocol("unused".into())) }
        fn probe(&mut self, _: backend_present::Probe<'_>) -> Result<backend_library::ReplyDto, backend_client::ClientError> { Err(backend_client::ClientError::Protocol("unused".into())) }
        fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, backend_client::ClientError> {
            match command {
                SurfaceCommand::CargoPackageReadme { request } => {
                    self.requests.push(request.clone());
                    if self.requests.len() == 1 { Ok(SurfaceReply::CargoPackageReadme(CargoPackageReadmeResultV1::Unavailable { package: Some(request.package), request_binding: None, reason: CargoPackageReadmeFailureV1::AuthorityUnavailable })) }
                    else { Ok(SurfaceReply::CargoPackageReadme(self.final_reply.clone())) }
                }
                SurfaceCommand::ProjectTree { root } => {
                    assert_eq!(root.as_str(), "/fixture/workspace/member"); self.trees += 1;
                    Ok(SurfaceReply::ProjectTree(self.tree.clone()))
                }
                _ => Err(backend_client::ClientError::Protocol("unexpected surface".into())),
            }
        }
    }

    #[test]
    fn cold_hint_observes_exact_member_tree_once_before_identical_readme_retry() {
        let (tree, key, result) = fixture();
        let cancel = crate::runtime::actor::CancellationToken::new();
        let outlines = super::super::reads::OutlineCache::default();
        let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines, progress: None };
        let mut cold = Cold { tree: tree.clone(), final_reply: result.clone(), requests: Vec::new(), trees: 0 };
        assert!(matches!(compose(&mut cold, &key, &context), Ok(PageValue::Browse(BrowseValue::CargoReadme(_)))));
        assert_eq!(cold.trees, 1); assert_eq!(cold.requests.len(), 2); assert_eq!(cold.requests[0], cold.requests[1]);
        assert_eq!(cold.requests[0].expected_workspace_root_digest, Some(key.context.request_binding().effective_workspace_root_digest));
        let mut changed = tree.clone(); let mut binding = key.context.request_binding(); binding.requested_root_digest = [4; 32]; changed.request_binding = Some(binding);
        let mut cold = Cold { tree: changed, final_reply: result, requests: Vec::new(), trees: 0 };
        assert!(matches!(compose(&mut cold, &key, &context), Err(ReadFailure::Fault(_))));
        assert_eq!(cold.requests.len(), 1, "a different tree binding cannot trigger the second README request");
        let final_reply = CargoPackageReadmeResultV1::Unavailable { package: Some(key.package.reference().clone()), request_binding: None, reason: CargoPackageReadmeFailureV1::AuthorityUnavailable };
        let mut cold = Cold { tree, final_reply, requests: Vec::new(), trees: 0 };
        assert!(matches!(compose(&mut cold, &key, &context), Err(ReadFailure::Fault(error)) if error.code() == FaultCode::Protocol));
        assert_eq!(cold.requests.len(), 2); assert_eq!(cold.trees, 1, "an unbound final reply cannot loop or become absence");
    }
}
