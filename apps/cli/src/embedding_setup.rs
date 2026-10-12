//! Explicit fixed-preset acquisition/installation; no owner startup or inference.

use crate::options::{Format, Options};
use backend_engine::application::{EmbeddingRequirement, EmbeddingRuntimeLimits};
use backend_local_service::builtin::query::{
    PinnedCandleSetupRequest, PreparedCandleSetup, admit_candle_setup_authority,
    download_candle_artifacts, inspect_candle_helper,
};
use backend_present::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand, OwnerContext};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub(super) const fn help_text() -> &'static str {
    "nudox embedding-setup — install the source-pinned local Linux CPU Candle recipe\n\n\
     Ordinary matched-package setup:\n\
       nudox embedding-setup [--limits /absolute/limits.json] [--required]\n\n\
     Uses the packaged helper build receipt and fixed Linux CPU limits (1 GiB child ceiling, 2 GiB shared owner credits, 120-second operation limit). Missing or mismatched helper packaging refuses before network acquisition. Limits may be explicitly overridden.\n\n\
     Advanced: inspect a matched trusted helper without executing it:\n\
       nudox embedding-setup inspect --program /absolute/backend-embedding-candle [--dependency /absolute/file]\n\n\
     Install supplied pinned artifacts and explicit resource limits:\n\
       nudox [--project PATH] [--workspace PATH] embedding-setup install --program /absolute/backend-embedding-candle --helper-identity HEX --model /absolute/model.safetensors --tokenizer /absolute/tokenizer.json --limits /absolute/limits.json [--dependency /absolute/file] [--required]\n\n\
     Download and install that fixed preset with the same trusted helper and limits:\n\
       nudox [--project PATH] [--workspace PATH] embedding-setup download --program /absolute/backend-embedding-candle --helper-identity HEX --limits /absolute/limits.json [--dependency /absolute/file] [--required]\n\n\
     Use the helper identity from a reviewed build/package receipt; inspect only observes bytes and does not attest their semantics. The source-pinned model is sentence-transformers/all-MiniLM-L6-v2 revision1110a243fdf4706b3f48f1d95db1a4f5529b4d41. Ordinary setup and advanced download perform network acquisition, with exact artifact byte lengths/BLAKE3 verification and private staging. No executable probes are performed. The mandatory OS memory ceiling is currently supported on Linux only; other platforms refuse before acquisition.\n\n\
     limits.json must supply every field: stdout_bytes, stderr_bytes, output_bytes, input_bytes, wall_time_ms, workspace_bytes, process_count, memory_bytes, cpu_time_ms, resident_credit_bytes. memory_bytes must be a nonzero explicit child ceiling; cpu_time_ms may be null. Limits and shared credits are admitted by the existing runtime policy. There is no implicit resource increase.\n\n\
     For a fresh workspace, run setup before the first nudox add . or search: those commands start the local owner automatically. Installation saves the recipe; it is not activation or semantic availability. For an already-running owner, run the exact scoped embedding-refresh command printed after setup and copy its observation command until the actual compiler commit and query admission are reported, then run the scoped Add command. A busy owner refuses without replacing its recipe. Refresh admission or activation does not prove complete semantic corpus coverage; actual inference and publication are still required. Existing required policy cannot be downgraded. Explicit remote configuration remains authoritative.\n"
}

struct Arguments {
    operation: Operation,
    program: PathBuf,
    dependencies: Vec<PathBuf>,
}
enum Operation {
    Inspect,
    Download {
        identity: [u8; 32],
        limits: PathBuf,
        requirement: EmbeddingRequirement,
    },
    Install {
        identity: [u8; 32],
        model: PathBuf,
        tokenizer: PathBuf,
        limits: PathBuf,
        requirement: EmbeddingRequirement,
    },
}

pub(super) fn run(words: &[String], options: &Options) -> Result<String, Fault> {
    if words.is_empty() || words.first().is_some_and(|word| word.starts_with("--")) {
        let mut limits = crate::embedding_package::default_limits();
        let mut requirement = EmbeddingRequirement::Optional;
        let mut limits_seen = false;
        let mut at = 0;
        while let Some(word) = words.get(at) {
            match word.as_str() {
                "--required" if requirement == EmbeddingRequirement::Optional => {
                    requirement = EmbeddingRequirement::Required
                }
                "--limits" if !limits_seen => {
                    limits_seen = true;
                    at += 1;
                    limits = read_limits(&absolute(
                        words
                            .get(at)
                            .ok_or_else(|| Fault::usage("--limits", "option needs a value"))?,
                    )?)?;
                }
                _ => {
                    return Err(Fault::usage(
                        word,
                        "unknown or duplicate ordinary setup option",
                    ));
                }
            }
            at += 1;
        }
        let helper = crate::embedding_package::discover().map_err(refusal)?;
        return download_install(
            helper.program,
            helper.dependencies,
            helper.identity,
            limits,
            requirement,
            options,
        );
    }
    let arguments = parse(words)?;
    match arguments.operation {
        Operation::Inspect => {
            let identity = inspect_candle_helper(&arguments.program, &arguments.dependencies)
                .map_err(|error| refusal(error.to_string()))?;
            render(options, SetupState::ObservedHelper, &hex(identity))
        }
        Operation::Download {
            identity,
            limits,
            requirement,
        } => {
            let limits = read_limits(&limits)?;
            download_install(
                arguments.program,
                arguments.dependencies,
                identity,
                limits,
                requirement,
                options,
            )
        }
        Operation::Install {
            identity,
            model,
            tokenizer,
            limits,
            requirement,
        } => {
            let limits = read_limits(&limits)?;
            let prepared = PreparedCandleSetup::prepare(PinnedCandleSetupRequest {
                program: arguments.program,
                dependencies: arguments.dependencies,
                expected_toolchain: identity,
                model,
                tokenizer,
                requirement,
                limits,
            })
            .map_err(|error| refusal(error.to_string()))?;
            let workspace = backend_runtime::WorkspacePaths::discover(
                options.project().cloned(),
                options.workspace().cloned(),
                options.endpoint().cloned(),
            )
            .map_err(|error| refusal(error.to_string()))?;
            workspace
                .initialize_data_directory()
                .map_err(|error| refusal(error.to_string()))?;
            let spec = prepared
                .install(&workspace.data().join("compiler"))
                .map_err(|error| refusal(error.to_string()))?;
            render(
                options,
                SetupState::installed(&workspace)?,
                &hex(spec.recipe_identity()),
            )
        }
    }
}
fn download_install(
    program: PathBuf,
    dependencies: Vec<PathBuf>,
    identity: [u8; 32],
    limits: EmbeddingRuntimeLimits,
    requirement: EmbeddingRequirement,
    options: &Options,
) -> Result<String, Fault> {
    admit_candle_setup_authority(&program, &dependencies, identity, limits)
        .map_err(|error| refusal(error.to_string()))?;
    let workspace = backend_runtime::WorkspacePaths::discover(
        options.project().cloned(),
        options.workspace().cloned(),
        options.endpoint().cloned(),
    )
    .map_err(|error| refusal(error.to_string()))?;
    workspace
        .initialize_data_directory()
        .map_err(|error| refusal(error.to_string()))?;
    let compiler = workspace.data().join("compiler");
    let downloaded = download_candle_artifacts(&compiler, |artifact, bytes, total| {
        eprintln!("embedding setup {artifact:?}: {bytes}/{total} bytes")
    })
    .map_err(|error| refusal(error.to_string()))?;
    let prepared = PreparedCandleSetup::prepare(PinnedCandleSetupRequest {
        program,
        dependencies,
        expected_toolchain: identity,
        model: downloaded.model(),
        tokenizer: downloaded.tokenizer(),
        requirement,
        limits,
    })
    .map_err(|error| refusal(error.to_string()))?;
    let spec = prepared
        .install(&compiler)
        .map_err(|error| refusal(error.to_string()))?;
    render(
        options,
        SetupState::installed(&workspace)?,
        &hex(spec.recipe_identity()),
    )
}
enum SetupState {
    ObservedHelper,
    InstalledNotActivated(OwnerContext),
}
impl SetupState {
    fn installed(workspace: &backend_runtime::WorkspacePaths) -> Result<Self, Fault> {
        let text = |path: &Path| {
            path.to_str().map(str::to_owned).ok_or_else(|| refusal(
                "Artifacts were saved but not activated. A selected attachment path is not UTF-8, so no safe copyable command can be provided.".to_owned(),
            ))
        };
        Ok(Self::InstalledNotActivated(OwnerContext::new(
            text(workspace.project())?,
            text(workspace.data())?,
            text(workspace.endpoint())?,
        )))
    }
    fn action(&self) -> Result<String, Fault> {
        match self {
            Self::ObservedHelper => {
                Ok("No execution or semantic attestation; compare this identity with a trusted matched build receipt.".to_owned())
            }
            Self::InstalledNotActivated(context) => {
                let command = context.reindex_shell().ok_or_else(|| Fault::usage("add", "the Add command is not registered"))?;
                let refresh = context.embedding_refresh_shell().ok_or_else(|| Fault::usage("embedding-refresh", "the refresh command is not registered"))?;
                Ok(format!("Saved for the next local owner startup. If no owner is running, `{command}` starts it with these artifacts; poll the Add receipt, then search. An already-running owner keeps its admitted recipe; saving does not reactivate it. Run `{refresh}` and copy its observation command until it reports the actual compiler commit and query admission. If it is busy or refuses, retain its actual cause and retry deliberately after the outstanding work completes. Then run the scoped Add command above and observe actual publication. Semantic availability requires completed inference under the installed recipe. Explicit remote configuration remains authoritative."))
            }
        }
    }
    const fn name(&self) -> &'static str {
        match self {
            Self::ObservedHelper => "observed-helper",
            Self::InstalledNotActivated(_) => "installed-not-activated",
        }
    }
}
fn render(options: &Options, state: SetupState, identity: &str) -> Result<String, Fault> {
    #[derive(serde::Serialize)]
    struct Observation<'a> {
        kind: &'static str,
        state: &'static str,
        identity: &'a str,
        action: &'a str,
    }
    let action = state.action()?;
    if options.format() == Format::Json {
        serde_json::to_string(&Observation {
            kind: "local-embedding-setup",
            state: state.name(),
            identity,
            action: &action,
        })
        .map(|text| text + "\n")
        .map_err(|error| refusal(error.to_string()))
    } else {
        Ok(format!(
            "local embedding {}: {identity}\n{}\n",
            state.name(),
            action
        ))
    }
}
fn refusal(message: String) -> Fault {
    Fault::new(
        FaultSlug::Rejected,
        Operand::Path("embedding-setup".into()),
        Cause::new(CauseSlug::Refused, message),
        Affordance::None,
    )
}
fn parse(words: &[String]) -> Result<Arguments, Fault> {
    let verb = words.first().map(String::as_str).ok_or_else(|| {
        Fault::usage(
            "embedding-setup",
            "choose inspect, install or download; run nudox embedding-setup --help",
        )
    })?;
    if !matches!(verb, "inspect" | "install" | "download") {
        return Err(Fault::usage(
            "embedding-setup",
            "choose inspect, install or download",
        ));
    }
    let mut values = BTreeMap::new();
    let mut dependencies = Vec::new();
    let mut requirement = EmbeddingRequirement::Optional;
    let mut at = 1;
    while let Some(name) = words.get(at) {
        at += 1;
        if name == "--required" && verb != "inspect" {
            if requirement == EmbeddingRequirement::Required {
                return Err(Fault::usage(name, "duplicate option"));
            }
            requirement = EmbeddingRequirement::Required;
            continue;
        }
        let allowed = name == "--program"
            || name == "--dependency"
            || (verb != "inspect" && matches!(name.as_str(), "--helper-identity" | "--limits"))
            || (verb == "install" && matches!(name.as_str(), "--model" | "--tokenizer"));
        if !allowed {
            return Err(Fault::usage(name, "unknown embedding setup option"));
        }
        let value = words
            .get(at)
            .ok_or_else(|| Fault::usage(name, "option needs a value"))?;
        at += 1;
        if value.starts_with("--") {
            return Err(Fault::usage(name, "option needs a value"));
        }
        if name == "--dependency" {
            if dependencies.len() >= 64 {
                return Err(Fault::usage(name, "at most64 dependencies"));
            }
            dependencies.push(absolute(value)?);
        } else if values.insert(name.as_str(), value.as_str()).is_some() {
            return Err(Fault::usage(name, "duplicate option"));
        }
    }
    let required = |name| {
        values
            .get(name)
            .copied()
            .ok_or_else(|| Fault::usage(name, "required embedding setup option"))
    };
    let program = absolute(required("--program")?)?;
    let operation = if verb == "inspect" {
        Operation::Inspect
    } else if verb == "download" {
        Operation::Download {
            identity: parse_hex(required("--helper-identity")?)?,
            limits: absolute(required("--limits")?)?,
            requirement,
        }
    } else {
        Operation::Install {
            identity: parse_hex(required("--helper-identity")?)?,
            model: absolute(required("--model")?)?,
            tokenizer: absolute(required("--tokenizer")?)?,
            limits: absolute(required("--limits")?)?,
            requirement,
        }
    };
    Ok(Arguments {
        operation,
        program,
        dependencies,
    })
}
fn absolute(value: &str) -> Result<PathBuf, Fault> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || value.len() > 4096 {
        return Err(Fault::usage(
            value,
            "an absolute path of at most4096 bytes is required",
        ));
    }
    Ok(path)
}
fn parse_hex(value: &str) -> Result<[u8; 32], Fault> {
    let mut digest = [0; 32];
    if value.len() != 64 {
        return Err(Fault::usage(
            "--helper-identity",
            "expected64 hexadecimal digits from the trusted receipt",
        ));
    }
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        };
        digest[index] = (digit(chunk[0])
            .ok_or_else(|| Fault::usage("--helper-identity", "invalid hex"))?
            << 4)
            | digit(chunk[1]).ok_or_else(|| Fault::usage("--helper-identity", "invalid hex"))?;
    }
    Ok(digest)
}
fn hex(bytes: [u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(64);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourcePolicy {
    stdout_bytes: u64,
    stderr_bytes: u64,
    output_bytes: u64,
    input_bytes: u64,
    wall_time_ms: u64,
    workspace_bytes: u64,
    process_count: u64,
    #[serde(deserialize_with = "explicit_optional_limit")]
    memory_bytes: Option<u64>,
    #[serde(deserialize_with = "explicit_optional_limit")]
    cpu_time_ms: Option<u64>,
    resident_credit_bytes: u64,
}
fn explicit_optional_limit<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    Option::<u64>::deserialize(deserializer)
}
fn read_limits(path: &Path) -> Result<EmbeddingRuntimeLimits, Fault> {
    let parent = path
        .parent()
        .ok_or_else(|| refusal("limits path lacks a parent".into()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| refusal("limits path lacks a UTF8 filename".into()))?;
    let directory = backend_platform::DirectoryCapability::open_read_only_source(parent)
        .map_err(|error| refusal(error.to_string()))?;
    let mut file = directory
        .open_file_read(name)
        .map_err(|error| refusal(error.to_string()))?;
    if file
        .metadata()
        .map_err(|error| refusal(error.to_string()))?
        .len()
        > 64 * 1024
    {
        return Err(refusal("limits file exceeds64KiB".into()));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| refusal(error.to_string()))?;
    if bytes.len() > 64 * 1024 {
        return Err(refusal("limits file exceeds64KiB".into()));
    }
    let policy: ResourcePolicy =
        serde_json::from_slice(&bytes).map_err(|error| refusal(error.to_string()))?;
    Ok(EmbeddingRuntimeLimits {
        stdout_bytes: policy.stdout_bytes,
        stderr_bytes: policy.stderr_bytes,
        output_bytes: policy.output_bytes,
        input_bytes: policy.input_bytes,
        wall_time_ms: policy.wall_time_ms,
        workspace_bytes: policy.workspace_bytes,
        process_count: policy.process_count,
        memory_bytes: policy.memory_bytes,
        cpu_time_ms: policy.cpu_time_ms,
        resident_credit_bytes: policy.resident_credit_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }
    #[test]
    fn setup_grammar_is_explicit_and_rejects_duplicate_or_missing_authority() {
        let parsed = parse(&words(
            "inspect --program /tmp/helper --dependency /tmp/sdk",
        ))
        .expect("inspect");
        assert!(matches!(parsed.operation, Operation::Inspect));
        assert_eq!(parsed.dependencies, [PathBuf::from("/tmp/sdk")]);
        for text in [
            "inspect --program relative",
            "inspect --program /tmp/one --program /tmp/two",
            "inspect --program /tmp/helper --required",
            "install --program /tmp/helper",
            "install --program --model /tmp/model",
            "download --program /tmp/helper --helper-identity 00 --limits /tmp/limits",
            "download --program /tmp/helper --model /tmp/model",
        ] {
            assert!(parse(&words(text)).is_err(), "{text}");
        }
        let arguments = words(&format!(
            "install --program /tmp/helper --helper-identity {} --model /tmp/model --tokenizer /tmp/tokenizer --limits /tmp/limits --required",
            "00".repeat(32)
        ));
        assert!(matches!(
            parse(&arguments).expect("install").operation,
            Operation::Install {
                requirement: EmbeddingRequirement::Required,
                ..
            }
        ));
        let download = words(&format!(
            "download --program /tmp/helper --helper-identity {} --limits /tmp/limits",
            "00".repeat(32)
        ));
        assert!(matches!(
            parse(&download).expect("fixed preset download").operation,
            Operation::Download {
                requirement: EmbeddingRequirement::Optional,
                ..
            }
        ));
    }
    #[test]
    fn setup_json_observations_never_claim_activation_or_available() {
        for state in [
            SetupState::ObservedHelper,
            SetupState::InstalledNotActivated(OwnerContext::new(
                "/original/project".into(),
                "/original/state".into(),
                "/original/socket".into(),
            )),
        ] {
            let output = render(
                &Options::plain(Format::Json),
                state,
                "actual-observed-identity",
            )
            .expect("render");
            let decoded: serde_json::Value = serde_json::from_str(&output).expect("json");
            assert_eq!(decoded["kind"], "local-embedding-setup");
            assert!(decoded.get("activated").is_none());
            assert!(decoded.get("available").is_none());
            assert!(
                decoded["action"]
                    .as_str()
                    .expect("action")
                    .contains("semantic")
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn installed_setup_guidance_copies_the_discovered_attachment_without_rebinding_or_activation() {
        let workspace = backend_runtime::WorkspacePaths::discover(
            Some("/original/project 'quoted'".into()),
            Some("/original/state with spaces".into()),
            Some("/original/short socket".into()),
        )
        .expect("explicit fixture attachment; no owner startup or installation");
        let state = SetupState::installed(&workspace).expect("UTF-8 scope");
        let action = state.action().expect("registered Add command");
        let command = action.split('`').nth(1).expect("exact copyable command");
        let parsed = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -- {command}; printf '%s\\000' \"$@\""))
            .output()
            .expect("argv only; no nudox execution");
        assert!(parsed.status.success());
        assert!(parsed.stderr.is_empty());
        let words = parsed
            .stdout
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .map(|word| String::from_utf8(word.to_vec()).expect("UTF-8 argv"))
            .collect::<Vec<_>>();
        assert_eq!(words[0], "nudox");
        let (options, command) = crate::options::split(&words[1..]).expect("actual global grammar");
        assert_eq!(
            options.project().map(PathBuf::as_path),
            Some(workspace.project())
        );
        assert_eq!(
            options.workspace().map(PathBuf::as_path),
            Some(workspace.data())
        );
        assert_eq!(
            options.endpoint().map(PathBuf::as_path),
            Some(workspace.endpoint())
        );
        let invocation = crate::invoke::parse(&command, None).expect("actual Add grammar");
        let request =
            backend_present::lower(&invocation, "/different/caller/directory").expect("typed Add");
        assert!(
            matches!(request, backend_present::Request::Index(path) if Path::new(&path) == workspace.project())
        );
        let refresh = action
            .split('`')
            .nth(3)
            .expect("exact scoped refresh command");
        let parsed = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -- {refresh}; printf '%s\\000' \"$@\""))
            .output()
            .expect("argv only; no nudox or helper execution");
        assert!(parsed.status.success());
        assert!(parsed.stderr.is_empty());
        let words = parsed
            .stdout
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .map(|word| String::from_utf8(word.to_vec()).expect("UTF-8 argv"))
            .collect::<Vec<_>>();
        assert_eq!(words[0], "nudox");
        let (options, words) = crate::options::split(&words[1..]).expect("actual global grammar");
        assert_eq!(
            options.project().map(PathBuf::as_path),
            Some(workspace.project())
        );
        assert_eq!(
            options.workspace().map(PathBuf::as_path),
            Some(workspace.data())
        );
        assert_eq!(
            options.endpoint().map(PathBuf::as_path),
            Some(workspace.endpoint())
        );
        assert_eq!(words, ["embedding-refresh"]);
        let invocation = crate::invoke::parse(&words, None).expect("registered refresh grammar");
        assert_eq!(invocation.grammar().name(), "embedding-refresh");
        assert!(
            invocation.at(0).is_none(),
            "real CLI capture must supply the config proof; guidance cannot fabricate it"
        );
        for format in [Format::Human, Format::Markdown, Format::Json] {
            let output = render(
                &Options::plain(format),
                SetupState::installed(&workspace).expect("UTF-8 scope"),
                "fixture-recipe",
            )
            .expect("bounded setup projection only");
            if format == Format::Json {
                let value: serde_json::Value = serde_json::from_str(&output).expect("JSON");
                assert_eq!(value["state"], "installed-not-activated");
                assert_eq!(value["action"], action);
                assert!(value.get("activated").is_none());
                assert!(value.get("available").is_none());
            } else {
                assert!(output.contains("installed-not-activated"));
                assert!(output.contains(&action));
            }
        }
        assert!(action.contains("already-running owner keeps its admitted recipe"));
        assert!(action.contains("does not reactivate it"));
        assert!(!action.contains("Reopen"));
        assert!(!action.contains("no CLI reload command"));
        assert!(action.contains("actual compiler commit and query admission"));
        assert!(action.contains("observe actual publication"));
        let help = crate::invoke::help();
        assert!(
            help.find("nudox embedding-setup").expect("setup")
                < help.find("nudox add .").expect("add")
        );
        use std::os::unix::ffi::OsStringExt as _;
        let invalid = backend_runtime::WorkspacePaths::discover(
            Some(std::ffi::OsString::from_vec(b"/original/non-utf8-\xff".to_vec()).into()),
            Some("/original/state".into()),
            Some("/original/socket".into()),
        )
        .expect("path projection fixture only");
        let fault = SetupState::installed(&invalid)
            .err()
            .expect("never replace path bytes with a different operand");
        assert_eq!(fault.slug(), FaultSlug::Rejected);
        assert!(fault.cause().sentence().contains("not UTF-8"));
        assert_eq!(fault.affordance(), &Affordance::None);
    }
    #[test]
    fn setup_resource_policy_requires_explicit_optional_fields_and_rejects_duplicates() {
        let complete = r#"{"stdout_bytes":1048576,"stderr_bytes":1024,"output_bytes":1049600,"input_bytes":1048576,"wall_time_ms":30000,"workspace_bytes":134217728,"process_count":4,"memory_bytes":134217728,"cpu_time_ms":null,"resident_credit_bytes":1073741824}"#;
        assert!(serde_json::from_str::<ResourcePolicy>(complete).is_ok());
        assert!(
            serde_json::from_str::<ResourcePolicy>(&complete.replace("\"cpu_time_ms\":null,", ""))
                .is_err()
        );
        assert!(
            serde_json::from_str::<ResourcePolicy>(
                &complete.replace("\"memory_bytes\":134217728,", "")
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<ResourcePolicy>(&complete.replace(
                "\"wall_time_ms\":30000,",
                "\"wall_time_ms\":30000,\"wall_time_ms\":30000,"
            ))
            .is_err()
        );
    }
}
