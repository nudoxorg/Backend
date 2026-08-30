# Wave B4 immutable-index reviewer calibration deck

## Candidate excerpt A

```diff
+pub struct SearchRequest {
+    pub snapshot: IndexSnapshotId,
+    pub segments: Vec<ContentId<AnyIndexDomain>>,
+    pub query: String,
+    pub route: WorkerId,
+}
+pub fn search(request: SearchRequest) -> Vec<Document> { backend(request) }
```

## Candidate excerpt B

```diff
+pub fn lexical_at(route: WorkerId, terms: &str) -> SearchTerminal {
+    let head = route.current_head();
+    match route.tantivy_search(head, terms) {
+        Ok(rows) => SearchTerminal::Complete(rows),
+        Err(_) => SearchTerminal::Complete(Vec::new()),
+    }
+}
```

## Candidate excerpt C

```diff
+pub type PublishedIndexSnapshot = tantivy::Index;
+pub fn segment_id(bytes: &[u8], tier: Tier) -> ExactSegmentId {
+    ExactSegmentId::from_canonical_bytes([bytes, tier.name().as_bytes()].concat())
+}
```

## Candidate excerpt D

```diff
+pub fn compact(local: &mut NodeIndex) {
+    local.repack_hot_segments();
+    local.head = local.latest_snapshot();
+}
```

## Calibration instruction

Assess these excerpts and the frozen card as if they were a proposed pre-edit ABI. Identify every
contract violation and the smallest falsifier. Do not assume the excerpts are intentionally wrong;
do not suggest an implementation or alter source.
