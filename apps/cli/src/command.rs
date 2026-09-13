//! CLI command grammar and producer-certificate construction.

use crate::MAX_TEXT;
use backend_library::{
    Command, DocumentQuery, GraphNeighborhoodQuery, NameQuery, OutlineQuery, PackageReference,
    Query, QueryLimit, SurfaceCommand, ViewRelation, ViewStateRoot, WireCertificate, WireClaim,
    WireSchema, admit_root_against, encode_id, package_key, symbol_key,
};

/// Parses the bounded command grammar without trusting a digest-only basis.
///
/// Queries carrying `--basis` must be parsed with [`parse_with_basis`] after a
/// daemon supplied the accepted current source root. This entry point remains
/// useful for commands that do not carry a basis and for validating syntax.
///
/// # Errors
///
/// Returns a message describing a missing command, invalid argument, unknown
/// option, out-of-range bound, or an unverified basis claim.
pub fn parse(args: &[String]) -> Result<Command, String> {
    parse_with_basis(args, None)
}

/// Parses the bounded command grammar against an accepted source root.
///
/// The caller obtains `expected_basis` from a trusted daemon reply. The
/// user-supplied hexadecimal `--basis` value is admitted by exact comparison
/// before it is lowered into a typed [`Command`].
///
/// # Errors
///
/// Returns a message describing a missing command, invalid argument, unknown
/// option, out-of-range bound, or a basis claim that does not match the
/// accepted root.
pub fn parse_with_basis(
    args: &[String],
    expected_basis: Option<ViewStateRoot>,
) -> Result<Command, String> {
    let Some(name) = args.first().map(String::as_str) else {
        return Err("missing command".to_owned());
    };
    match name {
        "packages" => require_no_extra(args, "packages").map(|()| Command::Packages),
        "health" => require_no_extra(args, "health").map(|()| Command::Revision),
        "add" | "index" => package(args, true),
        "remove" => package(args, false),
        "outline" => outline(args, expected_basis),
        "document" => symbol(args, expected_basis),
        "source" => source(args, expected_basis),
        "show" => show(args),
        "graph" => symbol_graph(args, expected_basis),
        "related" => related(args, expected_basis),
        "diff" => diff(args),
        "surface" => surface(args),
        "resolve" => resolve(args),
        "name" => name_query(args, expected_basis),
        "search" => search(args, expected_basis),
        other => Err(format!("unknown command: {other}")),
    }
}

/// Parses the complete daemon-owned product surface through its closed,
/// tagged command representation. Keeping one typed bridge here means new
/// surface variants cannot be exposed with a second, drifting CLI grammar.
fn surface(args: &[String]) -> Result<Command, String> {
    let [_, encoded] = args else {
        return Err("surface requires exactly one JSON command object".to_owned());
    };
    if encoded.len() > crate::MAX_FRAME {
        return Err("surface command exceeds the local control frame bound".to_owned());
    }
    let command = serde_json::from_str::<SurfaceCommand>(encoded)
        .map_err(|error| format!("invalid surface command: {error}"))?;
    command
        .admit()
        .map_err(|error| format!("invalid surface command: {error}"))?;
    Ok(Command::Surface(command))
}

fn diff(args: &[String]) -> Result<Command, String> {
    let [_, from, to] = args else {
        return Err("diff requires exactly two package references".to_owned());
    };
    let from = PackageReference::parse(from.clone())
        .map_err(|error| format!("invalid older package reference: {error}"))?;
    let to = PackageReference::parse(to.clone())
        .map_err(|error| format!("invalid newer package reference: {error}"))?;
    Ok(Command::Surface(SurfaceCommand::Diff { from, to }))
}

fn require_no_extra(args: &[String], name: &str) -> Result<(), String> {
    if args.len() == 1 {
        Ok(())
    } else {
        Err(format!("{name} takes no arguments"))
    }
}

fn bounded_text(value: &str, what: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err(format!("{what} cannot be empty"));
    }
    if value.len() > MAX_TEXT {
        return Err(format!("{what} exceeds {MAX_TEXT} bytes"));
    }
    Ok(value.to_owned())
}

fn package(args: &[String], add: bool) -> Result<Command, String> {
    let coordinate = args
        .get(1)
        .ok_or_else(|| "command requires package".to_owned())?;
    if args.len() != 2 {
        return Err("package commands take exactly one package".to_owned());
    }
    let package_name = bounded_text(coordinate, "package")?;
    let package = package_key(&package_name);
    Ok(if add {
        Command::Add { package }
    } else {
        Command::Remove { package }
    })
}

fn admitted_basis(
    value: &str,
    expected_basis: Option<ViewStateRoot>,
) -> Result<ViewStateRoot, String> {
    let expected = expected_basis
        .ok_or_else(|| "basis claim requires a daemon supplied expected root".to_owned())?;
    admit_root_against::<ViewRelation>(value, expected).map_err(|error| error.to_string())
}

fn basis_only(
    args: &[String],
    start: usize,
    expected_basis: Option<ViewStateRoot>,
) -> Result<ViewStateRoot, String> {
    let mut basis = None;
    let mut at = start;
    while at < args.len() {
        match args[at].as_str() {
            "--basis" => {
                if basis.is_some() {
                    return Err("--basis may only be supplied once".to_owned());
                }
                let value = args
                    .get(at + 1)
                    .ok_or_else(|| "--basis requires a root".to_owned())?;
                basis = Some(admitted_basis(value, expected_basis)?);
                at += 2;
            }
            "--limit" => {
                return Err("this command does not accept --limit".to_owned());
            }
            other => return Err(format!("unknown query option: {other}")),
        }
    }
    basis.ok_or_else(|| "query commands require --basis".to_owned())
}

fn basis_and_limit(
    args: &[String],
    start: usize,
    expected_basis: Option<ViewStateRoot>,
) -> Result<(ViewStateRoot, QueryLimit), String> {
    let mut basis = None;
    let mut limit = QueryLimit::default();
    let mut limit_seen = false;
    let mut at = start;
    while at < args.len() {
        match args[at].as_str() {
            "--basis" => {
                if basis.is_some() {
                    return Err("--basis may only be supplied once".to_owned());
                }
                let value = args
                    .get(at + 1)
                    .ok_or_else(|| "--basis requires a root".to_owned())?;
                basis = Some(admitted_basis(value, expected_basis)?);
                at += 2;
            }
            "--limit" => {
                if limit_seen {
                    return Err("--limit may only be supplied once".to_owned());
                }
                limit_seen = true;
                let value = args
                    .get(at + 1)
                    .ok_or_else(|| "--limit requires a positive integer".to_owned())?;
                let parsed = value
                    .parse::<u16>()
                    .map_err(|_| "--limit requires a positive integer".to_owned())?;
                limit = QueryLimit::new(parsed)
                    .ok_or_else(|| "--limit is outside the bounded query range".to_owned())?;
                at += 2;
            }
            other => return Err(format!("unknown query option: {other}")),
        }
    }
    Ok((
        basis.ok_or_else(|| "query commands require --basis".to_owned())?,
        limit,
    ))
}

fn outline(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let coordinate = args
        .get(1)
        .ok_or_else(|| "outline requires package".to_owned())?;
    let basis = basis_only(args, 2, expected_basis)?;
    let package = package_key(&bounded_text(coordinate, "package")?);
    Ok(Command::Outline(OutlineQuery::new(package, basis)))
}

fn symbol(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let address = args
        .get(1)
        .ok_or_else(|| "document requires symbol".to_owned())?;
    let basis = basis_only(args, 2, expected_basis)?;
    let symbol = symbol_key(&bounded_text(address, "symbol")?);
    Ok(Command::Document(DocumentQuery::new(symbol, basis)))
}

fn source(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let address = args
        .get(1)
        .ok_or_else(|| "source requires symbol".to_owned())?;
    let basis = basis_only(args, 2, expected_basis)?;
    let symbol = symbol_key(&bounded_text(address, "symbol")?);
    Ok(Command::Source(DocumentQuery::new(symbol, basis)))
}

fn show(args: &[String]) -> Result<Command, String> {
    let address = args
        .get(1)
        .ok_or_else(|| "show requires symbol".to_owned())?;
    if args.len() != 2 {
        return Err("show takes exactly one symbol".to_owned());
    }
    Ok(Command::Show {
        symbol: symbol_key(&bounded_text(address, "symbol")?),
    })
}

fn symbol_graph(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let address = args
        .get(1)
        .ok_or_else(|| "graph requires symbol".to_owned())?;
    let basis = basis_only(args, 2, expected_basis)?;
    Ok(Command::Graph(GraphNeighborhoodQuery::new(
        symbol_key(&bounded_text(address, "symbol")?),
        basis,
    )))
}

fn related(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let address = args
        .get(1)
        .ok_or_else(|| "related requires symbol".to_owned())?;
    let basis = basis_only(args, 2, expected_basis)?;
    Ok(Command::Related(GraphNeighborhoodQuery::new(
        symbol_key(&bounded_text(address, "symbol")?),
        basis,
    )))
}

fn name_query(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let text = args.get(1).ok_or_else(|| "name requires text".to_owned())?;
    let (basis, limit) = basis_and_limit(args, 2, expected_basis)?;
    Ok(Command::Name(NameQuery::new(
        bounded_text(text, "name")?,
        basis,
        limit,
    )))
}

fn resolve(args: &[String]) -> Result<Command, String> {
    let text = args
        .get(1)
        .ok_or_else(|| "resolve requires text".to_owned())?;
    if args.len() != 2 {
        return Err("resolve takes exactly one text value".to_owned());
    }
    Ok(Command::Resolve {
        text: bounded_text(text, "resolve")?,
    })
}

fn search(args: &[String], expected_basis: Option<ViewStateRoot>) -> Result<Command, String> {
    let text = args
        .get(1)
        .ok_or_else(|| "search requires text".to_owned())?;
    let (basis, limit) = basis_and_limit(args, 2, expected_basis)?;
    Ok(Command::Search(Query::new(
        bounded_text(text, "search")?,
        basis,
        limit,
    )))
}

pub(crate) fn needs_basis(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("outline" | "document" | "source" | "name" | "graph" | "related" | "search")
    )
}

fn key_certificate(schema: WireSchema, id: String, value: &str) -> WireCertificate {
    WireCertificate::new().with_claim(WireClaim::Key {
        schema,
        id,
        value: value.to_owned(),
    })
}

pub(crate) fn certificate_for_command(
    args: &[String],
    command: &Command,
    basis_certificate: Option<&WireCertificate>,
) -> Result<Option<WireCertificate>, String> {
    let key = |schema, id, value: &str| key_certificate(schema, encode_id(id), value);
    match command {
        Command::Packages
        | Command::Resolve { .. }
        | Command::Health
        | Command::Revision
        | Command::Surface(_) => Ok(None),
        Command::PackagePage(_) | Command::GraphQuery(_) => {
            Ok(Some(basis_certificate.cloned().ok_or_else(|| {
                "page basis certificate is missing".to_owned()
            })?))
        }
        Command::Add { package } | Command::Remove { package } => {
            let value = args
                .get(1)
                .ok_or_else(|| "command requires package".to_owned())?;
            Ok(Some(key(WireSchema::Package, package.as_bytes(), value)))
        }
        Command::Show { symbol } => {
            let value = args
                .get(1)
                .ok_or_else(|| "command requires symbol".to_owned())?;
            Ok(Some(key(WireSchema::Symbol, symbol.as_bytes(), value)))
        }
        Command::Graph(query) | Command::Related(query) => {
            let value = args
                .get(1)
                .ok_or_else(|| "graph requires symbol".to_owned())?;
            let certificate = basis_certificate
                .cloned()
                .ok_or_else(|| "query basis certificate is missing".to_owned())?;
            Ok(Some(certificate.with_claim_once(WireClaim::Key {
                schema: WireSchema::Symbol,
                id: encode_id(&query.symbol().claimed_bytes()),
                value: value.to_owned(),
            })))
        }
        Command::GraphPage { symbol, .. } => {
            let value = args
                .get(1)
                .ok_or_else(|| "graph requires symbol".to_owned())?;
            let certificate = basis_certificate
                .cloned()
                .ok_or_else(|| "page basis certificate is missing".to_owned())?;
            Ok(Some(certificate.with_claim_once(WireClaim::Key {
                schema: WireSchema::Symbol,
                id: encode_id(&symbol.claimed_bytes()),
                value: value.to_owned(),
            })))
        }
        Command::Document(query) | Command::Source(query) => {
            let value = args
                .get(1)
                .ok_or_else(|| "document requires symbol".to_owned())?;
            let certificate = basis_certificate
                .cloned()
                .ok_or_else(|| "query basis certificate is missing".to_owned())?;
            Ok(Some(certificate.with_claim_once(WireClaim::Key {
                schema: WireSchema::Symbol,
                id: encode_id(&query.symbol().claimed_bytes()),
                value: value.to_owned(),
            })))
        }
        Command::Outline(query) => {
            let value = args
                .get(1)
                .ok_or_else(|| "outline requires package".to_owned())?;
            let certificate = basis_certificate
                .cloned()
                .ok_or_else(|| "query basis certificate is missing".to_owned())?;
            Ok(Some(certificate.with_claim_once(WireClaim::Key {
                schema: WireSchema::Package,
                id: encode_id(query.package().as_bytes()),
                value: value.to_owned(),
            })))
        }
        Command::OutlinePage { package, .. } => {
            let value = args
                .get(1)
                .ok_or_else(|| "outline requires package".to_owned())?;
            let certificate = basis_certificate
                .cloned()
                .ok_or_else(|| "page basis certificate is missing".to_owned())?;
            Ok(Some(certificate.with_claim_once(WireClaim::Key {
                schema: WireSchema::Package,
                id: encode_id(package.as_bytes()),
                value: value.to_owned(),
            })))
        }
        Command::Name(_) | Command::Search(_) => {
            Ok(Some(basis_certificate.cloned().ok_or_else(|| {
                "query basis certificate is missing".to_owned()
            })?))
        }
    }
}
