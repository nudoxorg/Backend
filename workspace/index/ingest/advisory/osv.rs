use super::{
    time::rfc3339_millis,
    window::{RangeEvent, VersionWindow},
    *,
};

/// Parse one OSV JSON document into one [`AdvisorySource`] per affected
/// package.
///
/// A document with no `id` is rejected. An affected entry with no package name
/// is dropped. `withdrawn` sets `valid_to` and does not drop the advisory: a
/// withdrawn id still resolves. `stem_id` stays `None`; the feed name is kept
/// on [`AdvisorySource::affected_name`] for a later resolver.
pub fn parse_osv(body: &[u8], recorded_at: i64) -> Result<Vec<AdvisorySource>, String> {
    let value: serde_json::Value = serde_json::from_slice(body).map_err(|err| err.to_string())?;
    let id = value
        .get("id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .ok_or_else(|| "osv document has no id".to_owned())?;
    let summary = value
        .get("summary")
        .and_then(|summary| summary.as_str())
        .map(str::to_owned);
    let url = value
        .get("references")
        .and_then(|references| references.as_array())
        .and_then(|references| {
            references
                .iter()
                .find_map(|reference| reference.get("url").and_then(|url| url.as_str()))
        })
        .map(str::to_owned);
    let severity = value
        .pointer("/database_specific/severity")
        .and_then(|severity| severity.as_str())
        .map(str::to_owned)
        .or_else(|| {
            value
                .get("severity")
                .and_then(|severity| severity.as_array())
                .and_then(|severity| severity.first())
                .and_then(|entry| entry.get("score").and_then(|score| score.as_str()))
                .map(str::to_owned)
        });
    let valid_from = value
        .get("published")
        .and_then(|published| published.as_str())
        .and_then(rfc3339_millis)
        .unwrap_or(recorded_at);
    let valid_to = value
        .get("withdrawn")
        .and_then(|withdrawn| withdrawn.as_str())
        .and_then(rfc3339_millis);
    let affected = value
        .get("affected")
        .and_then(|affected| affected.as_array())
        .cloned()
        .unwrap_or_default();
    let mut sources = Vec::new();
    for entry in affected {
        let name = entry
            .pointer("/package/name")
            .and_then(|name| name.as_str())
            .filter(|name| !name.is_empty());
        let Some(name) = name else {
            continue;
        };
        let ecosystem = entry
            .pointer("/package/ecosystem")
            .and_then(|ecosystem| ecosystem.as_str())
            .map(str::to_owned);
        sources.push(AdvisorySource {
            upstream_id: id.to_owned(),
            stem_id: None,
            version_range: version_range(&entry),
            severity: severity.clone(),
            summary: summary.clone(),
            url: url.clone(),
            valid_from,
            valid_to,
            recorded_at,
            affected_name: Some(name.to_owned()),
            affected_ecosystem: ecosystem,
        });
    }
    if sources.is_empty() {
        sources.push(AdvisorySource {
            upstream_id: id.to_owned(),
            stem_id: None,
            version_range: None,
            severity,
            summary,
            url,
            valid_from,
            valid_to,
            recorded_at,
            affected_name: None,
            affected_ecosystem: None,
        });
    }
    Ok(sources)
}

fn version_range(entry: &serde_json::Value) -> Option<String> {
    version_window(entry).map(|window| window.render())
}

fn version_window(entry: &serde_json::Value) -> Option<VersionWindow> {
    if let Some(versions) = entry
        .get("versions")
        .and_then(|versions| versions.as_array())
    {
        let listed: Vec<String> = versions
            .iter()
            .filter_map(|version| version.as_str().map(str::to_owned))
            .collect();
        if !listed.is_empty() {
            return Some(VersionWindow::Listed(listed));
        }
    }
    let events = entry
        .pointer("/ranges/0/events")
        .and_then(|events| events.as_array())?;
    let mut parts = Vec::new();
    for event in events {
        if let Some(introduced) = event.get("introduced").and_then(|value| value.as_str()) {
            parts.push(RangeEvent::Introduced(introduced.to_owned()));
        }
        if let Some(fixed) = event.get("fixed").and_then(|value| value.as_str()) {
            parts.push(RangeEvent::Fixed(fixed.to_owned()));
        }
        if let Some(last) = event.get("last_affected").and_then(|value| value.as_str()) {
            parts.push(RangeEvent::LastAffected(last.to_owned()));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(VersionWindow::Events(parts))
    }
}

/// Map an OSV ecosystem token onto a catalog language.
///
/// The feed spells ecosystems as registry names (`crates.io`, `npm`, `PyPI`).
/// An unrecognized token leaves the advisory unresolved.
fn language_for_osv(token: &str) -> Option<crate::ecosystem::Language> {
    use crate::ecosystem::Language;
    match token {
        "crates.io" | "crates" => Some(Language::Rust),
        "npm" => Some(Language::Typescript),
        "Go" => Some(Language::Go),
        "PyPI" => Some(Language::Python),
        "Maven" => Some(Language::Java),
        "NuGet" => Some(Language::CSharp),
        other => crate::ecosystem::Language::from_token(other),
    }
}

/// Fill [`AdvisorySource::stem_id`] from the feed's ecosystem and package name.
///
/// The stem is the same `(language token, canonical name)` hash the catalog
/// uses for a package. A name that does not parse, or an ecosystem this index
/// does not speak, stays unresolved.
pub fn resolve_stem(source: &mut AdvisorySource) -> bool {
    use crate::ecosystem::LanguageExt;
    let (Some(name), Some(ecosystem)) = (
        source.affected_name.as_deref(),
        source.affected_ecosystem.as_deref(),
    ) else {
        return false;
    };
    let Some(language) = language_for_osv(ecosystem) else {
        return false;
    };
    let Some(parsed) = language.spec().parse_name(name) else {
        return false;
    };
    let id = heart::identity::derive::package_id_from_parts([
        language.as_token().as_bytes(),
        parsed.canonical().as_bytes(),
    ]);
    source.stem_id = Some(PackageStemId::from_uuid(*id.as_uuid()));
    true
}

/// Parse an OSV document and resolve every affected package that this index
/// can name.
pub fn resolve_osv(body: &[u8], recorded_at: i64) -> Result<Vec<AdvisorySource>, String> {
    let mut sources = parse_osv(body, recorded_at)?;
    for source in &mut sources {
        resolve_stem(source);
    }
    Ok(sources)
}
