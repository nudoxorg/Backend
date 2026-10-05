//! Typed, bounded projection of compact-fragment construction failures.
//!
//! The concrete semantic/IR error stays available to in-process consumers.
//! Wire consumers receive its closed phase family, the operands we can safely
//! name as coordinates or capacity facts, and a bounded human explanation.

use std::{fmt, fmt::Write as _};

use backend_semantic::ir::{
    BuildError, FragmentError, LayoutStep, PrepareError, SemanticSpace, WriteError,
};

use super::compiler::FragmentCause;

/// Concrete compact-fragment failure retained after the driver's scratch lease ends.
#[derive(Debug, Eq, PartialEq)]
pub enum CompilerFragmentFault {
    /// Owned semantic IR construction failed before compact-fragment preparation.
    Build(BuildError),
    /// Compact-fragment input validation or layout preparation failed.
    Prepare(PrepareError),
    /// Writing the prepared fragment into caller-owned bytes failed.
    Write(WriteError),
    /// Reopening the newly written fragment rejected its bytes.
    Validate(FragmentError),
}

/// Exact phase plus the original closed semantic/IR error.
#[derive(Debug, Eq, PartialEq)]
pub struct CompilerFragmentFailure {
    phase: FragmentCause,
    fault: CompilerFragmentFault,
}

impl CompilerFragmentFailure {
    /// Retains an owned IR build error as a preparation-phase compiler terminal.
    #[must_use]
    pub const fn build(fault: BuildError) -> Self {
        Self {
            phase: FragmentCause::Prepare,
            fault: CompilerFragmentFault::Build(fault),
        }
    }

    /// Retains an exact compact-fragment preparation error.
    #[must_use]
    pub const fn prepare(fault: PrepareError) -> Self {
        Self {
            phase: FragmentCause::Prepare,
            fault: CompilerFragmentFault::Prepare(fault),
        }
    }

    /// Retains an exact compact-fragment write error.
    #[must_use]
    pub const fn write(fault: WriteError) -> Self {
        Self {
            phase: FragmentCause::Write,
            fault: CompilerFragmentFault::Write(fault),
        }
    }

    /// Retains an exact fresh-fragment validation error.
    #[must_use]
    pub const fn validate(fault: FragmentError) -> Self {
        Self {
            phase: FragmentCause::Validate,
            fault: CompilerFragmentFault::Validate(fault),
        }
    }

    /// Returns the existing protocol phase. Build errors remain in the prepare phase,
    /// while their concrete `BuildError` family remains directly inspectable.
    #[must_use]
    pub const fn phase(&self) -> FragmentCause {
        self.phase
    }

    /// Returns the closed concrete semantic/IR error family and all of its original operands.
    #[must_use]
    pub const fn fault(&self) -> &CompilerFragmentFault {
        &self.fault
    }

    /// Returns only the coordinate or capacity facts with stable names at this boundary.
    /// Other exact operands remain available through [`Self::fault`].
    #[must_use]
    pub fn facts(&self) -> CompilerFragmentFaultFacts {
        match &self.fault {
            CompilerFragmentFault::Build(BuildError::InvalidTreeEntity { raw, count }) => {
                CompilerFragmentFaultFacts::TreeEntity {
                    raw: *raw,
                    count: *count,
                }
            }
            CompilerFragmentFault::Build(BuildError::Dangling { space, raw }) => {
                CompilerFragmentFaultFacts::Dangling {
                    space: *space,
                    raw: *raw,
                }
            }
            CompilerFragmentFault::Build(BuildError::InvalidOccurrenceSpan {
                owner,
                start,
                end,
            }) => CompilerFragmentFaultFacts::OccurrenceSpan {
                owner: owner.raw,
                start: *start,
                end: *end,
            },
            CompilerFragmentFault::Prepare(PrepareError::Count { lane, actual, .. }) => {
                CompilerFragmentFaultFacts::Count {
                    lane: *lane,
                    actual: *actual,
                }
            }
            CompilerFragmentFault::Prepare(PrepareError::LayoutOverflow {
                step,
                entity_count,
                type_node_count,
            }) => CompilerFragmentFaultFacts::LayoutOverflow {
                step: *step,
                entity_count: *entity_count,
                type_node_count: *type_node_count,
            },
            CompilerFragmentFault::Prepare(PrepareError::NativeCount { step, actual, .. }) => {
                CompilerFragmentFaultFacts::NativeCount {
                    step: *step,
                    actual: *actual,
                }
            }
            CompilerFragmentFault::Prepare(PrepareError::OutputLength { actual, .. }) => {
                CompilerFragmentFaultFacts::OutputLength { actual: *actual }
            }
            CompilerFragmentFault::Prepare(PrepareError::SemanticDataOverflow {
                atoms,
                products,
                children,
            }) => CompilerFragmentFaultFacts::SemanticDataOverflow {
                atoms: *atoms,
                products: *products,
                children: *children,
            },
            CompilerFragmentFault::Prepare(PrepareError::SemanticEntityRoots {
                roots,
                entities,
            }) => CompilerFragmentFaultFacts::SemanticEntityRoots {
                roots: *roots,
                entities: *entities,
            },
            CompilerFragmentFault::Prepare(PrepareError::SemanticAtomLength {
                ordinal,
                actual,
                ..
            }) => CompilerFragmentFaultFacts::AtomLength {
                ordinal: ordinal.raw,
                actual: *actual,
            },
            CompilerFragmentFault::Write(WriteError::OutputTooSmall {
                required,
                available,
            }) => CompilerFragmentFaultFacts::OutputTooSmall {
                required: *required,
                available: *available,
            },
            CompilerFragmentFault::Write(WriteError::AtomLength {
                ordinal, actual, ..
            })
            | CompilerFragmentFault::Write(WriteError::SemanticAtomLength {
                ordinal,
                actual,
                ..
            }) => CompilerFragmentFaultFacts::AtomLength {
                ordinal: ordinal.raw,
                actual: *actual,
            },
            CompilerFragmentFault::Write(WriteError::AtomExtent { ordinal }) => {
                CompilerFragmentFaultFacts::AtomCoordinate {
                    ordinal: ordinal.raw,
                }
            }
            CompilerFragmentFault::Build(_)
            | CompilerFragmentFault::Prepare(_)
            | CompilerFragmentFault::Write(_)
            | CompilerFragmentFault::Validate(_) => CompilerFragmentFaultFacts::None,
        }
    }

    /// Captures the source error's human explanation into a fixed byte budget.
    /// The original typed cause, not this secondary text, is the diagnostic authority.
    #[must_use]
    pub fn detail(&self) -> BoundedCompilerFragmentDetail {
        let mut detail = BoundedCompilerFragmentDetail::new();
        let fault: &dyn fmt::Display = match &self.fault {
            CompilerFragmentFault::Build(fault) => fault,
            CompilerFragmentFault::Prepare(fault) => fault,
            CompilerFragmentFault::Write(fault) => fault,
            CompilerFragmentFault::Validate(fault) => fault,
        };
        if !detail.truncated && (write!(&mut detail, "{fault}").is_err() || detail.text.is_empty())
        {
            detail.text.push_str("fragment failure detail unavailable");
            detail.truncated = true;
        }
        detail
    }
}

/// Bounded named operands for common relation, layout, and capacity failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerFragmentFaultFacts {
    /// No additional scalar operands are projected at this boundary.
    None,
    /// A tree-local entity coordinate was outside its entity table.
    TreeEntity {
        /// Rejected entity coordinate.
        raw: u32,
        /// Number of available entities.
        count: u32,
    },
    /// A semantic-pool reference was dangling.
    Dangling {
        /// Closed semantic pool containing the rejected coordinate.
        space: SemanticSpace,
        /// Rejected coordinate in that pool.
        raw: u32,
    },
    /// An occurrence span escaped the captured source extent of its owner.
    OccurrenceSpan {
        /// Entity whose source extent owns the occurrence.
        owner: u32,
        /// Inclusive occurrence start byte.
        start: u32,
        /// Exclusive occurrence end byte.
        end: u32,
    },
    /// A count could not fit the selected fragment lane's encoded width.
    Count {
        /// Lane whose count is being encoded.
        lane: LayoutStep,
        /// Original count before narrowing.
        actual: usize,
    },
    /// Layout arithmetic overflowed at one exact fragment step.
    LayoutOverflow {
        /// Layout step whose offset or extent overflowed.
        step: LayoutStep,
        /// Entity row count in this layout.
        entity_count: u32,
        /// Type-node row count in this layout.
        type_node_count: u32,
    },
    /// A valid wire count did not fit the platform's native address width.
    NativeCount {
        /// Lane whose count did not fit.
        step: LayoutStep,
        /// Rejected wire count.
        actual: u32,
    },
    /// The required output extent did not fit the wire's byte-coordinate width.
    OutputLength {
        /// Required complete fragment byte length.
        actual: usize,
    },
    /// Canonical semantic graph dimensions overflowed the payload layout.
    SemanticDataOverflow {
        /// Semantic atom count.
        atoms: u32,
        /// Semantic product count.
        products: u32,
        /// Semantic child count.
        children: u32,
    },
    /// Semantic graph root count differed from the fragment entity count.
    SemanticEntityRoots {
        /// Number of supplied semantic roots.
        roots: u32,
        /// Number of fragment entities requiring roots.
        entities: u32,
    },
    /// An atom byte length could not fit its wire field.
    AtomLength {
        /// Atom coordinate whose extent failed.
        ordinal: u32,
        /// Original atom length in bytes.
        actual: usize,
    },
    /// A caller output slice could not fit the exact prepared fragment.
    OutputTooSmall {
        /// Required output capacity in bytes.
        required: usize,
        /// Supplied output capacity in bytes.
        available: usize,
    },
    /// A prepared atom extent no longer fit its assigned byte region.
    AtomCoordinate {
        /// Atom coordinate whose extent changed.
        ordinal: u32,
    },
}

/// Closed family of the retained concrete fragment error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompilerFragmentFaultFamily {
    /// Semantic IR materialization failed.
    Build,
    /// Fragment preflight, canonicalization, or layout preparation failed.
    Prepare,
    /// Caller-owned output admission failed.
    Write,
    /// Freshly written compact bytes failed validation.
    Validate,
}

impl CompilerFragmentFailure {
    /// Returns the closed error family represented by the retained concrete error.
    #[must_use]
    pub const fn family(&self) -> CompilerFragmentFaultFamily {
        match &self.fault {
            CompilerFragmentFault::Build(_) => CompilerFragmentFaultFamily::Build,
            CompilerFragmentFault::Prepare(_) => CompilerFragmentFaultFamily::Prepare,
            CompilerFragmentFault::Write(_) => CompilerFragmentFaultFamily::Write,
            CompilerFragmentFault::Validate(_) => CompilerFragmentFaultFamily::Validate,
        }
    }
}

/// Maximum UTF-8 byte length of the secondary compiler fragment explanation.
pub const MAX_COMPILER_FRAGMENT_DETAIL_BYTES: usize = 384;

/// Human-readable secondary detail retained within a fixed UTF-8 byte budget.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BoundedCompilerFragmentDetail {
    /// Sanitized UTF-8 explanation, at most [`MAX_COMPILER_FRAGMENT_DETAIL_BYTES`] bytes.
    pub text: String,
    /// Whether the source explanation exceeded the byte budget or allocation was unavailable.
    pub truncated: bool,
}

impl BoundedCompilerFragmentDetail {
    fn new() -> Self {
        let mut text = String::new();
        let reserved = text
            .try_reserve_exact(MAX_COMPILER_FRAGMENT_DETAIL_BYTES)
            .is_ok();
        Self {
            text,
            truncated: !reserved,
        }
    }
}

impl fmt::Write for BoundedCompilerFragmentDetail {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if self.truncated {
            return Ok(());
        }
        for character in value.chars() {
            let character = if character.is_control() {
                ' '
            } else {
                character
            };
            let width = character.len_utf8();
            if self.text.len().saturating_add(width) > MAX_COMPILER_FRAGMENT_DETAIL_BYTES {
                self.truncated = true;
                break;
            }
            self.text.push(character);
        }
        Ok(())
    }
}
