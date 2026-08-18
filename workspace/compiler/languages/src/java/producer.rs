//! `JavaProducer` — the `javadoc`-doclet oracle wired to the [`Producer`] contract.

use crate::{PackageSource, Producer, ProducerError, ProducerId};
use nudox_ir::body::Language;
use nudox_ir::lower::Lowering;

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
        let extraction = invoke::invoke(src)?;

        // A doclet older than this build under-reports silently: every field
        // is `#[serde(default)]`, so its missing `references` arrives as an
        // empty `Box<[_]>` and `refs` answers "nothing here" for the whole
        // language. Failing loudly is the only way that reads as a stale
        // classes directory rather than an empty package — see
        // `schema::Extraction::staleness`, and `crate::go::producer::GoProducer`'s
        // identical wiring for the incident that made this worth doing before
        // it happens again in Java.
        if let Some(stale) = extraction.staleness() {
            return Err(ProducerError::OracleExit {
                command: PRODUCER_ID.to_owned(),
                code: "stale".to_owned(),
                stderr: stale.to_string(),
            });
        }

        Ok(extraction)
    }

    fn lower(&self, oracle: &Extraction, out: &mut Lowering<JavaId>) -> Result<(), ProducerError> {
        let mut ctx = LoweringCtx::new(out, &oracle.types);
        lower_extraction(&mut ctx, oracle);
        Ok(())
    }
}
