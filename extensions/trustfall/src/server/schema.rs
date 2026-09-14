//! Defines schema behavior for `backend-extension-trustfall`, whose purpose is to adapt borrowed graph facts to typed Trustfall queries.
//! This module owns the schema invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Fixed Trustfall grammar for the graph projection.

/// GraphQL schema whose only execution surface is a synchronous graph traversal.
pub(super) const GRAPH_SCHEMA: &str = r#"
schema { query: RootSchemaQuery }

type RootSchemaQuery {
  Entities: [Entity!]!
}

type Entity {
  high: Int!
  low: Int!
  neighbors: [Neighbor!]!
}

type Neighbor {
  entityHigh: Int!
  entityLow: Int!
  partition: Int!
}
"#;

/// Fixed typed operation executed by [`crate::server::TrustfallGraph::neighbors`].
pub(super) const NEIGHBORS_QUERY: &str = r#"
{
  Entities {
    high @filter(op: "=", value: ["$sourceHigh"])
    low @filter(op: "=", value: ["$sourceLow"])
    neighbors {
      entityHigh @output
      entityLow @output
      partition @output
    }
  }
}
"#;

/// GraphQL schema for lazy traversal of canonical semantic-image links.
pub(super) const SEMANTIC_GRAPH_SCHEMA: &str = r#"
schema { query: SemanticRootQuery }

type SemanticRootQuery {
  Entities: [SemanticEntity!]!
}

type SemanticEntity {
  high: Int!
  low: Int!
  outgoing: [SemanticLink!]!
}

type SemanticLink {
  entityHigh: Int!
  entityLow: Int!
  kind: Int!
  confidence: Int!
}
"#;

/// Fixed lazy operation executed by [`crate::server::SemanticTrustfallGraph::neighbors`].
pub(super) const SEMANTIC_NEIGHBORS_QUERY: &str = r#"
{
  Entities {
    high @filter(op: "=", value: ["$sourceHigh"])
    low @filter(op: "=", value: ["$sourceLow"])
    outgoing {
      entityHigh @output
      entityLow @output
      kind @output
      confidence @output
    }
  }
}
"#;

#[cfg(test)]
mod tests {
    use trustfall::Schema;
    use trustfall_core::frontend::parse;

    use super::{GRAPH_SCHEMA, NEIGHBORS_QUERY};

    #[test]
    fn fixed_schema_and_query_are_accepted_by_trustfall() {
        let schema = Schema::parse(GRAPH_SCHEMA).expect("fixed graph schema");
        parse(&schema, NEIGHBORS_QUERY).expect("fixed graph query");
    }
}
