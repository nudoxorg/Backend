//! Reader invocation identity, independent of the selected command's authority.

use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::{Map, Value, json};

/// The compact registered-tool entry point exposed to tool-list-only hosts.
pub const MCP_REGISTERED_CALL_TOOL: &str = "backend.call";

/// How the reader can execute a named call on this MCP host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpExposure {
    /// The selected named tool is directly exposed.
    Direct,
    /// The registered named tool is reached through the compact entry point.
    RegisteredCall,
}

impl McpExposure {
    /// Serializes one already-typed named call for the exposed entry point.
    #[must_use]
    pub fn expose_call(self, call: Value) -> Value {
        match self {
            Self::Direct => call,
            Self::RegisteredCall => json!({"name": MCP_REGISTERED_CALL_TOOL, "arguments": call}),
        }
    }

    pub(crate) fn advice(self, advice: crate::Affordance) -> crate::Affordance {
        match (self, advice) {
            (Self::Direct, advice)
            | (
                _,
                advice @ (crate::Affordance::None
                | crate::Affordance::Retry
                | crate::Affordance::RecoverOwner { .. }
                | crate::Affordance::RecoverStartup { .. }
                | crate::Affordance::ReadPackageGraphJson { .. }
                | crate::Affordance::ViaRegisteredCall(_)),
            ) => advice,
            (Self::RegisteredCall, advice) => {
                crate::Affordance::ViaRegisteredCall(Box::new(advice))
            }
        }
    }
}

/// The original selected tool and arguments, plus the reader's exposed route.
/// This is presentation context; it never changes dispatch or cursor authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpInvocation<'a> {
    exposure: McpExposure,
    selected_name: &'a str,
    arguments: &'a Map<String, Value>,
    adapted_arguments: bool,
}

impl<'a> McpInvocation<'a> {
    /// Records a reader invocation after registered-route admission.
    #[must_use]
    pub const fn new(
        exposure: McpExposure,
        selected_name: &'a str,
        arguments: &'a Map<String, Value>,
    ) -> Self {
        Self {
            exposure,
            selected_name,
            arguments,
            adapted_arguments: false,
        }
    }

    /// The exposed invocation mode, independent of the selected command.
    #[must_use]
    pub const fn exposure(self) -> McpExposure {
        self.exposure
    }

    /// The canonical selected command, independent of the exposed route.
    #[must_use]
    pub const fn selected_name(self) -> &'a str {
        self.selected_name
    }

    /// Borrows admitted caller operands for native page preparation.
    #[must_use]
    pub const fn arguments(self) -> &'a Map<String, Value> {
        self.arguments
    }

    /// Copyable reader guidance for the measured continuation call.
    #[must_use]
    pub const fn continuation_note(self) -> &'static str {
        match self.exposure {
            McpExposure::RegisteredCall => {
                "Continue `backend.call` by copying `structuredContent.continuation_call` as the next tool call; its selected name and original arguments are preserved, with the exact returned next-page operands."
            }
            McpExposure::Direct => {
                "Continue by copying `structuredContent.continuation_call` as the next tool call; it preserves the selected tool and original arguments with the exact returned next-page operands."
            }
        }
    }

    /// Retains exposure and selected identity with an admitted effective page shape.
    #[must_use]
    pub fn with_arguments<'b>(self, arguments: &'b Map<String, Value>) -> McpInvocation<'b>
    where
        'a: 'b,
    {
        McpInvocation {
            exposure: self.exposure,
            selected_name: self.selected_name,
            arguments,
            adapted_arguments: self.adapted_arguments || self.arguments != arguments,
        }
    }

    /// Changed effective operands require copying the prepared next call.
    #[must_use]
    pub const fn adapted_arguments(self) -> bool {
        self.adapted_arguments
    }

    /// The exact executable original invocation, including durable keys/detail.
    #[must_use]
    pub const fn executable_call(self) -> McpExecutableCall<'a> {
        McpExecutableCall {
            invocation: self,
            cursor: None,
        }
    }

    /// The exact next page call; all original non-cursor inputs are preserved.
    #[must_use]
    pub fn continuation_call<'b>(self, cursor: &'b str) -> McpExecutableCall<'b>
    where
        'a: 'b,
    {
        McpExecutableCall {
            invocation: self,
            cursor: Some(McpCursor::Opaque(cursor)),
        }
    }
    /// Continues an admitted native source request without copying or changing
    /// its portable owner guard. Only the requested byte-range offset changes.
    #[must_use]
    pub fn native_source_continuation_call(self, offset: u32) -> McpExecutableCall<'a> {
        let max_bytes = self
            .arguments
            .get("request")
            .and_then(|request| request.get("range"))
            .and_then(|range| range.get("max_bytes"))
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(backend_library::MAX_PRODUCT_TEXT_BYTES as u16);
        McpExecutableCall {
            invocation: self,
            cursor: Some(McpCursor::SemanticSourceRange { offset, max_bytes }),
        }
    }

    /// Retains an exact typed native graph cursor without a surface wrapper.
    #[must_use]
    pub fn native_graph_continuation_call<'b>(
        self,
        cursor: &'b backend_library::PackageGraphCursor,
    ) -> McpExecutableCall<'b>
    where
        'a: 'b,
    {
        McpExecutableCall {
            invocation: self,
            cursor: Some(McpCursor::PackageGraph(cursor)),
        }
    }
    /// Preserves the registered operator call while deriving the exact nested
    /// typed native request from its issued cursor and effective page size.
    #[must_use]
    pub fn surface_graph_continuation_call<'b>(
        self,
        request: &'b backend_library::PackageGraphPageRequest,
        cursor: &'b backend_library::PackageGraphCursor,
    ) -> McpExecutableCall<'b>
    where
        'a: 'b,
    {
        McpExecutableCall {
            invocation: self,
            cursor: Some(McpCursor::SurfaceGraph { request, cursor }),
        }
    }

    /// Raw semantic operators retain their exact nested typed command and
    /// registered exposure. No top-level cursor is invented for this grammar.
    #[must_use]
    pub fn surface_semantic_continuation_call<'b>(
        self,
        command: &'b backend_library::SurfaceCommand,
    ) -> McpExecutableCall<'b>
    where
        'a: 'b,
    {
        McpExecutableCall {
            invocation: self,
            cursor: Some(McpCursor::SurfaceSemantic(command)),
        }
    }
}

/// Borrowed exact call serialization; continuation overrides one cursor
/// without cloning original query, variables or operand collections.
#[derive(Clone, Copy, Debug)]
pub struct McpExecutableCall<'a> {
    invocation: McpInvocation<'a>,
    cursor: Option<McpCursor<'a>>,
}

impl Serialize for McpExecutableCall<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        match self.invocation.exposure {
            McpExposure::Direct => {
                map.serialize_entry("name", self.invocation.selected_name)?;
                map.serialize_entry(
                    "arguments",
                    &InvocationArguments {
                        arguments: self.invocation.arguments,
                        cursor: self.cursor,
                    },
                )?;
            }
            McpExposure::RegisteredCall => {
                map.serialize_entry("name", MCP_REGISTERED_CALL_TOOL)?;
                map.serialize_entry(
                    "arguments",
                    &Self {
                        invocation: McpInvocation {
                            exposure: McpExposure::Direct,
                            ..self.invocation
                        },
                        cursor: self.cursor,
                    },
                )?;
            }
        }
        map.end()
    }
}

#[derive(Clone, Copy, Debug)]
enum McpCursor<'a> {
    Opaque(&'a str),
    PackageGraph(&'a backend_library::PackageGraphCursor),
    SemanticSourceRange {
        offset: u32,
        max_bytes: u16,
    },
    SurfaceGraph {
        request: &'a backend_library::PackageGraphPageRequest,
        cursor: &'a backend_library::PackageGraphCursor,
    },
    SurfaceSemantic(&'a backend_library::SurfaceCommand),
}

impl Serialize for McpCursor<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Opaque(cursor) => cursor.serialize(serializer),
            Self::PackageGraph(cursor) => cursor.serialize(serializer),
            Self::SemanticSourceRange { offset, max_bytes } => {
                backend_library::SemanticSourceRange {
                    offset: *offset,
                    max_bytes: *max_bytes,
                }
                .serialize(serializer)
            }
            Self::SurfaceGraph { request, cursor } => BorrowedGraphCommand::PackageGraphPage {
                request: BorrowedGraphRequest::new(request, cursor),
            }
            .serialize(serializer),
            Self::SurfaceSemantic(command) => command.serialize(serializer),
        }
    }
}

// This closed typed projection mirrors the existing native command variant;
// it cannot carry code, HTTP, unknown commands, or unregistered dispatch.
#[derive(Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub(crate) enum BorrowedGraphCommand<T> {
    PackageGraphPage { request: T },
}
#[derive(Serialize)]
struct BorrowedGraphRequest<'a> {
    package: &'a backend_library::PackageReference,
    direction: backend_library::PackageGraphDirection,
    authority: Option<backend_library::PackageGraphSourceAuthority>,
    catalog_snapshot: Option<[u8; 32]>,
    limit: u16,
    cursor: &'a backend_library::PackageGraphCursor,
    control: backend_library::PackageGraphControl,
}
impl<'a> BorrowedGraphRequest<'a> {
    fn new(
        request: &'a backend_library::PackageGraphPageRequest,
        cursor: &'a backend_library::PackageGraphCursor,
    ) -> Self {
        Self {
            package: &request.package,
            direction: request.direction,
            authority: request.authority,
            // Exactly PackageGraphPageRequest::with_cursor semantics: the new
            // owner cursor supplies its selected catalog guard on first resume.
            catalog_snapshot: cursor.catalog_snapshot,
            limit: request.limit,
            cursor,
            control: request.control,
        }
    }
}

struct InvocationArguments<'a> {
    arguments: &'a Map<String, Value>,
    cursor: Option<McpCursor<'a>>,
}

impl Serialize for InvocationArguments<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(cursor @ (McpCursor::SurfaceGraph { .. } | McpCursor::SurfaceSemantic(_))) =
            self.cursor
        {
            let extra = usize::from(!self.arguments.contains_key("command"));
            let mut map = serializer.serialize_map(Some(self.arguments.len() + extra))?;
            for (name, value) in self.arguments {
                if name != "command" {
                    map.serialize_entry(name, value)?;
                }
            }
            map.serialize_entry("command", &cursor)?;
            return map.end();
        }
        let source_range = matches!(self.cursor, Some(McpCursor::SemanticSourceRange { .. }));
        let key = if source_range { "request" } else { "cursor" };
        let extra = usize::from(self.cursor.is_some() && !self.arguments.contains_key(key));
        let mut map = serializer.serialize_map(Some(self.arguments.len() + extra))?;
        for (name, value) in self.arguments {
            if name != key || self.cursor.is_none() {
                map.serialize_entry(name, value)?;
            }
        }
        if let Some(cursor) = self.cursor {
            match cursor {
                McpCursor::SemanticSourceRange { offset, max_bytes } => {
                    let request = self
                        .arguments
                        .get("request")
                        .and_then(Value::as_object)
                        .ok_or_else(|| {
                            serde::ser::Error::custom("admitted source request must be an object")
                        })?;
                    map.serialize_entry(
                        "request",
                        &SourceRangeArguments {
                            request,
                            range: backend_library::SemanticSourceRange { offset, max_bytes },
                        },
                    )?;
                }
                cursor => map.serialize_entry("cursor", &cursor)?,
            }
        }
        map.end()
    }
}

struct SourceRangeArguments<'a> {
    request: &'a Map<String, Value>,
    range: backend_library::SemanticSourceRange,
}
impl Serialize for SourceRangeArguments<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let extra = usize::from(!self.request.contains_key("range"));
        let mut map = serializer.serialize_map(Some(self.request.len() + extra))?;
        for (name, value) in self.request {
            if name != "range" {
                map.serialize_entry(name, value)?;
            }
        }
        map.serialize_entry("range", &self.range)?;
        map.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_surface_graph_call_matches_native_with_cursor_catalog_binding() {
        use backend_library::{
            PackageGraphCursor, PackageGraphDirection, PackageGraphPageRequest,
            PackageGraphSourceAuthority, PackageGraphSourceKey, PackageReference, SurfaceCommand,
        };
        let package = PackageReference::parse("pkg:cargo/demo@1").expect("package");
        let authority = PackageGraphSourceAuthority::Local([7; 32]);
        let request = PackageGraphPageRequest::new(
            package.clone(),
            PackageGraphDirection::Dependencies,
            Some(authority),
            4,
        )
        .expect("request");
        let source = PackageGraphSourceKey::new(package, authority);
        let bound = request
            .clone()
            .bind_catalog_snapshot([3; 32])
            .expect("owner catalog");
        let cursor = PackageGraphCursor {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root: [9; 32],
            facts_witness: [10; 32],
            recipe: bound.recipe(Some(&source)),
            catalog_snapshot: Some([3; 32]),
            source: Some(source),
            limit: 4,
            after_edge_id: [11; 32],
            seal: [12; 32],
        };
        let arguments = json!({"command":SurfaceCommand::PackageGraphPage { request:request.clone() },"detail":"full"});
        let invocation = McpInvocation::new(
            McpExposure::Direct,
            "backend.surface",
            arguments.as_object().expect("args"),
        );
        let call =
            serde_json::to_value(invocation.surface_graph_continuation_call(&request, &cursor))
                .expect("borrowed canonical call");
        assert_eq!(
            call,
            json!({"name":"backend.surface","arguments":{"command":SurfaceCommand::PackageGraphPage { request:request.with_cursor(cursor) },"detail":"full"}})
        );
        let typed: SurfaceCommand = serde_json::from_value(call["arguments"]["command"].clone())
            .expect("exact closed grammar");
        typed
            .admit()
            .expect("catalog guard is attached exactly like with_cursor");
        assert!(call["arguments"].get("cursor").is_none());
    }

    #[test]
    fn borrowed_call_preserves_all_arguments_and_overrides_only_cursor() {
        let value = json!({"query":"{ Declaration { coordinate @output } }", "variables":{"names":["raw λ", "stdin"], "count":3}, "limit":1, "detail":"full", "cursor":"old"});
        let arguments = value.as_object().expect("arguments");
        let invocation =
            McpInvocation::new(McpExposure::RegisteredCall, "backend.query", arguments);
        let original =
            json!({"name":"backend.call","arguments":{"name":"backend.query","arguments":value}});
        assert_eq!(
            serde_json::to_value(invocation.executable_call()).expect("call"),
            original
        );
        let mut expected = value.clone();
        expected["cursor"] = json!("next");
        assert_eq!(
            serde_json::to_value(invocation.continuation_call("next")).expect("call"),
            json!({"name":"backend.call","arguments":{"name":"backend.query","arguments":expected}})
        );
        assert_eq!(arguments["cursor"], "old");
        let direct = McpInvocation::new(McpExposure::Direct, "backend.query", arguments);
        assert_eq!(
            serde_json::to_value(direct.executable_call()).expect("direct"),
            original["arguments"]
        );
    }

    #[test]
    fn source_next_call_preserves_the_exact_guard_detail_and_byte_bound() {
        for explicit_range in [false, true] {
            let mut value =
                json!({"request":{"reference":{"seal":[1,2,3],"frozen":"λ"}},"detail":"full"});
            if explicit_range {
                value["request"]["range"] = json!({"offset":17,"max_bytes":211});
            }
            let original = value.clone();
            let arguments = value.as_object().expect("arguments");
            let mut expected = value.clone();
            expected["request"]["range"] = json!({"offset":228,"max_bytes":if explicit_range {211} else {backend_library::MAX_PRODUCT_TEXT_BYTES}});
            for exposure in [McpExposure::Direct, McpExposure::RegisteredCall] {
                let invocation = McpInvocation::new(exposure, "backend.semantic_source", arguments);
                assert_eq!(
                    serde_json::to_value(invocation.native_source_continuation_call(228))
                        .expect("exact call"),
                    exposure.expose_call(
                        json!({"name":"backend.semantic_source","arguments":expected})
                    )
                );
            }
            assert_eq!(
                value, original,
                "advice serialization borrows the immutable request"
            );
        }
    }

    #[test]
    fn typed_advice_wraps_once_and_retains_cli_action() {
        let advice = crate::Affordance::UseCommand {
            name: "package",
            args: Box::new(["pkg:npm/react@19.1.0".to_owned()]),
        };
        let direct = advice.tool_call().expect("direct advice");
        let shell = advice.shell();
        let compact = McpExposure::RegisteredCall.advice(advice);
        assert_eq!(compact.shell(), shell);
        assert_eq!(
            compact.tool_call(),
            Some(json!({"name":"backend.call","arguments":direct}))
        );
        let repeated = McpExposure::RegisteredCall.advice(compact.clone());
        assert_eq!(repeated, compact);
    }
}
