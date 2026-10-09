//! In your workspace: the places your packages name the declaration.
//!
//! The page carries them: the index gives a relation and a byte span for each
//! use, and the read worker put the line at that span, from your own file, on
//! the page when it read it (`runtime::workspace_lines`). This maps those
//! lines to the facts `facet::anatomy::symbol::derive::uses` reads what each
//! one does from. Nothing is read here, so the page never lands without its
//! places and there is no waiting state to draw.

use crate::model::pages::{Known, Resolution, SymbolPage, UseLine};
use backend_library::SemanticLinkKind;
use facet::anatomy::symbol::derive::uses::{Reader, Rel, Site, read_all};
use facet::anatomy::symbol::view::{UseEvidence, Uses};

const fn rel(kind: SemanticLinkKind) -> Rel {
    match kind {
        SemanticLinkKind::Calls => Rel::Calls,
        SemanticLinkKind::MethodCall => Rel::MethodCall,
        SemanticLinkKind::TypeReference => Rel::TypeReference,
        SemanticLinkKind::Reads => Rel::Reads,
        SemanticLinkKind::Writes => Rel::Writes,
        SemanticLinkKind::Imports => Rel::Imports,
        SemanticLinkKind::Implements => Rel::Implements,
        SemanticLinkKind::Overrides => Rel::Overrides,
        SemanticLinkKind::Reexports => Rel::Reexports,
        SemanticLinkKind::Inherits => Rel::Inherits,
        SemanticLinkKind::Documents => Rel::Documents,
    }
}

fn site(line: &UseLine) -> Site {
    Site {
        package: line.package.to_string(),
        file: line.file.to_string(),
        path: line.path.to_string(),
        line: line.line,
        text: line.text.to_string(),
        mark: line.mark.as_ref().map(|mark| mark.start as usize..mark.end as usize),
        rel: rel(line.relation),
        exact: line.resolution == Resolution::Resolved,
    }
}

/// The places `page` carries, in the order the index gave them.
fn sites(page: &SymbolPage) -> Vec<Site> {
    page.workspace.iter().map(site).collect()
}

/// A bounded successful reference list is an observation, not complete use
/// coverage. Keep failed reads and source-line omissions distinguishable from
/// a successfully returned zero, including the early page publication.
fn evidence(page: &SymbolPage) -> UseEvidence {
    match &page.references {
        Known::Unknown(_) => UseEvidence::Unavailable,
        Known::Known(references) => match references.coverage() {
            backend_library::ReferenceCoverage::Unestablished => UseEvidence::Reported {
                reported: references.reported_count(),
                readable: page.workspace.len(),
            },
        },
    }
}

pub(super) fn workspace(page: &SymbolPage, reader: &Reader) -> Uses {
    let mut uses = read_all(&sites(page), reader);
    uses.evidence = evidence(page);
    uses
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn unavailable_zero_and_unreadable_replies_cannot_prove_no_fastapi_callers() {
        use crate::model::pages::{GapReason, ReferenceObservation, ReferenceScope, ReferenceSite};
        use crate::shell::bodies::symbol::page_tests::{decl, page};
        use backend_library::{DeclarationKind, SemanticConfidence};
        let mut page = page(decl("backend/app/core/security.py", 22, "create_access_token", DeclarationKind::Function),
            "def create_access_token(subject: str | Any, expires_delta: timedelta) -> str:", None, vec![], vec![], vec![]);
        page.references = Known::unknown(GapReason::NoSemanticPublication, "1937 fact groups unavailable");
        assert_eq!(evidence(&page), UseEvidence::Unavailable);
        page.references = Known::Known(ReferenceObservation::new(Arc::from([])));
        assert_eq!(evidence(&page), UseEvidence::Reported { reported: 0, readable: 0 });
        // The exact pinned caller is known in source, but a reference whose
        // span cannot be read is retained in the observation, never erased.
        page.references = Known::Known(ReferenceObservation::new(Arc::from([ReferenceSite {
            site: decl("backend/app/api/routes/login.py", 24, "login_access_token", DeclarationKind::Function),
            relation: SemanticLinkKind::Calls,
            confidence: SemanticConfidence::Compiler,
            span: Known::unknown(GapReason::NotCaptured, "source span unavailable"),
            scope: ReferenceScope::Local,
        }])));
        assert_eq!(evidence(&page), UseEvidence::Reported { reported: 1, readable: 0 });
        page.workspace = Arc::from([UseLine {
            package: Arc::from("fastapi-full-stack"), file: Arc::from("backend/app/api/routes/login.py"),
            path: Arc::from("/fixture/fastapi-full-stack/backend/app/api/routes/login.py"), line: 39,
            text: Arc::from("        access_token=security.create_access_token("), mark: Some(30..49),
            relation: SemanticLinkKind::Calls, resolution: Resolution::Resolved,
        }]);
        assert_eq!(evidence(&page), UseEvidence::Reported { reported: 1, readable: 1 });
    }

    #[test]
    fn a_line_on_the_page_is_a_site_with_its_token_and_how_sure_the_index_was() {
        let line = UseLine {
            package: Arc::from("engine"),
            file: Arc::from("src/a.rs"),
            path: Arc::from("/w/engine/src/a.rs"),
            line: 12,
            text: Arc::from("let v = Value::Null;"),
            mark: Some(8..13),
            relation: SemanticLinkKind::TypeReference,
            resolution: Resolution::ByName,
        };
        let site = site(&line);
        assert_eq!((site.package.as_str(), site.line, site.rel, site.exact), ("engine", 12, Rel::TypeReference, false));
        assert_eq!(site.mark.map(|mark| line.text[mark].to_owned()), Some("Value".to_owned()));
    }
}
