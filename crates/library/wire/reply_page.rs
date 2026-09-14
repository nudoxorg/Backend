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
    Ok(ProjectionPage { snapshot, terminal })
}
