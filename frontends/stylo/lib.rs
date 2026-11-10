//! Native Stylo CSS stylesheet projection. It does not provide host-byte
//! occurrence ranges or evaluate selectors, cascade, conditions, or imports.
use backend_semantic::stylo_analysis::*;
/// Isolated worker framing and supervised invocation.
pub mod worker_protocol;
use cssparser::SourceLocation;
use std::{
    cell::RefCell,
    fmt::{self, Write},
};
struct BoundedText {
    text: String,
    limit: usize,
}
impl Write for BoundedText {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if s.len() > self.limit.saturating_sub(self.text.len()) {
            return Err(fmt::Error);
        }
        self.text.push_str(s);
        Ok(())
    }
}
use style::{
    context::QuirksMode,
    custom_properties::{References, SubstitutionFunctionKind},
    error_reporting::{ContextualParseError, ParseErrorReporter},
    properties::{CustomDeclarationValue, PropertyDeclaration},
    shared_lock::SharedRwLock,
    stylesheets::{AllowImportRules, CssRule, Origin, StylesheetContents, UrlExtraData},
};

/// Fixed native parser package and output semantics for this projection.
/// Build-bound fingerprint of the real adapter, neutral schema and native closure.
#[must_use]
pub fn support_fingerprint() -> [u8; 32] {
    fingerprint_parts(&[
        include_bytes!("lib.rs").as_slice(),
        include_bytes!("worker_protocol.rs").as_slice(),
        include_bytes!("worker.rs").as_slice(),
        include_bytes!("build.rs").as_slice(),
        include_bytes!("Cargo.toml").as_slice(),
        include_bytes!("../../crates/semantic/src/stylo_analysis.rs").as_slice(),
        env!("STYLO_NATIVE_BUILD_BASIS").as_bytes(),
    ])
}
fn fingerprint_parts(parts: &[&[u8]]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-native-support.v1\0");
    for part in parts {
        hash.update(&(part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    *hash.finalize().as_bytes()
}
/// Fixed native parser package and output semantics for this projection.
pub const RECIPE: &str = "stylo-native-v1;stylo=0.23.0;checksum=84fe6be1e185e318b93be8f1e025667c196584a9bc9d5b7aca34bba4a07fb488;selectors=0.42.0;cssparser=0.38.0;servo;native-declarations=deduplicated;correspondence=unknown;runtime=not-attempted";
/// Distinct source profile for verbatim embedded Unicode fragments.
pub const EMBEDDED_RECIPE: &str = "stylo-native-embedded-v1;verbatim-unicode;no-bom-strip;host-offset-map=checked;runtime=not-attempted";
/// A failed native analysis attempt; no empty-complete facts may be inferred.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StyloError {
    /// Non-UTF8 source.
    InvalidUtf8,
    /// Source or projected output exceeded its configured budget.
    Limit,
    /// Base URL or parse context is unsupported.
    InvalidConfig,
    /// Caller requested cancellation.
    Cancelled,
    /// Native global preference snapshots changed during the attempt.
    PreferencesChanged,
    /// Native CSS serialization failed.
    Serialization,
}
fn preferences() -> StyloPreferenceSnapshot {
    use std::sync::atomic::Ordering;
    StyloPreferenceSnapshot {
        booleans: stylo_static_prefs::BOOLS
            .iter()
            .map(|v| v.load(Ordering::SeqCst))
            .collect(),
        integers: stylo_static_prefs::INTEGERS
            .iter()
            .map(|v| v.load(Ordering::SeqCst))
            .collect(),
    }
}
fn location(value: SourceLocation, approximate: bool) -> StyloLocation {
    StyloLocation {
        line: value.line,
        column: value.column,
        approximate,
    }
}
struct Reporter {
    items: RefCell<Vec<StyloDiagnostic>>,
    max: usize,
    overflow: RefCell<bool>,
    text_max: usize,
    text_used: RefCell<usize>,
}
impl ParseErrorReporter for Reporter {
    fn report_error(&self, _: &UrlExtraData, where_: SourceLocation, error: ContextualParseError) {
        let mut items = self.items.borrow_mut();
        let mut used = self.text_used.borrow_mut();
        if items.len() >= self.max {
            *self.overflow.borrow_mut() = true;
            return;
        }
        let mut text = BoundedText {
            text: String::new(),
            limit: self.text_max.saturating_sub(*used),
        };
        if write!(&mut text, "{error:?}").is_err() {
            *self.overflow.borrow_mut() = true;
            return;
        }
        *used += text.text.len();
        items.push(StyloDiagnostic {
            location: location(where_, true),
            message: text.text,
        });
    }
}
/// Analyze a standalone UTF-8 byte host using real Stylo.
/// This API does not accept embedded Unicode fragments as standalone input. The parser has no public
/// in-parse cancellation hook: cancellation is checked before and after parsing
/// and while projecting. Do not describe this as interruptible parsing.
///
/// # Errors
/// Invalid UTF-8/configuration, cancellation, serialization or an exhausted
/// limit aborts the result; the caller must retain an incomplete host terminal.
pub fn analyze_with_cancel(
    bytes: &[u8],
    config: StyloParseConfig,
    mut cancelled: impl FnMut() -> bool,
) -> Result<StyloAnalysis, StyloError> {
    if config.source_kind != StyloSourceKind::StandaloneUtf8 {
        return Err(StyloError::InvalidConfig);
    }
    analyze_input(bytes, config, &mut cancelled)
}

/// Analyze an exact verified fragment without interpreting its leading U+FEFF
/// as a standalone transport BOM. Occurrence offsets are fragment-relative;
/// map them through the supplied verified extent before using host locations.
pub fn analyze_embedded_with_cancel(
    fragment: &backend_compile::VerifiedSourceExtent<'_>,
    config: StyloParseConfig,
    mut cancelled: impl FnMut() -> bool,
) -> Result<StyloAnalysis, StyloError> {
    if config.source_kind != StyloSourceKind::EmbeddedUnicodeFragment {
        return Err(StyloError::InvalidConfig);
    }
    analyze_input(fragment.text().as_bytes(), config, &mut cancelled)
}

fn analyze_input(
    bytes: &[u8],
    config: StyloParseConfig,
    cancelled: &mut impl FnMut() -> bool,
) -> Result<StyloAnalysis, StyloError> {
    if cancelled() {
        return Err(StyloError::Cancelled);
    }
    if bytes.len() as u64 > config.limits.input_bytes {
        return Err(StyloError::Limit);
    }
    if !config.author_origin || !config.no_quirks || config.allow_imports {
        return Err(StyloError::InvalidConfig);
    }
    let source = std::str::from_utf8(bytes).map_err(|_| StyloError::InvalidUtf8)?;
    // The CSS decoding stage removes a leading UTF-8 BOM. `from_str` does
    // not do this itself and would otherwise parse U+FEFF as an identifier.
    let (input, utf8_bom_removed) = match config.source_kind {
        StyloSourceKind::StandaloneUtf8 => source
            .strip_prefix('\u{feff}')
            .map_or((source, false), |rest| (rest, true)),
        StyloSourceKind::EmbeddedUnicodeFragment => (source, false),
    };
    let url = url::Url::parse(&config.base_url).map_err(|_| StyloError::InvalidConfig)?;
    let reporter = Reporter {
        items: RefCell::new(Vec::new()),
        max: config.limits.diagnostics as usize,
        overflow: RefCell::new(false),
        text_max: usize::try_from(config.limits.text_bytes).unwrap_or(usize::MAX),
        text_used: RefCell::new(0),
    };
    let before_preferences = preferences();
    let host_token = *blake3::hash(bytes).as_bytes();
    let observer = style::nudox_observer::begin(
        host_token,
        config.limits.records as usize,
        usize::try_from(config.limits.text_bytes).unwrap_or(usize::MAX),
    )
    .ok_or(StyloError::InvalidConfig)?;
    let lock = SharedRwLock::new();
    let sheet = StylesheetContents::from_str(
        input,
        UrlExtraData::from(url),
        Origin::Author,
        &lock,
        None,
        Some(&reporter),
        QuirksMode::NoQuirks,
        AllowImportRules::No,
        None,
    );
    if cancelled() {
        return Err(StyloError::Cancelled);
    }
    if before_preferences != preferences() {
        return Err(StyloError::PreferencesChanged);
    }
    let (observed_host, declaration_events, class_events, observer_overflow, skipped) =
        observer.finish().ok_or(StyloError::InvalidConfig)?;
    if observed_host != host_token {
        return Err(StyloError::InvalidConfig);
    }
    if observer_overflow {
        return Err(StyloError::Limit);
    }
    if *reporter.overflow.borrow() {
        return Err(StyloError::Limit);
    }
    let guard = lock.read();
    let diagnostics = reporter.items.into_inner();
    let mut text_used = *reporter.text_used.borrow();
    let has_diagnostics = !diagnostics.is_empty();
    let embedded = config.source_kind == StyloSourceKind::EmbeddedUnicodeFragment;
    let recipe = match config.source_kind {
        StyloSourceKind::StandaloneUtf8 => RECIPE,
        StyloSourceKind::EmbeddedUnicodeFragment => EMBEDDED_RECIPE,
    }
    .to_owned();
    let mut result = StyloAnalysis {
        config,
        recipe,
        support_fingerprint: support_fingerprint(),
        utf8_bom_removed,
        preferences: before_preferences.clone(),
        preference_stability: StyloPreferenceStability::Unknown,
        rules: Vec::new(),
        selectors: Vec::new(),
        declarations: Vec::new(),
        dependencies: Vec::new(),
        // The process-global preference snapshot has no exclusive-ownership proof.
        coverage: StyloCoverage::Partial,
        diagnostics,
        occurrences: Vec::new(),
        source_coverage: StyloCoverage::Partial,
        source_gaps: {
            let mut gaps = vec![StyloSourceGap::NestedOrComplexUncovered, StyloSourceGap::CustomPropertyReferencesUncovered];
            if skipped { gaps.push(StyloSourceGap::ObserverSkipped); }
            if has_diagnostics { gaps.push(StyloSourceGap::ParserDiagnostic); }
            gaps
        },
        source_observer_recipe: if !embedded { "stylo-observer-top-level-v1;native-class-token;accepted-custom-name;utf8-bom-map;disposition=unknown" } else { "stylo-observer-top-level-v1;native-class-token;accepted-custom-name;no-bom-removal;disposition=unknown" }.to_owned(),
    };
    fn add_text(used: &mut usize, text: &str, limit: u64) -> Result<(), StyloError> {
        *used = used.checked_add(text.len()).ok_or(StyloError::Limit)?;
        if *used as u64 > limit {
            return Err(StyloError::Limit);
        }
        Ok(())
    }
    fn add_dependencies(
        refs: &References,
        path: &[u32],
        ordinal: u32,
        ref_path: &[u32],
        result: &mut StyloAnalysis,
        used: &mut usize,
        records_used: &mut usize,
    ) -> Result<(), StyloError> {
        if ref_path.len() > result.config.limits.depth as usize {
            return Err(StyloError::Limit);
        }
        for (index, r) in refs.refs.iter().enumerate() {
            let mut current_path = ref_path.to_vec();
            current_path.push(u32::try_from(index).map_err(|_| StyloError::Limit)?);
            if current_path.len() > result.config.limits.depth as usize {
                return Err(StyloError::Limit);
            }
            if *records_used >= result.config.limits.records as usize {
                return Err(StyloError::Limit);
            }
            *records_used += 1;
            let name = r.name.as_str().to_owned();
            let kind = match r.substitution_kind {
                SubstitutionFunctionKind::Var => "var",
                SubstitutionFunctionKind::Env => "env",
                SubstitutionFunctionKind::Attr => "attr",
            }
            .to_owned();
            add_text(used, &name, result.config.limits.text_bytes)?;
            add_text(used, &kind, result.config.limits.text_bytes)?;
            result.dependencies.push(StyloVariableDependency {
                rule_path: path.to_vec(),
                declaration_ordinal: ordinal,
                name,
                kind,
                reference_path: current_path.clone(),
            });
            if let Some(fallback) = &r.fallback {
                add_dependencies(
                    &fallback.references,
                    path,
                    ordinal,
                    &current_path,
                    result,
                    used,
                    records_used,
                )?;
            }
        }
        Ok(())
    }
    let mut records_used = 0usize;
    let roots = sheet.rules.read_with(&guard);
    let mut stack = Vec::new();
    if roots.0.len() > result.config.limits.rules as usize {
        return Err(StyloError::Limit);
    }
    for (i, rule) in roots.0.iter().enumerate().rev() {
        stack.push((rule, vec![u32::try_from(i).map_err(|_| StyloError::Limit)?]));
    }
    while let Some((rule, path)) = stack.pop() {
        if cancelled() {
            return Err(StyloError::Cancelled);
        }
        if result.rules.len() >= result.config.limits.rules as usize
            || path.len() > result.config.limits.depth as usize
        {
            return Err(StyloError::Limit);
        }
        let native_kind = format!("{:?}", rule.rule_type());
        add_text(
            &mut text_used,
            &native_kind,
            result.config.limits.text_bytes,
        )?;
        let (kind, loc, condition) = match rule {
            CssRule::Style(node) => {
                let node = node.read_with(&guard);
                for (i, selector) in node.selectors.slice().iter().enumerate() {
                    if records_used >= result.config.limits.records as usize {
                        return Err(StyloError::Limit);
                    }
                    records_used += 1;
                    let canonical = cssparser::ToCss::to_css_string(selector);
                    add_text(&mut text_used, &canonical, result.config.limits.text_bytes)?;
                    let mut components = Vec::new();
                    for component in selector.iter_raw_match_order() {
                        let (kind, name) = match component {
                            selectors::parser::Component::Class(name) => {
                                (StyloSelectorComponentKind::Class, name)
                            }
                            selectors::parser::Component::ID(name) => {
                                (StyloSelectorComponentKind::Id, name)
                            }
                            _ => continue,
                        };
                        if records_used >= result.config.limits.records as usize {
                            return Err(StyloError::Limit);
                        }
                        records_used += 1;
                        let name: &str = name.0.as_ref();
                        add_text(&mut text_used, name, result.config.limits.text_bytes)?;
                        components.push(StyloSelectorComponent {
                            kind,
                            name: name.to_owned(),
                        });
                    }
                    result.selectors.push(StyloSelector {
                        rule_path: path.clone(),
                        ordinal: u32::try_from(i).map_err(|_| StyloError::Limit)?,
                        canonical,
                        specificity: selector.specificity(),
                        components,
                        component_coverage: StyloDependencyCoverage::Unknown,
                    });
                }
                let block = node.block.read_with(&guard);
                for (i, (declaration, importance)) in
                    block.declaration_importance_iter().enumerate()
                {
                    if records_used >= result.config.limits.records as usize {
                        return Err(StyloError::Limit);
                    }
                    records_used += 1;
                    let ordinal = u32::try_from(i).map_err(|_| StyloError::Limit)?;
                    let property = style_traits::ToCss::to_css_string(&declaration.id());
                    let mut value = String::new();
                    declaration
                        .to_css(&mut value)
                        .map_err(|_| StyloError::Serialization)?;
                    add_text(&mut text_used, &property, result.config.limits.text_bytes)?;
                    add_text(&mut text_used, &value, result.config.limits.text_bytes)?;
                    let dependency_coverage =
                        if let PropertyDeclaration::Custom(custom) = declaration {
                            if let CustomDeclarationValue::Unparsed(value) = &custom.value {
                                add_dependencies(
                                    &value.references,
                                    &path,
                                    ordinal,
                                    &[],
                                    &mut result,
                                    &mut text_used,
                                    &mut records_used,
                                )?;
                                StyloDependencyCoverage::Known
                            } else {
                                StyloDependencyCoverage::Unknown
                            }
                        } else {
                            StyloDependencyCoverage::Unknown
                        };
                    let kind = match declaration {
                        PropertyDeclaration::Custom(_) => StyloDeclarationKind::Custom,
                        PropertyDeclaration::WithVariables(_) => {
                            StyloDeclarationKind::WithVariables
                        }
                        PropertyDeclaration::CSSWideKeyword(_) => {
                            StyloDeclarationKind::CssWideKeyword
                        }
                        _ => StyloDeclarationKind::TypedLonghand,
                    };
                    result.declarations.push(StyloDeclaration {
                        rule_path: path.clone(),
                        ordinal,
                        kind,
                        property,
                        // Stylo suppresses value serialization for longhands expanded
                        // from variable shorthands; a custom property may validly be empty.
                        value_available: !value.is_empty()
                            || !matches!(declaration, PropertyDeclaration::WithVariables(_)),
                        value,
                        important: importance.important(),
                        dependency_coverage,
                    });
                }
                (
                    StyloRuleKind::Style,
                    Some(location(node.source_location, false)),
                    None,
                )
            }
            CssRule::Media(node) => {
                let condition =
                    style_traits::ToCss::to_css_string(node.media_queries.read_with(&guard));
                add_text(&mut text_used, &condition, result.config.limits.text_bytes)?;
                (
                    StyloRuleKind::Media,
                    Some(location(node.source_location, false)),
                    Some(condition),
                )
            }
            _ => {
                result.coverage = StyloCoverage::Partial;
                (StyloRuleKind::Other, None, None)
            }
        };
        result.rules.push(StyloRule {
            path: path.clone(),
            kind,
            native_kind,
            condition,
            location: loc,
        });
        let children = rule.children(&guard);
        if !children.is_empty() && path.len() >= result.config.limits.depth as usize {
            return Err(StyloError::Limit);
        }
        if result
            .rules
            .len()
            .saturating_add(stack.len())
            .saturating_add(children.len())
            > result.config.limits.rules as usize
        {
            return Err(StyloError::Limit);
        }
        for (i, child) in children.iter().enumerate().rev() {
            let mut child_path = path.clone();
            child_path.push(u32::try_from(i).map_err(|_| StyloError::Limit)?);
            stack.push((child, child_path));
        }
    }
    // Events are limited to successfully inserted top-level native style rules.
    // Name offsets are parser-input relative and mapped back to the verified host.
    let prefix = if utf8_bom_removed { 3usize } else { 0usize };
    // Bounded membership sets avoid occurrence-by-rule scans.
    let root_rules: std::collections::BTreeSet<_> = result
        .rules
        .iter()
        .filter(|r| r.path.len() == 1 && r.kind == StyloRuleKind::Style)
        .map(|r| r.path[0])
        .collect();
    let root_selectors: std::collections::BTreeSet<_> = result
        .selectors
        .iter()
        .filter(|s| s.rule_path.len() == 1)
        .map(|s| (s.rule_path[0], s.ordinal))
        .collect();
    let mut previous_class_end = 0usize;
    let mut class_scan_work = 0usize;
    for event in class_events {
        if event.start < previous_class_end || event.end < event.start {
            return Err(StyloError::InvalidConfig);
        }
        previous_class_end = event.end;
        let owner = event.owner_rule.ok_or(StyloError::InvalidConfig)?;
        let rule_ordinal = u32::try_from(owner).map_err(|_| StyloError::Limit)?;
        let selector_ordinal =
            u32::try_from(event.selector_ordinal).map_err(|_| StyloError::Limit)?;
        if !root_selectors.contains(&(rule_ordinal, selector_ordinal)) {
            return Err(StyloError::InvalidConfig);
        }
        let start = event.start.checked_add(prefix).ok_or(StyloError::Limit)?;
        let end = event.end.checked_add(prefix).ok_or(StyloError::Limit)?;
        let raw = source.get(start..end).ok_or(StyloError::InvalidUtf8)?;
        let mut token_parser = cssparser::Parser::new(raw);
        match token_parser
            .next_including_whitespace_and_comments()
            .map_err(|_| StyloError::InvalidConfig)?
            .clone()
        {
            cssparser::Token::Ident(name) if name.as_ref() == event.name => {}
            _ => return Err(StyloError::InvalidConfig),
        }
        if token_parser.position().byte_index() != raw.len() {
            return Err(StyloError::InvalidConfig);
        }
        class_scan_work = class_scan_work
            .checked_add(raw.len())
            .ok_or(StyloError::Limit)?;
        if class_scan_work > input.len() {
            return Err(StyloError::Limit);
        }
        if records_used >= result.config.limits.records as usize {
            return Err(StyloError::Limit);
        }
        records_used += 1;
        add_text(&mut text_used, raw, result.config.limits.text_bytes)?;
        add_text(&mut text_used, &event.name, result.config.limits.text_bytes)?;
        result.occurrences.push(StyloSourceOccurrence {
            rule_path: vec![rule_ordinal],
            selector_ordinal: Some(selector_ordinal),
            role: StyloSourceRole::ClassName,
            start: start as u64,
            end: end as u64,
            raw: raw.to_owned(),
            decoded: event.name,
            disposition: StyloSourceDisposition::Unknown,
        });
    }
    let mut previous_declaration_end = 0usize;
    let mut prefix_scan_work = 0usize;
    for event in declaration_events {
        if event.start < previous_declaration_end || event.end < event.start {
            return Err(StyloError::InvalidConfig);
        }
        previous_declaration_end = event.end;
        if !event.name.starts_with("--") {
            continue;
        }
        let owner = event.owner_rule.ok_or(StyloError::InvalidConfig)?;
        let rule_ordinal = u32::try_from(owner).map_err(|_| StyloError::Limit)?;
        if !root_rules.contains(&rule_ordinal) {
            return Err(StyloError::InvalidConfig);
        }
        let start = event.start.checked_add(prefix).ok_or(StyloError::Limit)?;
        let end = event.end.checked_add(prefix).ok_or(StyloError::Limit)?;
        let declaration = source.get(start..end).ok_or(StyloError::InvalidUtf8)?;
        let mut parser = cssparser::Parser::new(declaration);
        let site = loop {
            let local_start = parser.position().byte_index();
            let token = parser
                .next_including_whitespace_and_comments()
                .map_err(|_| StyloError::InvalidConfig)?
                .clone();
            let local_end = parser.position().byte_index();
            match token {
                cssparser::Token::WhiteSpace(_) | cssparser::Token::Comment(_) => continue,
                cssparser::Token::Ident(name) if name.as_ref() == event.name => {
                    break (start + local_start, start + local_end);
                }
                _ => return Err(StyloError::InvalidConfig),
            }
        };
        prefix_scan_work = prefix_scan_work
            .checked_add(site.1 - start)
            .ok_or(StyloError::Limit)?;
        if prefix_scan_work > input.len() {
            return Err(StyloError::Limit);
        }
        let raw = source.get(site.0..site.1).ok_or(StyloError::InvalidUtf8)?;
        if records_used >= result.config.limits.records as usize {
            return Err(StyloError::Limit);
        }
        records_used += 1;
        add_text(&mut text_used, raw, result.config.limits.text_bytes)?;
        add_text(&mut text_used, &event.name, result.config.limits.text_bytes)?;
        result.occurrences.push(StyloSourceOccurrence {
            rule_path: vec![rule_ordinal],
            selector_ordinal: None,
            role: StyloSourceRole::CustomPropertyDeclarationName,
            start: site.0 as u64,
            end: site.1 as u64,
            raw: raw.to_owned(),
            decoded: event.name,
            disposition: StyloSourceDisposition::Unknown,
        });
    }
    result.occurrences.sort_by_key(|o| (o.start, o.end));
    if before_preferences != preferences() {
        return Err(StyloError::PreferencesChanged);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> StyloParseConfig {
        StyloParseConfig {
            source_kind: StyloSourceKind::StandaloneUtf8,
            base_url: "https://example.invalid/main.css".to_owned(),
            author_origin: true,
            no_quirks: true,
            allow_imports: false,
            limits: StyloLimits {
                input_bytes: 100_000,
                rules: 100,
                depth: 16,
                records: 100,
                diagnostics: 100,
                text_bytes: 100_000,
            },
        }
    }
    #[test]
    fn support_fingerprint_binds_inputs_and_boundaries() {
        let parts: [&[u8]; 3] = [b"adapter", b"schema", b"native closure"];
        let baseline = fingerprint_parts(&parts);
        for index in 0..parts.len() {
            let mut changed = parts;
            changed[index] = b"changed";
            assert_ne!(baseline, fingerprint_parts(&changed));
        }
        assert_ne!(
            fingerprint_parts(&[b"a", b"bc"]),
            fingerprint_parts(&[b"ab", b"c"])
        );
        assert_ne!(support_fingerprint(), [0; 32]);
    }
    #[test]
    fn native_media_selectors_declarations_and_dependencies() {
        let css = b"@media (width > 60rem) { .hud-menu, .button { --x: 1px; --x: 2px; --chain: var(--x, var(--other)) var(--z, var(--end)); --empty: ; color: var(--chain); } }";
        let analysis = analyze_with_cancel(css, config(), || false).unwrap();
        assert_eq!(analysis.coverage, StyloCoverage::Partial);
        assert_eq!(
            analysis.preference_stability,
            StyloPreferenceStability::Unknown
        );
        assert!(!analysis.preferences.booleans.is_empty());
        assert_eq!(analysis.rules.len(), 2);
        assert_eq!(analysis.rules[0].kind, StyloRuleKind::Media);
        assert_eq!(analysis.rules[1].path, vec![0, 0]);
        assert_eq!(analysis.selectors.len(), 2);
        assert_eq!(analysis.selectors[0].components[0].name, "hud-menu");
        assert_eq!(
            analysis.selectors[0].component_coverage,
            StyloDependencyCoverage::Unknown
        );
        assert_eq!(
            analysis
                .declarations
                .iter()
                .filter(|d| d.property == "--x")
                .count(),
            1
        );
        let empty = analysis
            .declarations
            .iter()
            .find(|d| d.property == "--empty")
            .unwrap();
        assert!(empty.value_available);
        assert!(
            analysis
                .dependencies
                .iter()
                .any(|d| d.name == "z" && d.reference_path == [1])
        );
        assert!(
            analysis
                .dependencies
                .iter()
                .any(|d| d.name == "end" && d.reference_path == [1, 0])
        );
        assert_eq!(analysis.support_fingerprint, support_fingerprint());
        assert!(
            analysis
                .dependencies
                .iter()
                .any(|d| d.name == "x" && d.reference_path == [0])
        );
        assert!(
            analysis
                .dependencies
                .iter()
                .any(|d| d.name == "other" && d.reference_path == [0, 0])
        );
        assert_eq!(
            analysis,
            analyze_with_cancel(css, config(), || false).unwrap()
        );
    }
    #[test]
    fn observer_proves_root_class_and_duplicate_custom_property_sites() {
        let source = "\u{feff}/* π😀 */\r\n.foo\\:bar { --x: 1px; --x: 2px; --x\\31: 3px; } @media screen { .nested { --n: 1; } }";
        let result = analyze_with_cancel(source.as_bytes(), config(), || false).unwrap();
        assert!(result.utf8_bom_removed);
        assert_eq!(result.source_coverage, StyloCoverage::Partial);
        assert_eq!(result.occurrences.len(), 4);
        let class = &result.occurrences[0];
        assert_eq!(class.role, StyloSourceRole::ClassName);
        assert_eq!(class.raw, r"foo\:bar");
        assert_eq!(class.decoded, "foo:bar");
        assert_eq!(class.rule_path, [0]);
        assert_eq!(class.selector_ordinal, Some(0));
        let names: Vec<_> = result
            .occurrences
            .iter()
            .filter(|o| o.role == StyloSourceRole::CustomPropertyDeclarationName)
            .collect();
        assert_eq!(names.len(), 3);
        assert_eq!(names[2].raw, r"--x\31");
        assert_eq!(names[2].decoded, "--x1");
        assert_ne!(names[0].start, names[1].start);
        assert!(
            names
                .iter()
                .all(|o| o.disposition == StyloSourceDisposition::Unknown)
        );
        for occurrence in &result.occurrences {
            assert_eq!(
                &source[occurrence.start as usize..occurrence.end as usize],
                occurrence.raw
            );
        }
        assert!(
            !result
                .occurrences
                .iter()
                .any(|o| o.decoded == "nested" || o.decoded == "--n")
        );
    }
    #[test]
    fn class_name_site_excludes_skipped_comments_and_decodes_escapes() {
        let source = "\u{feff}/*😀*/\r\n./**/foo\\:bar { --x: 1; }";
        let result = analyze_with_cancel(source.as_bytes(), config(), || false).unwrap();
        let site = result
            .occurrences
            .iter()
            .find(|o| o.role == StyloSourceRole::ClassName)
            .unwrap();
        assert_eq!(site.raw, r"foo\:bar");
        assert_eq!(site.decoded, "foo:bar");
        assert_eq!(&source[site.start as usize..site.end as usize], site.raw);
    }
    #[test]
    fn observer_discards_rejected_rules_and_uses_inserted_rule_ordinals() {
        let source = ".bad. { --ignored: 0; } .one { --a: 1; } @media screen { .nested { --n: 2; } } .two { --b: 3; }";
        let result = analyze_with_cancel(source.as_bytes(), config(), || false).unwrap();
        assert_eq!(result.rules.len(), 4); // three roots plus one nested media child
        assert!(
            result
                .occurrences
                .iter()
                .any(|o| o.decoded == "one" && o.rule_path == [0])
        );
        assert!(
            result
                .occurrences
                .iter()
                .any(|o| o.decoded == "--a" && o.rule_path == [0])
        );
        assert!(
            result
                .occurrences
                .iter()
                .any(|o| o.decoded == "two" && o.rule_path == [2])
        );
        assert!(
            result
                .occurrences
                .iter()
                .any(|o| o.decoded == "--b" && o.rule_path == [2])
        );
        assert!(
            !result
                .occurrences
                .iter()
                .any(|o| matches!(o.decoded.as_str(), "bad" | "--ignored" | "nested" | "--n"))
        );
        assert_eq!(result.source_coverage, StyloCoverage::Partial);
    }
    #[test]
    fn utf8_bom_is_removed_before_stylo_parsing() {
        let css = "\u{feff}.foo { color: red; }";
        let result = analyze_with_cancel(css.as_bytes(), config(), || false).unwrap();
        assert!(result.utf8_bom_removed);
        assert_eq!(result.selectors.len(), 1);
        assert_eq!(result.selectors[0].canonical, ".foo");
        assert_eq!(result.selectors[0].specificity, 0x400);
        assert_eq!(result.rules[0].location.unwrap().column, 1);
    }
    #[test]
    fn native_location_reports_utf16_column_without_byte_guessing() {
        let css = ".first {}\r\n/*😀*/ .second {}";
        let result = analyze_with_cancel(css.as_bytes(), config(), || false).unwrap();
        let at = result.rules[1].location.unwrap();
        assert_eq!((at.line, at.column), (1, 8));
    }
    #[test]
    fn error_limits_context_and_cancellation_are_explicit() {
        let malformed = analyze_with_cancel(
            b".bad { color: definitely-not-a-color; } .ok { color: red; }",
            config(),
            || false,
        )
        .unwrap();
        assert_eq!(malformed.coverage, StyloCoverage::Partial);
        assert!(!malformed.diagnostics.is_empty());
        assert!(malformed.diagnostics[0].location.approximate);
        let mut tiny = config();
        tiny.limits.records = 0;
        assert_eq!(
            analyze_with_cancel(b".x {}", tiny, || false),
            Err(StyloError::Limit)
        );
        let mut unsupported = config();
        unsupported.allow_imports = true;
        assert_eq!(
            analyze_with_cancel(b"", unsupported, || false),
            Err(StyloError::InvalidConfig)
        );
        assert_eq!(
            analyze_with_cancel(b"", config(), || true),
            Err(StyloError::Cancelled)
        );
    }
}
