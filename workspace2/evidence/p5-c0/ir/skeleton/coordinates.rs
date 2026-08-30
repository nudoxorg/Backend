use core::mem::{align_of, size_of};
use nudox_ir_vocab::{DenseId, Entity, EntityId, TypeId};
use std::{
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const RAW_POSITION: u32 = 7;
const CONST_ENTITY: EntityId = EntityId::new(RAW_POSITION);
const MISMATCH: &[u8] = b"use nudox_ir_vocab::{EntityId, TypeId}; fn need_type(_: TypeId) {} fn main() { need_type(EntityId::new(7)); }";
const LEGAL: &[u8] = b"use nudox_ir_vocab::{EntityId, TypeId}; fn need_type(_: TypeId) {} fn main() { need_type(TypeId::new(7)); }";

#[test]
fn coordinate_layout_and_const_construction_are_exact() {
    assert_eq!(size_of::<DenseId<Entity>>(), 4);
    assert_eq!(align_of::<DenseId<Entity>>(), 4);
    assert_eq!(size_of::<EntityId>(), 4);
    assert_eq!(align_of::<EntityId>(), 4);
    assert_eq!(size_of::<TypeId>(), 4);
    assert_eq!(align_of::<TypeId>(), 4);
    assert_eq!(CONST_ENTITY, EntityId::new(RAW_POSITION));
}

fn rlib(deps: &Path) -> io::Result<PathBuf> {
    let mut resolved = None;
    for entry in fs::read_dir(deps)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("libnudox_ir_vocab-")
            && name.to_string_lossy().ends_with(".rlib")
            && resolved.replace(entry.path()).is_some()
        {
            return Err(io::Error::other("multiple nudox ir vocab rlibs"));
        }
    }
    resolved.ok_or_else(|| io::Error::other("nudox ir vocab rlib"))
}

fn compile(source: &[u8], deps: &Path, artifact: &Path) -> io::Result<Output> {
    let mut external = OsString::from("nudox_ir_vocab=");
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
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing rustc stdin"))?;
    input.write_all(source)?;
    drop(input);
    child.wait_with_output()
}

fn occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|part| *part == needle)
        .count()
}

fn mismatch(output: &Output) -> bool {
    let stderr = &output.stderr;
    !output.status.success()
        && occurrences(stderr, b"error[E") == 1
        && occurrences(stderr, b"error[E0308]") > 0
        && occurrences(stderr, b"EntityId") > 0
        && occurrences(stderr, b"TypeId") > 0
}

#[test]
fn entity_coordinate_cannot_fill_type_coordinate() -> io::Result<()> {
    let deps = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("missing test executable parent"))?;
    let artifact = rlib(&deps)?;
    let rejected = compile(MISMATCH, &deps, &artifact)?;
    let legal = compile(LEGAL, &deps, &artifact)?;
    assert!(mismatch(&rejected));
    assert!(legal.status.success());
    assert!(!mismatch(&legal));
    Ok(())
}
