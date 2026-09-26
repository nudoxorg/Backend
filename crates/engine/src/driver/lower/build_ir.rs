//! Materialize one owned semantic image from an admitted fact lane.
//!
//! The fact lanes stay on [`super::FactSet`]. This module writes the image
//! builder from those lanes without inventing visibility, members, or spans.

use super::*;

impl<'source> FactSet<'source> {
    /// Materializes the owned semantic image directly from this exact
    /// admitted lane. The compact fragment and image therefore cannot
    /// diverge on declaration names, kinds, or primitive facts.
    ///
    /// Facts not supplied by this lane remain explicit `Unknown` visibility
    /// or absent fields; the image never manufactures visibility, members,
    /// documentation, source spans, or language extensions.
    pub(in crate::driver) fn build_ir(
        &self,
        profile: backend_semantic::vocabulary::LanguageProfile,
        source: backend_semantic::ir::SourceIdentity,
        recipe: backend_semantic::vocabulary::CompileRecipeFact,
        declaration_scope: crate::driver::types::DeclarationScope<'source>,
    ) -> Result<Ir, backend_semantic::ir::BuildError> {
        let fact_count = self.len;
        let mut builder = IrBuilder::new();
        builder.set_language_profile(profile)?;
        if let Some(coordinate) = declaration_scope.coordinate() {
            builder.set_image_provenance_for_package(
                source,
                recipe,
                coordinate,
                declaration_scope.path(),
            )?;
        } else {
            builder.set_image_provenance(
                source,
                recipe,
                declaration_scope.lineage(),
                declaration_scope.path(),
            )?;
        }

        let versions = identity::versions(self, declaration_scope, profile)?;
        let mut tree = builder.reserve_tree(&versions[..fact_count])?;
        // Staging coordinates are segmented tags, not physical offsets.  The
        // projection stores exactly the rows admitted by this transaction.
        let staged_type_end = fact_count + self.anonymous_rows + self.computed_rows;
        let mut type_ids = vec![None; staged_type_end].into_boxed_slice();
        // 0 = unseen, 1 = recursively visiting, 2 = fully interned.  A
        // boolean can only distinguish cache hit from miss and recurses
        // forever on hostile compound type cycles.
        let mut type_seen = vec![0_u8; staged_type_end].into_boxed_slice();
        // Compound projections borrow this one measured transaction buffer;
        // they never allocate temporary vectors or invent placeholder IDs.
        let mut projection_scratch = ProjectionScratch::new(self.projected_type_demand());
        let mut semantic_types = vec![None; fact_count].into_boxed_slice();
        for (ordinal, semantic_type) in semantic_types.iter_mut().take(fact_count).enumerate() {
            *semantic_type = Some(live_type(
                &mut tree,
                self,
                ordinal as u32,
                &mut type_ids,
                &mut type_seen,
                &mut projection_scratch,
            )?);
        }
        // Documentation facts are allowed to arrive interleaved by owner.
        // Count/prefix/scatter once so each tree item borrows one contiguous
        // range without the former O(facts × docs) rescan or an accidental
        // assumption that documentation was grouped at collection time.
        let mut doc_counts = vec![0_usize; fact_count];
        for fact in self.doc_facts[..self.doc_len].iter() {
            let owner = fact.owner.raw as usize;
            let Some(count) = doc_counts.get_mut(owner) else {
                return Err(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Entity,
                    raw: fact.owner.raw,
                });
            };
            *count += 1;
        }
        let mut doc_ranges = vec![(0usize, 0usize); fact_count].into_boxed_slice();
        let mut cursor = 0_usize;
        for (ordinal, count) in doc_counts.iter().copied().enumerate() {
            doc_ranges[ordinal] = (cursor, count);
            cursor += count;
        }
        let mut doc_cursors = doc_ranges.iter().map(|range| range.0).collect::<Vec<_>>();
        let mut docs = vec![DocInput::SoftBreak; self.doc_len];
        for fact in self.doc_facts[..self.doc_len].iter() {
            let owner = fact.owner.raw as usize;
            let slot = doc_cursors[owner];
            docs[slot] = doc_input(&mut tree, fact.fragment)?;
            doc_cursors[owner] += 1;
        }
        // Every sparse language plane is materialized from the same staged
        // rows as the durable fragment.  There is intentionally no
        // TypeScript-only rewrite path: a compact fact can never disappear
        // merely because a caller asks for the owned IR view.
        let extension_demand = ExtensionDemand::measure(&self.extensions[..fact_count]);
        let mut extension_bindings = vec![None; fact_count].into_boxed_slice();
        let mut rewritten_typescript = Vec::with_capacity(extension_demand.typescript);
        let mut rewritten_csharp = Vec::with_capacity(extension_demand.csharp);
        let mut rewritten_go = Vec::with_capacity(extension_demand.go);
        let mut rewritten_rust = Vec::with_capacity(extension_demand.rust);
        let mut rewritten_python = Vec::with_capacity(extension_demand.python);
        let mut rewritten_java = Vec::with_capacity(extension_demand.java);
        let mut rewritten_clang = Vec::with_capacity(extension_demand.clang);
        for (ordinal, extension) in self.extensions[..fact_count].iter().enumerate() {
            match extension {
                Some(EmissionExtension::TypeScript(value)) => {
                    let pool_index = rewritten_typescript.len();
                    rewritten_typescript.push(backend_semantic::ir::TypeScriptFacts {
                        type_parameters: live_type_parameters(
                            &mut tree,
                            self,
                            value.type_parameters,
                            self.type_parameter_ranges[ordinal].ok_or(
                                backend_semantic::ir::BuildError::Dangling {
                                    space: backend_semantic::ir::SemanticSpace::TypeParameters,
                                    raw: value.type_parameters.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        declared: value
                            .declared
                            .map(|id| {
                                live_type(
                                    &mut tree,
                                    self,
                                    id.raw,
                                    &mut type_ids,
                                    &mut type_seen,
                                    &mut projection_scratch,
                                )
                            })
                            .transpose()?,
                        observed: value
                            .observed
                            .map(|id| {
                                live_type(
                                    &mut tree,
                                    self,
                                    id.raw,
                                    &mut type_ids,
                                    &mut type_seen,
                                    &mut projection_scratch,
                                )
                            })
                            .transpose()?,
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::TypeScript(pool_index));
                }
                Some(EmissionExtension::CSharp(value)) => {
                    let pool_index = rewritten_csharp.len();
                    rewritten_csharp.push(backend_semantic::ir::CSharpFacts {
                        constraints: live_type_parameters(
                            &mut tree,
                            self,
                            value.constraints,
                            self.type_parameter_ranges[ordinal].ok_or(
                                backend_semantic::ir::BuildError::Dangling {
                                    space: backend_semantic::ir::SemanticSpace::TypeParameters,
                                    raw: value.constraints.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        attributes: live_atom_list(&mut tree, self, value.attributes)?,
                        xml_provenance: value
                            .xml_provenance
                            .map(|span| live_extension_span(&mut tree, self, span))
                            .transpose()?,
                        ..*value
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::CSharp(pool_index));
                }
                Some(EmissionExtension::Go(value)) => {
                    let pool_index = rewritten_go.len();
                    rewritten_go.push(backend_semantic::ir::GoFacts {
                        signature: backend_semantic::ir::GoSignature {
                            parameters: live_type_list(
                                &mut tree,
                                self,
                                value.signature.parameters,
                                &mut type_ids,
                                &mut type_seen,
                                &mut projection_scratch,
                            )?,
                            results: live_type_list(
                                &mut tree,
                                self,
                                value.signature.results,
                                &mut type_ids,
                                &mut type_seen,
                                &mut projection_scratch,
                            )?,
                            ..value.signature
                        },
                        type_parameters: live_type_parameters(
                            &mut tree,
                            self,
                            value.type_parameters,
                            self.type_parameter_ranges[ordinal].ok_or(
                                backend_semantic::ir::BuildError::Dangling {
                                    space: backend_semantic::ir::SemanticSpace::TypeParameters,
                                    raw: value.type_parameters.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        fields: live_entity_list(&mut tree, self, value.fields)?,
                        method_set: live_entity_list(&mut tree, self, value.method_set)?,
                        build_constraints: live_atom_list(
                            &mut tree,
                            self,
                            value.build_constraints,
                        )?,
                        constant_value: live_atom_list(&mut tree, self, value.constant_value)?,
                        ..*value
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::Go(pool_index));
                }
                Some(EmissionExtension::Rust(value)) => {
                    let pool_index = rewritten_rust.len();
                    rewritten_rust.push(backend_semantic::ir::RustFacts {
                        lifetimes: live_atom_list(&mut tree, self, value.lifetimes)?,
                        where_clauses: live_type_parameters(
                            &mut tree,
                            self,
                            value.where_clauses,
                            self.type_parameter_ranges[ordinal].ok_or(
                                backend_semantic::ir::BuildError::Dangling {
                                    space: backend_semantic::ir::SemanticSpace::TypeParameters,
                                    raw: value.where_clauses.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        macros: live_atom_list(&mut tree, self, value.macros)?,
                        const_defaults: live_atom_list(&mut tree, self, value.const_defaults)?,
                        free_predicates: live_free_predicates(
                            &mut tree,
                            self,
                            value.free_predicates,
                            self.free_predicate_ranges[ordinal].ok_or(
                                backend_semantic::ir::BuildError::Dangling {
                                    space: backend_semantic::ir::SemanticSpace::FreePredicates,
                                    raw: value.free_predicates.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        ..*value
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::Rust(pool_index));
                }
                Some(EmissionExtension::Python(value)) => {
                    let pool_index = rewritten_python.len();
                    rewritten_python.push(backend_semantic::ir::PythonFacts {
                        decorators: live_atom_list(&mut tree, self, value.decorators)?,
                        ..*value
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::Python(pool_index));
                }
                Some(EmissionExtension::Java(value)) => {
                    let pool_index = rewritten_java.len();
                    rewritten_java.push(backend_semantic::ir::JavaFacts {
                        throws: live_type_list(
                            &mut tree,
                            self,
                            value.throws,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        annotations: live_atom_list(&mut tree, self, value.annotations)?,
                        overloads: live_entity_list(&mut tree, self, value.overloads)?,
                        record_components: live_entity_list(
                            &mut tree,
                            self,
                            value.record_components,
                        )?,
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::Java(pool_index));
                }
                Some(EmissionExtension::Clang(value)) => {
                    let pool_index = rewritten_clang.len();
                    rewritten_clang.push(backend_semantic::ir::ClangFacts {
                        templates: live_type_parameters(
                            &mut tree,
                            self,
                            value.templates,
                            self.type_parameter_ranges[ordinal].ok_or(
                                backend_semantic::ir::BuildError::Dangling {
                                    space: backend_semantic::ir::SemanticSpace::TypeParameters,
                                    raw: value.templates.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        includes: live_atom_list(&mut tree, self, value.includes)?,
                        ..*value
                    });
                    extension_bindings[ordinal] = Some(RewrittenExtension::Clang(pool_index));
                }
                None => {}
            }
        }
        // Member lists enter owned IR only when the authority explicitly
        // captured that parent's complete local set. A bound child alone is
        // containment evidence, never permission to manufacture a partial
        // member list from parentage.
        let mut member_counts = vec![0_usize; fact_count];
        for parent in self.provenance.parentage()[..fact_count]
            .iter()
            .copied()
            .filter_map(local_parent)
        {
            if self.provenance.member_sets().get(parent.index())
                != Some(&MemberSetCapture::Captured)
            {
                continue;
            }
            let Some(count) = member_counts.get_mut(parent.raw as usize) else {
                return Err(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Entity,
                    raw: parent.raw,
                });
            };
            *count += 1;
        }
        let mut member_ranges = vec![(0_usize, 0_usize); fact_count];
        let mut member_total = 0_usize;
        for (ordinal, count) in member_counts.iter().copied().enumerate() {
            member_ranges[ordinal] = (member_total, count);
            member_total += count;
        }
        let mut member_cursors = member_ranges
            .iter()
            .map(|range| range.0)
            .collect::<Vec<_>>();
        let mut members = vec![backend_semantic::ir::TreeEntityId::new(0); member_total];
        for (child, parentage) in self.provenance.parentage()[..fact_count]
            .iter()
            .copied()
            .enumerate()
        {
            let Some(parent) = local_parent(parentage) else {
                continue;
            };
            if self.provenance.member_sets().get(parent.index())
                != Some(&MemberSetCapture::Captured)
            {
                continue;
            }
            let slot = member_cursors[parent.raw as usize];
            members[slot] = backend_semantic::ir::TreeEntityId::new(child as u32);
            member_cursors[parent.raw as usize] += 1;
        }
        let source_file = self.provenance.source_spans()[..fact_count]
            .iter()
            .any(Option::is_some)
            // The declaration scope carries the authority-entered,
            // package-relative source path.  Source content identity remains
            // in the image provenance header; using its digest as a span's
            // file atom would make path navigation impossible after reopen.
            .then(|| tree.intern_atom(declaration_scope.path().as_bytes()))
            .transpose()?;
        let mut item_attribute_ranges = vec![(0_usize, 0_usize); fact_count].into_boxed_slice();
        let mut item_attribute_total = 0_usize;
        for (ordinal, extension) in self.extensions[..fact_count].iter().enumerate() {
            let Some(list) = extension.as_ref().and_then(extension_item_attributes) else {
                continue;
            };
            let list = list.raw as usize;
            if self.atom_list_len == 0 && list == 0 {
                continue;
            }
            let Some(row) = self.atom_lists.row(list) else {
                return Err(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::AtomList,
                    raw: list as u32,
                });
            };
            let length = row.len();
            item_attribute_ranges[ordinal] = (item_attribute_total, length);
            item_attribute_total = item_attribute_total.checked_add(length).ok_or(
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::AtomList,
                    raw: list as u32,
                },
            )?;
        }
        let empty_attribute: &'source [u8] = &[];
        let mut item_attributes = vec![empty_attribute; item_attribute_total].into_boxed_slice();
        for (ordinal, extension) in self.extensions[..fact_count].iter().enumerate() {
            let Some(list) = extension.as_ref().and_then(extension_item_attributes) else {
                continue;
            };
            let list = list.raw as usize;
            if self.atom_list_len == 0 && list == 0 {
                continue;
            }
            let (start, length) = item_attribute_ranges[ordinal];
            let Some(row) = self.atom_lists.row(list) else {
                return Err(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::AtomList,
                    raw: list as u32,
                });
            };
            for (relative, provisional) in row[..length].iter().enumerate() {
                let offset = usize::try_from(*provisional).map_err(|_| {
                    backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Atom,
                        raw: *provisional,
                    }
                })?;
                let attribute = *self.extension_atoms.get(offset).ok_or(
                    backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Atom,
                        raw: *provisional,
                    },
                )?;
                let position = start.checked_add(relative).ok_or(
                    backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::AtomList,
                        raw: list as u32,
                    },
                )?;
                let slot = item_attributes.get_mut(position).ok_or(
                    backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::AtomList,
                        raw: list as u32,
                    },
                )?;
                *slot = attribute;
            }
        }
        let empty_item = TreeItemInput {
            name: b"",
            kind: ItemKind::Function,
            visibility: Visibility::Unknown,
            authority: EntityAuthorityFacts::default(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        };
        let mut items = vec![empty_item; fact_count].into_boxed_slice();
        for (ordinal, item) in items.iter_mut().take(fact_count).enumerate() {
            let extension = match extension_bindings[ordinal] {
                Some(RewrittenExtension::TypeScript(index)) => Some(
                    LanguageExtensionInput::TypeScript(rewritten_extension_fact(
                        &rewritten_typescript,
                        index,
                        backend_semantic::ir::Language::TypeScript,
                        ordinal,
                    )?),
                ),
                Some(RewrittenExtension::CSharp(index)) => {
                    Some(LanguageExtensionInput::CSharp(rewritten_extension_fact(
                        &rewritten_csharp,
                        index,
                        backend_semantic::ir::Language::CSharp,
                        ordinal,
                    )?))
                }
                Some(RewrittenExtension::Go(index)) => {
                    Some(LanguageExtensionInput::Go(rewritten_extension_fact(
                        &rewritten_go,
                        index,
                        backend_semantic::ir::Language::Go,
                        ordinal,
                    )?))
                }
                Some(RewrittenExtension::Rust(index)) => {
                    Some(LanguageExtensionInput::Rust(rewritten_extension_fact(
                        &rewritten_rust,
                        index,
                        backend_semantic::ir::Language::Rust,
                        ordinal,
                    )?))
                }
                Some(RewrittenExtension::Python(index)) => {
                    Some(LanguageExtensionInput::Python(rewritten_extension_fact(
                        &rewritten_python,
                        index,
                        backend_semantic::ir::Language::Python,
                        ordinal,
                    )?))
                }
                Some(RewrittenExtension::Java(index)) => {
                    Some(LanguageExtensionInput::Java(rewritten_extension_fact(
                        &rewritten_java,
                        index,
                        backend_semantic::ir::Language::Java,
                        ordinal,
                    )?))
                }
                Some(RewrittenExtension::Clang(index)) => {
                    Some(LanguageExtensionInput::Clang(rewritten_extension_fact(
                        &rewritten_clang,
                        index,
                        backend_semantic::ir::Language::Clang,
                        ordinal,
                    )?))
                }
                None => None,
            };
            *item = TreeItemInput {
                name: self.names[ordinal],
                kind: self.kinds[ordinal],
                visibility: self.visibility[ordinal],
                authority: self.entity_authority(ordinal, &versions[..fact_count])?,
                parent: local_parent(self.provenance.parentage()[ordinal])
                    .map(|parent| backend_semantic::ir::TreeEntityId::new(parent.raw)),
                semantic_type: semantic_types[ordinal],
                members: &members
                    [member_ranges[ordinal].0..member_ranges[ordinal].0 + member_ranges[ordinal].1],
                docs: &docs[doc_ranges[ordinal].0..doc_ranges[ordinal].0 + doc_ranges[ordinal].1],
                attributes: &item_attributes[item_attribute_ranges[ordinal].0
                    ..item_attribute_ranges[ordinal].0 + item_attribute_ranges[ordinal].1],
                source: self.provenance.source_spans()[ordinal].and_then(|span| {
                    source_file.and_then(|file| {
                        backend_semantic::ir::SourceSpan::new(file, span.start, span.end)
                    })
                }),
                extension,
            };
        }
        let mut links = Vec::with_capacity(self.occurrence_len);
        for index in 0..self.occurrence_len {
            let owner = self.occurrence_owners[index];
            let occurrence = self.occurrences[index];
            let source = occurrence_source_span(self, source_file, owner, occurrence.span)?;
            links.push(TreeLinkInput {
                from: backend_semantic::ir::TreeEntityId::new(owner),
                target: external_from_occurrence(
                    &mut tree,
                    backend_semantic::ir::EntityId::new(owner),
                    occurrence.target,
                )?,
                kind: occurrence_link_kind(occurrence.kind),
                confidence: occurrence_link_confidence(occurrence.confidence),
                authority: OccurrenceAuthorityFacts {
                    source: if source.is_some() {
                        FactAvailability::Captured
                    } else {
                        FactAvailability::Unavailable
                    },
                },
                source,
            });
        }
        tree.commit(&items[..fact_count], &links)?;
        builder.finish()
    }
}
