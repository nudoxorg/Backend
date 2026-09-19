# PolicyRoot input for the bootstrap contract family. Keep this as metadata;
# execution remains in Nu and the v2 store will consume the same records later.
{
  schemaVersion = 1;
  source = "../../contracts/schema.json";
  unknownFields = "reject";
  canonicalEncoding = "canonical-json-v1";
  hashDomains = [
    "cell"
    "evaluation"
    "receipt"
    "lease"
    "evidence"
    "supersession"
    "context"
  ];
  commitScopeFixture = "../../fixtures/control-plane/scope-exactly-one.json";
}
