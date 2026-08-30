use std::{
    env,
    ffi::OsString,
    fs, io,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

const FORBIDDEN_SOURCE: &str = "use nudox_compile_registry::TypeScriptSubset;\nfn main() {\n    let subset = TypeScriptSubset;\n    let _ = subset.lower(&[]);\n}\n";
const LEGAL_SOURCE: &str = "use nudox_compile_registry::TypeScriptSubset;\nfn main() {\n    let subset = TypeScriptSubset;\n    let _ = subset.parse(&[]);\n}\n";

fn registry_rlib() -> io::Result<PathBuf> {
    let executable = env::current_exe()?;
    let dependencies = executable.parent().ok_or_else(|| {
        io::Error::other("the subset test executable has no dependency-directory parent")
    })?;
    let mut candidate = None;
    for entry in fs::read_dir(dependencies)? {
        let path = entry?.path();
        let is_registry_rlib =
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("libnudox_compile_registry-") && name.ends_with(".rlib")
                });
        if !is_registry_rlib {
            continue;
        }
        if candidate.is_some() {
            return Err(io::Error::other(
                "multiple registry rlibs beside subset executable",
            ));
        }
        candidate = Some(path);
    }
    candidate.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "missing registry rlib beside subset executable",
        )
    })
}

fn compile(source: &str, artifact: &PathBuf) -> io::Result<std::process::Output> {
    let dependencies = artifact
        .parent()
        .ok_or_else(|| io::Error::other("registry rlib has no dependency-directory parent"))?;
    let compiler = match env::var_os("RUSTC") {
        Some(compiler) => compiler,
        None => "rustc".into(),
    };
    let mut external_registry = OsString::from("nudox_compile_registry=");
    external_registry.push(artifact);
    let mut process = Command::new(compiler)
        .args(["--edition", "2024", "--crate-type", "lib"])
        .arg("-L")
        .arg(dependencies)
        .arg("--extern")
        .arg(external_registry)
        .arg("--emit=metadata=-")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = process.stdin.take().ok_or_else(|| {
        io::Error::other("rustc accepted a piped stdin configuration without a stdin handle")
    })?;
    stdin.write_all(source.as_bytes())?;
    drop(stdin);
    process.wait_with_output()
}

fn has_exact_absence_error(stderr: &[u8]) -> bool {
    let Ok(stderr) = std::str::from_utf8(stderr) else {
        return false;
    };
    let one_coded_header = stderr
        .lines()
        .filter(|line| line.starts_with("error[E"))
        .count()
        == 1;
    let has_unexpected_uncoded_header = stderr.lines().any(|line| {
        line.starts_with("error:") && line != "error: aborting due to 1 previous error"
    });
    one_coded_header
        && !has_unexpected_uncoded_header
        && stderr.contains("error[E0599]")
        && stderr.contains("TypeScriptSubset")
        && stderr.contains("lower")
}

#[test]
fn typescript_lower_is_a_causal_absent_member() -> io::Result<()> {
    let artifact = registry_rlib()?;
    let forbidden = compile(FORBIDDEN_SOURCE, &artifact)?;
    if forbidden.status.success() || !has_exact_absence_error(&forbidden.stderr) {
        return Err(io::Error::other(
            "forbidden TypeScript lower source lacked one causal E0599",
        ));
    }

    let legal = compile(LEGAL_SOURCE, &artifact)?;
    if !legal.status.success() || has_exact_absence_error(&legal.stderr) {
        return Err(io::Error::other(
            "legal TypeScript parse mutant did not clear absence predicate",
        ));
    }
    Ok(())
}
