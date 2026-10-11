//! Lossless portable-command certificate transport, independent of admission.
//!
//! BQPC/1 retains the closed command header and every certificate claim in
//! original order. Digests are untrusted raw bytes; canonical preimages and
//! evidence are exact length-delimited bytes. Expansion borrows those fields,
//! emits the existing CommandDto JSON, and grants no identities or authority.

use super::{CommandDto, CommandWire, command_to_wire, ensure_version};
use crate::{DTO_VERSION, MAX_COMMAND_BODY, WireCertificate, WireClaim, WireSchema};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};
use std::fmt;
use std::io::{self, Write};

const MAGIC: &[u8; 6] = b"BQPC\0\x01";
// The shortest claim is a tag, schema and 32-byte digest commitment.
const MIN_CLAIM_BYTES: usize = 34;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    version: u16,
    request_id: u64,
    command: CommandWire,
}

/// Packs one command using the versioned binary certificate layout.
///
/// The format changes only transport spelling. It retains the DTO version,
/// command fields, optional certificate, claim order, schemas, digests and
/// exact preimages. No compiler identity is constructed from digest bytes.
/// This is transport packing only; it does not admit a request or certificate.
///
/// # Errors
/// Refuses oversized frames, noncanonical digest spellings and codec errors.
pub fn encode_binary_certificate_command(request: &CommandDto) -> Result<Vec<u8>, String> {
    let header = Header {
        version: DTO_VERSION,
        request_id: request.request_id,
        command: command_to_wire(&request.command),
    };
    #[derive(Serialize)]
    struct OriginalEnvelope<'a> {
        version: u16,
        request_id: u64,
        command: &'a CommandWire,
        certificate: Option<&'a WireCertificate>,
    }
    // A compact frame must never bypass the existing expanded JSON cap.
    // Count the borrowed certificate without cloning any canonical preimage.
    let mut expanded = CountWriter(0);
    serde_json::to_writer(
        &mut expanded,
        &OriginalEnvelope {
            version: header.version,
            request_id: header.request_id,
            command: &header.command,
            certificate: request.certificate(),
        },
    )
    .map_err(codec_error)?;
    let header = encode_json(&header)?;
    let mut counter = CountWriter(0);
    write_frame(&mut counter, &header, request.certificate())?;
    let mut output = Vec::with_capacity(counter.0);
    write_frame(&mut output, &header, request.certificate())?;
    Ok(output)
}

/// Expands a binary command into the existing canonical JSON command body.
///
/// This is transport decoding, not command admission. Callers must pass the
/// result through the existing standalone or owner-aware command decoder.
/// All claim payloads remain borrowed until that decoder receives the JSON.
///
/// # Errors
/// Refuses unknown versions/tags/schemas, noncanonical headers, invalid UTF-8,
/// truncated or trailing fields, and frames or expanded JSON above the cap.
pub fn expand_binary_certificate_command(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_COMMAND_BODY {
        return Err("binary command exceeds body cap".to_owned());
    }
    let mut reader = Reader(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        return Err("unknown binary certificate command version".to_owned());
    }
    let header_bytes = reader.bytes()?;
    let header: Header = serde_json::from_slice(header_bytes).map_err(codec_error)?;
    ensure_version(header.version, "command")?;
    // One encoding per header. JSON aliases, whitespace and reordered fields
    // cannot silently produce another binary spelling of the same request.
    let canonical_header = encode_json(&header)?;
    if canonical_header != header_bytes {
        return Err("binary command header is not canonical".to_owned());
    }
    let certificate = match reader.byte()? {
        0 => None,
        1 => {
            let count = reader.length()?;
            if count > reader.0.len() / MIN_CLAIM_BYTES {
                return Err("binary certificate claim count exceeds remaining frame".to_owned());
            }
            let claims = reader.0;
            // Validate the complete borrowed sequence before serialization.
            // No count-sized Vec or payload-sized allocation is made here.
            for _ in 0..count {
                read_claim(&mut reader)?;
            }
            if !reader.0.is_empty() {
                return Err("binary certificate contains trailing bytes".to_owned());
            }
            Some(Certificate {
                claims: Claims {
                    bytes: claims,
                    count,
                },
            })
        }
        _ => return Err("unknown binary certificate presence tag".to_owned()),
    };
    if !reader.0.is_empty() {
        return Err("binary command contains trailing bytes".to_owned());
    }
    #[derive(Serialize)]
    struct Envelope<'a> {
        version: u16,
        request_id: u64,
        command: &'a CommandWire,
        certificate: Option<Certificate<'a>>,
    }
    encode_json(&Envelope {
        version: header.version,
        request_id: header.request_id,
        command: &header.command,
        certificate,
    })
}

fn encode_json(value: &impl Serialize) -> Result<Vec<u8>, String> {
    let mut counter = CountWriter(0);
    serde_json::to_writer(&mut counter, value).map_err(codec_error)?;
    let mut output = Vec::with_capacity(counter.0);
    serde_json::to_writer(&mut output, value).map_err(codec_error)?;
    Ok(output)
}

fn write_frame(
    writer: &mut impl Write,
    header: &[u8],
    certificate: Option<&WireCertificate>,
) -> Result<(), String> {
    writer.write_all(MAGIC).map_err(codec_error)?;
    write_bytes(writer, header)?;
    writer
        .write_all(&[u8::from(certificate.is_some())])
        .map_err(codec_error)?;
    if let Some(certificate) = certificate {
        write_length(writer, certificate.claims.len())?;
        for claim in &certificate.claims {
            write_claim(writer, claim)?;
        }
    }
    Ok(())
}

fn write_length(writer: &mut impl Write, length: usize) -> Result<(), String> {
    let length = u32::try_from(length).map_err(|_| "binary field length exceeds u32".to_owned())?;
    writer.write_all(&length.to_be_bytes()).map_err(codec_error)
}

fn write_bytes(writer: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    write_length(writer, bytes.len())?;
    writer.write_all(bytes).map_err(codec_error)
}

fn write_digest(writer: &mut impl Write, text: &str) -> Result<(), String> {
    let text = text.as_bytes();
    if text.len() != 64 {
        return Err(
            "binary digest requires exactly 64 lowercase hexadecimal characters".to_owned(),
        );
    }
    let nibble = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut digest = [0; 32];
    for (slot, pair) in digest.iter_mut().zip(text.chunks_exact(2)) {
        *slot = nibble(pair[0])
            .zip(nibble(pair[1]))
            .map(|(high, low)| high * 16 + low)
            .ok_or_else(|| "binary digest is not canonical lowercase hexadecimal".to_owned())?;
    }
    writer.write_all(&digest).map_err(codec_error)
}

fn write_schema_id(
    writer: &mut impl Write,
    tag: u8,
    schema: WireSchema,
    id: &str,
) -> Result<(), String> {
    writer
        .write_all(&[tag, schema_tag(schema)])
        .map_err(codec_error)?;
    write_digest(writer, id)
}

fn write_claim(writer: &mut impl Write, claim: &WireClaim) -> Result<(), String> {
    match claim {
        WireClaim::Key { schema, id, value } => {
            write_schema_id(writer, 1, *schema, id)?;
            write_bytes(writer, value.as_bytes())
        }
        WireClaim::KeyBytes { schema, id, value } => {
            write_schema_id(writer, 2, *schema, id)?;
            write_bytes(writer, value)
        }
        WireClaim::KeyCommitment { schema, id } => write_schema_id(writer, 3, *schema, id),
        WireClaim::RowIdentity {
            schema,
            id,
            preimage,
        } => {
            write_schema_id(writer, 4, *schema, id)?;
            write_bytes(writer, preimage.as_bytes())
        }
        WireClaim::Version { schema, id, value } => {
            write_schema_id(writer, 5, *schema, id)?;
            write_bytes(writer, value)
        }
        WireClaim::Root {
            schema,
            id,
            canonical,
        } => {
            write_schema_id(writer, 6, *schema, id)?;
            write_bytes(writer, canonical)
        }
        WireClaim::RootCommitment { schema, id } => write_schema_id(writer, 7, *schema, id),
        WireClaim::Intent { id, token, payload } => {
            writer.write_all(&[8]).map_err(codec_error)?;
            write_digest(writer, id)?;
            write_bytes(writer, token.as_bytes())?;
            write_bytes(writer, payload)
        }
        WireClaim::Delta {
            schema,
            id,
            base,
            target,
            changes,
        } => {
            write_schema_id(writer, 9, *schema, id)?;
            write_digest(writer, base)?;
            write_digest(writer, target)?;
            write_bytes(writer, changes)
        }
        WireClaim::Cursor {
            recipe,
            version,
            branch,
            log,
            schema,
            root,
            sequence,
        } => {
            writer.write_all(&[10]).map_err(codec_error)?;
            for digest in [recipe, version, branch, log] {
                write_digest(writer, digest)?;
            }
            writer
                .write_all(&schema.to_be_bytes())
                .map_err(codec_error)?;
            write_digest(writer, root)?;
            writer
                .write_all(&sequence.to_be_bytes())
                .map_err(codec_error)
        }
        WireClaim::Coverage {
            scope,
            observed,
            producer,
            context,
            evidence,
        } => {
            writer.write_all(&[11]).map_err(codec_error)?;
            for digest in [scope, observed, producer, context] {
                write_digest(writer, digest)?;
            }
            write_bytes(writer, evidence)
        }
    }
}

// Explicit tags belong to BQPC/1, never to Rust enum discriminant order.
const fn schema_tag(schema: WireSchema) -> u8 {
    match schema {
        WireSchema::Actor => 1,
        WireSchema::Package => 2,
        WireSchema::Symbol => 3,
        WireSchema::Object => 4,
        WireSchema::ViewRecipe => 5,
        WireSchema::ViewVersion => 6,
        WireSchema::Branch => 7,
        WireSchema::Log => 8,
        WireSchema::Document => 9,
        WireSchema::Name => 10,
        WireSchema::Outline => 11,
        WireSchema::SemanticShapeSource => 12,
        WireSchema::SemanticShapeBatch => 13,
        WireSchema::ViewRelation => 14,
    }
}

fn read_schema(reader: &mut Reader<'_>) -> Result<WireSchema, String> {
    match reader.byte()? {
        1 => Ok(WireSchema::Actor),
        2 => Ok(WireSchema::Package),
        3 => Ok(WireSchema::Symbol),
        4 => Ok(WireSchema::Object),
        5 => Ok(WireSchema::ViewRecipe),
        6 => Ok(WireSchema::ViewVersion),
        7 => Ok(WireSchema::Branch),
        8 => Ok(WireSchema::Log),
        9 => Ok(WireSchema::Document),
        10 => Ok(WireSchema::Name),
        11 => Ok(WireSchema::Outline),
        12 => Ok(WireSchema::SemanticShapeSource),
        13 => Ok(WireSchema::SemanticShapeBatch),
        14 => Ok(WireSchema::ViewRelation),
        _ => Err("unknown binary certificate schema tag".to_owned()),
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        if length > self.0.len() {
            return Err("truncated binary certificate field".to_owned());
        }
        let (value, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn length(&mut self) -> Result<usize, String> {
        let bytes = self.take(4)?.try_into().map_err(codec_error)?;
        usize::try_from(u32::from_be_bytes(bytes)).map_err(codec_error)
    }
    fn bytes(&mut self) -> Result<&'a [u8], String> {
        let length = self.length()?;
        self.take(length)
    }
    fn text(&mut self) -> Result<&'a str, String> {
        std::str::from_utf8(self.bytes()?).map_err(codec_error)
    }
    fn digest(&mut self) -> Result<Digest<'a>, String> {
        Ok(Digest(self.take(32)?.try_into().map_err(codec_error)?))
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().map_err(codec_error)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().map_err(codec_error)?,
        ))
    }
}

struct Digest<'a>(&'a [u8; 32]);
impl fmt::Display for Digest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}
impl Serialize for Digest<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum Claim<'a> {
    Key {
        schema: WireSchema,
        id: Digest<'a>,
        value: &'a str,
    },
    KeyBytes {
        schema: WireSchema,
        id: Digest<'a>,
        value: &'a [u8],
    },
    KeyCommitment {
        schema: WireSchema,
        id: Digest<'a>,
    },
    RowIdentity {
        schema: WireSchema,
        id: Digest<'a>,
        preimage: &'a str,
    },
    Version {
        schema: WireSchema,
        id: Digest<'a>,
        value: &'a [u8],
    },
    Root {
        schema: WireSchema,
        id: Digest<'a>,
        canonical: &'a [u8],
    },
    RootCommitment {
        schema: WireSchema,
        id: Digest<'a>,
    },
    Intent {
        id: Digest<'a>,
        token: &'a str,
        payload: &'a [u8],
    },
    Delta {
        schema: WireSchema,
        id: Digest<'a>,
        base: Digest<'a>,
        target: Digest<'a>,
        changes: &'a [u8],
    },
    Cursor {
        recipe: Digest<'a>,
        version: Digest<'a>,
        branch: Digest<'a>,
        log: Digest<'a>,
        schema: u16,
        root: Digest<'a>,
        sequence: u64,
    },
    Coverage {
        scope: Digest<'a>,
        observed: Digest<'a>,
        producer: Digest<'a>,
        context: Digest<'a>,
        evidence: &'a [u8],
    },
}

fn read_claim<'a>(reader: &mut Reader<'a>) -> Result<Claim<'a>, String> {
    Ok(match reader.byte()? {
        1 => Claim::Key {
            schema: read_schema(reader)?,
            id: reader.digest()?,
            value: reader.text()?,
        },
        2 => Claim::KeyBytes {
            schema: read_schema(reader)?,
            id: reader.digest()?,
            value: reader.bytes()?,
        },
        3 => Claim::KeyCommitment {
            schema: read_schema(reader)?,
            id: reader.digest()?,
        },
        4 => Claim::RowIdentity {
            schema: read_schema(reader)?,
            id: reader.digest()?,
            preimage: reader.text()?,
        },
        5 => Claim::Version {
            schema: read_schema(reader)?,
            id: reader.digest()?,
            value: reader.bytes()?,
        },
        6 => Claim::Root {
            schema: read_schema(reader)?,
            id: reader.digest()?,
            canonical: reader.bytes()?,
        },
        7 => Claim::RootCommitment {
            schema: read_schema(reader)?,
            id: reader.digest()?,
        },
        8 => Claim::Intent {
            id: reader.digest()?,
            token: reader.text()?,
            payload: reader.bytes()?,
        },
        9 => Claim::Delta {
            schema: read_schema(reader)?,
            id: reader.digest()?,
            base: reader.digest()?,
            target: reader.digest()?,
            changes: reader.bytes()?,
        },
        10 => Claim::Cursor {
            recipe: reader.digest()?,
            version: reader.digest()?,
            branch: reader.digest()?,
            log: reader.digest()?,
            schema: reader.u16()?,
            root: reader.digest()?,
            sequence: reader.u64()?,
        },
        11 => Claim::Coverage {
            scope: reader.digest()?,
            observed: reader.digest()?,
            producer: reader.digest()?,
            context: reader.digest()?,
            evidence: reader.bytes()?,
        },
        _ => return Err("unknown binary certificate claim tag".to_owned()),
    })
}

#[derive(Serialize)]
struct Certificate<'a> {
    claims: Claims<'a>,
}
struct Claims<'a> {
    bytes: &'a [u8],
    count: usize,
}
impl Serialize for Claims<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut reader = Reader(self.bytes);
        let mut sequence = serializer.serialize_seq(Some(self.count))?;
        for _ in 0..self.count {
            let claim = read_claim(&mut reader).map_err(serde::ser::Error::custom)?;
            sequence.serialize_element(&claim)?;
        }
        sequence.end()
    }
}

struct CountWriter(usize);
impl Write for CountWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_COMMAND_BODY.saturating_sub(self.0) {
            return Err(io::Error::other("binary command exceeds body cap"));
        }
        self.0 += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn codec_error(error: impl fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "binary_certificate_tests.rs"]
mod tests;
