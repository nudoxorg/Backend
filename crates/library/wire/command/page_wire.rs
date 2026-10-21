use super::{
    CursorWire, SymbolAddressWire, cursor_from_wire_against_catalog_owner, revision_from_wire,
};
use crate::{
    Cursor, PageContinuation, PageRequest, QueryLimit, WireCertificate, encode_id,
    wire::{cursor_from_wire, cursor_to_wire},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageRequestWire {
    pub(crate) basis: String,
    pub(crate) limit: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) window: Option<super::QueryWindowWire>,
    pub(crate) continuation: Option<CursorWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackagePageWire {
    pub(crate) package: String,
    pub(crate) page: PageRequestWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SymbolPageWire {
    pub(crate) symbol: SymbolAddressWire,
    pub(crate) page: PageRequestWire,
}

pub(crate) fn page_request_to_wire(page: PageRequest) -> PageRequestWire {
    PageRequestWire {
        basis: encode_id(page.basis().as_bytes()),
        limit: page.limit().get(),
        window: super::window_to_wire(page.window()),
        continuation: page
            .continuation()
            .map(|continuation| cursor_to_wire(continuation.cursor())),
    }
}

pub(crate) fn page_request_from_wire(
    value: PageRequestWire,
    certificate: &WireCertificate,
) -> Result<PageRequest, String> {
    let limit = QueryLimit::new(value.limit).ok_or_else(|| "invalid page limit".to_owned())?;
    let window = super::window_from_wire(value.window.as_ref(), limit)?;
    let mut page = PageRequest::new(revision_from_wire(certificate, &value.basis)?, limit)
        .with_window(window)
        .ok_or_else(|| "query window changed issued ceiling".to_owned())?;
    if let Some(cursor) = value.continuation {
        page = page.with_continuation(PageContinuation::from_cursor(cursor_from_wire(
            &cursor,
            certificate,
        )?));
    }
    Ok(page)
}

pub(crate) fn page_request_from_wire_against_catalog_owner(
    value: PageRequestWire,
    certificate: &WireCertificate,
    owner: Cursor,
) -> Result<PageRequest, String> {
    let limit = QueryLimit::new(value.limit).ok_or_else(|| "invalid page limit".to_owned())?;
    let window = super::window_from_wire(value.window.as_ref(), limit)?;
    let basis = revision_from_wire(certificate, &value.basis)?;
    if basis != owner.root() {
        return Err("page basis does not match the owner root".to_owned());
    }
    let mut page = PageRequest::new(basis, limit)
        .with_window(window)
        .ok_or_else(|| "query window changed issued ceiling".to_owned())?;
    if let Some(cursor) = value.continuation {
        page = page.with_continuation(PageContinuation::from_cursor(
            cursor_from_wire_against_catalog_owner(&cursor, certificate, owner)?,
        ));
    }
    Ok(page)
}
