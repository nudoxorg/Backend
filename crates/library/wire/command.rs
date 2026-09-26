use super::{
    DTO_VERSION, ReplyEnvelope, ViewEnvelopeWire, WireCertificate, WireSchema, freshness_from_wire,
    reply_from_wire, reply_from_wire_with_verifier, view_root_from_wire,
};
use crate::canonical::{PackageSchema, SymbolSchema, encode_id};
use crate::{
    Command, CommandReply, CompleteViewProjection, CoverageCapability, Cursor, ViewSnapshot,
    ViewStateRoot,
};
use backend_version::ProducerObservationVerifier;
use serde::{Deserialize, Serialize};

#[path = "command/cursor_wire.rs"]
mod cursor_wire;
#[path = "command/graph_query_wire.rs"]
mod graph_query_wire;
#[path = "command/page_wire.rs"]
mod page_wire;
#[path = "command/read_manifest_wire.rs"]
mod read_manifest_wire;
#[path = "command/query_wire.rs"]
mod query_wire;
pub(crate) use query_wire::SymbolAddressWire;
use query_wire::{
    DocumentQueryWire, GraphNeighborhoodQueryWire, NameQueryWire, OutlineQueryWire, QueryWire,
    document_query_from_wire, document_query_to_wire, graph_query_from_wire, graph_query_to_wire,
    name_query_from_wire, name_query_to_wire, outline_query_from_wire, outline_query_to_wire,
    query_from_wire, query_to_wire, symbol_address_from_wire, symbol_address_to_wire,
};
pub(crate) use cursor_wire::{
    cursor_from_wire, cursor_from_wire_with_capability, cursor_to_wire, frontier_from_wire,
    frontier_from_wire_with_capability, frontier_to_wire,
};
use graph_query_wire::{
    GraphQueryRequestWire, request_from_wire as graph_request_from_wire,
    request_from_wire_against_owner as graph_request_from_wire_against_owner,
    request_to_wire as graph_request_to_wire,
};
pub(crate) use graph_query_wire::{GraphValueWire, value_from_wire, value_to_wire};
use page_wire::{
    PackagePageWire, PageRequestWire, SymbolPageWire, page_request_from_wire, page_request_to_wire,
};
use read_manifest_wire::{ReadManifestWire, read_manifest_from_wire, read_manifest_to_wire};

/// Transport-neutral command DTO with explicit operation and protocol identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDto {
    /// Correlates the request with a reply.
    pub request_id: u64,
    /// Typed command lowered after transport validation.
    pub command: Command,
    /// Optional producer certificate carrying every logical identity
    /// preimage needed by a standalone receiver.
    certificate: Option<WireCertificate>,
}

impl CommandDto {
    /// Creates a command DTO at the current protocol version.
    #[must_use]
    pub const fn new(request_id: u64, command: Command) -> Self {
        Self {
            request_id,
            command,
            certificate: None,
        }
    }

    /// Attaches a producer certificate to this command envelope.
    #[must_use]
    pub fn with_certificate(mut self, certificate: WireCertificate) -> Self {
        self.certificate = Some(certificate);
        self
    }

    /// Returns the producer certificate, when one was attached.
    #[must_use]
    pub const fn certificate(&self) -> Option<&WireCertificate> {
        self.certificate.as_ref()
    }

    /// Returns the encoded DTO version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        DTO_VERSION
    }

    /// Decodes a wire command by comparing every claim with the caller's
    /// already accepted command.
    ///
    /// Wire IDs are deliberately not reconstructed from their fixed-width
    /// digests. The caller owns the expected typed value and this method
    /// returns that value only after strict envelope decoding and an exact
    /// wire comparison. Use this boundary when a transport has no canonical
    /// preimage to send alongside the claim.
    ///
    /// # Errors
    ///
    /// Returns an error when the envelope is malformed, has an unsupported
    /// version or unknown field, or differs from `expected` in any field.
    pub fn decode_against(bytes: &[u8], expected: &Self) -> Result<Self, String> {
        let envelope: CommandEnvelope =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "command")?;
        let observed = serde_json::to_value(&envelope).map_err(|error| error.to_string())?;
        let accepted = serde_json::to_value(expected).map_err(|error| error.to_string())?;
        if observed != accepted {
            return Err("wire command claims do not match the caller-owned command".to_owned());
        }
        Ok(expected.clone())
    }

    /// Decodes a command using the durable owner's already admitted cursor
    /// for graph-query continuation roots.
    ///
    /// All ordinary commands retain the strict canonical decoder. A graph
    /// continuation may carry a constant-size root commitment because the
    /// owner supplies and later compares the materialized root itself.
    ///
    /// # Errors
    /// Returns an error when the envelope, certificate, or continuation does
    /// not match the supplied owner cursor.
    pub fn decode_for_owner(bytes: &[u8], owner: Cursor) -> Result<Self, String> {
        let envelope: CommandEnvelope =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "command")?;
        let certificate = envelope.certificate.as_ref();
        let command = match envelope.command {
            CommandWire::GraphQuery(value) => {
                Command::GraphQuery(graph_request_from_wire_against_owner(
                    value,
                    required_certificate(certificate)?,
                    owner,
                )?)
            }
            command => command_from_wire(command, certificate)?,
        };
        Ok(Self {
            request_id: envelope.request_id,
            command,
            certificate: envelope.certificate,
        })
    }
}

/// Transport-neutral reply DTO retaining request correlation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplyDto {
    /// Request correlation identity.
    pub request_id: u64,
    /// Typed command result or transport-visible error.
    pub reply: CommandReply,
    /// Optional producer certificate carrying every logical identity
    /// preimage needed by a standalone receiver.
    certificate: Option<WireCertificate>,
    /// Exact owner cursor paired with a health root, when supplied by the
    /// producer. Clients must resume from this cursor rather than deriving
    /// stream progress from the visible view frontier.
    health_cursor: Option<Cursor>,
}

impl ReplyDto {
    /// Creates a successful reply DTO.
    #[must_use]
    pub const fn new(request_id: u64, reply: CommandReply) -> Self {
        Self {
            request_id,
            reply,
            certificate: None,
            health_cursor: None,
        }
    }

    /// Creates a health reply with the producer's exact owner cursor.
    ///
    /// The cursor remains beside the immutable root so process clients can
    /// resume after intent-only events that do not change the visible view.
    #[must_use]
    pub const fn health(request_id: u64, root: crate::ViewRoot, cursor: Cursor) -> Self {
        Self {
            request_id,
            reply: CommandReply::Health(root),
            certificate: None,
            health_cursor: Some(cursor),
        }
    }

    /// Attaches a producer certificate to this reply envelope.
    #[must_use]
    pub fn with_certificate(mut self, certificate: WireCertificate) -> Self {
        self.certificate = Some(certificate);
        self
    }

    /// Returns the producer certificate, when one was attached.
    #[must_use]
    pub const fn certificate(&self) -> Option<&WireCertificate> {
        self.certificate.as_ref()
    }

    /// Returns the producer cursor paired with a health root.
    #[must_use]
    pub const fn health_cursor(&self) -> Option<Cursor> {
        self.health_cursor
    }

    /// Attaches the exact owner cursor to a health reply assembled by a
    /// compatibility caller.
    #[must_use]
    pub const fn with_health_cursor(mut self, cursor: Cursor) -> Self {
        self.health_cursor = Some(cursor);
        self
    }

    /// Creates a typed error reply without flattening successful variants.
    #[must_use]
    pub fn error(request_id: u64, message: impl Into<String>) -> Self {
        Self {
            request_id,
            reply: CommandReply::Error(message.into()),
            certificate: None,
            health_cursor: None,
        }
    }

    /// Returns the encoded DTO version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        DTO_VERSION
    }

    /// Decodes a wire reply by comparing it with a caller-owned accepted
    /// reply. A digest-only wire identity never becomes trusted by itself.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, unsupported, unknown-field, or changed
    /// wire payloads.
    pub fn decode_against(bytes: &[u8], expected: &Self) -> Result<Self, String> {
        let envelope: ReplyEnvelope =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "reply")?;
        let observed = serde_json::to_value(&envelope).map_err(|error| error.to_string())?;
        let accepted = serde_json::to_value(expected).map_err(|error| error.to_string())?;
        if observed != accepted {
            return Err("wire reply claims do not match the caller-owned reply".to_owned());
        }
        Ok(expected.clone())
    }

    /// Decodes a reply using a producer certificate and optional source
    /// coverage capability. Complete snapshots require the capability in
    /// addition to their canonical identity preimages.
    ///
    /// # Errors
    ///
    /// Returns an error when any claim, canonical value, transition, or
    /// complete-coverage witness is invalid.
    pub fn decode_with_certificate(
        bytes: &[u8],
        capability: Option<CoverageCapability>,
    ) -> Result<Self, String> {
        let envelope: ReplyEnvelope =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "reply")?;
        let certificate = required_certificate(envelope.certificate.as_ref())?;
        let reply = reply_from_wire(envelope.reply, certificate, capability)?;
        let health_cursor = envelope
            .health_cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?;
        if health_cursor.is_some() && !matches!(&reply, CommandReply::Health(_)) {
            return Err("health cursor attached to a non-health reply".to_owned());
        }
        if matches!(&reply, CommandReply::Health(_)) && health_cursor.is_none() {
            return Err("health reply omitted its exact owner cursor".to_owned());
        }
        if let Some(cursor) = health_cursor {
            certificate.cursor_claim(cursor)?;
        }
        if let CommandReply::Health(root) = &reply {
            let cursor = health_cursor
                .ok_or_else(|| "health reply omitted its exact owner cursor".to_owned())?;
            CompleteViewProjection::admit(root.clone(), cursor)
                .map_err(|error| format!("health reply is not a complete projection: {error:?}"))?;
        }
        Ok(Self {
            request_id: envelope.request_id,
            reply,
            certificate: envelope.certificate,
            health_cursor,
        })
    }

    /// Decodes a reply using a verifier owned by an authenticated process
    /// channel. The verifier admits the certificate's complete scope,
    /// producer, context, and evidence tuple; the wire bytes never construct
    /// a [`CoverageCapability`] directly.
    ///
    /// # Errors
    ///
    /// Returns an error when the certificate, canonical identities, complete
    /// coverage observation, cursor, or view projection is invalid.
    pub fn decode_with_verifier<V: ProducerObservationVerifier>(
        bytes: &[u8],
        verifier: &V,
    ) -> Result<Self, String> {
        let envelope: ReplyEnvelope =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "reply")?;
        let certificate = required_certificate(envelope.certificate.as_ref())?;
        let reply = reply_from_wire_with_verifier(envelope.reply, certificate, verifier)?;
        let health_cursor = envelope
            .health_cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?;
        if health_cursor.is_some() && !matches!(&reply, CommandReply::Health(_)) {
            return Err("health cursor attached to a non-health reply".to_owned());
        }
        if matches!(&reply, CommandReply::Health(_)) && health_cursor.is_none() {
            return Err("health reply omitted its exact owner cursor".to_owned());
        }
        if let Some(cursor) = health_cursor {
            certificate.cursor_claim(cursor)?;
        }
        if let CommandReply::Health(root) = &reply {
            let cursor = health_cursor
                .ok_or_else(|| "health reply omitted its exact owner cursor".to_owned())?;
            CompleteViewProjection::admit(root.clone(), cursor)
                .map_err(|error| format!("health reply is not a complete projection: {error:?}"))?;
        }
        Ok(Self {
            request_id: envelope.request_id,
            reply,
            certificate: envelope.certificate,
            health_cursor,
        })
    }

    /// Revalidates a successful health reply and returns its complete
    /// proof-bearing root/cursor projection.
    ///
    /// Health is the process-coherence claim used by standalone clients to
    /// pin subsequent requests.  A typed root held by an in-process caller is
    /// not enough at this boundary: the reply is serialized and decoded again
    /// through its producer certificate, then admitted as a complete root at
    /// the producer's exact owner cursor. This keeps certificate admission
    /// and root/cursor pairing in one library-owned operation for CLI and MCP.
    ///
    /// # Errors
    ///
    /// Returns an error when the reply is not a health reply, lacks a valid
    /// producer certificate, does not carry complete coverage, or is based on
    /// another source relation.
    pub fn admit_complete_view_projection(
        &self,
        expected_basis: Option<ViewStateRoot>,
    ) -> Result<CompleteViewProjection, String> {
        self.admit_complete_view_projection_with_capability(expected_basis, None)
    }

    /// Revalidates a health reply using an externally admitted coverage
    /// capability supplied by the authenticated owner/channel.
    ///
    /// # Errors
    ///
    /// Returns an error when the reply certificate, capability, cursor, or
    /// complete view projection is invalid.
    pub fn admit_complete_view_projection_with_capability(
        &self,
        expected_basis: Option<ViewStateRoot>,
        capability: Option<CoverageCapability>,
    ) -> Result<CompleteViewProjection, String> {
        let capability = capability.or_else(|| match &self.reply {
            CommandReply::Health(root) => root.capability(),
            _ => None,
        });
        let bytes = serde_json::to_vec(self).map_err(|error| error.to_string())?;
        let admitted = Self::decode_with_certificate(&bytes, capability)?;
        let cursor = admitted
            .health_cursor
            .ok_or_else(|| "health reply omitted its exact owner cursor".to_owned())?;
        let CommandReply::Health(root) = admitted.reply else {
            return Err("reply is not a health view".to_owned());
        };
        let result = match expected_basis {
            Some(expected) => CompleteViewProjection::admit_against(root, cursor, expected),
            None => CompleteViewProjection::admit(root, cursor),
        };
        result.map_err(|error| format!("health view projection rejected: {error:?}"))
    }
}

/// Transport-neutral view payload retaining request correlation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewDto {
    /// Request correlation identity.
    pub request_id: u64,
    /// Coherent immutable view snapshot.
    pub snapshot: ViewSnapshot,
    /// Optional producer certificate carrying every logical identity
    /// preimage needed by a standalone receiver.
    certificate: Option<WireCertificate>,
}

impl ViewDto {
    /// Creates a view DTO at the current protocol version.
    #[must_use]
    pub const fn new(request_id: u64, snapshot: ViewSnapshot) -> Self {
        Self {
            request_id,
            snapshot,
            certificate: None,
        }
    }

    /// Attaches a producer certificate to this view envelope.
    #[must_use]
    pub fn with_certificate(mut self, certificate: WireCertificate) -> Self {
        self.certificate = Some(certificate);
        self
    }

    /// Returns the producer certificate, when one was attached.
    #[must_use]
    pub const fn certificate(&self) -> Option<&WireCertificate> {
        self.certificate.as_ref()
    }

    /// Returns the encoded DTO version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        DTO_VERSION
    }

    /// Decodes a wire view by comparing it with a caller-owned accepted
    /// snapshot. Root, recipe, version, basis, row, and cursor claims are
    /// therefore admitted by the expected typed snapshot rather than by a
    /// digest-only constructor.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, unsupported, unknown-field, or changed
    /// wire payloads.
    pub fn decode_against(bytes: &[u8], expected: &Self) -> Result<Self, String> {
        let envelope: ViewEnvelopeWire =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "view")?;
        let observed = serde_json::to_value(&envelope).map_err(|error| error.to_string())?;
        let accepted = serde_json::to_value(expected).map_err(|error| error.to_string())?;
        if observed != accepted {
            return Err("wire view claims do not match the caller-owned view".to_owned());
        }
        Ok(expected.clone())
    }

    /// Decodes a view using a producer certificate and optional source
    /// coverage capability.
    ///
    /// # Errors
    ///
    /// Returns an error when canonical identities or complete coverage cannot
    /// be admitted.
    pub fn decode_with_certificate(
        bytes: &[u8],
        capability: Option<CoverageCapability>,
    ) -> Result<Self, String> {
        let envelope: ViewEnvelopeWire =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "view")?;
        let certificate = required_certificate(envelope.certificate.as_ref())?;
        let root = view_root_from_wire(&envelope.snapshot.root, certificate, capability)?;
        let freshness = freshness_from_wire(envelope.snapshot.freshness, certificate)?;
        let next = envelope
            .snapshot
            .next
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?;
        Ok(Self {
            request_id: envelope.request_id,
            snapshot: ViewSnapshot {
                root,
                freshness,
                next,
                graph_relations: None,
                rich_graph: None,
            },
            certificate: envelope.certificate,
        })
    }
}

pub(crate) fn ensure_version(version: u16, kind: &str) -> Result<(), String> {
    if version == DTO_VERSION {
        Ok(())
    } else {
        Err(format!("unsupported {kind} DTO version"))
    }
}

pub(crate) fn required_certificate(
    certificate: Option<&WireCertificate>,
) -> Result<&WireCertificate, String> {
    certificate.ok_or_else(|| {
        "identity-bearing DTO requires a producer certificate with canonical preimages".to_owned()
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandEnvelope {
    version: u16,
    request_id: u64,
    command: CommandWire,
    certificate: Option<WireCertificate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
enum CommandWire {
    Packages(EmptyWire),
    PackagePage(PageRequestWire),
    Add(PackageWire),
    Remove(PackageWire),
    Document(DocumentQueryWire),
    Source(DocumentQueryWire),
    Show(SymbolWire),
    Outline(OutlineQueryWire),
    OutlinePage(PackagePageWire),
    Name(NameQueryWire),
    Resolve(TextWire),
    Search(QueryWire),
    Graph(GraphNeighborhoodQueryWire),
    Related(GraphNeighborhoodQueryWire),
    GraphPage(SymbolPageWire),
    GraphQuery(GraphQueryRequestWire),
    Surface(crate::SurfaceCommand),
    Health(EmptyWire),
    Revision(EmptyWire),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmptyWire {}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageWire {
    package: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SymbolWire {
    symbol: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextWire {
    pub(crate) text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CursorWire {
    recipe: String,
    version: String,
    branch: String,
    log: String,
    schema: u16,
    root: String,
    sequence: u64,
    query_offset: u64,
}

impl CursorWire {
    pub(crate) fn recipe(&self) -> &str {
        &self.recipe
    }

    pub(crate) fn version(&self) -> &str {
        &self.version
    }

    pub(crate) fn branch(&self) -> &str {
        &self.branch
    }

    pub(crate) fn log(&self) -> &str {
        &self.log
    }

    pub(crate) const fn schema(&self) -> u16 {
        self.schema
    }

    pub(crate) fn root(&self) -> &str {
        &self.root
    }

    pub(crate) const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub(crate) const fn query_offset(&self) -> u64 {
        self.query_offset
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrontierWire {
    branch: String,
    log: String,
    schema: u16,
    root: String,
    sequence: u64,
}

impl FrontierWire {}

impl Serialize for CommandDto {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let command = command_to_wire(&self.command);
        CommandEnvelope {
            version: DTO_VERSION,
            request_id: self.request_id,
            command,
            certificate: self.certificate().cloned(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CommandDto {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let envelope = CommandEnvelope::deserialize(deserializer)?;
        if envelope.version != DTO_VERSION {
            return Err(serde::de::Error::custom("unsupported command DTO version"));
        }
        let command = command_from_wire(envelope.command, envelope.certificate.as_ref())
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            request_id: envelope.request_id,
            command,
            certificate: envelope.certificate,
        })
    }
}

fn command_to_wire(command: &Command) -> CommandWire {
    match command {
        Command::Packages => CommandWire::Packages(EmptyWire {}),
        Command::PackagePage(page) => CommandWire::PackagePage(page_request_to_wire(*page)),
        Command::Add { package } => CommandWire::Add(PackageWire {
            package: encode_id(package.as_bytes()),
        }),
        Command::Remove { package } => CommandWire::Remove(PackageWire {
            package: encode_id(package.as_bytes()),
        }),
        Command::Document(query) => CommandWire::Document(document_query_to_wire(query)),
        Command::Source(query) => CommandWire::Source(document_query_to_wire(query)),
        Command::Show { symbol } => CommandWire::Show(SymbolWire {
            symbol: encode_id(symbol.as_bytes()),
        }),
        Command::Outline(query) => CommandWire::Outline(outline_query_to_wire(query)),
        Command::OutlinePage { package, page } => CommandWire::OutlinePage(PackagePageWire {
            package: encode_id(package.as_bytes()),
            page: page_request_to_wire(*page),
        }),
        Command::Name(query) => CommandWire::Name(name_query_to_wire(query)),
        Command::Resolve { text } => CommandWire::Resolve(TextWire { text: text.clone() }),
        Command::Search(query) => CommandWire::Search(query_to_wire(query)),
        Command::Graph(query) => CommandWire::Graph(graph_query_to_wire(query)),
        Command::Related(query) => CommandWire::Related(graph_query_to_wire(query)),
        Command::GraphPage { symbol, page } => CommandWire::GraphPage(SymbolPageWire {
            symbol: symbol_address_to_wire(*symbol),
            page: page_request_to_wire(*page),
        }),
        Command::GraphQuery(request) => CommandWire::GraphQuery(graph_request_to_wire(request)),
        Command::Surface(command) => CommandWire::Surface(command.clone()),
        Command::Health => CommandWire::Health(EmptyWire {}),
        Command::Revision => CommandWire::Revision(EmptyWire {}),
    }
}

fn command_from_wire(
    command: CommandWire,
    certificate: Option<&WireCertificate>,
) -> Result<Command, String> {
    match command {
        CommandWire::Packages(_) => Ok(Command::Packages),
        CommandWire::PackagePage(value) => Ok(Command::PackagePage(page_request_from_wire(
            value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Add(value) => Ok(Command::Add {
            package: required_certificate(certificate)?
                .key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
        }),
        CommandWire::Remove(value) => Ok(Command::Remove {
            package: required_certificate(certificate)?
                .key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
        }),
        CommandWire::Document(value) => Ok(Command::Document(document_query_from_wire(
            &value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Source(value) => Ok(Command::Source(document_query_from_wire(
            &value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Show(value) => Ok(Command::Show {
            symbol: required_certificate(certificate)?
                .key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
        }),
        CommandWire::Outline(value) => Ok(Command::Outline(outline_query_from_wire(
            &value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::OutlinePage(value) => Ok(Command::OutlinePage {
            package: required_certificate(certificate)?
                .key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
            page: page_request_from_wire(value.page, required_certificate(certificate)?)?,
        }),
        CommandWire::Name(value) => Ok(Command::Name(name_query_from_wire(
            value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Resolve(value) => Ok(Command::Resolve { text: value.text }),
        CommandWire::Search(value) => Ok(Command::Search(query_from_wire(
            value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Graph(value) => Ok(Command::Graph(graph_query_from_wire(
            &value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Related(value) => Ok(Command::Related(graph_query_from_wire(
            &value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::GraphPage(value) => Ok(Command::GraphPage {
            symbol: symbol_address_from_wire(&value.symbol, required_certificate(certificate)?)?,
            page: page_request_from_wire(value.page, required_certificate(certificate)?)?,
        }),
        CommandWire::GraphQuery(value) => Ok(Command::GraphQuery(graph_request_from_wire(
            value,
            required_certificate(certificate)?,
        )?)),
        CommandWire::Surface(value) => {
            value.admit().map_err(|error| error.to_string())?;
            Ok(Command::Surface(value))
        }
        CommandWire::Health(_) => Ok(Command::Health),
        CommandWire::Revision(_) => Ok(Command::Revision),
    }
}

fn revision_from_wire(
    certificate: &WireCertificate,
    encoded: &str,
) -> Result<crate::ViewRevision, String> {
    certificate
        .root_commitment_bytes::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)
        .map(crate::ViewRevision::from_bytes)
}
