//! Defines traversal behavior for the direct Clang semantic frontend of `compiler-driver`.
//! Owns the C callback boundary: no Rust panic crosses into libclang, every capacity breach
//! stops the traversal with a typed issue, and the active ancestor path lives in caller
//! scratch so owners are exact enclosing extents rather than invented contexts.

use std::{
    ffi::{c_int, c_uint, c_void},
    mem::{MaybeUninit, align_of, size_of},
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use super::{ffi, identity::IdentityInterner, parse::FactJournal, protocol::ClangSourceSpan};

/// Minimum ancestor frames the traversal path can operate with.
pub(crate) const TRAVERSAL_MIN_FRAMES: usize = 2;
/// Fraction of the identity scratch reserved for the ancestor path.
pub(crate) const TRAVERSAL_SCRATCH_DIVISOR: usize = 4;

/// Mutable traversal state carried through the libclang callback.
pub(crate) struct VisitState<'source, 'path, 'scratch, 'cancel> {
    /// Live translation unit owning every visited cursor.
    pub(crate) translation_unit: ffi::CxTranslationUnit,
    /// Exact caller source bytes every span indexes into.
    pub(crate) source: &'source [u8],
    /// Exact source name retained by typed issues.
    pub(crate) source_name: &'path Path,
    /// Caller-owned cancellation flag observed at every visit.
    pub(crate) cancellation: Option<&'cancel AtomicBool>,
    /// Transactional fact journal.
    pub(crate) journal: FactJournal<'scratch>,
    /// USR identity interner.
    pub(crate) identities: IdentityInterner<'scratch>,
    /// Admitted canonical entity count.
    pub(crate) entities: u32,
    /// Admitted type-use count.
    pub(crate) type_uses: u32,
    /// Admitted reference count.
    pub(crate) references: u32,
    /// Exact cursor-visit work counter.
    pub(crate) cursor_visits: usize,
    /// Caller-derived token capacity for bounded tokenization.
    pub(crate) token_limit: c_uint,
    /// Active ancestor path in caller scratch.
    pub(crate) path: TraversalPath<'scratch>,
    /// First typed issue that stopped the traversal, if any.
    pub(crate) issue: Option<VisitIssue>,
}

/// Owner context retained on one ancestor frame.
#[derive(Clone, Copy)]
struct OwnerContext {
    span: ClangSourceSpan,
}

/// One aligned ancestor frame kept in caller scratch.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct TraversalFrame {
    cursor: ffi::CxCursor,
    depth: usize,
    owner: Option<OwnerContext>,
}

/// Active ancestor path over aligned caller scratch frames.
pub(crate) struct TraversalPath<'scratch> {
    frames: &'scratch mut [MaybeUninit<TraversalFrame>],
    provided_bytes: usize,
    prefix_bytes: usize,
    length: usize,
    parent_queries: usize,
    cached_parent: ffi::CxCursor,
    cached_parent_index: usize,
    has_cached_parent: bool,
}

impl<'scratch> TraversalPath<'scratch> {
    /// Roots the path at the translation-unit cursor or reports the exact scratch shortfall.
    pub(crate) fn new(
        root: ffi::CxCursor,
        scratch: &'scratch mut [u8],
    ) -> Result<Self, VisitIssue> {
        // `CxCursor` contains opaque native state. Complete cursor/frame values stay in
        // aligned `MaybeUninit` storage rather than being serialized into an arbitrary
        // caller slice. The alignment prefix is charged to the caller high-water metric
        // and never read.
        let provided = scratch.len();
        // SAFETY: the scratch region is live for the path's lifetime and only
        // indices below `length` are ever read after `write_frame` initializes them.
        let (prefix, frames, _) = unsafe { scratch.align_to_mut::<MaybeUninit<TraversalFrame>>() };
        if frames.len() < TRAVERSAL_MIN_FRAMES {
            let required = size_of::<TraversalFrame>()
                .checked_mul(TRAVERSAL_MIN_FRAMES)
                .and_then(|bytes| bytes.checked_add(align_of::<TraversalFrame>() - 1))
                .ok_or(VisitIssue::FactCountOverflow)?;
            return Err(VisitIssue::IdentityScratchCapacity { provided, required });
        }
        let mut path = Self {
            frames,
            provided_bytes: provided,
            prefix_bytes: prefix.len(),
            length: 1,
            parent_queries: 0,
            cached_parent: ffi::CxCursor::default(),
            cached_parent_index: 0,
            has_cached_parent: false,
        };
        path.write_frame(
            0,
            TraversalFrame {
                cursor: root,
                depth: 0,
                owner: None,
            },
        )?;
        Ok(path)
    }

    /// Reads one initialized frame.
    fn frame(&self, index: usize) -> Option<TraversalFrame> {
        let frame = self.frames.get(index)?;
        // SAFETY: only indices below `length` are queried, and `write_frame` initializes
        // each slot before extending `length`.
        Some(unsafe { frame.assume_init_read() })
    }

    /// Initializes one frame slot or reports the exact scratch shortfall.
    fn write_frame(&mut self, index: usize, frame: TraversalFrame) -> Result<(), VisitIssue> {
        let Some(slot) = self.frames.get_mut(index) else {
            let required = index
                .checked_add(1)
                .and_then(|count| count.checked_mul(size_of::<TraversalFrame>()))
                .ok_or(VisitIssue::FactCountOverflow)?;
            return Err(VisitIssue::IdentityScratchCapacity {
                provided: self.provided_bytes,
                required,
            });
        };
        slot.write(frame);
        Ok(())
    }

    /// Records one visited cursor under its parent, returning its depth.
    pub(crate) fn observe(
        &mut self,
        cursor: ffi::CxCursor,
        parent: ffi::CxCursor,
        source: &[u8],
    ) -> Result<usize, VisitIssue> {
        let mut parent_index = None;
        // Sibling callbacks repeatedly name the same parent. Carrying the last successful
        // parent/index avoids rescanning the full active path in that common case; misses
        // fall back to a bounded reverse lookup.
        if self.has_cached_parent {
            self.parent_queries = self
                .parent_queries
                .checked_add(1)
                .ok_or(VisitIssue::FactCountOverflow)?;
            if self.cached_parent_index < self.length {
                // SAFETY: both cursors were produced by the live translation unit.
                let equal = unsafe { ffi::clang_equal_cursors(self.cached_parent, parent) };
                if equal != 0 {
                    parent_index = Some(self.cached_parent_index);
                }
            }
        }
        if parent_index.is_none() {
            // The callback API provides no exit event, so the active ancestor path is kept
            // in caller storage and every comparison is accounted for; the exact metric
            // makes the remaining native traversal work visible.
            for index in (0..self.length).rev() {
                self.parent_queries = self
                    .parent_queries
                    .checked_add(1)
                    .ok_or(VisitIssue::FactCountOverflow)?;
                let Some(frame) = self.frame(index) else {
                    break;
                };
                // SAFETY: both cursors were produced by the live translation unit.
                let equal = unsafe { ffi::clang_equal_cursors(frame.cursor, parent) };
                if equal != 0 {
                    parent_index = Some(index);
                    break;
                }
            }
        }
        let Some(parent_index) = parent_index else {
            return Err(VisitIssue::TraversalParentUnavailable);
        };
        let parent_frame = self
            .frame(parent_index)
            .ok_or(VisitIssue::TraversalParentUnavailable)?;
        let depth = parent_frame
            .depth
            .checked_add(1)
            .ok_or(VisitIssue::FactCountOverflow)?;
        let child_index = parent_index
            .checked_add(1)
            .ok_or(VisitIssue::FactCountOverflow)?;
        if child_index >= self.frames.len() {
            return Err(VisitIssue::TooDeep { depth });
        }
        let parent_owner = parent_frame.owner;
        // SAFETY: the cursor belongs to the live translation unit.
        let cursor_kind = unsafe { ffi::clang_get_cursor_kind(cursor) };
        let owner = if is_owner_kind(cursor_kind) {
            super::cursor::extent_span(source, cursor)
                .map(|span| OwnerContext { span })
                .or(parent_owner)
        } else {
            parent_owner
        };
        self.write_frame(
            child_index,
            TraversalFrame {
                cursor,
                depth,
                owner,
            },
        )?;
        self.length = child_index + 1;
        self.cached_parent = parent;
        self.cached_parent_index = parent_index;
        self.has_cached_parent = true;
        Ok(depth)
    }

    /// Exact owner extent of the deepest active frame.
    pub(crate) fn owner(&self) -> Option<ClangSourceSpan> {
        self.frame(self.length - 1)
            .and_then(|frame| frame.owner.map(|owner| owner.span))
    }

    /// Returns the nearest call expression in the active cursor ancestry.
    pub(crate) fn nearest_call_kind(&self) -> Option<c_int> {
        (0..self.length).rev().find_map(|index| {
            let frame = self.frame(index)?;
            // SAFETY: every active frame was copied from a cursor in the live translation unit.
            let kind = unsafe { ffi::clang_get_cursor_kind(frame.cursor) };
            matches!(
                kind,
                ffi::CX_CURSOR_CALL_EXPR | ffi::CX_CURSOR_CXX_MEMBER_CALL_EXPR
            )
            .then_some(kind)
        })
    }

    /// Exact path high-water mark including the alignment prefix.
    pub(crate) fn high_water(&self) -> Result<usize, VisitIssue> {
        self.prefix_bytes
            .checked_add(
                self.length
                    .checked_mul(size_of::<TraversalFrame>())
                    .ok_or(VisitIssue::FactCountOverflow)?,
            )
            .ok_or(VisitIssue::FactCountOverflow)
    }

    /// Exact parent-comparison work performed.
    pub(crate) fn parent_queries(&self) -> usize {
        self.parent_queries
    }
}

/// Typed issue that stopped one traversal.
#[derive(Clone, Copy, Debug)]
pub(crate) enum VisitIssue {
    /// Nesting exceeded the caller frame capacity at the named depth.
    TooDeep {
        /// Exact rejected depth.
        depth: usize,
    },
    /// Identity binding capacity rejected one more entry.
    BindingCapacity {
        /// Exact admitted caller capacity.
        limit: usize,
        /// Exact observed entry count including the rejected one.
        observed: usize,
    },
    /// libclang could not derive a USR identity.
    IdentityUnavailable,
    /// A referenced declaration lives outside the analyzed source.
    ExternalIdentityUnavailable,
    /// Identity scratch rejected the exact capacity requirement.
    IdentityScratchCapacity {
        /// Caller-provided identity scratch bytes.
        provided: usize,
        /// Exact required identity scratch bytes.
        required: usize,
    },
    /// Fact scratch rejected the exact capacity requirement.
    FactScratchCapacity {
        /// Caller-provided fact scratch bytes.
        provided: usize,
        /// Exact required fact scratch bytes.
        required: usize,
    },
    /// Tokenization exceeded the caller-derived token capacity.
    TokenCapacity {
        /// Observed token count.
        observed: usize,
        /// Caller-derived token capacity.
        limit: usize,
    },
    /// A checked fact counter exceeded its address space.
    FactCountOverflow,
    /// The callback observed a Rust panic.
    CallbackPanicked,
    /// The traversal lost its ancestor path.
    TraversalParentUnavailable,
}

impl VisitIssue {
    /// Converts the issue into its exact typed error retaining the source name.
    pub(crate) fn into_error<'input>(
        self,
        source_name: &'input Path,
    ) -> super::error::ClangError<'input> {
        match self {
            Self::TooDeep { depth } => super::error::ClangError::AstNestingTooDeep { depth },
            Self::BindingCapacity { limit, observed } => {
                super::error::ClangError::BindingCapacity { limit, observed }
            }
            Self::IdentityUnavailable => super::error::ClangError::LibclangIdentityUnavailable,
            Self::IdentityScratchCapacity { provided, required } => {
                super::error::ClangError::LibclangIdentityScratchTooSmall {
                    source_name,
                    provided,
                    required,
                }
            }
            Self::ExternalIdentityUnavailable => {
                super::error::ClangError::LibclangExternalIdentityUnavailable { source_name }
            }
            Self::FactScratchCapacity { provided, required } => {
                super::error::ClangError::LibclangFactScratchTooSmall {
                    source_name,
                    provided,
                    required,
                }
            }
            Self::TokenCapacity { observed, limit } => {
                super::error::ClangError::LibclangTokenCapacity {
                    source_name,
                    observed,
                    limit,
                }
            }
            Self::FactCountOverflow => super::error::ClangError::FactCountOverflow,
            Self::CallbackPanicked => super::error::ClangError::LibclangCallbackPanicked,
            Self::TraversalParentUnavailable => {
                super::error::ClangError::LibclangTraversalParentUnavailable
            }
        }
    }
}

impl<'source, 'path, 'scratch, 'cancel> VisitState<'source, 'path, 'scratch, 'cancel> {
    /// Journals one entity fact and advances its counter.
    pub(crate) fn emit_entity_fact(
        &mut self,
        fact: super::protocol::EntityFact<'source>,
    ) -> Result<(), VisitIssue> {
        self.journal
            .push(super::protocol::ClangFact::Entity(fact))?;
        self.entities = self
            .entities
            .checked_add(1)
            .ok_or(VisitIssue::FactCountOverflow)?;
        Ok(())
    }

    /// Journals one type-use fact and advances its counter.
    pub(crate) fn emit_type_use_fact(
        &mut self,
        fact: super::protocol::TypeUseFact<'source>,
    ) -> Result<(), VisitIssue> {
        self.journal
            .push(super::protocol::ClangFact::TypeUse(fact))?;
        self.type_uses = self
            .type_uses
            .checked_add(1)
            .ok_or(VisitIssue::FactCountOverflow)?;
        Ok(())
    }

    /// Journals one reference fact and advances its counter.
    pub(crate) fn emit_reference_fact(
        &mut self,
        fact: super::protocol::ReferenceFact<'source>,
    ) -> Result<(), VisitIssue> {
        self.journal
            .push(super::protocol::ClangFact::Reference(fact))?;
        self.references = self
            .references
            .checked_add(1)
            .ok_or(VisitIssue::FactCountOverflow)?;
        Ok(())
    }
}

/// The libclang traversal callback; never lets a Rust panic cross into C.
pub(crate) extern "C" fn visit_cursor(
    cursor: ffi::CxCursor,
    parent: ffi::CxCursor,
    data: *mut c_void,
) -> ffi::CxChildVisitResult {
    if data.is_null() {
        return ffi::CX_CHILD_VISIT_BREAK;
    }
    // SAFETY: `data` is the stack address of `VisitState` passed to
    // `clang_visit_children`; libclang invokes this callback synchronously.
    let state = unsafe { &mut *(data.cast::<VisitState<'_, '_, '_, '_>>()) };
    match catch_unwind(AssertUnwindSafe(|| {
        visit_cursor_inner(state, cursor, parent)
    })) {
        Ok(result) => result,
        Err(_) => {
            state.issue = Some(VisitIssue::CallbackPanicked);
            ffi::CX_CHILD_VISIT_BREAK
        }
    }
}

fn visit_cursor_inner(
    state: &mut VisitState<'_, '_, '_, '_>,
    cursor: ffi::CxCursor,
    parent: ffi::CxCursor,
) -> ffi::CxChildVisitResult {
    let cancelled = state
        .cancellation
        .is_some_and(|cancellation| cancellation.load(Ordering::Acquire));
    if state.issue.is_some() || cancelled {
        return ffi::CX_CHILD_VISIT_BREAK;
    }
    state.cursor_visits = match state.cursor_visits.checked_add(1) {
        Some(visits) => visits,
        None => {
            state.issue = Some(VisitIssue::FactCountOverflow);
            return ffi::CX_CHILD_VISIT_BREAK;
        }
    };
    if let Err(issue) = state.path.observe(cursor, parent, state.source) {
        state.issue = Some(issue);
        return ffi::CX_CHILD_VISIT_BREAK;
    }
    if let Err(issue) = super::cursor::visit_one(state, cursor, parent) {
        state.issue = Some(issue);
        return ffi::CX_CHILD_VISIT_BREAK;
    }
    ffi::CX_CHILD_VISIT_RECURSE
}

/// Reports whether cursors of this kind own their enclosing extent for descendants.
fn is_owner_kind(kind: c_int) -> bool {
    matches!(
        kind,
        ffi::CX_CURSOR_STRUCT_DECL
            | ffi::CX_CURSOR_UNION_DECL
            | ffi::CX_CURSOR_CLASS_DECL
            | ffi::CX_CURSOR_ENUM_DECL
            | ffi::CX_CURSOR_NAMESPACE
            | ffi::CX_CURSOR_FUNCTION_DECL
            | ffi::CX_CURSOR_CXX_METHOD
            | ffi::CX_CURSOR_CONSTRUCTOR
            | ffi::CX_CURSOR_DESTRUCTOR
            | ffi::CX_CURSOR_FUNCTION_TEMPLATE
    )
}
