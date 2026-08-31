use std::{
    env,
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use thiserror::Error;

#[derive(Debug, Error)]
enum ProbeError {
    #[error("cannot resolve the current test executable")]
    CurrentExecutable(#[source] io::Error),
    #[error("the current test executable has no dependency directory")]
    MissingDependencyDirectory,
    #[error("cannot read the dependency directory")]
    ReadDependencyDirectory(#[source] io::Error),
    #[error("cannot read a dependency directory entry")]
    ReadDependencyEntry(#[source] io::Error),
    #[error("no compatible format and vocabulary rlib pair was found")]
    MissingCompatibleArtifacts,
    #[error("more than one compatible format and vocabulary rlib pair was found")]
    MultipleCompatibleArtifacts,
    #[error("cannot spawn the consumer compiler")]
    SpawnCompiler(#[source] io::Error),
    #[error("the consumer compiler did not expose its standard input")]
    MissingCompilerInput,
    #[error("cannot write the consumer probe")]
    WriteCompilerInput(#[source] io::Error),
    #[error("cannot collect the consumer compiler result")]
    WaitForCompiler(#[source] io::Error),
}

struct RejectedProbe {
    source: &'static [u8],
    code: &'static str,
    symbols: &'static [&'static str],
}

fn is_rlib(path: &Path, crate_name: &str) -> bool {
    let prefix = format!("lib{crate_name}-");
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".rlib"))
}

fn compatible_artifacts(directory: &Path) -> Result<(PathBuf, PathBuf, PathBuf), ProbeError> {
    let formats = fs::read_dir(directory).map_err(ProbeError::ReadDependencyDirectory)?;
    let mut compatible = None;
    for format_entry in formats {
        let format_path = format_entry
            .map_err(ProbeError::ReadDependencyEntry)?
            .path();
        if !is_rlib(&format_path, "nudox_ir_format") {
            continue;
        }
        let vocabs = fs::read_dir(directory).map_err(ProbeError::ReadDependencyDirectory)?;
        for vocab_entry in vocabs {
            let vocab_path = vocab_entry.map_err(ProbeError::ReadDependencyEntry)?.path();
            if !is_rlib(&vocab_path, "nudox_ir_vocab") {
                continue;
            }
            let id_path = id_artifact(directory)?;
            let probe = compile(
                b"use nudox_id::{ContentId, SourceFactDomain}; use nudox_ir_format::{AtomInput, EntityKind, EntityRecord, PreparedFragment, SourceIdentity}; use nudox_ir_vocab::{AtomId, TypeId}; fn compatible() { let source = SourceIdentity { identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b\"source\"), byte_len: 6 }; let entities = [EntityRecord { semantic_type: TypeId::new(0), name: AtomId::new(0), kind: EntityKind::Constant }]; let atoms = [AtomInput { bytes: b\"name\" }]; let _ = PreparedFragment::prepare(source, &entities, &[], &atoms); }",
                directory,
                &format_path,
                &vocab_path,
                &id_path,
            )?;
            if !probe.status.success() {
                continue;
            }
            if compatible.is_some() {
                return Err(ProbeError::MultipleCompatibleArtifacts);
            }
            compatible = Some((format_path.clone(), vocab_path, id_path));
        }
    }
    compatible.ok_or(ProbeError::MissingCompatibleArtifacts)
}

fn id_artifact(directory: &Path) -> Result<PathBuf, ProbeError> {
    let entries = fs::read_dir(directory).map_err(ProbeError::ReadDependencyDirectory)?;
    let mut result = None;
    for entry in entries {
        let path = entry.map_err(ProbeError::ReadDependencyEntry)?.path();
        if !is_rlib(&path, "nudox_id") {
            continue;
        }
        if result.is_some() {
            return Err(ProbeError::MultipleCompatibleArtifacts);
        }
        result = Some(path);
    }
    result.ok_or(ProbeError::MissingCompatibleArtifacts)
}

fn compile(
    source: &[u8],
    dependencies: &Path,
    format_artifact: &Path,
    vocab_artifact: &Path,
    id_artifact: &Path,
) -> Result<Output, ProbeError> {
    let mut format_extern = OsString::from("nudox_ir_format=");
    format_extern.push(format_artifact);
    let mut vocab_extern = OsString::from("nudox_ir_vocab=");
    vocab_extern.push(vocab_artifact);
    let mut id_extern = OsString::from("nudox_id=");
    id_extern.push(id_artifact);
    let compiler = match env::var_os("RUSTC") {
        Some(compiler) => compiler,
        None => OsString::from("rustc"),
    };
    let mut child = Command::new(compiler)
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "lib",
            "--emit=metadata=-",
            "-L",
        ])
        .arg(dependencies)
        .arg("--extern")
        .arg(format_extern)
        .arg("--extern")
        .arg(vocab_extern)
        .arg("--extern")
        .arg(id_extern)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ProbeError::SpawnCompiler)?;
    let mut input = child.stdin.take().ok_or(ProbeError::MissingCompilerInput)?;
    input
        .write_all(source)
        .map_err(ProbeError::WriteCompilerInput)?;
    drop(input);
    child
        .wait_with_output()
        .map_err(ProbeError::WaitForCompiler)
}

fn assert_rejected(output: &Output, code: &str, symbols: &[&str]) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "unexpected compile success");
    assert!(stderr.contains(code), "missing {code}: {stderr}");
    for symbol in symbols {
        assert!(stderr.contains(symbol), "missing {symbol}: {stderr}");
    }
}

#[test]
fn exported_rlib_keeps_views_private_typed_and_caller_borrowing() -> Result<(), ProbeError> {
    let executable = env::current_exe().map_err(ProbeError::CurrentExecutable)?;
    let dependencies = executable
        .parent()
        .ok_or(ProbeError::MissingDependencyDirectory)?;
    let (format_artifact, vocab_artifact, id_artifact) = compatible_artifacts(dependencies)?;

    let rejected = [
        RejectedProbe {
            source: b"use nudox_ir_format::FragmentView; const BAD: FragmentView<'static> = FragmentView { envelope: &[], entities: &[], type_nodes: &[] };",
            code: "cannot construct",
            symbols: &["envelope", "entities", "type_nodes"],
        },
        RejectedProbe {
            source: b"use nudox_ir_format::FragmentView; fn bad() { let _: FragmentView<'_> = (&[][..]).into(); }",
            code: "error[E0277]",
            symbols: &["From", "FragmentView"],
        },
        RejectedProbe {
            source: b"use nudox_id::{ContentId, SourceFactDomain}; use nudox_ir_format::{PreparedFragment, SourceIdentity}; use nudox_ir_vocab::TypeId; fn bad() { let source = SourceIdentity { identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b\"source\"), byte_len: 6 }; let _ = PreparedFragment::prepare(source, &[TypeId::new(0)], &[], &[]); }",
            code: "error[E0308]",
            symbols: &["Entity", "Type"],
        },
        RejectedProbe {
            source: b"use nudox_ir_format::TypeNode; use nudox_ir_vocab::EntityId; fn bad() { let _ = TypeNode::Reference(EntityId::new(0)); }",
            code: "error[E0308]",
            symbols: &["Entity", "Type"],
        },
        RejectedProbe {
            source: b"use nudox_id::{ContentId, SourceFactDomain}; use nudox_ir_format::{AtomInput, EntityKind, EntityRecord, PreparedFragment, SourceIdentity, TypeNode}; use nudox_ir_vocab::{AtomId, TypeId}; fn bad() -> usize { let prepared; { let source = SourceIdentity { identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b\"source\"), byte_len: 6 }; let entities = [EntityRecord { semantic_type: TypeId::new(0), name: AtomId::new(0), kind: EntityKind::Constant }]; let nodes = [TypeNode::Reference(TypeId::new(0))]; let atoms = [AtomInput { bytes: b\"name\" }]; prepared = match PreparedFragment::prepare(source, &entities, &nodes, &atoms) { Ok(value) => value, Err(_) => return 0 }; } prepared.required_capacity() }",
            code: "error[E0597]",
            symbols: &["entities", "nodes"],
        },
        RejectedProbe {
            source: b"use nudox_id::{ContentId, SourceFactDomain}; use nudox_ir_format::{PreparedFragment, SourceIdentity}; fn bad() { let source = SourceIdentity { identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b\"source\"), byte_len: 6 }; let entities = []; let nodes = []; let atoms = []; let mut prepared = match PreparedFragment::prepare(source, &entities, &nodes, &atoms) { Ok(value) => value, Err(_) => return }; prepared.encoded_len = usize::MAX; }",
            code: "error[E0609]",
            symbols: &["encoded_len", "PreparedFragment"],
        },
    ];
    for probe in rejected {
        let output = compile(
            probe.source,
            dependencies,
            &format_artifact,
            &vocab_artifact,
            &id_artifact,
        )?;
        assert_rejected(&output, probe.code, probe.symbols);
    }

    let legal = compile(
        b"use nudox_id::{ContentId, SourceFactDomain}; use nudox_ir_format::{AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment, PrimitiveType, SourceIdentity, TypeNode}; use nudox_ir_vocab::{AtomId, TypeId}; fn legal(output: &mut [u8]) -> usize { let source = SourceIdentity { identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b\"source\"), byte_len: 6 }; let entities = [EntityRecord { semantic_type: TypeId::new(0), name: AtomId::new(0), kind: EntityKind::Constant }]; let nodes = [TypeNode::Primitive(PrimitiveType::Bool)]; let atoms = [AtomInput { bytes: b\"name\" }]; let prepared = match PreparedFragment::prepare(source, &entities, &nodes, &atoms) { Ok(value) => value, Err(_) => return 0 }; let bytes = match prepared.write_into(output) { Ok(value) => value, Err(_) => return 0 }; match FragmentView::validate(bytes) { Ok(view) => view.entities().count() + view.type_nodes().count() + view.atoms().count(), Err(_) => 0 } }",
        dependencies,
        &format_artifact,
        &vocab_artifact,
        &id_artifact,
    )?;
    assert!(
        legal.status.success(),
        "legal consumer rejected: {}",
        String::from_utf8_lossy(&legal.stderr)
    );
    Ok(())
}
