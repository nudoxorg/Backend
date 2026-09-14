use super::{CursorWire, SymbolAddressWire, revision_from_wire};
use crate::{
    PageContinuation, PageRequest, QueryLimit, WireCertificate, encode_id,
    wire::{cursor_from_wire, cursor_to_wire},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PageRequestWire {
    pub(crate) basis: String,
    pub(crate) limit: u16,
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
    let mut page = PageRequest::new(revision_from_wire(certificate, &value.basis)?, limit);
    if let Some(cursor) = value.continuation {
        page = page.with_continuation(PageContinuation::from_cursor(cursor_from_wire(
            &cursor,
            certificate,
        )?));
    }
    Ok(page)
}
