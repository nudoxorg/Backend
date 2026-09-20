//! Typed command dispatch for the versioned catalog.

use super::Library;
use super::certificate::{producer_certificate_for_health, producer_certificate_for_revision};
use crate::{
    Command, CommandDto, CommandReply, DocumentQuery, LibraryError, NameQuery, QueryLimit,
    ReplyDto, Row,
};

impl Library {
    /// Executes one typed in-process command.
    ///
    /// # Errors
    ///
    /// Returns a bounded projection for the selected read command.
    pub fn execute(&self, command: Command) -> Result<CommandReply, LibraryError> {
        let basis = self.revision_root();
        match command {
            Command::Packages => {
                let page = self.arrangement.packages_page(usize::from(QueryLimit::MAX));
                let rows: Vec<Row> = page
                    .ids
                    .iter()
                    .filter_map(|&id| self.row_for_id(id))
                    .collect();
                self.work.record_seek();
                self.work.record_output(rows.len());
                self.snapshot_for(
                    b"packages",
                    rows,
                    page.has_more.then_some(self.cursor),
                    None,
                    page.has_more.then_some(usize::from(QueryLimit::MAX)),
                )
                .map(CommandReply::Packages)
            }
            Command::Add { .. } | Command::Remove { .. } => Ok(CommandReply::Error(
                "durable intents must be submitted through the engine owner".to_owned(),
            )),
            Command::Document(query) => self.document(query).map(CommandReply::Document),
            Command::Show { symbol } => self
                .document(DocumentQuery {
                    symbol,
                    basis: basis.into(),
                    source: None,
                })
                .map(CommandReply::Document),
            Command::Outline(query) => self.outline(query).map(CommandReply::Outline),
            Command::Name(query) => self.names(&query).map(CommandReply::Names),
            Command::Resolve { text } => self
                .names(&NameQuery::new(text, basis, QueryLimit::default()))
                .map(|snapshot| {
                    CommandReply::Resolved(snapshot.root.rows().to_vec().into_boxed_slice())
                }),
            Command::Search(query) => self.search(&query).map(CommandReply::Search),
            Command::Graph(query) => self.graph(query).map(CommandReply::Graph),
            Command::Health => Ok(CommandReply::Health((*self.view).clone())),
            Command::Revision => Ok(CommandReply::Revision(crate::RevisionReceipt::new(
                self.view.root(),
                self.cursor,
                self.view.basis().object,
            ))),
        }
    }

    /// Executes one command DTO while preserving request correlation.
    #[must_use]
    pub fn execute_dto(&self, request: CommandDto) -> ReplyDto {
        let request_id = request.request_id;
        let reply = match self.execute(request.command) {
            Ok(reply) => reply,
            Err(error) => CommandReply::Error(error.to_string()),
        };
        let dto = match reply {
            CommandReply::Health(root) => ReplyDto::health(request_id, root, self.cursor),
            CommandReply::Revision(receipt) => {
                ReplyDto::new(request_id, CommandReply::Revision(receipt))
            }
            reply => ReplyDto::new(request_id, reply),
        };
        match &dto.reply {
            CommandReply::Health(root) => dto
                .health_cursor()
                .and_then(|cursor| producer_certificate_for_health(root, cursor))
                .map_or(dto.clone(), |certificate| dto.with_certificate(certificate)),
            CommandReply::Revision(receipt) => {
                producer_certificate_for_revision(&self.view, receipt.cursor())
                    .map_or(dto.clone(), |certificate| dto.with_certificate(certificate))
            }
            _ => dto,
        }
    }
}
