use super::*;
use crate::{Command, decode_command_body};

fn all_claims() -> WireCertificate {
    let id = "abcdef0123456789".repeat(4);
    let binary = vec![0, 255, 1, 128, b'\n', b'"', b'\\'].into_boxed_slice();
    let schema = WireSchema::Symbol;
    let claims = vec![
        WireClaim::Key {
            schema,
            id: id.clone(),
            value: "pkg::你好::café\n` | \\".to_owned(),
        },
        WireClaim::KeyBytes {
            schema,
            id: id.clone(),
            value: binary.clone(),
        },
        WireClaim::KeyCommitment {
            schema,
            id: id.clone(),
        },
        WireClaim::RowIdentity {
            schema,
            id: id.clone(),
            preimage: "scope::外部::overload\0with-NUL".to_owned(),
        },
        WireClaim::Version {
            schema: WireSchema::ViewVersion,
            id: id.clone(),
            value: binary.clone(),
        },
        WireClaim::Root {
            schema: WireSchema::ViewRelation,
            id: id.clone(),
            canonical: binary.clone(),
        },
        WireClaim::RootCommitment {
            schema: WireSchema::ViewRelation,
            id: id.clone(),
        },
        WireClaim::Intent {
            id: id.clone(),
            token: "operation\n`\"外部".to_owned(),
            payload: binary.clone(),
        },
        WireClaim::Delta {
            schema: WireSchema::ViewRelation,
            id: id.clone(),
            base: id.clone(),
            target: id.clone(),
            changes: binary.clone(),
        },
        WireClaim::Cursor {
            recipe: id.clone(),
            version: id.clone(),
            branch: id.clone(),
            log: id.clone(),
            schema: u16::MAX,
            root: id.clone(),
            sequence: u64::MAX,
        },
        WireClaim::Coverage {
            scope: id.clone(),
            observed: id.clone(),
            producer: id.clone(),
            context: id,
            evidence: binary,
        },
    ];
    claims
        .into_iter()
        .fold(WireCertificate::new(), WireCertificate::with_claim)
}

fn first_claim_offset(frame: &[u8]) -> usize {
    let mut reader = Reader(frame);
    reader.take(MAGIC.len()).expect("magic");
    reader.bytes().expect("header");
    assert_eq!(reader.byte().expect("certificate"), 1);
    reader.length().expect("count");
    frame.len() - reader.0.len()
}

#[test]
fn every_claim_family_and_duplicate_survives_exact_existing_json_roundtrip() {
    let certificate = all_claims();
    let certificate = certificate
        .clone()
        .with_claim(certificate.claims[0].clone());
    let request = CommandDto::new(u64::MAX, Command::Packages).with_certificate(certificate);
    let packed = encode_binary_certificate_command(&request).expect("closed transport");
    let expanded = expand_binary_certificate_command(&packed).expect("borrowed expansion");
    assert_eq!(
        expanded,
        serde_json::to_vec(&request).expect("existing JSON")
    );
    let decoded: CommandDto = serde_json::from_slice(&expanded).expect("identity-free command");
    assert_eq!(decoded, request);
    assert_eq!(decoded.certificate().expect("all claims").claims.len(), 12);
    assert_eq!(
        encode_binary_certificate_command(&decoded).expect("canonical repack"),
        packed
    );
}

#[test]
fn all_explicit_schema_tags_and_optional_certificate_spelling_are_closed() {
    for schema in [
        WireSchema::Actor,
        WireSchema::Package,
        WireSchema::Symbol,
        WireSchema::Object,
        WireSchema::ViewRecipe,
        WireSchema::ViewVersion,
        WireSchema::Branch,
        WireSchema::Log,
        WireSchema::Document,
        WireSchema::Name,
        WireSchema::Outline,
        WireSchema::SemanticShapeSource,
        WireSchema::SemanticShapeBatch,
        WireSchema::ViewRelation,
    ] {
        let bytes = [schema_tag(schema)];
        let mut reader = Reader(&bytes);
        assert_eq!(read_schema(&mut reader).expect("known schema"), schema);
        assert!(reader.0.is_empty());
    }
    for tag in [0, 15, 255] {
        assert!(read_schema(&mut Reader(&[tag])).is_err());
    }
    for certificate in [None, Some(WireCertificate::new())] {
        let mut request = CommandDto::new(1, Command::Packages);
        if let Some(certificate) = certificate {
            request = request.with_certificate(certificate);
        }
        let packed = encode_binary_certificate_command(&request).expect("optional certificate");
        assert_eq!(
            expand_binary_certificate_command(&packed).expect("exact JSON spelling"),
            serde_json::to_vec(&request).expect("original JSON")
        );
    }
}

#[test]
fn truncation_tags_lengths_utf8_and_trailing_bytes_refuse_before_expansion() {
    let request = CommandDto::new(1, Command::Packages).with_certificate(all_claims());
    let good = encode_binary_certificate_command(&request).expect("fixture");
    for end in 0..good.len() {
        assert!(
            expand_binary_certificate_command(&good[..end]).is_err(),
            "prefix {end}"
        );
    }
    let offset = first_claim_offset(&good);
    let mut mutants = vec![];
    for (index, byte) in [
        (5, 2),
        (offset, 0),
        (offset, 255),
        (offset + 1, 0),
        (offset + 1, 15),
        (offset + 38, 255),
        (offset - 5, 2),
    ] {
        let mut bad = good.clone();
        bad[index] = byte;
        mutants.push(bad);
    }
    for index in [6, offset - 4, offset + 34] {
        let mut bad = good.clone();
        bad[index..index + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        mutants.push(bad);
    }
    let mut trailing = good.clone();
    trailing.push(0);
    mutants.push(trailing);
    for bad in mutants {
        assert!(expand_binary_certificate_command(&bad).is_err());
    }
}

#[test]
fn unknown_header_fields_noncanonical_json_and_dto_versions_refuse() {
    let good =
        encode_binary_certificate_command(&CommandDto::new(1, Command::Packages)).expect("header");
    let mut reader = Reader(&good);
    reader.take(MAGIC.len()).expect("magic");
    let header = std::str::from_utf8(reader.bytes().expect("header")).expect("UTF-8");
    let tail = reader.0;
    for altered in [
        format!(" {header}"),
        header.replacen("\"version\":", "\"unknown\":0,\"version\":", 1),
        header.replacen(
            &format!("\"version\":{DTO_VERSION}"),
            "\"version\":65535",
            1,
        ),
    ] {
        let mut bad = MAGIC.to_vec();
        write_bytes(&mut bad, altered.as_bytes()).expect("length");
        bad.extend_from_slice(tail);
        assert!(expand_binary_certificate_command(&bad).is_err());
    }
}

#[test]
fn physical_and_expanded_caps_are_independent_and_checked_before_output_allocation() {
    assert!(expand_binary_certificate_command(&vec![0; MAX_COMMAND_BODY + 1]).is_err());
    let certificate = WireCertificate::new().with_claim(WireClaim::Root {
        schema: WireSchema::ViewRelation,
        id: "ab".repeat(32),
        canonical: vec![255; MAX_COMMAND_BODY / 3].into_boxed_slice(),
    });
    let request = CommandDto::new(1, Command::Packages).with_certificate(certificate.clone());
    assert!(
        encode_binary_certificate_command(&request).is_err(),
        "compact packing cannot bypass expanded cap"
    );
    let header = encode_json(&Header {
        version: DTO_VERSION,
        request_id: 1,
        command: command_to_wire(&Command::Packages),
    })
    .expect("header");
    let mut untrusted = vec![];
    write_frame(&mut untrusted, &header, Some(&certificate)).expect("compact wire input");
    assert!(untrusted.len() < MAX_COMMAND_BODY);
    assert!(
        expand_binary_certificate_command(&untrusted).is_err(),
        "expanded JSON cap still applies"
    );
    let mut counter = CountWriter(MAX_COMMAND_BODY - 1);
    assert!(counter.write_all(&[1]).is_ok());
    assert!(counter.write_all(&[1]).is_err());
}

#[test]
fn noncanonical_digest_spellings_are_not_normalized_into_accepted_ids() {
    for id in ["AB".repeat(32), "ab".repeat(31), "z0".repeat(32)] {
        let request = CommandDto::new(1, Command::Packages).with_certificate(
            WireCertificate::new().with_claim(WireClaim::KeyCommitment {
                schema: WireSchema::Symbol,
                id,
            }),
        );
        assert!(encode_binary_certificate_command(&request).is_err());
    }
}

#[test]
fn retained_go_graph_command_expands_to_exact_original_proof_before_existing_admission() {
    let original = include_bytes!("fixtures/current-go-graph-continuation.json");
    // Parse only the closed untrusted wire envelope; this constructs no IDs.
    let envelope: super::super::CommandEnvelope =
        serde_json::from_slice(original).expect("retained producer envelope");
    let header = encode_json(&Header {
        version: envelope.version,
        request_id: envelope.request_id,
        command: envelope.command,
    })
    .expect("closed header");
    let mut packed = vec![];
    write_frame(&mut packed, &header, envelope.certificate.as_ref())
        .expect("all producer material");
    assert_eq!(
        packed.as_slice(),
        include_bytes!("fixtures/current-go-graph-continuation.bqpc1")
    );
    let expanded =
        expand_binary_certificate_command(&packed).expect("lossless real proof expansion");
    assert_eq!(
        expanded.as_slice(),
        original,
        "including every canonical preimage and authority byte"
    );
    let admitted_original = decode_command_body(original)
        .expect("retained real producer command passes the existing strict decoder");
    let admitted_expanded = decode_command_body(&expanded)
        .expect("exact restored producer command passes the same strict decoder");
    assert_eq!(
        admitted_expanded, admitted_original,
        "transport supplies no new authority"
    );
    // Corrupt the real root preimage without changing its claimed digest. The
    // binary layer represents the input; only existing proof admission may trust it.
    let mut reader = Reader(&packed);
    reader.take(MAGIC.len()).expect("magic");
    reader.bytes().expect("header");
    reader.byte().expect("presence");
    let count = reader.length().expect("count");
    let mut root_offset = None;
    for _ in 0..count {
        let start = packed.len() - reader.0.len();
        if let Claim::Root { .. } = read_claim(&mut reader).expect("borrowed claim") {
            // Root payload follows its tag, schema, digest and u32 length.
            root_offset = Some(start + 1 + 1 + 32 + 4);
        }
    }
    let offset = root_offset.expect("actual predecessor root");
    let mut corrupted = packed;
    corrupted[offset] ^= 1;
    let expanded = expand_binary_certificate_command(&corrupted)
        .expect("untrusted bytes remain transport-only");
    assert!(
        decode_command_body(&expanded).is_err(),
        "changed root preimage is never admitted by transport"
    );
}
