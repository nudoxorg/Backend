//! Bounded native Vue SFC container experiment. This is not product admission.
//!
//! Verter's Vue-derived tokenizer and syntax parser operate on original UTF-8
//! bytes. We snapshot finalized roots on each parser event because its final
//! descriptor replaces duplicate scripts and templates. The original host is
//! the only source authority; no generated or concatenated text is indexed.
use verter_core::diagnostics::{Diagnostic, SyntaxPluginContext, SyntaxPluginOptions};
use verter_core::parser::Syntax;
use verter_core::tokenizer::{byte::tokenize_sfc, Event};
use verter_core::types::{NodeProp, NodeTag};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub name: std::ops::Range<usize>,
    pub value: Option<std::ops::Range<usize>>,
    pub directive: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub name: std::ops::Range<usize>,
    pub open: std::ops::Range<usize>,
    pub content: Option<std::ops::Range<usize>>,
    pub close: Option<std::ops::Range<usize>>,
    pub attributes: Vec<Attribute>,
}
impl Block {
    /// Translate an embedded parser's UTF-8 byte range to the original host.
    /// Reject out-of-region and non-character-boundary endpoints.
    pub fn host_range(&self, source: &str, local: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
        let content = self.content.as_ref()?;
        let embedded = source.get(content.clone())?;
        embedded.get(local.clone())?;
        Some(content.start.checked_add(local.start)?..content.start.checked_add(local.end)?)
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// The proof extracts this block only; its grammar has not been analyzed.
    UnanalyzedBlock { block: usize },
    /// An external source needs separately captured identity and resolution.
    ExternalSource { block: usize },
    /// Vue permits at most one template; Verter does not diagnose this here.
    DuplicateTemplate { block: usize },
}
#[derive(Debug)]
pub struct Container {
    pub blocks: Vec<Block>,
    pub diagnostics: Vec<Diagnostic>,
    pub gaps: Vec<Gap>,
}
fn convert(open: &NodeTag, close: Option<&NodeTag>, content: Option<(u32, u32)>, attrs: &[NodeProp]) -> Block {
    Block {
        name: (open.start as usize + 1)..open.name_end as usize,
        open: open.start as usize..open.end as usize,
        content: content.map(|(a,b)| a as usize..b as usize),
        close: close.map(|tag| tag.start as usize..tag.end as usize),
        attributes: attrs.iter().map(|a| Attribute {
            name: a.start as usize..a.name_end as usize,
            value: a.value_start.zip(a.value_end).map(|(a,b)| a as usize..b as usize),
            directive: a.is_directive,
        }).collect(),
    }
}
fn record(blocks: &mut Vec<Block>, block: Block) {
    if let Some(previous) = blocks.iter_mut().find(|old| old.open.start == block.open.start) {
        *previous = block;
    } else { blocks.push(block); }
}
/// Returns a structured refusal if source length cannot fit Verter's u32 spans.
pub fn parse(source: &str) -> Result<Container, &'static str> {
    if source.len() > u32::MAX as usize { return Err("Vue source exceeds native span capacity"); }
    let options = SyntaxPluginOptions::default();
    let ctx = SyntaxPluginContext { input: source, bytes: source.as_bytes(), options: &options, diagnostics: Vec::new() };
    let mut syntax = Syntax::new(false);
    let mut blocks = Vec::new();
    tokenize_sfc(source.as_bytes(), |event| {
        syntax.handle(&event, &ctx);
        if !matches!(event, Event::CloseTag { .. } | Event::SelfClosingTag { .. } | Event::End) { return; }
        for node in [syntax.script(), syntax.script_setup()].into_iter().flatten() {
            record(&mut blocks, convert(&node.tag_open, node.tag_close.as_ref(), node.content.map(|s| (s.start, s.end)), &node.attributes));
        }
        for node in syntax.style_nodes() {
            record(&mut blocks, convert(&node.tag_open, node.tag_close.as_ref(), node.content.map(|s| (s.start, s.end)), &node.attributes));
        }
        for node in syntax.unknown_nodes() {
            record(&mut blocks, convert(&node.tag_open, node.tag_close.as_ref(), node.content.map(|s| (s.start, s.end)), &node.attributes));
        }
        if let Some(ast) = syntax.template_ast() {
            let node = &ast.root;
            record(&mut blocks, convert(&node.tag_open, node.tag_close.as_ref(), node.content.as_ref().map(|s| (s.start, s.end)), &node.attributes));
        }
    });
    blocks.sort_by_key(|block| block.open.start);
    let valid = |range: &std::ops::Range<usize>| {
        range.start <= range.end && source.get(range.clone()).is_some()
    };
    if blocks.iter().any(|b| !valid(&b.name) || !valid(&b.open)
        || b.content.as_ref().is_some_and(|r| !valid(r))
        || b.close.as_ref().is_some_and(|r| !valid(r))
        || b.attributes.iter().any(|a| !valid(&a.name) || a.value.as_ref().is_some_and(|r| !valid(r)))) {
        return Err("native Vue parser returned an invalid host byte range");
    }
    let mut gaps = Vec::new();
    let mut template_seen = false;
    for (index, block) in blocks.iter().enumerate() {
        gaps.push(Gap::UnanalyzedBlock { block: index });
        if &source[block.name.clone()] == "template" {
            if template_seen { gaps.push(Gap::DuplicateTemplate { block: index }); }
            template_seen = true;
        }
        if block.attributes.iter().any(|a| !a.directive && &source[a.name.clone()] == "src") {
            gaps.push(Gap::ExternalSource { block: index });
        }
    }
    Ok(Container { blocks, diagnostics: syntax.take_diagnostics(), gaps })
}
