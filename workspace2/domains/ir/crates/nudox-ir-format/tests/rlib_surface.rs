use std::{
    env,
    ffi::OsString,
    fmt, fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

enum ProbeError {
    CurrentExecutable(io::Error),
    MissingDependencyDirectory,
    ReadDependencyDirectory(io::Error),
    ReadDependencyEntry(io::Error),
    MissingCompatibleArtifacts,
    MultipleCompatibleArtifacts,
    SpawnCompiler(io::Error),
    MissingCompilerInput,
    WriteCompilerInput(io::Error),
    WaitForCompiler(io::Error),
}

impl fmt::Debug for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CurrentExecutable(source) => formatter
                .debug_tuple("current executable")
                .field(source)
                .finish(),
            Self::MissingDependencyDirectory => formatter.write_str("missing dependency directory"),
            Self::ReadDependencyDirectory(source) => formatter
                .debug_tuple("read dependency directory")
                .field(source)
                .finish(),
            Self::ReadDependencyEntry(source) => formatter
                .debug_tuple("read dependency entry")
                .field(source)
                .finish(),
            Self::MissingCompatibleArtifacts => {
                formatter.write_str("missing compatible rlib artifacts")
            }
            Self::MultipleCompatibleArtifacts => {
                formatter.write_str("multiple compatible rlib artifact pairs")
            }
            Self::SpawnCompiler(source) => formatter
                .debug_tuple("spawn compiler")
                .field(source)
                .finish(),
            Self::MissingCompilerInput => formatter.write_str("missing compiler input"),
            Self::WriteCompilerInput(source) => formatter
                .debug_tuple("write compiler input")
                .field(source)
                .finish(),
            Self::WaitForCompiler(source) => formatter
                .debug_tuple("wait for compiler")
                .field(source)
                .finish(),
        }
    }
}

fn is_rlib(path: &Path, crate_name: &str) -> bool {
    let prefix = format!("lib{crate_name}-");
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".rlib"))
}

fn compatible_artifacts(directory: &Path) -> Result<(PathBuf, PathBuf), ProbeError> {
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
            let probe = compile(
                b"use nudox_ir_format::{EntityRecord, PreparedFragment}; use nudox_ir_vocab::TypeId; fn compatible() { let entities = [EntityRecord { semantic_type: TypeId::new(0) }]; let _ = PreparedFragment::prepare(&entities, &[]); }",
                directory,
                &format_path,
                &vocab_path,
            )?;
            if !probe.status.success() {
                continue;
            }
            if compatible.is_some() {
                return Err(ProbeError::MultipleCompatibleArtifacts);
            }
            compatible = Some((format_path.clone(), vocab_path));
        }
    }
    compatible.ok_or(ProbeError::MissingCompatibleArtifacts)
}

fn compile(
    source: &[u8],
    dependencies: &Path,
    format_artifact: &Path,
    vocab_artifact: &Path,
) -> Result<Output, ProbeError> {
    let mut format_extern = OsString::from("nudox_ir_format=");
    format_extern.push(format_artifact);
    let mut vocab_extern = OsString::from("nudox_ir_vocab=");
    vocab_extern.push(vocab_artifact);
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
    let (format_artifact, vocab_artifact) = compatible_artifacts(dependencies)?;

    let private_view = compile(
        b"use nudox_ir_format::FragmentView; const BAD: FragmentView<'static> = FragmentView { envelope: &[], entities: &[], type_nodes: &[] };",
        dependencies,
        &format_artifact,
        &vocab_artifact,
    )?;
    assert_rejected(
        &private_view,
        "error[E0451]",
        &["envelope", "entities", "type_nodes"],
    );

    let validator_only = compile(
        b"use nudox_ir_format::FragmentView; fn bad() { let _: FragmentView<'_> = (&[][..]).into(); }",
        dependencies,
        &format_artifact,
        &vocab_artifact,
    )?;
    assert_rejected(&validator_only, "error[E0277]", &["From", "FragmentView"]);

    let wrong_coordinate = compile(
        b"use nudox_ir_format::PreparedFragment; use nudox_ir_vocab::TypeId; fn bad() { let _ = PreparedFragment::prepare(&[TypeId::new(0)], &[]); }",
        dependencies,
        &format_artifact,
        &vocab_artifact,
    )?;
    assert_rejected(&wrong_coordinate, "error[E0308]", &["Entity", "Type"]);

    let wrong_edge = compile(
        b"use nudox_ir_format::TypeNode; use nudox_ir_vocab::EntityId; fn bad() { let _ = TypeNode::Reference(EntityId::new(0)); }",
        dependencies,
        &format_artifact,
        &vocab_artifact,
    )?;
    assert_rejected(&wrong_edge, "error[E0308]", &["Entity", "Type"]);

    let escaped_facts = compile(
        b"use nudox_ir_format::{EntityRecord, PreparedFragment, TypeNode}; use nudox_ir_vocab::TypeId; fn bad() -> usize { let prepared; { let entities = [EntityRecord { semantic_type: TypeId::new(0) }]; let nodes = [TypeNode::Reference(TypeId::new(0))]; prepared = match PreparedFragment::prepare(&entities, &nodes) { Ok(value) => value, Err(_) => return 0 }; } prepared.output_len() }",
        dependencies,
        &format_artifact,
        &vocab_artifact,
    )?;
    assert_rejected(&escaped_facts, "error[E0597]", &["entities", "nodes"]);

    let legal = compile(
        b"use nudox_ir_format::{EntityRecord, FragmentView, PreparedFragment, PrimitiveType, TypeNode}; use nudox_ir_vocab::TypeId; fn legal(output: &mut [u8]) -> usize { let entities = [EntityRecord { semantic_type: TypeId::new(0) }]; let nodes = [TypeNode::Primitive(PrimitiveType::Bool)]; let prepared = match PreparedFragment::prepare(&entities, &nodes) { Ok(value) => value, Err(_) => return 0 }; let bytes = match prepared.write_into(output) { Ok(value) => value, Err(_) => return 0 }; match FragmentView::validate(bytes) { Ok(view) => view.entities().count() + view.type_nodes().count(), Err(_) => 0 } }",
        dependencies,
        &format_artifact,
        &vocab_artifact,
    )?;
    assert!(
        legal.status.success(),
        "legal consumer rejected: {}",
        String::from_utf8_lossy(&legal.stderr)
    );
    Ok(())
}
