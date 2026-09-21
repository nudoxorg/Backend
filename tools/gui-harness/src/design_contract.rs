//! Machine-readable extraction of the Nudox HTML design references.
//!
//! The four files in `Nudox-Design-System` are the visual source of truth for
//! the GUI lane.  This module intentionally does not copy their markup into a
//! product view.  It extracts the values that a renderer can be checked
//! against: artboard size, palette, type, spacing, bevel/cut geometry, glyph
//! rules, focus treatment, and motion timings.  The extractor is deliberately
//! dependency-free so it can run in the same `nix shell` used by the capture
//! harness and so an HTML archive can be audited without a browser.

use crate::hash_bytes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// The four immutable reference artifacts supplied by the design contract.
pub const REQUIRED_ARTIFACTS: [(&str, &str, &str); 4] = [
    ("descent", "Descent", "artifacts/descent/Descent.dc.html"),
    (
        "colour-with-a-job",
        "Colour with a job",
        "artifacts/colour-with-a-job/Color.dc.html",
    ),
    (
        "one-mark-one-language",
        "One mark, one language",
        "artifacts/one-mark-one-language/Brand.dc.html",
    ),
    (
        "the-information-language",
        "The information language",
        "artifacts/the-information-language/Language.dc.html",
    ),
];

/// Schema version emitted by [`DesignContract`].
pub const DESIGN_CONTRACT_SCHEMA: u32 = 1;

/// A complete, hashed design reference contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesignContract {
    /// Machine-readable schema version.
    pub schema: u32,
    /// Stable contract name.
    pub contract_id: String,
    /// Relative source directory name.  Absolute paths are never persisted.
    pub source_root: String,
    /// Hash over ordered artifact paths and source bytes.
    pub source_sha256: String,
    /// Each artifact and its declared artboard baseline.
    pub artifacts: Vec<DesignArtifact>,
    /// Values shared by the design language and grouped for conformance checks.
    pub canonical: DesignSemantics,
    /// Capture dimensions that are part of this reference contract.
    pub capture_matrix: CaptureMatrix,
}

/// The exact reference identity attached to a rendered capture.
///
/// A capture is only comparable when the source contract is recorded beside
/// it.  This intentionally contains hashes and declared artboards rather
/// than copied pixels: reference images are supplied by the design lane and
/// are never synthesized by the harness.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceMetadata {
    /// Design contract schema consumed by the capture.
    pub schema: u32,
    /// Stable contract identifier.
    pub contract_id: String,
    /// Hash of the complete machine-readable contract.
    pub contract_sha256: String,
    /// Hash of the ordered, exact HTML artifact bytes.
    pub source_sha256: String,
    /// Exact source artifacts and their declared artboard baselines.
    pub artifacts: Vec<ReferenceArtifact>,
    /// Explicitly states that baselines are external review artifacts.
    pub capture_policy: String,
}

/// One source artifact identity retained in capture metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReferenceArtifact {
    /// Stable artifact id.
    pub id: String,
    /// Source path relative to the contract root.
    pub path: String,
    /// Exact source bytes hash.
    pub sha256: String,
    /// Declared CSS artboard dimensions.
    pub artboard: Artboard,
}

/// One HTML design reference and its source digest.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesignArtifact {
    /// Stable artifact id.
    pub id: String,
    /// Human-facing title from the HTML `<title>`.
    pub title: String,
    /// Path relative to the contract root.
    pub path: String,
    /// SHA-256 over the exact HTML bytes.
    pub sha256: String,
    /// Declared preview size from the export runtime.
    pub preview: Artboard,
    /// First `.nx` artboard size in the rendered HTML.
    pub artboard: Artboard,
    /// Extracted semantics for this artifact.
    pub semantics: DesignSemantics,
}

/// An artboard baseline in CSS pixels.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Artboard {
    /// CSS pixel width.
    pub width: u32,
    /// CSS pixel height.
    pub height: u32,
}

/// Canonical grouped values used by the design conformance layer.
///
/// The maps retain the selector and declaration context in their keys.  This
/// is intentional: a later FACET revision overrides an earlier token, and a
/// conformance report must be able to say which declaration changed rather
/// than silently flattening the cascade.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DesignSemantics {
    /// Palette and surface custom properties.
    pub palette: BTreeMap<String, Vec<TokenDeclaration>>,
    /// Font families and typography declarations.
    pub typography: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Spacing, dimensions, radii, and layout declarations.
    pub spacing: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Elevation, bevel highlights/shadows, and weave/shaft textures.
    pub bevel: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Chamfer and clipping declarations.
    pub cuts: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Icon, mark, diamond, and SVG glyph declarations.
    pub glyphs: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Durations, easing, transitions, keyframes, and animation declarations.
    pub motion: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Focus, hover, pressed, and visible-state declarations.
    pub focus: BTreeMap<String, Vec<StyleDeclaration>>,
    /// Exact declarations retained for audit/debugging.  This makes the
    /// generated contract lossless for the semantic CSS properties above.
    pub declarations: Vec<StyleDeclaration>,
    /// SVG view-box and class inventory used to catch glyph substitutions.
    pub svg_inventory: Vec<SvgGlyph>,
}

/// One custom-property declaration and its selector context.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TokenDeclaration {
    /// CSS custom-property name including the leading `--`.
    pub name: String,
    /// Selector or at-rule context.
    pub selector: String,
    /// Exact value after whitespace normalization.
    pub value: String,
    /// Declaration order in the source stylesheet.
    pub order: usize,
}

/// One CSS declaration retained by the contract.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StyleDeclaration {
    /// Selector or at-rule context.
    pub selector: String,
    /// CSS property name.
    pub property: String,
    /// Exact value after whitespace normalization.
    pub value: String,
    /// Declaration order in the source stylesheet.
    pub order: usize,
}

/// An SVG glyph inventory entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SvgGlyph {
    /// SVG class list, if present.
    pub class: Option<String>,
    /// Accessible label, if present.
    pub label: Option<String>,
    /// Exact viewBox, if present.
    pub view_box: Option<String>,
    /// Width attribute, if present.
    pub width: Option<String>,
    /// Height attribute, if present.
    pub height: Option<String>,
    /// Number of child path/shape elements.
    pub shape_count: usize,
}

/// Required viewport and scale matrix for design evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CaptureMatrix {
    /// Logical viewport names required by the GUI gate.
    pub viewports: Vec<String>,
    /// Representative logical sizes for wide, compact, and narrow evidence.
    pub viewport_sizes: BTreeMap<String, Artboard>,
    /// Backing scales required by the GUI gate.
    pub scales: Vec<u8>,
    /// Theme names required by the GUI gate.
    pub themes: Vec<String>,
    /// Motion modes required by the GUI gate.
    pub motion: Vec<String>,
    /// Interaction states required by the GUI gate.
    pub states: Vec<String>,
    /// Route roots that must be reachable through production navigation.
    pub routes: Vec<String>,
    /// Transient/error overlays that must have semantic evidence.
    pub overlays: Vec<String>,
    /// Keyboard and accessibility evidence required for each live journey.
    pub keyboard: Vec<String>,
}

impl Default for CaptureMatrix {
    fn default() -> Self {
        Self {
            viewports: vec!["wide".to_owned(), "compact".to_owned(), "narrow".to_owned()],
            viewport_sizes: BTreeMap::from([
                (
                    "wide".to_owned(),
                    Artboard {
                        width: 1440,
                        height: 900,
                    },
                ),
                (
                    "compact".to_owned(),
                    Artboard {
                        width: 1024,
                        height: 768,
                    },
                ),
                (
                    "narrow".to_owned(),
                    Artboard {
                        width: 640,
                        height: 480,
                    },
                ),
            ]),
            scales: vec![1, 2],
            themes: vec!["ink".to_owned(), "glacier".to_owned()],
            motion: vec!["reduced".to_owned(), "full".to_owned()],
            states: vec![
                "rest".to_owned(),
                "hover".to_owned(),
                "focus".to_owned(),
                "pressed".to_owned(),
                "disabled".to_owned(),
            ],
            routes: vec![
                "orbit".to_owned(),
                "package".to_owned(),
                "page".to_owned(),
                "source".to_owned(),
            ],
            overlays: vec![
                "loading".to_owned(),
                "stale".to_owned(),
                "offline".to_owned(),
                "error".to_owned(),
            ],
            keyboard: vec![
                "tab".to_owned(),
                "shift-tab".to_owned(),
                "enter".to_owned(),
                "escape".to_owned(),
                "focus-visible".to_owned(),
                "accessibility-tree".to_owned(),
                "animation-frame".to_owned(),
            ],
        }
    }
}

/// Errors emitted while extracting or validating the HTML contract.
#[derive(Debug, Error)]
pub enum DesignContractError {
    /// A required artifact is missing.
    #[error("design contract artifact is missing: {0}")]
    MissingArtifact(String),
    /// An artifact is malformed or omits a required baseline.
    #[error("design contract artifact is malformed: {0}")]
    Malformed(String),
    /// Reading a source artifact failed.
    #[error("design contract source I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSON encoding/decoding failed.
    #[error("design contract JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

impl DesignContract {
    /// Extracts all four artifacts from a contract root.
    pub fn extract(root: &Path) -> Result<Self, DesignContractError> {
        let mut artifacts = Vec::with_capacity(REQUIRED_ARTIFACTS.len());
        let mut source_bytes = Vec::new();
        for (id, title, relative) in REQUIRED_ARTIFACTS {
            let path = root.join(relative);
            let bytes = std::fs::read(&path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    DesignContractError::MissingArtifact(path.display().to_string())
                } else {
                    DesignContractError::Io(error)
                }
            })?;
            let html = String::from_utf8_lossy(&bytes);
            let actual_title = html_title(&html).ok_or_else(|| {
                DesignContractError::Malformed(format!("{relative}: missing <title>"))
            })?;
            if actual_title != title {
                return Err(DesignContractError::Malformed(format!(
                    "{relative}: title {actual_title:?} does not match {title:?}"
                )));
            }
            let preview = preview_artboard(&html).ok_or_else(|| {
                DesignContractError::Malformed(format!(
                    "{relative}: missing data-dc-script preview artboard"
                ))
            })?;
            let artboard = first_nx_artboard(&html).ok_or_else(|| {
                DesignContractError::Malformed(format!(
                    "{relative}: missing first .nx artboard dimensions"
                ))
            })?;
            if preview != artboard {
                return Err(DesignContractError::Malformed(format!(
                    "{relative}: preview {preview:?} disagrees with .nx {artboard:?}"
                )));
            }
            let semantics = extract_semantics(&html);
            source_bytes.extend_from_slice(relative.as_bytes());
            source_bytes.push(0);
            source_bytes.extend_from_slice(&bytes);
            artifacts.push(DesignArtifact {
                id: id.to_owned(),
                title: actual_title,
                path: relative.to_owned(),
                sha256: hash_bytes(&bytes),
                preview,
                artboard,
                semantics,
            });
        }
        let canonical = merge_semantics(&artifacts);
        Ok(Self {
            schema: DESIGN_CONTRACT_SCHEMA,
            contract_id: "nudox-design-system".to_owned(),
            source_root: "Nudox-Design-System".to_owned(),
            source_sha256: hash_bytes(&source_bytes),
            artifacts,
            canonical,
            capture_matrix: CaptureMatrix::default(),
        })
    }

    /// Returns a stable hash over the serialized contract.
    pub fn sha256(&self) -> Result<String, DesignContractError> {
        Ok(hash_bytes(&serde_json::to_vec(self)?))
    }

    /// Produces the compact identity written into each capture manifest.
    pub fn reference_metadata(&self) -> Result<ReferenceMetadata, DesignContractError> {
        Ok(ReferenceMetadata {
            schema: self.schema,
            contract_id: self.contract_id.clone(),
            contract_sha256: self.sha256()?,
            source_sha256: self.source_sha256.clone(),
            artifacts: self
                .artifacts
                .iter()
                .map(|artifact| ReferenceArtifact {
                    id: artifact.id.clone(),
                    path: artifact.path.clone(),
                    sha256: artifact.sha256.clone(),
                    artboard: artifact.artboard,
                })
                .collect(),
            capture_policy: "external-physical-capture-no-self-generated-baselines".to_owned(),
        })
    }

    /// Validates the contract has exactly four distinct, complete baselines.
    pub fn validate(&self) -> Result<(), DesignContractError> {
        if self.schema != DESIGN_CONTRACT_SCHEMA {
            return Err(DesignContractError::Malformed(format!(
                "unsupported schema {}",
                self.schema
            )));
        }
        if self.artifacts.len() != REQUIRED_ARTIFACTS.len() {
            return Err(DesignContractError::Malformed(format!(
                "expected {} artifacts, found {}",
                REQUIRED_ARTIFACTS.len(),
                self.artifacts.len()
            )));
        }
        if !is_sha256(&self.source_sha256) {
            return Err(DesignContractError::Malformed(
                "source_sha256 is not a lowercase SHA-256 digest".to_owned(),
            ));
        }
        let mut paths = std::collections::BTreeSet::new();
        for (artifact, (expected_id, expected_title, expected_path)) in
            self.artifacts.iter().zip(REQUIRED_ARTIFACTS)
        {
            if artifact.id != expected_id
                || artifact.title != expected_title
                || artifact.path != expected_path
            {
                return Err(DesignContractError::Malformed(format!(
                    "artifact {:?} does not match required reference {expected_id:?}",
                    artifact.id
                )));
            }
            if artifact.artboard.width == 0 || artifact.artboard.height == 0 {
                return Err(DesignContractError::Malformed(format!(
                    "{} has empty artboard",
                    artifact.id
                )));
            }
            if artifact.preview != artifact.artboard || !paths.insert(artifact.path.clone()) {
                return Err(DesignContractError::Malformed(format!(
                    "{} has a duplicate or inconsistent baseline",
                    artifact.id
                )));
            }
            if !is_sha256(&artifact.sha256) {
                return Err(DesignContractError::Malformed(format!(
                    "{} has an invalid source digest",
                    artifact.id
                )));
            }
        }
        Ok(())
    }

    /// Writes canonical JSON with stable formatting.
    pub fn write_json(&self, path: &Path) -> Result<(), DesignContractError> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self)?;
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Reads and validates a previously extracted contract.
    pub fn read_json(path: &Path) -> Result<Self, DesignContractError> {
        let contract: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        contract.validate()?;
        Ok(contract)
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn html_title(html: &str) -> Option<String> {
    let start = html.find("<title>")? + "<title>".len();
    let end = html[start..].find("</title>")? + start;
    Some(html[start..end].trim().to_owned())
}

fn preview_artboard(html: &str) -> Option<Artboard> {
    let marker = "data-props='";
    let start = html.find(marker)? + marker.len();
    let end = html[start..].find('\'')? + start;
    let props: serde_json::Value = serde_json::from_str(&html[start..end]).ok()?;
    let preview = props.get("$preview")?;
    Some(Artboard {
        width: preview.get("width")?.as_u64()?.try_into().ok()?,
        height: preview.get("height")?.as_u64()?.try_into().ok()?,
    })
}

fn first_nx_artboard(html: &str) -> Option<Artboard> {
    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find("<div class=\"nx") {
        let start = cursor + relative;
        let end = html[start..].find('>')? + start;
        let tag = &html[start..end];
        if let (Some(width), Some(height)) = (
            css_px_attribute(tag, "width"),
            css_px_attribute(tag, "height"),
        ) {
            return Some(Artboard { width, height });
        }
        cursor = end + 1;
    }
    None
}

fn css_px_attribute(tag: &str, name: &str) -> Option<u32> {
    let style_start = tag.find("style=\"")? + "style=\"".len();
    let style_end = tag[style_start..].find('"')? + style_start;
    for declaration in tag[style_start..style_end].split(';') {
        let (property, value) = declaration.split_once(':')?;
        if property.trim() == name {
            return value.trim().strip_suffix("px")?.trim().parse().ok();
        }
    }
    None
}

fn extract_style_blocks(html: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find("<style") {
        let start = cursor + relative;
        let Some(open_end_relative) = html[start..].find('>') else {
            break;
        };
        let content_start = start + open_end_relative + 1;
        let Some(close_relative) = html[content_start..].find("</style>") else {
            break;
        };
        let close = content_start + close_relative;
        blocks.push(html[content_start..close].to_owned());
        cursor = close + "</style>".len();
    }
    blocks
}

fn strip_css_comments(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative) = value[cursor..].find("/*") {
        let start = cursor + relative;
        output.push_str(&value[cursor..start]);
        let Some(end_relative) = value[start + 2..].find("*/") else {
            break;
        };
        cursor = start + 2 + end_relative + 2;
    }
    output.push_str(&value[cursor..]);
    output
}

#[derive(Clone, Debug)]
struct ParsedRule {
    selector: String,
    declarations: Vec<(String, String)>,
}

fn parse_rules(css: &str) -> Vec<ParsedRule> {
    let css = strip_css_comments(css);
    let mut rules = Vec::new();
    parse_rule_range(&css, 0, css.len(), "", &mut rules);
    rules
}

fn parse_rule_range(
    css: &str,
    mut start: usize,
    end: usize,
    prefix: &str,
    output: &mut Vec<ParsedRule>,
) {
    while start < end {
        while start < end && css.as_bytes()[start].is_ascii_whitespace() {
            start += 1;
        }
        if start >= end {
            break;
        }
        let Some(open_relative) = find_unquoted(css, start, end, '{') else {
            break;
        };
        let selector = css[start..open_relative].trim();
        let Some(close) = matching_brace(css, open_relative, end) else {
            break;
        };
        let body = &css[open_relative + 1..close];
        if body.contains('{') {
            let nested_prefix = if prefix.is_empty() {
                selector.to_owned()
            } else {
                format!("{prefix} {selector}")
            };
            parse_rule_range(body, 0, body.len(), &nested_prefix, output);
        } else {
            let selector = if prefix.is_empty() {
                selector.to_owned()
            } else {
                format!("{prefix} {selector}")
            };
            if !selector.is_empty() {
                let declarations = parse_declarations(body);
                if !declarations.is_empty() {
                    output.push(ParsedRule {
                        selector,
                        declarations,
                    });
                }
            }
        }
        start = close + 1;
    }
}

fn find_unquoted(value: &str, start: usize, end: usize, needle: char) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut quote = None;
    let mut escaped = false;
    let mut parentheses = 0_u32;
    for index in start..end {
        let ch = bytes[index] as char;
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == '(' {
            parentheses = parentheses.saturating_add(1);
        } else if ch == ')' {
            parentheses = parentheses.saturating_sub(1);
        } else if ch == needle && parentheses == 0 {
            return Some(index);
        }
    }
    None
}

fn matching_brace(value: &str, open: usize, end: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    for index in open..end {
        let ch = bytes[index] as char;
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == '{' {
            depth = depth.saturating_add(1);
        } else if ch == '}' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn parse_declarations(body: &str) -> Vec<(String, String)> {
    let mut declarations = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    let mut parentheses = 0_u32;
    let bytes = body.as_bytes();
    for index in 0..=body.len() {
        let at_end = index == body.len();
        let ch = if at_end { ';' } else { bytes[index] as char };
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(current) = quote {
            if ch == current {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == '(' {
            parentheses = parentheses.saturating_add(1);
        } else if ch == ')' {
            parentheses = parentheses.saturating_sub(1);
        } else if ch == ';' && parentheses == 0 {
            if let Some((property, value)) = body[start..index].split_once(':') {
                let property = property.trim();
                let value = value.trim();
                if !property.is_empty() && !value.is_empty() {
                    declarations.push((property.to_owned(), value.to_owned()));
                }
            }
            start = index + 1;
        }
    }
    declarations
}

fn extract_semantics(html: &str) -> DesignSemantics {
    let mut semantics = DesignSemantics::default();
    let mut order = 0;
    for style in extract_style_blocks(html) {
        for rule in parse_rules(&style) {
            for (property, value) in rule.declarations {
                let declaration = StyleDeclaration {
                    selector: rule.selector.clone(),
                    property: property.clone(),
                    value: value.clone(),
                    order,
                };
                order += 1;
                if property.starts_with("--") {
                    let token = TokenDeclaration {
                        name: property.clone(),
                        selector: rule.selector.clone(),
                        value: value.clone(),
                        order: declaration.order,
                    };
                    let group = token_group(&property);
                    if let Some(group) = group {
                        semantics
                            .palette
                            .entry(group.to_owned())
                            .or_default()
                            .push(token);
                    }
                }
                let groups = semantic_groups(&rule.selector, &property, &value);
                semantics.declarations.push(declaration.clone());
                for group in groups {
                    let destination = match group {
                        SemanticGroup::Typography => &mut semantics.typography,
                        SemanticGroup::Spacing => &mut semantics.spacing,
                        SemanticGroup::Bevel => &mut semantics.bevel,
                        SemanticGroup::Cuts => &mut semantics.cuts,
                        SemanticGroup::Glyphs => &mut semantics.glyphs,
                        SemanticGroup::Motion => &mut semantics.motion,
                        SemanticGroup::Focus => &mut semantics.focus,
                    };
                    destination
                        .entry(rule.selector.clone())
                        .or_default()
                        .push(declaration.clone());
                }
            }
        }
    }
    semantics.svg_inventory = extract_svg_inventory(html);
    semantics
}

#[derive(Clone, Copy)]
enum SemanticGroup {
    Typography,
    Spacing,
    Bevel,
    Cuts,
    Glyphs,
    Motion,
    Focus,
}

fn token_group(name: &str) -> Option<&'static str> {
    let bare = name.strip_prefix("--")?;
    if bare.starts_with('g') && bare[1..].chars().all(|ch| ch.is_ascii_digit())
        || bare.starts_with("line")
        || bare.starts_with("ink")
        || matches!(
            bare,
            "mint"
                | "teal"
                | "leaf"
                | "mint-ink"
                | "signal"
                | "peri"
                | "peri-hi"
                | "amber"
                | "coral"
                | "bevel-hi"
                | "bevel-lo"
                | "deep"
                | "shaft"
                | "weave"
                | "well"
                | "pane"
                | "inset"
                | "tint"
                | "tint2"
                | "veil"
                | "card"
                | "atmo"
                | "plate"
                | "plate2"
                | "plate3"
                | "table"
                | "glass"
        )
        || bare.starts_with("signal-")
        || bare.starts_with("peri-")
        || bare.starts_with("amber-")
        || bare.starts_with("coral-")
        || bare.starts_with("f-")
    {
        Some("palette")
    } else {
        None
    }
}

fn semantic_groups(selector: &str, property: &str, value: &str) -> Vec<SemanticGroup> {
    let selector_lower = selector.to_ascii_lowercase();
    let property_lower = property.to_ascii_lowercase();
    let value_lower = value.to_ascii_lowercase();
    let mut groups = Vec::new();
    if property.starts_with("--")
        && (property.contains("display")
            || property.contains("ui")
            || property.contains("mono")
            || property.contains("serif")
            || property.starts_with("--t-"))
        || property_lower.contains("font")
        || property_lower.contains("letter-spacing")
        || property_lower.contains("line-height")
        || selector_lower.contains(".t-")
        || selector_lower.contains(".hero-name")
        || selector_lower.contains(".doc h")
    {
        groups.push(SemanticGroup::Typography);
    }
    if property_lower.contains("gap")
        || property_lower.contains("padding")
        || property_lower.contains("margin")
        || property_lower.contains("width")
        || property_lower.contains("height")
        || property_lower.contains("min-")
        || property_lower.contains("max-")
        || property_lower.contains("grid-template")
        || property_lower.contains("border-radius")
        || selector_lower.contains(".g")
    {
        groups.push(SemanticGroup::Spacing);
    }
    if property_lower.contains("box-shadow")
        || property_lower.contains("bevel")
        || property_lower.contains("weave")
        || property_lower.contains("shaft")
        || property_lower.contains("elevation")
        || property_lower == "--e1"
        || property_lower == "--e2"
        || property_lower == "--e3"
    {
        groups.push(SemanticGroup::Bevel);
    }
    if property_lower.contains("clip-path")
        || selector_lower.contains(".cut")
        || value_lower.contains("polygon(")
        || selector_lower.contains("data-tip")
    {
        groups.push(SemanticGroup::Cuts);
    }
    if selector_lower.contains(".ico")
        || selector_lower.contains(".logo")
        || selector_lower.contains(".gem")
        || selector_lower.contains(".bead")
        || selector_lower.contains(".glyph")
        || selector_lower.contains(".mod")
        || selector_lower.contains(".k ")
        || selector_lower == ".k"
        || selector_lower.contains("stroke")
        || property_lower == "stroke-width"
    {
        groups.push(SemanticGroup::Glyphs);
    }
    if property_lower.contains("animation")
        || property_lower.contains("transition")
        || property_lower.contains("transform")
        || property_lower.contains("timing")
        || property.starts_with("--t-")
        || property.starts_with("--bounce")
        || property.starts_with("--drop")
        || selector_lower.contains("keyframes")
    {
        groups.push(SemanticGroup::Motion);
    }
    if selector_lower.contains("focus")
        || selector_lower.contains("hover")
        || selector_lower.contains("active")
        || selector_lower.contains("press")
        || property_lower == "outline"
        || property_lower == "outline-offset"
    {
        groups.push(SemanticGroup::Focus);
    }
    groups
}

fn extract_svg_inventory(html: &str) -> Vec<SvgGlyph> {
    let mut output = Vec::new();
    let mut cursor = 0;
    while let Some(relative) = html[cursor..].find("<svg") {
        let start = cursor + relative;
        let Some(open_end_relative) = html[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_relative;
        let tag = &html[start..open_end];
        let class = html_attribute(tag, "class");
        let label = html_attribute(tag, "aria-label");
        let view_box = html_attribute(tag, "viewBox");
        let width = html_attribute(tag, "width");
        let height = html_attribute(tag, "height");
        let close = html[open_end + 1..]
            .find("</svg>")
            .map_or(html.len(), |offset| open_end + 1 + offset);
        let body = &html[open_end + 1..close];
        let shape_count = [
            "<path",
            "<line",
            "<polyline",
            "<polygon",
            "<circle",
            "<rect",
            "<g ",
        ]
        .iter()
        .map(|needle| body.matches(needle).count())
        .sum();
        output.push(SvgGlyph {
            class,
            label,
            view_box,
            width,
            height,
            shape_count,
        });
        cursor = close.saturating_add("</svg>".len());
    }
    output
}

fn html_attribute(tag: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let start = tag.find(&marker)? + marker.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_owned())
}

fn merge_semantics(artifacts: &[DesignArtifact]) -> DesignSemantics {
    let mut merged = DesignSemantics::default();
    for artifact in artifacts {
        let prefix = artifact.id.clone();
        for (group, values) in &artifact.semantics.palette {
            for value in values {
                merged
                    .palette
                    .entry(group.clone())
                    .or_default()
                    .push(TokenDeclaration {
                        selector: format!("{prefix}:{}", value.selector),
                        ..value.clone()
                    });
            }
        }
        merge_styles(
            &mut merged.typography,
            &artifact.semantics.typography,
            &prefix,
        );
        merge_styles(&mut merged.spacing, &artifact.semantics.spacing, &prefix);
        merge_styles(&mut merged.bevel, &artifact.semantics.bevel, &prefix);
        merge_styles(&mut merged.cuts, &artifact.semantics.cuts, &prefix);
        merge_styles(&mut merged.glyphs, &artifact.semantics.glyphs, &prefix);
        merge_styles(&mut merged.motion, &artifact.semantics.motion, &prefix);
        merge_styles(&mut merged.focus, &artifact.semantics.focus, &prefix);
        for declaration in &artifact.semantics.declarations {
            merged.declarations.push(StyleDeclaration {
                selector: format!("{prefix}:{}", declaration.selector),
                ..declaration.clone()
            });
        }
        merged
            .svg_inventory
            .extend(artifact.semantics.svg_inventory.clone());
    }
    merged
}

fn merge_styles(
    destination: &mut BTreeMap<String, Vec<StyleDeclaration>>,
    source: &BTreeMap<String, Vec<StyleDeclaration>>,
    prefix: &str,
) {
    for (selector, declarations) in source {
        destination
            .entry(format!("{prefix}:{selector}"))
            .or_default()
            .extend(declarations.iter().cloned());
    }
}

/// Human-readable summary useful in CI logs.
pub fn summarize_contract(contract: &DesignContract) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "{}: {} artifacts, source sha256 {}",
        contract.contract_id,
        contract.artifacts.len(),
        contract.source_sha256
    );
    for artifact in &contract.artifacts {
        let _ = writeln!(
            output,
            "  {} {} {}x{} sha256 {}",
            artifact.id,
            artifact.path,
            artifact.artboard.width,
            artifact.artboard.height,
            artifact.sha256
        );
    }
    output
}

/// Finds the contract root from an explicit path or the standard checkout
/// sibling used by the GUI lanes.
pub fn resolve_contract_root(explicit: Option<&Path>) -> Result<PathBuf, DesignContractError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Ok(path) = std::env::var("NUDOX_DESIGN_CONTRACT_ROOT") {
        return Ok(PathBuf::from(path));
    }
    let current = std::env::current_dir()?;
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut candidates = vec![
        current.join("Nudox-Design-System"),
        current.join("../backend/Nudox-Design-System"),
        manifest_root.join("../../Nudox-Design-System"),
    ];
    // A checked-out worktree may sit below the repository that owns the
    // design archive. Walk ancestors instead of baking a developer-specific
    // absolute path into the capture binary.
    for ancestor in current.ancestors().skip(1) {
        candidates.push(ancestor.join("Nudox-Design-System"));
    }
    candidates
        .into_iter()
        .find(|path| path.join("artifacts").is_dir())
        .ok_or_else(|| {
            DesignContractError::MissingArtifact(
                "set NUDOX_DESIGN_CONTRACT_ROOT or pass --contract-root".to_owned(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System")
    }

    #[test]
    fn extracts_all_reference_artboards_and_core_semantics() {
        let root = contract_root();
        if !root.is_dir() {
            return;
        }
        let contract = DesignContract::extract(&root).expect("design contract");
        contract.validate().expect("valid contract");
        assert_eq!(contract.artifacts.len(), 4);
        assert_eq!(
            contract.artifacts[0].artboard,
            Artboard {
                width: 1440,
                height: 1160
            }
        );
        assert!(contract.canonical.palette.contains_key("palette"));
        assert!(!contract.canonical.typography.is_empty());
        assert!(!contract.canonical.spacing.is_empty());
        assert!(!contract.canonical.bevel.is_empty());
        assert!(!contract.canonical.cuts.is_empty());
        assert!(!contract.canonical.glyphs.is_empty());
        assert!(!contract.canonical.motion.is_empty());
        assert!(!contract.canonical.focus.is_empty());
    }

    #[test]
    fn parser_preserves_parentheses_and_quoted_values() {
        let rules = parse_rules(
            ".x{--a:linear-gradient(180deg,#fff,#000);font-family:\"A;B\",sans-serif;clip-path:polygon(0 0,100% 0)}",
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].declarations.len(), 3);
        assert_eq!(
            rules[0].declarations[0].1,
            "linear-gradient(180deg,#fff,#000)"
        );
        assert_eq!(rules[0].declarations[1].1, "\"A;B\",sans-serif");
    }

    #[test]
    fn missing_contract_artifact_fails_closed() {
        let path = std::env::temp_dir().join(format!(
            "nudox-design-contract-missing-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temp root");
        let result = DesignContract::extract(&path);
        assert!(matches!(
            result,
            Err(DesignContractError::MissingArtifact(_))
        ));
        let _ = std::fs::remove_dir_all(path);
    }
}
