//! Bounded read of one project's selected SQL analysis.
//!
//! Gathers the selection, hosts and facts of a project from the selected SQL
//! relation in one pass. Anything that is not a clean, complete read comes back
//! as an error or as gaps; callers must never treat a partial read as empty.
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{
        SqlAnalysisRecord, SqlHostTerminal, SqlInventory, sql_analysis_relation, sql_selection_key,
    },
};
use backend_semantic::sql_analysis::{SqlDialect, SqlFact, SqlGapCounts};

const MAX_ROWS: usize = 262_144;

pub(super) struct SqlHost {
    pub(super) key: [u8; 32],
    pub(super) path: String,
    pub(super) dialect: SqlDialect,
    pub(super) input: Option<[u8; 32]>,
    pub(super) source: Option<[u8; 32]>,
    pub(super) producer: Option<[u8; 32]>,
    pub(super) settings: Option<[u8; 32]>,
    pub(super) fact_digest: [u8; 32],
    pub(super) length: Option<u64>,
    pub(super) terminal: SqlHostTerminal,
}

pub(super) struct ProjectSql {
    /// Canonical project directory the selection was captured under.
    pub(super) root: String,
    pub(super) inventory_complete: bool,
    pub(super) implicit: bool,
    pub(super) gaps: SqlGapCounts,
    pub(super) hosts: Vec<SqlHost>,
    /// `(host index, ordinal, fact)` in host then ordinal order.
    pub(super) facts: Vec<(usize, u32, SqlFact)>,
}

/// Read the selected SQL analysis of `project`. `Ok(None)` means the snapshot
/// holds no SQL selection for it, which is unknown, not "no SQL".
pub(super) fn load_project(
    snapshot: &WorkspaceSnapshot,
    project: [u8; 32],
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<ProjectSql>, String> {
    let Some(relation) = sql_analysis_relation(snapshot).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let Some(SqlAnalysisRecord::Selection {
        project: owner,
        source_root,
        inventory,
        implicit,
        gaps,
        ..
    }) = relation
        .lookup(&sql_selection_key(project))
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    if owner != project {
        return Err("SQL selection belongs to another project".into());
    }
    let mut hosts: Vec<SqlHost> = Vec::new();
    let mut raw: Vec<([u8; 32], u32, SqlFact)> = Vec::new();
    let mut after = None;
    let mut rows = 0usize;
    loop {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|e| e.to_string())?;
        for (key, row) in page.entries() {
            rows += 1;
            if rows > MAX_ROWS {
                return Err("SQL relation exceeds the read bound".into());
            }
            match row {
                SqlAnalysisRecord::Host {
                    project: p,
                    path,
                    dialect,
                    input,
                    source,
                    producer,
                    settings,
                    fact_digest,
                    length,
                    terminal,
                    ..
                } if *p == project => {
                    hosts.push(SqlHost {
                        key: *key,
                        path: path.clone(),
                        dialect: *dialect,
                        input: *input,
                        source: *source,
                        producer: *producer,
                        settings: *settings,
                        fact_digest: *fact_digest,
                        length: *length,
                        terminal: *terminal,
                    });
                }
                SqlAnalysisRecord::Fact {
                    host,
                    ordinal,
                    fact,
                } => {
                    raw.push((*host, *ordinal, fact.clone()));
                }
                _ => {}
            }
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            Some(_) => return Err("SQL relation page did not advance".into()),
            None => break,
        }
    }
    hosts.sort_by(|a, b| a.path.cmp(&b.path));
    let index_of: std::collections::BTreeMap<[u8; 32], usize> =
        hosts.iter().enumerate().map(|(i, h)| (h.key, i)).collect();
    let mut facts: Vec<(usize, u32, SqlFact)> = raw
        .into_iter()
        .filter_map(|(host, ordinal, fact)| index_of.get(&host).map(|i| (*i, ordinal, fact)))
        .collect();
    facts.sort_by_key(|(host, ordinal, _)| (*host, *ordinal));
    Ok(Some(ProjectSql {
        root: source_root,
        inventory_complete: matches!(inventory, SqlInventory::Complete),
        implicit,
        gaps,
        hosts,
        facts,
    }))
}

/// Constant-size selected native host terminals for health. This says what the
/// selected snapshot analysed, never what a later installation might admit.
pub(super) fn progress(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<backend_library::SqlAnalysisProgress>, String> {
    let Some(relation) = sql_analysis_relation(snapshot).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    let mut summary = backend_library::SqlAnalysisProgress {
        analysed_files: 0,
        no_worker_files: 0,
        unsupported_dialect_files: 0,
        rejected_files: 0,
        other_unavailable_files: 0,
    };
    let mut after = None;
    let mut rows = 0usize;
    let mut has_hosts = false;
    loop {
        let page = relation
            .page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|e| e.to_string())?;
        for (_, row) in page.entries() {
            rows += 1;
            if rows > MAX_ROWS {
                return Err("SQL status exceeds the read bound".to_owned());
            }
            let SqlAnalysisRecord::Host { terminal, .. } = row else {
                continue;
            };
            has_hosts = true;
            let count = match terminal {
                SqlHostTerminal::Parsed { .. } => &mut summary.analysed_files,
                SqlHostTerminal::NativeError => &mut summary.rejected_files,
                SqlHostTerminal::Unavailable(
                    backend_engine::builtin::SqlUnavailable::EngineNotAdmitted,
                ) => &mut summary.no_worker_files,
                SqlHostTerminal::Unavailable(
                    backend_engine::builtin::SqlUnavailable::UnsupportedDialect,
                ) => &mut summary.unsupported_dialect_files,
                SqlHostTerminal::Unavailable(_) => &mut summary.other_unavailable_files,
            };
            *count = count
                .checked_add(1)
                .ok_or_else(|| "SQL status count overflow".to_owned())?;
        }
        match page.next().copied() {
            Some(next) if after != Some(next) => after = Some(next),
            Some(_) => return Err("SQL status page did not advance".to_owned()),
            None => break,
        }
    }
    Ok(has_hosts.then_some(summary))
}

// Fixed-size boxed chunks make each requested allocation layout explicit. The
// logical array length is counted before allocation; no Vec growth or BTree
// node-size assumption participates in the catalog budget.
const SLOTS_PER_CHUNK: usize = 32;
const CHUNKS_PER_GROUP: usize = 64;
const GROUPS_PER_ROOT: usize = 128;
struct SlotChunk<T> {
    values: [Option<T>; SLOTS_PER_CHUNK],
}
struct SlotGroup<T> {
    chunks: [Option<Box<SlotChunk<T>>>; CHUNKS_PER_GROUP],
}
struct SlotRoot<T> {
    groups: [Option<Box<SlotGroup<T>>>; GROUPS_PER_ROOT],
}
pub(super) struct Slots<T> {
    root: Option<Box<SlotRoot<T>>>,
    capacity: usize,
    charged: usize,
}
impl<T> Slots<T> {
    fn empty() -> Self {
        Self {
            root: None,
            capacity: 0,
            charged: 0,
        }
    }
    fn new(
        count: usize,
        remaining: &mut usize,
        bookkeeping: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, String> {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        if count == 0 {
            return Ok(Self::empty());
        }
        if count > MAX_ROWS {
            return Err("SQL slot count exceeds bound".into());
        }
        let chunks = count
            .checked_add(SLOTS_PER_CHUNK - 1)
            .ok_or("SQL slot overflow")?
            / SLOTS_PER_CHUNK;
        let groups = chunks
            .checked_add(CHUNKS_PER_GROUP - 1)
            .ok_or("SQL slot overflow")?
            / CHUNKS_PER_GROUP;
        if groups > GROUPS_PER_ROOT {
            return Err("SQL slot count exceeds bound".into());
        }
        let allocations = 1usize
            .checked_add(groups)
            .and_then(|n| n.checked_add(chunks))
            .ok_or("SQL slot overflow")?;
        let charge = std::mem::size_of::<SlotRoot<T>>()
            .checked_add(
                std::mem::size_of::<SlotGroup<T>>()
                    .checked_mul(groups)
                    .ok_or("SQL slot overflow")?,
            )
            .and_then(|n| n.checked_add(std::mem::size_of::<SlotChunk<T>>().checked_mul(chunks)?))
            .and_then(|n| n.checked_add(bookkeeping.checked_mul(allocations)?))
            .ok_or("SQL slot overflow")?;
        *remaining = remaining
            .checked_sub(charge)
            .ok_or("SQL catalog byte bound")?;
        let mut root = Box::new(SlotRoot {
            groups: std::array::from_fn(|_| None),
        });
        for group_index in 0..groups {
            if cancelled() {
                return Err("SQL read cancelled".into());
            }
            let mut group = Box::new(SlotGroup {
                chunks: std::array::from_fn(|_| None),
            });
            let group_chunks = (chunks - group_index * CHUNKS_PER_GROUP).min(CHUNKS_PER_GROUP);
            for i in 0..group_chunks {
                if cancelled() {
                    return Err("SQL read cancelled".into());
                }
                group.chunks[i] = Some(Box::new(SlotChunk {
                    values: std::array::from_fn(|_| None),
                }));
            }
            root.groups[group_index] = Some(group);
        }
        Ok(Self {
            root: Some(root),
            capacity: count,
            charged: charge,
        })
    }
    fn slot(&self, index: usize) -> Option<&Option<T>> {
        if index >= self.capacity {
            return None;
        }
        let chunk = index / SLOTS_PER_CHUNK;
        Some(
            &self.root.as_ref()?.groups[chunk / CHUNKS_PER_GROUP]
                .as_ref()?
                .chunks[chunk % CHUNKS_PER_GROUP]
                .as_ref()?
                .values[index % SLOTS_PER_CHUNK],
        )
    }
    fn slot_mut(&mut self, index: usize) -> Option<&mut Option<T>> {
        if index >= self.capacity {
            return None;
        }
        let chunk = index / SLOTS_PER_CHUNK;
        Some(
            &mut self.root.as_mut()?.groups[chunk / CHUNKS_PER_GROUP]
                .as_mut()?
                .chunks[chunk % CHUNKS_PER_GROUP]
                .as_mut()?
                .values[index % SLOTS_PER_CHUNK],
        )
    }
    fn put(&mut self, index: usize, value: T) -> Result<(), &'static str> {
        let slot = self.slot_mut(index).ok_or("SQL slot count changed")?;
        if slot.is_some() {
            return Err("duplicate SQL slot");
        }
        *slot = Some(value);
        Ok(())
    }
    fn get(&self, index: usize) -> Option<&T> {
        self.slot(index)?.as_ref()
    }
    fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.slot_mut(index)?.as_mut()
    }
    fn is_full(&self, cancelled: &dyn Fn() -> bool) -> Result<bool, String> {
        for i in 0..self.capacity {
            if cancelled() {
                return Err("SQL read cancelled".into());
            }
            if self.get(i).is_none() {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn swap(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let x = self
            .slot_mut(a)
            .expect("index in range")
            .take()
            .expect("filled slot");
        let y = self
            .slot_mut(b)
            .expect("index in range")
            .replace(x)
            .expect("filled slot");
        *self.slot_mut(a).expect("index in range") = Some(y);
    }
    fn sort_by(
        &mut self,
        compare: impl Fn(&T, &T) -> std::cmp::Ordering,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        // In-place heapsort needs no temporary allocation.
        fn sift<T>(
            slots: &mut Slots<T>,
            start: usize,
            end: usize,
            cmp: &impl Fn(&T, &T) -> std::cmp::Ordering,
            cancelled: &dyn Fn() -> bool,
        ) -> Result<(), String> {
            let mut root = start;
            loop {
                if cancelled() {
                    return Err("SQL read cancelled".into());
                }
                let Some(mut child) = root.checked_mul(2).and_then(|n| n.checked_add(1)) else {
                    break;
                };
                if child >= end {
                    break;
                }
                if child + 1 < end
                    && cmp(slots.get(child).unwrap(), slots.get(child + 1).unwrap()).is_lt()
                {
                    child += 1;
                }
                if !cmp(slots.get(root).unwrap(), slots.get(child).unwrap()).is_lt() {
                    break;
                }
                slots.swap(root, child);
                root = child;
            }
            Ok(())
        }
        for start in (0..self.capacity / 2).rev() {
            sift(self, start, self.capacity, &compare, cancelled)?;
        }
        for end in (1..self.capacity).rev() {
            if cancelled() {
                return Err("SQL read cancelled".into());
            }
            self.swap(0, end);
            sift(self, 0, end, &compare, cancelled)?;
        }
        Ok(())
    }
    pub(super) fn iter(&self) -> SlotsIter<'_, T> {
        SlotsIter {
            slots: self,
            next: 0,
        }
    }
    fn refund(self, remaining: &mut usize) -> Result<(), String> {
        let charge = self.charged;
        drop(self);
        *remaining = remaining.checked_add(charge).ok_or("SQL budget overflow")?;
        Ok(())
    }
}
impl<T> std::ops::Index<usize> for Slots<T> {
    type Output = T;
    fn index(&self, index: usize) -> &T {
        self.get(index).expect("validated SQL slot")
    }
}
pub(super) struct SlotsIter<'a, T> {
    slots: &'a Slots<T>,
    next: usize,
}
impl<'a, T> Iterator for SlotsIter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        while self.next < self.slots.capacity {
            let i = self.next;
            self.next += 1;
            if let Some(value) = self.slots.get(i) {
                return Some(value);
            }
        }
        None
    }
}
impl<'a, T> IntoIterator for &'a Slots<T> {
    type Item = &'a T;
    type IntoIter = SlotsIter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub(super) struct BoundedSqlHost {
    pub(super) key: [u8; 32],
    pub(super) path: String,
    pub(super) dialect: SqlDialect,
    pub(super) input: Option<[u8; 32]>,
    pub(super) source: Option<[u8; 32]>,
    pub(super) producer: Option<[u8; 32]>,
    pub(super) settings: Option<[u8; 32]>,
    pub(super) context: Option<[u8; 32]>,
    pub(super) fact_digest: [u8; 32],
    pub(super) length: Option<u64>,
    pub(super) terminal: SqlHostTerminal,
    expected_facts: u32,
    seen_facts: u32,
    seen_ordinals: Slots<u64>,
}
pub(super) struct BoundedProjectSql {
    pub(super) root: String,
    /// Complete only when the retained selection and latest attempt both support it.
    pub(super) inventory_complete: bool,
    /// The latest attempt observed the retained selection rather than failing.
    pub(super) latest_observed: bool,
    pub(super) implicit: bool,
    pub(super) gaps: SqlGapCounts,
    pub(super) hosts: Slots<BoundedSqlHost>,
    pub(super) facts: Slots<(usize, u32, SqlFact)>,
}
struct ProjectPlan {
    key: [u8; 32],
    selected: bool,
    filled_selection: bool,
    attempt_seen: bool,
    attempt_observed: bool,
    served_selection: Option<[u8; 32]>,
    selection_digest: Option<[u8; 32]>,
    inventory_digest: [u8; 32],
    declared_hosts: usize,
    counted_hosts: usize,
    filled_hosts: usize,
    counted_facts: usize,
    filled_facts: usize,
    sql: Option<BoundedProjectSql>,
}
pub(super) struct SqlProjects {
    plans: Slots<ProjectPlan>,
}
impl SqlProjects {
    fn position(&self, key: &[u8; 32]) -> Option<usize> {
        let (mut low, mut high) = (0, self.plans.capacity);
        while low < high {
            let mid = low + (high - low) / 2;
            match self.plans.get(mid)?.key.cmp(key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
    pub(super) fn get(&self, key: &[u8; 32]) -> Option<&BoundedProjectSql> {
        self.plans.get(self.position(key)?)?.sql.as_ref()
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &BoundedProjectSql> {
        self.plans.iter().filter_map(|plan| plan.sql.as_ref())
    }
}
#[derive(Clone, Copy)]
struct HostIndex {
    key: [u8; 32],
    project: usize,
    host: usize,
}
fn find_host(index: &Slots<HostIndex>, key: &[u8; 32]) -> Option<HostIndex> {
    let (mut low, mut high) = (0, index.capacity);
    while low < high {
        let mid = low + (high - low) / 2;
        let item = *index.get(mid)?;
        match item.key.cmp(key) {
            std::cmp::Ordering::Less => low = mid + 1,
            std::cmp::Ordering::Greater => high = mid,
            std::cmp::Ordering::Equal => return Some(item),
        }
    }
    None
}
fn charge_row(
    row: &SqlAnalysisRecord,
    remaining: &mut usize,
    bookkeeping: usize,
) -> Result<(), &'static str> {
    use backend_engine::builtin::{sql_row_owned_allocation_count, sql_row_owned_payload_bytes};
    let payload = sql_row_owned_payload_bytes(row).ok_or("SQL retained payload overflow")?;
    let allocations =
        sql_row_owned_allocation_count(row).ok_or("SQL retained allocation overflow")?;
    let cost = allocations
        .checked_mul(bookkeeping)
        .and_then(|n| n.checked_add(payload))
        .ok_or("SQL retained allocation overflow")?;
    *remaining = remaining
        .checked_sub(cost)
        .ok_or("SQL catalog byte bound")?;
    Ok(())
}
fn scan_sql(
    relation: &backend_engine::workspace::WorkspacePreflightRelationHandle<
        backend_engine::builtin::SqlAnalysisRelation,
    >,
    remaining: &mut usize,
    bookkeeping: usize,
    cancelled: &dyn Fn() -> bool,
    mut row_fn: impl FnMut(&[u8; 32], SqlAnalysisRecord, &mut usize) -> Result<(), &'static str>,
) -> Result<(), String> {
    use backend_engine::builtin::sql_visit_preflighted;
    let mut after = None;
    let mut rows = 0usize;
    loop {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let mut failure = None;
        let result = sql_visit_preflighted(
            relation,
            after.as_ref(),
            64,
            remaining,
            bookkeeping,
            &mut |key, row, budget| {
                if cancelled() {
                    failure = Some("SQL read cancelled");
                    return Err(backend_store::StoreError::Bounds);
                }
                rows = match rows.checked_add(1) {
                    Some(n) if n <= MAX_ROWS => n,
                    _ => {
                        failure = Some("SQL relation exceeds the read bound");
                        return Err(backend_store::StoreError::Bounds);
                    }
                };
                match row_fn(key, row, budget) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        failure = Some(error);
                        Err(backend_store::StoreError::Bounds)
                    }
                }
            },
        );
        if let Some(error) = failure {
            return Err(error.into());
        }
        let next = result.map_err(|error| error.to_string())?;
        match next {
            None => break,
            Some(key) if after.is_none_or(|previous| previous < key) => after = Some(key),
            Some(_) => return Err("SQL relation cursor did not advance".into()),
        }
    }
    Ok(())
}
fn selected_definition(fact: &SqlFact) -> bool {
    use backend_semantic::sql_analysis::SqlDefinitionAction;
    matches!(fact, SqlFact::Definition(definition) if definition.live_at_head && matches!(definition.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn))
}
// Serialize directly into fixed caller-owned scratch. Decoded canonical SQL
// rows already fit this bound; no serialization Vec or row clone is created.
struct JsonScratch<'a> {
    bytes: &'a mut [u8],
    used: usize,
}
impl std::io::Write for JsonScratch<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let end = self
            .used
            .checked_add(input.len())
            .ok_or(std::io::ErrorKind::WriteZero)?;
        let target = self
            .bytes
            .get_mut(self.used..end)
            .ok_or(std::io::ErrorKind::WriteZero)?;
        target.copy_from_slice(input);
        self.used = end;
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode_sql_json(row: &SqlAnalysisRecord, scratch: &mut [u8]) -> Result<usize, &'static str> {
    let mut output = JsonScratch {
        bytes: scratch,
        used: 0,
    };
    serde_json::to_writer(&mut output, row).map_err(|_| "SQL canonical serialization failed")?;
    Ok(output.used)
}
fn selection_digest(
    project: [u8; 32],
    row: &SqlAnalysisRecord,
    scratch: &mut [u8],
) -> Result<[u8; 32], &'static str> {
    let len = encode_sql_json(row, scratch)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql-analysis.v1.selection\0");
    hash.update(&sql_selection_key(project));
    hash.update(&(len as u64).to_be_bytes());
    hash.update(&scratch[..len]);
    Ok(*hash.finalize().as_bytes())
}
struct HostFactDigest {
    hash: blake3::Hasher,
    count: u32,
}
impl HostFactDigest {
    fn new() -> Self {
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.sql.sqlite.host-facts.v1\0");
        Self { hash, count: 0 }
    }
    fn push(&mut self, key: &[u8; 32], json: &[u8]) -> Result<(), &'static str> {
        self.count = self.count.checked_add(1).ok_or("SQL fact count overflow")?;
        self.hash.update(key);
        self.hash.update(&(json.len() as u64).to_be_bytes());
        self.hash.update(json);
        Ok(())
    }
    fn finish(mut self) -> [u8; 32] {
        self.hash.update(&self.count.to_be_bytes());
        *self.hash.finalize().as_bytes()
    }
}
struct FactDigestRow {
    project: usize,
    host: usize,
    ordinal: u32,
    key: [u8; 32],
    json: Box<[u8]>,
}

fn host_inventory_digest(
    hosts: &Slots<BoundedSqlHost>,
    cancelled: &dyn Fn() -> bool,
) -> Result<[u8; 32], &'static str> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql-analysis.v1.inventory\0");
    let mut previous: Option<&str> = None;
    for host in hosts {
        if cancelled() {
            return Err("SQL read cancelled");
        }
        if previous.is_some_and(|value| value >= host.path.as_str()) {
            return Err("duplicate or unsorted SQL host path");
        }
        hash.update(&(host.path.len() as u64).to_be_bytes());
        hash.update(host.path.as_bytes());
        hash.update(&[match host.dialect {
            SqlDialect::Sqlite => 1,
            SqlDialect::Postgres => 2,
        }]);
        previous = Some(&host.path);
    }
    hash.update(&(hosts.capacity as u64).to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}

fn verify_fact(
    key: &[u8; 32],
    host: &BoundedSqlHost,
    ordinal: u32,
    fact: &SqlFact,
) -> Result<(), &'static str> {
    use backend_engine::builtin::sql_fact_key;
    let (Some(input), Some(source), Some(producer), Some(settings), Some(length)) = (
        host.input,
        host.source,
        host.producer,
        host.settings,
        host.length,
    ) else {
        return Err("selected SQL fact lacks host basis");
    };
    if *key
        != sql_fact_key(
            host.key,
            host.dialect,
            input,
            source,
            producer,
            settings,
            host.context,
            ordinal,
        )
    {
        return Err("SQL fact key does not match host basis");
    }
    use backend_semantic::sql_analysis::SqlCoverage;
    let outline = matches!(
        host.terminal,
        SqlHostTerminal::Parsed {
            source: SqlCoverage::Partial,
            ..
        }
    );
    match fact {
        SqlFact::SqliteRelation { .. } | SqlFact::SqliteOutputLineage { .. }
            if host.dialect != SqlDialect::Sqlite
                || !matches!(
                    host.terminal,
                    SqlHostTerminal::Parsed {
                        contextual: SqlCoverage::Partial,
                        ..
                    }
                ) =>
        {
            return Err("SQL model fact lacks contextual host");
        }
        SqlFact::Diagnostic {
            extent: Some(range),
            ..
        } if u64::from(range.end) > length => return Err("SQL diagnostic extent outside host"),
        SqlFact::Statement { extent, .. } if !outline || u64::from(extent.end) > length => {
            return Err("SQL statement lacks outline host");
        }
        SqlFact::Definition(value)
            if !outline
                || u64::from(value.name_extent.end) > length
                || u64::from(value.extent.end) > length =>
        {
            return Err("SQL definition lacks outline host");
        }
        SqlFact::Reference(value)
            if !outline
                || u64::from(value.extent.end) > length
                || u64::from(value.full_extent.end) > length =>
        {
            return Err("SQL reference lacks outline host");
        }
        SqlFact::PostgresColumnRead { extent, .. }
            if host.dialect != SqlDialect::Postgres
                || host.context.is_none()
                || !matches!(
                    host.terminal,
                    SqlHostTerminal::Parsed {
                        source: SqlCoverage::Partial,
                        contextual: SqlCoverage::Partial,
                        sqlite_lineage: None
                    }
                )
                || u64::from(extent.end) > length =>
        {
            return Err("PostgreSQL read lacks contextual host");
        }
        _ => {}
    }
    Ok(())
}

fn plan_projects<'a>(
    projects: impl ExactSizeIterator<Item = &'a [u8; 32]>,
    remaining: &mut usize,
    allocation_bookkeeping_bytes: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<SqlProjects, String> {
    let mut result = SqlProjects {
        plans: Slots::new(
            projects.len(),
            remaining,
            allocation_bookkeeping_bytes,
            cancelled,
        )?,
    };
    let mut previous = None;
    for (i, key) in projects.copied().enumerate() {
        if previous.is_some_and(|value| value >= key) {
            return Err("SQL project keys are not strictly ordered".into());
        }
        previous = Some(key);
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        result.plans.put(
            i,
            ProjectPlan {
                key,
                selected: false,
                filled_selection: false,
                attempt_seen: false,
                attempt_observed: false,
                served_selection: None,
                selection_digest: None,
                inventory_digest: [0; 32],
                declared_hosts: 0,
                counted_hosts: 0,
                filled_hosts: 0,
                counted_facts: 0,
                filled_facts: 0,
                sql: None,
            },
        )?;
    }
    if !result.plans.is_full(cancelled)? {
        return Err("SQL project key count changed".into());
    }
    Ok(result)
}

/// Read selected SQL projects using one shared requested-allocation ledger.
/// All retained arrays use fixed-size boxed chunks charged before allocation;
/// the caller supplies its explicit per-allocation quota allowance. The quota
/// covers requested layouts, not allocator usable size or an RSS limit.
/// The sorted input project keys and snapshot are caller-owned and must already
/// be accounted for in the caller's remaining budget.
pub(super) fn load_projects_bounded<'a>(
    snapshot: &WorkspaceSnapshot,
    projects: impl ExactSizeIterator<Item = &'a [u8; 32]>,
    cancelled: &dyn Fn() -> bool,
    max_bytes: usize,
    allocation_bookkeeping_bytes: usize,
) -> Result<(SqlProjects, usize), String> {
    use backend_engine::builtin::{sql_analysis_preflight_relation, sql_host_key};
    let mut remaining = max_bytes;
    let mut result = plan_projects(
        projects,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
    )?;
    let Some(relation) = sql_analysis_preflight_relation(snapshot).map_err(|e| e.to_string())?
    else {
        return Ok((result, max_bytes - remaining));
    };

    // This serializer scratch overlaps decoder reservations during each visit.
    // The fixed boxed layout is charged before allocation and released only
    // after all digest checks and scans complete.
    let scratch_charge = backend_engine::builtin::MAX_SQL_ROW_BYTES
        .checked_add(allocation_bookkeeping_bytes)
        .ok_or("SQL scratch overflow")?;
    remaining = remaining
        .checked_sub(scratch_charge)
        .ok_or("SQL catalog byte bound")?;
    let mut digest_scratch = Box::new([0u8; backend_engine::builtin::MAX_SQL_ROW_BYTES]);

    // Count selection/host slots before retaining any owned row.
    scan_sql(
        &relation,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
        |key, row, _| {
            match &row {
                SqlAnalysisRecord::Selection {
                    project,
                    host_count,
                    inventory_digest,
                    policy_complete,
                    discovery,
                    ..
                } => {
                    if let Some(i) = result.position(project) {
                        if *key != sql_selection_key(*project)
                            || (*policy_complete && discovery.is_none())
                        {
                            return Err("invalid SQL selection key or policy");
                        }
                        let digest = selection_digest(*project, &row, &mut digest_scratch[..])?;
                        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
                        if plan.selected {
                            return Err("duplicate SQL selection");
                        }
                        plan.selected = true;
                        plan.selection_digest = Some(digest);
                        plan.inventory_digest = *inventory_digest;
                        plan.declared_hosts =
                            usize::try_from(*host_count).map_err(|_| "SQL host count overflow")?;
                    }
                }
                SqlAnalysisRecord::Attempt {
                    project,
                    outcome,
                    served_selection,
                } => {
                    if let Some(i) = result.position(project) {
                        if *key != backend_engine::builtin::sql_attempt_key(*project) {
                            return Err("SQL attempt key mismatch");
                        }
                        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
                        if plan.attempt_seen {
                            return Err("duplicate SQL attempt");
                        }
                        plan.attempt_seen = true;
                        plan.attempt_observed = matches!(
                            outcome,
                            backend_engine::builtin::SqlAttemptOutcome::Observed
                        );
                        plan.served_selection = *served_selection;
                    }
                }
                SqlAnalysisRecord::Host { project, path, .. } => {
                    if let Some(i) = result.position(project) {
                        if sql_host_key(*project, path).map_err(|_| "invalid SQL host path")?
                            != *key
                        {
                            return Err("SQL host key mismatch");
                        }
                        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
                        plan.counted_hosts = plan
                            .counted_hosts
                            .checked_add(1)
                            .ok_or("SQL host count overflow")?;
                    }
                }
                _ => {}
            }
            Ok(())
        },
    )?;
    let mut total_hosts = 0usize;
    for i in 0..result.plans.capacity {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
        if plan.attempt_seen
            && (plan.served_selection != plan.selection_digest
                || (plan.attempt_observed && !plan.selected))
        {
            return Err("SQL attempt does not bind selected analysis".into());
        }
        if !plan.selected {
            if plan.counted_hosts != 0 {
                return Err("SQL hosts lack selected project".into());
            }
            continue;
        }
        if !plan.attempt_seen {
            return Err("SQL selection missing latest attempt".into());
        }
        if plan.counted_hosts != plan.declared_hosts {
            return Err("SQL selected host count mismatch".into());
        }
        total_hosts = total_hosts
            .checked_add(plan.counted_hosts)
            .ok_or("SQL host count overflow")?;
        let hosts = Slots::new(
            plan.counted_hosts,
            &mut remaining,
            allocation_bookkeeping_bytes,
            cancelled,
        )?;
        plan.sql = Some(BoundedProjectSql {
            root: String::new(),
            inventory_complete: false,
            latest_observed: plan.attempt_observed,
            implicit: false,
            gaps: SqlGapCounts::default(),
            hosts,
            facts: Slots::new(0, &mut remaining, allocation_bookkeeping_bytes, cancelled)?,
        });
    }

    // Move selected roots and hosts into their already charged slots.
    scan_sql(
        &relation,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
        |key, row, budget| {
            match &row {
                SqlAnalysisRecord::Selection { project, .. }
                    if result.position(project).is_some() =>
                {
                    charge_row(&row, budget, allocation_bookkeeping_bytes)?;
                    let SqlAnalysisRecord::Selection {
                        project,
                        source_root,
                        inventory,
                        implicit,
                        gaps,
                        ..
                    } = row
                    else {
                        unreachable!()
                    };
                    let i = result
                        .position(&project)
                        .ok_or("missing SQL project slot")?;
                    if *key != sql_selection_key(project) {
                        return Err("SQL selection key mismatch");
                    }
                    let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
                    if plan.filled_selection {
                        return Err("duplicate SQL selection");
                    }
                    let sql = plan.sql.as_mut().ok_or("SQL selection count changed")?;
                    sql.root = source_root;
                    sql.inventory_complete =
                        sql.latest_observed && matches!(inventory, SqlInventory::Complete);
                    sql.implicit = implicit;
                    sql.gaps = gaps;
                    plan.filled_selection = true;
                }
                SqlAnalysisRecord::Host { project, .. } if result.position(project).is_some() => {
                    charge_row(&row, budget, allocation_bookkeeping_bytes)?;
                    let SqlAnalysisRecord::Host {
                        project,
                        path,
                        dialect,
                        input,
                        source,
                        producer,
                        settings,
                        context,
                        fact_count,
                        fact_digest,
                        length,
                        terminal,
                        ..
                    } = row
                    else {
                        unreachable!()
                    };
                    if sql_host_key(project, &path).map_err(|_| "invalid SQL host path")? != *key {
                        return Err("SQL host key mismatch");
                    }
                    let i = result
                        .position(&project)
                        .ok_or("missing SQL project slot")?;
                    let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
                    let sql = plan.sql.as_mut().ok_or("SQL hosts lack selected project")?;
                    sql.hosts.put(
                        plan.filled_hosts,
                        BoundedSqlHost {
                            key: *key,
                            path,
                            dialect,
                            input,
                            source,
                            producer,
                            settings,
                            context,
                            fact_digest,
                            length,
                            terminal,
                            expected_facts: fact_count,
                            seen_facts: 0,
                            seen_ordinals: Slots::empty(),
                        },
                    )?;
                    plan.filled_hosts = plan
                        .filled_hosts
                        .checked_add(1)
                        .ok_or("SQL host count overflow")?;
                }
                _ => {}
            }
            Ok(())
        },
    )?;
    let mut host_index = Slots::new(
        total_hosts,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
    )?;
    let mut host_index_count = 0usize;
    let mut expected_fact_total = 0usize;
    for i in 0..result.plans.capacity {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
        let Some(sql) = plan.sql.as_mut() else {
            continue;
        };
        if !plan.filled_selection
            || plan.filled_hosts != plan.counted_hosts
            || !sql.hosts.is_full(cancelled)?
        {
            return Err("SQL selection or host slot missing".into());
        }
        sql.hosts.sort_by(|a, b| a.path.cmp(&b.path), cancelled)?;
        if host_inventory_digest(&sql.hosts, cancelled)? != plan.inventory_digest {
            return Err("SQL selected inventory digest mismatch".into());
        }
        for h in 0..sql.hosts.capacity {
            if cancelled() {
                return Err("SQL read cancelled".into());
            }
            if h > 0 && sql.hosts[h - 1].path == sql.hosts[h].path {
                return Err("duplicate SQL host path".into());
            }
            let host = sql.hosts.get_mut(h).ok_or("missing SQL host slot")?;
            host_index.put(
                host_index_count,
                HostIndex {
                    key: host.key,
                    project: i,
                    host: h,
                },
            )?;
            host_index_count += 1;
            let count =
                usize::try_from(host.expected_facts).map_err(|_| "SQL fact count overflow")?;
            expected_fact_total = expected_fact_total
                .checked_add(count)
                .filter(|n| *n <= MAX_ROWS)
                .ok_or("SQL relation exceeds the read bound")?;
            let words = count.checked_add(63).ok_or("SQL fact count overflow")? / 64;
            host.seen_ordinals = Slots::new(
                words,
                &mut remaining,
                allocation_bookkeeping_bytes,
                cancelled,
            )?;
            for word in 0..words {
                if cancelled() {
                    return Err("SQL read cancelled".into());
                }
                host.seen_ordinals.put(word, 0)?;
            }
        }
    }
    if host_index_count != total_hosts || !host_index.is_full(cancelled)? {
        return Err("SQL host index missing".into());
    }
    host_index.sort_by(|a, b| a.key.cmp(&b.key), cancelled)?;
    for i in 1..host_index.capacity {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        if host_index[i - 1].key == host_index[i].key {
            return Err("duplicate SQL host identity".into());
        }
    }

    let mut digest_rows = Slots::new(
        expected_fact_total,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
    )?;
    let mut digest_count = 0usize;
    // Verify every selected host ordinal and count only the live definitions
    // needed by ordinary discovery/query projection.
    scan_sql(
        &relation,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
        |key, row, budget| {
            let SqlAnalysisRecord::Fact {
                host,
                ordinal,
                ref fact,
            } = row
            else {
                return Ok(());
            };
            let Some(index) = find_host(&host_index, &host) else {
                return Ok(());
            };
            let plan = result
                .plans
                .get_mut(index.project)
                .ok_or("missing SQL project slot")?;
            let sql = plan.sql.as_mut().ok_or("missing SQL selected project")?;
            let host = sql
                .hosts
                .get_mut(index.host)
                .ok_or("missing SQL host slot")?;
            if ordinal >= host.expected_facts {
                return Err("SQL fact ordinal outside host count");
            }
            verify_fact(key, host, ordinal, fact)?;
            let word = usize::try_from(ordinal / 64).map_err(|_| "SQL ordinal overflow")?;
            let mask = 1u64 << (ordinal % 64);
            let bits = host
                .seen_ordinals
                .get_mut(word)
                .ok_or("SQL ordinal slot missing")?;
            if *bits & mask != 0 {
                return Err("duplicate SQL fact ordinal");
            }
            *bits |= mask;
            host.seen_facts = host
                .seen_facts
                .checked_add(1)
                .ok_or("SQL fact count overflow")?;
            if selected_definition(fact) {
                if !matches!(host.terminal, SqlHostTerminal::Parsed { .. }) {
                    return Err("SQL definition lacks parsed host terminal");
                }
                plan.counted_facts = plan
                    .counted_facts
                    .checked_add(1)
                    .ok_or("SQL fact count overflow")?;
            }
            let len = encode_sql_json(&row, &mut digest_scratch[..])?;
            let charge = len
                .checked_add(allocation_bookkeeping_bytes)
                .ok_or("SQL digest allocation overflow")?;
            *budget = budget.checked_sub(charge).ok_or("SQL catalog byte bound")?;
            let json = Box::<[u8]>::from(&digest_scratch[..len]);
            digest_rows.put(
                digest_count,
                FactDigestRow {
                    project: index.project,
                    host: index.host,
                    ordinal,
                    key: *key,
                    json,
                },
            )?;
            digest_count = digest_count
                .checked_add(1)
                .ok_or("SQL digest row overflow")?;
            Ok(())
        },
    )?;
    if digest_count != expected_fact_total || !digest_rows.is_full(cancelled)? {
        return Err("SQL digest row missing".into());
    }
    digest_rows.sort_by(
        |a, b| (a.project, a.host, a.ordinal).cmp(&(b.project, b.host, b.ordinal)),
        cancelled,
    )?;
    let mut digest_index = 0usize;
    for i in 0..result.plans.capacity {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
        let Some(sql) = plan.sql.as_mut() else {
            continue;
        };
        for h in 0..sql.hosts.capacity {
            if cancelled() {
                return Err("SQL read cancelled".into());
            }
            let host = sql.hosts.get_mut(h).ok_or("missing SQL host slot")?;
            let mut digest = HostFactDigest::new();
            for ordinal in 0..host.expected_facts {
                if cancelled() {
                    return Err("SQL read cancelled".into());
                }
                let entry = digest_rows
                    .slot_mut(digest_index)
                    .and_then(Option::take)
                    .ok_or("SQL digest row missing")?;
                if (entry.project, entry.host, entry.ordinal) != (i, h, ordinal) {
                    return Err("SQL fact ordinal sequence mismatch".into());
                }
                digest.push(&entry.key, &entry.json)?;
                let refund = entry
                    .json
                    .len()
                    .checked_add(allocation_bookkeeping_bytes)
                    .ok_or("SQL budget overflow")?;
                drop(entry);
                remaining = remaining.checked_add(refund).ok_or("SQL budget overflow")?;
                digest_index += 1;
            }
            if digest.finish() != host.fact_digest {
                return Err("SQL host fact digest mismatch".into());
            }
            if host.seen_facts != host.expected_facts {
                return Err("SQL host facts missing".into());
            }
            let seen = std::mem::replace(&mut host.seen_ordinals, Slots::empty());
            seen.refund(&mut remaining)?;
        }
        sql.facts = Slots::new(
            plan.counted_facts,
            &mut remaining,
            allocation_bookkeeping_bytes,
            cancelled,
        )?;
    }

    if digest_index != expected_fact_total {
        return Err("SQL digest rows remain".into());
    }
    digest_rows.refund(&mut remaining)?;

    // Move only the selected definitions. The decoder's temporary reservation
    // remains live while each retained ownership charge is transferred.
    scan_sql(
        &relation,
        &mut remaining,
        allocation_bookkeeping_bytes,
        cancelled,
        |key, row, budget| {
            let SqlAnalysisRecord::Fact {
                host,
                ordinal,
                ref fact,
            } = row
            else {
                return Ok(());
            };
            let Some(index) = find_host(&host_index, &host) else {
                return Ok(());
            };
            if !selected_definition(fact) {
                return Ok(());
            }
            charge_row(&row, budget, allocation_bookkeeping_bytes)?;
            let SqlAnalysisRecord::Fact { fact, .. } = row else {
                unreachable!()
            };
            let plan = result
                .plans
                .get_mut(index.project)
                .ok_or("missing SQL project slot")?;
            let sql = plan.sql.as_mut().ok_or("missing SQL selected project")?;
            let host = sql.hosts.get(index.host).ok_or("missing SQL host slot")?;
            verify_fact(key, host, ordinal, &fact)?;
            sql.facts
                .put(plan.filled_facts, (index.host, ordinal, fact))?;
            plan.filled_facts = plan
                .filled_facts
                .checked_add(1)
                .ok_or("SQL fact count overflow")?;
            Ok(())
        },
    )?;
    for i in 0..result.plans.capacity {
        if cancelled() {
            return Err("SQL read cancelled".into());
        }
        let plan = result.plans.get_mut(i).ok_or("missing SQL project slot")?;
        let Some(sql) = plan.sql.as_mut() else {
            continue;
        };
        if plan.filled_facts != plan.counted_facts || !sql.facts.is_full(cancelled)? {
            return Err("SQL definition slot missing".into());
        }
        sql.facts
            .sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)), cancelled)?;
        for n in 1..sql.facts.capacity {
            if cancelled() {
                return Err("SQL read cancelled".into());
            }
            if (sql.facts[n - 1].0, sql.facts[n - 1].1) == (sql.facts[n].0, sql.facts[n].1) {
                return Err("duplicate SQL definition ordinal".into());
            }
        }
    }
    host_index.refund(&mut remaining)?;
    drop(digest_scratch);
    remaining = remaining
        .checked_add(scratch_charge)
        .ok_or("SQL budget overflow")?;
    let used = max_bytes
        .checked_sub(remaining)
        .ok_or("SQL budget overflow")?;
    Ok((result, used))
}

#[cfg(test)]
mod bounded_catalog_tests {
    use super::*;
    use backend_semantic::sql_analysis::SqlCoverage;

    #[test]
    fn borrowed_project_plan_rejects_duplicate_unordered_and_short_inputs() {
        struct Short<'a> {
            inner: std::slice::Iter<'a, [u8; 32]>,
        }
        impl<'a> Iterator for Short<'a> {
            type Item = &'a [u8; 32];
            fn next(&mut self) -> Option<Self::Item> {
                self.inner.next()
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                (2, Some(2))
            }
        }
        impl ExactSizeIterator for Short<'_> {
            fn len(&self) -> usize {
                2
            }
        }
        let a = [1; 32];
        let b = [2; 32];
        let valid = [a, b];
        let mut budget = 1024 * 1024;
        let planned = plan_projects(valid.iter(), &mut budget, 64, &|| false).unwrap();
        assert_eq!(planned.position(&a), Some(0));
        assert_eq!(planned.position(&b), Some(1));
        assert!(planned.get(&a).is_none());
        assert!(plan_projects([a, a].iter(), &mut budget, 64, &|| false).is_err());
        assert!(plan_projects([b, a].iter(), &mut budget, 64, &|| false).is_err());
        assert!(
            plan_projects(
                Short {
                    inner: valid[..1].iter()
                },
                &mut budget,
                64,
                &|| false
            )
            .is_err()
        );
    }

    #[test]
    fn slot_arrays_precharge_exact_requested_layout_before_allocation() {
        const N: usize = 33;
        const HEADER: usize = 64;
        let expected = std::mem::size_of::<SlotRoot<u64>>()
            + std::mem::size_of::<SlotGroup<u64>>()
            + 2 * std::mem::size_of::<SlotChunk<u64>>()
            + 4 * HEADER;
        let mut short = expected - 1;
        let failed = allocation_counter::measure(|| {
            assert!(Slots::<u64>::new(N, &mut short, HEADER, &|| false).is_err());
        });
        // Constructing the error String itself may allocate; no slot payload
        // or partial tree is admitted when the precharge fails.
        assert!(failed.bytes_max < std::mem::size_of::<SlotChunk<u64>>() as u64);
        assert_eq!(short, expected - 1);
        for initial in [expected, expected + 1] {
            let mut budget = initial;
            let measured = allocation_counter::measure(|| {
                let slots = Slots::<u64>::new(N, &mut budget, HEADER, &|| false).unwrap();
                assert_eq!(budget, initial - expected);
                assert_eq!(slots.capacity, N);
                std::hint::black_box(slots);
            });
            assert!(
                measured.bytes_max <= (expected - 4 * HEADER) as u64,
                "{measured:?}"
            );
            assert_eq!(measured.count_max, 4);
        }
    }

    #[test]
    fn staged_fact_bytes_and_slots_stay_within_precharged_peak() {
        const N: usize = 512;
        const BOOKKEEPING: usize = 64;
        let initial = 8 * 1024 * 1024;
        let mut budget = initial;
        let mut spent = 0;
        let payload = [b'x'; 255];
        let measured = allocation_counter::measure(|| {
            let mut rows = Slots::new(N, &mut budget, BOOKKEEPING, &|| false).unwrap();
            for ordinal in (0..N).rev() {
                let charge = payload.len() + BOOKKEEPING;
                budget = budget.checked_sub(charge).unwrap();
                let json = Box::<[u8]>::from(payload.as_slice());
                rows.put(
                    N - 1 - ordinal,
                    FactDigestRow {
                        project: 0,
                        host: 0,
                        ordinal: ordinal as u32,
                        key: [1; 32],
                        json,
                    },
                )
                .unwrap();
            }
            rows.sort_by(|a, b| a.ordinal.cmp(&b.ordinal), &|| false)
                .unwrap();
            assert!(rows.iter().map(|row| row.ordinal).eq(0..N as u32));
            spent = initial - budget;
            std::hint::black_box(rows);
        });
        assert!(measured.bytes_max <= spent as u64, "{measured:?} / {spent}");
    }

    #[test]
    fn chunked_sort_checks_completeness_duplicates_and_cancellation() {
        let mut budget = 8 * 1024 * 1024;
        let mut slots = Slots::new(1025, &mut budget, 64, &|| false).unwrap();
        for i in 0..1025 {
            slots.put(i, 1024 - i).unwrap();
        }
        assert!(slots.is_full(&|| false).unwrap());
        assert!(slots.put(0, 9).is_err());
        slots.sort_by(Ord::cmp, &|| false).unwrap();
        assert!(slots.iter().copied().eq(0..1025));
        assert!(slots.sort_by(Ord::cmp, &|| true).is_err());
        let mut missing = Slots::<usize>::new(2, &mut budget, 64, &|| false).unwrap();
        missing.put(0, 1).unwrap();
        assert!(!missing.is_full(&|| false).unwrap());
    }

    #[test]
    fn fixed_serializer_matches_owner_selection_and_fact_digests() {
        let project = [1; 32];
        let selection = SqlAnalysisRecord::Selection {
            project,
            source_root: "/tmp/a\"b".into(),
            config_input: [2; 32],
            config_source: [3; 32],
            config_length: 1,
            discovery: None,
            policy_complete: false,
            implicit: false,
            gaps: SqlGapCounts::default(),
            inventory: SqlInventory::Incomplete,
            inventory_digest: backend_engine::builtin::sql_inventory_digest(&[]).unwrap(),
            host_count: 0,
        };
        let mut scratch = [0u8; backend_engine::builtin::MAX_SQL_ROW_BYTES];
        assert_eq!(
            selection_digest(project, &selection, &mut scratch).unwrap(),
            backend_engine::builtin::sql_selection_digest(project, &selection).unwrap()
        );
        assert_eq!(
            HostFactDigest::new().finish(),
            backend_engine::builtin::sql_host_fact_digest(&[]).unwrap()
        );
        let row = SqlAnalysisRecord::Fact {
            host: [4; 32],
            ordinal: 0,
            fact: SqlFact::Diagnostic {
                code: backend_semantic::sql_analysis::SqlDiagnostic::Other,
                extent: None,
            },
        };
        let key = [5; 32];
        let len = encode_sql_json(&row, &mut scratch).unwrap();
        let mut hash = HostFactDigest::new();
        hash.push(&key, &scratch[..len]).unwrap();
        assert_eq!(
            hash.finish(),
            backend_engine::builtin::sql_host_fact_digest(&[(key, &row)]).unwrap()
        );
    }

    #[test]
    fn fixed_slots_inventory_matches_owner_digest_and_rejects_order_change() {
        fn host(path: &str, dialect: SqlDialect) -> BoundedSqlHost {
            BoundedSqlHost {
                key: [1; 32],
                path: path.into(),
                dialect,
                input: None,
                source: None,
                producer: None,
                settings: None,
                context: None,
                fact_digest: [2; 32],
                length: None,
                terminal: SqlHostTerminal::Unavailable(
                    backend_engine::builtin::SqlUnavailable::Read,
                ),
                expected_facts: 0,
                seen_facts: 0,
                seen_ordinals: Slots::empty(),
            }
        }
        let mut budget = 1024 * 1024;
        let mut hosts = Slots::new(2, &mut budget, 64, &|| false).unwrap();
        hosts.put(0, host("a.sql", SqlDialect::Sqlite)).unwrap();
        hosts.put(1, host("b.sql", SqlDialect::Postgres)).unwrap();
        let expected = backend_engine::builtin::sql_inventory_digest(&[
            ("a.sql".into(), SqlDialect::Sqlite),
            ("b.sql".into(), SqlDialect::Postgres),
        ])
        .unwrap();
        assert_eq!(host_inventory_digest(&hosts, &|| false).unwrap(), expected);
        hosts.swap(0, 1);
        assert!(host_inventory_digest(&hosts, &|| false).is_err());
    }

    #[test]
    fn fact_key_requires_exact_host_basis_and_ordinal() {
        let host = BoundedSqlHost {
            key: [1; 32],
            path: "a.sql".into(),
            dialect: SqlDialect::Sqlite,
            input: Some([2; 32]),
            source: Some([3; 32]),
            producer: Some([4; 32]),
            settings: Some([5; 32]),
            context: Some([6; 32]),
            fact_digest: [7; 32],
            length: Some(10),
            terminal: SqlHostTerminal::Parsed {
                source: SqlCoverage::Partial,
                contextual: SqlCoverage::Partial,
                sqlite_lineage: None,
            },
            expected_facts: 2,
            seen_facts: 0,
            seen_ordinals: Slots::empty(),
        };
        let key = backend_engine::builtin::sql_fact_key(
            host.key,
            host.dialect,
            [2; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            host.context,
            1,
        );
        let fact = SqlFact::Diagnostic {
            code: backend_semantic::sql_analysis::SqlDiagnostic::Other,
            extent: None,
        };
        assert!(verify_fact(&key, &host, 1, &fact).is_ok());
        assert!(verify_fact(&key, &host, 0, &fact).is_err());
        let outside = SqlFact::Diagnostic {
            code: backend_semantic::sql_analysis::SqlDiagnostic::Other,
            extent: Some(backend_semantic::sql_analysis::SqlByteRange { start: 0, end: 11 }),
        };
        assert!(verify_fact(&key, &host, 1, &outside).is_err());
        let mut changed = host;
        changed.terminal = SqlHostTerminal::NativeError;
        assert!(verify_fact(&key, &changed, 1, &fact).is_ok());
        changed.source = Some([8; 32]);
        assert!(verify_fact(&key, &changed, 1, &fact).is_err());
        changed.source = None;
        assert!(verify_fact(&key, &changed, 1, &fact).is_err());
    }
}
