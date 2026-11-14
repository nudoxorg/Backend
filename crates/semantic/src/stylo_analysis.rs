//! Native Stylo stylesheet facts. Source locations are claims in the native
//! parser's line/column coordinate system, never host byte extents.
use serde::{Deserialize, Serialize};

/// Explicit parser resource policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloLimits {
    /// Maximum input bytes.
    pub input_bytes: u64,
    /// Maximum number of native rules.
    pub rules: u32,
    /// Maximum native hierarchy depth.
    pub depth: u32,
    /// Maximum selector, declaration and dependency records combined.
    pub records: u32,
    /// Maximum parse diagnostic records.
    pub diagnostics: u32,
    /// Maximum total emitted text bytes.
    pub text_bytes: u64,
}
/// Input decoding profile for this native adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloSourceKind {
    /// Standalone UTF-8 transport bytes; a leading UTF-8 BOM is decoded away.
    /// This is not a Unicode fragment extracted from an enclosing host.
    StandaloneUtf8,
    /// Verbatim UTF-8 content from a verified enclosing source host. A leading
    /// U+FEFF is content and is not removed as a transport BOM.
    EmbeddedUnicodeFragment,
}
/// Native parser context; unsupported choices are rejected by the producer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloParseConfig {
    /// Explicit input decoding profile.
    pub source_kind: StyloSourceKind,
    /// Absolute stylesheet base URL; this is a parsing input, not a fetched resource.
    pub base_url: String,
    /// Whether author-origin parsing is selected.
    pub author_origin: bool,
    /// Whether no-quirks mode is selected.
    pub no_quirks: bool,
    /// Whether imports are parsed; no stylesheet loader is supplied.
    pub allow_imports: bool,
    /// Effective attempt limits.
    pub limits: StyloLimits,
}
/// Ordered Servo process-global preferences captured around a parse.
/// Array order is scoped to the locked `stylo_static_prefs` version.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloPreferenceSnapshot {
    /// Global Boolean preference values in native array order.
    pub booleans: Vec<bool>,
    /// Global integer preference values in native array order.
    pub integers: Vec<i32>,
}
/// Stability of the native global preference basis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloPreferenceStability {
    /// Equal before/after snapshots do not exclude concurrent ABA mutations.
    Unknown,
}
/// Native start location, not a verified byte range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloLocation {
    /// Zero-based line reported by cssparser/Stylo.
    pub line: u32,
    /// One-based UTF-16 column reported by cssparser/Stylo.
    pub column: u32,
    /// Whether the native reporter says this location is approximate.
    pub approximate: bool,
}
/// Rule kinds exposed by this finite native projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloRuleKind {
    /// CSS style rule.
    Style,
    /// Media condition and nested rules.
    Media,
    /// Another native rule; its specific semantics are not projected.
    Other,
}
/// A native rule within a single captured host revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloRule {
    /// Ordered tree path of zero-based sibling indices.
    pub path: Vec<u32>,
    /// Native rule category.
    pub kind: StyloRuleKind,
    /// Exact native CssRuleType variant name, including unprojected types.
    pub native_kind: String,
    /// Native serialized condition, where projected; not evaluated.
    pub condition: Option<String>,
    /// Native rule start, where projected.
    pub location: Option<StyloLocation>,
}
/// One selector in a Stylo selector list.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloSelector {
    /// Owning rule tree path.
    pub rule_path: Vec<u32>,
    /// Index in the native selector list.
    pub ordinal: u32,
    /// Native serialization; not verbatim source.
    pub canonical: String,
    /// Packed Stylo specificity.
    pub specificity: u32,
    /// Direct native class/ID components in matching order.
    pub components: Vec<StyloSelectorComponent>,
    /// Nested selector-list component enumeration remains unknown.
    pub component_coverage: StyloDependencyCoverage,
}
/// One direct native selector component, not an authored token occurrence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloSelectorComponent {
    /// Native component kind.
    pub kind: StyloSelectorComponentKind,
    /// Native decoded identifier.
    pub name: String,
}
/// Native selector component categories in this finite projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloSelectorComponentKind {
    /// Class selector.
    Class,
    /// ID selector.
    Id,
}
/// Reference enumeration capability for a retained declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloDependencyCoverage {
    /// Native API exposed all parsed references for this value.
    Known,
    /// Native API did not expose a complete reference list.
    Unknown,
}
/// Variant category of a native retained Stylo declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloDeclarationKind {
    /// A native custom-property declaration.
    Custom,
    /// A value awaiting variable substitution.
    WithVariables,
    /// A CSS-wide keyword.
    CssWideKeyword,
    /// A native typed longhand value.
    TypedLonghand,
}
/// A declaration retained by Stylo after its normalization and deduplication.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloDeclaration {
    /// Owning rule tree path.
    pub rule_path: Vec<u32>,
    /// Index in the native retained declaration block.
    pub ordinal: u32,
    /// Native declaration variant category.
    pub kind: StyloDeclarationKind,
    /// Native property name, not an authored name occurrence.
    pub property: String,
    /// Native value serialization; may be empty for expanded variable shorthands.
    pub value: String,
    /// Whether Stylo supplied a nonempty native value serialization.
    pub value_available: bool,
    /// Native importance flag.
    pub important: bool,
    /// Reference extraction capability for this declaration.
    pub dependency_coverage: StyloDependencyCoverage,
}
/// One native parsed substitution reference; this does not resolve a provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloVariableDependency {
    /// Owning rule tree path.
    pub rule_path: Vec<u32>,
    /// Native retained declaration index.
    pub declaration_ordinal: u32,
    /// Native parsed reference name.
    pub name: String,
    /// Native substitution kind (`var`, `env`, or `attr`).
    pub kind: String,
    /// Structural sibling-ordinal path through fallback reference lists.
    pub reference_path: Vec<u32>,
}
/// A Stylo parser diagnostic, with approximate native location.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloDiagnostic {
    /// Approximate native location.
    pub location: StyloLocation,
    /// Native contextual parse diagnostic, not an exact source extent.
    pub message: String,
}
/// Coverage only of the projected native rule kinds and parse diagnostics.
/// Selector components, value dependencies, authored sites, matching and
/// cascade each have separate capability states; this is not global coverage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloCoverage {
    /// No reported parse or projection gaps.
    Complete,
    /// Diagnostics or unprojected rule kinds qualify the result.
    Partial,
}
/// Supported exact authored source occurrence role.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloSourceRole {
    /// Class identifier consumed by the native selector parser.
    ClassName,
    /// Custom-property name accepted before native declaration deduplication.
    CustomPropertyDeclarationName,
}
/// Relationship of a written declaration to the retained native block.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloSourceDisposition {
    /// Retained or overridden status has not been established.
    Unknown,
}
/// One original-host byte occurrence witnessed by the native parser observer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloSourceOccurrence {
    /// Native top-level rule path for this captured host revision.
    pub rule_path: Vec<u32>,
    /// Selector index for class sites; absent for declaration names.
    pub selector_ordinal: Option<u32>,
    /// Authored lexical role.
    pub role: StyloSourceRole,
    /// Inclusive start byte in original UTF-8 host.
    pub start: u64,
    /// Exclusive end byte in original UTF-8 host.
    pub end: u64,
    /// Verbatim source spelling.
    pub raw: String,
    /// Native decoded identifier.
    pub decoded: String,
    /// Retained/overridden state is not inferred from equal names.
    pub disposition: StyloSourceDisposition,
}
/// Explicit capability gaps in the authored source observer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum StyloSourceGap {
    /// Nested and complex selector/rule source roles are outside the V1 observer.
    NestedOrComplexUncovered,
    /// Authored custom-property reference sites are not yet observed.
    CustomPropertyReferencesUncovered,
    /// The native observer skipped syntax or rolled back an unowned event.
    ObserverSkipped,
    /// Native parser reported a rejected or unsupported region.
    ParserDiagnostic,
}
/// Bounded native stylesheet analysis for one host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloAnalysis {
    /// Explicit parser and resource basis.
    pub config: StyloParseConfig,
    /// Exact producer and dependency recipe identifier.
    pub recipe: String,
    /// Fingerprint of adapter, schema, compiler and native dependency closure.
    pub support_fingerprint: [u8; 32],
    /// Whether the leading UTF-8 BOM was removed before native parsing.
    /// Native line/column coordinates refer to the remaining parser input.
    pub utf8_bom_removed: bool,
    /// Captured native process-global preference values before parsing.
    pub preferences: StyloPreferenceSnapshot,
    /// Stability cannot be certified without exclusive preference ownership.
    pub preference_stability: StyloPreferenceStability,
    /// Native rules in source order.
    pub rules: Vec<StyloRule>,
    /// Native selectors.
    pub selectors: Vec<StyloSelector>,
    /// Native retained declarations, not the authored declaration inventory.
    pub declarations: Vec<StyloDeclaration>,
    /// Native variable dependency references where publicly available.
    pub dependencies: Vec<StyloVariableDependency>,
    /// Exact source occurrences proven by the bounded observer subset.
    pub occurrences: Vec<StyloSourceOccurrence>,
    /// Source inventory coverage; V1 is partial for unsupported syntax/roles.
    pub source_coverage: StyloCoverage,
    /// Explicit bounded source-observer capability gaps.
    pub source_gaps: Vec<StyloSourceGap>,
    /// Versioned source-observer mapping recipe; bound by support fingerprint.
    pub source_observer_recipe: String,
    /// Native parser and projection diagnostics.
    pub diagnostics: Vec<StyloDiagnostic>,
    /// Coverage of this finite projection.
    pub coverage: StyloCoverage,
}
