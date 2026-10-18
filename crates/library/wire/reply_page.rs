use super::reply::snapshot_from_wire_with_admission;
use super::reply_admission::CoverageAdmission;
use super::{CursorWire, EmptyWire, SnapshotWire, WireCertificate, cursor_from_wire};
use crate::{PageContinuation, PageTerminal, ProjectionPage};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionPageWire {
    pub(crate) snapshot: SnapshotWire,
    pub(crate) terminal: PageTerminalWire,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) outline_order: Option<OutlinePageOrderWire>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutlinePageOrderWire {
    pub(crate) start: u64,
    pub(crate) symbols: Vec<String>,
}

pub(crate) fn order_to_wire(order: &crate::OutlinePageOrder) -> OutlinePageOrderWire {
    OutlinePageOrderWire {
        start: order.start(),
        symbols: order.symbols().iter().map(|id| crate::encode_id(id.as_bytes())).collect(),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PageTerminalWire {
    Complete(EmptyWire),
    More(CursorWire),
    Cancelled(EmptyWire),
}

pub(crate) fn page_from_wire<A: CoverageAdmission>(
    value: ProjectionPageWire,
    certificate: &WireCertificate,
    admission: &A,
) -> Result<ProjectionPage, String> {
    let snapshot = snapshot_from_wire_with_admission(value.snapshot, certificate, admission)?;
    let terminal = match value.terminal {
        PageTerminalWire::Complete(_) if snapshot.next.is_none() => PageTerminal::Complete,
        PageTerminalWire::More(value) => {
            let cursor = cursor_from_wire(&value, certificate)?;
            if snapshot.next != Some(cursor) {
                return Err("page terminal continuation does not match snapshot".to_owned());
            }
            PageTerminal::More(PageContinuation::from_cursor(cursor))
        }
        PageTerminalWire::Cancelled(_) if snapshot.next.is_none() => PageTerminal::Cancelled,
        PageTerminalWire::Complete(_) | PageTerminalWire::Cancelled(_) => {
            return Err("terminal page carries a continuation".to_owned());
        }
    };
    let outline_order = value.outline_order.map(|order| {
        if u64::try_from(order.symbols.len()).ok() != Some(snapshot.root.row_count())
            || order.symbols.len() > usize::from(crate::QueryLimit::MAX)
        { return Err("outline order exceeds its exact row count".to_owned()); }
        // Only already-admitted snapshot identities can discharge these wire
        // operands; arbitrary digests are never promoted to SymbolKey.
        let admitted = snapshot.root.rows().iter().filter_map(|row| match row.id {
            crate::RowId::Symbol(id) => Some((crate::encode_id(id.as_bytes()), id)), _ => None,
        }).collect::<std::collections::BTreeMap<_, _>>();
        let symbols = order.symbols.iter().map(|id| admitted.get(id).copied()
            .ok_or_else(|| "outline order names a foreign identity".to_owned()))
            .collect::<Result<Vec<_>, _>>()?;
        crate::OutlinePageOrder::new(order.start, symbols)
    }).transpose()?;
    Ok(ProjectionPage { snapshot, terminal, outline_order })
}
