//! Thin orchestration and closed typed error facts for capacity benchmark phases.

#[path = "runner/exact.rs"]
mod exact;
#[path = "runner/failure.rs"]
mod failure;
#[path = "runner/fixture.rs"]
mod fixture;
#[path = "runner/machine.rs"]
mod machine;
#[path = "runner/native.rs"]
mod native;
#[path = "runner/orchestration.rs"]
mod orchestration;
#[path = "runner/publication.rs"]
mod publication;
#[path = "runner/support.rs"]
mod support;
#[path = "runner/tantivy.rs"]
mod tantivy;
#[path = "runner/vector.rs"]
mod vector;

pub(crate) use failure::{
    BuildFailureFact, CompileFailureFact, ExactSegmentFault, LexicalSegmentFault,
};
pub(crate) use orchestration::Runtime;
