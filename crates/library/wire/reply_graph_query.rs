use super::command::{GraphValueWire, value_from_wire, value_to_wire};
use super::reply_admission::CoverageAdmission;
use super::reply_page::PageTerminalWire;
use super::{
    EmptyWire, WireCertificate, WireSchema, cursor_from_wire_with_capability, cursor_to_wire,
};
use crate::canonical::ObjectSchema;
use crate::{
    GraphQueryPage, GraphQueryRow, PageContinuation, PageTerminal, ViewRelation, ViewRevision,
    encode_id,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphQueryPageWire {
    root: String,
    source: String,
    rows: Vec<GraphQueryRowWire>,
    terminal: PageTerminalWire,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphQueryRowWire {
    fields: BTreeMap<String, GraphValueWire>,
}

pub(crate) fn page_to_wire(page: &GraphQueryPage) -> GraphQueryPageWire {
    GraphQueryPageWire {
        root: encode_id(page.revision.as_bytes()),
        source: encode_id(page.source.as_bytes()),
        rows: page
            .rows
            .iter()
            .map(|row| GraphQueryRowWire {
                fields: row
                    .fields()
                    .iter()
                    .map(|(name, value)| (name.clone(), value_to_wire(value)))
                    .collect(),
            })
            .collect(),
        terminal: match page.terminal {
            PageTerminal::Complete => PageTerminalWire::Complete(EmptyWire {}),
            PageTerminal::More(continuation) => {
                PageTerminalWire::More(cursor_to_wire(continuation.cursor()))
            }
            PageTerminal::Cancelled => PageTerminalWire::Cancelled(EmptyWire {}),
        },
    }
}

pub(crate) fn page_from_wire<A: CoverageAdmission>(
    value: GraphQueryPageWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<GraphQueryPage, String> {
    let Some(capability) = admission.admit(certificate, &value.source)? else {
        return Err("graph query page requires admitted producer coverage".to_owned());
    };
    let source = certificate.version_value::<ObjectSchema>(WireSchema::Object, &value.source)?;
    if capability.scope_root().as_bytes() != source.as_bytes() {
        return Err("graph query page source does not match producer coverage".to_owned());
    }
    let root = certificate.producer_root_value::<ViewRelation>(
        WireSchema::ViewRelation,
        &value.root,
        &capability,
    )?;
    let terminal = terminal_from_wire(value.terminal, certificate, &capability)?;
    let rows = value
        .rows
        .into_iter()
        .map(row_from_wire)
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    Ok(GraphQueryPage {
        revision: ViewRevision::from(root),
        source,
        rows,
        terminal,
    })
}

fn row_from_wire(value: GraphQueryRowWire) -> Result<GraphQueryRow, String> {
    let fields = value
        .fields
        .into_iter()
        .map(|(name, value)| value_from_wire(value).map(|value| (name, value)))
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    GraphQueryRow::new(fields).map_err(|error| error.to_string())
}

fn terminal_from_wire(
    terminal: PageTerminalWire,
    certificate: &WireCertificate,
    capability: &crate::CoverageCapability,
) -> Result<PageTerminal, String> {
    Ok(match terminal {
        PageTerminalWire::Complete(_) => PageTerminal::Complete,
        PageTerminalWire::Cancelled(_) => PageTerminal::Cancelled,
        PageTerminalWire::More(value) => PageTerminal::More(PageContinuation::from_cursor(
            cursor_from_wire_with_capability(&value, certificate, capability)?,
        )),
    })
}
