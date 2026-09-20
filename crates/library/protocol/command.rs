use super::{
    DTO_VERSION, ReplyEnvelope, ViewEnvelopeWire, WireCertificate, WireSchema, freshness_from_wire,
    reply_from_wire, reply_from_wire_with_verifier, view_root_from_wire,
};
use crate::canonical::{
    BranchSchema, LogSchema, PackageSchema, SymbolSchema, ViewRecipeSchema, ViewVersionSchema,
    encode_id,
};
use crate::{
    Command, CommandReply, CompleteViewProjection, CoverageCapability, Cursor, DocumentQuery,
    Frontier, GraphQuery, NameQuery, OutlineQuery, Query, QueryLimit, ViewSnapshot, ViewStateRoot,
};
use backend_version::ProducerObservationVerifier;
use serde::{Deserialize, Serialize};

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
    Add(PackageWire),
    Remove(PackageWire),
    Document(DocumentQueryWire),
    Show(SymbolWire),
    Outline(OutlineQueryWire),
    Name(NameQueryWire),
    Resolve(TextWire),
    Search(QueryWire),
    Graph(GraphQueryWire),
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
struct DocumentQueryWire {
    symbol: String,
    basis: String,
    source: Option<super::BasisWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OutlineQueryWire {
    package: String,
    basis: String,
    source: Option<super::BasisWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphQueryWire {
    symbol: String,
    basis: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NameQueryWire {
    text: String,
    limit: u16,
    basis: String,
    cursor: Option<CursorWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryWire {
    text: String,
    limit: u16,
    basis: String,
    cursor: Option<CursorWire>,
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
        Command::Add { package } => CommandWire::Add(PackageWire {
            package: encode_id(package.as_bytes()),
        }),
        Command::Remove { package } => CommandWire::Remove(PackageWire {
            package: encode_id(package.as_bytes()),
        }),
        Command::Document(query) => CommandWire::Document(document_query_to_wire(query)),
        Command::Show { symbol } => CommandWire::Show(SymbolWire {
            symbol: encode_id(symbol.as_bytes()),
        }),
        Command::Outline(query) => CommandWire::Outline(outline_query_to_wire(query)),
        Command::Name(query) => CommandWire::Name(name_query_to_wire(query)),
        Command::Resolve { text } => CommandWire::Resolve(TextWire { text: text.clone() }),
        Command::Search(query) => CommandWire::Search(query_to_wire(query)),
        Command::Graph(query) => CommandWire::Graph(graph_query_to_wire(query)),
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
        CommandWire::Show(value) => Ok(Command::Show {
            symbol: required_certificate(certificate)?
                .key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
        }),
        CommandWire::Outline(value) => Ok(Command::Outline(outline_query_from_wire(
            &value,
            required_certificate(certificate)?,
        )?)),
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
        CommandWire::Health(_) => Ok(Command::Health),
        CommandWire::Revision(_) => Ok(Command::Revision),
    }
}

fn document_query_to_wire(query: &DocumentQuery) -> DocumentQueryWire {
    DocumentQueryWire {
        symbol: encode_id(query.symbol().as_bytes()),
        basis: encode_id(query.basis().as_bytes()),
        source: query.source_basis().map(super::basis_to_wire),
    }
}

fn document_query_from_wire(
    value: &DocumentQueryWire,
    certificate: &WireCertificate,
) -> Result<DocumentQuery, String> {
    let basis = revision_from_wire(certificate, &value.basis)?;
    let source = value
        .source
        .as_ref()
        .map(|source| super::basis_from_wire(source, certificate))
        .transpose()?;
    if source.is_some_and(|source| !basis.matches(source.root)) {
        return Err("document query source basis does not match its root".to_owned());
    }
    Ok(DocumentQuery {
        symbol: certificate.key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
        basis,
        source,
    })
}

fn outline_query_to_wire(query: &OutlineQuery) -> OutlineQueryWire {
    OutlineQueryWire {
        package: encode_id(query.package().as_bytes()),
        basis: encode_id(query.basis().as_bytes()),
        source: query.source_basis().map(super::basis_to_wire),
    }
}

fn outline_query_from_wire(
    value: &OutlineQueryWire,
    certificate: &WireCertificate,
) -> Result<OutlineQuery, String> {
    let basis = revision_from_wire(certificate, &value.basis)?;
    let source = value
        .source
        .as_ref()
        .map(|source| super::basis_from_wire(source, certificate))
        .transpose()?;
    if source.is_some_and(|source| !basis.matches(source.root)) {
        return Err("outline query source basis does not match its root".to_owned());
    }
    Ok(OutlineQuery {
        package: certificate.key_value::<PackageSchema>(WireSchema::Package, &value.package)?,
        basis,
        source,
    })
}

fn graph_query_to_wire(query: &GraphQuery) -> GraphQueryWire {
    GraphQueryWire {
        symbol: encode_id(query.symbol().as_bytes()),
        basis: encode_id(query.basis().as_bytes()),
    }
}

fn graph_query_from_wire(
    value: &GraphQueryWire,
    certificate: &WireCertificate,
) -> Result<GraphQuery, String> {
    Ok(GraphQuery {
        symbol: certificate.key_value::<SymbolSchema>(WireSchema::Symbol, &value.symbol)?,
        basis: revision_from_wire(certificate, &value.basis)?,
    })
}

fn name_query_to_wire(query: &NameQuery) -> NameQueryWire {
    NameQueryWire {
        text: query.text().to_owned(),
        limit: query.limit().get(),
        basis: encode_id(query.basis().as_bytes()),
        cursor: query.cursor().map(cursor_to_wire),
    }
}

fn name_query_from_wire(
    value: NameQueryWire,
    certificate: &WireCertificate,
) -> Result<NameQuery, String> {
    let limit = QueryLimit::new(value.limit).ok_or_else(|| "invalid query limit".to_owned())?;
    Ok(NameQuery {
        text: value.text,
        limit,
        basis: revision_from_wire(certificate, &value.basis)?,
        cursor: value
            .cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?,
        read_manifest: None,
    })
}

fn query_to_wire(query: &Query) -> QueryWire {
    QueryWire {
        text: query.text().to_owned(),
        limit: query.limit().get(),
        basis: encode_id(query.basis().as_bytes()),
        cursor: query.cursor().map(cursor_to_wire),
    }
}

fn query_from_wire(value: QueryWire, certificate: &WireCertificate) -> Result<Query, String> {
    let limit = QueryLimit::new(value.limit).ok_or_else(|| "invalid query limit".to_owned())?;
    Ok(Query {
        text: value.text,
        limit,
        basis: revision_from_wire(certificate, &value.basis)?,
        cursor: value
            .cursor
            .as_ref()
            .map(|cursor| cursor_from_wire(cursor, certificate))
            .transpose()?,
        read_manifest: None,
    })
}

fn revision_from_wire(
    certificate: &WireCertificate,
    encoded: &str,
) -> Result<crate::ViewRevision, String> {
    certificate
        .root_commitment_bytes::<crate::ViewRelation>(WireSchema::ViewRelation, encoded)
        .map(crate::ViewRevision::from_bytes)
}

pub(crate) fn cursor_to_wire(cursor: Cursor) -> CursorWire {
    CursorWire {
        recipe: encode_id(cursor.recipe().as_bytes()),
        version: encode_id(cursor.version().as_bytes()),
        branch: encode_id(cursor.branch().as_bytes()),
        log: encode_id(cursor.log().as_bytes()),
        schema: cursor.schema(),
        root: encode_id(cursor.root().as_bytes()),
        sequence: cursor.sequence(),
        query_offset: cursor.query_offset(),
    }
}

pub(crate) fn cursor_from_wire(
    value: &CursorWire,
    certificate: &WireCertificate,
) -> Result<Cursor, String> {
    Ok(Cursor::for_view(
        certificate.key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.recipe)?,
        certificate.version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.version)?,
        Frontier::new(
            certificate.key_value::<BranchSchema>(WireSchema::Branch, &value.branch)?,
            certificate.key_value::<LogSchema>(WireSchema::Log, &value.log)?,
            value.schema,
            certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &value.root)?,
            value.sequence,
        ),
    )
    .with_query_offset(value.query_offset))
}

pub(crate) fn cursor_from_wire_with_capability(
    value: &CursorWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Cursor, String> {
    Ok(Cursor::for_view(
        certificate.key_bytes::<ViewRecipeSchema>(WireSchema::ViewRecipe, &value.recipe)?,
        certificate.version_value::<ViewVersionSchema>(WireSchema::ViewVersion, &value.version)?,
        Frontier::new(
            certificate.key_value::<BranchSchema>(WireSchema::Branch, &value.branch)?,
            certificate.key_value::<LogSchema>(WireSchema::Log, &value.log)?,
            value.schema,
            certificate.producer_root_value::<crate::ViewRelation>(
                WireSchema::ViewRelation,
                &value.root,
                capability,
            )?,
            value.sequence,
        ),
    )
    .with_query_offset(value.query_offset))
}

pub(crate) fn frontier_to_wire(frontier: Frontier) -> FrontierWire {
    FrontierWire {
        branch: encode_id(frontier.branch.as_bytes()),
        log: encode_id(frontier.log.as_bytes()),
        schema: frontier.schema,
        root: encode_id(frontier.root.as_bytes()),
        sequence: frontier.sequence,
    }
}

pub(crate) fn frontier_from_wire(
    frontier: &FrontierWire,
    certificate: &WireCertificate,
) -> Result<Frontier, String> {
    Ok(Frontier::new(
        certificate.key_value::<BranchSchema>(WireSchema::Branch, &frontier.branch)?,
        certificate.key_value::<LogSchema>(WireSchema::Log, &frontier.log)?,
        frontier.schema,
        certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &frontier.root)?,
        frontier.sequence,
    ))
}

pub(crate) fn frontier_from_wire_with_capability(
    frontier: &FrontierWire,
    certificate: &WireCertificate,
    capability: &CoverageCapability,
) -> Result<Frontier, String> {
    let root = certificate
        .producer_root_value::<crate::ViewRelation>(
            WireSchema::ViewRelation,
            &frontier.root,
            capability,
        )
        .or_else(|_| {
            certificate.root_value::<crate::ViewRelation>(WireSchema::ViewRelation, &frontier.root)
        })?;
    Ok(Frontier::new(
        certificate.key_value::<BranchSchema>(WireSchema::Branch, &frontier.branch)?,
        certificate.key_value::<LogSchema>(WireSchema::Log, &frontier.log)?,
        frontier.schema,
        root,
        frontier.sequence,
    ))
}
