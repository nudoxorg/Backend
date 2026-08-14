//! Verify that `schema.graphql` is accepted by the trustfall schema parser.

use trustfall::Schema;

#[test]
fn schema_parses() {
    let _schema = Schema::parse(include_str!("../../schema.graphql"))
        .expect("schema.graphql must be a valid trustfall schema");
}
