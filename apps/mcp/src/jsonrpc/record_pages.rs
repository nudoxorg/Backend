//! Bounded native re-reads preserve rows, producer scope and issued limits.
use super::*;
use wire_admission::ResponseBudget;

impl<P: Product> Server<P> {
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
        let mut selected = request.clone();
        let mut pinned = None;
        let mut arguments = invocation.arguments().clone();
        loop {
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let (Request::Search { limit, .. } | Request::Resolve { limit, .. }) = &selected else {
                return Err(RpcError::tool("records preparation changed request shape"));
            };
            let effective = *limit;
            let answer = answer_paged(&mut self.product, &selected, continuation)
                .map_err(|fault| RpcError::from_fault(&fault))?;
            let Answer::Records(records) = &answer else {
                return Err(RpcError::tool("records page changed shape"));
            };
            let basis = records
                .page_basis()
                .ok_or_else(|| RpcError::tool("records page omits native scope"))?;
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
                    if continuation.is_none()
                        && effective > 1
                        && semantic_pages::oversized(&error) =>
                {
                    let next = effective.div_ceil(2);
                    match &mut selected {
                        Request::Search { limit, .. } | Request::Resolve { limit, .. } => {
                            *limit = next
                        }
                        _ => unreachable!(),
                    }
                    arguments.insert("limit".to_owned(), json!(next));
                }
                Err(error) => return Err(error),
            }
        }
    }
}
