//! Runs the vendored `JavacTask` producer over a fixture that exercises every
//! widened resolved-use class — field read, field write, type use, enum
//! constant use, and constructor call — and proves each recorded row's closed
//! tag, atom-keyed target identity, and exact written name-token extent. The
//! invocation plane's rows keep their byte-compatible encoding beside them.

use std::{
    env,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use backend_frontend_java::legacy::{
    BoundImageError, DeclarationKind, ImageError, JavaAuthorityImage, UseTag,
    harness::{Harness, HarnessRequest, JavaSource, JdkToolchain},
};

#[derive(Debug, thiserror::Error)]
enum UsesTestError {
    #[error("NUDOX_JDK must name the pinned JDK used for Java authority integration tests")]
    MissingJdk,
    #[error(transparent)]
    Harness(#[from] backend_frontend_java::legacy::harness::HarnessError),
    #[error(transparent)]
    Bound(#[from] BoundImageError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error("expected image fact `{fact}`")]
    Missing { fact: &'static str },
    #[error("expected `{expected}`, found `{actual}`")]
    Text { expected: String, actual: String },
    #[error("unexpected use row at bytes {start}..{end}")]
    Unexpected { start: u32, end: u32 },
}

/// One fixture source exercising every widened resolved-use class.
const SOURCE: &str = "package demo;\n\nclass Wheel {\n\tint turns;\n\tvoid spin() {\n\t\tturns = turns + 1;\n\t}\n}\n\nenum Gear {\n\tLOW\n}\n\nclass Bike {\n\tWheel wheel = new Wheel();\n\tGear gear = Gear.LOW;\n\tvoid ride() {\n\t\twheel.spin();\n\t\twheel.turns = 2;\n\t}\n}\n";

#[test]
fn harness_records_every_widened_use_class_with_honest_kinds_and_extents()
-> Result<(), UsesTestError> {
    let jdk = PathBuf::from(env::var_os("NUDOX_JDK").ok_or(UsesTestError::MissingJdk)?);
    let toolchain = JdkToolchain::new(&jdk)?;
    let mut harness = Harness::new()?;
    harness.prepare(&toolchain)?;
    let sources = [JavaSource {
        name: Path::new("demo/Uses.java"),
        bytes: SOURCE.as_bytes(),
    }];
    let mut image_bytes = Vec::new();
    harness.image(
        &toolchain,
        HarnessRequest {
            sources: &sources,
            classpath: &[],
            release: backend_frontend_java::legacy::JavaRelease::Java21,
        },
        &mut image_bytes,
    )?;
    let image = JavaAuthorityImage::open(&image_bytes)?.image;

    // The invocation plane keeps its byte-compatible rows: one resolved
    // call, owned by `ride`, at the exact `spin` name-token extent.
    let references: Vec<_> = image.references().collect::<Result<Vec<_>, _>>()?;
    if references.len() != 1 {
        return Err(UsesTestError::Missing {
            fact: "one invocation-plane row",
        });
    }
    let invocation = &references[0];
    let spin_at = SOURCE
        .find("spin();")
        .ok_or(UsesTestError::Missing { fact: "spin call" })?;
    let spin_at = u32::try_from(spin_at).map_err(|_| UsesTestError::Missing {
        fact: "spin extent",
    })?;
    if invocation.start != spin_at || invocation.end != spin_at + 4 {
        return Err(UsesTestError::Unexpected {
            start: invocation.start,
            end: invocation.end,
        });
    }
    let invocation_owner = image.symbol(invocation.owner)?;
    assert_atom(invocation_owner.name, "ride")?;
    let invocation_target = image.symbol(invocation.target)?;
    assert_atom(invocation_target.name, "spin")?;

    // Every widened class carries its exact extent and target identity.
    let uses: Vec<_> = image.uses().collect::<Result<Vec<_>, _>>()?;
    let with_tag = |tag: UseTag| -> Vec<&backend_frontend_java::legacy::ResolvedUse<'_>> {
        uses.iter().filter(|row| row.kind == tag).collect()
    };
    let assert_extent = |row: &backend_frontend_java::legacy::ResolvedUse<'_>,
                         needle: &str,
                         offset: usize,
                         width: u32|
     -> Result<(), UsesTestError> {
        let at = SOURCE.find(needle).ok_or(UsesTestError::Missing {
            fact: "expected use extent",
        })?;
        let start = u32::try_from(at + offset).map_err(|_| UsesTestError::Missing {
            fact: "extent start",
        })?;
        if (row.start, row.end) != (start, start + width) {
            return Err(UsesTestError::Unexpected {
                start: row.start,
                end: row.end,
            });
        }
        Ok(())
    };

    // Field reads: the `turns` read of `turns = turns + 1`, then the
    // `wheel` receivers of `wheel.spin()` and `wheel.turns = 2`.
    let reads = with_tag(UseTag::FieldRead);
    if reads.len() != 3 {
        return Err(UsesTestError::Missing {
            fact: "three field reads",
        });
    }
    assert_extent(reads[0], "turns = turns + 1", 8, 5)?;
    assert_atom(reads[0].declaring, "demo.Wheel")?;
    assert_atom(
        reads[0].name.ok_or(UsesTestError::Missing {
            fact: "field read name",
        })?,
        "turns",
    )?;
    assert_extent(reads[1], "wheel.spin()", 0, 5)?;
    assert_extent(reads[2], "wheel.turns = 2", 0, 5)?;

    // Field writes: the assignment targets of `turns = turns + 1` and
    // `wheel.turns = 2`.
    let writes = with_tag(UseTag::FieldWrite);
    if writes.len() != 2 {
        return Err(UsesTestError::Missing {
            fact: "two field writes",
        });
    }
    assert_extent(writes[0], "turns = turns", 0, 5)?;
    assert_atom(writes[0].declaring, "demo.Wheel")?;
    assert_extent(writes[1], "wheel.turns = 2", 6, 5)?;
    assert_atom(writes[1].declaring, "demo.Wheel")?;
    assert_atom(
        writes[1].name.ok_or(UsesTestError::Missing {
            fact: "field write name",
        })?,
        "turns",
    )?;

    // Type uses: the `Wheel` field type, `Gear` in `Gear gear`, and `Gear`
    // in `Gear.LOW`.
    let type_uses = with_tag(UseTag::TypeUse);
    if type_uses.len() != 3 {
        return Err(UsesTestError::Missing {
            fact: "three type uses",
        });
    }
    assert_extent(type_uses[0], "Wheel wheel", 0, 5)?;
    assert_atom(type_uses[0].declaring, "demo.Wheel")?;
    if type_uses[0].name.is_some() {
        return Err(UsesTestError::Missing {
            fact: "type use has no name atom",
        });
    }
    assert_extent(type_uses[1], "Gear gear", 0, 4)?;
    assert_extent(type_uses[2], "Gear.LOW", 0, 4)?;

    // Enum constant use: `LOW` of `Gear.LOW`.
    let enum_uses = with_tag(UseTag::EnumConstantUse);
    if enum_uses.len() != 1 {
        return Err(UsesTestError::Missing {
            fact: "one enum constant use",
        });
    }
    assert_extent(enum_uses[0], "Gear.LOW", 5, 3)?;
    assert_atom(enum_uses[0].declaring, "demo.Gear")?;
    assert_atom(
        enum_uses[0].name.ok_or(UsesTestError::Missing {
            fact: "enum use name",
        })?,
        "LOW",
    )?;

    // Constructor call: `new Wheel()` targets Wheel's resolved constructor
    // through the invocation plane's own symbol keying.
    let constructors = with_tag(UseTag::ConstructorCall);
    if constructors.len() != 1 {
        return Err(UsesTestError::Missing {
            fact: "one constructor call",
        });
    }
    let constructor = constructors[0];
    assert_extent(constructor, "new Wheel()", 4, 5)?;
    let target = constructor.target.ok_or(UsesTestError::Missing {
        fact: "constructor target",
    })?;
    let resolved = image.symbol(target)?;
    assert_atom(resolved.owner, "demo.Wheel")?;

    // Declaration extents: every declaration whose row comes from a written
    // tree — including the package declaration of the binding unit —
    // carries an ordered extent inside the source bytes. Mandated or
    // synthetic member rows (default constructors, enum bridge methods)
    // have no written tree and stay explicitly absent.
    let mut positioned = 0_usize;
    for (ordinal, declaration) in image.declarations().enumerate() {
        let declaration = declaration?;
        let extent = image.declaration_extent(ordinal)?;
        if !extent.present() {
            continue;
        }
        let start = usize::try_from(extent.start.ok_or(UsesTestError::Missing {
            fact: "extent start",
        })?)
        .map_err(|_| UsesTestError::Missing {
            fact: "extent start width",
        })?;
        let end = usize::try_from(
            extent
                .end
                .ok_or(UsesTestError::Missing { fact: "extent end" })?,
        )
        .map_err(|_| UsesTestError::Missing {
            fact: "extent end width",
        })?;
        if SOURCE.get(start..end).is_none() || start >= end {
            return Err(UsesTestError::Missing {
                fact: "extent inside the bound source",
            });
        }
        positioned += 1;
    }
    // Wheel, its field, spin; Gear, LOW; Bike, its two fields, ride; the
    // package declaration of the binding unit.
    if positioned < 10 {
        return Err(UsesTestError::Missing {
            fact: "ten source-written declarations positioned",
        });
    }
    Ok(())
}

fn assert_atom(
    atom: backend_frontend_java::legacy::Atom<'_>,
    expected: &str,
) -> Result<(), UsesTestError> {
    let actual = atom.utf8().map_err(|_| UsesTestError::Text {
        expected: expected.to_owned(),
        actual: String::from("not utf-8"),
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(UsesTestError::Text {
            expected: expected.to_owned(),
            actual: String::from(actual),
        })
    }
}
