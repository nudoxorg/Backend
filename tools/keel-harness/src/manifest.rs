//! The evaluated Nix model is the only authoring surface for cases.
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Manifest {
    pub schema: u32,
    pub cases: Vec<Case>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Case {
    pub id: String,
    pub language: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub daemon: Option<bool>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Step {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cli: Vec<String>,
    #[serde(default)]
    pub tool: String,
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub bind: BTreeMap<String, String>,
    #[serde(default)]
    pub expect: Option<Expect>,
    #[serde(default)]
    pub binary: String,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub config: Option<Value>,
    #[serde(default)]
    pub stdin: Option<String>,
    #[serde(default = "timeout")]
    pub timeout_seconds: u64,
}
fn timeout() -> u64 {
    180
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Expect {
    pub exit: i32,
    #[serde(default)]
    pub json: Vec<Assertion>,
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    #[serde(default)]
    pub stderr_contains: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Assertion {
    pub pointer: String,
    #[serde(default)]
    pub select: Option<String>,
    #[serde(default, deserialize_with = "present")]
    pub equals: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub contains: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    pub not_contains: Option<Value>,
    pub set_equals: Option<Vec<Value>>,
    #[serde(default)]
    pub absent: bool,
}
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
pub(crate) fn relative(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.contains('\\')
        || std::path::Path::new(path)
            .components()
            .any(|p| !matches!(p, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "fixture path {path:?} must contain only relative normal components"
        ));
    }
    Ok(())
}
impl Manifest {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.schema != 1 || self.cases.is_empty() {
            return Err("manifest must have schema 1 and at least one case".into());
        }
        let mut ids = BTreeSet::new();
        for case in &self.cases {
            if !valid_id(&case.id) || !ids.insert(&case.id) {
                return Err(format!("invalid or duplicate case id {:?}", case.id));
            }
            if ![
                "rust",
                "python",
                "javascript",
                "typescript",
                "css",
                "sql",
                "protocol",
            ]
            .contains(&case.language.as_str())
            {
                return Err(format!(
                    "{}: unsupported or paused language {}",
                    case.id, case.language
                ));
            }
            if case.steps.is_empty() {
                return Err(format!("{} has no steps", case.id));
            }
            for path in case.files.keys() {
                relative(path)?;
            }
            let mut steps = BTreeSet::new();
            for step in &case.steps {
                if !valid_id(&step.id) || !steps.insert(&step.id) {
                    return Err(format!(
                        "{}: invalid or duplicate step {}",
                        case.id, step.id
                    ));
                }
                if step.timeout_seconds == 0 || step.timeout_seconds > 600 {
                    return Err(format!("{}: timeout must be 1..600 seconds", step.id));
                }
                for path in step.files.keys().chain(step.remove.iter()) {
                    relative(path)?;
                }
                match step.kind.as_str() {
                    "write" => {
                        if step.files.is_empty() && step.remove.is_empty() {
                            return Err(format!("{}: write changes nothing", step.id));
                        }
                    }
                    "index" | "restart" => {}
                    "cli" | "query" | "process" => {
                        let e = step.expect.as_ref().ok_or_else(|| {
                            format!(
                                "{}: executable steps require reviewed expectations",
                                step.id
                            )
                        })?;
                        if e.json.is_empty()
                            && e.stdout_contains.is_empty()
                            && e.stderr_contains.is_empty()
                        {
                            return Err(format!(
                                "{}: exit status alone is not a behavior oracle",
                                step.id
                            ));
                        }
                        for a in &e.json {
                            if !a.pointer.is_empty() && !a.pointer.starts_with('/') {
                                return Err(format!("{}: assertion needs a JSON pointer", step.id));
                            }
                            let n = usize::from(a.equals.is_some())
                                + usize::from(a.contains.is_some())
                                + usize::from(a.not_contains.is_some())
                                + usize::from(a.set_equals.is_some())
                                + usize::from(a.absent);
                            if n != 1 {
                                return Err(format!(
                                    "{}: assertions require exactly one operator",
                                    step.id
                                ));
                            }
                        }
                        if step.kind == "query" && (step.tool.is_empty() || step.cli.is_empty()) {
                            return Err(format!(
                                "{}: query needs both real CLI and MCP operations",
                                step.id
                            ));
                        }
                        if step.kind == "process" && !valid_id(&step.binary) {
                            return Err(format!(
                                "{}: process binary must be an artifact basename",
                                step.id
                            ));
                        }
                    }
                    other => return Err(format!("{}: unknown step kind {other}", step.id)),
                }
            }
        }
        Ok(())
    }
}
