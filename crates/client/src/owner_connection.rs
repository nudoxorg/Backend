//! One bounded owner-connection policy shared by command-bearing surfaces.

use crate::{Backoff, ClientError, RequestContext};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Reaches an authenticated owner before any product command is submitted.
///
/// `dial` authenticates the existing listener. Only a refused or missing
/// endpoint authorizes `bootstrap`; a dying peer is observed again, while a
/// slow owner, authority refusal or protocol failure is returned unchanged.
/// Bootstrap failures are returned unchanged too: a startup I/O error is not
/// another dial proving that the owner disappeared.
/// This is the CLI's owner-replacement policy, including its existing pacing.
/// Every dial, bootstrap and pause retains the caller's original deadline and
/// cancellation. The callbacks must not submit or replay product commands.
///
/// # Errors
/// Returns a non-connection-loss failure, caller cancellation, an exhausted
/// original deadline, or a protocol error when the caller supplied no bound.
pub fn connect_local_owner_with<T>(
    context: &RequestContext,
    endpoint: &Path,
    mut dial: impl FnMut(&Path) -> Result<T, ClientError>,
    mut bootstrap: impl FnMut() -> Result<PathBuf, ClientError>,
) -> Result<T, ClientError> {
    context.remaining_read_budget()?.ok_or_else(|| {
        ClientError::Protocol(
            "owner replacement requires the original bounded request scope".to_owned(),
        )
    })?;
    let mut backoff = Backoff::new(Duration::from_millis(25), Duration::from_millis(500));
    loop {
        context.check()?;
        let outcome = match dial(endpoint) {
            Ok(session) => return Ok(session),
            Err(error) if error.is_owner_absent() => {
                context.check()?;
                let ready = bootstrap()?;
                dial(&ready)
            }
            Err(error) => Err(error),
        };
        match outcome {
            Ok(session) => return Ok(session),
            Err(error) if error.is_connection_lost() => pause(&mut backoff, context)?,
            Err(error) => return Err(error),
        }
    }
}

fn pause(backoff: &mut Backoff, scope: &RequestContext) -> Result<(), ClientError> {
    let step = backoff.step();
    let started = Instant::now();
    loop {
        let remaining = scope.remaining_read_budget()?.ok_or_else(|| {
            ClientError::Protocol(
                "owner replacement requires the original bounded request scope".to_owned(),
            )
        })?;
        let left = step.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Ok(());
        }
        std::thread::sleep(left.min(remaining).min(Duration::from_millis(25)));
    }
}
