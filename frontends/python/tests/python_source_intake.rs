use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    NativePythonProjectAuthority, PythonProjectBytesSource, PythonProjectControl,
    PythonProjectCoverageGapKind, PythonProjectSourceStatus, PythonSourceDecodeFault,
    SymbolOutcome,
};
use backend_semantic::vocabulary::PythonVersion;

#[test]
fn raw_frontier_retains_per_file_syntax_and_codec_refusals_without_poisoning_valid_answers()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-per-file-intake-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let good = b"def answer() -> int:\n    return 1\n";
    let bad = b"def poison() -> int:\n    return 1\nx y\n";
    let latin = b"# coding: latin-1\nvalue = 'caf\xe9'\n";
    let caller = b"from good import answer\nanswer()\n";
    let dependent = b"from bad import poison\nvalue = poison()\n";
    let raw_dependent = b"import latin\nvalue = latin.value\n";
    let sources = [
        PythonProjectBytesSource {
            relative_path: "caller.py",
            source: caller,
        },
        PythonProjectBytesSource {
            relative_path: "good.py",
            source: good,
        },
        PythonProjectBytesSource {
            relative_path: "dependent.py",
            source: dependent,
        },
        PythonProjectBytesSource {
            relative_path: "bad.pyi",
            source: bad,
        },
        PythonProjectBytesSource {
            relative_path: "latin.py",
            source: latin,
        },
        PythonProjectBytesSource {
            relative_path: "raw_dependent.py",
            source: raw_dependent,
        },
    ];
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source)?;
    }
    let checker = NativePythonProjectAuthority::admit()?;
    let cancelled = AtomicBool::new(false);
    let control = || PythonProjectControl {
        cancelled: &cancelled,
        deadline: Instant::now() + Duration::from_secs(30),
    };
    for _ in 0..2 {
        let report = checker.analyze_project_bytes(
            &root,
            "pkg",
            &sources,
            PythonVersion::Python314,
            control(),
        )?;
        for source in &sources {
            assert!(report.module(source.relative_path).is_some());
        }
        assert_eq!(
            report.source_status("good.py"),
            Some(&PythonProjectSourceStatus::Analyzed)
        );
        assert_eq!(
            report.source_status("bad.pyi"),
            Some(&PythonProjectSourceStatus::UnavailableSyntax)
        );
        assert!(
            matches!(report.source_status("latin.py"), Some(PythonProjectSourceStatus::UnavailableEncoding(PythonSourceDecodeFault::UnsupportedCodec { codec, .. })) if codec.as_ref() == "latin-1")
        );
        assert!(
            matches!(report.source_status("dependent.py"), Some(PythonProjectSourceStatus::UnavailableDependency { dependencies }) if dependencies.iter().any(|path| path.as_ref() == "bad.pyi"))
        );
        assert!(
            matches!(report.source_status("raw_dependent.py"), Some(PythonProjectSourceStatus::UnavailableDependency { dependencies }) if dependencies.iter().any(|path| path.as_ref() == "latin.py"))
        );
        for path in ["bad.pyi", "latin.py", "dependent.py", "raw_dependent.py"] {
            let module = report.module(path).ok_or("missing selected raw module")?;
            assert!(
                module.symbols.is_empty()
                    && module.inferences.is_empty()
                    && module.imports.is_empty()
            );
        }
        assert!(
            report
                .diagnostics()
                .iter()
                .any(|d| d.relative_path.as_ref() == "bad.pyi" && d.severity.as_ref() == "ERROR")
        );
        for (path, kind, bytes) in [
            (
                "bad.pyi",
                PythonProjectCoverageGapKind::UnavailableSyntax,
                bad.len(),
            ),
            (
                "latin.py",
                PythonProjectCoverageGapKind::UnavailableEncoding,
                latin.len(),
            ),
        ] {
            let gaps = report
                .coverage_gaps()
                .iter()
                .filter(|gap| gap.relative_path.as_ref() == path && gap.kind == kind)
                .collect::<Vec<_>>();
            assert_eq!(gaps.len(), 1);
            assert_eq!(gaps[0].span.start, 0);
            assert_eq!(gaps[0].span.end as usize, bytes);
        }
        let module = report.module("caller.py").ok_or("missing valid caller")?;
        assert!(module.symbols.iter().any(|s| s.target == "answer" && matches!(&s.outcome, SymbolOutcome::Definition { target, .. } if target.relative_path.as_ref() == "good.py" && !target.same_module)));
        assert!(
            report
                .coverage_gaps()
                .iter()
                .any(|gap| gap.relative_path.as_ref() == "dependent.py"
                    && gap.kind == PythonProjectCoverageGapKind::UnavailableDependency)
        );
        report.witness().validate_current(control())?;
        std::fs::write(
            root.join("latin.py"),
            b"# coding: latin-1\nchanged = '\xe9'\n",
        )?;
        assert!(
            report.witness().validate_current(control()).is_err(),
            "Unavailable raw bytes still fence publication"
        );
        std::fs::write(root.join("latin.py"), latin)?;
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn wholly_undecoded_frontier_retains_resources_without_synthetic_semantic_text()
-> Result<(), Box<dyn std::error::Error>> {
    let root =
        std::env::temp_dir().join(format!("nudox-python-all-undecoded-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;
    let bytes = b"# coding: unsupported-application-codec\nvalue = '\xff'\n";
    std::fs::write(root.join("resource.py"), bytes)?;
    let checker = NativePythonProjectAuthority::admit()?;
    let cancelled = AtomicBool::new(false);
    let report = checker.analyze_project_bytes(
        &root,
        "pkg",
        &[PythonProjectBytesSource {
            relative_path: "resource.py",
            source: bytes,
        }],
        PythonVersion::Python313,
        PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        },
    )?;
    assert!(matches!(
        report.source_status("resource.py"),
        Some(PythonProjectSourceStatus::UnavailableEncoding(_))
    ));
    assert_eq!(
        report.module("resource.py").ok_or("missing raw module")?,
        &Default::default()
    );
    assert!(
        report.diagnostics().is_empty(),
        "Intake faults must not impersonate native diagnostics"
    );
    assert_eq!(report.coverage_gaps().len(), 1);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn a_selected_valid_interface_does_not_inherit_an_unselected_executable_syntax_gap()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-interface-intake-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let sources = [
        PythonProjectBytesSource {
            relative_path: "target.py",
            source: b"x y\n",
        },
        PythonProjectBytesSource {
            relative_path: "target.pyi",
            source: b"def answer() -> int: ...\n",
        },
        PythonProjectBytesSource {
            relative_path: "caller.py",
            source: b"from target import answer\nanswer()\n",
        },
    ];
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source)?;
    }
    let cancelled = AtomicBool::new(false);
    let report = NativePythonProjectAuthority::admit()?.analyze_project_bytes(
        &root,
        "pkg",
        &sources,
        PythonVersion::Python313,
        PythonProjectControl {
            cancelled: &cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        },
    )?;
    assert_eq!(
        report.source_status("target.py"),
        Some(&PythonProjectSourceStatus::UnavailableSyntax)
    );
    for path in ["target.pyi", "caller.py"] {
        assert_eq!(
            report.source_status(path),
            Some(&PythonProjectSourceStatus::Analyzed),
            "Only actual native dependency handles carry the refusal"
        );
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn native_utf8_bom_and_crlf_keep_original_byte_spans_and_the_complete_raw_program()
-> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!(
        "nudox-python-bom-crlf-intake-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let good = b"\xef\xbb\xbf# caf\xc3\xa9\r\ndef answer() -> int:\r\n    return 42\r\n";
    let caller = b"\xef\xbb\xbf# coding: utf-8\r\nfrom good import answer\r\nanswer()\r\n";
    let sources = [
        PythonProjectBytesSource {
            relative_path: "good.py",
            source: good,
        },
        PythonProjectBytesSource {
            relative_path: "caller.py",
            source: caller,
        },
        PythonProjectBytesSource {
            relative_path: "latin.py",
            source: b"# coding: latin-1\r\nvalue = 'caf\xe9'\r\n",
        },
    ];
    for source in &sources {
        std::fs::write(root.join(source.relative_path), source.source)?;
    }
    let target_name = good
        .windows(b"answer".len())
        .position(|bytes| bytes == b"answer")
        .ok_or("raw target name")?;
    let caller_name = caller
        .windows(b"answer()".len())
        .position(|bytes| bytes == b"answer()")
        .ok_or("raw call name")?;
    let manifest = sources
        .iter()
        .map(|source| {
            Ok((
                source.relative_path.to_owned(),
                *backend_semantic::ir::SourceIdentity::from_bytes(source.source)
                    .ok_or("raw source extent")?
                    .identity,
            ))
        })
        .collect::<Result<Vec<_>, &'static str>>()?;
    let program =
        backend_semantic::ir::python_program_identity(&manifest).ok_or("complete raw program")?;
    let cancelled = AtomicBool::new(false);
    let checker = NativePythonProjectAuthority::admit()?;
    for profile in [PythonVersion::Python313, PythonVersion::Python314] {
        let report = checker.analyze_project_bytes(
            &root,
            "pkg",
            &sources,
            profile,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )?;
        let symbol = report
            .module("caller.py")
            .ok_or("native BOM caller")?
            .symbols
            .iter()
            .find(|symbol| symbol.span.start as usize == caller_name)
            .ok_or("native call at exact raw byte offset")?;
        let SymbolOutcome::Definition { target, .. } = &symbol.outcome else {
            return Err("BOM/CRLF call did not resolve".into());
        };
        assert_eq!(target.relative_path.as_ref(), "good.py");
        assert_eq!(target.name_span.start as usize, target_name);
        assert_eq!(
            &good[target.name_span.start as usize..target.name_span.end as usize],
            b"answer"
        );
        let coordinate =
            backend_semantic::ir::PythonSourceCoordinate::decode(&target.source_coordinate)
                .ok_or("native source coordinate")?;
        assert_eq!(coordinate.0.name_start as usize, target_name);
        assert_eq!(coordinate.0.program, program);
        assert_eq!(
            coordinate.0.source,
            *backend_semantic::ir::SourceIdentity::from_bytes(good)
                .ok_or("exact BOM/CRLF source identity")?
                .identity
        );
        assert!(coordinate.0.declaration_end as usize <= good.len());
        assert!(coordinate.0.declaration_start <= target.name_span.start);
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}
