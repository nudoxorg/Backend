use super::{
    time::rfc3339_millis,
    window::{RangeEvent, VersionWindow},
    *,
};

/// Parse one RustSec advisory TOML file into an [`AdvisorySource`].
///
/// cargo-audit and cargo-deny both read this file shape. A single
/// `versions.patched` requirement of the form `>= x` becomes
/// `introduced:0,fixed:x`, the same window an OSV event range uses. Several
/// patched requirements are not one window, so the range stays empty and the
/// advisory is still upserted. The ecosystem is `crates.io`.
pub fn parse_rustsec(text: &str, recorded_at: i64) -> Result<AdvisorySource, String> {
    let value: toml::Value = toml::from_str(text).map_err(|err| err.to_string())?;
    let advisory = value
        .get("advisory")
        .ok_or_else(|| "rustsec advisory has no [advisory]".to_owned())?;
    let id = advisory
        .get("id")
        .and_then(toml::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "rustsec advisory has no id".to_owned())?;
    let package = advisory
        .get("package")
        .and_then(toml::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "rustsec advisory has no package".to_owned())?;
    let url = advisory
        .get("url")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let valid_from = advisory
        .get("date")
        .and_then(toml::Value::as_str)
        .and_then(|date| rfc3339_millis(&format!("{date}T00:00:00Z")))
        .unwrap_or(recorded_at);
    let valid_to = advisory
        .get("withdrawn")
        .and_then(toml::Value::as_str)
        .and_then(|date| rfc3339_millis(&format!("{date}T00:00:00Z")));
    let patched = value
        .get("versions")
        .and_then(|versions| versions.get("patched"))
        .and_then(toml::Value::as_array)
        .map(|patched| {
            patched
                .iter()
                .filter_map(toml::Value::as_str)
                .filter_map(patched_floor)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let version_range = if patched.len() == 1 {
        Some(
            VersionWindow::Events(vec![
                RangeEvent::Introduced("0".to_owned()),
                RangeEvent::Fixed(patched[0].clone()),
            ])
            .render(),
        )
    } else {
        None
    };
    let mut source = AdvisorySource {
        upstream_id: id.to_owned(),
        stem_id: None,
        version_range,
        severity: None,
        summary: advisory
            .get("title")
            .or_else(|| advisory.get("description"))
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        url,
        valid_from,
        valid_to,
        recorded_at,
        affected_name: Some(package.to_owned()),
        affected_ecosystem: Some("crates.io".to_owned()),
    };
    resolve_stem(&mut source);
    Ok(source)
}

/// The version a `>= x` patched requirement starts fixing.
fn patched_floor(requirement: &str) -> Option<String> {
    let requirement = requirement.trim();
    let rest = requirement.strip_prefix(">=")?.trim();
    if rest.is_empty() || rest.contains(|ch: char| ch.is_whitespace() || ch == ',') {
        return None;
    }
    Some(rest.trim_start_matches('v').to_owned())
}
