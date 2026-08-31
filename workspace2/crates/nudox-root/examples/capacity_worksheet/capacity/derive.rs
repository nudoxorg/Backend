//! Checked formulas that derive a deployment plan from supplied workload facts.

use super::{
    input::{Bytes, CapacityField, CapacityInput, CapacityModelError, Count, PerMille},
    output::CapacityPlan,
};

/// Recomputes one plan entirely from supplied workload facts.
#[allow(
    clippy::too_many_lines,
    reason = "the worksheet intentionally keeps typed validation and transparent capacity formulas in one auditable derivation"
)]
pub(crate) fn plan(input: &CapacityInput) -> Result<CapacityPlan, CapacityModelError> {
    validate_utilization(
        input.compiler.target_utilization_per_mille,
        CapacityField::CompilerUtilization,
    )?;
    validate_utilization(
        input.index.target_utilization_per_mille,
        CapacityField::IndexUtilization,
    )?;
    nonzero(
        input.compiler.cores_per_worker,
        CapacityField::CompilerCoresPerWorker,
    )?;
    nonzero(input.index.replica_count, CapacityField::IndexReplicaCount)?;
    nonzero(
        input.index.usable_durable_bytes_per_node.into(),
        CapacityField::IndexUsableDurableBytes,
    )?;
    nonzero(
        input.index.query_slots_per_node,
        CapacityField::IndexQuerySlots,
    )?;
    nonzero(
        input.index.usable_memory_bytes_per_node.into(),
        CapacityField::IndexUsableMemoryBytes,
    )?;
    nonzero(
        input.failure_domains.independent_domains,
        CapacityField::IndependentDomains,
    )?;
    nonzero(
        input.embedding.measured_items_per_second_per_worker,
        CapacityField::EmbeddingMeasuredThroughput,
    )?;
    nonzero(
        input.embedding.weight_bits,
        CapacityField::EmbeddingWeightBits,
    )?;
    nonzero(
        input.embedding.output_dimensions,
        CapacityField::EmbeddingOutputDimensions,
    )?;
    nonzero(
        input.embedding.output_element_bits,
        CapacityField::EmbeddingOutputElementBits,
    )?;
    nonzero(
        input.embedding.batch_items,
        CapacityField::EmbeddingBatchItems,
    )?;

    let compiler_core_millis = product(
        input.compiler.requests_per_second.0,
        input.compiler.cpu_millis_per_request.0,
        CapacityField::CompilerDemand,
    )?;
    let compiler_cores = occupied_capacity(
        compiler_core_millis,
        input.compiler.target_utilization_per_mille,
        CapacityField::CompilerDemand,
    )?;
    let compiler_workers = ceil_div(
        compiler_cores,
        input.compiler.cores_per_worker.0,
        CapacityField::CompilerCoresPerWorker,
    )?;

    let replicated_index_bytes = product(
        input.index.primary_durable_bytes.0,
        input.index.replica_count.0,
        CapacityField::IndexReplication,
    )?;
    let index_nodes_by_storage = ceil_div(
        replicated_index_bytes,
        input.index.usable_durable_bytes_per_node.0,
        CapacityField::IndexUsableDurableBytes,
    )?;
    let index_nodes_by_memory = ceil_div(
        input.index.hot_working_set_bytes.0,
        input.index.usable_memory_bytes_per_node.0,
        CapacityField::IndexUsableMemoryBytes,
    )?;
    let query_slot_millis = product(
        input.index.query_requests_per_second.0,
        input.index.cpu_millis_per_query.0,
        CapacityField::IndexQueryDemand,
    )?;
    let query_slots = occupied_capacity(
        query_slot_millis,
        input.index.target_utilization_per_mille,
        CapacityField::IndexQueryDemand,
    )?;
    let index_nodes_by_query = ceil_div(
        query_slots,
        input.index.query_slots_per_node.0,
        CapacityField::IndexQuerySlots,
    )?;
    let required_domains = input
        .failure_domains
        .tolerated_domain_failures
        .0
        .checked_add(1)
        .ok_or(CapacityModelError::Overflow {
            field: CapacityField::IndependentDomains,
        })?;
    if input.index.replica_count.0 < required_domains {
        return Err(CapacityModelError::InsufficientReplicas {
            replicas: input.index.replica_count.0,
            failures: input.failure_domains.tolerated_domain_failures.0,
        });
    }
    if input.failure_domains.independent_domains.0 < input.index.replica_count.0 {
        return Err(CapacityModelError::InsufficientDomains {
            domains: input.failure_domains.independent_domains.0,
            replicas: input.index.replica_count.0,
        });
    }
    let index_nodes_by_failure_domain = input.index.replica_count.0;

    let weight_bits = product(
        input.embedding.model_parameters.0,
        input.embedding.weight_bits.0,
        CapacityField::EmbeddingWeights,
    )?;
    let embedding_weight_bytes = ceil_div(weight_bits, 8, CapacityField::EmbeddingWeights)?;
    let vector_bits = product(
        input.embedding.output_dimensions.0,
        input.embedding.output_element_bits.0,
        CapacityField::EmbeddingVectorBytes,
    )?;
    let embedding_vector_bytes_per_item =
        ceil_div(vector_bits, 8, CapacityField::EmbeddingVectorBytes)?;
    let activation = product(
        input.embedding.activation_bytes_per_item.0,
        input.embedding.batch_items.0,
        CapacityField::EmbeddingBatchActivation,
    )?;
    let embedding_worker_gpu_bytes = embedding_weight_bytes
        .checked_add(input.embedding.runtime_workspace.0)
        .and_then(|total| total.checked_add(activation))
        .ok_or(CapacityModelError::Overflow {
            field: CapacityField::EmbeddingWorkerMemory,
        })?;
    let embedding_workers = ceil_div(
        input.embedding.items_per_second.0,
        input.embedding.measured_items_per_second_per_worker.0,
        CapacityField::EmbeddingMeasuredThroughput,
    )?;

    Ok(CapacityPlan {
        tier: input.tier,
        compiler_cores: Count(compiler_cores),
        compiler_workers: Count(compiler_workers),
        replicated_index_bytes: Bytes(replicated_index_bytes),
        index_nodes_by_storage: Count(index_nodes_by_storage),
        index_nodes_by_memory: Count(index_nodes_by_memory),
        index_nodes_by_query: Count(index_nodes_by_query),
        index_nodes_by_failure_domain: Count(index_nodes_by_failure_domain),
        index_nodes: Count(
            index_nodes_by_storage
                .max(index_nodes_by_query)
                .max(index_nodes_by_memory)
                .max(index_nodes_by_failure_domain),
        ),
        embedding_weight_bytes: Bytes(embedding_weight_bytes),
        embedding_vector_bytes_per_item: Bytes(embedding_vector_bytes_per_item),
        embedding_output_dimensions: input.embedding.output_dimensions,
        embedding_worker_gpu_bytes: Bytes(embedding_worker_gpu_bytes),
        embedding_worker_fits_gpu: embedding_worker_gpu_bytes
            <= input.embedding.usable_gpu_memory.0,
        embedding_workers: Count(embedding_workers),
        embedding_provider: input.embedding.provider,
    })
}

const fn nonzero(value: Count, field: CapacityField) -> Result<(), CapacityModelError> {
    if value.0 == 0 {
        Err(CapacityModelError::Zero { field })
    } else {
        Ok(())
    }
}

const fn validate_utilization(
    value: PerMille,
    field: CapacityField,
) -> Result<(), CapacityModelError> {
    if value.0 == 0 || value.0 >= 1000 {
        Err(CapacityModelError::Utilization {
            field,
            observed: value.0,
        })
    } else {
        Ok(())
    }
}

fn product(left: u64, right: u64, field: CapacityField) -> Result<u64, CapacityModelError> {
    left.checked_mul(right)
        .ok_or(CapacityModelError::Overflow { field })
}

fn occupied_capacity(
    millis_per_second: u64,
    utilization: PerMille,
    field: CapacityField,
) -> Result<u64, CapacityModelError> {
    let scaled = product(millis_per_second, 1000, field)?;
    let milliseconds_at_target = product(utilization.0, 1000, field)?;
    ceil_div(scaled, milliseconds_at_target, field)
}

fn ceil_div(
    numerator: u64,
    denominator: u64,
    field: CapacityField,
) -> Result<u64, CapacityModelError> {
    if denominator == 0 {
        return Err(CapacityModelError::Zero { field });
    }
    numerator
        .checked_add(denominator - 1)
        .ok_or(CapacityModelError::Overflow { field })
        .map(|total| total / denominator)
}

impl From<Bytes> for Count {
    fn from(value: Bytes) -> Self {
        Self(value.0)
    }
}

#[cfg(test)]
mod tests {
    use super::super::input::*;
    use super::*;

    #[test]
    fn capacity_is_driven_by_the_largest_independent_constraint() {
        let input = CapacityInput {
            tier: Tier::Growth,
            compiler: CompilerInput {
                requests_per_second: Count(10),
                cpu_millis_per_request: Millis(200),
                cores_per_worker: Count(4),
                target_utilization_per_mille: PerMille(800),
            },
            index: IndexInput {
                primary_durable_bytes: Bytes(1000),
                replica_count: Count(3),
                usable_durable_bytes_per_node: Bytes(1000),
                hot_working_set_bytes: Bytes(2000),
                usable_memory_bytes_per_node: Bytes(1000),
                query_requests_per_second: Count(200),
                cpu_millis_per_query: Millis(20),
                query_slots_per_node: Count(2),
                target_utilization_per_mille: PerMille(800),
            },
            embedding: EmbeddingInput {
                provider: EmbeddingProvider::Cuda,
                items_per_second: Count(100),
                measured_items_per_second_per_worker: Count(60),
                model_parameters: Count(1_000),
                output_dimensions: Count(10),
                output_element_bits: Count(8),
                weight_bits: Count(4),
                usable_gpu_memory: Bytes(10_000),
                runtime_workspace: Bytes(500),
                activation_bytes_per_item: Bytes(10),
                batch_items: Count(10),
            },
            failure_domains: FailureDomainInput {
                independent_domains: Count(3),
                tolerated_domain_failures: Count(1),
            },
        };
        let plan = plan(&input);
        assert!(plan.is_ok());
        let Ok(plan) = plan else {
            return;
        };
        assert_eq!(plan.compiler_workers.0, 1);
        assert_eq!(plan.index_nodes_by_storage.0, 3);
        assert_eq!(plan.index_nodes_by_query.0, 3);
        assert_eq!(plan.index_nodes_by_memory.0, 2);
        assert_eq!(plan.index_nodes.0, 3);
        assert_eq!(plan.embedding_vector_bytes_per_item.0, 10);
        assert_eq!(plan.embedding_workers.0, 2);
        assert!(plan.embedding_worker_fits_gpu);
    }

    #[test]
    fn replica_shortfall_is_a_typed_rejection() {
        let input = CapacityInput {
            tier: Tier::Starter,
            compiler: CompilerInput {
                requests_per_second: Count(1),
                cpu_millis_per_request: Millis(1),
                cores_per_worker: Count(1),
                target_utilization_per_mille: PerMille(500),
            },
            index: IndexInput {
                primary_durable_bytes: Bytes(1),
                replica_count: Count(1),
                usable_durable_bytes_per_node: Bytes(1),
                hot_working_set_bytes: Bytes(1),
                usable_memory_bytes_per_node: Bytes(1),
                query_requests_per_second: Count(1),
                cpu_millis_per_query: Millis(1),
                query_slots_per_node: Count(1),
                target_utilization_per_mille: PerMille(500),
            },
            embedding: EmbeddingInput {
                provider: EmbeddingProvider::CpuSimd,
                items_per_second: Count(1),
                measured_items_per_second_per_worker: Count(1),
                model_parameters: Count(1),
                output_dimensions: Count(1),
                output_element_bits: Count(8),
                weight_bits: Count(8),
                usable_gpu_memory: Bytes(100),
                runtime_workspace: Bytes(1),
                activation_bytes_per_item: Bytes(1),
                batch_items: Count(1),
            },
            failure_domains: FailureDomainInput {
                independent_domains: Count(2),
                tolerated_domain_failures: Count(1),
            },
        };
        assert!(matches!(
            plan(&input),
            Err(CapacityModelError::InsufficientReplicas {
                replicas: 1,
                failures: 1
            })
        ));
    }
}
