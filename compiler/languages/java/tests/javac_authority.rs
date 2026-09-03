//! Runs the vendored `JavacTask` producer and opens its emitted image in Rust.
//! It proves named-module overload resolution, Javadoc retention, and diagnostics.
//! The test requires an explicit pinned JDK path and never substitutes a scanner fixture.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
};

use compiler_languages_java::{
    BoundImageError, DeclarationExtension, DeclarationKind, JavaAuthorityImage, JavaImage, TypeKind,
};

static TEMPORARY_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, thiserror::Error)]
enum JavacTestError {
    #[error("NUDOX_JDK must name the pinned JDK used for Java authority integration tests")]
    MissingJdk,
    #[error("could not create Java authority test directory {path:?}: {source}")]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not read the JDK authority image {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not remove successful Java authority test directory {path:?}: {source}")]
    Cleanup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("JDK command `{program}` failed with status {status}: {stderr}")]
    Command {
        program: &'static str,
        status: std::process::ExitStatus,
        stderr: String,
    },
    #[error(transparent)]
    Image(#[from] compiler_languages_java::ImageError),
    #[error(transparent)]
    BoundImage(#[from] BoundImageError),
    #[error("expected image fact `{fact}`")]
    Missing { fact: &'static str },
    #[error("expected `{expected}`, found `{actual}`")]
    Text {
        expected: &'static str,
        actual: String,
    },
    #[error("JDK output was not UTF-8")]
    OutputUtf8,
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn create() -> Result<Self, JavacTestError> {
        for sequence in 0..64 {
            let serial = TEMPORARY_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!(
                "nudox-java-authority-{}-{serial}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(JavacTestError::Directory { path, source }),
            }
        }
        Err(JavacTestError::Missing {
            fact: "unique temporary directory",
        })
    }
}

impl TemporaryDirectory {
    fn remove(self) -> Result<(), JavacTestError> {
        fs::remove_dir_all(&self.path).map_err(|source| JavacTestError::Cleanup {
            path: self.path,
            source,
        })
    }
}

#[test]
fn javac_image_preserves_overload_docs_module_and_diagnostics() -> Result<(), JavacTestError> {
    let jdk = PathBuf::from(env::var_os("NUDOX_JDK").ok_or(JavacTestError::MissingJdk)?);
    let temporary = TemporaryDirectory::create()?;
    let outcome = (|| {
        let classes = temporary.path.join("classes");
        fs::create_dir(&classes).map_err(|source| JavacTestError::Directory {
            path: classes.clone(),
            source,
        })?;
        compile_producer(&jdk, &classes)?;

        let first = run_producer(&jdk, &classes, &temporary.path, "Cafe.java")?;
        let first_bytes = read_image(&first)?;
        let first_image = JavaAuthorityImage::open(&first_bytes)?.image;
        let first_target = call_target(first_image)?;
        assert_atom(first_target.owner, "demo.Helper")?;
        assert_atom(first_target.name, "render")?;
        assert_parameter(first_image, first_target.parameters, "int")?;
        let documented = documented_declaration(first_image)?;
        if documented.documentation_flavor != compiler_languages_java::DocFlavor::Traditional {
            return Err(JavacTestError::Missing {
                fact: "traditional Javadoc flavor",
            });
        }
        assert_atom(
            documented.documentation.ok_or(JavacTestError::Missing {
                fact: "Javadoc atom",
            })?,
            "A named module keeps package and cross-file authority explicit.",
        )?;
        assert_v2_extensions(first_image)?;

        let second = run_producer(&jdk, &classes, &temporary.path, "CafeChanged.java")?;
        let second_bytes = read_image(&second)?;
        let second_image = JavaAuthorityImage::open(&second_bytes)?.image;
        let second_target = call_target(second_image)?;
        assert_parameter(second_image, second_target.parameters, "java.lang.String")?;

        let broken = fixture("rejected/Broken.java");
        let output = Command::new(jdk.join("bin/java"))
            .args([
                OsString::from("--add-modules"),
                OsString::from("jdk.compiler,jdk.javadoc"),
            ])
            .args([OsString::from("-cp"), classes.into_os_string()])
            .arg("nudox.oracle.CompilerExtractor")
            .args(["--release", "21", "--outfile"])
            .arg(temporary.path.join("rejected.image"))
            .args(["--source-binding"])
            .arg(&broken)
            .arg(broken)
            .output()
            .map_err(|source| JavacTestError::Directory {
                path: jdk.join("bin/java"),
                source,
            })?;
        if output.status.success() {
            return Err(JavacTestError::Missing {
                fact: "javac diagnostic rejection",
            });
        }
        let stderr = String::from_utf8(output.stderr).map_err(|_| JavacTestError::OutputUtf8)?;
        for diagnostic in ["missingOne", "missingTwo"] {
            if !stderr.contains(diagnostic) {
                return Err(JavacTestError::Missing { fact: diagnostic });
            }
        }
        Ok(())
    })();
    outcome?;
    temporary.remove()
}

fn assert_v2_extensions(image: JavaImage<'_>) -> Result<(), JavacTestError> {
    let mut audited = None;
    let mut pair = None;
    for (ordinal, declaration) in image.declarations().enumerate() {
        let declaration = declaration?;
        let name = declaration
            .name
            .utf8()
            .map_err(|_| JavacTestError::OutputUtf8)?;
        if name == "audited" {
            audited = Some((ordinal, declaration));
        } else if declaration.kind == DeclarationKind::Record {
            pair = Some((ordinal, declaration));
        }
    }
    let (audited_ordinal, _) = audited.ok_or(JavacTestError::Missing {
        fact: "throws method",
    })?;
    let mut extensions = image.declaration_extensions(audited_ordinal)?;
    let annotation = extensions.next().ok_or(JavacTestError::Missing {
        fact: "Deprecated annotation",
    })??;
    match annotation {
        DeclarationExtension::Annotation(atom) => assert_atom(atom, "@java.lang.Deprecated")?,
        _ => {
            return Err(JavacTestError::Missing {
                fact: "annotation extension kind",
            });
        }
    }
    let throws = extensions.next().ok_or(JavacTestError::Missing {
        fact: "IOException throws extension",
    })??;
    match throws {
        DeclarationExtension::Throws(reference) => assert_atom(
            image
                .type_fact(reference)?
                .spelling
                .ok_or(JavacTestError::Missing {
                    fact: "IOException spelling",
                })?,
            "java.io.IOException",
        )?,
        _ => {
            return Err(JavacTestError::Missing {
                fact: "throws extension kind",
            });
        }
    }
    if extensions.next().is_some() {
        return Err(JavacTestError::Missing {
            fact: "exact method extension count",
        });
    }

    let (pair_ordinal, pair_declaration) = pair.ok_or(JavacTestError::Missing {
        fact: "record declaration",
    })?;
    if pair_declaration.kind != DeclarationKind::Record {
        return Err(JavacTestError::Missing {
            fact: "record declaration kind",
        });
    }
    let components = image
        .declaration_extensions(pair_ordinal)?
        .collect::<Result<Vec<_>, _>>()?;
    if components.len() != 2 {
        return Err(JavacTestError::Missing {
            fact: "two record components",
        });
    }
    for (extension, expected) in components.into_iter().zip(["left", "right"]) {
        let DeclarationExtension::RecordComponent(ordinal) = extension else {
            return Err(JavacTestError::Missing {
                fact: "record component extension kind",
            });
        };
        let declaration = image
            .declarations()
            .nth(ordinal)
            .ok_or(JavacTestError::Missing {
                fact: "record component declaration",
            })??;
        if declaration.kind != DeclarationKind::Field {
            return Err(JavacTestError::Missing {
                fact: "record component field kind",
            });
        }
        assert_atom(declaration.name, expected)?;
    }
    Ok(())
}

fn compile_producer(jdk: &Path, classes: &Path) -> Result<(), JavacTestError> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = [
        root.join("doclet/AuthorityImage.java"),
        root.join("doclet/CompilerExtractor.java"),
    ];
    command(
        Command::new(jdk.join("bin/javac"))
            .args([
                "--release",
                "21",
                "--add-modules",
                "jdk.compiler,jdk.javadoc",
                "-d",
            ])
            .arg(classes)
            .args(files),
        "javac",
    )
}

fn run_producer(
    jdk: &Path,
    classes: &Path,
    temporary: &Path,
    cafe: &str,
) -> Result<PathBuf, JavacTestError> {
    let output = temporary.join(cafe.replace(".java", ".image"));
    command(
        Command::new(jdk.join("bin/java"))
            .args(["--add-modules", "jdk.compiler,jdk.javadoc", "-cp"])
            .arg(classes)
            .arg("nudox.oracle.CompilerExtractor")
            .args(["--release", "21", "--outfile"])
            .arg(&output)
            .arg("--source-binding")
            .arg(fixture(&format!("src/demo/{cafe}")))
            .arg(fixture("src/module-info.java"))
            .arg(fixture("src/demo/Helper.java"))
            .arg(fixture(&format!("src/demo/{cafe}"))),
        "java",
    )?;
    Ok(output)
}

fn command(command: &mut Command, program: &'static str) -> Result<(), JavacTestError> {
    let output = command
        .output()
        .map_err(|source| JavacTestError::Directory {
            path: PathBuf::from(program),
            source,
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(program, output)?)
    }
}

fn command_failure(
    program: &'static str,
    output: Output,
) -> Result<JavacTestError, JavacTestError> {
    Ok(JavacTestError::Command {
        program,
        status: output.status,
        stderr: String::from_utf8(output.stderr).map_err(|_| JavacTestError::OutputUtf8)?,
    })
}

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/authority")
        .join(relative)
}

fn read_image(path: &Path) -> Result<Vec<u8>, JavacTestError> {
    fs::read(path).map_err(|source| JavacTestError::Read {
        path: path.to_owned(),
        source,
    })
}

fn documented_declaration(
    image: JavaImage<'_>,
) -> Result<compiler_languages_java::Declaration<'_>, JavacTestError> {
    for declaration in image.declarations() {
        let declaration = declaration?;
        if declaration.documentation.is_some() {
            return Ok(declaration);
        }
    }
    Err(JavacTestError::Missing {
        fact: "Javadoc declaration",
    })
}

fn call_target(
    image: JavaImage<'_>,
) -> Result<compiler_languages_java::Symbol<'_>, JavacTestError> {
    let reference = image
        .references()
        .next()
        .transpose()?
        .ok_or(JavacTestError::Missing {
            fact: "resolved cross-file call",
        })?;
    image.symbol(reference.target).map_err(Into::into)
}

fn assert_parameter(
    image: JavaImage<'_>,
    mut parameters: compiler_languages_java::TypeChildren<'_>,
    expected: &'static str,
) -> Result<(), JavacTestError> {
    let reference = parameters.next().ok_or(JavacTestError::Missing {
        fact: "overload parameter",
    })?;
    if parameters.next().is_some() {
        return Err(JavacTestError::Missing {
            fact: "single overload parameter",
        });
    }
    let parameter = image.type_fact(reference)?;
    if parameter.kind != TypeKind::Primitive && parameter.kind != TypeKind::Declared {
        return Err(JavacTestError::Missing {
            fact: "resolved parameter type kind",
        });
    }
    assert_atom(
        parameter.spelling.ok_or(JavacTestError::Missing {
            fact: "parameter spelling",
        })?,
        expected,
    )
}

fn assert_atom(
    atom: compiler_languages_java::Atom<'_>,
    expected: &'static str,
) -> Result<(), JavacTestError> {
    let actual = atom.utf8().map_err(|_| JavacTestError::OutputUtf8)?;
    if actual == expected {
        Ok(())
    } else {
        Err(JavacTestError::Text {
            expected,
            actual: String::from(actual),
        })
    }
}
