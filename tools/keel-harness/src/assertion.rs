//! Expectations are written independently of the live answer; no blessing mode.
use crate::manifest::Expect;
use serde_json::{Value, json};

fn contains(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::String(a), Value::String(e)) => a.contains(e.as_str()),
        (Value::Array(a), Value::Array(e)) => e.iter().all(|item| a.contains(item)),
        (Value::Array(a), e) => a.contains(e),
        _ => false,
    }
}
pub(crate) fn check(
    expect: &Expect,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
    value: Option<&Value>,
) -> Result<(), String> {
    if code != Some(expect.exit) {
        return Err(format!(
            "exit: expected {}, got {code:?}; stderr: {}",
            expect.exit,
            String::from_utf8_lossy(stderr)
        ));
    }
    for (label, bytes, needles) in [
        ("stdout", stdout, &expect.stdout_contains),
        ("stderr", stderr, &expect.stderr_contains),
    ] {
        let text = String::from_utf8_lossy(bytes);
        for needle in needles {
            if !text.contains(needle) {
                return Err(format!("{label}: missing reviewed text {needle:?}"));
            }
        }
    }
    for a in &expect.json {
        let value = value.ok_or_else(|| "expected JSON, received non-JSON stdout".to_owned())?;
        let found = value.pointer(&a.pointer);
        if a.absent {
            if found.is_some() {
                return Err(format!(
                    "{}: expected absent, got {}",
                    a.pointer,
                    found.unwrap_or(&Value::Null)
                ));
            }
            continue;
        }
        let mut actual = found
            .cloned()
            .ok_or_else(|| format!("{}: missing required JSON field", a.pointer))?;
        if let Some(select) = &a.select {
            let rows = actual
                .as_array()
                .ok_or_else(|| format!("{}: selection requires array", a.pointer))?;
            actual = Value::Array(
                rows.iter()
                    .map(|r| {
                        r.pointer(select)
                            .cloned()
                            .ok_or_else(|| format!("{}: row lacks {select}", a.pointer))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        let matched = if let Some(expected) = &a.equals {
            actual == *expected
        } else if let Some(expected) = &a.contains {
            contains(&actual, expected)
        } else if let Some(expected) = &a.not_contains {
            if !actual.is_array() && !actual.is_string() {
                return Err(format!(
                    "{}: not_contains requires an array or string",
                    a.pointer
                ));
            }
            !contains(&actual, expected)
        } else if let Some(expected) = &a.set_equals {
            let mut left = actual
                .as_array()
                .ok_or_else(|| format!("{}: set equality requires array", a.pointer))?
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>();
            let mut right = expected.iter().map(Value::to_string).collect::<Vec<_>>();
            left.sort();
            right.sort();
            left == right
        } else {
            false
        };
        if !matched {
            return Err(format!(
                "{}: reviewed expectation failed; actual={actual}; expected={}",
                a.pointer,
                json!({"equals":a.equals,"contains":a.contains,"not_contains":a.not_contains,"set_equals":a.set_equals})
            ));
        }
    }
    Ok(())
}
