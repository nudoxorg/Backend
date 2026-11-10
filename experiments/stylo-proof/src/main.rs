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
