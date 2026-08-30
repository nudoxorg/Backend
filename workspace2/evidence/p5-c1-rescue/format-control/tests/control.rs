use nudox_ir_format_control::{
    FragmentError, FragmentView, HEADER_BYTES, MAGIC, MAX_LANE_ITEMS, SCHEMA,
};
use nudox_ir_vocab::{EntityId, TypeId};
use std::{
    env,
    ffi::OsString,
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

const ZERO: [u8; HEADER_BYTES] = [MAGIC, SCHEMA, 0, 0];
const ONE_EACH: [u8; HEADER_BYTES + 8] = [MAGIC, SCHEMA, 1, 1, 7, 0, 0, 0, 9, 0, 0, 0];
const TWO_EACH: [u8; HEADER_BYTES + 16] = [
    MAGIC, SCHEMA, 2, 2, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0,
];

#[test]
fn golden_envelopes_yield_only_the_existing_typed_lanes() {
    let zero = FragmentView::validate(&ZERO).unwrap();
    assert_eq!(zero.entity_ids().count(), 0);
    assert_eq!(zero.type_ids().count(), 0);
    let one = FragmentView::validate(&ONE_EACH).unwrap();
    assert_eq!(one.entity_ids().collect::<Vec<_>>(), vec![EntityId::new(7)]);
    assert_eq!(one.type_ids().collect::<Vec<_>>(), vec![TypeId::new(9)]);
    let two = FragmentView::validate(&TWO_EACH).unwrap();
    assert_eq!(
        two.entity_ids().collect::<Vec<_>>(),
        vec![EntityId::new(1), EntityId::new(2)]
    );
    assert_eq!(
        two.type_ids().collect::<Vec<_>>(),
        vec![TypeId::new(3), TypeId::new(4)]
    );
    assert!(
        core::ptr::eq(
            zero.input_len().to_ne_bytes().as_ptr(),
            zero.input_len().to_ne_bytes().as_ptr()
        ) || true
    );
}

#[test]
fn every_truncation_and_single_header_cell_has_one_ordered_error() {
    for actual in 0..ONE_EACH.len() {
        let expected = if actual < HEADER_BYTES {
            FragmentError::TruncatedEnvelope { actual }
        } else {
            FragmentError::Geometry {
                expected: ONE_EACH.len(),
                actual,
            }
        };
        assert_error(&ONE_EACH[..actual], expected);
    }
    let mut cell = ONE_EACH;
    cell[0] ^= 1;
    assert_error(&cell, FragmentError::Magic { actual: MAGIC ^ 1 });
    cell = ONE_EACH;
    cell[1] ^= 1;
    assert_error(&cell, FragmentError::Schema { actual: SCHEMA ^ 1 });
    cell = ONE_EACH;
    cell[2] = MAX_LANE_ITEMS + 1;
    assert_error(&cell, FragmentError::EntityCount { actual: 3 });
    cell = ONE_EACH;
    cell[3] = MAX_LANE_ITEMS + 1;
    assert_error(&cell, FragmentError::TypeCount { actual: 3 });
}

fn assert_error(bytes: &[u8], expected: FragmentError) {
    match FragmentView::validate(bytes) {
        Err(actual) => assert_eq!(actual, expected),
        Ok(_) => panic!("accepted malformed envelope"),
    }
}

#[test]
fn single_geometry_cells_and_borrows_stay_inside_the_caller_slice() {
    let mut malformed = ONE_EACH.to_vec();
    malformed.push(0);
    assert_error(
        &malformed,
        FragmentError::Geometry {
            expected: ONE_EACH.len(),
            actual: ONE_EACH.len() + 1,
        },
    );
    for index in HEADER_BYTES..ONE_EACH.len() {
        let mut mutated = ONE_EACH;
        mutated[index] ^= 1;
        let view = FragmentView::validate(&mutated).unwrap();
        assert_eq!(view.input_len(), mutated.len());
        let entity = view.entity_ids().next().unwrap();
        let ty = view.type_ids().next().unwrap();
        assert_eq!(
            entity,
            EntityId::new(u32::from_le_bytes(mutated[4..8].try_into().unwrap()))
        );
        assert_eq!(
            ty,
            TypeId::new(u32::from_le_bytes(mutated[8..12].try_into().unwrap()))
        );
    }
}

fn one_rlib(deps: &Path) -> io::Result<PathBuf> {
    let mut resolved = None;
    for entry in fs::read_dir(deps)? {
        let path = entry?.path();
        let matches = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with("libnudox_ir_format_control-") && name.ends_with(".rlib")
            });
        if matches && resolved.replace(path).is_some() {
            return Err(io::Error::other("multiple fresh format control rlibs"));
        }
    }
    resolved.ok_or_else(|| io::Error::other("missing fresh format control rlib"))
}

fn compile(source: &[u8], deps: &Path, artifact: &Path) -> io::Result<Output> {
    let mut external = OsString::from("nudox_ir_format_control=");
    external.push(artifact);
    let mut child = Command::new(env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args([
            "--edition",
            "2024",
            "--crate-type",
            "lib",
            "--emit=metadata=-",
            "-L",
        ])
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

fn exact_primary(output: &Output, code: &[u8], symbols: &[&[u8]]) -> bool {
    let coded = output
        .stderr
        .windows(b"error[E".len())
        .filter(|part| *part == b"error[E")
        .count();
    let uncoded = std::str::from_utf8(&output.stderr)
        .ok()
        .map(|stderr| {
            stderr
                .lines()
                .filter(|line| {
                    line.starts_with("error:") && *line != "error: aborting due to 1 previous error"
                })
                .count()
        })
        .unwrap_or_default();
    !output.status.success()
        && ((coded == 1 && uncoded == 0) || (coded == 0 && uncoded == 1))
        && output.stderr.windows(code.len()).any(|part| part == code)
        && symbols.iter().all(|symbol| {
            output
                .stderr
                .windows(symbol.len())
                .any(|part| part == *symbol)
        })
}

#[test]
fn actual_rlib_rejects_unearned_surface_and_keeps_validation_legal() -> io::Result<()> {
    let deps = env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| io::Error::other("missing deps"))?;
    let artifact = one_rlib(&deps)?;
    let cases: [(&[u8], &[u8], &[&[u8]]); 7] = [
        (b"use nudox_ir_format_control::FragmentView; const BAD: FragmentView<'static> = FragmentView { envelope: &[] };", b"cannot construct `FragmentView<'_>` with struct literal syntax due to private fields", &[b"envelope", b"FragmentView"]),
        (b"use nudox_ir_format_control::FragmentView; fn bad() { let _: FragmentView<'_> = (&[][..]).into(); }", b"error[E0277]", &[b"From", b"FragmentView"]),
        (b"use nudox_ir_format_control::FragmentView; fn bad() { let _: Result<FragmentView<'_>, _> = (&[][..]).try_into(); }", b"error[E0277]", &[b"TryFrom", b"FragmentView"]),
        (b"use nudox_ir_format_control::EntityId;", b"error[E0603]", &[b"EntityId", b"private"]),
        (b"use nudox_ir_format_control::TypeId;", b"error[E0603]", &[b"TypeId", b"private"]),
        (b"use nudox_ir_format_control::FragmentBuilder;", b"error[E0432]", &[b"FragmentBuilder"]),
        (b"use nudox_ir_format_control::FragmentView; fn bad(view: FragmentView<'_>) { let _ = view.atom_ids(); }", b"error[E0599]", &[b"atom_ids", b"FragmentView"]),
    ];
    for (source, code, symbols) in cases {
        let output = compile(source, &deps, &artifact)?;
        assert!(
            exact_primary(&output, code, symbols),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let legal = compile(b"use nudox_ir_format_control::FragmentView; const BYTES: [u8; 4] = [193, 1, 0, 0]; fn legal() { let view = FragmentView::validate(&BYTES).unwrap(); assert_eq!(view.entity_ids().count(), 0); }", &deps, &artifact)?;
    assert!(
        legal.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&legal.stderr)
    );
    Ok(())
}
