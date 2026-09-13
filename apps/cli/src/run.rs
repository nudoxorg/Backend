//! Execution: one typed request becomes one presentation answer.
//!
//! A few commands need more than one round trip, and this is where that is
//! decided rather than inside a renderer. A declaration page is a document
//! plus the rows around it, and an outline is a tree of keys plus the rows
//! that give those keys names — the engine answers each part separately, so
//! the surface asks for each part and hands the pieces to the shared
//! assembler. A probe that fails does not fail the command: it becomes a note
//! on the page, because "I could not look" and "there are none" are different
//! answers and a reader is owed the difference.

use backend_client::{ClientError, Session};
use backend_library::{CommandReply, IntentId, Row, SurfaceCommand, ViewSnapshot, encode_id};
use backend_present::{
    Fault, Identity, KeyTag, Operand, OutlineTree, Page, ProductRecord, ProductView, RecordList,
    Shelf, Status, outline_tree, page_from_document, product_view, record_list,
    shelf_from_snapshot,
};

use backend_present::Request;

/// Rows fetched for one page's members and relations.
const PROBE_ROWS: u16 = 200;

/// One answer, in the shared presentation vocabulary.
#[derive(Clone, Debug)]
pub enum Answer {
    /// One declaration page.
    Page(Box<Page>),
    /// One bounded result page.
    Records(Box<RecordList>),
    /// The shelf.
    Shelf(Box<Shelf>),
    /// One named outline tree.
    Outline(Box<OutlineTree>),
    /// The engine's whole state.
    Status(Box<Status>),
    /// One durable product answer.
    Product(Box<ProductView>),
}

/// Executes one typed request against a connected session.
///
/// # Errors
///
/// Returns the typed fault the engine or the transport produced, with the
/// operand the caller supplied already attached.
pub fn execute(session: &mut Session, request: &Request) -> Result<Answer, Fault> {
    match request {
        Request::Shelf => shelf(session),
        Request::Status => status(session),
        Request::Index(path) => intent(session, path, true),
        Request::Remove(path) => intent(session, path, false),
        Request::Page(coordinate) => page(session, coordinate).map(Box::new).map(Answer::Page),
        Request::Source(coordinate) => source(session, coordinate),
        Request::Neighbourhood {
            coordinate,
            incoming,
        } => neighbourhood(session, coordinate, *incoming),
        Request::Search { text, limit } => {
            let snapshot = search_snapshot(session, text, *limit)?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::Resolve { text, limit } => {
            let snapshot = names_snapshot(session, text, *limit)?;
            Ok(Answer::Records(Box::new(record_list(text, &snapshot))))
        }
        Request::Outline(path) => outline(session, path),
        Request::Surface(command) => surface(session, command),
    }
}

fn shelf(session: &mut Session) -> Result<Answer, Fault> {
    let revision = session
        .revision()
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    let reply = session
        .packages()
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    let CommandReply::Packages(snapshot) = reply.reply else {
        return Err(shape("packages"));
    };
    Ok(Answer::Shelf(Box::new(shelf_from_snapshot(
        &snapshot,
        KeyTag::from_key(revision.root.as_bytes()),
    ))))
}

fn status(session: &mut Session) -> Result<Answer, Fault> {
    let report = session
        .health()
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    Ok(Answer::Status(Box::new(Status::from_report(&report, None))))
}

fn intent(session: &mut Session, path: &str, add: bool) -> Result<Answer, Fault> {
    let operand = Operand::Path(path.to_owned());
    let reply = if add {
        session.index(path)
    } else {
        session.remove(path)
    }
    .map_err(|error| Fault::from_client_error(&error, operand))?;
    let (heading, accepted) = match reply.reply {
        CommandReply::Added(id) => ("index", id),
        CommandReply::Removed(id) => ("remove", id),
        _ => return Err(shape("index")),
    };
    Ok(Answer::Product(Box::new(accepted_view(
        heading, path, accepted,
    ))))
}

fn accepted_view(heading: &str, path: &str, intent: IntentId) -> ProductView {
    ProductView::assembled(
        heading,
        vec![ProductRecord::new(
            format!("{heading} request accepted"),
            Some(path.to_owned()),
            vec![
                format!("intent {}", KeyTag::from_key(intent.as_bytes())),
                "pending".to_owned(),
                "read `backend health` for readiness".to_owned(),
            ],
        )],
    )
}

fn page(session: &mut Session, coordinate: &str) -> Result<Page, Fault> {
    let operand = Operand::Coordinate(backend_present::Coordinate::new(coordinate));
    let reply = session
        .document(coordinate)
        .map_err(|error| Fault::from_client_error(&error, operand.clone()))?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("document"));
    };
    let mut notes = Vec::with_capacity(2);
    let relations = probe_rows(session, coordinate, true, &mut notes);
    let members = probe_members(session, coordinate, &mut notes);
    Ok(page_from_document(
        coordinate, &document, &members, &relations, notes,
    ))
}

fn source(session: &mut Session, coordinate: &str) -> Result<Answer, Fault> {
    let operand = Operand::Coordinate(backend_present::Coordinate::new(coordinate));
    let reply = session
        .source(coordinate)
        .map_err(|error| Fault::from_client_error(&error, operand))?;
    let (CommandReply::Document(document) | CommandReply::Page(document)) = reply.reply else {
        return Err(shape("source"));
    };
    Ok(Answer::Page(Box::new(page_from_document(
        coordinate,
        &document,
        &[],
        &[],
        Vec::new(),
    ))))
}

fn neighbourhood(session: &mut Session, coordinate: &str, incoming: bool) -> Result<Answer, Fault> {
    let operand = Operand::Coordinate(backend_present::Coordinate::new(coordinate));
    let reply = if incoming {
        session.related(coordinate)
    } else {
        session.graph(coordinate)
    }
    .map_err(|error| Fault::from_client_error(&error, operand))?;
    let CommandReply::Graph(snapshot) = reply.reply else {
        return Err(shape("graph"));
    };
    Ok(Answer::Records(Box::new(record_list(
        coordinate, &snapshot,
    ))))
}

fn outline(session: &mut Session, path: &str) -> Result<Answer, Fault> {
    let operand = Operand::Path(path.to_owned());
    let reply = session
        .outline(path)
        .map_err(|error| Fault::from_client_error(&error, operand.clone()))?;
    let CommandReply::Outline(outline) = reply.reply else {
        return Err(shape("outline"));
    };
    let rows = outline_rows(session, path).map_err(|error| Fault::from_client_error(&error, operand))?;
    Ok(Answer::Outline(Box::new(outline_tree(path, &outline, &rows))))
}

fn surface(session: &mut Session, command: &SurfaceCommand) -> Result<Answer, Fault> {
    let reply = session
        .surface(command.clone())
        .map_err(|error| Fault::from_client_error(&error, Operand::Whole))?;
    Ok(Answer::Product(Box::new(product_view(&reply))))
}

fn search_snapshot(
    session: &mut Session,
    text: &str,
    limit: u16,
) -> Result<ViewSnapshot, Fault> {
    let reply = session
        .search(text, limit)
        .map_err(|error| Fault::from_client_error(&error, Operand::Text(text.to_owned())))?;
    match reply.reply {
        CommandReply::Search(snapshot) | CommandReply::Names(snapshot) => Ok(snapshot),
        _ => Err(shape("search")),
    }
}

fn names_snapshot(session: &mut Session, text: &str, limit: u16) -> Result<ViewSnapshot, Fault> {
    let reply = session
        .names(text, limit)
        .map_err(|error| Fault::from_client_error(&error, Operand::Text(text.to_owned())))?;
    match reply.reply {
        CommandReply::Names(snapshot) | CommandReply::Search(snapshot) => Ok(snapshot),
        _ => Err(shape("resolve")),
    }
}

/// Fetches the rows around one declaration, recording a note if it cannot.
fn probe_rows(
    session: &mut Session,
    coordinate: &str,
    incoming: bool,
    notes: &mut Vec<Fault>,
) -> Vec<Row> {
    let reply = if incoming {
        session.related(coordinate)
    } else {
        session.graph(coordinate)
    };
    match reply.map(|reply| reply.reply) {
        Ok(CommandReply::Graph(snapshot)) => snapshot.root.rows().to_vec(),
        Ok(_) => {
            notes.push(shape("related"));
            Vec::new()
        }
        Err(error) => {
            notes.push(
                Fault::from_client_error(
                    &error,
                    Operand::Coordinate(backend_present::Coordinate::new(coordinate)),
                )
                .with_affordance(backend_present::Affordance::UseCommand {
                    name: "related",
                    args: vec![coordinate.to_owned()].into_boxed_slice(),
                }),
            );
            Vec::new()
        }
    }
}

/// Fetches the owning package's flat outline so members can be named.
fn probe_members(session: &mut Session, coordinate: &str, notes: &mut Vec<Fault>) -> Vec<Row> {
    let Some(project) = Identity::parse(coordinate).project().cloned() else {
        return Vec::new();
    };
    match outline_rows(session, project.root()) {
        Ok(rows) => rows,
        Err(error) => {
            notes.push(
                Fault::from_client_error(&error, Operand::Path(project.root().to_owned()))
                    .with_affordance(backend_present::Affordance::UseCommand {
                        name: "outline",
                        args: vec![project.root().to_owned()].into_boxed_slice(),
                    }),
            );
            Vec::new()
        }
    }
}

fn outline_rows(session: &mut Session, path: &str) -> Result<Vec<Row>, ClientError> {
    let reply = session.outline_page(path, PROBE_ROWS, None)?;
    match reply.reply {
        CommandReply::ProjectionPage(page) => Ok(page.snapshot.root.rows().to_vec()),
        CommandReply::Packages(snapshot)
        | CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => Ok(snapshot.root.rows().to_vec()),
        _ => Err(ClientError::Protocol(
            "outline page reply changed shape".to_owned(),
        )),
    }
}

fn shape(what: &str) -> Fault {
    Fault::from_client_error(
        &ClientError::Protocol(format!("the {what} reply changed shape")),
        Operand::Text(what.to_owned()),
    )
}

/// Returns the exact revision an answer was read at, for a caption.
#[must_use]
pub fn revision_tag(session: &mut Session) -> Option<String> {
    session
        .revision()
        .ok()
        .map(|revision| encode_id(revision.root.as_bytes()))
}
