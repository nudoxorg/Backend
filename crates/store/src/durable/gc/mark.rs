//! Bounded mark traversal over authenticated packs, closures, and objects.

use super::super::{FileStore, PackId, StoreError, TypedObject};
use super::index::{MarkIndex, QueueIndex};
use super::roots::QueueItem;
use super::state::{GcLimits, GcState, MarkKey};
use super::state_io::{
    append_mark, append_queue_items, extend_log_digest, freeze_mark_index, persist_state,
    read_queue_item,
};
use super::sweep::GcPaths;
use super::{GC_MARK_DIGEST_DOMAIN, GC_QUEUE_DIGEST_DOMAIN, GC_QUEUE_RECORD_BYTES, Phase};

impl FileStore {
    pub(super) fn mark_page(
        &self,
        state: &mut GcState,
        limits: GcLimits,
        mark_index: &mut MarkIndex,
        queue_index: &mut QueueIndex,
    ) -> Result<(), StoreError> {
        let paths = GcPaths::new(self.root());
        let mut credits = MarkCredits::new(limits);
        let mut writer = QueueWriter::new(&paths, state, limits, queue_index);
        let mut processed = 0usize;
        // A manifest continuation is itself one bounded work item.  Stop the
        // page after admitting it so a queue page cannot multiply its row and
        // byte budgets by processing several continuations back-to-back.
        let mut manifest_page_processed = false;
        while processed < limits.mark_page_items {
            let Some(item) = read_queue_item(&paths.queue, writer.state.queue_offset)? else {
                break;
            };
            credits.charge_row()?;
            let is_manifest_page = self.process_mark_item(
                item,
                limits,
                mark_index,
                &mut writer,
                &mut credits,
                &mut manifest_page_processed,
            )?;
            writer.state.queue_offset = writer
                .state
                .queue_offset
                .checked_add(u64::try_from(GC_QUEUE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?)
                .ok_or(StoreError::Bounds)?;
            processed = processed.checked_add(1).ok_or(StoreError::Bounds)?;
            if is_manifest_page {
                break;
            }
        }
        writer.flush()?;
        if read_queue_item(&paths.queue, writer.state.queue_offset)?.is_none() {
            writer.state.mark_digest = freeze_mark_index(&paths.mark, limits, &paths.dir)?;
            self.prepare_sweep_phase(writer.state, Phase::SweepPacks, limits)?;
        }
        persist_state(&paths, writer.state)
    }

    fn process_mark_item(
        &self,
        item: QueueItem,
        limits: GcLimits,
        mark_index: &mut MarkIndex,
        writer: &mut QueueWriter<'_>,
        credits: &mut MarkCredits,
        manifest_page_processed: &mut bool,
    ) -> Result<bool, StoreError> {
        match item {
            QueueItem::Head { pack, closure } => {
                self.process_pack(pack, limits, mark_index, writer, credits)?;
                self.mark_key(
                    MarkKey::Closure(*closure.as_bytes()),
                    writer.state,
                    limits,
                    mark_index,
                )?;
                self.enqueue_manifest_page(closure, None, limits, writer, credits)?;
                *manifest_page_processed = true;
            }
            QueueItem::Pack(id) => self.process_pack(id, limits, mark_index, writer, credits)?,
            QueueItem::Closure(id) => {
                self.mark_key(
                    MarkKey::Closure(*id.as_bytes()),
                    writer.state,
                    limits,
                    mark_index,
                )?;
                self.enqueue_manifest_page(id, None, limits, writer, credits)?;
                *manifest_page_processed = true;
            }
            QueueItem::ManifestPage { closure, after } => {
                if !mark_index.contains(MarkKey::Closure(*closure.as_bytes()))? {
                    return Err(StoreError::Corrupt);
                }
                self.enqueue_manifest_page(closure, Some(after), limits, writer, credits)?;
                *manifest_page_processed = true;
            }
            QueueItem::Object(id) => {
                credits.charge_file(&self.object_path(id))?;
                let object = self.read_object(id)?;
                self.mark_key(
                    MarkKey::Object(*id.as_bytes()),
                    writer.state,
                    limits,
                    mark_index,
                )?;
                self.enqueue_object_references(&object, 0, writer, credits)?;
            }
            QueueItem::RelationRefs { object, after } => {
                credits.charge_file(&self.object_path(object))?;
                let object = self.read_object(object)?;
                self.enqueue_object_references(&object, after, writer, credits)?;
            }
        }
        Ok(*manifest_page_processed)
    }

    fn process_pack(
        &self,
        id: PackId,
        limits: GcLimits,
        mark_index: &mut MarkIndex,
        writer: &mut QueueWriter<'_>,
        credits: &mut MarkCredits,
    ) -> Result<(), StoreError> {
        let path = self.pack_path(id);
        if path.is_file() {
            credits.charge_file(&path)?;
            self.read_pack(id)?;
        } else {
            credits.charge_file(&self.tree_pack_path(id))?;
            let tree = self.open_tree_publication(id)?.ok_or(StoreError::Corrupt)?;
            let object = self.read_relation_node(tree.root())?;
            writer.push_credited(QueueItem::Object(object.id()), credits)?;
        }
        self.mark_key(
            MarkKey::Pack(*id.as_bytes()),
            writer.state,
            limits,
            mark_index,
        )
    }

    fn mark_key(
        &self,
        key: MarkKey,
        state: &mut GcState,
        limits: GcLimits,
        mark_index: &mut MarkIndex,
    ) -> Result<(), StoreError> {
        let paths = GcPaths::new(self.root());
        if mark_index.contains(key)? {
            return Ok(());
        }
        if state.marked_items >= limits.max_mark_items {
            return Err(StoreError::Bounds);
        }
        append_mark(&paths.mark, key, &paths.dir)?;
        mark_index.insert(key)?;
        state.mark_digest =
            extend_log_digest(state.mark_digest, GC_MARK_DIGEST_DOMAIN, &key.encode());
        state.marked_items = state
            .marked_items
            .checked_add(1)
            .ok_or(StoreError::Bounds)?;
        Ok(())
    }
}

struct QueueWriter<'a> {
    paths: &'a GcPaths,
    state: &'a mut GcState,
    limits: GcLimits,
    index: &'a mut QueueIndex,
    pending: Vec<QueueItem>,
}

/// Credits shared by every queue item handled by one durable mark step.
///
/// Manifest pages use the same byte pool as direct object and pack reads. This
/// keeps a queue record from multiplying the configured page budget by asking
/// a nested manifest page to start over with a fresh allowance.
struct MarkCredits {
    rows: usize,
    bytes: usize,
    probes: usize,
    enqueues: usize,
    refs: usize,
}

impl MarkCredits {
    const fn new(limits: GcLimits) -> Self {
        Self {
            // One queue record admits one manifest page, whose rows use the
            // remaining allowance. Keep that admission in the accounting so
            // a page of `N` objects still fits a call configured for `N`.
            rows: limits.mark_page_items.saturating_add(1),
            bytes: limits.mark_page_bytes,
            // A relation-reference slice probes its source and each admitted
            // child before writing the next continuation record.
            probes: limits.mark_page_items.saturating_mul(4),
            // A manifest row needs one reachability record and may also need
            // a continuation record. Keep this independent from queue rows so
            // reserving the continuation cannot consume the whole page.
            enqueues: limits.mark_page_items.saturating_mul(2),
            refs: limits.mark_page_items,
        }
    }

    fn charge_row(&mut self) -> Result<(), StoreError> {
        self.rows = self.rows.checked_sub(1).ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn charge_rows(&mut self, count: usize) -> Result<(), StoreError> {
        self.rows = self.rows.checked_sub(count).ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn charge_probe(&mut self) -> Result<(), StoreError> {
        self.probes = self.probes.checked_sub(1).ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn charge_enqueue(&mut self) -> Result<(), StoreError> {
        self.enqueues = self.enqueues.checked_sub(1).ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn charge_refs(&mut self, count: usize) -> Result<(), StoreError> {
        self.refs = self.refs.checked_sub(count).ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn charge_page(
        &mut self,
        stats: super::super::nodes::ManifestReadStats,
    ) -> Result<(), StoreError> {
        self.charge_probe()?;
        self.charge(stats.bytes_read)
    }

    fn charge_file(&mut self, path: &std::path::Path) -> Result<(), StoreError> {
        self.charge_probe()?;
        let bytes = std::fs::metadata(path)
            .map_err(|error| super::super::map_read_error(&error))?
            .len();
        let bytes = usize::try_from(bytes).map_err(|_| StoreError::Bounds)?;
        self.bytes = self.bytes.checked_sub(bytes).ok_or(StoreError::Bounds)?;
        Ok(())
    }

    fn charge(&mut self, bytes: usize) -> Result<(), StoreError> {
        self.bytes = self.bytes.checked_sub(bytes).ok_or(StoreError::Bounds)?;
        Ok(())
    }
}

impl<'a> QueueWriter<'a> {
    fn new(
        paths: &'a GcPaths,
        state: &'a mut GcState,
        limits: GcLimits,
        index: &'a mut QueueIndex,
    ) -> Self {
        Self {
            paths,
            state,
            limits,
            index,
            pending: Vec::with_capacity(limits.mark_page_items),
        }
    }

    fn push_credited(
        &mut self,
        item: QueueItem,
        credits: &mut MarkCredits,
    ) -> Result<(), StoreError> {
        credits.charge_enqueue()?;
        self.pending.push(item);
        if self.pending.len() >= self.limits.mark_page_items {
            self.flush()?;
        }
        Ok(())
    }

    fn push_structural(&mut self, item: QueueItem) -> Result<(), StoreError> {
        self.pending.push(item);
        if self.pending.len() >= self.limits.mark_page_items {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), StoreError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let appended = append_queue_items(self.paths, &self.pending, self.limits, self.index)?;
        for item in appended {
            self.state.queue_digest = extend_log_digest(
                self.state.queue_digest,
                GC_QUEUE_DIGEST_DOMAIN,
                &item.encode(),
            );
        }
        self.pending.clear();
        Ok(())
    }
}

impl FileStore {
    fn enqueue_manifest_page(
        &self,
        id: super::super::ClosureId,
        after: Option<super::super::ObjectId>,
        limits: GcLimits,
        writer: &mut QueueWriter<'_>,
        credits: &mut MarkCredits,
    ) -> Result<(), StoreError> {
        let manifest = self.open_closure(id)?;
        let page_limit = limits
            .mark_page_items
            .min(credits.rows)
            .min(credits.refs)
            .min(credits.enqueues.saturating_sub(1))
            .max(1);
        let page = manifest.page_with_budget(after, page_limit, credits.bytes)?;
        credits.charge_page(page.stats())?;
        credits.charge_rows(page.objects().len())?;
        credits.charge_refs(page.objects().len())?;
        for node_id in page.node_ids() {
            // Node identities are already covered by the page probe and are
            // persisted as reachability records without consuming the object
            // row allowance a second time.
            writer.push_structural(QueueItem::Object(*node_id))?;
        }
        for object in page.objects() {
            self.enqueue_object(object, writer, credits)?;
        }
        if let Some(next) = page.next() {
            if after.is_some_and(|previous| next <= previous) {
                return Err(StoreError::Corrupt);
            }
            writer.push_credited(
                QueueItem::ManifestPage {
                    closure: id,
                    after: next,
                },
                credits,
            )?;
        }
        Ok(())
    }

    fn enqueue_object(
        &self,
        object: &TypedObject,
        writer: &mut QueueWriter<'_>,
        credits: &mut MarkCredits,
    ) -> Result<(), StoreError> {
        object.verify_wire_version(self.relation_registry())?;
        writer.push_credited(QueueItem::Object(object.id()), credits)?;
        self.enqueue_object_references(object, 0, writer, credits)
    }

    fn enqueue_object_references(
        &self,
        object: &TypedObject,
        after: u64,
        writer: &mut QueueWriter<'_>,
        credits: &mut MarkCredits,
    ) -> Result<(), StoreError> {
        object.verify_wire_version(self.relation_registry())?;
        // Registered relation schemas carry their child versions and raw
        // value references in the authenticated node grammar. Ordinary typed
        // payloads are leaves with no product-specific parsing here.
        if self.relation_registry().contains_schema(object.schema()) {
            let references = self.relation_registry().node_references(
                object.schema(),
                object.version(),
                object.bytes(),
            )?;
            let total = references
                .children
                .len()
                .checked_add(references.value_references.len())
                .ok_or(StoreError::Bounds)?;
            let start = usize::try_from(after).map_err(|_| StoreError::Bounds)?;
            if start > total {
                return Err(StoreError::Corrupt);
            }
            let available = credits.refs.min(credits.enqueues.saturating_sub(1));
            let end = start
                .checked_add(available)
                .ok_or(StoreError::Bounds)?
                .min(total);
            credits.charge_refs(end.checked_sub(start).ok_or(StoreError::Bounds)?)?;
            for index in start..end {
                if index < references.children.len() {
                    let child_version = references.children[index];
                    let child_id = self.read_relation_ref(object.schema(), &child_version)?;
                    credits.charge_file(&self.object_path(child_id))?;
                    let child = self.read_object(child_id)?;
                    if child.schema() != object.schema() || child.version() != &child_version {
                        return Err(StoreError::Corrupt);
                    }
                    writer.push_credited(QueueItem::Object(child.id()), credits)?;
                } else {
                    let value_reference =
                        references.value_references[index - references.children.len()];
                    writer.push_credited(
                        QueueItem::Object(super::super::ObjectId::from_bytes(value_reference)),
                        credits,
                    )?;
                }
            }
            if end < total {
                writer.push_credited(
                    QueueItem::RelationRefs {
                        object: object.id(),
                        after: u64::try_from(end).map_err(|_| StoreError::Bounds)?,
                    },
                    credits,
                )?;
            }
        }
        Ok(())
    }
}
