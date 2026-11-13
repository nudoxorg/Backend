//! Private bridge from a Vue source host to a supervised Stylo fragment run.
//! This does not publish native facts or certify Vue scoped/module transforms.
use super::stylo_worker::{
    StyloWorkerFailure, StyloWorkerPermit, VerifiedStyloFragmentRun, run_stylo_fragment_worker,
};
use backend_semantic::stylo_analysis::{StyloParseConfig, StyloSourceKind};
use std::{path::PathBuf, sync::atomic::AtomicBool, time::Duration};

/// Parse a selected Vue host, admit only inline plain CSS, then run the pinned
/// native worker with the complete host and checked content range.
pub(super) fn capture_vue_style<'a>(
    permit: &StyloWorkerPermit,
    cwd: PathBuf,
    bytes: &'a [u8],
    block: usize,
    config: &StyloParseConfig,
    wall_time: Duration,
    memory_limit: Option<usize>,
    cancellation: &AtomicBool,
) -> Result<VerifiedStyloFragmentRun<'a>, StyloWorkerFailure> {
    if config.source_kind != StyloSourceKind::EmbeddedUnicodeFragment {
        return Err(StyloWorkerFailure::Protocol);
    }
    let vue = backend_frontend_vue::parse(bytes).map_err(|_| StyloWorkerFailure::Protocol)?;
    let fragment = vue
        .inline_css(block)
        .map_err(|_| StyloWorkerFailure::Protocol)?;
    run_stylo_fragment_worker(
        permit,
        cwd,
        vue.host(),
        fragment.byte_range(),
        config,
        wall_time,
        memory_limit,
        cancellation,
    )
}
