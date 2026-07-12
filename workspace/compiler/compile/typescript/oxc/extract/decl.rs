//! Declarations → `ir::kind::Entry` (+ member entries). The integrator of the
//! extract layer: dispatches a grouped [`SymbolGroup`] to the per-kind helpers
//! and assembles [`FactEntry`]s carrying provisional paths + type refs.
//!
//! See OXC-PORT-SPEC.md §1 for the exact per-declaration IR shapes to reproduce.
//! LEAF FILE — fill the `todo!()` bodies.

use std::path::PathBuf;

use oxc_ast::ast::{
    Class, ClassElement, Declaration, Function, MethodDefinitionKind, PropertyDefinitionType,
    PropertyKey, Statement, TSAccessibility, TSEnumDeclaration, TSEnumMemberName,
    TSInterfaceDeclaration, TSModuleDeclaration, TSModuleDeclarationBody,
    TSModuleDeclarationName, TSSignature, TSTypeAliasDeclaration, TSTypeName, VariableDeclaration,
    VariableDeclarationKind,
};

use ir::{
    entry::NudoxPath,
    function::Function as IrFunction,
    generics::TraitRef,
    kind::{Entry, Symbol, Visibility},
    module::Module,
    protocols::{ReceiverKind, TraitDef, TraitMethod},
    record::{Record, SumVariant},
    ty::Type,
};

use super::{
    types::PropertyFieldMetadata, Extractor, FactEntry, Result, SymbolGroup,
};

// ─── helpers ──────────────────────────────────────────────────────────────────

/// Select the primary declaration: prefer the one with a body (has_body = body.is_some()).
/// Fall back to the last declaration. Mirrors `pick_primary_declaration`.
fn pick_primary<'a>(group: &SymbolGroup<'a>) -> &'a Declaration<'a> {
    group
        .declarations
        .iter()
        .find(|d| decl_has_body(d))
        .or_else(|| group.declarations.last())
        .copied()
        .expect("SymbolGroup must have at least one declaration")
}

fn decl_has_body(decl: &Declaration<'_>) -> bool {
    match decl {
        Declaration::FunctionDeclaration(f) => f.body.is_some(),
        _ => false,
    }
}

fn accessibility_to_visibility(acc: Option<TSAccessibility>) -> Visibility {
    match acc {
        Some(TSAccessibility::Private) => Visibility::Private,
        Some(TSAccessibility::Protected) => Visibility::Protected,
        _ => Visibility::Public,
    }
}

/// Helper: empty Vec → None.
fn empty_to_none<T>(v: Vec<T>) -> Option<Vec<T>> {
    if v.is_empty() { None } else { Some(v) }
}

/// Flatten a `TSTypeName` to a dotted string (`A.B.C`).
fn ts_type_name_to_string(name: &TSTypeName<'_>) -> String {
    match name {
        TSTypeName::IdentifierReference(id) => id.name.to_string(),
        TSTypeName::QualifiedName(q) => {
            format!("{}.{}", ts_type_name_to_string(&q.left), q.right.name)
        }
        TSTypeName::ThisExpression(_) => "this".to_string(),
    }
}

/// Name string from a `PropertyKey`.
fn property_key_name<'a>(key: &PropertyKey<'a>, source: &'a str) -> String {
    match key {
        PropertyKey::StaticIdentifier(id) => id.name.to_string(),
        PropertyKey::PrivateIdentifier(id) => id.name.to_string(),
        _ => key.span().source_text(source).to_string(),
    }
}

// ─── impl block ───────────────────────────────────────────────────────────────

impl<'a> Extractor<'a> {
    /// Lower one grouped symbol into its IR entries: the primary entry plus any
    /// member entries (class methods/constructor, namespace elements). Applies
    /// the group's name / visibility / documentation and, for functions, folds
    /// non-primary declarations into `overloads`.
    pub(crate) fn lower_symbol(&mut self, group: &SymbolGroup<'a>) -> Result<Vec<FactEntry>> {
        let primary = pick_primary(group);
        let name = &group.name;
        let visibility = group.visibility.clone();

        // Get DocFacts from the primary declaration's node_id.
        let doc_facts = self.doc_facts_for_declaration(primary);
        // If `@ignore` is set, suppress entirely.
        if doc_facts.ignore {
            return Ok(vec![]);
        }
        let documentation = doc_facts.doc;
        let deprecation = doc_facts.deprecation;

        // Placeholder path — link phase assigns the real path.
        let placeholder_path = NudoxPath::Local(PathBuf::from(""));

        let mut entries: Vec<FactEntry> = Vec::new();

        match primary {
            // ─── Function ─────────────────────────────────────────────────
            Declaration::FunctionDeclaration(func) => {
                self.type_ref_scratch.clear();
                let mut ir_func = self.lower_function(func)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);

                // Overloads: other function declarations in the group.
                ir_func.overloads = self.function_overloads(group, func)?;

                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: ir_func,
                };
                entries.push(FactEntry {
                    entry: Entry::Function(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                });
            }

            // ─── Variable / Constant ──────────────────────────────────────
            Declaration::VariableDeclaration(var_decl) => {
                let var_entries = self.lower_variable(var_decl)?;
                // lower_variable already uses the right name from each declarator;
                // wrap with group-level documentation for single-declarator case.
                for mut fe in var_entries {
                    // Patch documentation/deprecation/visibility on the primary entry.
                    if let Some(patch) = entry_symbol_mut(&mut fe.entry) {
                        patch.apply(
                            documentation.clone(),
                            deprecation.clone(),
                            visibility.clone(),
                        );
                    }
                    entries.push(fe);
                }
            }

            // ─── TypeAlias ────────────────────────────────────────────────
            Declaration::TSTypeAliasDeclaration(alias) => {
                self.type_ref_scratch.clear();
                let ty = self.lower_type_alias(alias)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);

                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: ty,
                };
                entries.push(FactEntry {
                    entry: Entry::TypeAlias(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                });
            }

            // ─── Enum / SumType ───────────────────────────────────────────
            Declaration::TSEnumDeclaration(en) => {
                self.type_ref_scratch.clear();
                let variants = self.lower_enum(en)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);

                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: variants,
                };
                entries.push(FactEntry {
                    entry: Entry::SumType(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                });
            }

            // ─── Class / RecordType ───────────────────────────────────────
            Declaration::ClassDeclaration(cls) => {
                self.type_ref_scratch.clear();
                let (record, mut member_entries) = self.lower_class(name, cls)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);

                // Build member refs from the already-produced member entries.
                let member_refs: Vec<NudoxPath> = member_entries
                    .iter()
                    .map(|fe| NudoxPath::Local(PathBuf::from(fe.local_path.join("::"))))
                    .collect();

                let mut record = record;
                record.members = empty_to_none(member_refs);

                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: record,
                };
                entries.push(FactEntry {
                    entry: Entry::RecordType(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                });
                entries.append(&mut member_entries);
            }

            // ─── Interface / TraitDef ─────────────────────────────────────
            Declaration::TSInterfaceDeclaration(iface) => {
                self.type_ref_scratch.clear();
                let trait_def = self.lower_interface(name, iface)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);

                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: trait_def,
                };
                entries.push(FactEntry {
                    entry: Entry::TraitDef(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                });
            }

            // ─── Namespace / Module ───────────────────────────────────────
            Declaration::TSModuleDeclaration(ns) => {
                // Also merge any additional namespace declarations in the group
                // (augmentation merging: same name, multiple TSModuleDeclaration).
                let mut ns_entries = self.lower_namespace(name, ns)?;

                // Additional declarations: merge namespace entries.
                for extra_decl in group
                    .declarations
                    .iter()
                    .filter(|d| !std::ptr::eq(*d, primary))
                {
                    if let Declaration::TSModuleDeclaration(extra_ns) = extra_decl {
                        let mut extra = self.lower_namespace(name, extra_ns)?;
                        ns_entries.append(&mut extra);
                    }
                }

                let member_refs: Vec<NudoxPath> = ns_entries
                    .iter()
                    .filter(|fe| fe.local_path.len() == 2)
                    .map(|fe| NudoxPath::Local(PathBuf::from(fe.local_path.join("::"))))
                    .collect();

                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: Module { members: empty_to_none(member_refs) },
                };
                entries.push(FactEntry {
                    entry: Entry::Module(sym),
                    local_path: vec![name.clone()],
                    type_refs: Vec::new(),
                });
                entries.append(&mut ns_entries);
            }

            // ─── Unsupported / Reference (synthesised by link) ────────────
            _ => {
                // Triple-slash References are synthesised in link.rs.
                // Other unexpected declarations become an Info entry.
                let sym = Symbol {
                    name: name.clone(),
                    path: placeholder_path,
                    aliases: None,
                    visibility,
                    documentation,
                    deprecation,
                    doc_links: None,
                    inner: String::new(),
                };
                entries.push(FactEntry {
                    entry: Entry::Info(sym),
                    local_path: vec![name.clone()],
                    type_refs: Vec::new(),
                });
            }
        }

        Ok(entries)
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Lower a class into its `Record` plus separated constructor/method member
    /// entries (member paths recorded on the record).
    pub(crate) fn lower_class(
        &mut self,
        name: &str,
        cls: &Class<'a>,
    ) -> Result<(Record, Vec<FactEntry>)> {
        // ── fields from PropertyDefinition ───────────────────────────────
        let mut fields = Vec::new();
        let mut index_signatures_ir = Vec::new();
        let mut extra_entries: Vec<FactEntry> = Vec::new();

        for elem in cls.body.body.iter() {
            match elem {
                ClassElement::PropertyDefinition(prop) => {
                    let prop_name = property_key_name(&prop.key, self.source);
                    let mut decorators: Vec<String> = prop
                        .decorators
                        .iter()
                        .map(|d| d.span.source_text(self.source).to_string())
                        .collect();
                    if prop.r#override {
                        decorators.push("override".to_string());
                    }
                    if prop.r#type == PropertyDefinitionType::TSAbstractPropertyDefinition {
                        decorators.push("abstract".to_string());
                    }
                    let ty_opt = prop.type_annotation.as_ref().map(|ann| &ann.type_annotation);
                    let field = self.property_field(
                        &prop_name,
                        ty_opt,
                        PropertyFieldMetadata {
                            optional: prop.optional,
                            readonly: prop.readonly,
                            is_static: prop.r#static,
                            visibility: Some(accessibility_to_visibility(prop.accessibility)),
                            documentation: None, // JSDoc via node_id on property would need semantic
                            decorators: &decorators,
                        },
                    )?;
                    fields.push(field);
                }
                ClassElement::TSIndexSignature(sig) => {
                    index_signatures_ir.push(self.index_signature(sig)?);
                }
                _ => {}
            }
        }

        // ── super types ──────────────────────────────────────────────────
        let mut super_types: Vec<Type> = Vec::new();
        if let Some(extends_expr) = &cls.super_class {
            // Extract the identifier from the extends expression (Expression variant).
            let id_str = match extends_expr.as_ref() {
                oxc_ast::ast::Expression::Identifier(id) => id.name.to_string(),
                other => other.span().source_text(self.source).to_string(),
            };
            let generic_args = if let Some(tp) = &cls.super_type_arguments {
                let args: Result<Vec<_>> = tp
                    .params
                    .iter()
                    .map(|t| self.lower_ts_type(t).map(ir::generics::GenericArg::Type))
                    .collect();
                let args = args?;
                if args.is_empty() { None } else { Some(args) }
            } else {
                None
            };
            super_types.push(Type::TypeReference(ir::ty::TypeReference {
                identifier: id_str,
                generic_args,
            }));
        }
        for implements in cls.implements.iter() {
            // TSClassImplements has `expression: TSTypeName` + optional type_arguments
            let id_str = ts_type_name_to_string(&implements.expression);
            let generic_args = if let Some(tp) = &implements.type_arguments {
                let args: Result<Vec<_>> = tp
                    .params
                    .iter()
                    .map(|t| self.lower_ts_type(t).map(ir::generics::GenericArg::Type))
                    .collect();
                let args = args?;
                if args.is_empty() { None } else { Some(args) }
            } else {
                None
            };
            super_types.push(Type::TypeReference(ir::ty::TypeReference {
                identifier: id_str,
                generic_args,
            }));
        }

        // ── generics ─────────────────────────────────────────────────────
        let generics = if let Some(tp) = &cls.type_parameters {
            self.lower_type_params(tp)?
        } else {
            None
        };

        let record = Record {
            name: Some(name.to_string()),
            generics,
            fields,
            call_signatures: None,
            constructors: None,
            methods: None,
            index_signatures: empty_to_none(index_signatures_ir),
            super_types: empty_to_none(super_types),
            implemented_protocols: None,
            members: None, // filled by caller after member extraction
        };

        // ── constructors ─────────────────────────────────────────────────
        let ctor_methods: Vec<_> = cls
            .body
            .body
            .iter()
            .filter_map(|elem| {
                if let ClassElement::MethodDefinition(m) = elem {
                    if m.kind == MethodDefinitionKind::Constructor {
                        return Some(m.as_ref());
                    }
                }
                None
            })
            .collect();

        if !ctor_methods.is_empty() {
            // Primary: prefer body.is_some(), fall back to last.
            let primary_idx = ctor_methods
                .iter()
                .rposition(|m| m.value.body.is_some())
                .unwrap_or(ctor_methods.len().saturating_sub(1));
            let primary_ctor = ctor_methods[primary_idx];

            self.type_ref_scratch.clear();
            let mut ctor_func = self.constructor_signature(&primary_ctor.value)?;
            let type_refs = std::mem::take(&mut self.type_ref_scratch);

            // Overloads: all non-primary ctor signatures.
            let overloads: Result<Vec<_>> = ctor_methods
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != primary_idx)
                .map(|(_, m)| self.constructor_signature(&m.value))
                .collect();
            let overloads = overloads?;
            ctor_func.overloads = empty_to_none(overloads);

            let ctor_visibility = accessibility_to_visibility(primary_ctor.accessibility);
            let ctor_sym = Symbol {
                name: "constructor".to_string(),
                path: NudoxPath::Local(PathBuf::from("")),
                aliases: None,
                visibility: ctor_visibility,
                documentation: None,
                deprecation: None,
                doc_links: None,
                inner: ctor_func,
            };
            extra_entries.push(FactEntry {
                entry: Entry::Function(ctor_sym),
                local_path: vec![name.to_string(), "constructor".to_string()],
                type_refs,
            });
        }

        // ── methods ───────────────────────────────────────────────────────
        // Group by method name (dedup by first occurrence, include all overloads).
        let mut seen_methods: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
        let method_defs: Vec<_> = cls
            .body
            .body
            .iter()
            .filter_map(|elem| {
                if let ClassElement::MethodDefinition(m) = elem {
                    if m.kind != MethodDefinitionKind::Constructor {
                        return Some(m.as_ref());
                    }
                }
                None
            })
            .collect();

        for method in &method_defs {
            let method_name = property_key_name(&method.key, self.source);
            if !seen_methods.insert(method_name.clone()) {
                continue; // already processed this name
            }
            // Collect all methods with this name.
            let same_name: Vec<_> = method_defs
                .iter()
                .filter(|m| property_key_name(&m.key, self.source) == method_name)
                .collect();

            let primary_idx = same_name
                .iter()
                .rposition(|m| m.value.body.is_some())
                .unwrap_or(same_name.len().saturating_sub(1));
            let primary_method = same_name[primary_idx];

            self.type_ref_scratch.clear();
            let mut method_func = self.lower_function(&primary_method.value)?;
            let type_refs = std::mem::take(&mut self.type_ref_scratch);

            // Overloads: all non-primary same-name methods.
            let overloads: Result<Vec<_>> = same_name
                .iter()
                .enumerate()
                .filter(|(idx, _)| *idx != primary_idx)
                .map(|(_, m)| self.lower_function(&m.value))
                .collect();
            let overloads = overloads?;
            method_func.overloads = empty_to_none(overloads);

            let method_visibility =
                accessibility_to_visibility(primary_method.accessibility);
            let method_sym = Symbol {
                name: method_name.clone(),
                path: NudoxPath::Local(PathBuf::from("")),
                aliases: None,
                visibility: method_visibility,
                documentation: None,
                deprecation: None,
                doc_links: None,
                inner: method_func,
            };
            extra_entries.push(FactEntry {
                entry: Entry::Function(method_sym),
                local_path: vec![name.to_string(), method_name],
                type_refs,
            });
        }

        Ok((record, extra_entries))
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Lower an interface into `TraitDef`.
    pub(crate) fn lower_interface(
        &mut self,
        _name: &str,
        iface: &TSInterfaceDeclaration<'a>,
    ) -> Result<TraitDef> {
        let generics = if let Some(tp) = &iface.type_parameters {
            self.lower_type_params(tp)?
        } else {
            None
        };

        // extends → super_traits
        let mut super_traits: Vec<TraitRef> = Vec::new();
        for heritage in iface.extends.iter() {
            // TSInterfaceHeritage has `expression` (the base type name) + optional type_arguments
            let id_str = heritage.expression.span().source_text(self.source).to_string();
            let args: Vec<ir::generics::TypeExpr> = if let Some(tp) = &heritage.type_arguments {
                tp.params
                    .iter()
                    .map(|t| -> Result<ir::generics::TypeExpr> {
                        let ty = self.lower_ts_type(t)?;
                        Ok(self.type_to_expr(&ty))
                    })
                    .collect::<Result<Vec<_>>>()?
            } else {
                Vec::new()
            };
            super_traits.push(TraitRef { name: id_str, args });
        }

        let mut required_methods: Vec<TraitMethod> = Vec::new();
        let mut properties: Vec<ir::record::Field> = Vec::new();

        // Enumerate call/index signature indices separately.
        let mut call_idx: usize = 0;
        let mut index_idx: usize = 0;

        for sig in iface.body.body.iter() {
            match sig {
                TSSignature::TSMethodSignature(m) => {
                    let method_name = property_key_name(&m.key, self.source);
                    let generics = if let Some(tp) = &m.type_parameters {
                        self.lower_type_params(tp)?
                    } else {
                        None
                    };
                    let (receiver, parameters) =
                        self.lower_params_with_receiver(m.params.as_ref(), Some(ReceiverKind::SharedRef))?;
                    let return_type = m
                        .return_type
                        .as_ref()
                        .map(|ann| self.lower_ts_type(&ann.type_annotation).map(Box::new))
                        .transpose()?;
                    required_methods.push(TraitMethod {
                        name: method_name,
                        parameters,
                        return_type,
                        generics,
                        attributes: None,
                        documentation: None,
                        receiver,
                        has_default_implementation: false,
                    });
                }
                TSSignature::TSCallSignatureDeclaration(call) => {
                    let method_name = if call_idx == 0 {
                        "__call".to_string()
                    } else {
                        format!("__call_{}", call_idx)
                    };
                    call_idx += 1;
                    let generics = if let Some(tp) = &call.type_parameters {
                        self.lower_type_params(tp)?
                    } else {
                        None
                    };
                    let (receiver, parameters) =
                        self.lower_params_with_receiver(call.params.as_ref(), Some(ReceiverKind::SharedRef))?;
                    let return_type = call
                        .return_type
                        .as_ref()
                        .map(|ann| self.lower_ts_type(&ann.type_annotation).map(Box::new))
                        .transpose()?;
                    required_methods.push(TraitMethod {
                        name: method_name,
                        parameters,
                        return_type,
                        generics,
                        attributes: None,
                        documentation: None,
                        receiver,
                        has_default_implementation: false,
                    });
                }
                TSSignature::TSIndexSignature(idx_sig) => {
                    // Lower as `__index*` method (index signature as method).
                    let method_name = if index_idx == 0 {
                        "__index".to_string()
                    } else {
                        format!("__index_{}", index_idx)
                    };
                    index_idx += 1;
                    // Build a simple TraitMethod from the index signature.
                    let ir_sig = self.index_signature(idx_sig)?;
                    let key_param = ir::parameter::Parameter::Literal(
                        ir::parameter::LiteralParameter {
                            name: "key".to_string(),
                            r#type: Some(*ir_sig.key_type),
                            attributes: None,
                            default_value: None,
                            description: None,
                        },
                    );
                    let return_type = Some(Box::new(*ir_sig.value_type));
                    required_methods.push(TraitMethod {
                        name: method_name,
                        parameters: Some(vec![key_param]),
                        return_type,
                        generics: None,
                        attributes: None,
                        documentation: None,
                        receiver: Some(ReceiverKind::SharedRef),
                        has_default_implementation: false,
                    });
                }
                TSSignature::TSPropertySignature(prop) => {
                    let prop_name = property_key_name(&prop.key, self.source);
                    let ty_opt =
                        prop.type_annotation.as_ref().map(|ann| &ann.type_annotation);
                    let mut decorators: Vec<String> = Vec::new();
                    if prop.readonly {
                        decorators.push("readonly".to_string());
                    }
                    let field = self.property_field(
                        &prop_name,
                        ty_opt,
                        PropertyFieldMetadata {
                            optional: prop.optional,
                            readonly: prop.readonly,
                            is_static: prop.r#static,
                            visibility: None,
                            documentation: None,
                            decorators: &decorators,
                        },
                    )?;
                    properties.push(field);
                }
                TSSignature::TSConstructSignatureDeclaration(_cs) => {
                    // Construct signatures: lower as `new` method (optional enhancement).
                    // For now, map to a trait method named `new`.
                    required_methods.push(TraitMethod {
                        name: "new".to_string(),
                        parameters: None,
                        return_type: None,
                        generics: None,
                        attributes: None,
                        documentation: None,
                        receiver: Some(ReceiverKind::Static),
                        has_default_implementation: false,
                    });
                }
            }
        }

        Ok(TraitDef {
            generics,
            super_traits: empty_to_none(super_traits),
            associated_types: None,
            properties: empty_to_none(properties),
            required_methods: empty_to_none(required_methods),
            provided_methods: None,
            required_constants: None,
            attributes: None,
            object_safe: None,
            sealed: None,
            cfg: None,
            members: None,
        })
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Lower an enum into its `SumVariant`s.
    pub(crate) fn lower_enum(&mut self, en: &TSEnumDeclaration<'a>) -> Result<Vec<SumVariant>> {
        let variants = en
            .body
            .members
            .iter()
            .map(|member| {
                let member_name = match &member.id {
                    TSEnumMemberName::Identifier(id) => id.name.to_string(),
                    TSEnumMemberName::String(s) => s.value.to_string(),
                    TSEnumMemberName::ComputedString(s) => s.value.to_string(),
                    TSEnumMemberName::ComputedTemplateString(t) => {
                        t.span.source_text(self.source).to_string()
                    }
                };
                SumVariant {
                    name: member_name,
                    data: None,
                    documentation: None,
                }
            })
            .collect();
        Ok(variants)
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Lower a namespace / ambient module into member entries.
    pub(crate) fn lower_namespace(
        &mut self,
        parent_name: &str,
        ns: &TSModuleDeclaration<'a>,
    ) -> Result<Vec<FactEntry>> {
        let mut member_entries: Vec<FactEntry> = Vec::new();

        // Flatten nested `module A { module B { ... } }` into the block.
        let block = match &ns.body {
            Some(TSModuleDeclarationBody::TSModuleBlock(block)) => block.as_ref(),
            Some(TSModuleDeclarationBody::TSModuleDeclaration(nested)) => {
                // nested namespace: recurse
                let nested_name = match &nested.id {
                    TSModuleDeclarationName::Identifier(id) => id.name.to_string(),
                    TSModuleDeclarationName::StringLiteral(s) => s.value.to_string(),
                };
                let full_name = format!("{}.{}", parent_name, nested_name);
                return self.lower_namespace(&full_name, nested);
            }
            None => return Ok(Vec::new()),
        };

        for stmt in block.body.iter() {
            let mut sub = self.lower_ns_stmt(stmt, parent_name)?;
            member_entries.append(&mut sub);
        }

        Ok(member_entries)
    }

    /// Lower a single statement inside a `TSModuleBlock`, prepending
    /// `parent_name` to all produced local_paths.
    fn lower_ns_stmt(
        &mut self,
        stmt: &'a Statement<'a>,
        parent_name: &str,
    ) -> Result<Vec<FactEntry>> {
        // Determine if the stmt is an export-wrapped declaration.
        let (is_exported, inner_stmt): (bool, &'a Statement<'a>) = match stmt {
            Statement::ExportNamedDeclaration(exp) if exp.declaration.is_some() => {
                (true, stmt) // we'll unwrap the inner decl below
            }
            Statement::ExportDefaultDeclaration(_) => return Ok(Vec::new()),
            _ => (false, stmt),
        };

        // Unwrap ExportNamedDeclaration to its inner declaration for lowering.
        // We do this by matching again on the inner decl.
        let (elem_name, mut sub_entries) = match inner_stmt {
            Statement::ExportNamedDeclaration(exp) => {
                if let Some(decl) = &exp.declaration {
                    let name = match decl_name(decl) {
                        Some(n) => n,
                        None => return Ok(Vec::new()),
                    };
                    let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                    let group = SymbolGroup {
                        name: name.clone(),
                        is_default: false,
                        declarations: vec![decl],
                        visibility: vis,
                    };
                    (name, self.lower_symbol(&group)?)
                } else {
                    return Ok(Vec::new());
                }
            }
            // Bare declaration variants (INHERIT flattened).
            Statement::FunctionDeclaration(f) => {
                let name = f.id.as_ref().map(|id| id.name.to_string());
                let name = match name {
                    Some(n) => n,
                    None => return Ok(Vec::new()),
                };
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                self.type_ref_scratch.clear();
                let mut ir_func = self.lower_function(f)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);
                // No overloads in a simple namespace stmt.
                ir_func.overloads = None;
                let sym = Symbol {
                    name: name.clone(),
                    path: NudoxPath::Local(PathBuf::from("")),
                    aliases: None,
                    visibility: vis,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: ir_func,
                };
                let fe = FactEntry {
                    entry: Entry::Function(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                };
                (name, vec![fe])
            }
            Statement::ClassDeclaration(c) => {
                let name = c.id.as_ref().map(|id| id.name.to_string());
                let name = match name { Some(n) => n, None => return Ok(Vec::new()) };
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                self.type_ref_scratch.clear();
                let (record, mut member_entries) = self.lower_class(&name, c)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);
                let member_refs: Vec<NudoxPath> = member_entries
                    .iter()
                    .map(|fe| NudoxPath::Local(PathBuf::from(fe.local_path.join("::"))))
                    .collect();
                let mut record = record;
                record.members = empty_to_none(member_refs);
                let sym = Symbol {
                    name: name.clone(),
                    path: NudoxPath::Local(PathBuf::from("")),
                    aliases: None,
                    visibility: vis,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: record,
                };
                let mut entries = vec![FactEntry {
                    entry: Entry::RecordType(sym),
                    local_path: vec![name.clone()],
                    type_refs,
                }];
                entries.append(&mut member_entries);
                (name, entries)
            }
            Statement::VariableDeclaration(v) => {
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                let entries = self.lower_variable(v)?;
                let entries: Vec<FactEntry> = entries.into_iter().map(|mut fe| {
                    if let Some(patch) = entry_symbol_mut(&mut fe.entry) {
                        patch.apply(None, None, vis.clone());
                    }
                    fe
                }).collect();
                let first_name = entries.first()
                    .and_then(|fe| fe.local_path.first())
                    .cloned()
                    .unwrap_or_default();
                (first_name, entries)
            }
            Statement::TSTypeAliasDeclaration(alias) => {
                let name = alias.id.name.to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                self.type_ref_scratch.clear();
                let ty = self.lower_type_alias(alias)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);
                let sym = Symbol {
                    name: name.clone(),
                    path: NudoxPath::Local(PathBuf::from("")),
                    aliases: None,
                    visibility: vis,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: ty,
                };
                (name, vec![FactEntry { entry: Entry::TypeAlias(sym), local_path: vec![name.clone()], type_refs }])
            }
            Statement::TSInterfaceDeclaration(iface) => {
                let name = iface.id.name.to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                self.type_ref_scratch.clear();
                let trait_def = self.lower_interface(&name, iface)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);
                let sym = Symbol {
                    name: name.clone(),
                    path: NudoxPath::Local(PathBuf::from("")),
                    aliases: None,
                    visibility: vis,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: trait_def,
                };
                (name, vec![FactEntry { entry: Entry::TraitDef(sym), local_path: vec![name.clone()], type_refs }])
            }
            Statement::TSEnumDeclaration(en) => {
                let name = en.id.name.to_string();
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                self.type_ref_scratch.clear();
                let variants = self.lower_enum(en)?;
                let type_refs = std::mem::take(&mut self.type_ref_scratch);
                let sym = Symbol {
                    name: name.clone(),
                    path: NudoxPath::Local(PathBuf::from("")),
                    aliases: None,
                    visibility: vis,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: variants,
                };
                (name, vec![FactEntry { entry: Entry::SumType(sym), local_path: vec![name.clone()], type_refs }])
            }
            Statement::TSModuleDeclaration(nested_ns) => {
                let ns_name = match &nested_ns.id {
                    TSModuleDeclarationName::Identifier(id) => id.name.to_string(),
                    TSModuleDeclarationName::StringLiteral(s) => s.value.to_string(),
                };
                let full_name = format!("{}.{}", parent_name, ns_name);
                let ns_entries = self.lower_namespace(&ns_name, nested_ns)?;
                // These will be re-prefixed below.
                // Build a Module entry for the nested namespace.
                let member_refs: Vec<NudoxPath> = ns_entries
                    .iter()
                    .filter(|fe| fe.local_path.len() == 1)
                    .map(|fe| NudoxPath::Local(PathBuf::from(fe.local_path.join("::"))))
                    .collect();
                let vis = if is_exported { Visibility::Public } else { Visibility::Private };
                let sym = Symbol {
                    name: ns_name.clone(),
                    path: NudoxPath::Local(PathBuf::from("")),
                    aliases: None,
                    visibility: vis,
                    documentation: None,
                    deprecation: None,
                    doc_links: None,
                    inner: Module { members: empty_to_none(member_refs) },
                };
                let mut entries = vec![FactEntry {
                    entry: Entry::Module(sym),
                    local_path: vec![ns_name.clone()],
                    type_refs: Vec::new(),
                }];
                entries.extend(ns_entries);
                let _ = full_name;
                (ns_name, entries)
            }
            _ => return Ok(Vec::new()),
        };

        // Prepend parent_name to all local_paths.
        for fe in &mut sub_entries {
            let mut new_path = vec![parent_name.to_string()];
            new_path.extend_from_slice(&fe.local_path);
            fe.local_path = new_path;
        }
        let _ = elem_name;
        Ok(sub_entries)
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Lower a variable declaration into per-binding entries.
    pub(crate) fn lower_variable(
        &mut self,
        decl: &VariableDeclaration<'a>,
    ) -> Result<Vec<FactEntry>> {
        let mut entries = Vec::new();
        let is_const = decl.kind == VariableDeclarationKind::Const;

        for declarator in decl.declarations.iter() {
            // Extract name from the binding pattern.
            let var_name = match &declarator.id {
                oxc_ast::ast::BindingPattern::BindingIdentifier(id) => id.name.to_string(),
                other => other.span().source_text(self.source).to_string(),
            };

            // Type: explicit annotation → referenced-symbol → initializer inference.
            self.type_ref_scratch.clear();
            let _inferred_type: Option<Type> =
                if let Some(ann) = &declarator.type_annotation {
                    Some(self.lower_ts_type(&ann.type_annotation)?)
                } else if let Some(init) = &declarator.init {
                    self.infer_type_from_expr(init, is_const)
                } else {
                    None
                };
            let type_refs = std::mem::take(&mut self.type_ref_scratch);

            let sym: Symbol<()> = Symbol {
                name: var_name.clone(),
                path: NudoxPath::Local(PathBuf::from("")),
                aliases: None,
                visibility: Visibility::Public,
                documentation: None,
                deprecation: None,
                doc_links: None,
                inner: (),
            };

            let entry = if is_const {
                Entry::Constant(sym)
            } else {
                Entry::Variable(sym)
            };

            entries.push(FactEntry {
                entry,
                local_path: vec![var_name],
                type_refs,
            });
        }

        Ok(entries)
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Lower a type alias's RHS type.
    pub(crate) fn lower_type_alias(
        &mut self,
        alias: &TSTypeAliasDeclaration<'a>,
    ) -> Result<Type> {
        self.lower_ts_type(&alias.type_annotation)
    }

    // ─────────────────────────────────────────────────────────────────────────

    /// Collect non-primary function declarations in a group as overloads.
    pub(crate) fn function_overloads(
        &mut self,
        group: &SymbolGroup<'a>,
        primary: &Function<'a>,
    ) -> Result<Option<Vec<IrFunction>>> {
        // Only meaningful when the primary is a function declaration.
        let overloads: Result<Vec<_>> = group
            .declarations
            .iter()
            .filter_map(|d| {
                if let Declaration::FunctionDeclaration(f) = d {
                    // Use pointer comparison to skip the primary.
                    if !std::ptr::eq(f.as_ref() as *const Function, primary as *const Function) {
                        return Some(self.lower_function(f));
                    }
                }
                None
            })
            .collect();
        let overloads = overloads?;
        Ok(empty_to_none(overloads))
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Private helpers
    // ─────────────────────────────────────────────────────────────────────────

    /// Get DocFacts for a top-level declaration by probing the node's attached JSDoc.
    fn doc_facts_for_declaration(&self, decl: &Declaration<'a>) -> super::DocFacts {
        let node_id = match decl {
            Declaration::FunctionDeclaration(f) => f.node_id.get(),
            Declaration::ClassDeclaration(c) => c.node_id.get(),
            Declaration::VariableDeclaration(v) => v.node_id.get(),
            Declaration::TSTypeAliasDeclaration(a) => a.node_id.get(),
            Declaration::TSInterfaceDeclaration(i) => i.node_id.get(),
            Declaration::TSEnumDeclaration(e) => e.node_id.get(),
            Declaration::TSModuleDeclaration(m) => m.node_id.get(),
            _ => return super::DocFacts::default(),
        };
        self.jsdoc_for_node(node_id)
    }
}

// ─── free helpers ─────────────────────────────────────────────────────────────

/// Mutable access to the `Symbol` inside any `Entry` variant that wraps a `Symbol<T>`.
/// Returns `None` for variants that don't have a common Symbol trait (all do, but
/// we need a way to patch them without full pattern matching).
/// We only need name/doc/visibility patching for the primary symbol, so this
/// returns a helper wrapper.
struct SymbolPatch<'e> {
    documentation: &'e mut Option<String>,
    deprecation: &'e mut Option<ir::kind::Deprecation>,
    visibility: &'e mut ir::kind::Visibility,
}

fn entry_symbol_mut(entry: &mut Entry) -> Option<SymbolPatch<'_>> {
    match entry {
        Entry::Function(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::Constant(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::Variable(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::TypeAlias(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::SumType(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::RecordType(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::TraitDef(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::Module(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        Entry::Info(s) => Some(SymbolPatch {
            documentation: &mut s.documentation,
            deprecation: &mut s.deprecation,
            visibility: &mut s.visibility,
        }),
        _ => None,
    }
}

impl<'e> SymbolPatch<'e> {
    fn apply(
        self,
        doc: Option<String>,
        dep: Option<ir::kind::Deprecation>,
        vis: ir::kind::Visibility,
    ) {
        *self.documentation = doc;
        *self.deprecation = dep;
        *self.visibility = vis;
    }
}

/// Extract the declared name from a `Declaration`.
fn decl_name(decl: &Declaration<'_>) -> Option<String> {
    match decl {
        Declaration::FunctionDeclaration(f) => {
            f.id.as_ref().map(|id| id.name.to_string())
        }
        Declaration::ClassDeclaration(c) => {
            c.id.as_ref().map(|id| id.name.to_string())
        }
        Declaration::VariableDeclaration(v) => {
            v.declarations.first().and_then(|d| match &d.id {
                oxc_ast::ast::BindingPattern::BindingIdentifier(id) => {
                    Some(id.name.to_string())
                }
                _ => None,
            })
        }
        Declaration::TSTypeAliasDeclaration(a) => Some(a.id.name.to_string()),
        Declaration::TSInterfaceDeclaration(i) => Some(i.id.name.to_string()),
        Declaration::TSEnumDeclaration(e) => Some(e.id.name.to_string()),
        Declaration::TSModuleDeclaration(m) => match &m.id {
            TSModuleDeclarationName::Identifier(id) => Some(id.name.to_string()),
            TSModuleDeclarationName::StringLiteral(s) => Some(s.value.to_string()),
        },
        _ => None,
    }
}
