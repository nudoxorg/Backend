//! Detached, metered replay of selected native compiler evidence.
use std::cell::Cell;

use backend_engine::builtin::{NativeSourceEvidence, ProductSemanticCaptureOutcome, ProductSemanticPublicationRecord, SemanticPublicationClaim};
use backend_extension_turso::{SelectedGeneration, SelectedNativeHeaderBudget, SelectedPublicationWire, visit_selected_publication_wire};
use backend_semantic::ir::{BorrowedNativeProgramSourceManifest, BorrowedNativeProgramSourceRef, BorrowedProgramGraph, ImageProvenance, RetainedIndexedNativeProgramSourceManifest, RetainedProgramGraph, SemanticCoreReader, SemanticImageAdmissionMeter, SemanticImageView};
use backend_store::FileStore;

use super::{charge_native_header_work, recovered_publication_coverage};
use super::super::{BuiltinModelError, profile::BuiltinCaptureChange};

type Result<T> = std::result::Result<T, String>;

fn step(budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool, units: u64) -> Result<()> {
    budget.checkpoint(units, stopped).map_err(|e| format!("native replay stopped: {e:?}"))
}
fn eq(a: &[u8], b: &[u8], budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<bool> {
    if a.len() != b.len() { return Ok(false); }
    for (a, b) in a.chunks(1024).zip(b.chunks(1024)) {
        step(budget, stopped, 1)?;
        if a != b { return Ok(false); }
    }
    Ok(true)
}
fn cmp(a: &[u8], b: &[u8], budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<std::cmp::Ordering> {
    for (a, b) in a.chunks(1024).zip(b.chunks(1024)) {
        step(budget, stopped, 1)?;
        let order = a.cmp(b);
        if order != std::cmp::Ordering::Equal { return Ok(order); }
    }
    Ok(a.len().cmp(&b.len()))
}
fn index_at(index: &[u8], position: usize) -> usize {
    let at = position * 4;
    u32::from_le_bytes(index[at..at + 4].try_into().expect("bounded index")) as usize
}
fn swap_index(index: &mut [u8], a: usize, b: usize) {
    for offset in 0..4 { index.swap(a * 4 + offset, b * 4 + offset); }
}
fn build_index(
    profile: backend_semantic::vocabulary::LanguageProfile, rows: &[&NativeSourceEvidence], project: &[u8; 32], duplicate_error: &'static str,
    budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool,
) -> Result<(Box<[u8]>, usize)> {
    let bytes = rows.len().checked_mul(4).ok_or("native replay index overflow")?;
    let mut index = allocate(bytes, budget, stopped)?;
    let mut count = 0usize;
    for (ordinal, row) in rows.iter().enumerate() {
        step(budget, stopped, 1)?;
        if &row.project == project && row.profile == profile {
            let value = u32::try_from(ordinal).map_err(|_| "native replay row overflow")?;
            index[count * 4..count * 4 + 4].copy_from_slice(&value.to_le_bytes());
            count += 1;
        }
    }
    fn sift(index: &mut [u8], rows: &[&NativeSourceEvidence], mut root: usize, end: usize, budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<()> {
        loop {
            step(budget, stopped, 1)?;
            let Some(mut child) = root.checked_mul(2).and_then(|n| n.checked_add(1)) else { return Ok(()); };
            if child >= end { return Ok(()); }
            let key = |pos| rows[index_at(index, pos)].path.as_bytes();
            if child + 1 < end && cmp(key(child), key(child + 1), budget, stopped)?.is_lt() { child += 1; }
            if !cmp(key(root), key(child), budget, stopped)?.is_lt() { return Ok(()); }
            swap_index(index, root, child);
            root = child;
        }
    }
    for root in (0..count / 2).rev() { sift(&mut index, rows, root, count, budget, stopped)?; }
    for end in (1..count).rev() {
        step(budget, stopped, 1)?;
        swap_index(&mut index, 0, end);
        sift(&mut index, rows, 0, end, budget, stopped)?;
    }
    for i in 1..count {
        step(budget, stopped, 1)?;
        if cmp(rows[index_at(&index, i - 1)].path.as_bytes(), rows[index_at(&index, i)].path.as_bytes(), budget, stopped)?.is_eq() {
            return Err(duplicate_error.into());
        }
    }
    Ok((index, count))
}
fn find_index(rows: &[&NativeSourceEvidence], index: &[u8], count: usize, path: &[u8], budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<Option<usize>> {
    let mut low = 0usize;
    let mut high = count;
    while low < high {
        step(budget, stopped, 1)?;
        let middle = low + (high - low) / 2;
        if cmp(rows[index_at(index, middle)].path.as_bytes(), path, budget, stopped)?.is_lt() { low = middle + 1; } else { high = middle; }
    }
    if low < count {
        let ordinal = index_at(index, low);
        if cmp(rows[ordinal].path.as_bytes(), path, budget, stopped)?.is_eq() { return Ok(Some(ordinal)); }
    }
    Ok(None)
}
fn allocate(len: usize, budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<Box<[u8]>> {
    step(budget, stopped, u64::try_from(len.div_ceil(1024)).unwrap_or(u64::MAX))?;
    budget.reserve_retained(len).map_err(|e| format!("native replay memory: {e:?}"))?;
    backend_store::try_allocate_exact_zeroed_bytes(len).map_err(|e| format!("native replay allocation: {e:?}"))
}
fn allocate_u32(count: usize, budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<Vec<u32>> {
    let bytes = count.checked_mul(std::mem::size_of::<u32>()).ok_or("native replay header index overflow")?;
    step(budget, stopped, u64::try_from(bytes.div_ceil(1024)).unwrap_or(u64::MAX))?;
    budget.reserve_retained(bytes).map_err(|e| format!("native replay header index memory: {e:?}"))?;
    let mut credit = bytes;
    backend_store::try_allocate_exact_zeroed_u32_budgeted(count, &mut credit)
        .map_err(|e| format!("native replay header index allocation: {e:?}"))
}

fn copy(bytes: &[u8], budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<Box<[u8]>> {
    let mut out = allocate(bytes.len(), budget, stopped)?;
    for (dst, src) in out.chunks_mut(1024).zip(bytes.chunks(1024)) {
        step(budget, stopped, 1)?;
        dst.copy_from_slice(src);
    }
    Ok(out)
}

enum Program {
    Single { value: RetainedIndexedNativeProgramSourceManifest },
    Forest { value: RetainedProgramGraph, _nodes: Box<[u8]>, _leaf: Box<[u8]> },
}
impl Program {
    fn identity(&self) -> [u8; 32] { match self { Self::Single { value, .. } => value.program(), Self::Forest { value, .. } => value.membership_identity() } }
    fn toolchain(&self) -> backend_version::ContentId<backend_version::ToolchainDomain> { match self { Self::Single { value, .. } => value.toolchain(), Self::Forest { value, .. } => value.toolchain() } }
    fn visit(&self, budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool, mut visit: impl FnMut(BorrowedNativeProgramSourceRef<'_>) -> Result<()>) -> Result<()> {
        match self {
            Self::Single { value, .. } => value.visit_sources(|n| step(budget, stopped, n), &mut visit),
            Self::Forest { value, .. } => value.visit_sources(|n| step(budget, stopped, n), |row| visit(row.row)),
        }
    }
    fn source_for_package_bytes(&self, path: &[u8], budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<Option<backend_semantic::ir::SourceIdentity>> {
        match self {
            Self::Single { value } => value.source_for_package_bytes(path, |n| step(budget, stopped, n)),
            Self::Forest { value, .. } => value.source_for_package_bytes(path, |n| step(budget, stopped, n)),
        }
    }
    fn image_matches_bytes(&self, path: &[u8], identity: &[u8; 32], budget: &SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<bool> {
        match self { Self::Single { .. } => Ok(true), Self::Forest { value, .. } => value.image_matches_bytes(path, identity, |n| step(budget, stopped, n)) }
    }
}
struct Meter<'a> { budget: &'a SelectedNativeHeaderBudget, stopped: &'a dyn Fn() -> bool, free: Cell<usize> }
impl SemanticImageAdmissionMeter for Meter<'_> {
    fn work(&self, units: u64) -> bool { self.budget.checkpoint(units, self.stopped).is_ok() }
    fn reserve(&self, bytes: usize) -> bool { match self.free.get().checked_sub(bytes) { Some(left) => { self.free.set(left); true }, None => false } }
    fn release(&self, bytes: usize) { self.free.set(self.free.get().checked_add(bytes).expect("bounded scratch reservation")); }
}

struct Plan {
    claim: bool,
    program: Option<Program>,
    headers: Option<(Box<[u8]>, [u8; 32])>,
    lines: Option<Box<[u8]>>,
    flags: Box<[u8]>,
    scratch_credit: usize,
}

/// Perform both passes entirely before returning; no staged result mutates the workspace.
pub(super) fn admit(
    store: &FileStore, selected: &SelectedGeneration, capture: &BuiltinCaptureChange,
    rows: &[&NativeSourceEvidence], retirements: &[&NativeSourceEvidence],
    budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool,
) -> std::result::Result<(), BuiltinModelError> {
    admit_inner(store, selected, capture, rows, retirements, budget, stopped).map_err(BuiltinModelError)
}
fn admit_inner(
    store: &FileStore, selected: &SelectedGeneration, capture: &BuiltinCaptureChange,
    rows: &[&NativeSourceEvidence], retirements: &[&NativeSourceEvidence],
    budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool,
) -> Result<()> {
    let ProductSemanticCaptureOutcome::Published { coverage, claim } = capture.outcome else { return Err("native replay capture is not published".into()); };
    if selected.input_digest() != capture.capture.input_digest() { return Err("native replay selected input differs".into()); }
    let project = capture.key.package_key().to_bytes();
    let mut flags = allocate(rows.len(), budget, stopped)?;
    let (source_index, count) = build_index(capture.key.profile(), rows, &project, "duplicate native replay source path", budget, stopped)?;
    let (retired_index, retired_count) = build_index(capture.key.profile(), retirements, &project, "duplicate native replay retirement path", budget, stopped)?;
    if count == 0 { return Err("native replay retirement requires a nonempty selected Program".into()); }
    for i in 0..count {
        step(budget, stopped, 1)?;
        let ordinal = index_at(&source_index, i);
        flags[ordinal] = 1;
        if find_index(retirements, &retired_index, retired_count, rows[ordinal].path.as_bytes(), budget, stopped)?.is_some() {
            return Err("duplicate native replay retirement path".into());
        }
    }
    let mut plan = Plan { claim: false, program: None, headers: None, lines: None, flags, scratch_credit: 0 };
    visit_selected_publication_wire(store, selected, budget, stopped, 128 * 1024 * 1024, 4096, |item, budget| {
        match item {
            SelectedPublicationWire::Metadata { manifest_bytes, binding_bytes, declared_fragment_count, closure_member_count } => {
                if closure_member_count > 8192 { return Err("native replay metadata closure exceeds admission limit".into()); }
                let facts = backend_engine::publication::manifest::validate_manifest_facts_checkpointed(manifest_bytes, |n| charge_native_header_work(budget, stopped, n)).map_err(|e| format!("native replay manifest: {e:?}"))?;
                if facts.fragment_count != declared_fragment_count { return Err("native replay manifest count differs".into()); }
                step(budget, stopped, 1)?;
                let binding = backend_engine::publication::binding::CompilationBindingView::validate(binding_bytes).map_err(|e| format!("native replay binding: {e}"))?;
                let observed = SemanticPublicationClaim::admit(facts, *binding).map_err(str::to_owned)?;
                let actual_coverage = recovered_publication_coverage(selected.observation().observation().value(), facts.fragment_count).map_err(|e| e.0)?;
                if observed != claim || (ProductSemanticPublicationRecord::Published { coverage: actual_coverage, claim: observed }) != (ProductSemanticPublicationRecord::Published { coverage, claim }) { return Err("native replay compiler history differs from capture".into()); }
                plan.claim = true;
            }
            SelectedPublicationWire::Program { member, schema, bytes } => {
                if !plan.claim || plan.program.is_some() { return Err("native replay Program order differs".into()); }
                let retained = copy(bytes, budget, stopped)?;
                let program = if schema == backend_version::SchemaIdentity::new(0x7a, 0xc009, 1) {
                    let size = BorrowedNativeProgramSourceManifest::scratch_bytes(&retained).map_err(|e| format!("native Program scratch: {e:?}"))?;
                    let scratch = allocate(size, budget, stopped)?;
                    let value = RetainedIndexedNativeProgramSourceManifest::admit(retained, scratch, |n| step(budget, stopped, n)).map_err(|e| format!("native Program: {e:?}"))?;
                    Program::Single { value }
                } else if schema == backend_version::SchemaIdentity::new(0x7a, 0xc009, 2) {
                    let shape = BorrowedProgramGraph::scratch(&retained, |n| step(budget, stopped, n)).map_err(|e| format!("native Program shape: {e:?}"))?;
                    let mut nodes = allocate(shape.node_bytes(), budget, stopped)?;
                    let rows = allocate(shape.row_bytes(), budget, stopped)?;
                    let mut leaf = allocate(shape.leaf_paths, budget, stopped)?;
                    let value = RetainedProgramGraph::admit(retained, rows, &mut nodes, &mut leaf, |n| step(budget, stopped, n)).map_err(|e| format!("native Program graph: {e:?}"))?;
                    Program::Forest { value, _nodes: nodes, _leaf: leaf }
                } else { return Err("native Program schema differs".into()); };
                if &program.identity() != member.membership_identity() { return Err("native Program identity differs".into()); }
                plan.program = Some(program);
            }
            SelectedPublicationWire::Image { .. } => { if plan.program.is_none() { return Err("native replay Program unavailable".into()); } }
            SelectedPublicationWire::Headers { member, bytes } => { plan.headers = Some((copy(bytes, budget, stopped)?, *member.object_id())); }
            SelectedPublicationWire::Lines { bytes, .. } => { plan.lines = Some(copy(bytes, budget, stopped)?); }
        }
        Ok(())
    })?;
    if !plan.claim { return Err("native replay metadata absent".into()); }
    let program = plan.program.as_ref().ok_or("native replay Program absent")?;
    // Program membership is a separate authority plane from image ownership.
    // The flags borrow the existing evidence rows; no caller data is copied.
    program.visit(budget, stopped, |entry| {
        let Some(path) = entry.package_path else { return Ok(()); };
        if find_index(retirements, &retired_index, retired_count, path, budget, stopped)?.is_some() {
            return Err("native replay retired path remains in Program".into());
        }
        if let Some(i) = find_index(rows, &source_index, count, path, budget, stopped)? {
            let row = rows[i];
            if plan.flags[i] & 2 != 0 || entry.source.byte_len != row.byte_length || entry.source.identity.as_ref() != row.source_digest.as_slice() {
                return Err("native replay Program source differs from evidence".into());
            }
            plan.flags[i] |= 2;
        }
        Ok(())
    })?;
    let headers = plan.headers.as_ref().map(|(wire, digest)| {
        let mut checkpoint = |n| charge_native_header_work(budget, stopped, n);
        backend_semantic::native_ts_headers::NativeTsHeaderAggregateView::parse_canonical_checkpointed(wire, &mut checkpoint)
            .map(|view| (view, *digest)).map_err(|e| format!("native replay headers: {e:?}"))
    }).transpose()?;
    let mut header_slots = match headers.as_ref() {
        Some((view, _)) => allocate_u32(view.len(), budget, stopped)?,
        None => Vec::new(),
    };
    let header_index = headers.as_ref().map(|(view, digest)| {
        let mut checkpoint = |n| charge_native_header_work(budget, stopped, n);
        view.index_files_checkpointed(&mut header_slots, &mut checkpoint)
            .map(|index| (index, *digest)).map_err(|e| format!("native replay header index: {e:?}"))
    }).transpose()?;
    let lines = plan.lines.as_ref().map(|wire| {
        let mut checkpoint = |n| charge_native_header_work(budget, stopped, n);
        backend_semantic::native_source_lines::NativeSourceLineAggregateView::parse(wire, &mut checkpoint)
            .map_err(|e| format!("native replay line maps: {e:?}"))
    }).transpose()?;
    // Older selected metadata has no separate line aggregate; retain its
    // already-admitted inline evidence. For v10, walk the selected aggregate
    // once and match each file through the precharged sorted evidence index.
    if let Some(aggregate) = &lines {
        let mut files = aggregate.files();
        while files.len() != 0 {
            step(budget, stopped, 1)?;
            let file = files.next().ok_or("native replay line iteration ended early")?;
            let Some(i) = find_index(rows, &source_index, count, file.path.as_bytes(), budget, stopped)? else { continue; };
            let row = rows[i];
            let Some(expected) = &row.line_starts else { continue; };
            if file.source.byte_len != row.byte_length || file.source.identity.as_ref() != row.source_digest.as_slice() || file.line_count() != expected.len() {
                return Err("native replay line source differs".into());
            }
            for (index, expected) in expected.iter().copied().enumerate() {
                step(budget, stopped, 1)?;
                if file.line_start(index) != Some(expected) { return Err("native replay line offsets differ".into()); }
            }
            plan.flags[i] |= 16;
        }
        for i in 0..count {
            step(budget, stopped, 1)?;
            let ordinal = index_at(&source_index, i);
            if rows[ordinal].line_starts.is_some() && plan.flags[ordinal] & 16 == 0 {
                return Err("native replay line file absent".into());
            }
        }
    }
    let mut seen_claim = false;
    let mut seen_program = false;
    visit_selected_publication_wire(store, selected, budget, stopped, 128 * 1024 * 1024, 4096, |item, budget| {
        match item {
            SelectedPublicationWire::Metadata { manifest_bytes, binding_bytes, declared_fragment_count, closure_member_count } => {
                if closure_member_count > 8192 { return Err("native replay metadata closure exceeds admission limit".into()); }
                let facts = backend_engine::publication::manifest::validate_manifest_facts_checkpointed(manifest_bytes, |n| charge_native_header_work(budget, stopped, n)).map_err(|e| format!("native replay manifest: {e:?}"))?;
                let binding = backend_engine::publication::binding::CompilationBindingView::validate(binding_bytes).map_err(|e| format!("native replay binding: {e}"))?;
                let observed = SemanticPublicationClaim::admit(facts, *binding).map_err(str::to_owned)?;
                let actual_coverage = recovered_publication_coverage(selected.observation().observation().value(), facts.fragment_count).map_err(|e| e.0)?;
                if facts.fragment_count != declared_fragment_count || observed != claim || actual_coverage != coverage { return Err("native replay compiler history differs from capture".into()); }
                seen_claim = true;
            }
            SelectedPublicationWire::Program { member, .. } => {
                if !seen_claim || &program.identity() != member.membership_identity() { return Err("native replay Program changed".into()); }
                seen_program = true;
            }
            SelectedPublicationWire::Image { member, bytes } => {
                if !seen_program { return Err("native replay Program unavailable".into()); }
                let preflight = Meter { budget, stopped, free: Cell::new(0) };
                let scratch = backend_semantic::ir::semantic_image_admission_scratch(bytes, &preflight).map_err(|e| format!("native replay image preflight: {e:?}"))?;
                if scratch > plan.scratch_credit {
                    budget.reserve_retained(scratch - plan.scratch_credit).map_err(|e| format!("native replay image memory: {e:?}"))?;
                    plan.scratch_credit = scratch;
                }
                let meter = Meter { budget, stopped, free: Cell::new(plan.scratch_credit) };
                let view = SemanticImageView::reopen_bounded(bytes, &meter).map_err(|e| format!("native replay image: {e:?}"))?;
                let ImageProvenance::Captured { source, recipe, scope, .. } = view.image_facts().provenance else { return Err("selected program image has no provenance".into()); };
                let path = view.atom(scope.path).ok_or("native replay image path absent")?;
                if program.source_for_package_bytes(path, budget, stopped)? != Some(source)
                    || recipe.profile.language() != backend_semantic::vocabulary::Language::TypeScript
                    || recipe.toolchain != program.toolchain() {
                    return Err("selected program differs from image source or recipe".into());
                }
                if !program.image_matches_bytes(path, member.semantic_image_identity(), budget, stopped)? { return Err("native replay image differs from sealed Program".into()); }
                let Some(index) = find_index(rows, &source_index, count, path, budget, stopped)? else { return Ok(()); };
                let row = rows[index];
                if plan.flags[index] & 4 != 0 { return Err("duplicate native replay image path".into()); }
                if !row.admits_image() { return Err("native replay Program member unexpectedly has an image".into()); }
                capture.key.admit_image_checkpointed(&view, |n| step(budget, stopped, n)).map_err(|e| format!("bind native replay image: {e:?}"))?;
                if plan.flags[index] & 2 == 0 || source.byte_len != row.byte_length || source.identity.as_ref() != row.source_digest.as_slice()
                    || recipe.profile != row.profile || recipe.tool != backend_semantic::vocabulary::NativeTool::TypeScriptCompiler || recipe.toolchain != program.toolchain() {
                    return Err("native replay image differs from Program or evidence".into());
                }
                let mut checkpoint = |n| charge_native_header_work(budget, stopped, n);
                if let Some(claimed) = &row.headers {
                    let (index, digest) = header_index.as_ref().ok_or("native replay header witness absent")?;
                    let file = super::super::native_ts_headers::witnessed_indexed_file_borrowed(row, index, *digest, &mut checkpoint)
                        .map_err(|e| format!("native replay header: {e}"))?;
                    super::super::native_ts_headers::admit_image_borrowed(row, claimed, file, &view, bytes, &mut checkpoint)
                        .map_err(|e| format!("native replay header: {e}"))?;
                    plan.flags[index] |= 8;
                } else if let Some((index, _)) = &header_index {
                    if index.find_file_checkpointed(&row.path, &mut checkpoint)?.is_some() { return Err("native replay lost compiler header reference".into()); }
                }
                plan.flags[index] |= 4;
            }
            SelectedPublicationWire::Headers { .. } | SelectedPublicationWire::Lines { .. } => {}
        }
        Ok(())
    })?;
    if !seen_claim || !seen_program { return Err("native replay selected members absent".into()); }
    if let Some((aggregate, _)) = &headers {
        for file in aggregate.files() {
            step(budget, stopped, 1)?;
            let found = find_index(rows, &source_index, count, file.path.as_bytes(), budget, stopped)?
                .is_some_and(|i| plan.flags[i] & 8 != 0);
            if !found { return Err("native replay did not consume compiler header files".into()); }
        }
    }
    for (i, row) in rows.iter().enumerate() {
        step(budget, stopped, 1)?;
        if plan.flags[i] & 1 == 0 { continue; }
        if plan.flags[i] & 2 == 0 || (row.admits_image() && plan.flags[i] & 4 == 0) {
            return Err("native replay source lacks its claimed Program evidence".into());
        }
    }
    step(budget, stopped, 1)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::builtin::NativeSourceEvidenceKind;
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    #[test]
    fn evidence_index_bounds_memory_work_duplicates_and_stop() {
        let project = [7; 32];
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let rows: Vec<_> = (0..4096).rev().map(|i| NativeSourceEvidence {
            project, profile, path: format!("src/{i:04}.ts"), input_digest: [1; 32],
            source_digest: [2; 32], byte_length: 1, kind: NativeSourceEvidenceKind::Image,
            line_starts: None, headers: None,
        }).collect();
        let refs: Vec<_> = rows.iter().collect();
        let bytes = refs.len() * 4;
        let mut budget = SelectedNativeHeaderBudget::new(bytes, 2_000_000);
        let (index, count) = build_index(profile, &refs, &project, "duplicate", &mut budget, &|| false).expect("bounded index");
        assert_eq!(count, refs.len());
        assert_eq!(budget.remaining_memory(), 0);
        for value in [0, 1, 1024, 4095] {
            let path = format!("src/{value:04}.ts");
            let found = find_index(&refs, &index, count, path.as_bytes(), &budget, &|| false).expect("lookup").expect("present");
            assert_eq!(refs[found].path, path);
        }
        assert!(find_index(&refs, &index, count, b"src/9999.ts", &budget, &|| false).unwrap().is_none());
        let mut short = SelectedNativeHeaderBudget::new(bytes - 1, 2_000_000);
        assert!(build_index(profile, &refs, &project, "duplicate", &mut short, &|| false).is_err());
        let calls = Cell::new(0usize);
        let stop = || { let next = calls.get() + 1; calls.set(next); next >= 10 };
        let mut cancelled = SelectedNativeHeaderBudget::new(bytes, 2_000_000);
        assert!(build_index(profile, &refs, &project, "duplicate", &mut cancelled, &stop).is_err());
        assert_eq!(calls.get(), 10);
        let duplicate = [&rows[0], &rows[0]];
        let mut budget = SelectedNativeHeaderBudget::new(8, 100);
        assert_eq!(build_index(profile, &duplicate, &project, "duplicate", &mut budget, &|| false).err().as_deref(), Some("duplicate"));
    }
}
