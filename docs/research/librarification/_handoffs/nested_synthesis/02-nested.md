# Nested synthesis for task 02

# Nested agent agent-a2369db871622ee02
Description: Audit registry postgres/database modules
Size: 547688

Here is an exhaustive audit of all files in `/Users/philocalyst/Projects/Backend/workspace/registry`:

---

## 1. Module Overview (`lib.rs:1–103`)

**Public modules declared** (lib.rs:29–45): `blob`, `coordination`, `error`, `health`, `identity`, `index`, `ingest`, `metadata`, `package`, `persist`, `protocol`, `queue`, `resolve`, `runtime`, `schema`, `search`, `store`.

**Core types**:
- `Package` (lib.rs:62–69): wraps `Coordinates + Toolchain`
- `GlobalPackage` (lib.rs:86–103): `id: PackageId`, `package: Package`, `state: ResolutionState`, `facets: Option<SearchFacets>`

The `#[serde(default)]` on `facets` (lib.rs:101) ensures backwards compatibility for records stored before the facets column existed.

---

## 2. SQL Schema (`schema/mod.rs`)

Schema uses `sea_query` for DDL generation — no hand-written SQL, no `sqlx::migrate!` (schema/mod.rs:9–10). Applied via `schema_ddl()` → `GlobalStore::connect` on every boot, all `IF NOT EXISTS`.

### Tables

**`packages`** (schema/mod.rs:43–71, created at mod.rs:260–302):
- `id uuid PRIMARY KEY` — deterministic `PackageId` (UUIDv5)
- `language text NOT NULL CHECK(language IN (...))` — ecosystem token
- `origin_token text NOT NULL`
- `name_canonical text NOT NULL`
- `name_original text NOT NULL`
- `version_canonical text NOT NULL`
- `visibility text NOT NULL CHECK(...)`
- `owner_tenant uuid NOT NULL`
- `owner_kind text NOT NULL CHECK(...)`
- `toolchain jsonb NOT NULL`
- `created_at timestamptz NOT NULL DEFAULT current_timestamp`
- `updated_at timestamptz NOT NULL DEFAULT current_timestamp`

**`parse_status`** (schema/mod.rs:75–100, created at mod.rs:305–344):
- `package_id uuid PRIMARY KEY` + FK → `packages.id` ON DELETE CASCADE
- `state text NOT NULL CHECK(state IN ('unindexed','progressing','stored','failed','deadlettered'))`
- `phase text NULL CHECK(phase IS NULL OR phase IN (...))`
- `content_hash bytea NULL` — 32-byte BLAKE3 hash when `Stored`
- `attempts int NOT NULL DEFAULT 0`
- `failure jsonb NULL` — serialized `heart::Failure` when `Failed`/`DeadLettered`
- `facets jsonb NULL` — serialized `SearchFacets`, NULL until rich metadata extracted
- `needed bool NOT NULL DEFAULT false`
- `updated_at timestamptz NOT NULL DEFAULT current_timestamp`

**`jobs`** (schema/mod.rs:103–121, created at mod.rs:347–376):
- `id bigserial PRIMARY KEY`
- `package_id uuid NOT NULL UNIQUE` — one live job per package; FK → `packages.id` CASCADE
- `state text NOT NULL CHECK(state IN (...))`
- `attempts int NOT NULL DEFAULT 0`
- `enqueued_at timestamptz NOT NULL DEFAULT current_timestamp`
- `lease_until timestamptz NULL` — NULL or past = runnable
- `priority int NOT NULL DEFAULT 0`

**`outbox`** (schema/mod.rs:124–138, created at mod.rs:379–406):
- `seq bigserial PRIMARY KEY` — monotonic watermark cursor
- `package_id uuid NOT NULL` FK → `packages.id` CASCADE
- `generation bytea NOT NULL` — 32-byte content hash
- `sink_kind text NOT NULL CHECK(sink_kind IN (...))`
- `created_at timestamptz NOT NULL DEFAULT current_timestamp`

**`sink_watermarks`** (schema/mod.rs:141–151, created at mod.rs:409–428):
- `sink_kind text PRIMARY KEY CHECK(...)`
- `last_seq bigint NOT NULL DEFAULT 0`
- `updated_at timestamptz NOT NULL DEFAULT current_timestamp`

**`symbols`** (schema/mod.rs:154–168, created at mod.rs:431–453):
- `id uuid PRIMARY KEY` — deterministic `SymbolId`
- `package_id uuid NOT NULL` FK → `packages.id` CASCADE
- `fq_name text NOT NULL`
- `kind text NOT NULL CHECK(kind IN (...))`
- `generation bytea NOT NULL` — 32-byte generation hash

### Indexes (schema/mod.rs:461–525)
- `idx_packages_coords`: UNIQUE on `(language, origin_token, name_canonical, version_canonical)`
- `idx_parse_status_state`: on `parse_status.state` — scheduling scans
- `idx_parse_status_hash`: on `parse_status.content_hash` — freshness lookups
- `idx_jobs_runnable`: on `(jobs.priority, jobs.enqueued_at)` — dequeue hot path; note: schema/mod.rs:490–492 comments that the `WHERE lease_until IS NULL OR lease_until < now()` partial predicate is appended via raw SQL since sea-query 0.32 has no portable partial-index builder
- `idx_outbox_dedupe`: UNIQUE on `(outbox.package_id, outbox.generation, outbox.sink_kind)` — idempotency
- `idx_outbox_sink_seq`: on `(outbox.sink_kind, outbox.seq)` — watermark reads
- `idx_symbols_package`: on `symbols.package_id`

### Discriminant CHECK domains (schema/mod.rs:207–244)
All discriminant columns use `text` with `CHECK(col IN (...))` — no Postgres native `ENUM` types (schema/mod.rs:14–21 explains why: avoids `ALTER TYPE` transaction hazards). Domains are derived from Rust enum `strum::VariantNames::VARIANTS` for lockstep correctness.

- `STATE_VALUES` (mod.rs:207–208): `["unindexed", "progressing", "stored", "failed", "deadlettered"]`
- `PHASE_VALUES` (mod.rs:214): from `heart::Phase::VARIANTS`
- `SINK_KIND_VALUES` (mod.rs:221): from `heart::DerivedStore::VARIANTS`
- `SYMBOL_KIND_VALUES` (mod.rs:227): from `heart::SymbolKind::VARIANTS`
- `LANGUAGE_VALUES` (mod.rs:233): from `heart::Language::VARIANTS`
- `OWNER_KIND_VALUES` (mod.rs:238): from `heart::OwnerKind::VARIANTS`
- `VISIBILITY_VALUES` (mod.rs:243): from `heart::Visibility::VARIANTS`

---

## 3. All SQL Queries (schema/queries.rs — verbatim semantics)

### index module

**`upsert_package`** (queries.rs:57–115):
```sql
INSERT INTO packages (id, language, origin_token, name_canonical, name_original, version_canonical, visibility, owner_tenant, owner_kind, toolchain)
VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
ON CONFLICT (id) DO UPDATE
  SET name_original = EXCLUDED.name_original,
      visibility = EXCLUDED.visibility,
      owner_tenant = EXCLUDED.owner_tenant,
      owner_kind = EXCLUDED.owner_kind,
      toolchain = EXCLUDED.toolchain,
      updated_at = CURRENT_TIMESTAMP
RETURNING id
```

**`set_state`** (queries.rs:120–181):
```sql
INSERT INTO parse_status (package_id, state, phase, content_hash, attempts, failure, needed)
VALUES ($1, $2, $3, $4, $5, $6, $7)
ON CONFLICT (package_id) DO UPDATE
  SET state = EXCLUDED.state,
      phase = EXCLUDED.phase,
      content_hash = EXCLUDED.content_hash,
      attempts = EXCLUDED.attempts,
      failure = EXCLUDED.failure,
      needed = EXCLUDED.needed,
      updated_at = CURRENT_TIMESTAMP
```

**`set_facets`** (queries.rs:193–207):
```sql
UPDATE parse_status
SET facets = $1, updated_at = CURRENT_TIMESTAMP
WHERE package_id = $2
```

**`get_state`** (queries.rs:212–223):
```sql
SELECT state, phase, content_hash, needed, failure
FROM parse_status
WHERE package_id = $1
```

**`get_generation`** (queries.rs:229–235):
```sql
SELECT content_hash
FROM parse_status
WHERE package_id = $1 AND state = 'stored'
```

**`get_package`** (queries.rs:245–269):
```sql
SELECT p.id, p.language, p.origin_token, p.name_canonical, p.name_original,
       p.version_canonical, p.visibility, p.owner_tenant, p.owner_kind, p.toolchain,
       ps.facets
FROM packages p
LEFT JOIN parse_status ps ON ps.package_id = p.id
WHERE p.id = $1
```

**`select_progressing`** (queries.rs:273–279):
```sql
SELECT package_id
FROM parse_status
WHERE state = 'progressing'
```

**`upsert_symbol`** (queries.rs:284–317):
```sql
INSERT INTO symbols (id, package_id, fq_name, kind, generation)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (id) DO UPDATE
  SET package_id = EXCLUDED.package_id,
      fq_name = EXCLUDED.fq_name,
      kind = EXCLUDED.kind,
      generation = EXCLUDED.generation
```

**`symbols_for`** (queries.rs:329–349):
```sql
SELECT s.id, s.package_id, s.fq_name, s.kind, p.language
FROM symbols s
INNER JOIN packages p ON p.id = s.package_id
WHERE s.package_id = $1
```

### queue module

**`enqueue`** (queries.rs:364–380):
```sql
INSERT INTO jobs (package_id, state, priority)
VALUES ($1, 'unindexed', $2)
ON CONFLICT (package_id) DO NOTHING
RETURNING id
```

**`dequeue_batch`** (queries.rs:400–433) — the `FOR UPDATE SKIP LOCKED` hot path:
```sql
UPDATE jobs
SET lease_until = $lease_until, attempts = attempts + 1
WHERE id IN (
    SELECT id FROM jobs
    WHERE lease_until IS NULL OR lease_until < CURRENT_TIMESTAMP
    ORDER BY priority DESC, enqueued_at ASC
    LIMIT $limit
    FOR UPDATE SKIP LOCKED
)
RETURNING id, package_id, state, attempts, enqueued_at, lease_until
```

**`complete`** (queries.rs:447–453) — guarded delete:
```sql
DELETE FROM jobs
WHERE id = $1
  AND lease_until IS NOT NULL
  AND lease_until > CURRENT_TIMESTAMP
RETURNING id
```

**`fail_retry`** (queries.rs:460–468) — retry branch:
```sql
UPDATE jobs
SET state = 'failed', lease_until = $next_visible_at
WHERE id = $1
  AND lease_until IS NOT NULL
  AND lease_until > CURRENT_TIMESTAMP
RETURNING id
```

**`fail_deadletter`** (queries.rs:474–480) — dead-letter branch:
```sql
DELETE FROM jobs
WHERE id = $1
  AND lease_until IS NOT NULL
  AND lease_until > CURRENT_TIMESTAMP
RETURNING id
```

**`get_attempts`** (queries.rs:485–490):
```sql
SELECT attempts FROM jobs WHERE id = $1
```

**`get_job_id_for_package`** (queries.rs:495–500):
```sql
SELECT id FROM jobs WHERE package_id = $1
```

**`get_package_for_job`** (queries.rs:505–510):
```sql
SELECT package_id FROM jobs WHERE id = $1
```

**`renew_lease`** (queries.rs:518–525) — heartbeat:
```sql
UPDATE jobs
SET lease_until = $next_lease_until
WHERE id = $1
  AND lease_until IS NOT NULL
  AND lease_until > CURRENT_TIMESTAMP
RETURNING id
```

**`reclaim_expired_leases`** (queries.rs:531–536):
```sql
UPDATE jobs
SET lease_until = NULL
WHERE lease_until < CURRENT_TIMESTAMP
```

### outbox module

**`append_one`** (queries.rs:557–579):
```sql
INSERT INTO outbox (package_id, generation, sink_kind)
VALUES ($1, $2, $3)
ON CONFLICT (package_id, generation, sink_kind) DO NOTHING
```

**`append_all`** (queries.rs:584–610) — multi-row insert for all SinkKinds:
```sql
INSERT INTO outbox (package_id, generation, sink_kind)
VALUES ($1, $2, 'sink1'), ($1, $2, 'sink2'), ...
ON CONFLICT (package_id, generation, sink_kind) DO NOTHING
```

**`read_since`** (queries.rs:616–630):
```sql
SELECT seq, package_id, generation, sink_kind, created_at
FROM outbox
WHERE sink_kind = $1 AND seq > $2
ORDER BY seq ASC
LIMIT $3
```

**`head`** (queries.rs:635–640):
```sql
SELECT MAX(seq)::bigint FROM outbox WHERE sink_kind = $1
```

**`advance_watermark`** (queries.rs:647–667):
```sql
INSERT INTO sink_watermarks (sink_kind, last_seq)
VALUES ($1, $2)
ON CONFLICT (sink_kind) DO UPDATE
  SET last_seq = GREATEST(sink_watermarks.last_seq, $2),
      updated_at = CURRENT_TIMESTAMP
```

**`try_advisory_lock`** (queries.rs:688–694) — Postgres advisory lock:
```sql
SELECT pg_try_advisory_lock($1, $2)
```
Lock class: `ADVISORY_LOCK_CLASS_OUTBOX_SINK = 0x0B0B_0001` (queries.rs:675), `objid` = sink iteration index.

**`advisory_unlock`** (queries.rs:699–705):
```sql
SELECT pg_advisory_unlock($1, $2)
```

**`read_watermark`** (queries.rs:710–715):
```sql
SELECT last_seq FROM sink_watermarks WHERE sink_kind = $1
```

**`min_consumed_watermark`** (queries.rs:740–748):
```sql
SELECT CASE WHEN count(*) = {n} THEN COALESCE(min(last_seq), 0) ELSE 0 END
FROM sink_watermarks
```
Where `{n}` = `sink_count()` (total number of SinkKind variants).

**`delete_consumed_below`** (queries.rs:755–759):
```sql
DELETE FROM outbox WHERE seq <= $1
```

### search module

**`changed_since`** (queries.rs:797–830):
```sql
SELECT p.id, p.language, p.origin_token, p.name_canonical, p.name_original,
       p.version_canonical, p.toolchain, p.updated_at,
       ps.state, ps.phase, ps.content_hash, ps.needed, ps.failure, ps.facets
FROM packages p
LEFT JOIN parse_status ps ON ps.package_id = p.id
WHERE p.updated_at > $1
ORDER BY p.updated_at ASC
LIMIT $2
```

### persist module

**`reset_transient_parse_status`** (queries.rs:847–859):
```sql
UPDATE parse_status
SET state = 'unindexed', phase = NULL, needed = false, updated_at = CURRENT_TIMESTAMP
WHERE state = 'progressing'
```

**`clear_all_leases`** (queries.rs:865–873):
```sql
UPDATE jobs
SET lease_until = NULL
WHERE lease_until IS NOT NULL
```

---

## 4. Postgres-Specific Features Used

| Feature | Location |
|---|---|
| `FOR UPDATE SKIP LOCKED` | queue/mod.rs:360–365 docstring; queries.rs:414–417 |
| Session-level advisory locks (`pg_try_advisory_lock` / `pg_advisory_unlock`) | coordination.rs:226–253; queries.rs:688–705 |
| `GREATEST(...)` in ON CONFLICT upsert | queries.rs:655–661 (watermark advance) |
| `COALESCE(min(last_seq), 0)` with CASE guard | queries.rs:743–746 |
| `bigserial` auto-increment PKs | `outbox.seq`, `jobs.id` (schema/mod.rs:383, 351) |
| `uuid` column type for all entity IDs | schema/mod.rs:264, 309, 281, 435 |
| `bytea` for content hashes (32 bytes) | schema/mod.rs:325, 385, 444 |
| `jsonb` for Toolchain, Failure, Facets | schema/mod.rs:288, 327, 328 |
| `timestamptz` for all timestamps | throughout schema/mod.rs |
| `ON CONFLICT ... DO UPDATE` (upsert) | queries.rs:100–113, 167–179, 307–315, 601–609 |
| `ON CONFLICT ... DO NOTHING` | queries.rs:373–377, 571–578 |
| `RETURNING` clause | queries.rs:112, 180, 318, 379, 432, 453, 480 |
| `LEFT JOIN` | queries.rs:261–269, 819–823 |
| `INNER JOIN` | queries.rs:338–347 |
| Foreign key `ON DELETE CASCADE` | schema/mod.rs:336–343, 367–374, 398–405, 444–451 |
| CHECK constraints (text-enum emulation) | schema/mod.rs:267–269, 277–280, 281–284, 311–322 |
| SQLSTATE retry codes `40001` (serialization), `40P01` (deadlock), `57P03`, `53300` | error.rs:620–621 |

---

## 5. CAS Implementation (`store.rs`)

**Key layout** (store.rs:99–116):
- `cas/{blake3-hex-64-chars}` — content-addressed immutable blobs
- `ptr/{package-uuid}` — mutable pointer (32 raw bytes = ContentHash) from PackageId to current manifest hash

**Operations**:
- `put_section` (store.rs:122–136): checks existence first (idempotent), verifies integrity before put via `verify_integrity`
- `get_section` (store.rs:140–146): fetches + verifies BLAKE3 hash
- `get_section_range` (store.rs:151–161): partial ranged read, no integrity check (documented trade-off)
- `put_manifest` (store.rs:165–190): validates manifest, serializes via postcard, puts as a CAS section, then repoints `ptr/` — the pointer repoint is a single atomic object-store replace
- `get_manifest` (store.rs:194–210): reads `ptr/`, resolves to CAS hash, fetches manifest, validates
- `exists` (store.rs:214–220): HEAD on the pointer path
- `list_cas` (store.rs:240–264): lists all `cas/` objects, filters to 64-hex-char names (32-byte BLAKE3), returns `Vec<ContentHash>`

**Integrity gate** (`error.rs:651–658`): `verify_integrity` computes BLAKE3 of bytes and compares to expected `ContentHash`. Called on every read and before every write.

**Connection probe** (store.rs:277–286): HEADs `cas/.reachability-probe`; `NotFound` is success.

---

## 6. Queue Implementation (`queue/mod.rs`)

**Mechanism**: `SELECT ... FOR UPDATE SKIP LOCKED LIMIT n` wrapped in an `UPDATE ... WHERE id IN (subquery) RETURNING ...` — one atomic statement that claims a disjoint set per worker (queue/mod.rs:360–391; queries.rs:400–433).

**`LeasedJob` witness type** (queue/mod.rs:80–117): compile-time proof a job was dequeued by this worker; consumed by `complete`/`fail` — cannot be minted externally. Does NOT prove lease still held (queue/mod.rs:50–76).

**Lease guard** (queries.rs:439–442): `lease_until IS NOT NULL AND lease_until > CURRENT_TIMESTAMP` applied to `complete`, `fail_retry`, `fail_deadletter`, `renew_lease`. Empty `RETURNING` → `QueueError::LeaseLost`.

**Retry policy** (queue/mod.rs:133–166): exponential backoff (`base * 2^attempt`, capped at `max_backoff`); `RetryDecision::Retry { after }` vs `RetryDecision::DeadLetter` based on `FailureKind::is_retriable() && attempts < max_attempts`.

**Priority** (queue/mod.rs:174–189): `Priority(i32)` newtype, `ORDER BY priority DESC, enqueued_at ASC`. Default = 0.

**State discriminant decoding** (queue/mod.rs:284–311): `deadlettered` → `DeadLettered`, `failed` → `Failed` with stub payloads; real payloads in `parse_status.failure`. Unknown token → `Unindexed`.

**Reclaim** (queue/mod.rs:524–531; queries.rs:531–536): `UPDATE jobs SET lease_until = NULL WHERE lease_until < now()` — periodic sweeper.

---

## 7. Coordination / Outbox Pattern (`coordination.rs`)

**Pattern**: transactional outbox (coordination.rs:4–13). On emit, one `OutboxEntry` per `SinkKind` is inserted in the same transaction as the `parse_status → Stored` transition and the `set_facets` update. Impossible for manifest to exist without downstream sinks being notified (coordination.rs:130–183).

**`record_stored`** (coordination.rs:147–183): three-step atomic transaction:
1. `set_state_transaction` → `Stored { hash }`
2. `set_facets` UPDATE
3. `append_all` multi-row outbox insert

All three commit in a single transaction (coordination.rs:180).

**Dedup key**: `(package_id, generation, sink_kind)` UNIQUE constraint → `ON CONFLICT DO NOTHING` (coordination.rs:108–126, queries.rs:570–578). Re-emit of same generation is silent no-op.

**Polling** (coordination.rs:187–198): `read_since(kind, watermark, limit)` — per-consumer poll.

**Watermark advance** (coordination.rs:215–221): `GREATEST(...)` upsert — monotonic, idempotent.

**Advisory lock** (coordination.rs:225–253): `pg_try_advisory_lock($class, $objid)` on a pinned connection for per-replica sink drain exclusion. Lock class `0x0B0B_0001`, objid = sink iteration index. `SinkLockGuard` (coordination.rs:321–356) holds the connection; explicit `release()` runs `pg_advisory_unlock`, `Drop` returns connection (postgres frees session advisory locks on connection reset).

**GC** (coordination.rs:257–288): `gc_consumed()` — reads `min_consumed_watermark()` (a CASE that requires all sinks to have watermark rows), then `delete_consumed_below(floor)`. Safe floor = 0 when any sink is missing its watermark row.

---

## 8. GlobalStore / Index (`index/mod.rs`)

**Connect** (index/mod.rs:83–118): runs `SELECT 1`, then applies `schema_ddl()` statements in a transaction (all `IF NOT EXISTS`).

**`upsert`** (index/mod.rs:142–158): two-statement transaction — `upsert_package` + `set_state`, committed atomically.

**`get`** (index/mod.rs:204–223): `get_state` + `get_package` (two queries), assembles `GlobalPackage` with facets from column 10 of the package+parse_status join.

**`generation`** (index/mod.rs:226–245): `get_generation` → 32-byte bytea decode via `codec::generation_from_bytes`.

**`upsert_symbol`** / **`symbols_for`** (index/mod.rs:248–282): upsert and read serving-projection symbols.

**Row decoders**:
- `row_to_state` (index/mod.rs:369–383): columns 0–4 (state, phase, content_hash, needed, failure)
- `row_to_package` (index/mod.rs:330–355): columns 1,2,4,5,6,8,9 (positional per `get_package` join)
- `row_to_facets` (index/mod.rs:358–366): column 10 (the `ps.facets` from the left join)
- `row_to_symbol` (index/mod.rs:306–327): columns 0–4 + ecosystem join

**Probe** (index/mod.rs:431–446): `get_state(PackageId::nil())` — `NotFound` = healthy.

---

## 9. Persist / Crash Recovery (`persist.rs`)

**`reconcile_on_start`** (persist.rs:34–82):
1. `select_progressing()` — reads all packages stuck in `Progressing`
2. `reset_transient_parse_status()` + `clear_all_leases()` — in one transaction (persist.rs:58–69)
3. `queue.enqueue(package)` for each recovered package

**`reset_transient`** (persist.rs:86–95): single-package state reset; `Progressing` → `Unindexed { needed: false }`, all other states pass through.

---

## 10. Error Handling (`error.rs`)

**SQLSTATE retryable codes** (error.rs:620–621): `"40001"` (serialization_failure), `"40P01"` (deadlock_detected), `"57P03"` (cannot_connect_now), `"53300"` (too_many_connections).

**Object-store retryable** (error.rs:265–272): `Generic { .. }` and `JoinError { .. }` are transient; everything else is permanent.

**`verify_integrity`** (error.rs:651–658): BLAKE3 digest comparison; all content-addressed reads pass through this.

**`JobIdForError`** (error.rs:384): lightweight UUID carrier to avoid pulling full `Job` type into error variants.

---

## 11. Health (`health/mod.rs`)

**Required backends** (health/mod.rs:51): `[BackendKind::Postgres, BackendKind::ObjectStore]` — if either is down, `Health::Down`. Other backends (Qdrant, Terminus, Tantivy) → `Health::Degraded(vec![...])`.

**`aggregate`** (health/mod.rs:46–69): pure function over `&[Probe]` — caller runs `tokio::join!` on the concrete store handles and passes results; no boxing of probe futures.

---

## 12. Codec (`schema/codec.rs`)

**Type mappings**:
- `PackageId/SymbolId` ↔ `uuid` — via serde round-trip through JSON string (codec.rs:92–119)
- `ContentHash/Generation` ↔ `bytea` (exactly 32 bytes) — `hash_to_bytes`/`hash_from_bytes` (codec.rs:127–141)
- All discriminant enums ↔ `text` tokens via `strum::FromStr`/`IntoStaticStr`
- `Failure`, `Toolchain`, `SearchFacets` ↔ `jsonb` via `serde_json`
- `ResolutionState` ↔ `(state text, phase text, content_hash bytea, attempts int, failure jsonb, needed bool)` tuple — `state_to_columns`/`state_from_columns` (codec.rs:218–267)

**Custom origin tokens** (codec.rs:325–338): well-known registries have short tokens (`"crates.io"`, `"npm"`, `"pypi"`, `"flakehub"`, `"nuget"`); custom origins reconstruct their URL from the token string.

---

## 13. Identity (`identity.rs`)

Thin re-export module (identity.rs:1–15): re-exports `heart::identity::{EntryUri, SymbolId, PackageId, namespace}` and `heart::package::Coordinates as PackageCoordinates`. No identity is minted locally — all derivation is delegated to heart's deterministic UUIDv5 derivers.

---

## 14. Protocol (`protocol.rs`)

Wire protocol for `POST /compile` (protocol.rs:1–211):
- **Transport**: postcard (`application/x-postcard`) — not JSON
- `CompileRequest` (protocol.rs:61–69): coordinates + toolchain + files
- `CompileResponse` (protocol.rs:73–93): `Ok { surface: Vec<u8>, references: Vec<WireFile>, identifiers: Vec<String> }` or `Err { kind, message }`
- `WireReference` (protocol.rs:117–127): `target`, `span_start: u64`, `span_end: u64`, `kind: u8`
- `WireTarget` (protocol.rs:131–136): `External { path, dependency }` or `Local(String)`
- `ReferenceKind` discriminant table (protocol.rs:200–209): uses `ir::syntax::ReferenceKind`'s `#[repr(u8)]` + `strum::FromRepr` — not a hand-rolled match

---

## 15. Version Resolution (`resolve.rs`)

Not postgres-related; pure application logic:
- `VersionRequest` (resolve.rs:30): `Latest | Exact(PackageVersion) | Constraint(RangeConstraint)`
- `RangeConstraint` (resolve.rs:40–45): `Semver(semver::VersionReq)` or `Range { ecosystem, spec }` (for PEP 440, NuGet interval notation)
- `published_versions` (resolve.rs:258–285): HTTP GET to ecosystem API; filters yanked crates.io versions
- `select` (resolve.rs:115–173): pure filter+`pick_best` — stable-before-prerelease preference
- `nuget` submodule (resolve.rs:359–585): full NuGet version parser (4-part numeric + prerelease) and interval-notation range matcher (`[1.0,2.0)`, `(1.0,)`, bare `1.2.3` = minimum)


# Nested agent agent-a73e6692df7052d21
Description: Audit registry blob, ingest, metadata modules
Size: 243526

Here is the exhaustive audit of the nine files in `/Users/philocalyst/Projects/Backend/workspace/registry`:

---

## 1. CAS / Blob Storage

### `blob/mod.rs` — Core manifest and wire types

**BlobManifest** (lines 44–66): The central serializable record for a package snapshot. Fields:
- `package: PackageId` — which package.
- `files: nonempty::NonEmpty<FileEntry>` — type-enforced non-emptiness; file bytes live separately in `cas/{hash}`.
- `ir_ref: ContentHash` — pointer to the serialized IR object.
- `references_ref: ContentHash` — pointer to the CST-free cross-reference data.
- `toolchain: Toolchain` — provenance.

**Two deliberately distinct hashes** (lines 68–106, comment block): The module-level comment is explicit that these must never be unified:
- Hash ① **generation stamp** (`identity_bytes`): bespoke length-prefixed encoding of `(path, file-hash)` pairs + `ir_ref` bytes + `references_ref` bytes + postcard-encoded toolchain. Purpose: stable cache freshness check that survives schema field additions.
- Hash ② **CAS key** (`manifest_cas_key`): `ContentHash::of_bytes(postcard::to_allocvec(self))`. Purpose: the blob's address in `cas/` and the value written to `ptr/{package-uuid}`.

**`identity_bytes`** (lines 117–139): Sorts `FileEntry` items by path defensively even before `validate()`, then length-prefixes every segment with a `u64` LE byte count. `toolchain` is postcard-encoded separately to avoid silent stamp-busting on struct changes.

**`manifest_cas_key`** (lines 159–162): Single canonical definition of the blob address — `postcard::to_allocvec(self)` → `ContentHash::of_bytes`. Callers are explicitly told not to inline this.

**`validate`** (lines 166–181): Checks strict ascending path order (windows(2) pairwise comparison). `Equal` → `DuplicateFilePathInManifest`. `Greater` → `ManifestFilesNotSorted`. Non-emptiness is already carried by `NonEmpty<T>`.

**`FileEntry`** (lines 189–200): `path: SmolStr`, `hash: ContentHash`, `size: u64`. Bytes not inline — stored at `cas/{hash}`.

**`ReferenceSet`** (lines 213–260): Bespoke codec for `ir::syntax::ResolvedReference` (which has no serde impls). `encode` → `postcard::to_allocvec`; `decode` → `postcard::from_bytes`. Wire form uses `WireFile { path, references: Vec<WireReference> }`.

**Wire types** (lines 270–350):
- `WireReference` (lines 277–283): `target: WireTarget`, `span_start: u64`, `span_end: u64`, `kind: u8`.
- `WireTarget` (lines 285–289): `External { path: String, dependency: String }` or `Local(String)`.
- `WireReference::into_reference` (lines 301–310): Validates `span_start <= span_end`; returns `InvertedReferenceSpan` error if violated.
- `WireTarget::from_target` (lines 314–325): Rejects non-UTF-8 paths (`NonUtf8ReferencePath`).
- `kind_to_wire` / `kind_from_wire` (lines 342–350): `u8` discriminants via `#[repr(u8)]` + `FromRepr`; `from_repr` fails with `UnknownReferenceKindDiscriminant { wire }`.

No GC mechanism is present in this file. No `MemoryCas`, `DiskCas`, `RegistryCas`, or `object_store` usage appears here — those abstractions appear to live in a `store` module (referenced at `crate::store::Store`).

---

### `blob/creation.rs` — BlobBuilder (streaming CAS assembly)

**`PendingSection`** (lines 25–31): `hash: ContentHash` + `bytes: bytes::Bytes`. The unit of deferred `cas/` writes.

**`BlobBuilder`** (lines 40–48): Fields — `package`, `toolchain`, `files: Vec<FileEntry>`, `pending: Vec<PendingSection>`, `digest: ContentHasher`, `ir_ref: Option<ContentHash>`, `references_ref: Option<ContentHash>`. Never holds more than one file in memory at a time (streaming contract).

**`address`** (lines 66–71): Private helper — hashes `bytes` → `ContentHash::of_bytes`, folds hash into running `digest` via `self.digest.update(hash.as_bytes())`, queues `PendingSection`, returns the hash.

**`push_file`** (lines 77–86): Checks for duplicate path in `self.files` before proceeding (returns `DuplicateFilePathInBuilder`). Folds `path.len()` (u64 LE) and `path.as_bytes()` into the running digest. Calls `address(bytes)` to hash + queue. The digest feeds both path and content — arrival-order dependent until `finalize`.

**`set_ir`** (lines 90–95): Guards against double-attachment (`IrSectionAttachedTwice`). Calls `address(ir_bytes)`.

**`set_references`** (lines 100–107): Guards against double-attachment (`ReferencesSectionAttachedTwice`). Calls `refs.encode()` then `address`.

**`finalize`** (lines 114–135): Requires `ir_ref`, `references_ref`, and at least one file (else `MissingIrSection`, `MissingReferencesSection`, `ManifestHasNoFiles`). Sorts `files` by path for canonical ordering. Wraps in `nonempty::NonEmpty::from_vec`. Calls `manifest.validate()`. Returns `(BlobManifest, Vec<PendingSection>)` — no I/O performed.

**`provisional_generation`** (line 139): `self.digest.finalize()` — a work-in-progress snapshot, not the committed value.

**`source_files`** (lines 145–150): Joins `FileEntry` with `PendingSection` by hash to yield `(&SmolStr, &bytes::Bytes)` — used by the compile phase for language producers without a second archive pass.

**First-write-wins semantics**: Not implemented in the builder. The builder is I/O-free; idempotency is in the `store::put_section` path (see `emit.rs`).

---

### `blob/emit.rs` — Terminal blob emission

**`Emitted`** (lines 23–29): `written: usize` (new sections) + `deduped: usize` (already-present, no-op).

**`emit`** (lines 41–73): Three-phase async function:
1. **CAS writes** (lines 50–55): Iterates `sections`, calls `store.put_section(section).await`. Return value `true` = written, `false` = deduped (first-write-wins semantics are enforced inside `put_section`). Retry-safe: every already-present hash is a cheap no-op.
2. **Manifest** (lines 59–60): `store.put_manifest(&manifest).await` — stores the manifest as its own `cas/` object and repoints `ptr/{package-uuid}` at it. Returns `generation`.
3. **Outbox fan-out** (lines 64–65): `SinkKind::iter().collect()` then `outbox.append(package, generation, &kinds)`. One idempotent intent per derived sink (qdrant, terminus, tantivy).

**No GC** is visible in any blob module. No `object_store` crate usage is visible in these three files; `store::Store` is an opaque abstraction.

---

## 2. Ingest Module

### `ingest/mod.rs` — Archive ingest orchestration

**`ExtractionLimits`** (lines 24–43): The safety envelope — `max_total_bytes: u64`, `max_file_bytes: u64`, `max_files: usize`, `max_path_depth: usize`. `DEFAULT` constants: 512 MiB total, 64 MiB per file, 50,000 files, depth 32.

**`ingest_archive`** (lines 57–98): Two-stage async function.
- **Stage 1** (lines 79–89): Reads the *compressed* stream with `.take(ceiling + 1)` into a `Vec<u8>`. If `compressed.len() > ceiling` → `CompressedSizeExceeded`. This bounds compressed data using the uncompressed ceiling — logically sound because a compressed stream exceeding the uncompressed ceiling cannot possibly extract under it.
- **Stage 2** (lines 93–97): Hands off to `spawn_blocking` → `extract_into_builder`. blake3 hashing and tar walking run on a blocking thread.

**`extract_into_builder`** (lines 102–168): Synchronous half on blocking thread.
- Decompresses via `flate2::read::GzDecoder` (TarGz), `zstd::stream::read::Decoder` (TarZst), or raw `Cursor` (Tar) (lines 114–121).
- Walks `tar::Archive::entries()`.
- **Non-UTF-8 path rejection** (lines 132–139): `std::str::from_utf8(&entry.path_bytes())` fails → `NonUtf8Path { bytes }`. This is classified as `UnsafeArchive` because non-UTF-8 is "jail-ambiguous."
- **Declared-size pre-check** (lines 143–149): Header's declared size is checked against `max_file_bytes` before any read. A lying header cannot force materialization past the ceiling.
- **`sanitize_entry` call** (lines 152–158): Passes a closure that does `.take(limits.max_file_bytes + 1).read_to_end(&mut bytes)` — the +1 is to detect post-limit data (the budget enforces the actual ceiling).
- Admitted files go into `BlobBuilder::push_file`.
- Logs `files = admitted` on completion (line 166).

**`ArchiveFormat`** (lines 171–179): `TarGz`, `TarZst`, `Tar`.

---

### `ingest/extract.rs` — Sanitizing extractor (security boundary)

**`EntryAllowlist`** (lines 24–56): Default-deny. `SAFE` = `{ regular: true, directories: true }`. `admits` method (lines 38–51): symlinks, hardlinks, character/block devices, FIFOs, and `Other` are hard-coded `false` — not configurable.

**`SanitizedEntry`** (lines 59–68): `path: SmolStr` (jailed, normalized), `bytes: Bytes`.

**`Budget`** (lines 73–115): Tracks `total_bytes: u64` and `file_count: usize` against `ExtractionLimits`. `charge(size)` (lines 90–115): Atomically checks (all-or-nothing on failure) — file count first, per-file size second, total bytes third. Uses `saturating_add` to avoid overflow on total. Nothing is consumed on failure.

**`jail_path`** (lines 124–159): The single choke point for path safety. Rejects:
- Absolute paths starting with `/` (line 131).
- Backslash separators (line 131).
- Embedded NULs (line 131).
- Windows-drive absolute paths (`X:...`) (lines 129–131).
- `..` that would pop past the root — `segments.pop().is_none()` → `PathTraversal` (lines 140–143).
- Empty paths after normalization (lines 148–150).
- Paths exceeding `root_depth_limit` (lines 151–158).
- Skips `""` and `"."` components silently (line 139).
- Returns `SmolStr::from(segments.join("/"))` — a clean, normalized relative path.

**`classify`** (lines 162–174): Maps `tar::EntryType` to `EntryKind`. `Regular | Continuous | GNUSparse` → `Regular`. `Directory` → `Directory`. `Symlink` → `Symlink`. `Link` (hardlink) → `Hardlink`. `Char/Block/Fifo` → their kinds. Everything else → `Other`.

**`sanitize_entry`** (lines 183–213): Ordered pipeline:
1. Entry-type allowlist check (lines 192–198) — disallowed → `DisallowedEntry`.
2. Path jail (line 200) — `jail_path(limits.max_path_depth, raw_path)`.
3. Directories: admitted (path validated) but yield `Ok(None)` — no bytes (lines 203–205).
4. `read_bytes()` closure called (line 208).
5. `budget.charge(bytes.len() as u64)` (lines 209–211) — violation → `IngestError::Unsafe(locate(violation, &path))`.
6. Returns `Ok(Some(SanitizedEntry { path, bytes }))`.

**`locate`** (lines 217–224): Fills in the path on a `FileTooLarge` budget violation (budget doesn't know paths).

---

## 3. Metadata Module

### `metadata/mod.rs` — Module root

**`PackageMetadata`** (lines 20–27): `id: PackageId` + `links: StoreLinks`.

**`StoreLinks`** (lines 31–39): `vector: bool`, `graph: bool`, `text: bool` — the cross-store link bitmap. `fully_linked` (lines 41–44): returns `self.vector && self.graph && self.text`.

Public re-exports: `RichMetadata`, `SearchFacets`, `normalize_keyword`, `Specifics`, `Synonyms`.

---

### `metadata/hash.rs` — Content addressing re-exports

Lines 9: A pure re-export module — `pub use heart::content::{ContentHash, ContentHasher, Freshness}`. No logic. Exists to give registry callers a single import path. Canonical generation stamp is documented as `crate::BlobManifest::identity_bytes`.

---

### `metadata/heuristics.rs` — Keyword normalization + synonym/specifics tables

**`normalize_keyword`** (lines 25–45): Fast path via `is_already_canonical` (all lowercase alphanumeric + hyphens). Otherwise:
- Splits on whitespace, `_`, `-`.
- `apply_well_known_replacements`: `I/O` → `io`, `C++` → `cpp`, `C/C++` → `c-or-cpp`, `OSes` → `os`, `LaTeX` → `latex`, `XeTeX` → `xetex`, `CHAdeMO` → `chademo` (lines 57–67).
- `split_on_slashes`: splits on `/`.
- `apply_formatting_rules` pipeline (lines 74–85): strip plural acronyms, replace `%`/`&`, normalize byte suffixes (`iB`), normalize mixed casing (iOS-style camelCase, TeX-style), format standard prefixes (`rfc-NNN`, `iso-NNN`, etc.), lowercase `*Script` tokens.
- `inline_kebab_case` via `heck::AsKebabCase`.
- `truncate_keyword`: target 55 chars, max 65; appends `…` on truncation.

**`Synonyms`** (lines 170–359): Loads `tag-synonyms.csv` (hash comment, no-headers, trimmed, flexible, up to 2500 entries). Fields: `mapping: HashMap<SmolStr, (SmolStr, u8)>`.
- Score field: 0–5; scores >5 are logged as errors.
- Duplicate handling: higher-score wins (lines 213–220).
- Cycle detection (lines 235–259): follows chains up to depth 20 via `take(21)`; at depth ≥ 20 the originating key is removed.
- `get` (lines 264–268): relevance = `(votes/5.0 + 0.1).min(1.0)`.
- `normalize` (lines 333–344): two-hop normalization with `min_votes` threshold, returns `(canonical, combined_weight)`.
- `max_normalize` (lines 279–329): recursive up to depth 2; handles split-hyphen normalization of sub-words.
- `map_normalize` (lines 348–354): single-hop, returns original word on miss.

**`Specifics`** (lines 459–692): Loads `specific-keywords.txt` and `bland-keywords.txt`. Four internal maps:
- `combine: HashMap<SmolStr, String>` — keyword + combinations (` & ` lines).
- `relate: HashMap<SmolStr, String>` — keyword relations (` < ` and ` + ` lines).
- `split: HashMap<SmolStr, SplitOffsets>` — dot-separated split definitions.
- `bland: HashSet<SmolStr>` — generic/overly-common keywords from `bland-keywords.txt`.

**`SpecificsAction`** (lines 366–383): `Increase(SpecificsCond)`, `Decrease(SpecificsCond)`, `Fold(SpecificsCond)`.

**`SpecificsCond`** (lines 386–453): Carries `with: &str` and `include_exclude: &str` for `!`/`?` conditional modifiers. `matches(callback)` (lines 429–431): `only_if_all().all(callback) && !unless().any(callback)`.

**`parse_relation`** (lines 525–575): Parses ` < `, ` & `, ` + ` lines into `relate` or `combine`. Validates that stems are non-empty, non-self-referential, and have required `+/-` prefixes when `must_have_prefix=true`.

**`parse_split`** (lines 578–602): Parses `foo.bar.baz` → stores as `"foo-bar-baz"` with a `Vec<u8>` of byte offsets indicating split points (up to 4 dots).

**`get_relations`** (lines 649–665): Returns `SpecificsAction` iterator: `+` prefix → `Increase`, `-` prefix → `Decrease`, no prefix → `Fold`.

**`is_bland`** (lines 683–688): Returns `Some(SpecificsAction::Fold(...))` if keyword is in the bland set.

---

### `metadata/rich.rs` — Rich metadata extraction (Phase 2)

**`STOPWORDS`** (lines 18–136): 87-entry sorted slice for `binary_search`.

**`IDENT_STOPWORDS`** (lines 138–147): 65-entry sorted slice for identifier word filtering.

**`SKIP_SECTIONS`** (lines 150–179): README section headers to ignore: license, contributing, installation, changelog, credits, acknowledgements, authors, todo, code of conduct, building, setup, msrv, copyright, sponsors.

**`KNOWN_CATEGORIES`** (lines 182–198): 15 well-known category slugs for inference: `async-io`, `parser`, `web-programming`, `command-line-utilities`, `encoding`, `compression`, `cryptography`, `data-structures`, `algorithms`, `science`, `embedded`, `wasm`, `graphics`, `database`, `network-programming`.

**`Score`** (lines 206–295): Weighted quality accumulator. `scores: Vec<(f64, f64, &'static str)>` = (value, max, label). `total()` = `Σclamp(v,0,max) / Σmax`. Methods: `has` (boolean), `n` (integer clamped), `frac` (0..=1 fraction), `score_f` (raw), `group` (embeds sub-score).

**`ScoreAdj`** (lines 213–306): Handle for post-hoc score adjustment — `mul(by)` and `adj(f)`.

**`ExtractionInput`** (lines 315–339): Input to the extractor:
- `name: &str`, `description: Option<&str>`, `manifest_keywords: &[String]`, `manifest_categories: &[String]`, `readme: Option<&str>`, `identifiers: &[String]`, `dependencies: &[String]`, `has_repository: bool`, `has_documentation: bool`, `has_license: bool`, `loc: u32`.

**`RichMetadata`** (lines 343–370): `keywords: Vec<(f32, SmolStr)>` (descending weight, capped 20, top normalized to 1.0), `categories: Vec<(f32, SmolStr)>`, `quality: f32`. `keyword_text()` joins slugs with spaces; `quality_ppm()` maps to 0..=1,000,000.

**`SearchFacets`** (lines 381–415): The `Eq`-able projection of `RichMetadata` — `keywords: Vec<SmolStr>` (weights dropped), `quality_ppm: u32`. Exists because `RichMetadata` has `f32` and cannot be `Eq`. `from_rich` (lines 393–397) derives it. `quality()` (lines 411–414) converts back to `f32`.

**`prose_words`** (lines 432–445): Splits on whitespace and `.,()"';!?[]{}`. Filters `len < 2`, normalizes via `normalize_keyword`, drops stopwords.

**`ident_words`** (lines 449–482): camelCase → snake_case via `heck::AsSnakeCase`. Strips common method prefixes (`get_`, `set_`, `is_`, `as_`, `to_`, `try_`, `into_`, `from_`, `with_`, `new_`) and suffixes (`_ref`, `_mut`, `_iter`, `_t`, `_str`). Splits on `_`, normalizes, filters ident stopwords.

**`combine_weights`** (lines 491–495): Multi-source agreement bonus — `hi + lo * 0.3`.

**`readme_relevant_text`** (lines 499–529): Parses README line-by-line looking for `#` headings. Emits `(text, weight)` for each section, skipping `SKIP_SECTIONS`. No weight for preamble = 0.3; `section_weight` function dispatches to 0.45 (overview/about/summary/description/introduction), 0.35 (examples/usage/features), 0.25 (getting started/quick start/documentation/how it works), 0.3 (default).

**`compute_quality`** (lines 613–637): Score components:
- `description_len` (max 30): fraction of 300 chars.
- `repository` (10 pts), `documentation` (20 pts), `license` (10 pts), `keywords` (7 pts), `categories` (5 pts).
- README richness group (max 5): text_len/3000 (75 pts), code_blocks*5 capped at 25, has_code (30 pts), sections*4 capped at 30.
- `non_trivial` (2 pts if `loc > 700`), `non_giant` (1 pt if `loc < 80,000`), `loc` fraction of 10,000 (max 3).

**`extract`** (lines 685–792): Seven extraction passes with weights:
1. Manifest keywords — weight 1.0 (line 705–709).
2. Manifest categories — weight 0.7 (lines 712–717).
3. Crate name parts — weight 0.6 (lines 722–730); skips `["rs", "impl", "internal", "shared"]`.
4. Description words — weight 0.6 (lines 733–737).
5. README words — weight `0.3 * section_weight` (lines 740–745).
6. Source identifiers — weight 0.25 (lines 749–752).
7. Dependencies as `dep:name` — weight 0.2 invisible (lines 756–759).

After all passes: `apply_synonyms_and_specifics` remaps/down-weights, `dep:` prefixed keys are removed from visible output (line 766), remaining stopwords dropped. Sorted descending by weight, truncated to 20, top-normalized to 1.0. Categories from manifest (up to 3) or inferred from `KNOWN_CATEGORIES` keyword overlap (lines 794–817).

**Tests** (lines 825–861): `score_basic` (full score = 1.0), `score_partial` (5/20 = 0.25), `quality_ppm_bounded` (asserts ≤ 1,000,000 on a well-formed input).

---

## Key Observations / Gaps

1. **No GC**: None of the audited files implements garbage collection of `cas/` objects. No reference-counting, sweep, or eviction logic is visible.

2. **No MemoryCas / DiskCas / RegistryCas / object_store**: These abstractions are entirely absent from the nine files. They appear to live in `crate::store::Store` (referenced at `blob/emit.rs:51`, `blob/emit.rs:60`) and presumably `crate::coordination::Outbox`, neither of which is in this audit set.

3. **First-write-wins** is implicit in `store.put_section` returning `bool` (`true` = written, `false` = already present) — the semantics are enforced inside `Store`, not in the blob layer.

4. **Blob naming**: Sections are stored at `cas/{hash}` (hex BLAKE3 digest); the manifest pointer lives at `ptr/{package-uuid}`. This naming scheme is documented in comments but the actual `object_store` path construction is in `store::Store`.

5. **Streaming budget**: The `+1` read clamp in `extract_into_builder` (line 156) is a minor subtlety — it reads up to `max_file_bytes + 1` bytes to detect oversized files, but the `budget.charge` then catches the actual byte count against the limit. The pre-check on the declared header size (line 143) prevents materializing the extra byte in practice.

6. **`identity_bytes` vs `manifest_cas_key` pinning**: Tests asserting golden values for both hashes are mentioned (`tests/blob_hash_pins.rs`, line 105) but not in this audit set.


# Nested agent agent-ab28289d0fb87930d
Description: Audit registry runtime modules (text/vector/graph/session)
Size: 393218

Here is the exhaustive audit of `/Users/philocalyst/Projects/Backend/workspace/registry/runtime`:

---

## mod.rs (lines 1–35)

The top-level module is purely organizational. It declares the four runtime areas as pub sub-modules (`error`, `graph`, `session`, `text`, `vector`) and re-exports `RuntimeError`. The module doc (lines 1–26) articulates the architecture:

- `text` (tantivy) is the default search surface, replica-local, postgres-watermarked
- `vector` (qdrant) is gated behind `SemanticGate`, heavy/semantic-only
- `graph` (terminus) is source of truth for symbol structure and relations
- `session` is per-user monotone join-semilattice exploration state

Cross-cutting rules stated: Cold/Live typestates gate query methods; freshness is a `ContentHash`, not a row-level timestamp; every query carries an `AccessContext` for auth; one `thiserror` enum per area, all `Retryable`, all aggregated under `RuntimeError`.

---

## 1. Text Search (Tantivy)

### text/mod.rs (lines 1–30)

Re-exports: `TextIndex`, `TextSchema`, `Poller`, `Watermark`, `TextQuery`. Defines `TextCursorKey = (heart::Score, heart::SymbolId)` (line 29).

Architecture note (lines 6–16): tantivy is replica-local, single-writer; tantivy indexing and search are dispatched to `spawn_blocking` at each boundary.

### text/index.rs — Schema

**`TextSchema`** (lines 38–95): 11 fields, all handled as stored `STRING` or identifier-tokenized `TEXT`:

| Field | Type | Purpose |
|---|---|---|
| `id` | `STRING | STORED` | UUID upsert/delete key |
| `package` | `STRING | STORED` | Package UUID, stored |
| `ecosystem` | `STRING | STORED` | Lowercase language token, filterable |
| `kind` | `STRING | STORED` | `SymbolKind` PascalCase, filterable |
| `name` | `STRING | STORED` | Plain name, original case |
| `fq_name` | `STRING | STORED` | Fully-qualified name, original case |
| `name_lower` | `STRING` (not stored) | Lowercased plain name — exact/contains match surface |
| `fq_lower` | `STRING` (not stored) | Lowercased FQ name — path-contains match surface |
| `name_tokens` | tokenized `TEXT` | Identifier-tokenized plain name — subtoken match |
| `fq_tokens` | tokenized `TEXT` | Identifier-tokenized FQ name — path-subtoken match |

`ident_options()` (lines 88–94): sets tokenizer to `IDENT_TOKENIZER`, index option `WithFreqsAndPositions` (phrase queries supported), not stored.

**`TextIndex`** (lines 116–229):
- `writer: Mutex<IndexWriter>` — single-writer serialized behind a mutex (line 119)
- `reader: IndexReader` — `ReloadPolicy::Manual`, reloaded explicitly at each search (lines 143–145)
- Writer heap budget: `WRITER_MEMORY_BYTES = 50_000_000` (line 23)
- `open_or_create` (lines 128–147): opens MmapDirectory, opens/creates schema, registers the `ident` tokenizer, creates writer with 50MB budget, creates reader with manual reload
- Upsert strategy (lines 178–202): delete-by-term (`id` field) then add — never a growing log. `upsert_with` does a delete + add per symbol. `upsert_batch` (lines 206–214) locks writer once, loops `upsert_with` for all symbols, then commits once. 
- `remove` (lines 218–221): delete-by-id term, uncommitted
- `commit` (lines 225–228): flushes pending writes
- `snapshot` / `snapshot_hash` (lines 155–158, 234–242): hashes segment ids + `num_docs` + `num_deleted_docs` — the anchor a `Cursor` is minted against; any commit changing visible docs changes this hash
- `symbol_from_document` (lines 245–282): reconstructs `Symbol` from stored fields using typed field extraction

### text/poll.rs — Watermark and Poll Mechanism

**`Watermark`** (lines 27–35):
- `sequence: u64` — last postgres change sequence pulled into the index
- `BOTTOM: Watermark { sequence: 0 }` — nothing consumed yet (line 34)
- Monotone: only ever advances; idempotent on replay (re-upserts by id are no-ops)

**Constants** (lines 38–54):
- `BATCH_LIMIT: i64 = 64` — max outbox intents per poll
- `MAX_BACKOFF: Duration = 60s` — backoff ceiling on transient poll errors
- `WATERMARK_FILE = "watermark.json"` — sidecar next to tantivy directory

**SQL queries** (lines 48–55):
- `PENDING_SQL`: `SELECT seq, package_id FROM outbox WHERE sink_kind = 'text' AND seq > $1 ORDER BY seq ASC LIMIT $2` — reads change intents from the outbox table in sequence order
- `SYMBOLS_SQL`: `SELECT s.id, s.package_id, s.fq_name, s.kind, p.language FROM symbols s JOIN packages p ON p.id = s.package_id WHERE s.package_id = ANY($1)` — fetches full symbol rows for changed packages

**`Poller`** (lines 60–176):
- Fields: `pool: sqlx::PgPool`, `watermark_path: PathBuf`, `interval: Duration`
- `watermark()` (lines 85–91): reads `watermark.json`, returns `BOTTOM` if not found
- `record()` (lines 95–101): write-then-rename for crash safety; serializes watermark as JSON
- `poll_once()` (lines 110–154): 
  1. Load current watermark
  2. Query `PENDING_SQL` with current sequence and `BATCH_LIMIT = 64`
  3. Collect package ids and advance the sequence counter
  4. Batch-fetch symbols via `SYMBOLS_SQL`
  5. `blocking(|| index.upsert_batch(&symbols))` — spawns on blocking thread pool (or inline on single-thread runtimes)
  6. Persist new watermark
- `run()` (lines 158–175): loop with exponential backoff (doubling, capped at `MAX_BACKOFF`); on retryable errors backs off, on non-retryable returns error
- `blocking()` helper (lines 181–188): `block_in_place` on multi-thread runtime, inline otherwise

**Row decode** (lines 193–222): `symbol_from_row` extracts id, package_id, fq_name, kind, language from the joined postgres row. `plain_name` (lines 217–222) extracts leaf segment by splitting on `['/', '.', ':']`.

### text/query.rs — Query Structure and Search

**`TextQuery`** (lines 32–42): `terms: String`, `ecosystem: Option<Language>`, `kinds: Vec<SymbolKind>`.

**`MAX_FETCH: usize = 10_000`** (line 29) — deepest keyset resume depth for text search.

**`build_query`** (lines 231–318): lowers a `TextQuery` into a tantivy tree:
- 5-tier disjunction on name fields, all `Should`:
  1. Exact term on `name_lower` (line 250–253)
  2. Regex `.*needle.*` on `name_lower` (line 241–249)
  3. Regex `.*needle.*` on `fq_lower`
  4. Subtoken conjunction on `name_tokens` (each part `Must`) — if `subtokens()` non-empty
  5. Subtoken conjunction on `fq_tokens`
- Outer `Must` clauses: name disjunction (required), optional ecosystem filter, optional kind filter (as a `Should` disjunction under a `Must`)

**`search`** (lines 110–127): returns a stream; delegates to `search_async`. The `after` parameter requires `Cursor<TextCursorKey, Enforced>` — the `Enforced` brand means snapshot freshness is checked.

**`search_async`** (lines 131–209):
1. `spawn_blocking`: reload reader, compute `live_snapshot`, parse query into `Arc<dyn Query>`
2. Snapshot check (lines 160–163): if cursor snapshot != live snapshot, return `TextError::Cursor(CursorError::StaleSnapshot)`
3. Delegates to `pagination::keyset_page` with `MAX_FETCH = 10_000`
4. Each fetch iteration is a `spawn_blocking` call running `TopDocs::with_limit(fetch)` search

**`finite_score`** (lines 213–216): clamps non-finite BM25 scores to 0.0.

**`escape_regex`** (lines 322–334): escapes all regex metacharacters for safe contains-matching.

### text/tokenizer.rs — Identifier Tokenizer

**`IDENT_TOKENIZER = "ident"`** (line 44) — the registered analyzer name.

**`MAX_TOKEN_CHARS: usize = 64`** (line 48) — guards against pathological identifiers.

**`strip_arity_suffix`** (lines 61–70): strips CLR generic arity suffix (`` `N ``) before splitting, e.g. `` List`1 `` → `List`.

**`split_identifier`** (lines 80–120): byte-span splitter that breaks on:
- Non-alphanumeric characters (separator, dropped)
- `lower→Upper` camel hump
- Acronym tail: `UpperUpperLower` (e.g. `HTTP` in `HTTPServer`)
- Digit↔letter transitions (e.g. `utf8` → `utf`, `8`)

**`subtokens`** (lines 126–131): query-side counterpart — splits on whitespace then `split_identifier`, lowercases each span. Used in `build_query` to construct the subtoken conjunctions.

**`IdentifierTokenizer`** (lines 136–195): implements `tantivy::Tokenizer`. Walks whitespace-delimited words, emits only sub-words (not the whole identifier) via `split_identifier`. Each token has correct byte offsets and monotone positions.

**`register`** (lines 219–225): builds `TextAnalyzer` chain: `IdentifierTokenizer` → `RemoveLongFilter(64)` → `LowerCaser`, registered as `"ident"` on the index. Must be called after every open.

---

## 2. Vector Search (Qdrant)

### vector/mod.rs

**`SemanticCursorKey = (heart::Score, SymbolId)`** (line 49) — keyset key for semantic pagination. Cursor is `Advisory` (line 43–48): no cheap content hash for ANN indexes, so freshness is best-effort.

**`CollectionName`** (lines 77–102): validated string, non-empty, charset `[a-zA-Z0-9\-_.]`. Single global collection holds all symbol vectors; scoping (per-language, per-package) lives in per-point payload.

**`SymbolPoint<M>`** (lines 110–125): the wire/payload shape:
- `symbol: SymbolId`
- `embedding: Embedding<M>`
- `ecosystem: heart::Language` — for language-scoped filtering in payload
- `purpose: EmbeddingPurpose` — code vs documentation

**`Semantic<M, S>`** (lines 131–148): generic over model brand `M` and state `S` (Cold/Live).
- `client: Arc<Qdrant>` — shared gRPC client
- `collection: CollectionName`

**Connect** (lines 161–202):
1. `health_check()` — basic reachability
2. `collection_info()` — extracts configured dense-vector dimension via `configured_dimension()`
3. Asserts `found == M::DIMENSIONS`, else `ConnectFailure::DimensionMismatch`
4. Promotes to `Semantic<M, Live>`

**`search` / `search_with_filter`** (lines 204–306):
- Requires `SemanticGate` consumed by value — one gate authorizes one query
- `MAX_FETCH: usize = 4096` (line 251) — over-fetch ceiling for vector pagination
- Delegates to `pagination::keyset_page`; each fetch iteration calls `SearchPointsBuilder` with `with_payload(false)` — point id IS the symbol UUID (line 327–343), no payload round-trip needed
- Optional `qdrant_client::qdrant::Filter` for language scoping within single collection
- Returns `impl Stream<Item = Result<Scored<SymbolId>, VectorError>>`

**Payload schema** (lines 349–363, `point_struct`): each qdrant point has:
- Point id: `symbol.as_uuid().to_string()`
- Vector: `embedding.as_slice().to_vec()`
- Payload: `{"ecosystem": "<language_token>", "purpose": "code"|"documentation"}`

**`VectorSink<M>`** (lines 388–446): `tower::Service` impl for upsert. `MAX_BATCH = 256` (line 397). Batch service chunks input to 256 per qdrant call with `wait(true)` for immediate searchability. Single-point service wraps in `vec!`.

**`scored_symbol`** (lines 328–343): decodes qdrant `ScoredPoint` — uses UUID point id directly as `SymbolId`, no payload decode on the hot path.

### vector/cache.rs

**`EmbeddingKey`** (lines 29–40): `{model: ModelId, text_hash: ContentHash}` — keyed on model + BLAKE3 hash of embedded text. Package snapshot deliberately excluded, enabling cross-package deduplication.

**`EmbeddingCache<M>`** (lines 45–85):
- Backed by `moka::future::Cache<EmbeddingKey, Embedding<M>>`
- `new(capacity: u64)` — LRU/TinyLFU eviction
- `get_or_embed()` (lines 65–79): get-then-insert pattern (not moka's `try_get_with`) because `EmbedError` sources are non-`Clone`. Two concurrent misses may embed twice — harmless since embedding is deterministic and last-write-wins.
- `get()` (lines 82–84): best-effort peek without embedding

### vector/embedding.rs

**`EmbeddingPurpose`** (lines 30–35): `Code` or `Documentation` — same text under different purposes yields different vectors.

**`Embedding<M>`** (lines 40–84):
- Storage: `Arc<[f32]>` (not a fixed-size array, no `generic_const_exprs`)
- Construction boundary: `from_slice` and `from_vec` validate against `M::DIMENSIONS`; `EmbedError::LengthMismatch` on mismatch
- `NON_ZERO` const (lines 48–50): compile-time assertion that `M::DIMENSIONS > 0`
- `zeroed()` (lines 71–74): all-zero vector for readiness probes
- Serde: serializes as `[f32]`, deserializes via `from_vec` (length-validated)
- Manual `Clone`, `Debug`, `PartialEq` to avoid spurious `M: Clone/Debug/PartialEq` bounds

**`Embedder` trait** (lines 125–146):
- `type Model: EmbeddingModel`
- `embed(&str, EmbeddingPurpose) -> Result<Embedding<M>, EmbedError>` — single text
- `embed_batch(&[&str], EmbeddingPurpose) -> Result<Vec<Embedding<M>>, EmbedError>` — batch in one round-trip

### vector/gate.rs

**`SemanticGate`** (lines 31–63):
- `reason: &'static str` — audit/trace reason, never affects equality
- Non-`Copy`, non-`Clone`, `#[must_use]`
- `issue(reason: &'static str)` — planner issuance only
- `for_readiness()` — fixed reason `"readiness probe"`, sole non-planner site

### vector/similarity.rs

**`cosine<M>`** (lines 12–23): f64-accumulated dot product, normalizes by sqrt of L2 norms, clamps to `[-1.0, 1.0]`, maps zero vector to `0.0`. Always returns a finite `Score`.

### vector/model/mod.rs

**`EmbeddingModel` trait** (lines 19–26): sealed. `DIMENSIONS: usize`, `fn id() -> ModelId`. Sealed via private `sealed::Sealed` supertrait — only catalog types implement it.

### vector/model/catalog.rs

Two brands:
- `E5Small`: `DIMENSIONS = 384`, id = `"intfloat/e5-small-v2"`
- `OpenAi3Small`: `DIMENSIONS = 1536`, id = `"openai/text-embedding-3-small"`

### vector/model/id.rs

**`ModelId`**: `nutype`-derived non-empty string, trimmed on construction. Derives `Debug, Clone, PartialEq, Eq, Hash, Display, AsRef, Serialize, Deserialize`.

---

## 3. Graph Resolution (Terminus)

### graph/mod.rs

**`RelationKind`** (lines 36–64): `Member`, `Reference`, `Occurrence`, `Implements`, `Extends`, `ReExport`. Derives `strum::Display` + `strum::EnumString` for round-trip token encoding.

**`GraphStore` trait** (lines 72–97): `get_occurrences(item) -> Stream<Scored<SymbolId>>`, `get_references(item) -> Stream<Scored<SymbolId>>`, `are_related(from, to) -> Option<RelationKind>`.

**Connection types**: `Organization`, `Database`, `Credentials` (lines 127–196). Validation: ASCII alphanumerics + `_-` only, via `validate_terminus_name`. `Credentials` holds password as `SecretString`.

**`Graph<S>`** (lines 206–236): `client: reqwest::Client`, `endpoint: Url`, `organization`, `database`, `credentials`, state phantom.

**Connect** (lines 250–319):
1. `GET /api/info` with basic auth — reachability + credentials
2. `GET /api/db/{org}/{db}` — confirms org/db exist; `404 → ConnectFailure::SchemaMismatch`
3. Promotes to `Graph<Live>`

**Document model** (lines 322–333, comment):
- Class `Symbol`: `id`, `package`, `ecosystem`, `plain`, `fully_qualified`, `kind`
- Class `Relation`: `from`, `kind`, `to`

**WOQL builders** (lines 336–378): minimal DSL — `variable`, `string`, `triple`, `is_a`, `and`, `select`. All produce JSON-LD values.

**`insert_symbols`** (lines 442–494): POST to `/api/document/{org}/{db}` with `graph_type=instance`, `?full_replace=false`. Each doc has `@id = "Symbol/{uuid}"` so replay replaces in-place (idempotent).

**`bindings`** (lines 497–536): POST WOQL query to `/api/woql/{org}/{db}`, deserializes `{bindings: [...]}` response.

**`edge_endpoints`** (lines 539–562): shared query body for `get_occurrences`/`get_references`. Queries `Relation` class by anchor property, kind token, yields the other endpoint. All hits get `direct_score() = 1.0`.

**`outgoing_edges`** (lines 564–588): WOQL `Select([Kind, Target])` over `Relation` docs where `from = origin`. Used by expansion BFS.

**`get_occurrences`** (lines 596–605): finds *source* (`from`) of `Occurrence` edges pointing *at* `item` — i.e. which symbols' declarations/signatures contain `item`.

**`get_references`** (lines 607–617): finds *source* (`from`) of `Reference` edges pointing *at* `item` — callers/users.

**`are_related`** (lines 619–638): WOQL select on `Kind` where `from` and `to` are fixed; returns first binding or `None`.

### graph/expansion.rs

**`ExpansionBounds`** (lines 13–16): `depth: NonZeroUsize`, `breadth: NonZeroUsize`.

**`ExpandedEdge`** (lines 18–24): `target: Scored<SymbolId>`, `via: SymbolId` (the node that was expanded), `relation: RelationKind`, `depth: NonZeroUsize`.

**`expand_via`** (lines 34–65): pure async BFS, generic over the neighbor-fetch closure:
- Visited set: `BTreeSet` starting with `origin`
- Frontier initialized to `[origin]`
- For each depth level `1..=bounds.depth`:
  - Score = `1.0 / depth` (nearer neighbors rank higher)
  - For each node in frontier: call `neighbors(via)`, take up to `bounds.breadth` edges
  - A node reached twice emits distinct edges but is only expanded once (de-dup via `visited`)
  - If frontier empty after a level, breaks early

**`Graph<Live>::expand`** (lines 67–80): wraps `expand_via` with `outgoing_edges` as the neighbor function, streams results.

### graph/resolution.rs

**`Resolution`** (lines 11–17): `Stable(SymbolId)`, `Moved { previous, next: Scored<SymbolId> }`, `Removed(SymbolId)`, `Added(SymbolId)`.

**`diff(previous, next)`** (lines 29–81): pure symbol diff, no graph I/O:
1. Exact FQ-path match → `Stable` (uses *next* version's id going forward)
2. Same plain name + same kind, best path similarity (Jaccard) → `Moved`
3. No match → `Removed`
4. Unclaimed next-version symbols → `Added`

**`path_similarity`** (lines 85–98): segment-set Jaccard over splits on `['/', '.', ':']`.

### graph/structure.rs

**`StructureNode`** (lines 11–17): `symbol: Symbol`, `parent: Option<SymbolId>`, `relation: Option<RelationKind>`, `kind: SymbolKind`.

**`assemble`** (lines 23–38): joins a flat list of `Symbol`s with a `BTreeMap<SymbolId, (RelationKind, SymbolId)>` of parent-edges into `StructureNode`s. The `kind` is sourced from the graph's symbol record (authoritative), not from stale copies in other stores.

---

## 4. Session Store

### session.rs

**`Edge`** (lines 28–35): `from: SymbolId`, `kind: RelationKind`, `to: SymbolId`. Derives `Ord` for `BTreeSet` membership.

**`SessionId = Id<SessionGraph>`** (line 38).

**`SessionGraph`** (lines 43–73):
- `nodes: BTreeSet<SymbolId>`, `edges: BTreeSet<Edge>`
- `merge(&mut self, other)`: union of both sets — idempotent, commutative, associative (join-semilattice laws documented at lines 63–68)
- Serialization: `serde::Serialize + Deserialize` — both sets are serialized in full

**`Persistence`** (lines 76–83): `Ephemeral` (no-op persist/load) or `Directory(PathBuf)` (one JSON file per session, named `{uuid}.json`).

**`SessionStore` trait** (lines 95–122): `open`, `merge_into`, `snapshot`, `clear`, `persist`, `load`. All async.

**`MemorySessionStore`** (lines 127–242):
- `sessions: RwLock<HashMap<SessionId, Arc<SessionGraph>>>` — copy-on-write Arc
- `open`: checks in-memory map first; on miss reads disk snapshot or starts empty; race-safe: uses `entry().or_insert()` so concurrent opener's graph wins
- `merge_into`: calls `open` first (folds persisted snapshot before delta); locks write guard; clones current graph, merges delta, wraps in new `Arc`, swaps — held snapshots see old Arc
- `snapshot`: read-only clone of existing Arc (cheap reference count bump)
- `clear`: inserts empty graph in map, removes file (tolerates NotFound)
- `persist`: JSON-encodes graph, write-then-rename to `{uuid}.json` (crash-safe)
- `load`: reads disk snapshot, returns None if absent

**`PgSessionStore`** (lines 268–397):
- `pool: sqlx::PgPool`
- Table schema (lines 279–287): `sessions(id uuid PK, graph jsonb NOT NULL, updated_at timestamptz NOT NULL DEFAULT now())`
- `migrate()`: `CREATE TABLE IF NOT EXISTS sessions (...)` — idempotent, call on boot
- `open`: `INSERT ... ON CONFLICT DO NOTHING` then `SELECT` — upsert-to-empty ensures row exists, read-back is authoritative
- `merge_into` (lines 327–365): full serialized transaction — `BEGIN`, `INSERT ON CONFLICT DO NOTHING`, `SELECT ... FOR UPDATE` (row lock), decode JSON graph, `merge(delta)`, `UPDATE SET graph = ...`, `COMMIT`. Row lock serializes concurrent replicas; semilattice union makes order irrelevant.
- `clear`: `INSERT ... ON CONFLICT DO UPDATE SET graph = empty` — keeps row alive
- `persist`: no-op (every mutation already commits to postgres)
- `load`: `SELECT graph` + JSON decode

---

## 5. Pagination

### pagination.rs

**`keyset_page`** (lines 33–73): single generic function shared by both text and vector backends:
- Parameters: `after: Option<(Score, SymbolId)>`, `target: usize` (page size), `max_fetch: usize` (ceiling), `id_of: Fn(&T) -> SymbolId`, `fetch_ranked: FnMut(usize) -> Fut` (returns `(hits, exhausted)`)
- Initial fetch size: `target` on first page, `target * 2` capped at `max_fetch` on resume (line 44–47)
- Sort: `score DESC, id ASC` on every loop iteration (lines 51–55) — re-sorts hits since backends don't guarantee stable total order
- Filter (lines 57–65): `after.is_none_or(|(score, id)| hit.score < score || (hit.score == score && id_of(hit) > id))` — strict keyset predicate
- If not exhausted and can fetch more, doubles `fetch` and loops; otherwise returns
- Max over-fetch depths: text uses `10_000`, vector uses `4_096`

---

## 6. Error Module

### error.rs

**`GraphQueryError`** (lines 26–74): `HttpStatus`, `ParseFailed`, `UnsupportedFeature`, `NotFound`, `ConstraintViolation`, `Unauthorized`, `Timeout`, `UnknownBinding`, `ResultShapeMismatch`, `Other`.

**`TextQueryError`** (lines 77–114): `Empty`, `TooLong`, `ContainsControl`, `InvalidPattern {needle, cause: tantivy::TantivyError}`, `InvalidEcosystem`, `InvalidKindFilter`, `SyntaxError`, `Malformed`.

**`EmbedRejectionReason`** (lines 117–166): `Serialization`, `HttpStatus`, `MalformedResponse`, `BatchSizeMismatch`, `EmptyResponse`, `InputRejected`, `DimensionProblem`, `AuthFailed`, `ProviderError`, `QuotaExceeded`, `UnsupportedModel`, `Other`.

**`GraphError`** (lines 169–208): `Connect`, `Transport(reqwest::Error)`, `Query(GraphQueryError)`, `Decode(serde_json::Error)`, `NotFound`. Retryable: Transport on timeout/connect, Connect delegates.

**`VectorError`** (lines 211–252): `Connect`, `Collection(CollectionNameError)`, `Transport(qdrant_client::QdrantError)`, `Embed(EmbedError)`, `Payload(serde_json::Error)`. Transport always retryable.

**`RowDecodeError`** (lines 255–264): `UnknownEcosystem`, `UnknownSymbolKind`.

**`TextError`** (lines 267–310): `Io`, `Engine(tantivy::TantivyError)`, `Poll(sqlx::Error)`, `Query(TextQueryError)`, `Cursor(CursorError)`, `Codec(serde_json::Error)`, `Row(RowDecodeError)`. Retryable: `Io` and `Poll` only.

**`SessionError`** (lines 313–343): `Io`, `Codec`, `Database(sqlx::Error)`, `NotFound`. Retryable: `Io` and specific sqlx variants (`Io`, `PoolTimedOut`, `WorkerCrashed`).

**`EmbedError`** (lines 349–396): `Transport`, `Rejected(EmbedRejectionReason)`, `RateLimited { retry_after: Option<Duration> }`, `DimensionMismatch { expected, found }`, `LengthMismatch { expected, found }`. Retryable: Transport and RateLimited; `retry_after()` propagates the hint from RateLimited.

**`RuntimeError`** (lines 401–483): aggregates all five area enums via `#[from]`. Implements `Retryable` by delegating to the inner variant. Also provides `From<GraphQueryError>`, `From<TextQueryError>`, `From<EmbedRejectionReason>` conversions for ergonomic `?` usage without wrapping boilerplate.


# Nested agent agent-a4999d89c16e5ff38
Description: Audit registry search modules
Size: 163391

Here is a complete, exhaustive audit of all four files.

---

## `/Users/philocalyst/Projects/Backend/workspace/registry/search/mod.rs`

### Module Overview (lines 1–23)

The module-level doc explains the architecture: Postgres is the source of truth, and a replica-local Tantivy index is a query abstraction synced from Postgres via a watermark. Results are `heart::Scored<GlobalPackage>`, keyset-paginated via `heart::Cursor`, and always access-filtered. The comment at lines 10–22 explains the core pagination invariant: the five-stage ranking pipeline is run *once* over the full over-fetched candidate set; each item is stamped with a strictly-descending **rank score** derived from its final ordinal. Page 1 and page N are slices of one total order.

### Constants and Types

- **`NEUTRAL_QUALITY: f32 = 0.5`** (line 35): Used for packages with no extracted facets. Chosen as a midpoint so the fusion multiplier neither erases (`0.0`) nor inflates unranked packages relative to their BM25 relevance.
- **`SearchKey = (heart::Score, PackageId)`** (line 47): The keyset type — a rank score paired with package id as tiebreak. The score is *not* raw BM25; it is a strictly-descending ordinal-derived value.

### `RegistryQuery` struct (lines 50–63)

Fields:
- `text: String` — free-text query
- `ecosystem: Option<Language>` — optional ecosystem filter (searches all when `None`)
- `limit: usize` — page size
- `after: Option<Cursor<SearchKey>>` — keyset cursor; `None` for first page

### `search_page` (lines 76–106)

Public entry point. Calls `collect_ranked_hits`, computes a `snapshot` hash from the index watermark's position bytes (line 82), then:
- Extracts the cursor's `after` key (lines 85–90); logs a debug message if the cursor's snapshot is older than the current one (but still serves from the newer index — no error)
- Filters hits strictly: `hit.score < score || (hit.score == score && hit.value.id > id)` (lines 91–94) — this is `(score desc, id asc)` resume semantics
- Fetches `limit + 1` items to detect `has_more` (lines 97–100)
- Encodes the last item as the next cursor (lines 101–104)
- Returns `Page { items, next }` (line 105)

### `collect_ranked_hits` (lines 117–189)

The core pipeline:
1. Over-fetches with `limit * 4 + 32` candidates (line 124)
2. Calls `index.query(&query.text, over_fetch)` to get raw `(PackageId, f32)` BM25 scores (line 125)
3. Calls `index.hydrate(&ids)` to get full `GlobalPackage` records (lines 129–142) — no Postgres round-trip
4. Ecosystem-filters hydrated records (lines 133–136)
5. Wraps each in `Scored<GlobalPackage>` using the raw BM25 score clamped via `finite_score` (lines 138–142)
6. Calls `multi_parent::merge(scored)` to de-duplicate (lines 145–148), then extracts representatives
7. Builds `ranking::Candidate<GlobalPackage>` items (lines 151–166), extracting `bm25`, `quality`, `keywords` from `package.facets`; falls back to `NEUTRAL_QUALITY` + empty keywords for packages without facets (lines 158–162). `downloads` is hardcoded to `0` here (line 164) — **a known gap**
8. Calls `ranking::rank_full(&query.text, candidates, limit)` (line 174) — runs the full pipeline without truncating
9. Stamps each ranked item with `rank_score(ordinal, total)` (lines 179–186): ordinal 0 gets the highest score, each later item gets exactly one less. Values stay within f32's exact-integer range for any realistic candidate set

### `rank_score` (lines 200–202)

`(total - ordinal) as f32`, clamped through `finite_score`. Strictly descending; every item has a distinct score.

### `finite_score` (lines 206–209)

Clamps raw f32 to `heart::Score` (which requires finiteness); falls back to `0.0` if the value is NaN or infinite.

---

## `/Users/philocalyst/Projects/Backend/workspace/registry/search/multi_parent.rs`

### Module Overview (lines 1–12)

Handles the case where the same logical package is reachable via multiple registry origins (federated mirrors, vendored forks, etc.), which would produce duplicate `GlobalPackage` records. The module collapses these into a single `Merged` view.

### `Merged` struct (lines 20–24)

Has a single public field: `representative: Scored<GlobalPackage>` — the highest-scored record for that logical package.

### `merge` function (lines 32–66)

- Key function (lines 37–44): `(ecosystem, canonical_name, canonical_version)` — intentionally origin-independent, so federated copies of the same package collapse together regardless of their `PackageId`
- Uses a `HashMap<key, usize>` as an index into a `Vec<Merged>` (lines 46–47)
- For each `Scored<GlobalPackage>` in the input:
  - `Vacant` slot: insert new `Merged` and record its index (lines 51–53)
  - `Occupied` slot: compare scores; if the new entry is strictly better, dethrone the representative; ties keep the earlier arrival (lines 55–61) — this ensures stable pagination across equal-scored duplicates
- Returns `Vec<Merged>` in input-arrival order

---

## `/Users/philocalyst/Projects/Backend/workspace/registry/search/ranking.rs`

### Module Overview (lines 1–16)

Ports the five-stage score-fusion + diversity pipeline from `search_index/src/lib_search_index.rs` in the lib.rs monorepo. Pure and deterministic: no RNG, no I/O, no timestamps. Ties broken by name.

### `Candidate<T>` struct (lines 25–41)

Generic over an opaque payload `T`. Fields:
- `item: T` — caller's record; ranking never inspects it
- `name: String` — for exact/contains bonus and tiebreaking
- `bm25: f32` — raw BM25 relevance from Tantivy
- `quality: f32` — quality signal in `0.0..=1.0`
- `downloads: u64` — monthly downloads (used by bubble sort and representative pull-up)
- `keywords: Vec<SmolStr>` — normalized keywords for diversity pass

### `RankingConfig` struct (lines 44–108) with `Default` impl (lines 111–132)

All constants mirror lib.rs defaults:
- `quality_kink_threshold: 0.4` — items above get `quality + 1.0`, items below get raw `quality`
- `exact_name_bonus: 10.0` — flat bonus for exact name match
- `contains_cap_fraction: 0.75` — cap for contains-name bonus
- `contains_max_boosted: 5` — max candidates eligible for contains bonus
- `dividing_min_pop: 2` — minimum keyword population to qualify as dividing keyword
- `dividing_too_common_fraction: 5/8` — fraction above which a keyword is "too common"
- `dividing_good_pop_fraction: 1/3` — fraction below which a keyword gets ×2 weight
- `dividing_good_pop_min: 10` — absolute floor for ×2 weight boost
- `dividing_min_set_size: 25` — minimum result set for diversity pass to activate
- `representative_quality_fraction: 0.97`, `representative_quality_floor: 0.55`
- `representative_downloads_fraction: 0.9`, `representative_downloads_floor: 100_000`
- `bubble_downloads_min: 200`, `bubble_downloads_max: 1_000_000`, `bubble_ratio: 3`

### Entry Points (lines 134–188)

- **`rank`** (line 141): calls `rank_with` with `RankingConfig::default()`. Passes `retain == limit` → truncates to one page.
- **`rank_with`** (lines 146–155): explicit config; calls `rank_pipeline(cfg, query, candidates, limit, limit)`.
- **`rank_full`** (line 174): calls `rank_full_with` with default config; passes `retain == usize::MAX` — keeps the whole order.
- **`rank_full_with`** (lines 179–188): calls `rank_pipeline(cfg, query, candidates, limit, usize::MAX)`.

The distinction between `rank` and `rank_full` is solely in the `retain` argument to the shared pipeline: `rank` truncates for single-page use, `rank_full` retains the entire ordered list for keyset pagination.

### `rank_pipeline` (lines 195–247)

The shared five-stage implementation:

**Stage (a) — fuse_scores** (line 207): returns parallel `Vec<f32>` of fused scores

**Stage (b) — sort** (lines 210–228): attaches scores as parallel vec, sorts `(fused_score desc, name asc)`, rebuilds candidates in sorted order using a `Vec<Option<Candidate<T>>>` drain pattern

**Stage (c) — diversity_pass** (line 231): operates on `&mut sorted` and `&mut scores` in parallel

**Stage (d) — pull_up_representatives** (line 238): the `retain` argument is threaded here; `limit` tunes eligibility

**Stage (e) — downloads_bubble** (lines 241–245): runs on `sorted[2..]` and then `sorted[5..]` (two separate passes on progressively longer tails), but only if `sorted.len() > 5`

### `fuse_scores` (lines 262–313)

Pre-computes `top4_score` (line 266): the 4th-highest BM25 score among all candidates (falls back to 1st or 0.0). Detects specificity: `query.contains(['-', '_']) || query.len() > 15` (line 270).

For each candidate:
- **Quality kink**: `q = quality + 1.0` if `quality > 0.4`, else `q = quality`; `score = bm25 * q`
- **Exact match**: if `name_lower == query_lower`, adds `exact_name_bonus` (10.0) — additive, not replacing
- **Contains match** (lines 292–308): if `boosted < 5` and `contains_query_names(name, query)`:
  - Computes `quality_bonus = (quality^2 + 0.25) * min(2.0, 1.1)`
  - Specificity factor: 0.9 if specific query, 0.15 otherwise
  - `bonus_factor = 1.0 + quality_bonus * specificity * 2.0 / (2.0 + boosted)`
  - `boosted_score = score * bonus_factor`
  - `cap = top4_score * 3.0 + boosted_score`; `capped = (cap/4.0).max(score * (1 + (bonus_factor-1)*0.1))`
  - Final: uses `capped` if `boosted_score > top4_score`, otherwise `boosted_score`
  - Increments `boosted`

### `contains_query_names` (lines 318–331)

Strips `cargo`/`rust` prefix and `rs` suffix from `name`, then does substring containment (longer.contains(shorter)).

### `diversity_pass` (lines 349–434)

Returns early if `n < dividing_min_set_size` (25). Builds `kw_keys` — sorted, NUL-delimited keyword strings per candidate (lines 358–364). Counts keyword populations in a `HashMap<&str, u32>` (lines 368–383): items in the top half (index < n/2) contribute weight 2, others weight 1; duplicate keyword-sets are skipped via a `seen_sets` HashSet. Selects the "best dividing keyword" (lines 389–402): max by effective population, where the `[good_pop_min, N/3]` band gets ×2 effective weight. Demotes carriers of the dividing keyword starting at position 3 (line 408 skips positions 0–2): factor = `(0.9 - carrier_count/10.0).max(0.33)` (lines 416–418). Re-sorts via index sort + `apply_permutation` (lines 423–433).

### `pull_up_representatives` (lines 456–548)

Returns early (with truncation) if `n < 7` or `better_half <= 5`. Computes:
- `better_half = min(n/3, 50)` (line 468)
- `take = clamp(limit/70, 1, 3)` (line 474)
- `max_downloads` and `max_quality` over `candidates[..better_half]`
- `high_rank = max(max_quality * 0.97, 0.55)` (line 482)
- `high_dl = max(max_downloads * 0.9, 100_000)` (line 484)

Quality-based pull-up (lines 491–498): scans `candidates[3..better_half]`, pulls up to `take` items with `quality >= high_rank`. Downloads-based pull-up (lines 501–515): pulls up to `take - got` additional items where `quality >= (0.32 if downloads >= 1M else 0.55)` and `downloads >= high_dl`. Always re-includes top-3 in the pulled set (line 519). Deduplicates and sorts pull indices, then removes in reverse index order (to avoid drift) and reverses to restore relative order (lines 523–529). Sorts pulled crates by mixed score: `(0.3 + quality) * quality * log2(downloads + 25_000)` (lines 533–541). Truncates remaining tail to `retain - top_crates.len()`, prepends pulled crates (lines 544–547).

### `downloads_bubble` (lines 559–568)

Iterates `chunks_exact_mut(2)` — non-overlapping pairs only. Swaps if: `b.downloads > 200 && b.downloads < 1_000_000 && a.downloads * 3 < b.downloads`.

### `apply_permutation` (lines 576–604)

Cycle-follower in-place permutation applied to two parallel slices (`items` and `scores`). O(n) time, O(n) mark space. Uses a `done` bitvec and follows cycles by swapping.

### Tests (lines 609–756)

- `fusion_kink_high_quality_wins` (line 627)
- `fusion_kink_boundary` (line 639)
- `exact_name_bonus_floats_exact_match` (line 652)
- `representative_pullup_popular_item` (line 667): 20 mediocre items + one popular at index 15; asserts it lands in top 5
- `downloads_bubble_swaps_adjacent_pair` (line 684)
- `downloads_bubble_no_swap_when_large_exceeds_cap` (line 698)
- `rank_is_deterministic` (line 712)
- `truncation_respects_limit` (line 728)
- `empty_input_returns_empty` (line 735)
- `limit_zero_returns_empty` (line 741)
- `limit_larger_than_candidates_returns_all` (line 748)

---

## `/Users/philocalyst/Projects/Backend/workspace/registry/search/tantivy.rs`

### Module Overview (lines 1–5)

Tantivy is used *not* as a source of truth but for its search capabilities. The index is a disposable, rebuildable replica of a Postgres slice. Kept in sync by polling Postgres from a watermark.

### `SyncWatermark` struct (lines 18–24)

Holds `position: i64` — a Postgres logical sequence (e.g., `updated_at` cursor or txid). Serializable for durable persistence.

- **`WATERMARK_FILE: &str = "sync_watermark.json"`** (line 27): persisted next to the index directory root.

### `PackageIndex` struct (lines 30–36)

Fields:
- `index: Index` — the Tantivy index
- `reader: IndexReader` — searcher handle
- `watermark: SyncWatermark` — in-memory current position
- `dir: PathBuf` — root directory (for watermark file I/O)

### `Fields` struct (lines 40–47)

Resolved schema fields: `package_id`, `name`, `description`, `keywords`, `ecosystem`, `record`.

### Constants (lines 51–54)

- `WRITER_HEAP_BYTES: usize = 50 << 20` — 50 MB heap for Tantivy writer per sync
- `SYNC_BATCH: u64 = 1024` — rows per sync poll

### `PackageIndex::open` (lines 62–95)

Creates the directory if needed, opens or creates the Tantivy index with `MmapDirectory`, opens a reader, and loads the watermark from disk. **Orphan watermark guard** (lines 72–81): if `position > 0` but `num_docs() == 0`, resets watermark to 0 to avoid skipping history on resume.

### `PackageIndex::schema` (lines 99–114)

Defines the Tantivy schema:
- `package_id`: `STRING | STORED` — raw-indexed for exact term lookups
- `name`: `TEXT` — searchable
- `description`: `TEXT` — searchable (currently unused in practice; declared for future use)
- `keywords`: `TEXT` — searchable
- `ecosystem`: `STRING | STORED` — exact term
- `record`: `STORED` only (never indexed) — full serialized `GlobalPackage` JSON, avoiding Postgres round-trips on hydration

### `PackageIndex::fields` (lines 118–129)

Resolves all six fields by name from the live schema.

### `PackageIndex::absorb` (lines 136–173)

Upsert loop: for each `GlobalPackage` record, deletes by `package_id` term then re-adds. The document includes:
- Both canonical and original name (lines 154–155)
- Ecosystem token (line 156)
- Keywords from `facets.keyword_text()` if facets exist (lines 159–162)
- Full JSON serialized `GlobalPackage` in `record` field (line 163)

Commits the writer and reloads the reader (lines 167–168), advances the watermark in memory and persists it to disk (lines 169–170).

### `PackageIndex::sync_from` (lines 179–194)

Converts `watermark.position` (microseconds) to a `chrono::DateTime` (line 180), calls `queries::search::changed_since(after, SYNC_BATCH)` to fetch up to 1024 changed rows, iterates to extract records and compute the new `head` watermark as `max` of all `updated_micros` values (lines 187–192), then calls `self.absorb`.

### `PackageIndex::query` (lines 199–229)

- Parses text via `QueryParser::for_index` over `[name, description, keywords]` fields (lines 207–213)
- Collects `TopDocs::with_limit(limit.max(1))` (lines 215–217)
- Extracts `package_id` from the stored field, parses it as UUID, maps to `PackageId` (lines 219–228)
- Returns `Vec<(PackageId, f32)>` of raw BM25 scores

### `PackageIndex::hydrate` (lines 235–256)

For each `PackageId`, issues a `TermQuery` on `package_id`, takes the top-1 hit, reads the `record` field, and deserializes `GlobalPackage`. IDs not yet in the replica are silently skipped with a debug log (lines 246–248) — eventual consistency by design.

### `PackageIndex::watermark` (line 259)

Accessor returning `self.watermark`.

### `load_watermark` (lines 267–291)

Reads `sync_watermark.json`; on `NotFound`, silently returns `{ position: 0 }`. On corrupt JSON or other I/O errors, logs a warning and returns 0.

### `persist_watermark` (lines 294–307)

Atomic write: serializes to a `.json.tmp` file and renames to the final path. Best-effort (warnings on failure, no panic).

### `stored_text` (lines 310–320)

Extracts a `Str` value from a stored field; errors if the field is missing or wrong type (invariant: this module wrote it, so it can never happen in practice).

### `codec_to_search` (lines 327–329)

Converts `codec::CodecError` to `SearchError::Codec`.

### `row_to_record` (lines 336–382)

Reassembles a `GlobalPackage` from a `changed_since` query row. Column mapping:
- 0: `id: uuid::Uuid`
- 1: `language: String`
- 2: `origin_token: String`
- 3: skipped (name_canonical — unused directly, derived via `codec`)
- 4: `name_original: String`
- 5: `version_canonical: String`
- 6: `toolchain_json: serde_json::Value`
- 7: `updated_at: DateTime<Utc>`
- 8–12: lifecycle columns (`state`, `phase`, `content_hash`, `needed`, `failure`) — left-joined, may be `None`
- 13: `facets_json: Option<serde_json::Value>` — nullable JSONB facets column

When `state` is `None` (no lifecycle row), the package gets `ResolutionState::Unindexed { needed: false }` (line 361). Facets are decoded via `codec::facets_from_json`. The `facets` field on `GlobalPackage` is `None` when metadata was never extracted for this generation (line 379 comment).

---

## Key Observations and Gaps

1. **`downloads` is hardcoded to `0`** in `collect_ranked_hits` (mod.rs line 164). The `Candidate` struct supports it and the ranking pipeline uses it, but `GlobalPackage` does not appear to carry a download count yet — the bubble sort and representative pull-up download logic is wired but effectively a no-op today.

2. **`description` field is declared but never populated** in `absorb` (tantivy.rs lines 104–105, 107): the schema declares it and the query parser searches it, but no code calls `document.add_text(fields.description, ...)`.

3. **No description field in `GlobalPackage`**: The hydration path stores only `name`, `keywords`, and `ecosystem` from `GlobalPackage` (plus the raw JSON blob). Description search is declared but placeholder.

4. **`query_parse` can fail on special Tantivy query syntax** in user input (tantivy.rs line 212): parse errors surface as `SearchError::TantivyQueryParse` — no sanitization or fallback to a literal query.

5. **Cursor snapshot mismatch is advisory only** (mod.rs lines 86–89): if the replica has advanced since the cursor was issued, results continue from the newer snapshot with a debug log, potentially skipping or duplicating items that were added between pages. This is documented as acceptable for an eventually-consistent replica.

6. **`dividing_min_set_size: 25`** (ranking.rs line 122): the diversity pass is entirely inactive for result sets smaller than 25 — most searches will never trigger it.

7. **`rank_full` is the pagination path; `rank` is the legacy single-page path** — both are exported, but only `rank_full` is called from `collect_ranked_hits`.


# Nested agent agent-afdf450285b6dd112
Description: Audit server modules comprehensively
Size: 564620

Here is the complete exhaustive audit of the nudox server codebase.

---

## nudox Server — Exhaustive Audit

---

### 1. Full Route Table

**router.rs:49–64** — `router()` assembles three sub-routers with shared `Arc<Server<M>>` state. Middleware stack: `record_http_metrics` (RED metrics, `route_layer`), then `request_trace_layer` (OTel spans, outer `.layer`).

#### Read Plane (router.rs:169–177)

Body ceiling: **2 MiB** (`READ_PLANE_BODY_CEILING`). No timeout on this plane.

| Method | Path | Handler | Auth |
|--------|------|---------|------|
| `POST` | `/search` | `search::search` | `Principal` → `ReadCap` (`"search.symbols"`) |
| `POST` | `/search/semantic` | `search::search_semantic` | same, forces `semantic = true` |
| `POST` | `/packages/search` | `search::search_packages` | `Principal` → `ReadCap` (`"search.packages"`) |
| `POST` | `/expand` | `search::expand` | `Principal` → `ReadCap` (`"search.expand"`) |
| `GET` | `/symbols/:id` | `search::get_symbol` | `Principal` → `ReadCap` (`"search.resolve_symbol"`) |
| `GET` | `/sessions/:id` | `search::get_session` | `Principal` (cap not explicitly used) |

#### Write Plane — Mutations (router.rs:184–201)

Body ceiling: **min(max_request_bytes, 64 KiB)** (`WRITE_PLANE_BODY_CEILING`). Timeout: `limits.upload_timeout` (default 2 s), 504 on expiry.

| Method | Path | Handler | Auth |
|--------|------|---------|------|
| `POST` | `/packages` | `indexing::add_package` | `Principal` → `WriteCap` (`"packages.ensure_initialized"`) |
| `GET` | `/packages/:id` | `indexing::get_package` | None (no cap minted) |
| `POST` | `/packages/:id/sync` | `indexing::sync_package` | `Principal` → `WriteCap` (`"packages.sync"`) |

#### Write Plane — Operations (router.rs:197–200)

No timeout, no body limit.

| Method | Path | Handler | Auth |
|--------|------|---------|------|
| `GET` | `/healthz` | `health::livez` | None — always 200 |
| `GET` | `/readyz` | `health::readyz` | None — probes all backends |
| `GET` | `/metrics` | `health::metrics` | None — Prometheus text |

#### Admin Plane (router.rs:206–218)

Body ceiling: same as write plane (64 KiB). Timeout: `limits.upload_timeout`.

| Method | Path | Handler | Auth |
|--------|------|---------|------|
| `POST` | `/admin/packages/:id/verify` | `admin::verify_package` | `AdminPrincipal` → `AdminCap` (`"admin.verify"`) |
| `POST` | `/admin/packages/:id/rebuild` | `admin::rebuild_package` | `AdminPrincipal` → `AdminCap` (`"admin.rebuild"`) |

**Total: 12 routes.**

---

### 2. SearchPlanner Logic (planner.rs)

**planner.rs:12–15** — `Plan` is a simple two-variant enum: `Precise` or `Semantic(SemanticGate)`.

**planner.rs:23–78** — `SearchPlanner` owns one `Quota` (semantic budget). Defaults: **64 admissions per 60-second window** (`DEFAULT_SEMANTIC_BUDGET`, `DEFAULT_SEMANTIC_WINDOW`).

**`plan()` decision table (planner.rs:39–59):**
- `Query::Literal(_)` → always `Plan::Precise` (no check, no quota spend).
- `Query::Abstract(_)` → calls `self.semantic_quota.admit()`:
  - Admitted → `Plan::Semantic(SemanticGate::issue(<reason>))` where reason distinguishes `NaturalLanguage` vs `CodeSnippet`.
  - Denied (quota exhausted) → degrades to `Plan::Precise`, logs at debug.

**Quota implementation (planner.rs:88–126):** Fixed window (`Mutex<QuotaWindow>`). On each `admit()`: if `elapsed >= window`, reset to a fresh window with 0 admitted; then if `admitted < capacity`, increment and return `true`. A poisoned mutex returns `false` (deny the expensive path, not `panic!`).

**`authorize_similar()` (planner.rs:64–69):** Same quota path, gate reason `"similar-items sidebar"`. Not currently wired to any route — dead surface for now.

**`extend_across_federation()` (planner.rs:76–78):** Mints a gate from an already-issued reason, for per-source fan-out (one user query → N sources each needing their own single-use `SemanticGate`).

**`SemanticGate` is the only user-facing gate constructor path** — `SemanticGate::issue` is called only here (and in the extend helper), never in any handler or coordination layer directly.

---

### 3. ForgeRuntime Cold/Ready Typestate

**No `forge.rs` or `ForgeRuntime` exists in this codebase.** The lib.rs comment at line 121 explicitly states: "The HTTP client for the compiler daemon (replaces the in-process `ForgeRuntime` that was removed in the Buck2→Cargo migration)." The Cold/Ready typestate was completely removed. What remains is:

- **`compiler_client.rs`** — a plain `CompilerClient { base: Url, http: reqwest::Client }` with no typestate.
- The Cold/Ready pattern survives in other stores (`GlobalStore<Cold>`, `Store<Cold>`, `Queue<Cold>`, etc. — lib.rs:256–275), all from `heart::{Cold, Live}` — but none of these are `ForgeRuntime`.

The `CompilerClient::compile()` (compiler_client.rs:55–89) POSTs postcard-encoded `CompileRequest` to `{base}/compile`, expects postcard-encoded `CompileResponse`, and maps `CompileResponse::Err` to a typed `RemoteError`.

---

### 4. poll.rs Background Loops

**Five loops, started from `Server::serve()` (lib.rs:386–400), gated by `Role`:**

#### `queue_worker` (poll.rs:42–65) — Role: `Forge` or `All`

Supervisor loop wrapping `Indexer::run_worker_until(drain)`. On drain-signal clean return, exits cleanly. On error, backs off `SUPERVISOR_BACKOFF` (5 s), re-checks drain, restarts.

`run_worker_until` (indexing.rs:383–424): inner loop — every tick: reclaim expired leases per source, dequeue up to `max_inflight_jobs` jobs, `for_each_concurrent` drives them, then `sleep(poll_interval)`. Exits cleanly when `drain.is_cancelled()`.

#### `outbox_consumer` (poll.rs:70–112) — Role: `Gateway` or `All`

One task per `SinkKind` (Text, Vector, Graph). Per tick, per source in federation precedence:
1. Try to take a postgres advisory lock (`try_lock_sink`) — skip if another replica holds it.
2. Call `consume_once`: read watermark, read up to `OUTBOX_BATCH` (64) entries since watermark, call `materialize` per entry, advance watermark after each success.
3. Release lock.
4. `sleep(poll_interval)`.

`materialize` (poll.rs:156–186): reads `symbols_for(entry.package)` from GlobalStore; then dispatches:
- `SinkKind::Text` → `materialize_text` (tantivy upsert_batch).
- `SinkKind::Vector` → `materialize_vector` (per-symbol: cache→embed→`SymbolPoint`, then `store.uploader().oneshot(points)`).
- `SinkKind::Graph` → `materialize_graph` (`graph.insert_symbols`).

#### `package_index_poller` (poll.rs:352–363) — Role: `Gateway` or `All`

Every `poll_interval`, calls `packages.synchronize()` per source (pulls changed packages from postgres into local tantivy package index).

#### `text_index_poller` (poll.rs:368–391) — Role: `Gateway` or `All`

Every `poll_interval`, reads `outbox.head(SinkKind::Text)` and `outbox.read_watermark(SinkKind::Text)` per source, emits gauge `text_index_watermark_lag`. Pure observability — no writes.

#### `cas_gc` (poll.rs:313–348) — Role: `Gateway` or `All`

Runs on `GC_INTERVAL` (1 hour). Per source:
1. Calls `outbox.gc_consumed()` — deletes outbox rows at or below the minimum watermark across all sinks. Increments counter `outbox_rows_reclaimed`.
2. Calls `blobs.list_cas()` — emits gauge `cas_blobs_stored`. **Read-only**: blob deletion is explicitly deferred (see the extensive inline comment, poll.rs:269–313 — the race between a mark-and-sweep and an in-flight `put_manifest` is documented as unsolved).

---

### 5. Config Resolution — `nudox.toml` + `NUDOX_*` env

**config.rs:281–303** — `ServerConfiguration::resolve()` uses three-layer figment:

1. **Defaults** — `Serialized::defaults(Self::default())`: `127.0.0.1:8080`, single `"definitive"` source pointing at localhost services with defaults below.
2. **File** — Path from `NUDOX_CONFIG` env var, or `nudox.toml` in cwd. Parsed via the `toml` crate, merged as `Serialized::defaults`. Skipped if file absent.
3. **Env** — `environment_fragment()` (config.rs:406–419): scans all env vars with `NUDOX_` prefix. `NUDOX_CONFIG` is explicitly skipped. `__`-separated nesting (`NUDOX_LIMITS__MAX_INFLIGHT_JOBS=4`). Values parsed as JSON scalars (number/bool) with string fallback.

**Last layer wins** (env overrides file overrides defaults).

**Boot guard** (config.rs:315–332): in `Deployment::Production`, any source with default postgres URL (`postgres://nudox:nudox@127.0.0.1:5432/nudox`) or terminus password (`root`) is a hard startup error.

**Key fields and their defaults:**

| Field | Default | Env key |
|-------|---------|---------|
| `serving_address` | `127.0.0.1:8080` | `NUDOX_SERVING_ADDRESS` |
| `role` | `All` | `NUDOX_ROLE` |
| `deployment` | `Development` | `NUDOX_DEPLOYMENT` |
| `compiler_endpoint` | `http://127.0.0.1:8080` | `NUDOX_COMPILER_ENDPOINT` |
| `metadata_data_dir` | `None` | `NUDOX_METADATA_DATA_DIR` |
| `limits.upload_timeout` | 2 s | `NUDOX_LIMITS__UPLOAD_TIMEOUT` |
| `limits.max_request_bytes` | 256 MiB | `NUDOX_LIMITS__MAX_REQUEST_BYTES` |
| `limits.max_inflight_jobs` | 16 | `NUDOX_LIMITS__MAX_INFLIGHT_JOBS` |
| `limits.poll_interval` | 2 s | `NUDOX_LIMITS__POLL_INTERVAL` |
| `limits.job_lease` | 120 s | `NUDOX_LIMITS__JOB_LEASE` |
| `limits.job_deadline` | 600 s (10 min) | `NUDOX_LIMITS__JOB_DEADLINE` |
| `limits.drain_deadline` | 30 s | `NUDOX_LIMITS__DRAIN_DEADLINE` |
| `endpoints.postgres` | `postgres://nudox:nudox@127.0.0.1:5432/nudox` | `NUDOX_DEFINITIVE__ENDPOINTS__POSTGRES` |
| `endpoints.terminus` | `http://127.0.0.1:6363` | `NUDOX_DEFINITIVE__ENDPOINTS__TERMINUS` |
| `endpoints.qdrant` | `http://127.0.0.1:6334` | `NUDOX_DEFINITIVE__ENDPOINTS__QDRANT` |
| `endpoints.embeddings` | `http://127.0.0.1:11434/v1/embeddings` | `NUDOX_DEFINITIVE__ENDPOINTS__EMBEDDINGS` |
| `endpoints.object_store` | `file://{tmpdir}/nudox/blobs` | `NUDOX_DEFINITIVE__ENDPOINTS__OBJECT_STORE` |

---

### 6. Coordination Handlers

All live under `coordination/`.

#### `coordination/health.rs` — `Server::health()` and `Server::parse_status()`

**health.rs:29–52** — `Server::health()` probes every source concurrently via `probe_source` (5 backends per source: postgres, object_store, terminus, qdrant, tantivy — parallel `tokio::join!`). Folds into: `Ready`, `Degraded(Vec<BackendKind>)`, or `Down`. A dead overlay is non-fatal (degrades backends into report); a dead definitive base returns `Down` immediately.

**health.rs:56–63** — `Server::parse_status(package)` — thin wrapper: `GlobalStore::get_state`, maps `NotFound` to `Ok(None)`.

#### `coordination/initialization.rs` — `Server::ensure_initialized()`

**initialization.rs:38–52** — `initialization_decision()` pure function: state machine over `Option<&ResolutionState>` × `Option<Freshness>`:
- `None` / `Unindexed` → `Enqueue`
- `Progressing` → `AlreadyInFlight`
- `Stored` + `Stale` → `Enqueue`; `Stored` + `Fresh/None` → `Serve`
- `Failed` → `Enqueue`
- `DeadLettered` → `Hold`

**initialization.rs:90–126** — `ensure_initialized()`: looks up current state, calls decision table (always passes `freshness = None` — freshness only recomputed on sync), if `Enqueue` upserts a `GlobalPackage` record and enqueues. Both ops are idempotent.

#### `coordination/indexing.rs` — `Indexer` pipeline

**indexing.rs:41–55** — `Indexer { server, compiler, acquisition, fractions }`. The `fractions` map (Mutex-guarded) tracks per-package percentage during a job (progress fractions currently set but not exposed over HTTP).

**Four pipeline phases** (all call `self.advance()` to update `ResolutionState` before running):

1. **Acquire** (indexing.rs:85–98): reads coordinates from GlobalStore.
2. **Extract** (indexing.rs:100–127): `fetch_archive` (reqwest, timeout `job_deadline/2`), then `ingest_archive` (tar.gz, `ExtractionLimits::DEFAULT`, `EntryAllowlist::SAFE`) → `BlobBuilder`.
3. **Compile** (indexing.rs:129–179): serializes files into `CompileRequest`, calls `self.compiler.compile()` (postcard over HTTP), extracts `ir_bytes` + `references`; calls `builder.set_ir` + `builder.set_references`.
4. **Emit** (indexing.rs:182–227): `builder.finalize()` → manifest + sections; `extract_facets`; `blob::emit::emit` (writes CAS + sets outbox); `outbox.record_stored` (postgres: sets `Stored { hash }` state + facets); removes package from `fractions`.

**Archive URL construction** (indexing.rs:283–318): per-origin patterns for CratesIo, NpmPublic, PyPi (two-step: PyPI JSON metadata to sdist URL), NuGet, FlakeHub, Custom.

**Job lifecycle** (indexing.rs:383–535): `run_worker_until` → `drive_job` → `tokio::select! { job_future, heartbeat_future }`. Heartbeat (`beat_lease`) renews lease every `job_lease/3` (floor: 5 s). Failure classification via `classify_failure` maps `ServerError` variants to `FailureKind` (Transient/Malformed/SourceUnavailable/Timeout/Internal).

#### `coordination/search.rs` — `Server::search_symbols()`, `resolve_symbol()`, `expand()`

**search.rs:21–31** — `search_symbols`: dispatches on `Plan`:
- `Plan::Precise` → `precise_hits`: per-source tantivy search, merge overlay-first, bound by page limit.
- `Plan::Semantic(gate)` → `semantic_hits`: embeds query once, fans gate across federation via `extend_across_federation`, per-source qdrant search, hydrates ids to symbols via `symbol_by_id`, applies filter, merges overlay-first.

**search.rs:34–45** — `resolve_symbol`: walks federation in precedence order, returns first `Some` hit tagged with source.

**search.rs:47–58** — `expand`: per-source `related_hits` (graph walk, 1-deep BFS, breadth 16 — `EXPANSION_BOUNDS` in search/mod.rs:87–90), merges overlay-first (unlimited, caller truncates at request limit).

---

### 7. Component Triage: Client Library vs INDEX Service vs Delete

#### Belongs in a Future Client Library

- **`http/dto.rs:22–78`** — `SearchRequestDto` and its `into_search()` lowering, `AddPackageDto` and `into_coordinates()`. These are the wire shapes a client SDK would need to serialize.
- **`search/query.rs`** — `LiteralQuery`, `AbstractQuery`, `Query`, `Filter`, `PackageSelector`, `Pagination`, `Search`. The full query vocabulary belongs in a shared types crate the client and server both depend on.
- **`error.rs:57–120`** — `QueryError` — client-visible error taxonomy for query parsing.
- **`http/dto.rs:161–165`** — `HealthDto` — client-visible health shape.
- **`coordination/initialization.rs:14–18`** — `Initialized` struct — the response DTO for add/sync/get-package. Already `Serialize + Deserialize`.
- **`http/handlers/admin.rs:29–64`** — `VerifyResponse`, `RebuildResponse` — admin response shapes.

#### Belongs in the INDEX Service (keep as-is, INDEX-specific logic)

- **`coordination/indexing.rs`** — the entire pipeline (acquire, extract, compile, emit, job worker loop, heartbeat, failure classification, `parse_cargo_toml`, `extract_facets`). This is the heart of the INDEX plane's work.
- **`poll.rs`** — all five background loops are INDEX/coordinator-level infrastructure.
- **`save/blobs.rs`** — `verify_blobs`, `rebuild_from_blobs`, `BlobAudit`, `SinkDrainState`. Admin operations tied to the blob store.
- **`save/mod.rs`** — `rebuild`, `verify_reproducible`, `current_manifest`. Ditto.
- **`coordination/initialization.rs:38–127`** — `initialization_decision` + `ensure_initialized` — server-side lifecycle state machine.
- **`search/registry.rs`** — `PackageSearchIndex`, `RegistrySearchSurface` — the package tantivy index is a local replica maintained by the server.
- **`search/symbols.rs`** — `SymbolTextSurface` — adapter over the local replica text index.
- **`coordination/health.rs`** — backend probing and aggregate health.
- **`authz.rs`** — capability witnesses; appropriate at the service boundary.
- **`compiler_client.rs`** — the compile plane client stub.
- **`lib.rs` (`Server`, `SourceStores`, assembly)** — the assembled service.

#### Candidates for Deletion or Major Simplification

- **`search/planner.rs:64–69` — `authorize_similar()`** — not wired to any route. Dead code until a "similar items" sidebar endpoint exists.
- **`save/blobs.rs:249–307`** — `sink_drain_state()` and `sink_drained_for()` and `derived_store_current()` are all `#[allow(dead_code)]` — not called from any live path. Either wire them to a route or delete.
- **`coordination/indexing.rs:614–758` — `CargoManifestFacts`, `parse_cargo_toml`, `extract_facets`** — only handles Rust; a non-Rust ecosystem falls back immediately (line 703). The Rust path is real but the `dependencies` vec (line 738) is permanently empty and `loc` is always 0 (line 737) — those fields are stubbed.
- **`http/dto.rs:149–159` — `bad_request()` helper** — marked `#[allow(dead_code)]` in its own comment ("Deprecated in favor of direct construction…").
- **`search/query.rs:96–106`** — `VersionConstraint` enum is defined but `PackageSelector.version` is always `None` in `SearchRequestDto::into_search()` (line 59: `PackageSelector { name, version: None }`). Version-based filtering is dead.


# Nested agent agent-a9c1f9b84566bb184
Description: Audit heart crate comprehensively
Size: 370031

All files have been read. Here is the exhaustive audit.

---

## nudox `workspace/heart` — Exhaustive Audit

---

### 1. Identity Types

#### `Id<T>` — `identity/id.rs:10`
The universal identifier constructor. A newtype over `uuid::Uuid` branded by phantom `T`:
```
pub struct Id<T>(Uuid, PhantomData<fn() -> T>)
```
- `from_uuid(uuid)` — hydrates from storage (line 15)
- `new_random()` — v4 UUID, for values with no reproducible key (line 22)
- `from_name(namespace, name)` — v5 UUID (deterministic), the single derivation primitive used everywhere (line 27)
- `cast::<U>()` — re-tags to a different brand preserving the UUID (line 32)
- Derives: `Copy`, `Clone`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`, `Debug`, `Display`, `Serialize`, `Deserialize`
- `Debug` prints `Id<typename>(uuid)` (line 53); `Display` delegates to `Uuid::Display` (line 58)

#### `PackageId` — `identity/package.rs:18`
```
pub enum Package {}           // zero-variant phantom brand
pub type PackageId = Id<Package>;
```
Derived via `Id::from_name(&namespace::PACKAGE, &coordinates.identity_bytes())` — constructed in `package/coordinates.rs:37`.

#### `SymbolId` — `identity/symbol.rs:15`
```
pub type SymbolId = Id<Symbol>;
```
Derived in `EntryUri::symbol_id(instance_token)` (line 42): concatenates `instance_token ++ NUL ++ canonical_uri` as bytes, then `Id::from_name(&namespace::SYMBOL, &bytes)`. Salted per-instance so two corpus instances never share symbol ids.

#### `EntryUri` — `identity/symbol.rs:20–48`
```
pub struct EntryUri {
    pub package: PackageId,
    pub path:    Box<[SmolStr]>,
}
```
- `canonical()` — `"{package_id}/{seg0}/{seg1}/…"` using `/` as separator (unambiguous since no ecosystem uses it in its symbol grammar) (line 31)
- `symbol_id(instance_token)` — deterministic derivation site (line 42)

#### `ContentHash` — `content.rs:10`
```
pub struct ContentHash([u8; 32]);
```
- `from_bytes([u8; 32])` — wrap raw digest (line 14)
- `of_bytes(&[u8])` — `blake3::hash(bytes)` (line 20)
- `hex()` — lower-hex via `data_encoding::HEXLOWER` (line 23)
- `builder()` — returns `ContentHasher` (line 25)
- `as_bytes()` — raw 32 bytes (line 17)

#### `JobKey` — `content.rs:58`
```
pub struct JobKey(ContentHash);
```
Newtype over `ContentHash`. Derives via `JobKey::derive(producer_version, toolchain, source, dep_lock)` (line 63): streams each component with a `u64` little-endian length prefix, then a blake3 final. The golden digest for this layout is tested against `"3ae60363..."` (line 150). Child keys via `with_tag(tag)`: `H(jobkey ‖ len(tag) ‖ tag)` (line 86).

#### UUIDv5 Namespaces — `identity/namespace.rs`
- `PACKAGE: Uuid = 0x6e75_646f_785f_706b_675f_6e73_0000_0001` (line 7) — for `PackageId`
- `SYMBOL: Uuid = 0x6e75_646f_785f_7379_6d5f_6e73_0000_0002` (line 10) — for `SymbolId`
- Source backend namespace: `NAMESPACE = 0x6e75_646f_785f_7372_635f_6e73_0000_0003` (access/source.rs:11) — for `SourceId`

---

### 2. Connect/Cold/Live Typestate Machinery

#### `connection.rs`
- `struct Cold;` (line 9) — configured-but-unverified store handle
- `struct Live;` (line 12) — connected, ready store handle
- `trait Connect: Sized` (line 20):
  - Associated type: `type Live` — the live handle produced on success (line 22)
  - Method: `async fn connect(self) -> Result<Self::Live, ConnectError>` (line 25)
  - Custom diagnostic: `#[diagnostic::on_unimplemented]` (lines 16–19) that fires if a type does not implement `Connect`

The `Cold`/`Live` pair is a classic typestate — calling `connect()` consumes the `Cold` and produces the `Live` associated type. Query methods live on the `Live` type, making it impossible to query an unconnected store at compile time.

---

### 3. Failure Taxonomy

#### `error/mod.rs`

**`StoreError` trait** (line 7): blanket `impl` for any `Error + Send + Sync + 'static`. Used as a bound everywhere an error from storage is threaded through.

**`Retryable` trait** (lines 13–22):
- `is_retryable(&self) -> bool` — transient (timeout, 5xx, reset) vs fatal
- `retry_after(&self) -> Option<Duration>` — backpressure hint (default `None`)

**`BackendKind` enum** (lines 26–44):
| Variant | Meaning |
|---|---|
| `Postgres` | Global index / relational spine |
| `Qdrant` | Semantic vector store |
| `Terminus` | Graph store |
| `ObjectStore` | Blob store (S3/GCS/local) |
| `Tantivy` | Full-text search index |

#### `error/connect.rs`

**`ConnectFailure` enum** (lines 5–28):
| Variant | Meaning |
|---|---|
| `Unreachable` | DNS/refused/network |
| `Auth` | Authentication/authorization rejected |
| `SchemaMismatch` | Schema or migration state incompatible |
| `DimensionMismatch { expected, found }` | Vector collection dimension disagrees with compiled `DIM` |
| `Timeout` | Handshake timed out |
| `Other(anyhow::Error)` | Anything else, source preserved |

**`ConnectError` struct** (lines 31–38):
```
pub struct ConnectError {
    pub backend: BackendKind,
    pub kind:    ConnectFailure,   // #[source]
}
```
- Display: `"failed to connect to {backend}: {kind}"`
- `Retryable`: `is_retryable()` is `true` only for `Unreachable` and `Timeout` (lines 49–54)

#### `error/failure.rs`

**`Phase` enum** (lines 27–36) — pipeline phases in order:
| Variant | Meaning |
|---|---|
| `Acquiring` | Resolving version + downloading source archive |
| `Extracting` | Extracting + sanitizing untrusted source archive |
| `Compiling` | Compiler lowering source to IR |
| `Emitting` | Fanning parsed result out to derived stores |

**`ErrorDetails` enum** (lines 43–47):
- `Message(String)` — fallback for cases where only a rendered message was captured; more specific variants deferred

**`Failure` struct** (lines 51–67):
```
pub struct Failure {
    pub attempts: u32,
    pub phase:    Phase,
    #[serde(rename = "error")]
    pub message:  String,
    pub cause:    Option<ErrorDetails>,
    pub at:       DateTime<Utc>,
}
```

**`FailureKind` enum** (lines 71–78):
| Variant | Retriable |
|---|---|
| `Transient` | yes |
| `SourceUnavailable` | no |
| `Malformed` | no |
| `Timeout` | yes |
| `Unsafe` | no |
| `Internal` | no |
`is_retriable()` is `const fn` (line 81).

**`ResolutionState` enum** (lines 88–99):
| Variant | Meaning |
|---|---|
| `Unindexed { needed: bool }` | No indexing attempted |
| `Progressing(Phase)` | In progress at given phase |
| `Stored { hash: ContentHash }` | All phases complete |
| `Failed(Failure)` | Failed, retriable until policy ceiling |
| `DeadLettered(Failure)` | Failed, awaiting human inspection |

---

### 4. `DerivedStore` — `sink.rs:37–41`

```
pub enum DerivedStore {
    Vector,
    Graph,
    Text,
}
```
Names the three derived stores that a record fans out to after indexing. Wire token is lowercase name. Also carries `strum::VariantNames` for the postgres `CHECK` domain source.

The file also defines the fan-out delivery machinery:
- `RetryTransient { remaining: usize }` (line 44) — a `tower::retry::Policy` that retries up to `remaining` times when `E: Retryable` and `is_retryable()` is true (line 61–65)
- `SinkExt<Req>` trait (line 76) — extension trait over `tower::Service` providing `deliver()` (4-budget default) and `deliver_with_budget()`, backed by `Retry<RetryTransient, S>` (lines 91–95)

---

### 5. Federation Model — `access/federation.rs` and `access/source.rs`

#### `Source` and `SourceId` — `access/source.rs`
```
pub struct Backend;                     // phantom brand
pub type SourceId = Id<Backend>;        // line 17
pub struct Source {
    pub id:      SourceId,
    pub name:    SmolStr,
    pub backend: BackendKind,
}
```
`SourceId` is derived deterministically from `name` bytes in the `NAMESPACE` UUID (line 29), so the same named source keeps its identity across restarts.

#### `SourceRole` — `access/federation.rs:10`
```
pub enum SourceRole { Definitive, Overlay }
```

#### `Sourced<T>` — `access/federation.rs:17`
```
pub struct Sourced<T> {
    pub value:  T,
    pub source: SourceId,
    pub role:   SourceRole,
}
```
Methods: `map()`, `as_ref()`.

#### `Federation<S>` — `access/federation.rs:45`
```
pub struct Federation<S> {
    base:     Sourced<S>,         // exactly one definitive source
    overlays: Vec<Sourced<S>>,    // zero or more precedence-ordered overlays
}
```
- `new(base_id, base)` — starts from definitive base (line 52)
- `with_overlay(id, handle)` — appends at lowest overlay precedence (line 64)
- `base()` — single-source fast path (line 74)
- `in_precedence()` — iterator: overlays (highest first) then definitive base (lines 78–83)
- `query<T, F>(f)` — applies closure to each source in precedence order, returns first `Some` wrapped in `Sourced<T>` (lines 88–96)

The model: one DEFINITIVE registry plus any number of user-hosted OVERLAY registries. Overlays take precedence.

---

### 6. Cache Module

#### Overall structure — `cache/mod.rs`

Three stampede-resistance primitives + three CAS tiers, all unified in one module (formerly two separate crates: `caching` and `cas`).

**Public exports:**
- `jittered` — TTL jitter
- `SingleFlight<K>`, `Ticket` — request coalescing
- `StampedeCache<K, V>` — moka-backed L1 with XFetch + stale-while-revalidate
- `DiskCas` — L2 disk CAS
- `CasError` — error type
- `MemoryCas` — in-process test/L1 CAS
- `NoL3`, `Tiered<L3>` — tiered composition
- Re-exports `ContentHash`, `JobKey`

**`Cas` trait** (lines 93–109):
- `get(key: ContentHash) -> Result<Option<Bytes>, CasError>` — clean miss is `Ok(None)`
- `put(bytes: Bytes) -> Result<ContentHash, CasError>` — hashes and stores
- `put_keyed(key, bytes) -> Result<bool, CasError>` — first-write-wins; `false` = already existed

**`EvictableCas` trait** (lines 112–121):
- `invalidate(key) -> Result<(), CasError>` — only for mutable tiers (DiskCas, MemoryCas, Tiered)

#### blake3 usage
- `ContentHash::of_bytes(bytes)` — `blake3::hash(bytes)` (content.rs:20)
- `ContentHasher` — wraps `blake3::Hasher` for streaming (content.rs:36)
- `JobKey::derive(...)` — streaming blake3 with length-prefixed parts (content.rs:67–73)
- `DiskCas` blob envelope: `blake3(value) ‖ value` — self-authenticating blobs (disk.rs:157–173)

#### moka usage — `cache/stampede.rs`
`StampedeCache<K, V>` wraps `moka::future::Cache<K, Entry<V>>`. Hard TTL is `2 × soft_ttl` (line 92) to keep values servable during background refresh.

#### Tiered CAS — `cache/tiered.rs`
```
L1 — StampedeCache<ContentHash, Bytes>  (in-process, moka + XFetch)
L2 — Option<DiskCas>                    (node-local plain directory, runtime option)
L3 — L3: Cas (compile-time type, NoL3 = absent)
```
- `L1_TTL = 24h`, `PROMOTE_COST = 1ms` assumed XFetch cost for promotions
- Read order: L1 → L2 → L3; hits promote upward
- Write order: durable tiers first (L2, L3) then L1
- `NoL3` returns `CasError::Unsupported` — `is_unsupported()` makes this a soft skip in all paths
- Eviction (invalidate) drops L1 + L2 only; L3 is content-addressed and treated as immutable

#### Single-flight — `cache/single_flight.rs`
```
pub struct SingleFlight<K> {
    inflight: DashMap<K, Arc<Notify>>,
}
pub enum Ticket<'a, K> {
    Leader(Leader<'a, K>),
    Waiter(Arc<Notify>),
}
```
- `enter(key)` — first caller gets `Leader`, concurrent callers get `Waiter(Arc<Notify>)` (line 60)
- `Leader` is an RAII guard; on `drop` removes key from inflight and calls `notify_waiters()` — so a panicking or failed leader never wedges the herd (lines 101–108)
- `is_inflight(key)` — lets waiters detect a leader that finished between handoff and park (line 73)
- Lost-wakeup-safe pattern (documented at line 83–91): register `notified().enable()` before re-checking store

#### Stampede / XFetch — `cache/stampede.rs`
`StampedeCache::get_or_load(key, compute)`:
1. Hit → `maybe_refresh` (probabilistic background refresh), return immediately (lines 119–121)
2. Miss → `enter(key)`:
   - `Leader`: compute, on `Ok` store, on `Err` store nothing (waiter re-races) (lines 125–134)
   - `Waiter`: register `notified.enable()` before re-checking store (lost-wakeup-safe), loop (lines 136–149)

`should_refresh(entry)` — XFetch: `age + cost × beta × (−ln U) ≥ ttl`, where `U` is uniform random (lines 158–163). Beta defaults to `1.0` (paper optimum), configurable via `with_beta()`.

`maybe_refresh` — spawns a background `tokio::spawn` that tries to become `Leader` and recompute; if another refresh is already in flight (`is_inflight`), does nothing (lines 168–196).

#### Jitter — `cache/jitter.rs`
`jittered(base, frac)` — multiplies `base` by uniform random factor `[1−frac, 1+frac]`, `frac` clamped to `[0, 1]` (line 13–21). De-synchronises write-time TTLs; complements XFetch which de-synchronises reads.

#### `DiskCas` — `cache/disk.rs`
- Layout: `{root}/cas/{blake3-hex}` (line 50)
- Blob format: `blake3(value) ‖ value` (lines 157–173) — self-authenticating
- Env var: `NUDOX_PARSE_CACHE` or temp `nudox-parse-cache` (lines 39–42)
- Race-safe publish: writes unique-named temp (`{pid}.{counter}.tmp`), then `hard_link`; `AlreadyExists` → `Ok(false)` (lines 83–121)
- Sync methods (`get_sync`, `put_keyed_sync`, `invalidate_sync`) for `spawn_blocking` callers
- Implements `Cas` and `EvictableCas`

#### `CasError` — `cache/error.rs`
| Variant | Meaning |
|---|---|
| `Io { path, source }` | Local filesystem failure |
| `Integrity { path }` | Blake3 envelope check failed (corrupt blob; auto-deleted on read) |
| `Unsupported(&'static str)` | Operation not available on this tier (e.g. `NoL3`) |
`is_unsupported()` is the soft-skip predicate (line 38).

#### `MemoryCas` — `cache/memory.rs`
`Mutex<HashMap<ContentHash, Bytes>>` — no durability, for tests and ephemeral L1 fill. First-write-wins via `HashMap::entry` (lines 36–42).

---

### 7. Ecosystem, Tenant, Cursor, Score, Health Types

#### `Language` — `ecosystem.rs:32–55`
Enum covering 7 languages: `Rust`, `Typescript`, `Python`, `Go`, `Java`, `Nix`, `CSharp`. Uses `ConstParamTy` (line 20) so it can be a const generic parameter. Wire token is lowercase via strum `serialize_all = "lowercase"`. `as_token()` returns `&'static str` (line 64).

#### `Edition` — `ecosystem.rs:68–74`
`E2015`, `E2018`, `E2021`, `E2024`.

#### `Toolchain` — `ecosystem.rs:78–100`
Per-ecosystem enum carrying concrete version info:
- `Rust { compiler: Version, edition: Edition }`
- `Typescript { compiler: Version }`
- `Python { interpreter: Version }`
- `Go { compiler: Version }`
- `Java { compiler: Version }`
- `Nix { evaluator: Version }`
- `CSharp { sdk: Version }`

#### `OwnerKind` / `Visibility` — `tenant.rs`
```
pub enum OwnerKind { Individual, Enterprise }
pub enum Visibility { Personal, Private, Public }
```
Both stored as lowercase SQL tokens. `VariantNames::VARIANTS` is the source for postgres `CHECK` constraints.

#### `Cursor<K, P>` — `cursor.rs:84`
```
pub struct Cursor<K, P: SnapshotPolicy = Advisory> {
    pub after:    K,
    pub snapshot: ContentHash,
    _policy:      PhantomData<fn() -> P>,   // #[serde(skip)]
}
```
Two policy types, sealed via `mod private`:
- `Enforced` (line 52) — freshness checked at construction AND decode; text search uses this
- `Advisory` (line 57) — snapshot carried as hint only; ANN/Qdrant uses this

`PolicyTag` enum (line 22) is written into the wire envelope (`Wire<K>`) so the brand survives encoding/decoding — prevents silently reinterpreting an `Advisory` token as `Enforced`.

`Cursor::decode_tagged` is crate-private; public decode paths are:
- `Cursor::<K, Advisory>::decode(token)` — tag check only (line 170)
- `Cursor::<K, Enforced>::decode(token, live: ContentHash)` — tag check + mandatory freshness re-verify against `live` (line 207)

`CursorError` (lines 222–241):
| Variant | Meaning |
|---|---|
| `Encoding(DecodeError)` | Not valid base64url |
| `Payload(postcard::Error)` | Decoded bytes don't match keyset shape |
| `PolicyMismatch { expected, found }` | Token minted under different policy |
| `StaleSnapshot` | Snapshot doesn't match live index; restart pagination |

Encoding: `postcard` → `BASE64URL_NOPAD` (line 125–127).

#### `Score` / `Scored<T>` — `score.rs`
```
pub struct Score(f32);      // nutype, validates finite
pub struct Scored<T> { pub value: T, pub score: Score }
```
`nutype` validates `finite` (no NaN/Inf). `PartialOrd` on `Scored<T>` orders by score alone (line 48). `Scored::map()` preserves score (line 39).

#### `Probe` / `Probeable` — `health.rs`
```
pub struct Probe {
    pub backend:    BackendKind,
    pub healthy:    bool,
    pub latency_ms: Option<u32>,
    pub detail:     Option<String>,
}
pub trait Probeable {
    fn backend(&self) -> BackendKind;
    async fn probe(&self) -> Probe;
}
```
`timed(backend, body)` — measures round-trip, `None` detail = healthy (lines 45–53). `assert_probe_future_send::<P>()` — compile-time guard using return-type notation `P: Probeable<probe(..): Send>` (line 63).

#### `Page<T>` — `search.rs`
```
pub struct Page<T> {
    pub items: Vec<Scored<T>>,
    pub next:  Option<String>,   // opaque cursor token
}
```

#### `Percent` / `JobProgress` / `Progressive` — `progress.rs`
- `Percent(u8)` — nutype, validates `≤ 100` (line 12)
- `Progressive` trait (line 50): `type Phase`, `const PHASES`, `status()`, `current_phase()`, `phase_progress()`, `overall()`, `is_complete()`, `when_complete()`
- `JobProgress { state: ResolutionState, phase_fraction: Percent }` — implements `Progressive` with `Phase` order: Acquiring → Extracting → Compiling → Emitting (line 131)

#### `Versioned<T>` — `version.rs`
```
pub struct Versioned<T> { version: PackageVersion, object: T }
```
Private fields with accessors `version()`, `get()`, `into_inner()`, `map()`.

#### `Symbol` / `Name` / `SymbolKind` — `symbol.rs`
```
pub struct Name { pub plain: SmolStr, pub fully_qualified: SmolStr }
pub struct Symbol {
    pub id:        SymbolId,
    pub package:   PackageId,
    pub ecosystem: Language,
    pub name:      Name,
    pub kind:      SymbolKind,
}
pub enum SymbolKind { Function, Type, Module, Constant, Variable, Trait, Impl, Other }
```

#### `PackageName` / `Coordinates` — `package/mod.rs`, `package/coordinates.rs`
```
pub struct PackageName { pub ecosystem: Language, canonical: SmolStr, original: SmolStr }
pub struct Coordinates { pub origin: RegistryOrigin, pub name: PackageName, pub version: PackageVersion }
```
`Coordinates::id()` calls `Id::from_name(&namespace::PACKAGE, &self.identity_bytes())` — the canonical `PackageId` derivation site (coordinates.rs:37). `identity_bytes()` is length-prefixed (origin token + canonical name + canonical version) making the encoding injective (coordinates.rs:21–35).

Per-ecosystem name canonicalization in `package/mod.rs`:
- Rust: lowercase, `_` → `-` (line 93)
- npm: lowercase, optional `@scope/name` (line 101)
- PyPI: PEP 503 lowercase, separator runs → `-` (line 122)
- Go: as-provided if valid chars (line 136)
- Java/Maven: lowercase (line 146)
- NuGet/C#: lowercase (line 157)
- Nix/FlakeHub: `org/project`, lowercase, max one slash (line 167)

---

### Key Architectural Notes

- `lib.rs:2` requires `#![feature(adt_const_params)]` (for `Language` as const param) and `#![feature(return_type_notation)]` (for `Probeable<probe(..): Send>`)
- `DerivedStore` (sink.rs) is the fan-out discriminant for the three derived stores; it is NOT the same as `BackendKind` (error/mod.rs), which tracks where failures originate
- The `Guid` type alias (`lib.rs:54`) = `uuid::Uuid`, distinct from the branded `Id<T>` hierarchy
- `PackageVersion::Nix` carries `semver::Version` with `+rev-{sha}` build metadata preserved but ignored in ordering (package.rs:99–101)
- `RegistryOrigin::Custom { name: SmolStr, url: url::Url }` allows non-standard registries; `token()` returns `Cow::Owned(name)` for them (package.rs:188)


