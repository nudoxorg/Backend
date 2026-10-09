//! Bounded rendering for typed service diagnostics.

use std::fmt::Write as _;

pub(super) const MAX_LOCAL_COMPILE_ERROR_BYTES: usize = backend_library::MAX_PRODUCT_TEXT_BYTES;
pub(super) const MAX_LOCAL_COMPILE_ERROR_CAUSES: usize = 12;
pub(super) const MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES: usize = 1_024;

/// Renders a bounded typed source chain after a stable operation-specific prefix.
///
/// Distinct typed causes follow the prefix, with a fixed byte and depth budget so unusually
/// verbose errors cannot grow a reply without bound.
pub(super) fn bounded_error_chain(prefix: &str, error: &dyn std::error::Error) -> String {
    const CAUSE_PREFIX: &str = "\ncaused by: ";

    let first = bounded_error_display(error, MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES);
    let mut previous = first.clone();
    let mut source = error.source();
    let mut seen: [Option<&dyn std::error::Error>; MAX_LOCAL_COMPILE_ERROR_CAUSES + 1] =
        [None; MAX_LOCAL_COMPILE_ERROR_CAUSES + 1];
    seen[0] = Some(error);
    let mut seen_count = 1;
    let mut causes = Vec::new();
    let mut cycle_detected = false;
    for _ in 0..MAX_LOCAL_COMPILE_ERROR_CAUSES {
        let Some(cause) = source else {
            break;
        };
        if seen[..seen_count]
            .iter()
            .flatten()
            .any(|visited| std::ptr::eq(*visited, cause))
        {
            cycle_detected = true;
            break;
        }
        seen[seen_count] = Some(cause);
        seen_count += 1;

        let message = bounded_error_display(cause, MAX_LOCAL_COMPILE_CAUSE_MESSAGE_BYTES);
        if !previous.ends_with(&message) {
            causes.push(message.clone());
        }
        previous = message;
        source = cause.source();
    }
    let depth_truncated = if !cycle_detected {
        if let Some(next) = source {
            if seen[..seen_count]
                .iter()
                .flatten()
                .any(|visited| std::ptr::eq(*visited, next))
            {
                cycle_detected = true;
                false
            } else {
                true
            }
        } else {
            false
        }
    } else {
        false
    };
    let status = if cycle_detected {
        Some("\nerror cause chain cycle detected".to_owned())
    } else if depth_truncated {
        Some(format!(
            "\nadditional causes omitted after depth limit {MAX_LOCAL_COMPILE_ERROR_CAUSES}"
        ))
    } else {
        None
    };

    let base_bytes = prefix.len() + first.len();
    let cause_bytes = causes
        .iter()
        .map(|cause| CAUSE_PREFIX.len() + cause.len())
        .sum::<usize>();
    let status_bytes = status.as_ref().map_or(0, String::len);
    let mut output = BoundedDiagnosticText::new(MAX_LOCAL_COMPILE_ERROR_BYTES);
    let _ = write!(&mut output, "{prefix}{first}");

    if base_bytes + cause_bytes + status_bytes <= MAX_LOCAL_COMPILE_ERROR_BYTES {
        for cause in &causes {
            let _ = write!(&mut output, "{CAUSE_PREFIX}{cause}");
        }
    } else if let Some(deepest) = causes.last() {
        let middle = &causes[..causes.len() - 1];
        let deepest_bytes = CAUSE_PREFIX.len() + deepest.len();
        let reserved_tail = deepest_bytes + status_bytes;
        let omission_marker_bytes =
            format!("\nintermediate causes omitted: {}", middle.len()).len();
        let mut used_bytes = base_bytes;
        let mut included = 0;
        for cause in middle {
            let cause_bytes = CAUSE_PREFIX.len() + cause.len();
            if used_bytes + cause_bytes + omission_marker_bytes + reserved_tail
                > MAX_LOCAL_COMPILE_ERROR_BYTES
            {
                break;
            }
            let _ = write!(&mut output, "{CAUSE_PREFIX}{cause}");
            used_bytes += cause_bytes;
            included += 1;
        }
        let omitted = middle.len() - included;
        if omitted > 0 {
            let _ = write!(&mut output, "\nintermediate causes omitted: {omitted}");
        }
        let _ = write!(&mut output, "{CAUSE_PREFIX}{deepest}");
    }
    if let Some(status) = status {
        let _ = write!(&mut output, "{status}");
    }
    output.finish()
}

fn bounded_error_display(error: &dyn std::fmt::Display, maximum_bytes: usize) -> String {
    let mut output = BoundedDiagnosticText::new(maximum_bytes);
    let _ = write!(&mut output, "{error}");
    output.finish()
}

struct BoundedDiagnosticText {
    text: String,
    maximum_bytes: usize,
    truncated: bool,
}

impl BoundedDiagnosticText {
    fn new(maximum_bytes: usize) -> Self {
        Self {
            text: String::new(),
            maximum_bytes,
            truncated: false,
        }
    }

    fn finish(self) -> String {
        self.text
    }

    fn push_sanitized(&mut self, value: &str) {
        for character in value.chars() {
            self.text
                .push(if character == '\0' { ' ' } else { character });
        }
    }
}

impl std::fmt::Write for BoundedDiagnosticText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        if self.truncated {
            return Err(std::fmt::Error);
        }
        const TRUNCATION_MARKER: &str = "…";
        let remaining = self.maximum_bytes.saturating_sub(self.text.len());
        if value.len() <= remaining {
            self.push_sanitized(value);
            return Ok(());
        }
        let content_budget = remaining.saturating_sub(TRUNCATION_MARKER.len());

        let mut boundary = value.len().min(content_budget);
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        self.push_sanitized(&value[..boundary]);
        if remaining >= TRUNCATION_MARKER.len() {
            self.text.push_str(TRUNCATION_MARKER);
        }
        self.truncated = true;
        Err(std::fmt::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_LOCAL_COMPILE_ERROR_BYTES, bounded_error_chain};
    use backend_engine::application::{
        LocalCompilerHostError, LocalCompilerOpenError, LocalCompilerRuntimeOpenError,
    };
    use backend_store::journal::{JournalError, JournalIoStep, PublicationOpenError};
    use std::io;

    #[test]
    fn compiler_owner_diagnostic_retains_rooted_publication_journal_cause() {
        let error = LocalCompilerHostError::RuntimeOpen(LocalCompilerRuntimeOpenError::Compiler(
            LocalCompilerOpenError::Publisher(PublicationOpenError::Journal(JournalError::Io {
                step: JournalIoStep::Open,
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "publication journal root cause sentinel",
                ),
            })),
        ));

        let detail = bounded_error_chain("open compiler owner: ", &error);

        assert!(detail.starts_with(
            "open compiler owner: could not create local durable compiler publication owner"
        ));
        assert!(detail.contains("caused by: publication journal could not be opened"));
        assert!(detail.contains("caused by: journal I/O failed during Open"));
        assert!(detail.ends_with("publication journal root cause sentinel"));
        assert!(detail.len() <= MAX_LOCAL_COMPILE_ERROR_BYTES);
        assert!(backend_library::ProductText::new(detail).is_ok());
    }
}
