//! Native graph pages selected against the exact exposed wire response.
use super::*;

/// The registered reader and raw operator retain their existing reply shapes.
#[derive(Clone, Copy)]
pub(super) enum GraphToolProjection {
    Registered,
    Surface,
}

impl<P: Product> Server<P> {
    pub(super) fn package_graph_tool(
        &mut self,
        request: &backend_library::PackageGraphPageRequest,
        arguments: &Map<String, Value>,
        detail: Detail,
        invocation: McpInvocation<'_>,
        response_id: &Value,
        projection: GraphToolProjection,
    ) -> Result<Value, RpcError> {
        let lifetime = self.request_context.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
        self.product
            .set_request_context(Some(lifetime.clone()))
            .map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
        let mut selection = backend_present::PackageGraphPageSelection::new(request)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        loop {
            lifetime.check().map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
            let command = SurfaceCommand::PackageGraphPage {
                request: selection.request().clone(),
            };
            let reply = self.product.surface(command.clone()).map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
            let SurfaceReply::PackageGraphPage(page) = &reply else {
                return Err(RpcError::invalid("native graph page changed shape"));
            };
            selection
                .admit_page(page)
                .map_err(|fault| RpcError::from_fault(&fault))?;
            // A fixed page may fit JSON even when duplicate readable text or
            // transport metadata does not. Prove that alternative before the
            // owned reply moves into its registered presentation.
            let json_alternative = if selection.is_fixed_size() {
                backend_present::package_graph_json_alternative(
                    page,
                    selection.request(),
                    detail,
                    self.product.cli_context(),
                )
            } else {
                None
            };
            let rendered = match projection {
                GraphToolProjection::Registered => {
                    let answer = Answer::Product(Box::new(
                        backend_present::product_view_for_owned_command(&command, reply),
                    ));
                    let mut page_arguments = arguments.clone();
                    page_arguments.insert("limit".to_owned(), json!(selection.request().limit));
                    let effective = invocation.with_arguments(&page_arguments);
                    let context = continuation_context(
                        &self.project,
                        invocation.selected_name(),
                        &page_arguments,
                        detail,
                    );
                    self.rendered_admitted(
                        answer,
                        detail,
                        &context,
                        CursorTarget::CliOption,
                        effective,
                    )
                }
                GraphToolProjection::Surface => {
                    surface_graph_result(&reply, selection.request(), detail, invocation)
                }
            };
            let result = match rendered {
                Ok(result) => result,
                Err(error) => {
                    lifetime
                        .check()
                        .map_err(semantic_pages::semantic_client_error)?;
                    if error
                        .fault
                        .as_ref()
                        .is_some_and(|fault| selection.shrink_after(fault))
                    {
                        continue;
                    }
                    if let Some(fault) = error.fault.as_ref()
                        && fault.cause().slug() == backend_present::CauseSlug::Oversized
                        && let Some(advice) = json_alternative
                    {
                        return Err(RpcError::from_fault(&fault.clone().with_affordance(advice)));
                    }
                    return Err(error);
                }
            };
            // Measure the actual ID, text, native rows, executable continuation,
            // fixed-point metadata and newline before choosing the page. The
            // final codec repeats exactly this admission for both transports.
            let budget = wire_admission::ResponseBudget::new(response_id);
            let measured = budget.prepare(result);
            // Native work and tokenization can finish after the caller stops.
            // Preserve that stop before success, resize, or fallback advice.
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            if measured.fits() {
                let mut envelope = measured.into_value();
                return envelope
                    .as_object_mut()
                    .and_then(|object| object.remove("result"))
                    .ok_or_else(|| RpcError::invalid("admitted graph envelope changed shape"));
            }
            let fault = wire_admission::ResponseBudget::fault(&measured);
            if !selection.shrink_after(&fault) {
                let fault = match json_alternative {
                    Some(advice) => fault.with_affordance(advice),
                    None => fault,
                };
                return Err(RpcError::from_fault(&fault));
            }
        }
    }
}

fn surface_graph_result(
    reply: &SurfaceReply,
    request: &backend_library::PackageGraphPageRequest,
    detail: Detail,
    invocation: McpInvocation<'_>,
) -> Result<Value, RpcError> {
    let SurfaceReply::PackageGraphPage(page) = reply else {
        return Err(RpcError::invalid("native surface graph changed shape"));
    };
    let continuation = match &page.terminal {
        backend_library::PackageGraphPageTerminal::More(cursor) => {
            Some(invocation.surface_graph_continuation_call(request, cursor))
        }
        _ => None,
    };
    let payload = encode_serializable(
        "surface",
        detail,
        None,
        SurfaceBody {
            continuation_call: continuation.as_ref(),
            fault: None,
            index_poll: None,
            surface: SurfaceProjection {
                reply,
                next_cursor: None,
                index_job: None,
                compact_receipt: false,
            },
        },
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
    let text = backend_present::package_graph_markdown(page, detail, invocation);
    if text.len() > backend_present::MAX_PREVIEW_TEXT_BYTES {
        return Err(RpcError::from_fault(&oversized_fault(BudgetExceeded {
            bytes: text.len(),
            budget: backend_present::MAX_PREVIEW_TEXT_BYTES,
        })));
    }
    let structured = serde_json::from_slice(&payload.bytes)
        .map_err(|error| RpcError::tool(format!("typed surface graph decode failed: {error}")))?;
    Ok(complete_tool_result(&text, structured, false))
}
