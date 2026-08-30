use std::{
    ffi::OsString,
    fs,
    io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const FORGED_VIEW: &[u8] = b"use nudox_ir_format::FragmentView; fn main() { let _ = FragmentView { bytes: &[], entity: 0..0, ty: 0..0 }; }";
const RAW_INTO: &[u8] = b"use nudox_ir_format::FragmentView; fn main() { let _: FragmentView<'_> = [0_u8; 32].into(); }";
const LEGAL_VALIDATE: &[u8] = b"use nudox_ir_format::FragmentView; fn main() { let bytes = []; let _ = FragmentView::validate(&bytes); }";
const FORBIDDEN_SURFACE: &[u8] = b"use nudox_ir_format::{EntityId, PreparedFragment, ThirdLane}; fn main() {}";

fn sole_rlib(deps: &Path) -> io::Result<PathBuf> {
    let mut found = None;
    for entry in fs::read_dir(deps)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text.starts_with("libnudox_ir_format-") && text.ends_with(".rlib") {
            if found.replace(entry.path()).is_some() {
                return Err(io::Error::other("multiple nudox ir format rlibs"));
            }
        }
    }
    found.ok_or_else(|| io::Error::other("missing nudox ir format rlib"))
}

fn compile(source: &[u8], deps: &Path, artifact: &Path) -> io::Result<Output> {
    let mut external = OsString::from("nudox_ir_format=");
    external.push(artifact);
    let mut child = Command::new("rustc")
        .args(["--edition", "2024", "--emit=metadata=-", "-L"])
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
    child.wait_with_output()
}

fn has_only_error(stderr: &[u8], code: &[u8]) -> bool {
    stderr.windows(b"error[E".len()).filter(|part| *part == b"error[E").count() == 1
        && stderr.windows(code.len()).any(|part| part == code)
}

#[test]
fn actual_rlib_fixtures_are_causal() -> io::Result<()> {
    let deps = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("missing executable parent"))?;
    let artifact = sole_rlib(&deps)?;
    let forged = compile(FORGED_VIEW, &deps, &artifact)?;
    let raw = compile(RAW_INTO, &deps, &artifact)?;
    let legal = compile(LEGAL_VALIDATE, &deps, &artifact)?;
    let forbidden = compile(FORBIDDEN_SURFACE, &deps, &artifact)?;
    assert!(!forged.status.success());
    assert!(forged.stderr.windows(b"E0451".len()).any(|part| part == b"E0451"));
    assert!(!raw.status.success() && has_only_error(&raw.stderr, b"E0277"));
    assert!(legal.status.success());
    assert!(!forbidden.status.success() && has_only_error(&forbidden.stderr, b"E0432"));
    Ok(())
}
