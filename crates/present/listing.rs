//! The member list of one declaration page: its one-line rows, and the window
//! of them a response carries.
//!
//! "What members does this type have?" is the most-asked question about a
//! declaration, and a type can have hundreds. A page therefore shows every
//! member as one compact line (a handle that names it, then its short
//! signature) up to a budget, and says how many remain and how to page to
//! them. The same window drives the readable text and the typed projection, so
//! the two can never disagree about which rows a response carries.

use crate::dto::{IdentityDto, RecordDto};
use crate::fault::Fault;
use crate::identity::{Identity, IdentityShape};
use crate::page::{Listing, Member, MemberGroup, Page, Source};
use crate::window::{Window, WindowKind, WindowRequest, fingerprint, one_line};
use backend_library::DeclarationKind;
use std::collections::BTreeMap;

/// Longest signature, in characters, one list row spells out.
pub(crate) const ROW_SIGNATURE_CHARS: usize = 140;

/// Longest documentation sentence, in characters, one typed row carries.
pub(crate) const ROW_SUMMARY_CHARS: usize = 120;

/// Longest signature a typed row carries, in characters. A signature of a
/// generated declaration can run to kilobytes; the full text stays one
/// `show` away.
pub(crate) const TYPED_SIGNATURE_CHARS: usize = 600;

/// The shortest spelling that selects one declaration again.
///
/// A declaration with a site reads `path:line::name`, which every
/// coordinate-taking command accepts as an operand without the project
/// root a full coordinate repeats on every line. An external placeholder
/// reads as its display name: its content-addressed coordinate says nothing a
/// reader can use.
#[must_use]
pub fn handle(identity: &Identity) -> String {
    match identity.shape() {
        IdentityShape::External => {
            let name = identity.name();
            if name.is_empty() {
                identity.coordinate().as_str().to_owned()
            } else {
                format!("{name} (external)")
            }
        }
        IdentityShape::Declaration | IdentityShape::Semantic => {
            match (identity.path(), identity.line()) {
                (Some(path), Some(line)) => {
                    format!("{path}:{line}::{}", identity.trail().joined())
                }
                (Some(path), None) => format!("{path}::{}", identity.trail().joined()),
                // A compiler-addressed declaration with no captured site: its
                // name is all there is, and the project is the context.
                _ => identity.trail().joined(),
            }
        }
        // A file is named by its path alone; the project is the context.
        IdentityShape::Module => identity
            .path()
            .map_or_else(|| identity.trail_within(None), |path| path.as_str().to_owned()),
        IdentityShape::Package | IdentityShape::Opaque => identity.trail_within(None),
    }
}

/// One member as one line: its handle, then its short signature.
#[must_use]
pub(crate) fn member_line(member: &Member) -> String {
    let mut line = handle(member.identity());
    if let Some(signature) = member.signature() {
        let text = one_line(&signature.text(), ROW_SIGNATURE_CHARS);
        if !text.is_empty() {
            line.push_str("  ");
            line.push_str(&text);
        }
    }
    line
}

/// One member as the typed projection carries it.
///
/// Summary and standard rows leave out the two identity fields a list of
/// hundreds cannot afford to repeat: the owning project (the prefix of the
/// coordinate) and the exact selected-symbol operand (what `show` of the row
/// returns). `full` keeps every field, and the response ladder lowers the
/// detail rather than fail when that does not fit.
#[must_use]
pub(crate) fn member_dto(member: &Member, with_detail: bool, full_identity: bool) -> RecordDto {
    RecordDto {
        identity: if full_identity {
            IdentityDto::new(member.identity())
        } else {
            IdentityDto::listed(member.identity())
        },
        kind: member.kind().map(|kind| kind.name().to_owned()),
        language: None,
        state: None,
        signature: with_detail
            .then(|| member.signature())
            .flatten()
            .map(|signature| one_line(&signature.text(), TYPED_SIGNATURE_CHARS)),
        summary: with_detail
            .then(|| member.summary())
            .flatten()
            .map(|summary| one_line(summary, ROW_SUMMARY_CHARS)),
        score: None,
    }
}

fn typed_bytes(member: &Member) -> usize {
    serde_json::to_vec(&member_dto(member, true, false)).map_or(usize::MAX / 4, |bytes| bytes.len() + 1)
}

/// Cuts a page's members to one window and records how the list was cut.
///
/// # Errors
///
/// Returns the stale-cursor fault when `request` resumes a list that has
/// changed, and a usage fault when its cursor is malformed or past the end.
pub(crate) fn window_members(page: Page, request: WindowRequest) -> Result<Page, Fault> {
    let members = page
        .members()
        .iter()
        .flat_map(MemberGroup::members)
        .cloned()
        .collect::<Vec<_>>();
    let total = members.len();
    let print = fingerprint(
        members
            .iter()
            .map(|member| member.identity().coordinate().as_str()),
    );
    let start = request.start(WindowKind::Members, print, total)?;
    let text = members
        .iter()
        .map(|member| member_line(member).len().saturating_add(3))
        .collect::<Vec<_>>();
    let typed = members.iter().map(typed_bytes).collect::<Vec<_>>();
    let window = Window::select(WindowKind::Members, request, start, print, &text, &typed);
    let mut kinds = BTreeMap::<u8, (DeclarationKind, usize)>::new();
    for member in &members {
        let kind = member.kind().unwrap_or(DeclarationKind::Unknown);
        kinds.entry(kind.wire_tag()).or_insert((kind, 0)).1 += 1;
    }
    let shown = members[window.range()].to_vec();
    let listing = Listing::new(window, kinds.into_values().collect());
    Ok(page
        .with_members(MemberGroup::group(shown))
        .with_listing(listing))
}

/// Text bytes a window of source lines spends when the caller named no limit.
const SOURCE_TEXT_BUDGET: usize = 12 * 1024;

/// Cuts the captured source of a `source` answer to one window of its lines.
///
/// The text of a declaration can run to a megabyte; a response carries as many
/// whole lines as fit its budget and says how to read on.
///
/// # Errors
///
/// Returns the stale-cursor fault when the caller resumes source that has
/// changed, and a usage fault when its cursor is malformed.
pub(crate) fn window_source(page: Page, request: WindowRequest) -> Result<Page, Fault> {
    let Source::Captured {
        site,
        lines,
        truncation,
    } = page.source()
    else {
        return Ok(page);
    };
    let print = fingerprint(
        std::iter::once(site.path().as_str()).chain(lines.iter().map(crate::page::SourceLine::text)),
    );
    let start = request.start(WindowKind::Source, print, lines.len())?;
    let costs = lines
        .iter()
        .map(|line| line.text().len().saturating_add(6))
        .collect::<Vec<_>>();
    let window = Window::select_within(
        WindowKind::Source,
        request,
        start,
        print,
        &costs,
        &costs,
        SOURCE_TEXT_BUDGET,
    );
    let cut = Source::Captured {
        site: site.clone(),
        lines: lines[window.range()].to_vec().into_boxed_slice(),
        truncation: *truncation,
    };
    Ok(page.with_source(cut).with_listing(Listing::new(window, Vec::new())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn a_declaration_is_named_by_its_site_and_a_placeholder_by_its_name() {
        let declared = Identity::parse("/abs/p::src/color.go:278::Color::Add");
        assert_eq!(handle(&declared), "src/color.go:278::Color::Add");
        let external = Identity::parse(&format!("/abs/p::external::symbol:{}::Writer", "ab".repeat(32)));
        assert_eq!(handle(&external), "Writer (external)");
        let module = Identity::parse("/abs/p::src/lib.rs");
        assert_eq!(handle(&module), "src/lib.rs");
    }
}
