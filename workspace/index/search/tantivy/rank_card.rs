//! Compact rank card stored in a FAST bytes column.
//!
//! The live rank path reads this card and the numeric FAST columns. The stored
//! `record` JSON is decoded only for the hits that survive ranking.

use heart::PackageId;
use smol_str::SmolStr;

use crate::{GlobalPackage, error::SearchError, schema::codec};

pub(super) const HAS_FACETS: u64 = 1 << 0;
pub(super) const WITHDRAWN: u64 = 1 << 1;
pub(super) const SQUAT: u64 = 1 << 2;
pub(super) const MALWARE: u64 = 1 << 3;
pub(super) const VERIFIED: u64 = 1 << 4;
pub(super) const HAS_DOWNLOADS: u64 = 1 << 5;
pub(super) const HAS_DEPENDENTS: u64 = 1 << 6;
pub(super) const HAS_POPULARITY: u64 = 1 << 7;

/// Strings ranking and de-duplication need, packed so a hit does not open the
/// document store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RankCard {
    pub(crate) id: PackageId,
    pub(crate) ecosystem: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) repo: Option<String>,
    pub(crate) keywords: Vec<SmolStr>,
}

/// One retrieval hit's rank inputs. `bm25` is filled by the searcher.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RankHit {
    pub(crate) card: RankCard,
    pub(crate) bm25: f32,
    pub(crate) quality: f32,
    pub(crate) downloads: Option<u64>,
    pub(crate) dependents: Option<u32>,
    pub(crate) popularity_pct: Option<f32>,
    pub(crate) withdrawn: bool,
    pub(crate) squat_suspect: bool,
    pub(crate) malware: bool,
    pub(crate) facet_verified_repo: bool,
}

pub(super) struct NumericColumns {
    pub(super) dependents: u64,
    pub(super) presence: u64,
}

pub(super) fn columns_for(record: &GlobalPackage) -> (Vec<u8>, NumericColumns) {
    let (card, numeric) = parts(record);
    (encode(&card), numeric)
}

fn parts(record: &GlobalPackage) -> (RankCard, NumericColumns) {
    let coordinates = &record.package.coordinates;
    let (repo, keywords, presence, dependents) = match record.facets.as_ref() {
        None => (None, Vec::new(), 0, 0),
        Some(facets) => {
            let mut presence = HAS_FACETS;
            if facets.withdrawn {
                presence |= WITHDRAWN;
            }
            if facets.squat_suspect {
                presence |= SQUAT;
            }
            if facets.malware {
                presence |= MALWARE;
            }
            if facets.verified_repo {
                presence |= VERIFIED;
            }
            if facets.downloads.is_some() {
                presence |= HAS_DOWNLOADS;
            }
            let dependents = facets.dependents.map_or(0, u64::from);
            if facets.dependents.is_some() {
                presence |= HAS_DEPENDENTS;
            }
            if facets.popularity_pct.is_some() {
                presence |= HAS_POPULARITY;
            }
            (
                facets.repo_slug.as_deref().map(str::to_owned),
                facets
                    .keywords
                    .iter()
                    .map(|kw| kw.as_str().to_owned())
                    .collect(),
                presence,
                dependents,
            )
        }
    };
    let card = RankCard {
        id: record.id,
        ecosystem: coordinates.ecosystem().as_token().to_owned(),
        name: coordinates.name.canonical().to_owned(),
        version: coordinates.version.canonical(),
        repo,
        keywords: keywords.into_iter().map(SmolStr::new).collect(),
    };
    (card, NumericColumns {
        dependents,
        presence,
    })
}

/// Rebuild a hit from a package that was not in the text result
/// (semantic-only).
pub(crate) fn hit_from_package(record: &GlobalPackage, bm25: f32) -> RankHit {
    let (card, numeric) = parts(record);
    decode_hit(
        card,
        bm25,
        record
            .facets
            .as_ref()
            .map(|facets| u64::from(facets.quality_ppm))
            .unwrap_or(0),
        record
            .facets
            .as_ref()
            .and_then(|facets| facets.downloads)
            .unwrap_or(0),
        record
            .facets
            .as_ref()
            .and_then(|facets| facets.popularity_pct)
            .map_or(0, |value| u64::from(value.min(10_000)) * 100),
        numeric.dependents,
        numeric.presence,
    )
}

pub(super) fn decode_hit(
    card: RankCard,
    bm25: f32,
    quality_ppm: u64,
    downloads: u64,
    popularity_pct_ppm: u64,
    dependents: u64,
    presence: u64,
) -> RankHit {
    let has_facets = presence & HAS_FACETS != 0;
    RankHit {
        card,
        bm25,
        quality: if has_facets {
            quality_ppm as f32 / 1_000_000.0
        } else {
            0.5
        },
        downloads: (presence & HAS_DOWNLOADS != 0).then_some(downloads),
        dependents: (presence & HAS_DEPENDENTS != 0)
            .then_some(u32::try_from(dependents).unwrap_or(u32::MAX)),
        popularity_pct: (presence & HAS_POPULARITY != 0).then(|| {
            let basis = (popularity_pct_ppm / 100).min(10_000) as f32;
            (basis / 10_000.0).clamp(0.0, 1.0)
        }),
        withdrawn: presence & WITHDRAWN != 0,
        squat_suspect: presence & SQUAT != 0,
        malware: presence & MALWARE != 0,
        facet_verified_repo: presence & VERIFIED != 0,
    }
}

pub(super) fn encode(card: &RankCard) -> Vec<u8> {
    let mut out = Vec::new();
    push_str(&mut out, &card.id.to_string());
    push_str(&mut out, &card.ecosystem);
    push_str(&mut out, &card.name);
    push_str(&mut out, &card.version);
    push_str(&mut out, card.repo.as_deref().unwrap_or(""));
    let count = u32::try_from(card.keywords.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&count.to_le_bytes());
    for keyword in card.keywords.iter().take(count as usize) {
        push_str(&mut out, keyword.as_str());
    }
    out
}

pub(super) fn decode(bytes: &[u8]) -> Result<RankCard, SearchError> {
    let mut cursor = 0;
    let id_text = take_str(bytes, &mut cursor)?;
    let identifier = id_text
        .parse::<uuid::Uuid>()
        .map_err(|_| SearchError::StoredIdNotUuid)?;
    let ecosystem = take_str(bytes, &mut cursor)?;
    let name = take_str(bytes, &mut cursor)?;
    let version = take_str(bytes, &mut cursor)?;
    let repo = take_str(bytes, &mut cursor)?;
    let count = take_u32(bytes, &mut cursor)?;
    let mut keywords = Vec::with_capacity(count as usize);
    for _ in 0..count {
        keywords.push(SmolStr::new(take_str(bytes, &mut cursor)?));
    }
    Ok(RankCard {
        id: codec::package_id_from_uuid(identifier),
        ecosystem,
        name,
        version,
        repo: (!repo.is_empty()).then_some(repo),
        keywords,
    })
}

fn push_str(out: &mut Vec<u8>, text: &str) {
    let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&text.as_bytes()[..len as usize]);
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, SearchError> {
    let end = cursor.checked_add(4).ok_or_else(truncated)?;
    let raw: [u8; 4] = bytes
        .get(*cursor..end)
        .ok_or_else(truncated)?
        .try_into()
        .map_err(|_| truncated())?;
    *cursor = end;
    Ok(u32::from_le_bytes(raw))
}

fn take_str(bytes: &[u8], cursor: &mut usize) -> Result<String, SearchError> {
    let len = take_u32(bytes, cursor)? as usize;
    let end = cursor.checked_add(len).ok_or_else(truncated)?;
    let slice = bytes.get(*cursor..end).ok_or_else(truncated)?;
    *cursor = end;
    String::from_utf8(slice.to_vec()).map_err(|error| SearchError::RowDecode {
        column: "rank_card",
        detail: error.to_string(),
    })
}

fn truncated() -> SearchError {
    SearchError::RowDecode {
        column: "rank_card",
        detail: "truncated rank card".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> RankCard {
        RankCard {
            id: codec::package_id_from_uuid(uuid::Uuid::from_u128(7)),
            ecosystem: "rust".into(),
            name: "serde".into(),
            version: "1.0.0".into(),
            repo: Some("serde-rs/serde".into()),
            keywords: vec![SmolStr::new("ser\tde"), SmolStr::new("json")],
        }
    }

    #[test]
    fn card_round_trips_including_delimiter_bytes() {
        let original = card();
        let decoded = decode(&encode(&original)).expect("decode");
        assert_eq!(decoded, original);
        let again = decode(&encode(&decoded)).expect("second decode");
        assert_eq!(encode(&original), encode(&again));
    }

    #[test]
    fn empty_repo_and_keywords_stay_absent() {
        let mut original = card();
        original.repo = None;
        original.keywords.clear();
        let decoded = decode(&encode(&original)).expect("decode");
        assert_eq!(decoded.repo, None);
        assert!(decoded.keywords.is_empty());
    }

    #[test]
    fn truncated_card_is_an_error() {
        let bytes = encode(&card());
        assert!(decode(&bytes[..3]).is_err());
    }

    #[test]
    fn indexed_quality_matches_facets_in_either_insert_order() {
        use crate::{
            GlobalPackage, Package,
            ecosystem::PackageNameExt as _,
            metadata::SearchFacets,
            package::{Coordinates, PackageName},
        };
        use heart::{
            Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain,
        };

        fn pkg(name: &str, quality_ppm: u32, keywords: &[&str]) -> GlobalPackage {
            let coordinates = Coordinates {
                origin: RegistryOrigin::CratesIo,
                name: PackageName::new(Language::Rust, name).expect("name"),
                version: PackageVersion::try_from((Language::Rust, "1.0.0")).expect("version"),
            };
            let package = Package {
                coordinates,
                toolchain: Toolchain::Rust {
                    compiler: semver::Version::new(1, 85, 0),
                    edition: Edition::E2024,
                },
            };
            let id = package.id();
            GlobalPackage {
                id,
                package,
                state: ResolutionState::Unindexed { needed: false },
                facets: Some(SearchFacets {
                    quality_ppm,
                    keywords: keywords.iter().map(|kw| SmolStr::new(*kw)).collect(),
                    ..Default::default()
                }),
            }
        }

        let forward = vec![
            pkg("serde", 950_000, &["serialization"]),
            pkg("serde-json", 850_000, &["serialization", "json"]),
            pkg("serde-derive", 800_000, &["serialization", "derive"]),
            pkg("serde-spam", 100_000, &["serde", "serialization"]),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        let mut seen: Vec<Vec<(String, u32, f32)>> = Vec::new();
        for records in [forward, reversed] {
            let dir = tempfile::tempdir().expect("dir");
            let mut index = super::super::PackageIndex::open(dir.path()).expect("open");
            index.absorb(records.iter(), 1).expect("absorb");
            let structured = crate::search::StructuredQuery::parse("serde", Some(Language::Rust));
            let hits = index.query_signals(&structured, 8).expect("signals");
            let mut row: Vec<_> = hits
                .iter()
                .map(|hit| {
                    (
                        hit.card.name.clone(),
                        (hit.quality * 1_000_000.0) as u32,
                        hit.bm25,
                    )
                })
                .collect();
            row.sort_by(|a, b| a.0.cmp(&b.0));
            seen.push(row);
            for record in &records {
                let hit = hits
                    .iter()
                    .find(|hit| hit.card.name == record.package.coordinates.name.canonical())
                    .expect("hit");
                let expected = record.facets.as_ref().unwrap().quality_ppm as f32 / 1_000_000.0;
                assert!(
                    (hit.quality - expected).abs() < 1e-6,
                    "{} quality {} != {expected}",
                    hit.card.name,
                    hit.quality
                );
            }
        }
        assert_eq!(seen[0], seen[1], "bm25 must not depend on insert order");
    }
}
