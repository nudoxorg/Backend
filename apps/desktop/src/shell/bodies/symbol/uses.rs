//! In your workspace: the places your packages name the declaration.
//!
//! The page carries them: the index gives a relation and a byte span for each
//! use, and the read worker put the line at that span, from your own file, on
//! the page when it read it (`runtime::workspace_lines`). This maps those
//! lines to the facts `facet::anatomy::symbol::derive::uses` reads what each
//! one does from. Nothing is read here, so the page never lands without its
//! places and there is no waiting state to draw.

use crate::model::pages::{Resolution, SymbolPage, UseLine};
use backend_library::SemanticLinkKind;
use facet::anatomy::symbol::derive::uses::{Rel, Site};

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
        mark: line
            .mark
            .as_ref()
            .map(|mark| mark.start as usize..mark.end as usize),
        rel: rel(line.relation),
        exact: line.resolution == Resolution::Resolved,
    }
}

/// The places `page` carries, in the order the index gave them.
pub(super) fn sites(page: &SymbolPage) -> Vec<Site> {
    page.workspace.iter().map(site).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

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
        assert_eq!(
            (site.package.as_str(), site.line, site.rel, site.exact),
            ("engine", 12, Rel::TypeReference, false)
        );
        assert_eq!(
            site.mark.map(|mark| line.text[mark].to_owned()),
            Some("Value".to_owned())
        );
    }
}
