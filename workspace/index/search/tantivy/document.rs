//! One tantivy document per [`GlobalPackage`].

use crate::{GlobalPackage, ecosystem::PackageNameExt as _, error::SearchError};

use super::schema::{self, Fields};

pub(super) fn build_document(
    fields: &Fields,
    record: &GlobalPackage,
) -> Result<tantivy::TantivyDocument, SearchError> {
    let package_token = record.id.to_string();
    let json = serde_json::to_string(record).map_err(|error| SearchError::JsonDecode {
        domain: "GlobalPackage",
        source: error,
    })?;

    let coordinates = &record.package.coordinates;
    let name = &coordinates.name;
    let structured = name.structured();
    let canonical_lower = name.canonical().to_ascii_lowercase();
    let original_lower = name.original().to_ascii_lowercase();
    let search_surface = structured.search_surface();
    let search_surface_lower = search_surface.to_ascii_lowercase();
    let namespace_text = structured.namespace.join(" ");

    let mut document = tantivy::TantivyDocument::default();
    document.add_text(fields.package_id, &package_token);
    document.add_text(fields.name_exact, &search_surface_lower);
    if canonical_lower != search_surface_lower {
        document.add_text(fields.name_exact, &canonical_lower);
    }
    if original_lower != search_surface_lower && original_lower != canonical_lower {
        document.add_text(fields.name_exact, &original_lower);
    }
    document.add_text(fields.name_tokens, &search_surface);
    if !namespace_text.is_empty() {
        document.add_text(fields.name_ns, &namespace_text);
    }
    document.add_text(fields.ecosystem, coordinates.ecosystem().as_token());

    let ecosystem = coordinates.ecosystem();
    let base_keywords = record
        .facets
        .as_ref()
        .map(crate::metadata::SearchFacets::keyword_text)
        .unwrap_or_default();
    let enrichment = super::super::ranking::enrich::enrich_package_text(
        &search_surface_lower,
        &base_keywords,
        ecosystem,
    );
    let keywords_text = super::super::ranking::enrich::merge_keywords(&base_keywords, &enrichment);

    let signals = schema::ranking_signals_from_facets(record.facets.as_ref());
    document.add_u64(fields.quality_ppm, signals.quality_ppm);
    document.add_u64(fields.downloads, signals.downloads);
    document.add_u64(fields.popularity_pct_ppm, signals.popularity_pct_ppm);

    if let Some(facets) = &record.facets {
        if let Some(description) = &facets.description {
            document.add_text(fields.description, description.as_str());
        }
        if !keywords_text.is_empty() {
            document.add_text(fields.keywords, &keywords_text);
        }
        for dependency in &facets.dependencies {
            document.add_text(fields.deps, dependency.as_str());
        }
        if let Some(license) = &facets.license {
            document.add_text(fields.license, license.as_str().to_ascii_lowercase());
        }
        if let Some(repository) = &facets.repo_slug {
            document.add_text(fields.repo, repository.as_str());
        }
    } else if !keywords_text.is_empty() {
        document.add_text(fields.keywords, &keywords_text);
    }

    document.add_text(fields.record, &json);
    Ok(document)
}
