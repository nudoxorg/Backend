//! Native semantic pages measured with every exposed continuation and guard.
use super::*;
use backend_library::{SemanticSourceRequest, SemanticVersionsPage};
use wire_admission::ResponseBudget;

impl<P: Product> Server<P> {
    pub(super) fn source_membership_tool(
        &mut self,
        request: &backend_library::PackageSourceMembershipPageRequestV1,
        detail: Detail,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        let mut selection = backend_present::PackageSourceMembershipSelection::new(request)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let lifetime = self.semantic_lifetime()?;
        let budget = ResponseBudget::new(response_id);
        loop {
            lifetime.check().map_err(semantic_client_error)?;
            let command = SurfaceCommand::PackageSourceMembership {
                request: selection.request().clone(),
            };
            let reply = self
                .product
                .surface(command.clone())
                .map_err(semantic_client_error)?;
            selection
                .admit_reply(&reply)
                .map_err(|fault| RpcError::from_fault(&fault))?;
            let arguments = semantic_arguments(invocation, &command)?;
            let rendered = self
                .semantic_result(
                    &command,
                    reply,
                    detail,
                    invocation.with_arguments(&arguments),
                )
                .and_then(|result| budget.admit(result));
            lifetime.check().map_err(semantic_client_error)?;
            match rendered {
                Ok(result) => return Ok(result),
                Err(error)
                    if error
                        .fault
                        .as_ref()
                        .is_some_and(|fault| selection.shrink_after(fault)) => {}
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn semantic_lifetime(&mut self) -> Result<RequestContext, RpcError> {
        let lifetime = self.request_context.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(semantic_client_error)?;
        self.product
            .set_request_context(Some(lifetime.clone()))
            .map_err(semantic_client_error)?;
        Ok(lifetime)
    }

    pub(super) fn semantic_versions_tool(
        &mut self,
        command: &SurfaceCommand,
        detail: Detail,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        let SurfaceCommand::SemanticVersions {
            package,
            limit,
            cursor,
        } = command
        else {
            return Err(RpcError::invalid("semantic version command changed shape"));
        };
        command
            .admit()
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        let lifetime = self.semantic_lifetime()?;
        let budget = ResponseBudget::new(response_id);
        let mut effective = *limit;
        let mut pinned = None;
        loop {
            lifetime.check().map_err(semantic_client_error)?;
            let selected = SurfaceCommand::SemanticVersions {
                package: package.clone(),
                limit: effective,
                cursor: cursor.clone(),
            };
            let reply = self
                .product
                .surface(selected.clone())
                .map_err(semantic_client_error)?;
            backend_library::admit_surface_reply(&selected, &reply)
                .map_err(|error| RpcError::tool(error.to_string()))?;
            let SurfaceReply::SemanticVersionsPage(page) = &reply else {
                return self
                    .semantic_result(&selected, reply, detail, invocation)
                    .and_then(|result| budget.admit(result));
            };
            let basis = (
                page.source_relation_root,
                page.semantic_relation_root,
                page.total,
            );
            if pinned.is_some_and(|expected| expected != basis) {
                return Err(RpcError::tool(
                    "semantic version roots changed while selecting a bounded page; restart discovery explicitly",
                ));
            }
            pinned = Some(basis);
            let arguments = semantic_arguments(invocation, &selected)?;
            let rendered = self
                .semantic_result(
                    &selected,
                    reply,
                    detail,
                    invocation.with_arguments(&arguments),
                )
                .and_then(|result| budget.admit(result));
            lifetime.check().map_err(semantic_client_error)?;
            match rendered {
                Ok(result) => return Ok(result),
                Err(error) if cursor.is_none() && effective > 1 && oversized(&error) => {
                    effective = effective.div_ceil(2);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Smaller byte ranges preserve the same portable guard and exact offset.
    /// Each native reply is correlated to the selected request before exposure.
    /// No bytes from an oversized attempt are spliced into the successful page.
    pub(super) fn semantic_source_tool(
        &mut self,
        request: &SemanticSourceRequest,
        detail: Detail,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        self.semantic_source_prepare(request, response_id, |server, selected, reply| {
            let command = SurfaceCommand::SemanticSource {
                request: selected.clone(),
            };
            command
                .admit()
                .map_err(|error| RpcError::invalid(error.to_string()))?;
            let arguments = semantic_arguments(invocation, &command)?;
            server.semantic_result(
                &command,
                reply,
                detail,
                invocation.with_arguments(&arguments),
            )
        })
    }

    pub(super) fn semantic_source_resource(
        &mut self,
        request: &SemanticSourceRequest,
        uri: &str,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        self.semantic_source_prepare(request, response_id, |_server, selected, reply| {
            let SurfaceReply::SemanticSource(chunk) = reply else {
                return Err(RpcError::tool("native semantic source resource changed shape"));
            };
            let next_uri = chunk.next.map(|offset| {
                let mut next = selected.clone();
                next.range.offset = offset;
                next.resource_uri()
            }).transpose().map_err(|error| RpcError::tool(error.to_string()))?;
            let text = serde_json::to_string(&chunk).map_err(|error| RpcError::tool(error.to_string()))?;
            resources::encode_result("resource", json!({
                "contents":[{"uri":uri,"mimeType":"application/json","text":text}],
                "_meta":{"backend/semanticSourceRange":selected.range,"backend/nextUri":next_uri}
            }))
        })
    }

    fn semantic_source_prepare(
        &mut self,
        request: &SemanticSourceRequest,
        response_id: &Value,
        mut render: impl FnMut(
            &mut Self,
            &SemanticSourceRequest,
            SurfaceReply,
        ) -> Result<Value, RpcError>,
    ) -> Result<Value, RpcError> {
        let lifetime = self.semantic_lifetime()?;
        let budget = ResponseBudget::new(response_id);
        let mut selected = request.clone();
        SurfaceCommand::SemanticSource {
            request: selected.clone(),
        }
        .admit()
        .map_err(|error| RpcError::invalid(error.to_string()))?;
        loop {
            lifetime.check().map_err(semantic_client_error)?;
            let command = SurfaceCommand::SemanticSource {
                request: selected.clone(),
            };
            let reply = self
                .product
                .surface(command.clone())
                .map_err(semantic_client_error)?;
            backend_library::admit_surface_reply(&command, &reply)
                .map_err(|error| RpcError::tool(error.to_string()))?;
            let result = render(self, &selected, reply).and_then(|result| budget.admit(result));
            lifetime.check().map_err(semantic_client_error)?;
            match result {
                Ok(result) => return Ok(result),
                Err(error) if selected.range.max_bytes > 1 && oversized(&error) => {
                    selected.range.max_bytes = selected.range.max_bytes.div_ceil(2);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn semantic_result(
        &mut self,
        command: &SurfaceCommand,
        reply: SurfaceReply,
        detail: Detail,
        invocation: McpInvocation<'_>,
    ) -> Result<Value, RpcError> {
        let raw = invocation.selected_name() == SURFACE_TOOL;
        if !raw {
            let context = continuation_context(
                &self.project,
                invocation.selected_name(),
                invocation.arguments(),
                detail,
            );
            return self.rendered_admitted(
                Answer::Product(Box::new(backend_present::product_view_for_owned_command(
                    command, reply,
                ))),
                detail,
                &context,
                CursorTarget::CliOption,
                invocation,
            );
        }
        let next_command = match (&reply, command) {
            (
                SurfaceReply::SemanticVersionsPage(page),
                SurfaceCommand::SemanticVersions { package, limit, .. },
            ) => page
                .next_cursor
                .as_ref()
                .map(|cursor| SurfaceCommand::SemanticVersions {
                    package: package.clone(),
                    limit: *limit,
                    cursor: Some(cursor.clone()),
                }),
            (SurfaceReply::SemanticSource(chunk), SurfaceCommand::SemanticSource { request }) => {
                chunk.next.map(|offset| {
                    let mut request = request.clone();
                    request.range.offset = offset;
                    SurfaceCommand::SemanticSource { request }
                })
            }
            (SurfaceReply::PackageSourceMembershipPage(backend_library::PackageSourceMembershipPageResultV1::Page {
                source_relation_root, source_version, next, ..
            }), SurfaceCommand::PackageSourceMembership { request }) => next.as_ref().map(|cursor| {
                let request = request.clone().with_selection(*source_relation_root, *source_version).with_cursor(cursor.clone());
                SurfaceCommand::PackageSourceMembership { request }
            }),
            _ => None,
        };
        let next = next_command
            .as_ref()
            .map(|command| invocation.surface_semantic_continuation_call(command));
        let mut view = backend_present::product_view_for_command(command, &reply);
        view.expose_mcp_advice(invocation.exposure());
        // Raw full detail remains the canonical native SurfaceReply. Summary
        // explicitly names a display projection and keeps the same exact next call.
        if detail == Detail::Summary && let SurfaceReply::SemanticVersionsPage(page) = &reply {
            let projected = backend_present::ProductDto::for_detail(&view, detail);
            let mut body = serde_json::json!({
                "surface": {
                    "result": "semantic-versions-summary",
                    "data": projected.semantic_versions_summary,
                },
                "continuation_call": next,
            });
            if next.is_none() && let Some(fields) = body.as_object_mut() {
                fields.remove("continuation_call");
            }
            let payload = encode_serializable(
                "surface", detail, None, body, DEFAULT_RESPONSE_BUDGET_BYTES,
            ).map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
            let structured = serde_json::from_slice(&payload.bytes)
                .map_err(|error| RpcError::tool(error.to_string()))?;
            return Ok(complete_tool_result(
                &format!("{}\nFor native source references, request detail=full (fresh discovery).\n",
                    versions_preview(page, next_command.is_some(), invocation)), structured, false,
            ));
        }
        let payload = encode_serializable(
            "surface",
            detail,
            None,
            SurfaceBody {
                continuation_call: next.as_ref(),
                fault: view.fault().map(backend_present::FaultDto::new),
                index_poll: None,
                surface: SurfaceProjection {
                    reply: &reply,
                    next_cursor: None,
                    index_job: None,
                    compact_receipt: false,
                },
            },
            DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
        let structured = serde_json::from_slice(&payload.bytes)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        let mut text = match &reply {
            SurfaceReply::SemanticVersionsPage(page) => {
                versions_preview(page, next.is_some(), invocation)
            }
            _ => markdown::product_with_detail(&view, detail),
        };
        if matches!(&reply, SurfaceReply::SemanticSource(_)) && next.is_some() {
            text.push_str("\n\n");
            text.push_str(invocation.continuation_note());
        }
        Ok(complete_tool_result(
            &text,
            structured,
            view.fault().is_some(),
        ))
    }
}

pub(super) fn semantic_client_error(error: ClientError) -> RpcError {
    RpcError::from_fault(&Fault::from_client_error(
        &error,
        backend_present::Operand::Whole,
    ))
}
pub(super) fn oversized(error: &RpcError) -> bool {
    error
        .fault
        .as_ref()
        .is_some_and(|fault| fault.cause().slug() == backend_present::CauseSlug::Oversized)
}
fn semantic_arguments(
    invocation: McpInvocation<'_>,
    command: &SurfaceCommand,
) -> Result<Map<String, Value>, RpcError> {
    let mut arguments = invocation.arguments().clone();
    if invocation.selected_name() == SURFACE_TOOL {
        arguments.insert(
            "command".to_owned(),
            serde_json::to_value(command).map_err(|error| RpcError::tool(error.to_string()))?,
        );
    } else {
        match command {
            SurfaceCommand::SemanticVersions { limit, .. } => {
                arguments.insert("limit".to_owned(), json!(limit));
            }
            SurfaceCommand::SemanticSource { request } => {
                arguments.insert("request".to_owned(), json!(request));
            }
            SurfaceCommand::PackageSourceMembership { request } => {
                arguments.insert("request".to_owned(), json!(request));
            }
            _ => return Err(RpcError::tool("semantic arguments changed shape")),
        }
    }
    Ok(arguments)
}

/// Every coordinate occupies a complete line. An oversized page is resized
/// upstream, never cut through a coordinate or silently counted as complete.
pub(super) fn versions_preview(
    page: &SemanticVersionsPage,
    more: bool,
    invocation: McpInvocation<'_>,
) -> String {
    let mut text = format!(
        "# Semantic versions\nVersions {}..{} of {}; {} descriptor(s) on this page; {} remain after this page.\n",
        page.offset,
        page.offset + page.entries.len() as u64,
        page.total,
        page.entries.len(),
        page.total - page.offset - page.entries.len() as u64
    );
    for entry in &page.entries {
        let metadata = entry.metadata();
        text.push('\n');
        text.push_str(metadata.coordinate.as_str());
        text.push_str(&format!(
            "\ngeneration {}; {} artifact(s); compiler coverage {}; source {}; {}; {}\n",
            encode_id(&metadata.generation.to_bytes()),
            metadata.artifacts,
            if metadata.complete {
                "complete"
            } else {
                "partial"
            },
            match entry {
                backend_library::SemanticVersionDescriptor::Available { .. } => "available",
                _ => "unavailable",
            },
            if metadata.selected {
                "selected"
            } else {
                "not selected"
            },
            match metadata.freshness {
                backend_library::SemanticVersionFreshness::Current { .. } => "current source input",
                backend_library::SemanticVersionFreshness::Historical { .. } =>
                    "historical source input",
                backend_library::SemanticVersionFreshness::Unverified => "freshness unverified",
            },
        ));
        if let backend_library::SemanticSourceHistory::Published { proof, .. } = entry.history() {
            text.push_str(&format!(
                "published complete history: {} image(s)\n",
                proof.image_count
            ));
        }
    }
    if more {
        text.push('\n');
        text.push_str(invocation.continuation_note());
    }
    text
}
