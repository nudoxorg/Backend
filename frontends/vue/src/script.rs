//! Native TSZ parse-only script facts; no project or module authority is implied.
use super::{BlockKind, Container, RawAttribute};
use backend_compile::VerifiedSourceExtent;
use tsz_parser::parser::{ParserState, syntax_kind_ext};

/// The narrow set of TSZ declaration forms exposed by this first slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DeclarationKind {
    /// A simple variable declarator.
    Variable,
    /// A named function declaration.
    Function,
    /// A named class declaration.
    Class,
    /// An interface declaration.
    Interface,
    /// A type alias declaration.
    TypeAlias,
    /// An enum declaration.
    Enum,
    /// A local name introduced by an import; no module binding is implied.
    Import,
    /// A name introduced by a destructuring binding.
    Binding,
}
/// A parser-observed declaration with the exact original host spelling.
#[derive(Clone, Debug)]
pub struct ScriptDeclaration<'a> {
    /// Native syntactic kind; this does not establish a bound symbol.
    pub kind: DeclarationKind,
    /// Full native declaration extent in the same original host.
    pub owner: VerifiedSourceExtent<'a>,
    /// Identifier bytes and both original host identity domains.
    pub name: VerifiedSourceExtent<'a>,
}
/// A direct call locally associated by TSZ with an observed declaration.
/// The target is local to one script region; imported module resolution and
/// cross-block Vue scope are not established.
#[derive(Clone, Debug)]
pub struct ScriptCall<'a> {
    /// Original authored direct-call identifier.
    pub site: VerifiedSourceExtent<'a>,
    /// Exact local declaration name associated by the native binder.
    pub target: VerifiedSourceExtent<'a>,
}
/// One unmodified TSZ diagnostic, optionally mapped into the verified host.
#[derive(Clone, Debug)]
pub struct ScriptSyntaxDiagnostic<'a> {
    /// Native TSZ diagnostic.
    pub diagnostic: tsz_parser::parser::ParseDiagnostic,
    /// Host range if the native position is a valid UTF-8 byte range.
    pub host: Option<VerifiedSourceExtent<'a>>,
}
/// The independent completeness of the syntax-only script observations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptCoverage {
    /// Only selected declaration forms are inventoried; no binding or module
    /// resolution is asserted, including when diagnostics are empty.
    Partial,
}
/// Syntax-only observations. This is always incomplete for Vue semantics,
/// binding, module resolution, and unsupported declaration forms.
#[derive(Clone, Debug)]
pub struct ScriptParse<'a> {
    /// Explicit coverage, independent of diagnostics and result count.
    pub coverage: ScriptCoverage,
    /// Supported parser-observed declarations, including nested declarations.
    pub declarations: Vec<ScriptDeclaration<'a>>,
    /// Direct calls proved by the native binder within this one script block.
    pub calls: Vec<ScriptCall<'a>>,
    /// Direct calls with unavailable or ambiguous local association.
    pub unresolved_calls: usize,
    /// All native parser diagnostics.
    pub diagnostics: Vec<ScriptSyntaxDiagnostic<'a>>,
    /// Number of supported-form declarations whose name was not mapped.
    pub unmapped_names: usize,
}
/// Why this block cannot be parsed as an admitted inline JS/TS script.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptParseError {
    /// No such script block exists.
    NotScript,
    /// External, self-closing or otherwise absent inline content.
    NoInlineContent,
    /// The lang attribute is ambiguous, dynamic, encoded, or unsupported.
    UnsupportedLanguage,
    /// Cooperative native parsing or binding stopped before completion.
    Stopped,
}
impl<'a> Container<'a> {
    /// Parse a verbatim script region using TSZ. The synthetic filename selects
    /// grammar only and is never admitted as a source or module identity.
    pub fn parse_script(&self, block: usize) -> Result<ScriptParse<'a>, ScriptParseError> {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            &cancelled,
            5_000_000,
        );
        self.parse_script_with_checkpoint(block, &budget)
    }
    /// Parse and bind one script under the caller's cooperative work policy.
    /// Stopped native computations are discarded rather than returned as complete.
    pub fn parse_script_with_checkpoint(
        &self,
        block: usize,
        checkpoint: &dyn tsz_common::ExecutionCheckpoint,
    ) -> Result<ScriptParse<'a>, ScriptParseError> {
        if !matches!(self.kind(block), Some(BlockKind::Script { .. })) {
            return Err(ScriptParseError::NotScript);
        }
        let region = self
            .inline_content(block)
            .ok_or(ScriptParseError::NoInlineContent)?;
        let suffix = match self.raw_attribute(block, "lang") {
            Some(RawAttribute::Missing) => "js",
            Some(RawAttribute::Value(value)) => match value.text() {
                "js" => "js",
                "jsx" => "jsx",
                "ts" => "ts",
                "tsx" => "tsx",
                _ => return Err(ScriptParseError::UnsupportedLanguage),
            },
            _ => return Err(ScriptParseError::UnsupportedLanguage),
        };
        let mut parser = ParserState::new(format!("script.{suffix}"), region.text().to_owned());
        let root = parser
            .parse_source_file_with_execution_checkpoint(checkpoint)
            .map_err(|_| ScriptParseError::Stopped)?;
        let diagnostics = parser
            .get_diagnostics()
            .iter()
            .cloned()
            .map(|diagnostic| {
                let start = diagnostic.start as usize;
                let host = start
                    .checked_add(diagnostic.length as usize)
                    .and_then(|end| region.subregion(start..end).ok());
                ScriptSyntaxDiagnostic { diagnostic, host }
            })
            .collect();
        let arena = parser.get_arena();
        let mut binder = tsz_binder::BinderState::new();
        binder
            .bind_source_file_with_execution_checkpoint(arena, root, checkpoint)
            .map_err(|_| ScriptParseError::Stopped)?;
        let mut declarations = Vec::new();
        let mut binding_nodes = Vec::new();
        let mut unmapped_names = 0;
        for (node_index, node) in arena.nodes.iter().enumerate() {
            checkpoint
                .checkpoint(1)
                .map_err(|_| ScriptParseError::Stopped)?;
            let candidate = match node.kind {
                syntax_kind_ext::VARIABLE_DECLARATION => arena
                    .get_variable_declaration(node)
                    .map(|d| (DeclarationKind::Variable, d.name)),
                syntax_kind_ext::FUNCTION_DECLARATION => arena
                    .get_function(node)
                    .map(|d| (DeclarationKind::Function, d.name)),
                syntax_kind_ext::CLASS_DECLARATION => arena
                    .get_class(node)
                    .map(|d| (DeclarationKind::Class, d.name)),
                syntax_kind_ext::INTERFACE_DECLARATION => arena
                    .get_interface(node)
                    .map(|d| (DeclarationKind::Interface, d.name)),
                syntax_kind_ext::TYPE_ALIAS_DECLARATION => arena
                    .get_type_alias(node)
                    .map(|d| (DeclarationKind::TypeAlias, d.name)),
                syntax_kind_ext::ENUM_DECLARATION => arena
                    .get_enum(node)
                    .map(|d| (DeclarationKind::Enum, d.name)),
                syntax_kind_ext::IMPORT_SPECIFIER => arena
                    .get_specifier(node)
                    .map(|d| (DeclarationKind::Import, d.name)),
                syntax_kind_ext::IMPORT_CLAUSE => arena
                    .get_import_clause(node)
                    .filter(|d| !d.name.is_none())
                    .map(|d| (DeclarationKind::Import, d.name)),
                syntax_kind_ext::NAMESPACE_IMPORT => arena
                    .get_named_imports(node)
                    .map(|d| (DeclarationKind::Import, d.name)),
                syntax_kind_ext::BINDING_ELEMENT => arena
                    .get_binding_element(node)
                    .map(|d| (DeclarationKind::Binding, d.name)),
                _ => None,
            };
            if let Some((kind, name)) = candidate {
                let name_node = name.0;
                let mapped = arena
                    .get(name)
                    .filter(|n| arena.get_identifier(n).is_some())
                    .and_then(|n| region.subregion(n.pos as usize..n.end as usize).ok())
                    .filter(|extent| !extent.text().is_empty());
                let owner = region.subregion(node.pos as usize..node.end as usize).ok();
                match (owner, mapped) {
                    (Some(owner), Some(name))
                        if {
                            let outer = owner.byte_range();
                            let inner = name.byte_range();
                            outer.start <= inner.start && inner.end <= outer.end
                        } =>
                    {
                        binding_nodes.push((node_index as u32, name_node, name.clone()));
                        declarations.push(ScriptDeclaration { kind, owner, name })
                    }
                    _ => unmapped_names += 1,
                }
            }
        }
        // Build one bounded native-symbol lookup, avoiding a declaration scan
        // for every call. Multiple declarations for one symbol remain ambiguous.
        let mut targets = std::collections::BTreeMap::new();
        for (node, name_node, name) in binding_nodes {
            let declaration_symbol = binder.node_symbols.get(&node).copied();
            let name_symbol = binder.node_symbols.get(&name_node).copied();
            let symbol = match (declaration_symbol, name_symbol) {
                (Some(a), Some(b)) if a != b => continue,
                (Some(a), _) | (_, Some(a)) => a,
                _ => continue,
            };
            targets
                .entry(symbol)
                .and_modify(|slot: &mut Option<VerifiedSourceExtent<'a>>| *slot = None)
                .or_insert(Some(name));
        }
        let mut calls = Vec::new();
        let mut unresolved_calls = 0usize;
        for node in &arena.nodes {
            checkpoint
                .checkpoint(1)
                .map_err(|_| ScriptParseError::Stopped)?;
            if node.kind != syntax_kind_ext::CALL_EXPRESSION {
                continue;
            }
            let Some(call) = arena.get_call_expr(node) else {
                continue;
            };
            let Some(identifier) = arena
                .get(call.expression)
                .filter(|n| arena.get_identifier(n).is_some())
            else {
                continue;
            };
            let Some(symbol) = binder.resolve_identifier(arena, call.expression) else {
                unresolved_calls += 1;
                continue;
            };
            let Some(Some(target)) = targets.get(&symbol) else {
                unresolved_calls += 1;
                continue;
            };
            let Some(site) = region
                .subregion(identifier.pos as usize..identifier.end as usize)
                .ok()
                .filter(|v| !v.text().is_empty())
            else {
                unresolved_calls += 1;
                continue;
            };
            calls.push(ScriptCall {
                site,
                target: target.clone(),
            });
        }
        Ok(ScriptParse {
            coverage: ScriptCoverage::Partial,
            declarations,
            calls,
            unresolved_calls,
            diagnostics,
            unmapped_names,
        })
    }
}
