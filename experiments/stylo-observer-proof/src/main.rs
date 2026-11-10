use std::cell::RefCell;
use style::{
    context::QuirksMode,
    error_reporting::{ContextualParseError, ParseErrorReporter},
    shared_lock::SharedRwLock,
    stylesheets::{AllowImportRules, CssRule, Origin, StylesheetContents, UrlExtraData},
};
struct Reporter(RefCell<Vec<String>>);
impl ParseErrorReporter for Reporter {
    fn report_error(
        &self,
        _: &UrlExtraData,
        location: cssparser::SourceLocation,
        error: ContextualParseError,
    ) {
        self.0.borrow_mut().push(format!("{location:?}: {error:?}"));
    }
}
fn main() {
    let source = "/* π */\r\n@media (width > 60rem) { .hud-menu, .button { --space: 1px; --space: 2px; gap: var(--space); } } .button { color: var(--space); }";
    let lock = SharedRwLock::new();
    let reporter = Reporter(RefCell::new(Vec::new()));
    let url = UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap());
    let sheet = StylesheetContents::from_str(
        source,
        url,
        Origin::Author,
        &lock,
        None,
        Some(&reporter),
        QuirksMode::NoQuirks,
        AllowImportRules::No,
        None,
    );
    let guard = lock.read();
    fn walk(
        rules: &[CssRule],
        guard: &style::shared_lock::SharedRwLockReadGuard<'_>,
        depth: usize,
    ) {
        for rule in rules {
            match rule {
                CssRule::Media(media) => println!(
                    "depth={depth} media location={:?} query={:?}",
                    media.source_location,
                    media.media_queries.read_with(guard)
                ),
                CssRule::Style(rule) => {
                    let style = rule.read_with(guard);
                    let block = style.block.read_with(guard);
                    println!(
                        "depth={depth} style location={:?} selectors={:?} declarations={:?} len={}",
                        style.source_location,
                        style.selectors,
                        block.declarations(),
                        block.len()
                    );
                }
                _ => println!("depth={depth} other={rule:?}"),
            }
            walk(rule.children(guard), guard, depth + 1);
        }
    }
    walk(&sheet.rules.read_with(&guard).0, &guard, 0);
    println!("diagnostics={:?}", reporter.0.borrow());
}

#[cfg(test)]
mod tests {
    use super::*;
    use selectors::{
        Element, OpaqueElement, SelectorImpl as SelectorTrait,
        attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint},
        bloom::BloomFilter,
        context::{
            MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags,
            SelectorCaches,
        },
        matching::{ElementSelectorFlags, matches_selector_list},
    };
    use style::selector_parser::SelectorImpl;
    type Impl = SelectorImpl;
    type Ident = <Impl as SelectorTrait>::Identifier;
    type Local = <Impl as SelectorTrait>::LocalName;
    type Namespace = <Impl as SelectorTrait>::NamespaceUrl;
    type Attr = <Impl as SelectorTrait>::AttrValue;
    type LocalRef = <Impl as SelectorTrait>::BorrowedLocalName;
    type NamespaceRef = <Impl as SelectorTrait>::BorrowedNamespaceUrl;
    type Pseudo = <Impl as SelectorTrait>::NonTSPseudoClass;
    type PseudoElement = <Impl as SelectorTrait>::PseudoElement;
    #[derive(Clone, Copy, Debug)]
    struct Node<'a>(&'a str);
    impl Element for Node<'_> {
        type Impl = Impl;
        fn opaque(&self) -> OpaqueElement {
            OpaqueElement::new(&self.0.as_bytes()[0])
        }
        fn parent_element(&self) -> Option<Self> {
            None
        }
        fn parent_node_is_shadow_root(&self) -> bool {
            false
        }
        fn containing_shadow_host(&self) -> Option<Self> {
            None
        }
        fn is_pseudo_element(&self) -> bool {
            false
        }
        fn prev_sibling_element(&self) -> Option<Self> {
            None
        }
        fn next_sibling_element(&self) -> Option<Self> {
            None
        }
        fn first_element_child(&self) -> Option<Self> {
            None
        }
        fn is_html_element_in_html_document(&self) -> bool {
            true
        }
        fn has_local_name(&self, _: &LocalRef) -> bool {
            false
        }
        fn has_namespace(&self, _: &NamespaceRef) -> bool {
            false
        }
        fn is_same_type(&self, _: &Self) -> bool {
            true
        }
        fn attr_matches(
            &self,
            _: &NamespaceConstraint<&Namespace>,
            _: &Local,
            _: &AttrSelectorOperation<&Attr>,
        ) -> bool {
            false
        }
        fn match_non_ts_pseudo_class(&self, _: &Pseudo, _: &mut MatchingContext<Impl>) -> bool {
            false
        }
        fn match_pseudo_element(&self, _: &PseudoElement, _: &mut MatchingContext<Impl>) -> bool {
            false
        }
        fn apply_selector_flags(&self, _: ElementSelectorFlags) {}
        fn is_link(&self) -> bool {
            false
        }
        fn is_html_slot_element(&self) -> bool {
            false
        }
        fn has_id(&self, _: &Ident, _: CaseSensitivity) -> bool {
            false
        }
        fn has_class(&self, name: &Ident, sensitivity: CaseSensitivity) -> bool {
            sensitivity.eq(
                self.0.as_bytes(),
                <str as AsRef<str>>::as_ref(&name.0).as_bytes(),
            )
        }
        fn has_custom_state(&self, _: &Ident) -> bool {
            false
        }
        fn imported_part(&self, _: &Ident) -> Option<Ident> {
            None
        }
        fn is_part(&self, _: &Ident) -> bool {
            false
        }
        fn is_empty(&self) -> bool {
            true
        }
        fn is_root(&self) -> bool {
            true
        }
        fn add_element_unique_hashes(&self, _: &mut BloomFilter) -> bool {
            false
        }
    }
    #[test]
    fn native_simple_class_token_has_exact_original_bytes() {
        let css = "\u{feff}/* π😀 */\r\n./**/foo\\:bar { color: red; }";
        fn parse(css: &str) -> String {
            let lock = SharedRwLock::new();
            let sheet = StylesheetContents::from_str(css, UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap()), Origin::Author, &lock, None, None, QuirksMode::NoQuirks, AllowImportRules::No, None);
            let guard = lock.read(); let rules = sheet.rules.read_with(&guard);
            let CssRule::Style(rule) = &rules.0[0] else { panic!("style missing") };
            let rule = rule.read_with(&guard);
            format!("{:?}|{:?}", rule.selectors, rule.block.read_with(&guard).declarations())
        }
        let decoded = css.strip_prefix('\u{feff}').unwrap();
        let control = parse(decoded);
        let observer = style::nudox_observer::begin([16; 32], 100, 4096).unwrap();
        assert_eq!(parse(decoded), control);
        let (_, _, classes, overflow, skipped) = observer.finish().unwrap();
        assert!(!skipped);
        assert!(!overflow); assert_eq!(classes.len(), 1);
        let class = &classes[0]; assert_eq!(class.name, "foo:bar");
        assert_eq!(&css[class.start + 3..class.end + 3], r"foo\:bar");
        assert_eq!(class.selector_ordinal, 0);
        assert_eq!(class.owner_rule, Some(0));
        let nested = style::nudox_observer::begin([17; 32], 100, 4096).unwrap();
        parse(":is(.foo) { color: red; }");
        let (_, _, nested_classes, _, skipped) = nested.finish().unwrap();
        assert!(nested_classes.is_empty(), "nested selector correspondence is unsupported");
        assert!(skipped);
        let limited = style::nudox_observer::begin([18; 32], 0, 4096).unwrap();
        assert_eq!(parse(decoded), control);
        assert!(limited.finish().unwrap().3);
    }
    #[test]
    fn observer_commits_only_inserted_top_level_rule_ownership() {
        let css = ".bad. { --ignored: 0; } .one { --a: 1; } @media screen { .nested { --n: 2; } } .two { --b: 3; }";
        let observation = style::nudox_observer::begin([19; 32], 100, 4096).unwrap();
        let lock = SharedRwLock::new();
        let sheet = StylesheetContents::from_str(css, UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap()), Origin::Author, &lock, None, None, QuirksMode::NoQuirks, AllowImportRules::No, None);
        let guard = lock.read(); let roots = sheet.rules.read_with(&guard);
        assert_eq!(roots.0.len(), 3);
        assert!(matches!(roots.0[0], CssRule::Style(_)));
        assert!(matches!(roots.0[1], CssRule::Media(_)));
        assert!(matches!(roots.0[2], CssRule::Style(_)));
        let (_, declarations, classes, overflow, skipped) = observation.finish().unwrap();
        assert!(!overflow); assert!(skipped);
        assert_eq!(declarations.iter().map(|d| (&*d.name, d.owner_rule)).collect::<Vec<_>>(), vec![("--a", Some(0)), ("--b", Some(2))]);
        assert_eq!(classes.iter().map(|d| (&*d.name, d.owner_rule)).collect::<Vec<_>>(), vec![("one", Some(0)), ("two", Some(2))]);
    }
    #[test]
    fn observer_suppresses_reentrant_host_and_cleans_up_on_unwind() {
        struct Reentrant(std::cell::Cell<bool>);
        impl ParseErrorReporter for Reentrant {
            fn report_error(&self, _: &UrlExtraData, _: cssparser::SourceLocation, _: ContextualParseError) {
                if self.0.replace(true) { return; }
                let lock = SharedRwLock::new();
                let _ = StylesheetContents::from_str(".inner { --inner: 2; }", UrlExtraData::from(url::Url::parse("https://example.invalid/inner.css").unwrap()), Origin::Author, &lock, None, None, QuirksMode::NoQuirks, AllowImportRules::No, None);
            }
        }
        let guard = style::nudox_observer::begin([12; 32], 100, 4096).unwrap();
        let reporter = Reentrant(std::cell::Cell::new(false));
        let lock = SharedRwLock::new();
        let _ = StylesheetContents::from_str(".outer { color: invalid-color; --outer: 1; }", UrlExtraData::from(url::Url::parse("https://example.invalid/outer.css").unwrap()), Origin::Author, &lock, None, Some(&reporter), QuirksMode::NoQuirks, AllowImportRules::No, None);
        assert!(reporter.0.get());
        let (_, events, _, overflow, _) = guard.finish().unwrap();
        assert!(!overflow);
        assert!(events.iter().any(|e| e.name == "--outer"));
        assert!(!events.iter().any(|e| e.name == "--inner"));
        let _ = std::panic::catch_unwind(|| { let _guard = style::nudox_observer::begin([13; 32], 1, 4096).unwrap(); panic!("test cleanup"); });
        assert!(style::nudox_observer::begin([14; 32], 1, 4096).is_some());
    }
    #[test]
    fn accepted_declaration_hook_preserves_duplicate_raw_name_sites_and_semantics() {
        let css = "/* π */\r\n.x { --x: 1px; --x: 2px; }";
        fn parse(css: &str) -> (String, usize) {
            let lock = SharedRwLock::new();
            let sheet = StylesheetContents::from_str(css, UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap()), Origin::Author, &lock, None, None, QuirksMode::NoQuirks, AllowImportRules::No, None);
            let guard = lock.read(); let roots = sheet.rules.read_with(&guard);
            let CssRule::Style(rule) = &roots.0[0] else { panic!("style") }; let rule = rule.read_with(&guard); let block = rule.block.read_with(&guard);
            (format!("{:?}|{:?}", rule.selectors, block.declarations()), block.len())
        }
        let control = parse(css);
        let observation = style::nudox_observer::begin([7; 32], 10, 4096).unwrap();
        assert!(style::nudox_observer::begin([8; 32], 10, 4096).is_none());
        let observed = parse(css);
        let (host, events, _, overflow, _) = observation.finish().unwrap();
        assert_eq!(host, [7; 32]);
        assert!(!overflow); assert_eq!(control, observed); assert_eq!(control.1, 1);
        assert_eq!(events.len(), 2);
        let mut sites = Vec::new();
        for event in &events {
            assert_eq!(event.owner_rule, Some(0)); assert_eq!(event.name, "--x"); assert_eq!(event.property_ids, ["--x"]);
            let mut p = cssparser::Parser::new(&css[event.start..event.end]);
            loop { let s = p.position().byte_index(); let token = p.next_including_whitespace_and_comments().unwrap().clone(); let e = p.position().byte_index();
                if let cssparser::Token::Ident(name) = token { assert_eq!(name.as_ref(), event.name); sites.push((event.start+s, event.start+e)); break; }
            }
        }
        assert_ne!(sites[0], sites[1]);
        for (start,end) in sites { assert_eq!(&css[start..end], "--x"); }
        let limited = style::nudox_observer::begin([9; 32], 0, 4096).unwrap();
        assert_eq!(parse(css), control); assert!(limited.finish().unwrap().3);
        let dropped = style::nudox_observer::begin([10; 32], 10, 4096).unwrap();
        drop(dropped);
        assert!(style::nudox_observer::begin([11; 32], 10, 4096).is_some());
        let limited_text = style::nudox_observer::begin([15; 32], 10, 0).unwrap();
        assert_eq!(parse(css), control);
        assert!(limited_text.finish().unwrap().3);
    }
    #[test]
    fn cssparser_host_byte_positions_are_verbatim_not_semantic_correspondence() {
        use cssparser::{Parser, Token};
        fn scan<'a>(p: &mut Parser<'a>, out: &mut Vec<(usize, usize, String)>) {
            loop {
                let start = p.position().byte_index();
                let Ok(token) = p.next_including_whitespace_and_comments().cloned() else {
                    break;
                };
                let end = p.position().byte_index();
                if let Token::Ident(name) = &token {
                    out.push((start, end, name.to_string()));
                }
                if matches!(
                    token,
                    Token::Function(_)
                        | Token::CurlyBracketBlock
                        | Token::SquareBracketBlock
                        | Token::ParenthesisBlock
                ) {
                    let result: Result<(), cssparser::ParseError<()>> =
                        p.parse_nested_block(|nested| {
                            scan(nested, out);
                            Ok(())
                        });
                    assert!(result.is_ok());
                }
            }
        }
        let source =
            "\u{feff}/* π */\r\n.foo\\:bar { --x: var(--space); --x: red; content: \".fake\"; }";
        let mut parser = Parser::new(source);
        let mut tokens = Vec::new();
        scan(&mut parser, &mut tokens);
        assert!(
            tokens
                .iter()
                .any(|(s, e, name)| name == "foo:bar" && &source[*s..*e] == r"foo\:bar")
        );
        let declarations: Vec<_> = tokens.iter().filter(|(_, _, name)| name == "--x").collect();
        assert_eq!(declarations.len(), 2);
        assert_ne!(declarations[0].0, declarations[1].0);
        assert!(
            tokens
                .iter()
                .any(|(s, e, name)| name == "--space" && &source[*s..*e] == "--space")
        );
        assert!(!tokens.iter().any(|(_, _, name)| name == "fake"));
    }
    #[test]
    fn native_rules_preserve_semantics_but_deduplicate_written_declarations() {
        use style::custom_properties::SubstitutionFunctionKind;
        use style::properties::{CustomDeclarationValue, PropertyDeclaration};
        let css = "/* π */\r\n@media (width > 60rem) { .hud-menu { --x: 1px; --x: 2px; --chain: var(--x, var(--other)); color: var(--chain); } }";
        let lock = SharedRwLock::new();
        let reporter = Reporter(RefCell::new(Vec::new()));
        let sheet = StylesheetContents::from_str(
            css,
            UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap()),
            Origin::Author,
            &lock,
            None,
            Some(&reporter),
            QuirksMode::NoQuirks,
            AllowImportRules::No,
            None,
        );
        let guard = lock.read();
        let rules = sheet.rules.read_with(&guard);
        let CssRule::Media(media) = &rules.0[0] else {
            panic!("MQ4 was not parsed")
        };
        let rules = media.rules.read_with(&guard);
        let CssRule::Style(style) = &rules.0[0] else {
            panic!("style missing")
        };
        let style = style.read_with(&guard);
        assert_eq!(style.selectors.slice().len(), 1);
        assert_eq!(style.selectors.slice()[0].specificity(), 0x400);
        let block = style.block.read_with(&guard);
        let custom: Vec<_> = block
            .declarations()
            .iter()
            .filter_map(|d| match d {
                PropertyDeclaration::Custom(c) => Some(c),
                _ => None,
            })
            .collect();
        assert_eq!(custom.len(), 2, "duplicate --x is collapsed");
        let chain = custom.iter().find(|c| c.name.as_str() == "chain").unwrap();
        let CustomDeclarationValue::Unparsed(value) = &chain.value else {
            panic!("expected unparsed custom property")
        };
        assert_eq!(value.references.refs.len(), 1);
        let dep = &value.references.refs[0];
        assert_eq!(dep.name.as_str(), "x");
        assert_eq!(dep.substitution_kind, SubstitutionFunctionKind::Var);
        let nested = dep.fallback.as_ref().unwrap();
        assert_eq!(nested.references.refs[0].name.as_str(), "other");
        assert!(
            block
                .declarations()
                .iter()
                .any(|d| matches!(d, PropertyDeclaration::WithVariables(_)))
        );
        assert!(reporter.0.borrow().is_empty());
    }
    #[test]
    fn stylo_reports_rejected_rules_and_retains_valid_siblings() {
        let css = ".bad { color: definitely-not-a-color; } .good { color: red; }";
        let lock = SharedRwLock::new();
        let reporter = Reporter(RefCell::new(Vec::new()));
        let sheet = StylesheetContents::from_str(
            css,
            UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap()),
            Origin::Author,
            &lock,
            None,
            Some(&reporter),
            QuirksMode::NoQuirks,
            AllowImportRules::No,
            None,
        );
        let guard = lock.read();
        assert_eq!(sheet.rules.read_with(&guard).0.len(), 2);
        assert!(!reporter.0.borrow().is_empty());
    }
    #[test]
    fn real_stylo_selector_matches_explicit_element() {
        let lock = SharedRwLock::new();
        let sheet = StylesheetContents::from_str(
            ".hud-menu { color: red; }",
            UrlExtraData::from(url::Url::parse("https://example.invalid/app.css").unwrap()),
            Origin::Author,
            &lock,
            None,
            None,
            QuirksMode::NoQuirks,
            AllowImportRules::No,
            None,
        );
        let guard = lock.read();
        let rules = sheet.rules.read_with(&guard);
        let CssRule::Style(rule) = &rules.0[0] else {
            panic!("missing style rule")
        };
        let rule = rule.read_with(&guard);
        let mut caches = SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );
        assert!(matches_selector_list(
            &rule.selectors,
            Node("hud-menu"),
            &mut context
        ));
        assert!(!matches_selector_list(
            &rule.selectors,
            Node("different"),
            &mut context
        ));
    }
}
