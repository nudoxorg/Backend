//! Native project lowering must carry Ruff's complete direct-body inventories.

use std::{
    collections::BTreeSet,
    error::Error,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

use backend_frontend_python::legacy::{
    CheckerReport,
    checker::{NativePythonProjectAuthority, PythonProjectControl, PythonProjectSource},
    extract,
};
use backend_semantic::{
    ir::{FactAvailability, Ir, SemanticReader, SourceIdentity},
    vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, PythonVersion, Stage},
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

use super::{FactSet, collect_with_project_checker};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

fn project_image(source: &str, checker: &CheckerReport) -> TestResult<Ir> {
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let module = extract(source.as_bytes(), PythonVersion::Python314)
        .map_err(|cause| format!("{cause:?}"))?;
    let mut facts = FactSet::new();
    collect_with_project_checker(&module, source.as_bytes(), &mut facts, checker)
        .map_err(|cause| format!("{cause:?}"))?;
    let identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
    let recipe = CompileRecipeFact::derive(
        profile,
        Stage::LowerIr,
        NativeTool::Python,
        identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"native-python-members-regression"),
    );
    facts
        .build_ir(
            profile,
            SourceIdentity {
                identity,
                byte_len: source.len() as u32,
            },
            recipe,
            crate::driver::types::DeclarationScope::fixture(),
        )
        .map_err(|cause| format!("{cause:?}").into())
}

fn members(ir: &Ir, owner: &[u8]) -> TestResult<(FactAvailability, Vec<Vec<u8>>)> {
    let item = ir
        .items()
        .find(|item| item.name() == owner)
        .ok_or("class absent")?;
    let authority = ir
        .entity(item.id())
        .ok_or("class entity absent")?
        .authority
        .members;
    let ids = item.members();
    assert_eq!(
        ids.iter().collect::<BTreeSet<_>>().len(),
        ids.len(),
        "member IDs are distinct"
    );
    let mut names = Vec::new();
    for id in ids {
        let member = ir.item(*id).ok_or("member entity absent")?;
        assert!(
            ir.entity(*id)
                .ok_or("member entity absent")?
                .source
                .is_some()
        );
        names.push(
            member
                .name()
                .named_bytes()
                .ok_or("unnamed direct member")?
                .to_vec(),
        );
    }
    names.sort();
    Ok((authority, names))
}

fn native_image(source: &str) -> TestResult<Ir> {
    let root = tempfile::tempdir()?;
    let authority = NativePythonProjectAuthority::admit()?;
    let sources = [
        PythonProjectSource {
            relative_path: "pkg/__init__.py",
            source: "",
        },
        PythonProjectSource {
            relative_path: "pkg/model.py",
            source,
        },
    ];
    let cancelled = AtomicBool::new(false);
    let report = authority.analyze_project(
        root.path(),
        "pkg",
        &sources,
        PythonVersion::Python314,
        PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        },
    )?;
    let checked = report
        .module("pkg/model.py")
        .ok_or("native model report absent")?;
    project_image(source, checked)
}

#[test]
fn native_project_inventory_preserves_declared_members_and_unavailable_bodies() -> TestResult {
    let source = concat!(
        "class Base:\n    inherited: int = 1\n",
        "class Child(Base):\n    own: str = 'x'\n",
        "    class Meta:\n        label = 'child'\n",
        "    def run(self, value: int) -> int:\n        self.runtime = value\n        return value\n",
        "class Empty:\n    pass\n",
        "class Imported:\n    import math\n",
        "class Conditional:\n    if True:\n        hidden = 1\n",
        "class Multiple:\n    first = second = 1\n",
        "class Dynamic:\n    exec('created = 1')\n",
    );
    let ir = native_image(source)?;
    assert_eq!(
        members(&ir, b"Child")?,
        (
            FactAvailability::Captured,
            vec![b"Meta".to_vec(), b"own".to_vec(), b"run".to_vec()]
        )
    );
    assert_eq!(
        members(&ir, b"Empty")?,
        (FactAvailability::Captured, vec![])
    );
    for name in [
        b"Imported".as_slice(),
        b"Conditional",
        b"Multiple",
        b"Dynamic",
    ] {
        assert_eq!(members(&ir, name)?, (FactAvailability::Unavailable, vec![]));
    }
    Ok(())
}

#[test]
fn native_project_inventory_refuses_unrepresented_class_scope_bindings() -> TestResult {
    let source = concat!(
        "class Base:\n    pass\n",
        "class NamedValue:\n    x = (y := 1)\n",
        "class NamedDefault:\n    def run(self, arg=(defaulted := 1)):\n        return arg\n",
        "class NamedDecorator:\n    @(decorator := staticmethod)\n    def run():\n        pass\n",
        "class NamedBase:\n    class Inner((base := Base)):\n        pass\n",
        "class NamedClassDecorator:\n    @(decorator := lambda cls: cls)\n    class Inner:\n        pass\n",
        "class NamedLambdaDefault:\n    factory = lambda arg=(defaulted := 1): arg\n",
        "class WrappedExec:\n    x = exec('created = 1')\n",
        "class WrappedEval:\n    x = eval('(created := 1)')\n",
        "class BuiltinExec:\n    x = __builtins__['exec']('created = 1')\n",
        "class BuiltinEval:\n    x = __builtins__['eval']('(created := 1)')\n",
        "class NamespaceUpdate:\n    x = locals().update({'created': 1})\n",
        "class NamespaceSetItem:\n    x = vars().__setitem__('created', 1)\n",
        "class NamespaceUnboundUpdate:\n    x = dict.update(locals(), {'created': 1})\n",
        "class FunctionBodyOnly:\n    def run(self):\n        x = (local := 1)\n        exec('runtime = 1')\n        return x\n",
        "class LambdaBodyOnly:\n    factory = lambda: (local := 1)\n",
        "class NestedBodyOnly:\n    class Inner:\n        x = (inner_only := 1)\n",
        "class OrdinaryFactory:\n    field = dict(default=1)\n",
    );
    let ir = native_image(source)?;
    for name in [
        "NamedValue",
        "NamedDefault",
        "NamedDecorator",
        "NamedBase",
        "NamedClassDecorator",
        "NamedLambdaDefault",
        "WrappedExec",
        "WrappedEval",
        "BuiltinExec",
        "BuiltinEval",
        "NamespaceUpdate",
        "NamespaceSetItem",
        "NamespaceUnboundUpdate",
    ] {
        assert_eq!(
            members(&ir, name.as_bytes())?,
            (FactAvailability::Unavailable, vec![])
        );
    }
    for (owner, member) in [
        ("FunctionBodyOnly", "run"),
        ("LambdaBodyOnly", "factory"),
        ("NestedBodyOnly", "Inner"),
        ("OrdinaryFactory", "field"),
    ] {
        assert_eq!(
            members(&ir, owner.as_bytes())?,
            (FactAvailability::Captured, vec![member.as_bytes().to_vec()])
        );
    }
    Ok(())
}

#[test]
fn actual_docs_document_inventory_reaches_native_project_entrypoint() -> TestResult {
    // Literal source from Docs 6e5dee38e927fac86855d17dfb614fbb64b172b3,
    // src/backend/core/models.py, Document at line 994. The complete file is
    // pinned, including its Python 3.14 syntax, rather than rewritten for a host parser.
    let source = include_str!("fixtures/docs_models_6e5dee38.py");
    assert_eq!(source.len(), 86_300);
    use sha2::Digest;
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(source.as_bytes())),
        "282e82df97d13367659c0efc308fed977924e182220fe3ec6ecf3da71e16d31e",
    );
    let module = extract(source.as_bytes(), PythonVersion::Python314)
        .map_err(|cause| format!("{cause:?}"))?;
    let declaration = module
        .declarations
        .iter()
        .find(|row| row.name == "Document")
        .ok_or("literal Document declaration absent")?;
    assert_eq!(
        source.as_bytes()[..declaration.name_span.start as usize]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count()
            + 1,
        994
    );
    let inventory = module
        .declared_member_inventories
        .iter()
        .find(|row| row.owner == declaration.name_span)
        .ok_or("Ruff complete Document inventory absent")?;
    assert_eq!(inventory.members.len(), 48);
    // Member completeness is Ruff-owned. An empty typed report exercises the
    // exact native project emission entrypoint without fabricating Pyrefly types,
    // resolving Django dependencies, or claiming another project-check execution.
    let ir = project_image(source, &CheckerReport::default())?;
    let (availability, actual) = members(&ir, b"Document")?;
    assert_eq!(availability, FactAvailability::Captured);
    let mut expected = [
        "title",
        "excerpt",
        "link_reach",
        "link_role",
        "creator",
        "deleted_at",
        "ancestors_deleted_at",
        "has_deleted_children",
        "duplicated_from",
        "attachments",
        "alphabet",
        "steplen",
        "node_order_by",
        "path",
        "objects",
        "Meta",
        "__str__",
        "__init__",
        "is_leaf",
        "key_base",
        "file_key",
        "get_self_and_ancestors_paths",
        "get_nb_accesses_cache_key",
        "get_nb_accesses",
        "nb_accesses_direct",
        "nb_accesses_ancestors",
        "_saved_link_definition",
        "from_db",
        "refresh_from_db",
        "remember_link_definition",
        "link_definition_changed",
        "invalidate_nb_accesses_cache",
        "get_role",
        "has_link_trace",
        "compute_ancestors_links_paths_mapping",
        "link_definition",
        "ancestors_link_definition",
        "ancestors_link_definition",
        "ancestors_link_reach",
        "ancestors_link_role",
        "computed_link_definition",
        "computed_link_reach",
        "computed_link_role",
        "get_abilities",
        "send_email",
        "send_invitation_email",
        "soft_delete",
        "restore",
    ]
    .map(|name| name.as_bytes().to_vec())
    .to_vec();
    expected.sort();
    assert_eq!(
        actual, expected,
        "both property accessor sites survive; inherited/runtime members are excluded"
    );
    Ok(())
}
