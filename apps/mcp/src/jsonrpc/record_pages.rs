//! Native catalog delivery shrinks independently of issued query credit.
use super::*;
use wire_admission::ResponseBudget;

pub(super) fn query_window(
    issued: u16,
    delivery: u16,
) -> Result<backend_library::QueryWindow, RpcError> {
    let issued = backend_library::QueryLimit::new(issued)
        .ok_or_else(|| RpcError::invalid("invalid issued catalog credit"))?;
    let delivery = backend_library::QueryLimit::new(delivery)
        .ok_or_else(|| RpcError::invalid("invalid catalog delivery bound"))?;
    backend_library::QueryWindow::new(issued, delivery)
        .ok_or_else(|| RpcError::invalid("catalog delivery exceeds issued credit"))
}

pub(super) fn shrink_window(
    continued: bool,
    issued: &mut u16,
    delivery: &mut u16,
    arguments: &mut Map<String, Value>,
) -> bool {
    if *delivery <= 1 {
        return false;
    }
    *delivery = (*delivery).div_ceil(2);
    if !continued {
        *issued = *delivery;
        arguments.insert("limit".to_owned(), json!(*issued));
    }
    true
}

pub(super) fn pin_revision(
    pinned: &mut Option<backend_library::ViewRevision>,
    revision: backend_library::ViewRevision,
) -> Result<(), RpcError> {
    if pinned.is_some_and(|expected| expected != revision) {
        return Err(RpcError::tool(
            "catalog source changed while selecting a bounded page; restart discovery explicitly",
        ));
    }
    *pinned = Some(revision);
    Ok(())
}

impl<P: Product> Server<P> {
    /// A failed native read consumes Session's imported prepared command.
    /// Re-import the same authenticated input before each attempt; never use
    /// a successor produced by an oversized attempt or retain MCP page state.
    pub(super) fn reimport_catalog_cursor(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        invocation: McpInvocation<'_>,
        expected: Option<PageContinuation>,
    ) -> Result<(), RpcError> {
        if expected.is_some() {
            let context =
                continuation_context(&self.project, invocation.selected_name(), arguments, detail);
            if self.continuation(arguments, &context)? != expected {
                return Err(RpcError::invalid(
                    "catalog replay changed its admitted input frontier",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn records_tool(
        &mut self,
        request: &Request,
        detail: Detail,
        continuation: Option<PageContinuation>,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        let lifetime = self.semantic_lifetime()?;
        let budget = ResponseBudget::new(response_id);
        let mut arguments = invocation.arguments().clone();
        let (Request::Search { limit, .. } | Request::Resolve { limit, .. }) = request else {
            return Err(RpcError::tool("records preparation changed request shape"));
        };
        let mut issued = *limit;
        let mut delivery = issued;
        let mut pinned = None;
        let mut pinned_revision = None;
        loop {
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let selected = match request {
                Request::Search { text, .. } => Request::Search {
                    text: text.clone(),
                    limit: issued,
                },
                Request::Resolve { text, .. } => Request::Resolve {
                    text: text.clone(),
                    limit: issued,
                },
                _ => return Err(RpcError::tool("catalog request changed shape")),
            };
            let probe = match &selected {
                Request::Search { text, limit } => Probe::Search {
                    text,
                    limit: *limit,
                },
                Request::Resolve { text, limit } => Probe::Names {
                    text,
                    limit: *limit,
                },
                _ => return Err(RpcError::tool("catalog request changed shape")),
            };
            self.reimport_catalog_cursor(&arguments, detail, invocation, continuation)?;
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let reply = self.product.probe_page_window(
                probe,
                continuation,
                query_window(issued, delivery)?,
            );
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let reply = match reply {
                Err(ClientError::ResponseTooLarge { basis: Some(basis) }) => {
                    pin_revision(&mut pinned_revision, basis)?;
                    if shrink_window(
                        continuation.is_some(),
                        &mut issued,
                        &mut delivery,
                        &mut arguments,
                    ) {
                        continue;
                    }
                    return Err(RpcError::from_fault(&backend_present::probe_fault(
                        &ClientError::ResponseTooLarge { basis: Some(basis) },
                        probe,
                    )));
                }
                Err(error) => {
                    return Err(RpcError::from_fault(&backend_present::probe_fault(
                        &error, probe,
                    )));
                }
                Ok(reply) => reply,
            };
            let answer = backend_present::catalog_page_answer(&selected, reply)
                .map_err(|fault| RpcError::from_fault(&fault))?;
            let Answer::Records(records) = &answer else {
                return Err(RpcError::tool("records page changed shape"));
            };
            let basis = records
                .page_basis()
                .ok_or_else(|| RpcError::tool("records page omits native scope"))?;
            pin_revision(&mut pinned_revision, basis.0.root.into())?;
            if pinned.is_some_and(|expected| expected != basis) {
                return Err(RpcError::tool(
                    "records source changed while selecting a bounded page; restart discovery explicitly",
                ));
            }
            pinned = Some(basis);
            let context = continuation_context(
                &self.project,
                invocation.selected_name(),
                &arguments,
                detail,
            );
            let result = self
                .rendered_admitted(
                    answer,
                    detail,
                    &context,
                    CursorTarget::CliOption,
                    invocation.with_arguments(&arguments),
                )
                .and_then(|result| budget.admit(result));
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            match result {
                Ok(result) => return Ok(result),
                Err(error)
                    if semantic_pages::oversized(&error)
                        && shrink_window(
                            continuation.is_some(),
                            &mut issued,
                            &mut delivery,
                            &mut arguments,
                        ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}
