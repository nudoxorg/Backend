//! Persisted code-side style link facts: how TypeScript, TSX and JSX sources
//! use stylesheets, classes and custom properties, and how components use each
//! other. A fact is a producer claim about one source file's bytes; admission
//! authenticates the claim against the selected project and the host's content
//! identity before anything serves it, and a missing row means "not published",
//! never "no links".
use serde::{Deserialize, Serialize};

/// Versioned recipe of the code scan which produced these facts.
pub const CODE_LINK_RECIPE: &str = "code-style-links-v1;oxc;lexical-class-and-import-binding";
/// Largest number of facts one code host may carry.
pub const MAX_CODE_FACTS_PER_HOST: u32 = 20_000;
/// Longest identifier, specifier or token kept in one fact.
pub const MAX_CODE_FACT_TEXT: usize = 1024;

/// What an import binds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeImported {
    /// `import local from "spec"`.
    Default,
    /// `import { name as local } from "spec"`.
    Named(String),
    /// `import * as local from "spec"`.
    Namespace,
}

/// One lexical fact found in a code host. None of these is a build-transform
/// claim: a literal class token is only a candidate link.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "fact", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CodeFactRow {
    /// A value import binding.
    Import {
        local: String,
        spec: String,
        imported: CodeImported,
        line: u32,
    },
    /// `import "spec"` with no binding.
    SideEffectImport { spec: String, line: u32 },
    /// A literal class token in `className`, `class` or a class helper.
    ClassUse {
        token: String,
        line: u32,
        owner: Option<String>,
        conditional: bool,
    },
    /// `styles.card` where `styles` is a stylesheet import binding. `binding`
    /// is the ordinal of the `Import` fact it reads.
    ModuleClassUse {
        binding: u32,
        class: String,
        line: u32,
        owner: Option<String>,
    },
    /// A JSX tag naming a component-like local.
    Render {
        local: String,
        line: u32,
        owner: Option<String>,
    },
    /// A custom property written or read from code.
    PropertyUse {
        name: String,
        line: u32,
        owner: Option<String>,
        writes: bool,
    },
    /// A component-like declaration.
    Component { name: String, line: u32 },
    /// An exported name and the local declaration behind it.
    Export { exported: String, local: String },
    /// A re-export through another module.
    Reexport {
        exported: String,
        spec: String,
        imported: String,
    },
}

impl CodeFactRow {
    /// Checks text bounds only; relations between facts are checked by the host.
    pub fn validate(&self) -> Result<(), String> {
        let ok = |text: &str| !text.is_empty() && text.len() <= MAX_CODE_FACT_TEXT && !text.contains('\0');
        let owner_ok = |owner: &Option<String>| owner.as_deref().is_none_or(ok);
        let valid = match self {
            Self::Import { local, spec, imported, .. } => {
                ok(local) && ok(spec) && !matches!(imported, CodeImported::Named(name) if !ok(name))
            }
            Self::SideEffectImport { spec, .. } => ok(spec),
            Self::ClassUse { token, owner, .. } => ok(token) && owner_ok(owner),
            Self::ModuleClassUse { class, owner, .. } => ok(class) && owner_ok(owner),
            Self::Render { local, owner, .. } => ok(local) && owner_ok(owner),
            Self::PropertyUse { name, owner, .. } => ok(name) && owner_ok(owner),
            Self::Component { name, .. } => ok(name),
            Self::Export { exported, local } => ok(exported) && ok(local),
            Self::Reexport { exported, spec, imported } => ok(exported) && ok(spec) && ok(imported),
        };
        if valid { Ok(()) } else { Err("invalid code link fact".into()) }
    }
}

/// Terminal of one code host's scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeHostState {
    /// Parsed and scanned; `dynamic_class_expressions` counts class lists with
    /// runtime parts whose static tokens were linked but whose rest was not.
    Scanned { dynamic_class_expressions: u32 },
    /// The source does not parse; no facts follow.
    Unparsed,
    /// The source could not be read or fit no budget; no facts follow.
    Unavailable(CodeUnavailable),
}

/// Why a code host has no facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodeUnavailable {
    /// The file could not be read under the selected root.
    ReadUnavailable,
    /// The file is not valid UTF-8.
    InvalidUtf8,
    /// A size or row budget was exhausted.
    Limit,
}

/// Selection-level witness of the project's code hosts.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeSelection {
    /// Scan recipe which produced every host of this selection.
    pub recipe: String,
    /// Number of `CodeHost` rows owned by the project.
    pub count: u32,
    /// Digest of those rows in key order.
    pub digest: [u8; 32],
}
