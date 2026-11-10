//! Class names written in a Vue template, with exact byte ranges into the
//! original file.
//!
//! This reads the template block only: static `class="a b"` attributes and the
//! string literals of `:class` / `v-bind:class` expressions. It does not run
//! the template compiler, so a class built from a runtime value is counted as a
//! dynamic binding and never linked. Every token carries the range of its
//! spelling in the host, so a reader can show the exact source.
use backend_compile::VerifiedSourceExtent;
use std::ops::Range;
use verter_core::ast::types::AstNodeKind;
use verter_core::diagnostics::{SyntaxPluginContext, SyntaxPluginOptions};
use verter_core::parser::Syntax;
use verter_core::tokenizer::byte::tokenize_sfc;

/// One class token in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateClass {
    /// The class name as written.
    pub token: String,
    /// Where the token is spelled in the host.
    pub range: Range<usize>,
    /// Found inside a `:class` binding, where nothing proves it applies.
    pub conditional: bool,
}

/// A `$style.name` member read from a `:class` binding, the Vue spelling of a
/// CSS-module member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateModuleClass {
    /// The member name.
    pub name: String,
    /// Where the member name is spelled in the host.
    pub range: Range<usize>,
}

/// Everything a template says about classes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateClasses {
    /// Static and literal class tokens, in source order.
    pub classes: Vec<TemplateClass>,
    /// `$style.name` members, in source order.
    pub module_members: Vec<TemplateModuleClass>,
    /// `:class` bindings. Only their string literals and `$style` members are
    /// linked; the rest of each expression is unread.
    pub dynamic_bindings: u32,
}

/// Reads the class tokens of the host's template block. `None` when the file
/// has no template.
#[must_use]
pub fn template_classes(host: &VerifiedSourceExtent<'_>) -> Option<TemplateClasses> {
    let source = host.text();
    let options = SyntaxPluginOptions::default();
    let ctx = SyntaxPluginContext {
        input: source,
        bytes: source.as_bytes(),
        options: &options,
        diagnostics: Vec::new(),
    };
    let mut syntax = Syntax::new(false);
    let mut events = 0usize;
    tokenize_sfc(source.as_bytes(), |event| {
        events += 1;
        if events <= 2_000_000 {
            syntax.handle(&event, &ctx);
        }
    });
    let ast = syntax.template_ast()?;
    let mut found = TemplateClasses::default();
    for node in &ast.nodes {
        let AstNodeKind::Element(element) = &node.kind else { continue };
        for prop in &element.props {
            let name = source.get(prop.start as usize..prop.name_end as usize).unwrap_or("");
            let (Some(start), Some(end)) = (prop.value_start, prop.value_end) else { continue };
            let (start, end) = (start as usize, end as usize);
            if source.get(start..end).is_none() {
                continue;
            }
            if !prop.is_directive && name == "class" {
                push_tokens(source, start..end, false, &mut found);
            } else if prop.is_directive {
                let argument = prop
                    .arg_start
                    .zip(prop.arg_end)
                    .and_then(|(a, b)| source.get(a as usize..b as usize));
                if argument == Some("class") && matches!(name, ":" | "v-bind") {
                    found.dynamic_bindings += 1;
                    scan_expression(source, start..end, &mut found);
                }
            }
        }
    }
    found.classes.sort_by_key(|class| class.range.start);
    found.module_members.sort_by_key(|member| member.range.start);
    Some(found)
}

fn push_tokens(source: &str, range: Range<usize>, conditional: bool, into: &mut TemplateClasses) {
    let text = &source[range.clone()];
    let mut offset = 0usize;
    for piece in text.split_inclusive(char::is_whitespace) {
        let trimmed = piece.trim_end_matches(char::is_whitespace);
        if !trimmed.is_empty() && trimmed.len() <= 256 {
            let from = range.start + offset;
            into.classes.push(TemplateClass {
                token: trimmed.to_owned(),
                range: from..from + trimmed.len(),
                conditional,
            });
        }
        offset += piece.len();
    }
}

/// Reads string literals and `$style.name` members out of a binding without
/// interpreting it. Template literals with `${}` are skipped whole.
fn scan_expression(source: &str, range: Range<usize>, into: &mut TemplateClasses) {
    let text = &source[range.clone()];
    let bytes = text.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"' | b'`') => {
                let start = i + 1;
                let mut j = start;
                let mut plain = true;
                while j < bytes.len() && bytes[j] != quote {
                    if bytes[j] == b'\\' {
                        plain = false;
                        j += 1;
                    } else if quote == b'`' && bytes[j] == b'$' && bytes.get(j + 1) == Some(&b'{') {
                        plain = false;
                    }
                    j += 1;
                }
                if plain && j <= bytes.len() && text.is_char_boundary(start) && text.is_char_boundary(j.min(bytes.len())) {
                    push_tokens(source, range.start + start..range.start + j.min(bytes.len()), true, into);
                }
                i = j + 1;
            }
            b'$' if text[i..].starts_with("$style.") => {
                let name_start = i + "$style.".len();
                let len = text[name_start..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '$'))
                    .unwrap_or(text.len() - name_start);
                if len > 0 {
                    into.module_members.push(TemplateModuleClass {
                        name: text[name_start..name_start + len].to_owned(),
                        range: range.start + name_start..range.start + name_start + len,
                    });
                }
                i = name_start + len;
            }
            _ => i += 1,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use backend_compile::VerifiedSourceText;

    fn classes_of(source: &str) -> Option<TemplateClasses> {
        let verified = VerifiedSourceText::capture(source.as_bytes()).expect("utf-8");
        let host = verified.extent(0..source.len()).expect("host");
        template_classes(&host)
    }

    #[test]
    fn static_and_bound_classes_carry_exact_host_ranges() {
        let source = "<template>\n  <div class=\"card  big\" :class=\"{ 'is-on': on, off: !on }\">\n    <span :class=\"[$style.title, 'x']\" />\n  </div>\n</template>\n<style scoped>.card { color: red; }</style>\n";
        let found = classes_of(source).expect("template");
        for class in &found.classes {
            assert_eq!(&source[class.range.clone()], class.token, "{class:?}");
        }
        let static_tokens: Vec<&str> = found.classes.iter().filter(|c| !c.conditional).map(|c| c.token.as_str()).collect();
        assert_eq!(static_tokens, ["card", "big"]);
        let bound: Vec<&str> = found.classes.iter().filter(|c| c.conditional).map(|c| c.token.as_str()).collect();
        assert_eq!(bound, ["is-on", "x"], "string literals are linked; bare object keys are not");
        assert_eq!(found.dynamic_bindings, 2);
        assert_eq!(found.module_members.len(), 1);
        assert_eq!(&source[found.module_members[0].range.clone()], "title");
    }

    #[test]
    fn a_file_without_a_template_has_no_classes() {
        assert!(classes_of("<script setup>const a = 1</script>\n<style>.a{}</style>").is_none());
    }

    #[test]
    fn template_literals_with_substitutions_are_not_linked() {
        let source = "<template><i :class=\"`btn-${size} fixed`\" /></template>";
        let found = classes_of(source).expect("template");
        assert!(found.classes.is_empty(), "{found:?}");
        assert_eq!(found.dynamic_bindings, 1);
    }
}
