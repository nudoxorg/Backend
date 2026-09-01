//! Proves an external embedder double can participate in the publish crate's test graph.
//! The test keeps model authority as an identity and metric only.
//! It is the narrow compile-time seam for the seal journey.
use server_index_build::EntityEmbedder;
use server_index_graph_vector::{Metric, ModelId};

struct SealEmbedder;

impl EntityEmbedder for SealEmbedder {
    const DIMENSION: usize = 2;
    fn model(&self) -> ModelId {
        ModelId::new([9; 16])
    }
    fn metric(&self) -> Metric {
        Metric::NegativeDotProduct
    }
    fn embed(&self, fact: server_index_build::EntityFactView<'_>, coordinates: &mut [i16]) {
        let Ok(entity) = i16::try_from(fact.entity.raw) else { return };
        let Ok(name_length) = i16::try_from(fact.name.len()) else { return };
        let mut cells = coordinates.iter_mut();
        if let Some(cell) = cells.next() {
            *cell = entity;
        }
        if let Some(cell) = cells.next() {
            *cell = name_length;
        }
    }
}

#[test]
fn external_seal_embedder_is_typed() {
    let embedder = SealEmbedder;
    assert_eq!(embedder.model(), ModelId::new([9; 16]));
    assert_eq!(embedder.metric(), Metric::NegativeDotProduct);
}
