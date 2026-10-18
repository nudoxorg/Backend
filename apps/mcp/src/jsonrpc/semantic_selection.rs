//! Selection acknowledges a mutation; the native history catalog is a separate read.
use super::*;
use backend_library::{PackageCoordinate, SemanticGenerationId, SemanticLanguageProfile};

/// An acknowledgement of the exact selected lane, not a partial version record.
/// The owner's complete SemanticVersionRecord remains unchanged for native/CLI callers.
#[derive(Serialize)]
struct SelectionReceipt<'a> {
    schema: &'static str,
    package: &'a PackageReference,
    coordinate: &'a PackageCoordinate,
    profile: SemanticLanguageProfile,
    generation: SemanticGenerationId,
    selected: bool,
}

#[derive(Serialize)]
struct SelectionBody<'a> {
    heading: &'static str,
    selection_receipt: SelectionReceipt<'a>,
    metadata_call: McpExecutableCall<'a>,
    metadata_read: &'static str,
}

impl<P: Product> Server<P> {
    pub(super) fn select_semantic_version_tool(
        &mut self,
        command: &SurfaceCommand,
        detail: Detail,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        let SurfaceCommand::SelectSemanticVersion {
            package,
            coordinate,
            profile,
            generation,
        } = command
        else {
            return Err(RpcError::invalid(
                "semantic selection command changed shape",
            ));
        };
        command
            .admit()
            .map_err(|error| RpcError::invalid(error.to_string()))?;
        // Expert commands can carry an explicit Local/Purl kind that a string
        // grammar cannot reconstruct; preserve their typed package namespace.
        let (metadata_tool, metadata_arguments) = if invocation.selected_name() == SURFACE_TOOL {
            (
                SURFACE_TOOL,
                json!({"command":SurfaceCommand::SemanticVersions {
                    package:package.clone(), limit:1, cursor:None,
                }, "detail":"full"}),
            )
        } else {
            (
                "backend.semantic_versions",
                json!({"package":package.as_str(),"limit":1,"detail":"full"}),
            )
        };
        let metadata_arguments = metadata_arguments
            .as_object()
            .ok_or_else(|| RpcError::tool("semantic metadata read arguments changed shape"))?;
        let payload = encode_serializable(
            "product",
            detail,
            None,
            SelectionBody {
                heading: "select-semantic-version",
                selection_receipt: SelectionReceipt {
                    schema: "backend.semantic-selection-receipt.v1",
                    package,
                    coordinate,
                    profile: *profile,
                    generation: *generation,
                    selected: true,
                },
                metadata_call: McpInvocation::new(
                    invocation.exposure(), metadata_tool, metadata_arguments,
                ).executable_call(),
                metadata_read: "This is a selection acknowledgement, not a semantic version record. Read metadata_call and follow descriptor pages to this exact coordinate/profile/generation. Each descriptor reports source availability; when available, its semantic-source reference and byte ranges retrieve the complete record and history catalog.",
            },
            DEFAULT_RESPONSE_BUDGET_BYTES,
        ).map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
        let structured = serde_json::from_slice(&payload.bytes)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        // All success bytes depend only on admitted request identities. Admit the
        // actual escaped caller ID, metadata, tokens and stdio newline BEFORE
        // mutation. A committed selection never becomes a transport overflow
        // merely because its native history proof is large. No owner re-read.
        let result = wire_admission::ResponseBudget::new(response_id).admit(
            complete_tool_result(
                "Selection confirmed. This receipt identifies the selected lane; copy structuredContent.metadata_call to read complete metadata through native pages and semantic-source ranges.",
                structured,
                false,
            ),
        )?;
        let reply = self.product.surface(command.clone()).map_err(|error| {
            RpcError::from_fault(&Fault::from_client_error(
                &error,
                surface_error_operand(command),
            ))
        })?;
        backend_library::admit_surface_reply(command, &reply)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        let SurfaceReply::SemanticVersionSelected(record) = reply else {
            return Err(RpcError::tool("semantic selection reply changed shape"));
        };
        if record.package != *package
            || record.coordinate != *coordinate
            || record.profile != *profile
            || record.generation != *generation
            || !record.selected
        {
            return Err(RpcError::tool(
                "semantic selection reply does not confirm the requested lane",
            ));
        }
        Ok(result)
    }
}
