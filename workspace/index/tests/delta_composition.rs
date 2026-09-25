//! One revision, three sinks. IR entries, vector points, and content keys
//! classify the same identities through [`index::delta::merge_sorted`].

use index::{
    delta::{ContentKey, Delta, content_delta},
    frontier::{
        ir::{IrEntryKey, project},
        vector::{PointId, upserts_required, upserts_to_skip},
    },
};
use smol_str::SmolStr;

struct Row {
    id: u8,
    payload: u8,
}

fn intro(id: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[31] = id;
    bytes
}

fn hash(payload: u8) -> [u8; 32] {
    [payload; 32]
}

fn ir(rows: &[Row]) -> Vec<IrEntryKey> {
    rows.iter()
        .map(|row| IrEntryKey {
            intro_id: intro(row.id),
            content_hash: hash(row.payload),
        })
        .collect()
}

fn points(rows: &[Row]) -> Vec<PointId> {
    rows.iter()
        .map(|row| PointId {
            package: SmolStr::new("pkg"),
            intro_hex: SmolStr::new(format!("{:02x}", row.id)),
            content_hash: hash(row.payload),
        })
        .collect()
}

fn keys(rows: &[Row]) -> Vec<ContentKey> {
    rows.iter()
        .map(|row| ContentKey::hash("revision", format!("{:02x}", row.id), &hash(row.payload)))
        .collect()
}

fn id_of_intro(intro_id: [u8; 32]) -> u8 {
    intro_id[31]
}

fn sets(delta: &Delta<IrEntryKey>) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut added: Vec<_> = delta
        .added
        .iter()
        .map(|entry| id_of_intro(entry.intro_id))
        .collect();
    let mut changed: Vec<_> = delta
        .changed
        .iter()
        .map(|entry| id_of_intro(entry.intro_id))
        .collect();
    let mut removed: Vec<_> = delta
        .removed
        .iter()
        .map(|entry| id_of_intro(entry.intro_id))
        .collect();
    added.sort_unstable();
    changed.sort_unstable();
    removed.sort_unstable();
    (added, changed, removed)
}

#[test]
fn one_revision_classifies_the_same_on_ir_vectors_and_content_keys() {
    let prior = [
        Row { id: 1, payload: 1 },
        Row { id: 2, payload: 2 },
        Row { id: 3, payload: 3 },
        Row { id: 2, payload: 9 },
    ];
    let next = [
        Row { id: 2, payload: 9 },
        Row { id: 4, payload: 4 },
        Row { id: 1, payload: 1 },
        Row { id: 3, payload: 7 },
    ];

    let ir_delta = project(&ir(&prior), &ir(&next));
    let (added, changed, removed) = sets(&ir_delta);
    assert_eq!(added, vec![4]);
    assert_eq!(changed, vec![3]);
    assert_eq!(removed, Vec::<u8>::new());

    let content = content_delta(&keys(&prior), &keys(&next));
    let content_ids = |rows: &[ContentKey]| {
        let mut ids: Vec<_> = rows.iter().map(|key| key.id.to_string()).collect();
        ids.sort();
        ids
    };
    assert_eq!(content_ids(&content.added), vec!["04".to_string()]);
    assert_eq!(content_ids(&content.changed), vec!["03".to_string()]);
    assert!(content.removed.is_empty());

    let prior_points = points(&prior);
    let next_points = points(&next);
    let required = upserts_required(&prior_points, &next_points);
    let skipped = upserts_to_skip(&prior_points, &next_points);
    let mut required_ids: Vec<_> = required
        .iter()
        .map(|point| point.intro_hex.to_string())
        .collect();
    let mut skipped_ids: Vec<_> = skipped
        .iter()
        .map(|point| point.intro_hex.to_string())
        .collect();
    required_ids.sort();
    skipped_ids.sort();
    assert_eq!(required_ids, vec!["03".to_string(), "04".to_string()]);
    assert_eq!(skipped_ids, vec!["01".to_string(), "02".to_string()]);
    assert_eq!(required.len() + skipped.len(), next_points.len());
}
