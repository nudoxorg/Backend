//! Detached, metered replay of selected native compiler evidence.
use std::cell::Cell;

use backend_engine::builtin::{NativeSourceEvidence, ProductSemanticCaptureOutcome, ProductSemanticPublicationRecord, SemanticPublicationClaim};
use backend_extension_turso::{SelectedGeneration, SelectedNativeHeaderBudget, SelectedPublicationWire, visit_selected_publication_wire};
use backend_semantic::ir::{BorrowedNativeProgramSourceManifest, BorrowedNativeProgramSourceRef, BorrowedProgramGraph, ImageProvenance, RetainedNativeProgramSourceManifest, RetainedProgramGraph, SemanticCoreReader, SemanticImageAdmissionMeter, SemanticImageView};
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
fn allocate(len: usize, budget: &mut SelectedNativeHeaderBudget, stopped: &dyn Fn() -> bool) -> Result<Box<[u8]>> {
    step(budget, stopped, u64::try_from(len.div_ceil(1024)).unwrap_or(u64::MAX))?;
    budget.reserve_retained(len).map_err(|e| format!("native replay memory: {e:?}"))?;
    backend_store::try_allocate_exact_zeroed_bytes(len).map_err(|e| format!("native replay allocation: {e:?}"))
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
    Single { value: RetainedNativeProgramSourceManifest, _scratch: Box<[u8]> },
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

fn applies(capture: &BuiltinCaptureChange, row: &NativeSourceEvidence, project: &[u8; 32]) -> bool {
    &row.project == project && row.profile == capture.key.profile()
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
    let mut count = 0usize;
    for (i, row) in rows.iter().enumerate() {
        step(budget, stopped, 1)?;
        if !applies(capture, row, &project) { continue; }
        for previous in &rows[..i] {
            step(budget, stopped, 1)?;
            if applies(capture, previous, &project) && eq(previous.path.as_bytes(), row.path.as_bytes(), budget, stopped)? { return Err("duplicate native replay source path".into()); }
        }
        flags[i] = 1;
        count += 1;
    }
    for (i, row) in retirements.iter().enumerate() {
        step(budget, stopped, 1)?;
        if !applies(capture, row, &project) { continue; }
        for previous in &retirements[..i] {
            step(budget, stopped, 1)?;
            if applies(capture, previous, &project) && eq(previous.path.as_bytes(), row.path.as_bytes(), budget, stopped)? { return Err("duplicate native replay retirement path".into()); }
        }
        for source in rows {
            step(budget, stopped, 1)?;
            if applies(capture, source, &project) && eq(source.path.as_bytes(), row.path.as_bytes(), budget, stopped)? { return Err("duplicate native replay retirement path".into()); }
        }
    }
    if count == 0 { return Err("native replay retirement requires a nonempty selected Program".into()); }
    let mut plan = Plan { claim: false, program: None, headers: None, lines: None, flags, scratch_credit: 0 };
    visit_selected_publication_wire(store, selected, budget, stopped, 128 * 1024 * 1024, 4096, |item, budget| {
        match item {
            SelectedPublicationWire::Metadata { manifest_bytes, binding_bytes, declared_fragment_count } => {
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
                    let mut scratch = allocate(size, budget, stopped)?;
                    let value = RetainedNativeProgramSourceManifest::admit(retained, &mut scratch, |n| step(budget, stopped, n)).map_err(|e| format!("native Program: {e:?}"))?;
                    Program::Single { value, _scratch: scratch }
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
        for row in retirements {
            step(budget, stopped, 1)?;
            if applies(capture, row, &project) && eq(path, row.path.as_bytes(), budget, stopped)? {
                return Err("native replay retired path remains in Program".into());
            }
        }
        for (i, row) in rows.iter().enumerate() {
            step(budget, stopped, 1)?;
            if plan.flags[i] & 1 == 0 || !eq(path, row.path.as_bytes(), budget, stopped)? { continue; }
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
    let lines = plan.lines.as_ref().map(|wire| {
        let mut checkpoint = |n| charge_native_header_work(budget, stopped, n);
        backend_semantic::native_source_lines::NativeSourceLineAggregateView::parse(wire, &mut checkpoint)
            .map_err(|e| format!("native replay line maps: {e:?}"))
    }).transpose()?;
    for (i, row) in rows.iter().enumerate() {
        if plan.flags[i] & 1 == 0 { continue; }
        if let Some(expected) = &row.line_starts {
            let aggregate = lines.as_ref().ok_or("native replay line map absent")?;
            let mut checkpoint = |n| charge_native_header_work(budget, stopped, n);
            let file = aggregate.find_file(&row.path, &mut checkpoint)?.ok_or("native replay line file absent")?;
            if file.source.byte_len != row.byte_length || file.source.identity.as_ref() != row.source_digest.as_slice() || file.line_count() != expected.len() {
                return Err("native replay line source differs".into());
            }
            for (index, expected) in expected.iter().copied().enumerate() {
                step(budget, stopped, 1)?;
                if file.line_start(index) != Some(expected) { return Err("native replay line offsets differ".into()); }
            }
        }
    }
    let mut seen_claim = false;
    let mut seen_program = false;
    visit_selected_publication_wire(store, selected, budget, stopped, 128 * 1024 * 1024, 4096, |item, budget| {
        match item {
            SelectedPublicationWire::Metadata { manifest_bytes, binding_bytes, declared_fragment_count } => {
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
                let mut member_found = false;
                program.visit(budget, stopped, |entry| {
                    if let Some(package) = entry.package_path {
                        if eq(package, path, budget, stopped)? {
                            if member_found || entry.source != source { return Err("selected program differs from image source".into()); }
                            member_found = true;
                        }
                    }
                    Ok(())
                })?;
                if !member_found || recipe.profile.language() != backend_semantic::vocabulary::Language::TypeScript || recipe.toolchain != program.toolchain() {
                    return Err("selected program differs from image source or recipe".into());
                }
                if !program.image_matches_bytes(path, member.semantic_image_identity(), budget, stopped)? { return Err("native replay image differs from sealed Program".into()); }
                let mut matched = None;
                for (i, row) in rows.iter().enumerate() {
                    step(budget, stopped, 1)?;
                    if plan.flags[i] & 1 != 0 && eq(path, row.path.as_bytes(), budget, stopped)? { matched = Some(i); break; }
                }
                let Some(index) = matched else { return Ok(()); };
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
                    let (aggregate, digest) = headers.as_ref().ok_or("native replay header witness absent")?;
                    let file = super::super::native_ts_headers::witnessed_file_borrowed(row, aggregate, *digest, &mut checkpoint)?;
                    super::super::native_ts_headers::admit_image_borrowed(row, claimed, file, &view, bytes, &mut checkpoint)?;
                    plan.flags[index] |= 8;
                } else if let Some((aggregate, _)) = &headers {
                    if aggregate.find_file_checkpointed(&row.path, &mut checkpoint)?.is_some() { return Err("native replay lost compiler header reference".into()); }
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
            let mut found = false;
            for (i, row) in rows.iter().enumerate() {
                step(budget, stopped, 1)?;
                if plan.flags[i] & 8 != 0 && eq(file.path.as_bytes(), row.path.as_bytes(), budget, stopped)? { found = true; break; }
            }
            if !found { return Err("native replay did not consume compiler header files".into()); }
        }
    }
    for (i, row) in rows.iter().enumerate() {
        if plan.flags[i] & 1 == 0 { continue; }
        if plan.flags[i] & 2 == 0 || (row.admits_image() && plan.flags[i] & 4 == 0) {
            return Err("native replay source lacks its claimed Program evidence".into());
        }
    }
    step(budget, stopped, 1)?;
    Ok(())
}
