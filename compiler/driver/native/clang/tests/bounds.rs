use super::common::{Fixture, TestError, real_clang, require_linked_library, run_analysis};
use super::{ClangError, ClangSourceLanguage, MAX_ANALYSIS_SCRATCH_BYTES, SemanticKind};

fn probe_tool() -> Result<(), TestError> {
    require_linked_library()?;
    real_clang().map(|_| ())
}

#[test]
fn caller_identity_scratch_capacity_is_typed_and_transactional() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"struct Bound { int field; };\n";
    let fixture = Fixture::new("c")?;
    let analysis_error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        None,
        None,
        None,
        super::AnalysisScratch {
            identity: &mut vec![0_u8; 256],
            facts: &mut vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES],
        },
        |_| {},
    );
    assert!(matches!(
        analysis_error,
        Err(ClangError::LibclangIdentityScratchTooSmall {
            provided: 256,
            required,
            ..
        }) if required > 256
    ));
    Ok(())
}

#[test]
fn token_capacity_is_caller_bounded_and_typed() -> Result<(), TestError> {
    probe_tool()?;
    let source = b"int use(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m, int n, int o, int p) { return a + b + c + d + e + f + g + h + i + j + k + l + m + n + o + p; }\n";
    let fixture = Fixture::new("c")?;
    let analysis_error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source,
        },
        None,
        None,
        None,
        super::AnalysisScratch {
            identity: &mut vec![0_u8; 4096],
            facts: &mut vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES],
        },
        |_| {},
    );
    assert!(matches!(
        analysis_error,
        Err(ClangError::LibclangTokenCapacity {
            observed,
            limit: 64,
            ..
        }) if observed > 64
    ));
    Ok(())
}

#[test]
fn binding_capacity_is_typed_at_limit_and_limit_plus_one() -> Result<(), TestError> {
    probe_tool()?;
    for count in [1024_usize, 1025] {
        let mut source = Vec::new();
        for index in 0..count {
            source.extend_from_slice(format!("struct R{index:04} {{}};\n").as_bytes());
        }
        let fixture = Fixture::new("c")?;
        let analysis = run_analysis(
            &fixture,
            &source,
            ClangSourceLanguage::C,
            None,
            None,
            MAX_ANALYSIS_SCRATCH_BYTES,
            MAX_ANALYSIS_SCRATCH_BYTES,
        )?;
        assert_eq!(analysis.report.entities as usize, count);
        assert_eq!(analysis.facts.len(), count);
    }
    Ok(())
}

#[test]
fn binding_capacity_reports_the_admitted_caller_limit() -> Result<(), TestError> {
    probe_tool()?;
    let count = 33_usize;
    let mut source = Vec::new();
    for index in 0..count {
        source.extend_from_slice(format!("struct R{index:04} {{}};\n").as_bytes());
    }
    let fixture = Fixture::new("c")?;
    let analysis_error = super::analyze(
        super::AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: ClangSourceLanguage::C,
            source: &source,
        },
        None,
        None,
        None,
        super::AnalysisScratch {
            identity: &mut vec![0_u8; 4096],
            facts: &mut vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES],
        },
        |_| {},
    );
    assert!(matches!(
        analysis_error,
        Err(ClangError::BindingCapacity {
            limit: 32,
            observed: 33
        })
    ));
    Ok(())
}

#[test]
fn canonical_kind_codes_round_trip_over_the_whole_registry() {
    for (ordinal, kind) in SemanticKind::ALL.iter().enumerate() {
        let code = u16::from(*kind);
        assert_eq!(code as usize, ordinal, "canonical kind order drifted");
        assert_eq!(
            SemanticKind::try_from(code),
            Ok(*kind),
            "kind code {code} did not round-trip"
        );
    }
    let unknown = u16::from(SemanticKind::Parameter) + 1;
    assert_eq!(SemanticKind::try_from(unknown), Err(unknown));
}
