use std::{
    collections::hash_map::DefaultHasher,
    ffi::OsString,
    fs,
    hash::{Hash, Hasher},
    io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const FORGED_VIEW: &[u8] = b"use nudox_ir_format::FragmentView; fn main() { let _ = FragmentView { bytes: &[], entity: 0..0, ty: 0..0 }; }";
const RAW_INTO: &[u8] = b"use nudox_ir_format::FragmentView; fn main() { let _: FragmentView<'_> = [0_u8; 32].into(); }";
const LEGAL_NAMED: &[u8] = b"use nudox_ir_format::FragmentView; fn main() { let bytes: &[u8] = &[]; let _ = FragmentView::validate(bytes); }";
const UNEARNED_IMPORTS: &[u8] = b"use nudox_ir_format::{EntityId, PreparedFragment, ThirdLane}; fn main() {}";

struct FixtureOutput {
    source_hash: u64,
    artifact_hash: u64,
    artifact: PathBuf,
    compiler_version: String,
    output: Output,
}

struct SelectedRlib {
    cardinality: usize,
    path: PathBuf,
    hash: u64,
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn sole_rlib(deps: &Path) -> io::Result<SelectedRlib> {
    let mut candidates: [Option<PathBuf>; 2] = [None, None];
    let mut cardinality = 0_usize;
    for entry in fs::read_dir(deps)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text.starts_with("libnudox_ir_format-") && text.ends_with(".rlib") {
            if cardinality == candidates.len() {
                return Err(io::Error::other("multiple nudox ir format rlibs"));
            }
            candidates[cardinality] = Some(entry.path());
            cardinality += 1;
        }
    }
    if cardinality != 1 {
        return Err(io::Error::other(if cardinality == 0 {
            "missing nudox ir format rlib"
        } else {
            "multiple nudox ir format rlibs"
        }));
    }
    let path = candidates[0]
        .take()
        .ok_or_else(|| io::Error::other("missing selected rlib"))?;
    let hash = stable_hash(&fs::read(&path)?);
    Ok(SelectedRlib { cardinality, path, hash })
}

fn invoke_actual_rlib(
    source: &[u8],
    deps: &Path,
    selected: &SelectedRlib,
) -> io::Result<FixtureOutput> {
    let compiler = Command::new("rustc").arg("--version").output()?;
    let compiler_version = String::from_utf8_lossy(&compiler.stdout).into_owned();
    let mut external = OsString::from("nudox_ir_format=");
    external.push(&selected.path);
    let mut child = Command::new("rustc")
        .args(["--crate-name", "rlib_fixture", "--edition", "2024", "--emit=metadata=-", "-L"])
        .arg(deps)
        .arg("--extern")
        .arg(external)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing rustc stdin"))?;
    stdin.write_all(source)?;
    drop(stdin);
    let output = child.wait_with_output()?;
    Ok(FixtureOutput {
        source_hash: stable_hash(source),
        artifact_hash: selected.hash,
        artifact: selected.path.clone(),
        compiler_version,
        output,
    })
}

fn has_code(stderr: &[u8], code: &[u8]) -> bool {
    stderr.windows(code.len()).any(|part| part == code)
}

fn has_only_error(stderr: &[u8], code: &[u8]) -> bool {
    stderr.windows(b"error[E".len()).filter(|part| *part == b"error[E").count() == 1
        && has_code(stderr, code)
}

#[test]
fn actual_rlib_fixtures_retain_causal_compiler_outputs() -> io::Result<()> {
    let deps = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("missing executable parent"))?;
    let selected = sole_rlib(&deps)?;
    assert_eq!(selected.cardinality, 1);
    let forged = invoke_actual_rlib(FORGED_VIEW, &deps, &selected)?;
    let raw = invoke_actual_rlib(RAW_INTO, &deps, &selected)?;
    let legal = invoke_actual_rlib(LEGAL_NAMED, &deps, &selected)?;
    let imports = invoke_actual_rlib(UNEARNED_IMPORTS, &deps, &selected)?;

    assert!(!forged.output.status.success());
    assert!(has_code(&forged.output.stderr, b"E0451"));
    for field in [b"bytes".as_slice(), b"entity", b"ty"] {
        assert!(forged.output.stderr.windows(field.len()).any(|part| part == field));
    }
    assert!(!raw.output.status.success() && has_only_error(&raw.output.stderr, b"E0277"));
    assert!(legal.output.status.success());
    assert!(!imports.output.status.success() && has_only_error(&imports.output.stderr, b"E0432"));
    for name in [b"EntityId".as_slice(), b"PreparedFragment", b"ThirdLane"] {
        assert!(imports.output.stderr.windows(name.len()).any(|part| part == name));
    }
    assert_eq!(raw.source_hash, stable_hash(RAW_INTO));
    assert_eq!(legal.artifact, selected.path);
    assert_eq!(imports.artifact_hash, forged.artifact_hash);
    assert!(!forged.compiler_version.is_empty());
    Ok(())
}
