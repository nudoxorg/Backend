//! `JavaProducer` — the `javadoc`-doclet oracle wired to the [`Producer`] contract.

use nudox_ir::body::Language;
use nudox_ir::lower::Lowering;
use crate::{PackageSource, Producer, ProducerError, ProducerId};

use crate::java::{
    invoke,
    lower::{JavaId, LoweringCtx, lower_extraction},
    schema::Extraction,
};

/// Identifies the Java oracle producer in the registry; bump when the output
/// shape changes.
pub const PRODUCER_ID: &str = "java-javadoc/1";

/// The Java producer: `javadoc` + the vendored `nudox.oracle.Extractor`
/// doclet, lowered by [`crate::java::lower`].
///
/// Stateless by design (see `Producer`'s trait doc) — all per-run
/// configuration (release level, doclet classes location, `javadoc` binary)
/// is read from the environment inside [`invoke`](crate::java::invoke), not
/// carried on `self`, exactly like `GoProducer`'s equivalent knobs.
#[derive(Debug, Default, Clone, Copy)]
pub struct JavaProducer;

impl JavaProducer {
    /// Construct a new Java producer.
    pub fn new() -> Self {
        JavaProducer
    }
}

impl Producer for JavaProducer {
    type Id = JavaId;
    type Oracle = Extraction;

    const ID: ProducerId = ProducerId(PRODUCER_ID);
    const LANGUAGE: Language = Language::Java;

    fn invoke(&self, src: &PackageSource) -> Result<Extraction, ProducerError> {
        invoke::invoke(src)
    }

    fn lower(&self, oracle: &Extraction, out: &mut Lowering<JavaId>) -> Result<(), ProducerError> {
        let mut ctx = LoweringCtx::new(out, &oracle.types);
        lower_extraction(&mut ctx, oracle);
        Ok(())
    }
}
