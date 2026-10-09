//! Exact-byte explanations of refused captured native configurations.
//!
//! The public native parser remains the admission authority. This helper runs
//! only after that parser has refused a file; it never supplies a replacement
//! config or removes settings to admit an otherwise refused project.

use std::collections::BTreeMap;
use std::path::Path;

use pyrefly_config::config::ConfigFile;
use pyrefly_config::error_kind::{ErrorKind, Severity};
use pyrefly_config::finder::ConfigError;
use pyrefly_config::pyproject::PyProject;
use serde::Deserialize;
use serde::de::value::StrDeserializer;

use backend_semantic::vocabulary::MAX_NATIVE_DIAGNOSTIC_BYTES;

use super::project::checkpoint;
use super::{CheckerError, PythonProjectControl};
use crate::legacy::Span;

/// Exact disposition of a refused captured native configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonProjectConfigurationFault {
    /// A diagnostic option does not exist in the pinned native compiler.
    UnsupportedDiagnosticOption {
        /// Complete configuration key, including the root/sub-config scope.
        option: Box<str>,
        /// Exact raw UTF-8 value spelling retained from the captured document.
        value: Box<str>,
        /// Exact raw byte range of that value, including BOM/CRLF bytes if present.
        value_span: Span,
    },
    /// The same native config type and TOML parser reject the exact input.
    InvalidNativeConfiguration {
        /// Bounded native parser cause, without native path context.
        message: Box<str>,
        /// Exact parser byte range, when provided by the TOML authority.
        span: Option<Span>,
    },
    /// A known native diagnostic option has an invalid configured severity.
    InvalidDiagnosticValue {
        /// Complete native option key.
        option: Box<str>,
        /// Exact captured value spelling.
        value: Box<str>,
        /// Exact raw byte range of that value.
        value_span: Span,
        /// Refusal under the pinned native severity deserializer.
        message: Box<str>,
    },
    /// Native parsing stopped before other fields could be validated. This
    /// original failure is retained alongside independently classified options.
    NativeParserRefusal {
        /// Bounded original native parser cause.
        message: Box<str>,
        /// Original native TOML error range, when available.
        span: Option<Span>,
    },
    /// Exact undecodable bytes in a captured UTF-8 configuration. TOML did not
    /// run; the actual native loader refusal is retained as a separate fault.
    InvalidConfigurationEncoding {
        /// Exact raw byte range of the UTF-8 decode rejection.
        span: Span,
    },
    /// The native file loader refused for a reason outside TOML deserialization.
    NativeLoaderRefusal {
        /// Native severity, without upgrading a warning to a parse error.
        severity: Box<str>,
        /// Bounded public native message, retained without reinterpretation.
        message: Box<str>,
    },
}

#[derive(Default, Deserialize)]
struct DiagnosticOptions {
    #[serde(default)]
    errors: BTreeMap<String, toml_native::Spanned<toml_native::Value>>,
    #[serde(default, rename = "sub-config", alias = "sub_config")]
    sub_configs: Vec<DiagnosticSettings>,
}

// Native sub-configs have diagnostic settings but cannot contain sub-configs.
#[derive(Deserialize)]
struct DiagnosticSettings {
    #[serde(default)]
    errors: BTreeMap<String, toml_native::Spanned<toml_native::Value>>,
}

#[derive(Deserialize)]
struct DiagnosticPyProject {
    #[serde(default)]
    tool: Option<DiagnosticTool>,
}

#[derive(Deserialize)]
struct DiagnosticTool {
    #[serde(default)]
    pyrefly: Option<DiagnosticOptions>,
}

fn span(range: std::ops::Range<usize>) -> Option<Span> {
    Some(Span {
        start: u32::try_from(range.start).ok()?,
        end: u32::try_from(range.end).ok()?,
    })
}

/// One existing compiler-failure byte allowance shared by all retained payloads.
/// Every retained fault also consumes one unit, bounding zero-length operands.
#[derive(Default)]
struct Evidence {
    faults: Vec<PythonProjectConfigurationFault>,
    used: usize,
    omitted: u32,
    truncated: bool,
}

impl Evidence {
    fn remaining(&self) -> usize {
        MAX_NATIVE_DIAGNOSTIC_BYTES.saturating_sub(self.used)
    }

    fn omit(&mut self) {
        // Captured documents are at most CONFIG_BYTES (1 MiB), so one fault
        // per key/loader error cannot exhaust this count.
        self.omitted += 1;
    }

    fn retain(&mut self, fault: PythonProjectConfigurationFault, bytes: usize) {
        self.used += bytes + 1;
        self.faults.push(fault);
    }

    fn message(&mut self, text: &str, maximum: usize) -> Box<str> {
        let mut end = text.len().min(maximum);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.truncated |= end != text.len();
        text[..end].into()
    }
}

fn diagnostic_faults(
    text: &str,
    pyproject: bool,
    evidence: &mut Evidence,
    control: PythonProjectControl<'_>,
) -> Result<(), CheckerError> {
    checkpoint(control)?;
    let options = if pyproject {
        toml_native::from_str::<DiagnosticPyProject>(text)
            .ok()
            .and_then(|document| document.tool.and_then(|tool| tool.pyrefly))
    } else {
        toml_native::from_str::<DiagnosticOptions>(text).ok()
    };
    checkpoint(control)?;
    let Some(options) = options else {
        return Ok(());
    };
    fn collect(
        errors: &BTreeMap<String, toml_native::Spanned<toml_native::Value>>,
        scope: &str,
        text: &str,
        evidence: &mut Evidence,
        control: PythonProjectControl<'_>,
    ) -> Result<(), CheckerError> {
        for (key, value) in errors {
            checkpoint(control)?;
            let kind = key.parse::<ErrorKind>();
            // The pinned native ErrorDisplayConfig visitor accepts booleans or
            // a native Severity string. Borrow the parsed value: an oversized
            // unknown array/string is never cloned into another JSON tree.
            let invalid =
                if kind.is_ok() {
                    match value.get_ref() {
                        toml_native::Value::Boolean(_) => None,
                        toml_native::Value::String(value) => Severity::deserialize(
                            StrDeserializer::<serde::de::value::Error>::new(value),
                        )
                        .err()
                        .map(|_| "invalid native severity string"),
                        _ => Some("expected string or boolean"),
                    }
                } else {
                    None
                };
            if kind.is_ok() && invalid.is_none() {
                continue;
            }
            let range = value.span();
            let (Some(value_span), Some(raw_value)) = (span(range.clone()), text.get(range)) else {
                evidence.omit();
                continue;
            };
            let message = invalid.unwrap_or("");
            let width = scope
                .len()
                .saturating_add("errors.".len())
                .saturating_add(key.len())
                .saturating_add(raw_value.len())
                .saturating_add(message.len());
            // Reserve one unit for the independent native parser refusal.
            // Omit a whole oversized operand; never pair clipped bytes with
            // the original exact span or imply that an omitted option passed.
            if width.saturating_add(2) > evidence.remaining() {
                evidence.omit();
                continue;
            }
            let option = format!("{scope}errors.{key}").into_boxed_str();
            let fault = if kind.is_err() {
                PythonProjectConfigurationFault::UnsupportedDiagnosticOption {
                    option,
                    value: raw_value.into(),
                    value_span,
                }
            } else {
                PythonProjectConfigurationFault::InvalidDiagnosticValue {
                    option,
                    value: raw_value.into(),
                    value_span,
                    message: message.into(),
                }
            };
            evidence.retain(fault, width);
        }
        Ok(())
    }
    collect(
        &options.errors,
        if pyproject { "tool.pyrefly." } else { "" },
        text,
        evidence,
        control,
    )?;
    for (index, sub_config) in options.sub_configs.iter().enumerate() {
        checkpoint(control)?;
        let prefix = if pyproject { "tool.pyrefly." } else { "" };
        collect(
            &sub_config.errors,
            &format!("{prefix}sub-config[{index}]."),
            text,
            evidence,
            control,
        )?;
    }
    checkpoint(control)
}

/// Explains a native loader refusal using the exact captured bytes. The result
/// remains a refusal; no config produced by this diagnostic helper is admitted.
/// Parsing is bounded by the existing captured-input allowance, and retained
/// evidence shares the existing compiler-failure allowance. Checkpoints surround
/// each finite parse and each retained option; native TOML itself has no callback.
pub(super) fn configuration_error(
    captured_path: &Path,
    original_path: &Path,
    errors: &[ConfigError],
    control: PythonProjectControl<'_>,
) -> Result<CheckerError, CheckerError> {
    checkpoint(control)?;
    let parent = captured_path
        .parent()
        .ok_or_else(|| super::project::project_error("", "captured configuration has no parent"))?;
    let name = captured_path.file_name().ok_or_else(|| {
        super::project::project_error("", "captured configuration has no filename")
    })?;
    // Reuse bounded, no-follow, same-open-file source capture rather than an
    // unbounded explanatory read after the native loader has refused.
    let bytes = super::project::read_configuration_input(parent, Path::new(name), control)?
        .ok_or_else(|| super::project::project_error("", "captured configuration disappeared"))?;
    let source = backend_semantic::ir::SourceIdentity::from_bytes(&bytes).ok_or_else(|| {
        super::project::project_error("", "captured config exceeds source extent")
    })?;
    let pyproject = name == ConfigFile::PYPROJECT_FILE_NAME;
    let is_toml = pyproject
        || ConfigFile::CONFIG_FILE_NAMES
            .iter()
            .any(|candidate| name == *candidate);
    let mut evidence = Evidence::default();
    if is_toml {
        match std::str::from_utf8(&bytes) {
            Ok(text) => {
                checkpoint(control)?;
                let native_parse_error = if pyproject {
                    toml_native::from_str::<PyProject>(text).err()
                } else {
                    toml_native::from_str::<ConfigFile>(text).err()
                };
                checkpoint(control)?;
                if let Some(error) = native_parse_error {
                    diagnostic_faults(text, pyproject, &mut evidence, control)?;
                    let message =
                        evidence.message(error.message(), evidence.remaining().saturating_sub(1));
                    let width = message.len();
                    let error_span = error.span().and_then(span);
                    let fault = if evidence.faults.is_empty() && evidence.omitted == 0 {
                        PythonProjectConfigurationFault::InvalidNativeConfiguration {
                            message,
                            span: error_span,
                        }
                    } else {
                        PythonProjectConfigurationFault::NativeParserRefusal {
                            message,
                            span: error_span,
                        }
                    };
                    evidence.retain(fault, width);
                } else {
                    loader_faults(errors, &mut evidence, control)?;
                }
            }
            Err(error) => {
                let rejected = span(
                    error.valid_up_to()
                        ..error.valid_up_to().saturating_add(
                            error
                                .error_len()
                                .unwrap_or(bytes.len() - error.valid_up_to()),
                        ),
                )
                .ok_or_else(|| {
                    super::project::project_error(
                        "",
                        "configuration decode extent exceeds source bound",
                    )
                })?;
                // One unit for this exact byte fact leaves the remaining shared
                // allowance for the actual loader cause. No TOML refusal is
                // invented when the native UTF-8 loader never reached TOML.
                evidence.retain(
                    PythonProjectConfigurationFault::InvalidConfigurationEncoding {
                        span: rejected,
                    },
                    0,
                );
                loader_faults(errors, &mut evidence, control)?;
            }
        }
    } else {
        loader_faults(errors, &mut evidence, control)?;
    }
    checkpoint(control)?;
    Ok(CheckerError::ProjectConfiguration {
        path: original_path.to_path_buf(),
        source_identity: *source.identity,
        source_byte_len: source.byte_len,
        omitted_faults: evidence.omitted,
        evidence_truncated: evidence.truncated,
        faults: evidence.faults.into_boxed_slice(),
    })
}

fn loader_faults(
    errors: &[ConfigError],
    evidence: &mut Evidence,
    control: PythonProjectControl<'_>,
) -> Result<(), CheckerError> {
    for error in errors {
        checkpoint(control)?;
        let severity = format!("{:?}", error.severity()).into_boxed_str();
        if severity.len() + 1 > evidence.remaining() {
            evidence.omit();
            continue;
        }
        // This pinned public API only exposes an owned message. Its producer
        // already consumed a <=1 MiB captured document; retain only our shared
        // remaining allowance and do not accumulate the returned strings.
        let original = error.get_message();
        checkpoint(control)?;
        let message = evidence.message(&original, evidence.remaining() - severity.len() - 1);
        let width = severity.len() + message.len();
        evidence.retain(
            PythonProjectConfigurationFault::NativeLoaderRefusal { severity, message },
            width,
        );
    }
    checkpoint(control)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
        PythonProjectControl {
            cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn refuse(name: &str, text: &str) -> Box<[PythonProjectConfigurationFault]> {
        let root =
            std::env::temp_dir().join(format!("nudox-python-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("fixture root");
        let path = root.join("pyproject.toml");
        std::fs::write(&path, text).expect("exact config bytes");
        let (_, errors) = ConfigFile::from_file(&path);
        assert!(!errors.is_empty(), "actual native loader refusal");
        let error = configuration_error(&path, &path, &errors, control(&AtomicBool::new(false)))
            .expect("bounded diagnostic");
        let CheckerError::ProjectConfiguration {
            path: original,
            source_identity,
            faults,
            ..
        } = error
        else {
            panic!("typed configuration refusal")
        };
        assert_eq!(original, path);
        assert_eq!(
            source_identity,
            *backend_semantic::ir::SourceIdentity::from_bytes(text.as_bytes())
                .expect("exact source identity")
                .identity
        );
        assert_eq!(
            std::fs::read(&path).expect("unchanged raw source"),
            text.as_bytes()
        );
        std::fs::remove_dir_all(root).expect("fixture cleanup");
        faults
    }

    #[test]
    fn authentic_pypa_diagnostic_options_have_exact_unsupported_values() {
        let text = "# 🐍 exact CRLF config\r\n[tool.pyrefly]\r\npreset = 'all'\r\n[tool.pyrefly.errors]\r\nmissing-override-decorator = false\r\nimplicit-bool = false\r\nunused-call-result = false\r\n";
        let faults = refuse("authentic-options", text);
        let options = faults
            .iter()
            .filter_map(|fault| match fault {
                PythonProjectConfigurationFault::UnsupportedDiagnosticOption {
                    option,
                    value,
                    value_span,
                } => {
                    assert_eq!(value.as_ref(), "false");
                    assert_eq!(
                        &text[value_span.start as usize..value_span.end as usize],
                        value.as_ref()
                    );
                    Some(option.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            options,
            [
                "tool.pyrefly.errors.implicit-bool",
                "tool.pyrefly.errors.unused-call-result"
            ]
        );
        assert!(faults.iter().any(|fault| matches!(fault,
            PythonProjectConfigurationFault::NativeParserRefusal { message, .. }
                if message.contains("unknown variant") && message.contains("implicit-bool")
        )));
    }

    #[test]
    fn supported_native_diagnostic_policy_is_not_classified_as_unsupported() {
        let text = "[tool.pyrefly]\npreset = 'all'\n[tool.pyrefly.errors]\nmissing-override-decorator = false\n";
        assert!(toml_native::from_str::<PyProject>(text).is_ok());
        let mut evidence = Evidence::default();
        diagnostic_faults(text, true, &mut evidence, control(&AtomicBool::new(false))).unwrap();
        assert!(evidence.faults.is_empty());
        assert_eq!(evidence.omitted, 0);
    }

    #[test]
    fn malformed_toml_retains_the_actual_parse_cause_and_range() {
        let text = "[tool.pyrefly]\nsearch-path = ['src'\n";
        let faults = refuse("malformed-toml", text);
        assert_eq!(faults.len(), 1);
        assert!(
            matches!(&faults[0], PythonProjectConfigurationFault::InvalidNativeConfiguration {
            message, span: Some(span)
        } if !message.is_empty() && span.start <= span.end && span.end as usize <= text.len())
        );
    }

    #[test]
    fn mixed_unsupported_and_invalid_known_values_keep_both_and_native_refusal() {
        let text =
            "[tool.pyrefly.errors]\nimplicit-bool = false\nmissing-override-decorator = [false]\n";
        let faults = refuse("mixed-options", text);
        assert!(faults.iter().any(|fault| matches!(fault,
            PythonProjectConfigurationFault::UnsupportedDiagnosticOption { option, .. }
                if option.as_ref() == "tool.pyrefly.errors.implicit-bool"
        )));
        assert!(faults.iter().any(|fault| matches!(fault,
            PythonProjectConfigurationFault::InvalidDiagnosticValue { option, value, value_span, message }
                if option.as_ref() == "tool.pyrefly.errors.missing-override-decorator"
                && value.as_ref() == "[false]"
                && &text[value_span.start as usize..value_span.end as usize] == value.as_ref()
                && message.contains("expected string or boolean")
        )));
        assert!(faults.iter().any(|fault| matches!(
            fault,
            PythonProjectConfigurationFault::NativeParserRefusal { .. }
        )));
    }

    #[test]
    fn diagnostics_never_attribute_ignored_nested_sub_config_options() {
        let text = "[tool.pyrefly.errors]\nimplicit-bool = false\n[[tool.pyrefly.sub-config]]\nmatches = '**/*.py'\n[tool.pyrefly.sub-config.errors]\nunused-call-result = false\n[[tool.pyrefly.sub-config.sub-config]]\nmatches = '**/*.py'\n[tool.pyrefly.sub-config.sub-config.errors]\nnested-not-native = false\n";
        let faults = refuse("nested-options", text);
        let options = faults
            .iter()
            .filter_map(|fault| match fault {
                PythonProjectConfigurationFault::UnsupportedDiagnosticOption { option, .. } => {
                    Some(option.as_ref())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            options,
            [
                "tool.pyrefly.errors.implicit-bool",
                "tool.pyrefly.sub-config[0].errors.unused-call-result"
            ]
        );
    }

    #[test]
    fn native_loader_warnings_keep_severity_and_refuse_exact_bytes() {
        let text = "[tool.pyrefly]\nunknown-native-setting = true\n";
        assert!(toml_native::from_str::<PyProject>(text).is_ok());
        let faults = refuse("native-warning", text);
        assert_eq!(faults.len(), 1);
        assert!(
            matches!(&faults[0], PythonProjectConfigurationFault::NativeLoaderRefusal {
            severity, message,
        } if severity.as_ref() == "Warn" && message.contains("Extra keys found in config")
            && message.contains("unknown-native-setting"))
        );
    }

    #[test]
    fn invalid_configuration_utf8_keeps_raw_span_identity_and_actual_native_loader_refusal() {
        let root =
            std::env::temp_dir().join(format!("nudox-python-config-utf8-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pyproject.toml");
        let bytes = b"[tool.pyrefly]\r\n# \xff\r\n";
        std::fs::write(&path, bytes).unwrap();
        let (_, errors) = ConfigFile::from_file(&path);
        assert_eq!(
            errors.len(),
            1,
            "actual native loader rejected original bytes"
        );
        let native_message = errors[0].get_message();
        let refusal =
            configuration_error(&path, &path, &errors, control(&AtomicBool::new(false))).unwrap();
        let CheckerError::ProjectConfiguration {
            source_identity,
            source_byte_len,
            omitted_faults,
            faults,
            ..
        } = refusal
        else {
            panic!("typed config")
        };
        let rejected = bytes.iter().position(|byte| *byte == 255).unwrap() as u32;
        assert_eq!(
            source_identity,
            *backend_semantic::ir::SourceIdentity::from_bytes(bytes)
                .unwrap()
                .identity
        );
        assert_eq!(source_byte_len as usize, bytes.len());
        assert_eq!(omitted_faults, 0);
        assert_eq!(faults.len(), 2);
        assert!(
            matches!(&faults[0], PythonProjectConfigurationFault::InvalidConfigurationEncoding { span } if span.start == rejected && span.end == rejected + 1)
        );
        assert!(
            matches!(&faults[1], PythonProjectConfigurationFault::NativeLoaderRefusal { severity, message }
            if severity.as_ref() == "Error" && !message.is_empty() && native_message.starts_with(message.as_ref()))
        );
        assert!(!faults.iter().any(|fault| matches!(
            fault,
            PythonProjectConfigurationFault::InvalidNativeConfiguration { .. }
                | PythonProjectConfigurationFault::NativeParserRefusal { .. }
        )));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explanatory_capture_refuses_cancellation_and_expired_deadline_before_io() {
        let missing = Path::new("/missing/pyproject.toml");
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            configuration_error(missing, missing, &[], control(&cancelled)),
            Err(CheckerError::Cancelled { .. })
        ));
        let active = AtomicBool::new(false);
        let expired = PythonProjectControl {
            cancelled: &active,
            deadline: Instant::now() - Duration::from_secs(1),
        };
        assert!(matches!(
            configuration_error(missing, missing, &[], expired),
            Err(CheckerError::Deadline { .. })
        ));
    }

    #[test]
    fn oversized_exact_option_values_are_omitted_with_bounded_source_bound_evidence() {
        let root =
            std::env::temp_dir().join(format!("nudox-python-config-budget-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pyproject.toml");
        let mut text = format!(
            "[tool.pyrefly.errors]\nunknown-large = '{}'\n",
            "🧭".repeat(10000)
        );
        for index in 0..300 {
            text.push_str(&format!("unknown-{index:03} = false\n"));
        }
        std::fs::write(&path, &text).unwrap();
        let (_, errors) = ConfigFile::from_file(&path);
        let error =
            configuration_error(&path, &path, &errors, control(&AtomicBool::new(false))).unwrap();
        let CheckerError::ProjectConfiguration {
            source_identity,
            source_byte_len,
            omitted_faults,
            evidence_truncated,
            faults,
            ..
        } = error
        else {
            panic!("typed config")
        };
        let retained = faults
            .iter()
            .filter(|fault| {
                matches!(
                    fault,
                    PythonProjectConfigurationFault::UnsupportedDiagnosticOption { .. }
                )
            })
            .count();
        assert_eq!(retained + omitted_faults as usize, 301);
        assert!(omitted_faults > 290);
        assert!(evidence_truncated);
        assert_eq!(source_byte_len as usize, text.len());
        assert_eq!(
            source_identity,
            *backend_semantic::ir::SourceIdentity::from_bytes(text.as_bytes())
                .unwrap()
                .identity
        );
        let mut width = faults.len();
        for fault in &faults {
            match fault {
                PythonProjectConfigurationFault::UnsupportedDiagnosticOption {
                    option,
                    value,
                    value_span,
                } => {
                    width += option.len() + value.len();
                    assert_eq!(
                        &text[value_span.start as usize..value_span.end as usize],
                        value.as_ref()
                    );
                    assert!(!option.ends_with("unknown-large"));
                }
                PythonProjectConfigurationFault::NativeParserRefusal { message, span } => {
                    width += message.len();
                    assert!(span.is_some());
                }
                _ => panic!("only unsupported options plus independent native parser refusal"),
            }
        }
        assert!(width <= MAX_NATIVE_DIAGNOSTIC_BYTES);
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explanatory_parse_refuses_input_above_existing_capture_bound() {
        let root = std::env::temp_dir().join(format!(
            "nudox-python-config-capture-bound-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("pyproject.toml");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(1024 * 1024 + 1).unwrap();
        let result = configuration_error(&path, &path, &[], control(&AtomicBool::new(false)));
        assert!(
            matches!(result, Err(CheckerError::ProjectReport { message, .. }) if message.contains("bounded regular captured file"))
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
