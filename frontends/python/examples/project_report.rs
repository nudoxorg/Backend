//! Explicit native-authority comparison over immutable pinned package bytes.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use backend_frontend_python::legacy::checker::{
    NativePythonProjectAuthority, PYTHON_NATIVE_PROJECT_SOURCE_REVISION, PythonProjectControl,
    PythonProjectSource, SymbolOutcome, is_ignored_python_source_directory,
};
use backend_semantic::vocabulary::PythonVersion;
use serde_json::json;

fn files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_symlink() {
            continue;
        }
        let path = entry.path();
        if metadata.is_dir() {
            if is_ignored_python_source_directory(&entry.file_name()) {
                continue;
            }
            files(root, &path, output)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "py" || extension == "pyi")
        {
            output.push(
                path.strip_prefix(root)
                    .map_err(std::io::Error::other)?
                    .to_path_buf(),
            );
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 3 {
        return Err("usage: project_report ABS_PACKAGE_ROOT PACKAGE_NAME".into());
    }
    let checker = NativePythonProjectAuthority::admit()?;
    let root = PathBuf::from(&args[1]);
    let mut paths = Vec::new();
    files(&root, &root, &mut paths)?;
    paths.sort();
    let owned = paths
        .iter()
        .map(|path| {
            Ok((
                path.to_str().ok_or("non-UTF8 source path")?.to_owned(),
                std::fs::read_to_string(root.join(path))?,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
    let sources = owned
        .iter()
        .map(|(path, source)| PythonProjectSource {
            relative_path: path,
            source,
        })
        .collect::<Vec<_>>();
    let cancelled = AtomicBool::new(false);
    let mut passes = Vec::new();
    for pass in 0..2 {
        let started = Instant::now();
        let report = checker.analyze_project(
            &root,
            &args[2],
            &sources,
            PythonVersion::Python314,
            PythonProjectControl {
                cancelled: &cancelled,
                deadline: started + Duration::from_secs(60),
            },
        )?;
        report.witness().validate_current(PythonProjectControl {
            cancelled: &cancelled,
            deadline: started + Duration::from_secs(60),
        })?;
        let mut modules = Vec::new();
        for source in &sources {
            let module = report
                .module(source.relative_path)
                .ok_or("incomplete native frontier")?;
            let definitions = module.symbols.iter().filter_map(|symbol| match &symbol.outcome {
                SymbolOutcome::Definition { target, callee } => Some(json!({"source_span":{"start":symbol.span.start,"end":symbol.span.end},"spelling":symbol.target,
                    "target_path":target.relative_path,"target_name":target.qualified_name,"target_kind":format!("{:?}",target.kind),"target_span":{"start":target.name_span.start,"end":target.name_span.end},"same_module":target.same_module,
                    "native_callee":callee.as_ref().map(|callee| json!({"target_path":callee.relative_path,"target_name":callee.qualified_name,"target_kind":format!("{:?}",callee.kind),"target_span":{"start":callee.name_span.start,"end":callee.name_span.end},"same_module":callee.same_module}))})),
                _ => None,
            }).collect::<Vec<_>>();
            let inferences = module.inferences.iter().map(|inference| json!({"span":{"start":inference.site.start,"end":inference.site.end},"site":format!("{:?}",inference.kind),"type":format!("{:?}",inference.observed)})).collect::<Vec<_>>();
            modules.push(json!({"path":source.relative_path,"definitions":definitions,"inferences":inferences}));
        }
        passes.push(json!({"pass":pass,"session":"fresh committed State","elapsed_ms":started.elapsed().as_millis(),"modules":modules}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"schema":"python-native-project-comparison.v1","package":args[2],"native_source_revision":PYTHON_NATIVE_PROJECT_SOURCE_REVISION,
        "selected_profile":"Python314","scope":"native authority only; no public CLI/MCP or precise read-set purity claim",
        "source_frontier":sources.iter().map(|source| json!({"path":source.relative_path,"bytes":source.source.len(),"blake3":blake3::hash(source.source.as_bytes()).to_hex().to_string()})).collect::<Vec<_>>(),"passes":passes})
        )?
    );
    Ok(())
}
