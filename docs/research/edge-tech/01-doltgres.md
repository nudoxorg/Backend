# Doltgres Deep Dive for nudox (INDEX / REGISTRY / Graph Tiering)

**Research date:** 2026-07-16 (facts re-verified against GitHub releases, DoltHub blog, Doltgres docs, Hosted product pages)  
**Scope:** DoltgreSQL (and Dolt family as needed) as a candidate major component for the nudox platform — especially remote INDEX metadata, versioned symbol/catalog storage, multi-generation lineage, and comparison to TerminusDB hot-tier graph.  
**Non-goals:** Implement code; rewrite master architecture; replace CAS design; produce a product decision with legal/procurement sign-off.

**Related research (read, not rewritten):**
- [07 Terminus tiering](../librarification/07-terminus-tiering/PLAN.md) — hot-tier admission, Terminus v12 state, WOQL/GraphQL, DB-per-package
- [11 SQLite INDEX](../librarification/11-sqlite-index/PLAN.md) — INDEX as single-writer SQLite + Litestream; schema map of packages/parse_status/symbols/outbox
- [20 Graph-over-IR](../librarification/20-graph-over-ir/PLAN.md) — cold `GraphStore` over CAS IR; hop-class inventory (mostly depth-1)
- Live schema: `workspace/registry/schema/mod.rs` (`packages`, `parse_status`, `jobs`, `outbox`, `sink_watermarks`, `symbols`)

**Primary external sources (verify live):**
- https://github.com/dolthub/doltgresql (releases, README, architecture notes)
- https://www.doltgres.com/docs/introduction/
- https://www.dolthub.com/blog/2025-04-16-doltgres-goes-beta/
- https://www.dolthub.com/blog/2026-06-26-doltgres-1-0-coming-this-fall/
- https://www.dolthub.com/blog/2026-07-10-doltgres-99-percent-sql-logic-tests/
- https://www.dolthub.com/docs/architecture/storage-engine/
- https://www.doltgres.com/docs/reference/version-control/remotes/
- https://www.dolthub.com/docs/concepts/dolthub/permissions/
- https://www.dolthub.com/blog/2024-11-07-doltgres-supports-users/
- https://github.com/dolthub/doltlite (embedded / local-first sibling)
- https://www.dolthub.com/ (Hosted Dolt / DoltHub product matrix)

---

## 0. Executive framing for nudox

nudox splits concerns roughly as:

| Layer | Role | Planned store (baseline) |
|---|---|---|
| **INDEX** (remote) | Catalog, symbols projection, jobs metadata, outbox/watermarks, optional hot graph pointer | Postgres → **SQLite + Litestream** (11); CAS blobs in S3 |
| **REGISTRY** (desktop) | Offline package cache, disk CAS, embedded tantivy + vectors | sqlite + disk CAS |
| **Hot graph** | Multi-gen symbol docs, lineage, path-ish queries for *hottest* packages only | **TerminusDB** (07) |
| **Cold graph** | Same `GraphStore` trait over IR blobs | In-memory projection from CAS (20) |

**The Doltgres question (original):** Does a version-controlled Postgres-compatible SQL database collapse some of INDEX metadata *and* multi-generation symbol/graph history into one system with Git-like commits/branches/diffs — reducing or eliminating Terminus for hot packages?

**The Doltgres question (reframed 2026-07-16):** Not as Terminus/SQLite full replacement, but as a **versioned relational catalog** for (1) crates.io-page-class **package metadata** across ecosystems and (2) treesitter/symbol **catalog rows** with commit history — beside SQLite operational INDEX and Terminus/cold-IR graph. Full detail: **§9R**.

**Short verdict up front — Updated 2026-07-16 reframing** (supersedes same-day A–D default; prior prose kept in §9):

| Option | Verdict |
|---|---|
| **(A) INDEX metadata only** *(ops SoT)* | **Still reject** for jobs/outbox/parse_status SoT — versioning operational tables is mostly *anti*-feature. *(Unchanged.)* |
| **(A′) Package metadata catalog (versioned)** | **Evaluate strongly** — crates.io-page fields (description, owners, versions, downloads, license, links, readme, keywords, features, …) as commit-gated, diffable tables. See §9R. |
| **(B) metadata + symbols** *(broad INDEX dual-role)* | **Superseded by A′/B′** — do not merge ops INDEX into Doltgres. |
| **(B′) Symbol catalog with generation history via commits** | **Evaluate** — moniker/kind/path/sig_hash rows; `dolt_diff` for “symbols appeared between 1.0 and 1.1”; not full trees (re-parse per plan 13). |
| **(C) full graph replacing Terminus** | **Still reject** — graph edges/path queries remain Terminus-hot or cold IR. |
| **(D) Core SoT for jobs/outbox** | **Still reject** — SQLite+Litestream (11) remains operational SoT. |
| **Recommended role if any** | **Promising side-store (post-1.0 spike):** self-hosted Doltgres **catalog service** for A′±B′ next to SQLite INDEX; optional public **Dolt (MySQL) / DoltHub** read-only mirrors if remote parity still missing for Doltgres; desktop still sqlite REGISTRY snapshots. **Not** Terminus, not IR blob store, not multi-tenant org model. |

---

## 1. What is Doltgres vs Dolt vs Postgres?

### 1.1 One-line definitions

| Product | What it is | Wire / dialect | Maturity (2026-07-16) |
|---|---|---|---|
| **Postgres** | Classic OLTP RDBMS | Postgres wire + SQL | Production gold standard |
| **Dolt** | Version-controlled SQL DB (Git for *tables*) | **MySQL** wire + dialect | **1.0 / production** for years; many customers |
| **Doltgres / DoltgreSQL** | Postgres-flavored Dolt | **Postgres** wire + dialect | **Beta**; latest release **v0.57.0** (2026-07-10); **1.0 targeted 2026-08-06** |
| **DoltLite** | SQLite fork with prolly-tree pager | SQLite C API / SQL | **Alpha** (~v0.11.x Jul 2026); embeddable / local-first |
| **DoltHub** | Hosted collaboration (GitHub-for-data) | HTTPS remotes, PRs, orgs | Mature for **Dolt**; Doltgres **cannot push to DoltHub/DoltLab** yet (custom remotes only) |
| **Hosted Dolt** | Managed cloud server (AWS/GCP style) | SQL server | Advertises **Dolt and Doltgres** |

Sources: [doltgresql README](https://github.com/dolthub/doltgresql), [doltgres intro](https://www.doltgres.com/docs/introduction/), [doltgres.com product page](https://doltgres.com/), [dolthub.com](https://www.dolthub.com/).

### 1.2 Architecture (Doltgres)

From DoltHub’s beta launch post and the storage-engine docs:

1. **Postgres wire protocol** — clients speak `psql`, `sqlx`+postgres, `pgx`, Prisma, Django, etc.
2. **Postgres SQL parser** (Cockroach-derived lineage in-tree under `postgres/parser`) → AST.
3. AST **translated** into forms the shared engine understands.
4. **Query engine:** [go-mysql-server](https://github.com/dolthub/go-mysql-server) (same engine as Dolt) — *not* the Postgres executor.
5. **Storage engine:** Dolt’s **Prolly Trees + commit graph** (shared with Dolt).
6. **Version control surface:** SQL functions/procedures + system tables (`dolt_log`, `dolt_status`, `dolt_diff_*`, `DOLT_COMMIT()`, `DOLT_BRANCH()`, …) — **no Dolt-style CLI** on Doltgres.

Implication: “Postgres-compatible” means **dialect + wire + types + catalog**, not “we run Postgres under the hood.” Correctness and performance are therefore *reimplemented*, measured against Postgres as oracle.

```
Client (pg wire)
    │
    ▼
Doltgres server (Go)
    ├─ Postgres parser → AST
    ├─ Dialect translation layer
    ├─ go-mysql-server query engine
    ├─ VC procedures / system tables
    └─ Dolt storage (prolly trees + commit graph on disk)
```

### 1.3 Prolly trees (why versioning is cheap-ish)

Dolt (and thus Doltgres) stores table data and schema as **content-addressed probabilistic B-trees** (“Prolly Trees”), roots arranged in a **Merkle DAG commit graph** (Git-like).

Properties that matter for nudox:

| Property | B-tree (Postgres/SQLite) | Prolly tree (Dolt family) |
|---|---|---|
| Point read | O(log n) | O(log n) |
| Diff of size *d* | O(n) scan both trees | **O(d)** — walk differing hashes |
| Structural sharing | ❌ full copies or MVCC undo | ✅ identical subtrees shared across commits |
| History-independent root hash | No | Yes — same row set ⇒ same hash (order-independent) |

Refs: [Storage Engine docs](https://www.dolthub.com/docs/architecture/storage-engine/), [Prolly Tree docs](https://www.dolthub.com/docs/architecture/storage-engine/prolly-tree/).

**For package generations:** if generation N and N+1 share 95% of symbol rows, storage growth tracks the *delta*, and `dolt_diff` between commits is proportional to changed rows — ideal for “what symbols changed between versions?” **without** re-scanning full tables.

### 1.4 Versioning model (Git mapped to SQL)

| Git concept | Dolt / Doltgres |
|---|---|
| Working tree | Uncommitted table mutations on current branch session |
| Staging | `dolt_add(...)` / staged flag in `dolt_status` |
| Commit | `dolt_commit('-m', ...)` → content-addressed hash |
| Branch | Named pointer to commit; checkout switches HEAD |
| Merge | Three-way **row-level** merge; conflicts on same PK |
| Diff | `dolt_diff_*` tables / TVFs; cell-level to_/from_ |
| Log | `dolt_log` system table |
| Remote | `dolt_remote`, `dolt_push`, `dolt_pull`, `dolt_clone` |
| PR | **DoltHub product** (not core DB); review + merge workflow for data |
| Time travel | `SELECT ... AS OF 'commit_hash'` / branch / tag |
| Rebase / cherry-pick / revert | Supported in Dolt family (surface maturity varies by product) |

**Critical semantic:** Version control applies to **database tables (schema + data)**, not to arbitrary blobs. Blobs belong in CAS (S3/disk); Dolt stores **hashes / metadata / relational projections**.

### 1.5 SQL dialect reality

**Works well enough for app SQL (Beta claims, Apr 2025 + ongoing):** schemas, sequences, most types, arrays, COPY, `pg_catalog` (partial), users/roles (partial), `ON CONFLICT`, `RETURNING`, stored procedures, domains, TOAST-like large values, etc.

**Known gaps (Beta list; treat as moving target toward 1.0):** historically included triggers (later), collations ignored, CTEs (`WITH`), multi-table updates, window functions, custom operators/indexes/aggregates, extensions (PostGIS etc.), some `psql` meta-commands. Always re-check [SQL support docs](https://www.doltgres.com/docs/reference/sql-support/) before betting a feature.

**nudox INDEX SQL surface** (from `registry/schema`): simple upserts, text CHECKs, JSON/bytea, indexes, single-row state machines — **well inside** the safe subset. Graph multi-hop recursive CTEs would be the risky subset if used for expansion.

### 1.6 Dolt vs Doltgres differences that matter for product

| Axis | Dolt | Doltgres |
|---|---|---|
| Wire | MySQL | Postgres |
| CLI (`dolt pull` etc.) | Full Git-like CLI | **No CLI** — SQL only (`SELECT DOLT_PULL()`) |
| DoltHub / DoltLab remotes | First-class | **Not supported** — file/S3/GCS/OCI/HTTP custom remotes only (README limitation as of 0.57) |
| Production claim | 1.0 | Beta → 1.0 imminent (Aug 6 2026) |
| Shared storage/VC | Same prolly + commit graph | Same |
| Rust clients | MySQL drivers | **Postgres drivers** (`sqlx` postgres feature already in tree) |

If nudox already abandoned Postgres dialect for SQLite INDEX (plan 11), **Dolt’s MySQL dialect is not a free win either** — both Dolt and Doltgres force dialect re-entry. Doltgres is preferred *only if* you want Postgres ecosystem + `sqlx` postgres path.

---

## 2. Project health 2026

### 2.1 Company and license

| Item | Detail |
|---|---|
| Company | **DoltHub, Inc.** (also products: Dolt, Doltgres, DoltLite, Dumbo, Hosted Dolt, DoltHub, DoltLab, Dolt Workbench) |
| License | **Apache-2.0** (doltgresql LICENSE) |
| Language | **Go** (~93%) + Postgres parser artifacts |
| Community | Discord-heavy; blog-weekly cadence; issue-driven 24h bug-fix culture marketed heavily |
| Hosted $ | Hosted Dolt (managed), DoltHub Pro (private DBs), DoltLab (self-host collab) |

### 2.2 Release status (as of 2026-07-16)

| Milestone | Date | Note |
|---|---|---|
| Alpha announced | 2023-11 | Show HN era |
| Beta | **2025-04-16** | “ready for production use case” with bugs/missing features |
| State of Doltgres | 2025-10 | Progress update |
| 1.0 date announced | **2026-06-26** | Target **2026-08-06** (DoltHub 8-year anniversary) |
| 99% SQL Logic Tests | **2026-07-10** | Correctness milestone for 1.0 |
| Latest tag | **v0.57.0** | **2026-07-10** ([releases](https://github.com/dolthub/doltgresql/releases)) |
| Stars | ~2k | Smaller than Dolt; active commits (4.5k+ on main) |

**1.0 bar (vendor-defined):**
1. Correctness ≈ 99% SQL Logic Test compliance (**claimed hit Jul 10 2026**)
2. Storage format stability (no 1.x migrations)
3. Performance within **~3× Postgres** sysbench (was ~3.3× mid-2026; earlier Beta README tables showed ~5.2× overall for 0.50.0)
4. Tool/ORM compatibility (Prisma, Django, SQLAlchemy, Laravel, Knex, etc. blogged)

### 2.3 Production readiness assessment (independent of marketing)

| Signal | Assessment |
|---|---|
| Dolt (MySQL twin) | Production-proven; use as confidence prior for storage/VC |
| Doltgres Beta | Usable for greenfield apps that tolerate missing Postgres edges; **not** drop-in for every Postgres app |
| Pre-1.0 storage | **Risk of format migrations** until 1.0 guarantee locks |
| Replication / backup | Explicitly “work in progress” in README limitations; 1.0 punch list includes replication + incremental/auto GC |
| Remotes to DoltHub | **Missing for Doltgres** — collab product surface incomplete |
| Auth completeness | Users exist (SCRAM-256); privilege model incomplete historically (table-level subset; role groups delayed) |
| Extensions | None — no PostGIS, no pgvector in-engine |

**For nudox:** Shipping INDEX on Doltgres **before Aug 2026 1.0** would be a needless platform bet. After 1.0, still compete against SQLite+Litestream’s simpler ops model (11).

### 2.4 Performance snapshot

Published README benchmarks for **0.50.0** (stale relative to 0.57 but directionally useful):

- Overall mean latency multiplier vs Postgres: **~5.2×**
- Point select: ~0.52 ms vs ~0.14 ms (~3.7×)
- Some scans/joins much worse (index join scan ~13× in that table)

1.0 goal: **within 3×** on key sysbench measures; mid-2026 blog claimed ~3.3× progress.

**nudox INDEX hot path:** package ensure, parse_status upsert, outbox append, symbol batch upsert — latency budget is tens of ms to seconds of pipeline work, **not** sub-ms. Even 5× Postgres is often fine *if* correctness/ops work. The issue is **complexity tax**, not raw latency vs SQLite on NVMe.

### 2.5 Rust / client ecosystem

| Client path | Fit for nudox |
|---|---|
| **sqlx** `runtime-tokio-rustls` + `postgres` | Direct — already used historically for INDEX; Doltgres is wire-compatible target |
| **tokio-postgres / deadpool-postgres** | Works in principle |
| **Sea-query** `PostgresQueryBuilder` | Works; plan 11 was moving *away* toward SQLite builder |
| **Native Rust embed of Doltgres** | **No** — Go server binary only |
| **Dolt CLI from Rust** | Doltgres has no CLI; spawn `doltgres` process or connect over TCP |
| **DoltLite C API via rusqlite-like binding** | Possible long-term for desktop; **no first-party Rust crate** yet (Python/Node/Swift/Go examples exist) |
| **Dolt MySQL protocol** | Would need `sqlx` mysql feature path — second dialect in monorepo |

**Ergonomics ranking for Rust monorepo:** SQLite (rusqlite/sqlx) ≫ Postgres-wire remote (sqlx) ≫ spawning Go server ≫ CGO to DoltLite.

### 2.6 Known limitations checklist (2026-07)

1. No Dolt-style CLI (SQL-only VC ops).
2. No DoltHub/DoltLab remote push (custom remotes only).
3. Backup/replication incomplete relative to Postgres/Dolt 1.0 maturity.
4. No GSSAPI; auth historically SCRAM-password only.
5. No extension ecosystem (pgvector, PostGIS, pg_trgm, …).
6. Incomplete privilege model vs full Postgres (improving; verify at pin version).
7. No true row-level security (RLS) product story like Postgres RLS.
8. GC required for history growth (`dolt_gc`); auto/incremental GC landing toward 1.0.
9. Concurrent writers: **branch-level** isolation is a strength; single-branch write concurrency still transactional — not magic multi-master.
10. Not embedded — always a **server process** (contrast DoltLite / SQLite).

---

## 3. Users / organizations / multi-tenancy (load-bearing)

This section answers: *Can Doltgres/DoltHub replace application-level multi-tenant concepts (users, orgs, packages, visibility, federation) the way TerminusDB databases/users/roles/teams or GitHub orgs do?*

### 3.1 Three different “multi-tenant” surfaces (do not conflate)

| Surface | What it is | Comparable to |
|---|---|---|
| **A. SQL server auth** | Postgres-style users, passwords, GRANT/REVOKE on tables | Postgres roles — **not** product multi-tenancy |
| **B. Dolt branch permissions** | Who may write which branch on a running server | Git branch protection lite |
| **C. DoltHub orgs / collab** | Hosted GitHub-for-data: orgs, teams, DB collaborators, PRs | GitHub orgs/repos — **product**, not the engine |
| **D. Application multi-tenancy** | nudox `owner_tenant`, visibility, package ACLs, federation | Must live in **app tables** or external IdP |

**Doltgres does not give you (D).** It gives you (A) + VC, optionally (B) from Dolt heritage, and (C) only if you adopt DoltHub *and* (today) mostly with **Dolt**, not Doltgres remotes.

### 3.2 Doltgres SQL auth (A)

From [Doltgres Now Supports Users](https://www.dolthub.com/blog/2024-11-07-doltgres-supports-users/) (2024-11, still the conceptual model):

- Default superuser `postgres` / password (change immediately).
- `CREATE USER` / `ALTER USER` / SCRAM-256 authentication.
- `GRANT` / `REVOKE` on tables — initially **SELECT/INSERT/UPDATE/DELETE/TRUNCATE** only; ownership tracked but incomplete vs full Postgres.
- **Users/privileges live outside the commit graph** (server-local), intentionally:
  - Clones do not copy credentials (security).
  - Historical `AS OF` queries still subject to **current** grants (admin can revoke historical reads).
- Branch-local table ownership quirks when same table name created on different branches.

**Missing / delayed relative to full Postgres multi-tenant SaaS:**
- Role groups (as of that post).
- Full object privilege matrix.
- Postgres **RLS policies**.
- Cert/mTLS auth (planned later).

**For nudox:** INDEX is a **service backend**, not a multi-tenant SQL product exposed to end customers. App auth (users/orgs/JWT) should **not** map 1:1 onto Doltgres SQL users. Use a single app role + app-level ACL tables (`owner_tenant`, `visibility`) as today.

### 3.3 Branch permissions (B)

Dolt SQL servers support **branch permissions** (write/admin per branch); readers generally can read any branch. This is useful for:

- Agent/isolation workflows (each agent writes its own branch, merge with review).
- Human data-editing workflows with protected `main`.

This is **not** package-tenant isolation. Tenants sharing one DB with branch-per-tenant is an anti-pattern for thousands of tenants (branch explosion + shared catalog noise).

### 3.4 DoltHub organizations (C)

From [DoltHub Permissions docs](https://www.dolthub.com/docs/concepts/dolthub/permissions/):

| Concept | Behavior |
|---|---|
| User-owned DB | Collaborators via settings form |
| Org-owned DB | Org members get read on private DBs; owners get admin |
| Teams | Teams as collaborators on org DBs (granular admin/write) |
| Public vs private | Public by default; private needs Pro; private free tier size limits historically (1GB/mo free private then paid) |
| Write levels | Write (edit data, merge PRs, push) vs Admin (settings + collabs) |
| PR workflow | Issues + pull requests + optional CI on PRs |

**GitHub-like?** Yes for **data repositories**. **Not** a full SaaS multi-tenant identity platform (no SAML org directory for *your* product’s customers; no package visibility model).

**Doltgres gap:** README still states **cannot push to DoltHub or DoltLab** — only custom remotes (file, S3+Dynamo, GCS, OCI, HTTP remotesapi). Hosted Dolt may run Doltgres instances, but the **DoltHub social/collab remote graph** is Dolt-first.

### 3.5 TerminusDB comparison (users / DBs / teams)

From plan 07 and Terminus product model (v12):

| Capability | TerminusDB | Doltgres engine | DoltHub product |
|---|---|---|---|
| Database as isolation unit | **First-class** (DB-per-package recommended) | Database-per-catalog exists, but VC is the star feature | Repo/DB as unit |
| Users / roles | Server users, capabilities, teams | SQL users + incomplete GRANTs | Org members, teams, collab roles |
| Document history | Layer stack per commit; semantic JSON diff | Row/table history; SQL diffs | PR UI on tables |
| Graph query | WOQL + GraphQL | SQL only | SQL workbench |
| Branch model | Named commits | Git-complete | PR merge onto branches |
| Multi-tenant SaaS for *your* customers | Still app-level for end-users | Still app-level | Hosts *data* collab, not your IdP |
| Rust story | HTTP + community schema crates | Postgres wire | N/A |

**Conclusion on multi-tenancy:**  
Neither Terminus nor Doltgres replaces nudox’s **application** multi-tenancy (`Tenant`, package visibility, federation).  
Terminus is stronger as **per-package versioned document graph isolation**.  
DoltHub is stronger as **human data collaboration** (if you publish data products).  
Doltgres alone is a **database engine** with VC — multi-tenant product features are **not** first-class beyond SQL auth and optional branch ACL.

### 3.6 Hosted vs self-hosted for nudox

| Mode | Pros | Cons for nudox |
|---|---|---|
| Self-host `doltgres` on k8s | Control, CAS co-location, no DoltHub dependency | Ops of Go server + remotes + GC + not-yet-1.0 risks |
| Hosted Dolt (managed) | Less ops | Data gravity, cost, less control over filesystem layout next to S3 CAS workers |
| DoltHub remotes | Nice PR UX | **Doltgres remote gap**; public-by-default culture mismatch with private package INDEX |
| SQLite+Litestream (11) | Minimal ops, already designed | No built-in table versioning |

**Recommendation:** If ever adopted, **self-host** next to INDEX workers; do not design around DoltHub as identity plane.

---

## 4. As metadata store for INDEX

### 4.1 Can it replace SQLite/Postgres for packages, parse_status, symbols, outbox, watermarks?

**Technically yes** for the relational subset:

- Wire-compatible with existing `sqlx` postgres code paths (pre-SQLite migration).
- Tables in `registry/schema/mod.rs` are vanilla: uuid/text/json/bytea/timestamptz/bigserial-like sequences.
- Upserts + CHECK domains map cleanly.

**Should it?** Compare to plan 11 baseline:

| Concern | SQLite+Litestream | Doltgres |
|---|---|---|
| Single-writer INDEX | Natural | Possible but heavier |
| Continuous backup | Litestream → S3 | Remotes to S3/Dynamo or file; GC discipline |
| HA | Restore RTO; or rqlite fallback | Dolt replication protocol (maturing) |
| Ops surface | One process + sidecar | Go server + VC + GC + remotesapi |
| Version every parse_status flip | Useless / harmful history bloat | Must **ignore** tables or auto-commit carefully |
| Schema migrations | App migrations | Also versioned schema commits (nice *and* footgun) |
| Latency | Excellent local | 3–5× Postgres class; still OK |
| Maturity | SQLite eternal | Beta→1.0 |

### 4.2 Which tables should be versioned vs unversioned?

Dolt versions the whole database by default. Operational tables **should not** accumulate history:

| Table | Version? | Why |
|---|---|---|
| `packages` | Optional soft | Identity mostly immutable; ownership updates rare |
| `parse_status` | **No / ignore** | High churn state machine; history better as metrics |
| `jobs` | **No** | Ephemeral queue (and queue is leaving DB → k8s per 11) |
| `outbox` | **No** | Monotonic seq; watermark consumers; GC deletes |
| `sink_watermarks` | **No** | Cursor rows |
| `symbols` | **Yes, if using Dolt for generations** | Perfect for commit-per-generation diffs |
| Future `edges` / `monikers` | **Yes** | Lineage + graph adjacency |
| Future `tenants` / ACL | Soft | App multi-tenancy; not VC-critical |

Dolt supports patterns like **`dolt_ignore`** (DoltLite documents this clearly; Dolt family has ignore concepts) and discipline: only `dolt_add` tables you intend to commit. For server INDEX, you’d run **explicit commit boundaries** at generation publish, not autocommit every status flip.

### 4.3 Branch-per-tenant? Branch-per-generation?

| Pattern | Fit | Verdict |
|---|---|---|
| **Branch-per-tenant** | Isolates writes; merge to main for publish | **Poor** at scale (10k–500k tenants/packages): branch metadata explosion; cross-tenant queries harder; not how INDEX works today |
| **Branch-per-generation** | Each package version is a branch | **Poor** — generations are a DAG of content, not parallel editing branches; use **commits on `main`** (or package DB) instead |
| **Commit-per-generation on `main`** | `dolt_commit` after symbols upsert for generation G | **Good** if using Dolt for symbol history |
| **DB-per-package** | Isolation like Terminus recommendation | Possible; ops heavy vs single INDEX DB + `package_id` columns |
| **Single INDEX DB, no VC on ops tables** | Current model | **Best** for metadata SoT |

### 4.4 Diff of symbol tables between package versions

This is the **killer feature** relative to plain SQLite:

```sql
-- After committing generation G1 and G2 as two commits on main:
SELECT * FROM dolt_diff_symbols
 WHERE from_commit = :g1_commit AND to_commit = :g2_commit;

-- Or hash-addressed refs if you tag commits with content hash:
SELECT * FROM dolt_diff_summary(:g1, :g2);
```

Cell-level to_/from_ for `fq_name`, `kind`, presence/absence — free **cross-generation symbol delta** without custom tables.

**Caveats:**
- Diff is **relational**, not semantic IR-level (no understanding of moniker renames unless you store monikers as stable keys).
- Symbol **identity** across generations remains the hard problem (lineage monikers) — Dolt does not invent monikers; it only diffs whatever primary keys you chose.
- If `symbols.id` is a global UUID that changes every generation, diffs show delete+insert noise. Prefer **stable moniker / FQ key** as PK for versioned symbol tables (see §10).

### 4.5 Scale (rows, concurrent writers)

| Workload | Expected | Doltgres fit |
|---|---|---|
| Packages rows | 10^5–10^7 | Fine |
| Symbols rows (hot projection) | 10^7–10^9 possible if all packages | **Partition** by package or only store hot; full universe better in CAS+search |
| Concurrent INDEX writers | Prefer 1 writer process | Aligns with plan 11 single-writer; Dolt branches allow parallel *if* you redesign |
| Outbox throughput | Bursty batches | Works; don’t version it |
| History depth | Unlimited commits | Need GC policy; structural sharing helps |

Dolt markets agentic multi-writer via **branch isolation** — interesting for multi-agent doc generation, less relevant for centralized package INDEX.

### 4.6 INDEX verdict

**Do not replace SQLite+Litestream INDEX SoT with Doltgres** for operational metadata.  
**Optional:** secondary versioned DB (or branch) that **only** receives commit-gated symbol/edge projections for packages that want generation diffs / time-travel SQL — overlapping Terminus hot-tier *partially*.

> **Updated 2026-07-16 reframing:** The optional secondary DB is now specified as **A′ package metadata + B′ symbol catalog** (§9R), not a vague dual-role INDEX. Ops tables stay on SQLite; graph stays Terminus/IR.

---

## 5. As symbol lookup + graph store

### 5.1 Can relational + versioned tables host the graph?

nudox `GraphStore` (plan 20) is mostly **depth-1**:

| Method | Hop | SQL shape |
|---|---|---|
| `get_occurrences(item)` | reverse 1 | `SELECT from_id FROM edges WHERE kind='Occurrence' AND to_id=$1` |
| `get_references(item)` | reverse 1 | same, `kind='Reference'` |
| `are_related(from,to)` | 0–1 | PK lookup on `(from_id,to_id)` or adjacency |
| `outgoing_edges` / expand BFS | multi | app-loop or recursive CTE |

**Yes**, adjacency tables can implement cold/hot graph for depth-1 **as well as** cold IR projection does. Versioning adds:

- Multi-generation history **in SQL** without Terminus document layers.
- Fast commit-to-commit edge diffs.

### 5.2 Multi-generation history without Terminus

| Need | Terminus | Doltgres adjacency | Cold IR (20) |
|---|---|---|---|
| Symbol docs with rich JSON | Document store | JSON columns or normalize | Re-project from IR |
| Incremental publish (Δ only) | Delta layers | Upsert + commit | New IR blob |
| Time travel | Commit layers | `AS OF` / history tables | Keep old IR hashes in CAS |
| Semantic JSON patch | Strong | Weak (row/cell) | App-level |
| Path queries | WOQL/GraphQL | Recursive SQL / app BFS | App BFS |
| Multi-hop ergonomics | Better | Awkward | App-controlled |
| Scale to full universe | Hot only | Hot only (same economics) | Cold default |

**Without Terminus**, you can still serve multi-gen if:
1. Each generation’s symbols/edges are committed (or stored as rows with `generation` column — which INDEX `symbols` already has), and  
2. CAS holds IR SoT forever.

Dolt’s advantage over “symbols.generation column” is **automatic structural sharing + diff tools + branches**. A generation column + SQLite already answers “symbols at generation G” with a filter — **without** VC engine complexity. Diff between generations needs either two filters + app set-diff (already `resolution::diff` pure helper) or `dolt_diff`.

**Honest comparison:** For *current* GraphStore surface, **generation-column SQLite + CAS** already covers multi-gen serving. Dolt shines when humans/agents need **branch/merge/PR** on the catalog itself.

### 5.3 What’s awkward in SQL for multi-hop graph?

1. **Recursive CTEs** for paths — verbose, easy to blow limits, planner sensitive; Doltgres CTE support historically incomplete (verify at pin).
2. **Variable-length paths with filters** — WOQL/GraphQL express better.
3. **Heterogeneous documents** (Symbol with nested implements/member_of) — either wide JSON or many edge kinds; Terminus documents map closer to compiler `GraphCorpus`.
4. **Cross-package federation** — still app-orchestrated (same for Terminus; plan 07 uses cross-pkg index DB).
5. **Lineage monikers** — graph of identity over time is a separate schema problem; neither engine solves name binding.

### 5.4 Symbol catalog / monikers / lineage edges

Sketch (see §10):

```
monikers(stable_key PK, package_id, kind, ...)
symbol_generations(stable_key, generation, symbol_id, fq_name, ...)
lineage_edges(from_key, to_key, relation, confidence, evidence_gen)
edges(from_id, to_id, kind, generation)  -- GraphStore adjacency
```

Dolt commits after each generation publish → `dolt_diff_symbol_generations` for renames if keys stable.

### 5.5 Graph store verdict

| Role | Use Doltgres? |
|---|---|
| Implement depth-1 GraphStore | Yes capable; **not better** than SQLite/Terminus/cold IR for serving |
| Replace Terminus hot tier | **No** — lose document/WOQL ergonomics; maturity risk |
| Generation diffs for catalog QA | **Yes, niche strong** |
| Full multi-hop knowledge graph product | Prefer Terminus or specialized graph DB |

---

## 6. Versioning superpowers for nudox

### 6.1 Commit-gated pipeline

Idealized flow:

```
parse package → write IR to CAS (content hash H)
             → upsert relational projection (symbols/edges) in working set
             → dolt_add('symbols','edges',...)
             → dolt_commit('-m', 'package P generation H')
             → tag or note commit hash next to parse_status.content_hash
             → append outbox (non-versioned) for search/vector sinks
```

**Gating property:** Serving tier only reads committed HEAD (or tagged commit). Broken mid-upsert states never visible if readers use committed refs.

**SQLite equivalent:** transactional upsert + only then mark `parse_status=stored`. You already have atomicity without Git semantics.

**Dolt extra:** ability to open a **branch** for experimental reparse, merge after validation — useful for pipeline migration A/B, not required for v1 INDEX.

### 6.2 Package generation as Dolt commit

| Approach | Pros | Cons |
|---|---|---|
| One global `main`, commit = batch of packages | Simple | Coarse; hard to bisect one package |
| One global `main`, commit = single package generation | Fine-grained log | Very high commit rate; log noise |
| DB-per-package, commit-per-generation | Clean history per package | Many DBs; ops |
| Don’t use Dolt; `generation` column + CAS | Matches current design | Manual diffs |

**Practical:** If experimenting, **one Dolt DB**, commit message includes `package_id` + content hash, or store mapping table `generation_commits(package_id, content_hash, commit_hash)`.

### 6.3 Lineage via merge ancestry

Merge ancestry ≠ symbol lineage.  
Git merge graph tracks **how database states combined**, not “symbol A renamed to B.”  
Symbol lineage still needs explicit moniker edges (compiler / linker responsibility).

Where merge ancestry *does* help:
- Merging a **reindex branch** that rewrote many packages.
- Multi-agent concurrent catalog edits with conflict detection on same symbol PK.

### 6.4 Time-travel queries

```sql
SELECT * FROM symbols AS OF 'abc123...';
SELECT * FROM dolt_history_symbols WHERE id = $1;
```

Useful for debugging “what did INDEX serve last Tuesday?”  
Also solvable with: immutable CAS IR + `parse_status` history table / audit log — lighter.

### 6.5 Fit with CAS (hashes only in Dolt, blobs in S3)

**Excellent architectural fit** (same as INDEX design today):

| Store | Contents |
|---|---|
| S3 / disk CAS | IR, source archives, occurrence sets, optional graph postcard blobs |
| Dolt / SQLite INDEX | Package identity, content hashes, symbol projection, edges, outbox |
| Search (Tantivy) | Text index over projection |
| Vectors | Embeddings store |
| Terminus (hot) | Document graph materialization |

Dolt does **not** replace CAS. Putting large IR JSON into Dolt tables would bloat prolly storage and slow merges — **anti-pattern**.

### 6.6 Rebases

`dolt_rebase` exists in family products — useful for cleaning agent commit noise. **Dangerous** on a multi-writer INDEX if remotes already pulled commits (history rewrite). Prefer append-only commits in production INDEX.

---

## 7. Desktop / embedded

### 7.1 Can Doltgres run embedded in a GPUI desktop app?

**No (practical).** Doltgres is a **Go server process**. Options:

1. Bundle `doltgres` binary, spawn localhost, connect via Postgres driver — heavy (memory, port, lifecycle).
2. Remote-only INDEX (desktop talks to cloud) — matches many SaaS designs; loses offline.
3. Keep **SQLite REGISTRY** local (current plan).

### 7.2 Dolt (MySQL) embedded?

Still a server or CLI process; not a linkable library for Rust/GPUI cleanly.

### 7.3 DoltLite (the actual embedded candidate)

[dolthub/doltlite](https://github.com/dolthub/doltlite) (2026, alpha, Apache-2.0):

- SQLite fork: **prolly pager** under `btree.h`.
- Full SQLite C API (`doltlite.h`) — drop-in relink path.
- VC via SQL functions (`dolt_commit`, `dolt_branch`, …) and virtual tables.
- Bindings: Python, Node, Swift, Android, WASM; Go example; **no official Rust crate** yet — would wrap C API like rusqlite.
- Remotes (file/HTTP) exist but **auth insecure by default** (warning in README).
- Latest observed: **v0.11.32** (2026-07-15); ~163 stars — early.

**REGISTRY fit:** Conceptually closest to “versioned local sqlite.”  
**Reality check:** Alpha; REGISTRY already has sqlite + disk CAS + tantivy. Adding DoltLite means CGO, divergence from stock SQLite, dual engine complexity. Only justified if local **branch/merge of user-edited docs/catalog** becomes a product feature.

### 7.4 Replication local ← remote INDEX

| Path | Notes |
|---|---|
| Doltgres remotes (S3/file/HTTP) | Pull subset DBs; Doltgres-to-Doltgres |
| Litestream restore of SQLite INDEX | Plan 11 style; not live replica |
| App-level sync of packages user cares about | Likely true REGISTRY model regardless of engine |
| DoltLite clone of a published package DB | Possible future “download versioned package catalog” |

**Desktop verdict:** Stay on **sqlite REGISTRY**. Watch **DoltLite** only if local VC UX becomes a product bet. Do not embed Doltgres.

---

## 8. Comparison matrix

Axes: multi-tenant users/orgs · graph queries · version lineage · ops complexity · Rust ergonomics · license · hot-path latency · offline

Scoring: **●●●** strong · **●●** adequate · **●** weak · **○** poor/absent

| Axis | Doltgres | TerminusDB | SQLite+Litestream | Plain Postgres |
|---|---|---|---|---|
| **Multi-tenant users/orgs (app)** | ● app tables + SQL users | ●● DB isolation + server roles | ● app tables | ●● RLS optional + app |
| **Collab orgs (GitHub-like)** | ●○ (DoltHub incomplete for DG) | ○ | ○ | ○ |
| **Graph queries** | ● SQL/recursive | **●●●** WOQL/GQL | ● SQL/app BFS | ●● SQL + extensions |
| **Version lineage (data)** | **●●●** Git-complete | **●●●** layers/commits | ● app/CAS hashes | ● temporal tables / app |
| **Generation diffs** | **●●●** dolt_diff | **●●●** document diff | ●● app set-diff | ●● app |
| **Ops complexity** | ●● (server+GC+VC) | ● (Prolog server+layers) | **●●●** simple | ●● mature but heavier HA |
| **Rust ergonomics** | ●● sqlx pg | ● HTTP + community | **●●●** rusqlite/sqlx | ●● sqlx pg |
| **License** | Apache-2.0 | Apache-2.0 | Public domain + Litestream | Postgres license |
| **Hot-path latency** | ●● (3–5× PG) | ●● (HTTP+graph) | **●●●** local | **●●●** |
| **Offline / embedded** | ○ server | ● embed crate stale | **●●●** | ○ |
| **Maturity 2026-07** | ●● Beta→1.0 | ●●● v12.0.6 | **●●●** | **●●●** |
| **CAS coexistence** | **●●●** | **●●●** | **●●●** | **●●●** |
| **Matches nudox GraphStore depth-1** | ●● | **●●●** | ●● | ●● |
| **Commit-gated agent workflows** | **●●●** | ●● | ● | ● |

### 8.1 Narrative comparisons

**Doltgres vs TerminusDB**  
Same high-level pitch (“versioned knowledge”), different data model. Terminus = document-graph + query languages. Doltgres = relational Git. For *hottest packages with rich symbol docs and path queries*, Terminus still wins. For *tabular catalog diffs and SQL ecosystem*, Doltgres wins. They are **not drop-in substitutes**.

**Doltgres vs SQLite+Litestream**  
SQLite wins INDEX SoT on ops, embeddability, and monorepo simplicity. Doltgres wins only if **Git-on-tables** is a product requirement.

**Doltgres vs plain Postgres**  
Postgres wins maturity, extensions, RLS, ecosystem, latency. Doltgres wins built-in commit graph / branch / merge / remote sync of *database state*. You can fake weak versioning on Postgres with audit tables — you cannot fake efficient structural sharing + O(d) diff without prolly-like storage.

---

## 9. Verdict options (A–D)

> **Updated 2026-07-16 reframing:** Options **(A)** and **(B)** below described Doltgres as optional INDEX dual-role / ops-adjacent storage. That framing is **superseded** by **§9R (A′/B′)**: package-metadata + symbol **catalog** only; SQLite remains ops SoT; Terminus/IR remain graph. Keep this section as historical reasoning for rejections of C and D (still valid) and for why pure ops-INDEX-on-Doltgres fails.

### (A) INDEX metadata only

**Pros**
- Postgres wire reuses prior sqlx skill.
- Remotes as backup story (S3) alternative to Litestream.
- Schema versioning with commit history.

**Cons**
- Versioning operational tables is mostly waste and risk.
- Heavier than SQLite+Litestream for single-writer INDEX.
- Pre/post-1.0 risk window.
- No gain on multi-tenant product model.

**When to pick:** You must keep Postgres dialect for other reasons *and* want DB remotes — still weaker than Hosted Postgres + app versioning.

### (B) metadata + symbols (versioned symbols subsystem)

**Pros**
- Commit-per-generation + `dolt_diff` on symbols/edges.
- Time travel for catalog debugging.
- Branch for reindex experiments.
- CAS remains blob SoT.

**Cons**
- Second database beside INDEX or forced dual role.
- Does not remove need for search/vector/CAS.
- Symbol identity design still on you.
- Overlaps Terminus *and* cold IR without matching either’s best trait.

**When to pick:** Product wants **SQL-native generation browser / PR-on-catalog** for internal tooling or customer data products.

### (C) full graph replacing Terminus

**Pros**
- One less specialty store.
- Unified SQL access for symbols+edges+meta.
- Git workflow familiar to engineers.

**Cons**
- Weak multi-hop / document model vs Terminus.
- GraphStore expand becomes recursive SQL or app BFS (you already have BFS).
- Hot-tier admission economics unchanged — still cannot hold whole universe.
- Maturity and missing extensions (no graph-specific indexing beyond B-tree/prolly).
- Loses WOQL/GraphQL path and semantic JSON diff.

**Verdict:** **Reject as Terminus replacement.** Cold IR (20) + hot Terminus (07) remains cleaner separation.

### (D) reject / niche only

**Pros**
- Aligns with plan 11 (SQLite INDEX) and 07/20 (Terminus + cold IR).
- Avoids Beta/1.0 transition risk.
- Team focus on monikers, IR, search.

**Cons**
- Forgoes best-in-class table-level VC if that becomes strategic.
- DoltLite local-first opportunity deferred.

**Original recommended default (2026-07-16 morning):** **(D) with a thin watchlist** — re-evaluate **(B)** after Doltgres **1.0**.

**Updated 2026-07-16 reframing default:** **Reject (C) and (D-as-ops-SoT)** unchanged; **promote (A′)** and **evaluate (B′)** as a **catalog side-service**, not INDEX replacement. Detail and schema: **§9R**. Decision log: §13.

### Recommended role if any

> **Updated 2026-07-16 reframing** — superseded list below kept for history; authoritative list in §9R.8.

1. **Do not** block SQLite INDEX migration on Doltgres.  
2. **Do not** demote Terminus solely because Doltgres exists.  
3. **Optional spike (post-1.0):** versioned `symbols`+`edges` DB for one ecosystem’s hottest packages; measure diff UX vs Terminus document history vs pure CAS.  
4. **Desktop:** ignore Doltgres; optionally track **DoltLite** for experimental local VC — not production REGISTRY 2026.  
5. **Multi-tenancy:** remain application-level; never outsource org/package ACLs to DoltHub.

---

## 9R. Reframed role: package metadata + symbol catalog

**Status:** Authoritative scope clarification **2026-07-16** (same calendar day as initial deep dive). Prior sections §1–§8 and §10–§22 remain valid research; verdicts that treated Doltgres as Terminus/SQLite full replacement or as ops INDEX SoT are **superseded** where they conflict with this section.

### 9R.1 Intent (what we are *not* doing)

| Explicit non-goal | Why |
|---|---|
| Free multi-tenant org model | App ACL / JWT / tenants tables remain SoT (§3) |
| Terminus graph replacement | Multi-hop docs, WOQL/GraphQL, hot document layers stay 07 |
| IR / treesitter blob store | CAS + re-parse (plan 13); Dolt holds **catalog rows + hashes**, not AST trees |
| Jobs / outbox / parse_status SoT | SQLite+Litestream INDEX (plan 11) |
| Desktop embed of Doltgres | Server only; REGISTRY stays sqlite |

**Goal:** Use Dolt’s **commit graph + O(d) table diffs + branches** where the *data product* is a **versioned registry catalog** — analogous to “crates.io package page + a symbol index,” not a graph database and not an operational queue.

### 9R.2 What “crates.io page” metadata is (per ecosystem)

Live INDEX `packages` today is **identity only** (`language`, `origin_token`, names, `version_canonical`, visibility, owner tenant, toolchain JSON) — see `workspace/registry/schema/mod.rs`. Rich “package page” fields are not first-class tables yet (`SearchFacets` on `parse_status` is a thin derived keyword/quality blob). A′ fills that gap as a **crawlable, versioned metadata catalog**.

| Surface | Rust (crates.io API) | npm | PyPI | Go (pkg.go.dev / module) | Maven Central | NuGet |
|---|---|---|---|---|---|---|
| **Identity** | crate `name` / `id` | package name (scoped) | project name | module path | `groupId:artifactId` | package id |
| **Description / summary** | `description` | `description` | `summary` + long description | synopsis / README lead | POM `description` | `description` |
| **License** | per-version `license` (SPDX-ish) | `license` / `licenses` | classifiers + `license` | LICENSE detection | POM licenses | licenseExpression / URL |
| **Links** | documentation, homepage, repository | homepage, repository, bugs | home_page, project_urls | repo from go.mod / discovery | scm, url | projectUrl |
| **Owners / maintainers** | owners API (users/teams) | maintainers | author / maintainer | module owners (limited) | developers | authors |
| **Versions** | version list, yanked, created_at | versions + dist-tags | releases | module versions | artifact versions | versions |
| **Downloads** | `downloads`, `recent_downloads`, per-version | npm downloads API | pypistats / bigquery | proxy counters (limited) | Central stats (limited) | downloadCount |
| **Keywords / categories** | keywords, categories | keywords | keywords, classifiers / trove | — / badges | — | tags |
| **Features / extras** | Cargo `features` map | (exports/optional deps) | extras / optional deps | build tags | classifiers | dependency groups |
| **README** | raw README per version | readme field | description HTML/md | README from repo | — | readme |
| **Deps (summary)** | dependencies API | dependencies/dev/peer/optional | requires_dist | go.mod require | POM deps | dependency groups |
| **Yank / deprecate** | `yanked` | deprecated | yank (limited) | retract (rare) | — | unlisted / deprecated |

**crates.io API field anchors** (`crates_io_api::Crate` / `Version` / owners endpoints — representative):

- Crate-level: `name`, `description`, `documentation`, `homepage`, `repository`, `downloads`, `recent_downloads`, `categories`, `keywords`, `max_version`, `max_stable_version`, `created_at`, `updated_at`, `links`
- Version-level: version number, `license`, `downloads`, `features`, `yanked`, `crate_size`, `rust_version`, `published_by`, created/updated, readme path
- Related: owners list, reverse dependencies, category slugs

**Design rule:** Normalize into **shared columns** + `ecosystem_extra JSONB` for registry-specific keys rather than seven incompatible schemas.

### 9R.3 Schema sketch — package metadata (A′)

```sql
-- Package identity across ecosystems (catalog key; may dual-write from INDEX packages)
CREATE TABLE package_meta (
  package_key       TEXT PRIMARY KEY,          -- e.g. 'rust:crates-io:serde' or uuid
  ecosystem         TEXT NOT NULL,             -- rust|npm|python|go|maven|nuget|...
  registry          TEXT NOT NULL,             -- crates-io|npmjs|pypi|proxy.golang.org|...
  name_canonical    TEXT NOT NULL,
  name_display      TEXT NOT NULL,
  description       TEXT,
  license_spdx      TEXT,                      -- best-effort normalized
  homepage_url      TEXT,
  repository_url    TEXT,
  documentation_url TEXT,
  downloads_total   BIGINT,
  downloads_recent  BIGINT,
  keywords          TEXT[] ,                   -- or JSONB array if PG-array gaps
  categories        TEXT[],
  max_version       TEXT,
  max_stable_version TEXT,
  created_at_origin TIMESTAMPTZ,
  updated_at_origin TIMESTAMPTZ,
  crawled_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
  ecosystem_extra   JSONB NOT NULL DEFAULT '{}',
  UNIQUE (ecosystem, registry, name_canonical)
);

CREATE TABLE package_versions (
  package_key       TEXT NOT NULL REFERENCES package_meta(package_key),
  version           TEXT NOT NULL,
  version_canonical TEXT NOT NULL,
  yanked            BOOLEAN NOT NULL DEFAULT false,
  deprecated        BOOLEAN NOT NULL DEFAULT false,
  license_spdx      TEXT,
  published_at      TIMESTAMPTZ,
  downloads         BIGINT,
  crate_size_bytes  BIGINT,
  published_by      TEXT,
  rustc_msrv        TEXT,                      -- null outside cargo
  features_json     JSONB,                     -- cargo features / extras map
  deps_summary      JSONB,                     -- optional compact dep list
  content_hash      BYTEA,                     -- CAS key of tarball/IR if known
  PRIMARY KEY (package_key, version_canonical)
);

CREATE TABLE package_owners (
  package_key       TEXT NOT NULL REFERENCES package_meta(package_key),
  owner_kind        TEXT NOT NULL,             -- user|team|org|email|publisher
  owner_id          TEXT NOT NULL,             -- registry-native id/login
  owner_display     TEXT,
  role              TEXT,                      -- owner|maintainer|author
  PRIMARY KEY (package_key, owner_kind, owner_id)
);

CREATE TABLE package_links (
  package_key       TEXT NOT NULL REFERENCES package_meta(package_key),
  link_kind         TEXT NOT NULL,             -- homepage|repo|docs|bugs|funding|...
  url               TEXT NOT NULL,
  PRIMARY KEY (package_key, link_kind, url)
);

CREATE TABLE package_readme (
  package_key       TEXT NOT NULL,
  version_canonical TEXT NOT NULL,
  format            TEXT NOT NULL DEFAULT 'markdown',  -- markdown|html|rst|text
  body              TEXT,                      -- or body_cas_ref if large
  body_cas_ref      TEXT,                      -- prefer CAS when large
  PRIMARY KEY (package_key, version_canonical)
);

-- Optional: time-series download snapshots (high churn → commit rarely / ignore)
CREATE TABLE package_download_snapshots (
  package_key       TEXT NOT NULL,
  captured_at       TIMESTAMPTZ NOT NULL,
  downloads_total   BIGINT,
  downloads_recent  BIGINT,
  PRIMARY KEY (package_key, captured_at)
);
```

**Commit discipline (A′):**

| Event | Stage + commit? |
|---|---|
| Full ecosystem registry crawl batch | **Yes** — one commit (or one commit per ecosystem branch merge) |
| Single package page refresh | Optional batch; avoid commit-per-download-counter tick |
| Download counter-only updates | Prefer **not** versioning every scrape — snapshot table unstaged or daily commit |
| README/license/owner change | **Yes** with package meta |

### 9R.4 Schema sketch — symbol / treesitter catalog (B′)

Not full parse trees (those re-parse from source/CAS per plan 13). Catalog **rows** only:

```sql
CREATE TABLE symbol_catalog (
  moniker           TEXT NOT NULL,             -- stable string moniker (scheme:id)
  package_key       TEXT NOT NULL,
  version_canonical TEXT NOT NULL,             -- registry version *or* generation tag
  generation        BYTEA,                     -- CAS content hash when from INDEX parse
  kind              TEXT NOT NULL,             -- Function|Struct|Module|... (SymbolKind)
  path              TEXT,                      -- file path within package
  fq_name           TEXT NOT NULL,
  sig_hash          BYTEA,                     -- hash of normalized signature / shape
  span_start_line   INT,
  span_end_line     INT,
  file_content_hash BYTEA,                     -- hash of file bytes (not full tree)
  occurrence_count  INT,                       -- optional summary, not full posting list
  extra             JSONB NOT NULL DEFAULT '{}',
  PRIMARY KEY (moniker, package_key, version_canonical)
);
CREATE INDEX idx_symcat_pkg_ver ON symbol_catalog (package_key, version_canonical);
CREATE INDEX idx_symcat_fq ON symbol_catalog (package_key, fq_name);
CREATE INDEX idx_symcat_path ON symbol_catalog (package_key, version_canonical, path);

-- Optional: coarse occurrence summary (not Tantivy postings)
CREATE TABLE symbol_occurrence_summary (
  moniker           TEXT NOT NULL,
  package_key       TEXT NOT NULL,
  version_canonical TEXT NOT NULL,
  ref_count         INT NOT NULL DEFAULT 0,
  def_count         INT NOT NULL DEFAULT 0,
  PRIMARY KEY (moniker, package_key, version_canonical)
);

CREATE TABLE catalog_generation_commits (
  package_key       TEXT NOT NULL,
  version_canonical TEXT NOT NULL,
  generation        BYTEA,
  commit_hash       CHAR(32) NOT NULL,         -- dolt commit
  committed_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  message           TEXT,
  PRIMARY KEY (package_key, version_canonical)
);
```

**Explicitly out of B′ tables:** full treesitter CST/AST, full occurrence posting lists, IR JSON documents, vector embeddings, multi-hop edge materializations for product graph UX.

### 9R.5 How Dolt commits / branches help

| VC feature | Catalog use |
|---|---|
| **Branch per ecosystem crawl** | `branch crates-io-crawl`, `branch npm-crawl` → merge to `main` after validation |
| **Commit per registry snapshot** | Daily/hourly crawl: `dolt_commit('-m', 'crates-io snapshot 2026-07-16T12:00Z')` |
| **Commit per package version parse** | After symbol projection for `serde@1.0.210`: commit + row in `catalog_generation_commits` |
| **`dolt_diff` symbol appearance** | “What symbols appeared between 1.0.0 and 1.1.0?” → diff `symbol_catalog` between two commits (stable moniker PK) |
| **`AS OF` time travel** | “Metadata as of date” for package pages / compliance freeze |
| **PR / branch review** | Internal: reindex pipeline branch; public: optional data PR on published mirror |
| **Structural sharing** | Unchanged package/symbol rows across snapshots cost little |

```sql
-- Symbols added between two published versions (via mapped commits)
SELECT * FROM dolt_diff_symbol_catalog
 WHERE from_commit = :commit_1_0_0
   AND to_commit   = :commit_1_1_0
   AND diff_type = 'added';

-- Package metadata as of a freeze tag
SELECT * FROM package_meta AS OF 'tag:crates-io-2026-07-01'
 WHERE name_canonical = 'serde';
```

**Moniker PK discipline is load-bearing:** random UUID per parse ⇒ diff = delete+insert noise. Prefer stable moniker strings (plan 19).

### 9R.6 vs SQLite INDEX `symbols` (plan 11)

Live INDEX `symbols`: `(id UUID PK, package_id, fq_name, kind, generation bytea)` — serving projection for operational path, co-located with `parse_status` / outbox for atomic “stored + project + fan-out.”

| Concern | SQLite INDEX symbols (11) | Doltgres catalog B′ |
|---|---|---|
| Hot point lookup p99 | **Wins** (local WAL, dual pool) | Worse latency class (remote Go server, 3–5× PG) |
| Atomic with outbox / parse_status | **Wins** (single-file txn) | Cross-DB dual-write |
| Embed in desktop REGISTRY | **Wins** | Cannot embed Doltgres |
| Ops simplicity | **Wins** | Server + GC + remotes |
| `dolt_diff` / `AS OF` / branch reindex | App set-diff + CAS only | **Wins** |
| Human/agent PR on catalog data | Weak | **Wins** (especially if mirrored to DoltHub via Dolt) |
| Full-universe serving projection | Appropriate for INDEX slice | Same economics — hot/partial only |
| History of crawl corrections | Manual | First-class commits |

**Rule of thumb:** SQLite = **ops + hot serving SoT**. Doltgres = **versioned catalog / analytics / freeze / diff UX**. Dual-write or async project from parse pipeline; never block outbox on Dolt commit unless catalog is the product path.

### 9R.7 vs Terminus

| Concern | Doltgres A′/B′ | Terminus (07) | Cold IR (20) |
|---|---|---|---|
| Package page relational fields | **Primary** | Awkward (docs, not tables) | N/A |
| Symbol catalog rows + SQL diff | **Primary** | Possible as docs; weaker tabular diff | Re-project + app diff |
| Graph edges / path queries | **Out of scope** (optional thin adjacency only if needed) | **Hot primary** | Cold default |
| Multi-gen rich symbol *documents* | JSON columns weak | **Strong** | IR SoT |
| Lineage monikers | Store edges as tables if needed; engine ≠ binder | Document layers | App |

**Partition:** metadata + symbol **relational catalog** → Doltgres (evaluate); **graph edges / path queries** → Terminus-hot or cold IR; **blobs** → CAS.

### 9R.8 Hosting topology for A′/B′

| Mode | Fit |
|---|---|
| **Self-hosted Doltgres** next to INDEX workers | **Default for private / full catalog** — sqlx postgres, S3 custom remotes, CAS co-located |
| **Hosted Dolt (managed Doltgres)** | Ops offload if data-residency OK; still no free multi-tenant product model |
| **DoltHub public read-only mirrors** | Attractive for **public package metadata** collaboration/PRs — but Doltgres **cannot push to DoltHub yet** (as of 0.57). Options: (1) wait for remote parity; (2) mirror via **Dolt (MySQL)** export of public subset; (3) publish snapshot dumps without DoltHub UX |
| **Desktop** | **No Doltgres embed.** Export catalog snapshots → sqlite REGISTRY (subset of `package_meta` + symbols user cares about) via app sync (plan 14) |

```
Registry crawlers / parse pipeline
        │
        ├─► SQLite INDEX (ops SoT): packages id, parse_status, jobs→k8s, outbox, serving symbols
        │
        ├─► Doltgres CATALOG (A′/B′): package_meta*, symbol_catalog, commits/branches
        │         └─ optional: push public subset → DoltHub (when remotes allow) or static dump
        │
        ├─► CAS: IR, readme large bodies, source archives
        ├─► Terminus hot / cold IR: graph
        └─► Desktop REGISTRY sqlite: offline snapshots from INDEX/catalog APIs
```

### 9R.9 Updated verdict matrix

| Option | Verdict | Notes |
|---|---|---|
| **A′ Package metadata catalog (versioned)** | **Evaluate strongly** | Best product fit for Dolt VC; crawl snapshots + `AS OF` + ecosystem branches |
| **B′ Symbol catalog with generation history via commits** | **Evaluate** | Strong if monikers stable; complements not replaces INDEX `symbols` |
| **A Ops INDEX metadata only** | **Reject as SoT** | Unchanged — anti-feature on outbox/watermarks/status churn |
| **B Dual-role INDEX+symbols on Doltgres** | **Reject** | Prefer side-car catalog; SQLite remains ops |
| **C Full graph** | **Still reject** | Terminus + cold IR |
| **D Core SoT for jobs/outbox** | **Still reject** | Plan 11 |

**Recommended role (reframed):** Post-**1.0**, spike **A′** on one ecosystem (crates.io), optionally **B′** for that ecosystem’s parsed packages; keep SQLite INDEX migration unblocked; keep Terminus plan 07.

### 9R.10 Implementation sketch if A′/B′ adopted

1. **Do not** put Doltgres on the INDEX write transaction critical path for outbox.
2. **Catalog service** process (or INDEX worker side-effect):
   - On registry crawl: upsert `package_meta*` → stage → `dolt_commit` on ecosystem branch / main.
   - On parse `stored`: project moniker rows into `symbol_catalog` (from IR/symbols) → commit; record `catalog_generation_commits`.
3. **Dual-write modes:**
   - **Async (preferred):** outbox sink `catalog` drains to Doltgres (at-least-once; idempotent upsert by PK).
   - **Sync dual-write:** only if product requires catalog commit hash in API response — higher failure coupling.
4. **Read path:** package page API may read Doltgres (or read replica/snapshot); symbol hot path may keep reading SQLite INDEX until catalog proves latency.
5. **Export job:** periodic `SELECT …` dump or `dolt_push` remote → desktop-oriented sqlite snapshot builder.
6. **GC:** retain tags for weekly freezes; `dolt_gc` policy on crawl history depth.

### 9R.11 Risks (A′/B′-specific)

| Risk | Severity | Mitigation |
|---|---|---|
| Doltgres still **beta** / pre-1.0 format | High until Aug 2026 | Gate spike on 1.0; CAS + SQLite remain authoritative for ops |
| **No DoltHub remote parity** for Doltgres | Medium for public mirror UX | Custom remotes; optional Dolt MySQL mirror; or wait |
| **Symbol table size** (full universe × versions) | High | Hot packages only; partition by ecosystem/package; CAS for cold |
| **Write amplification** (commit per version × huge symbol sets) | Medium | Batch commits; don’t commit download-only ticks; structural sharing helps row-stable data |
| Dual-write drift vs SQLite INDEX | Medium | Idempotent sink; reconcile job; generation_commits watermark |
| Unstable monikers ⇒ useless diffs | High for B′ | Plan 19 moniker RFC before B′ production |
| README bodies bloat prolly storage | Medium | `body_cas_ref` for large readmes |
| Team distraction from moniker/IR | Medium | Spike time-boxed; A′ first (metadata-only) |

### 9R.12 Open questions (reframed)

1. Is **package page history** (`AS OF`, crawl audit) a user-facing product or internal-only?
2. Single Doltgres DB for all ecosystems vs **DB/branch-per-ecosystem**?
3. Public mirror: wait for Doltgres→DoltHub remotes vs **Dolt MySQL** export path?
4. B′: version_canonical (registry) vs generation (content hash) as primary versioning axis — or both?
5. Should INDEX sqlite `symbols` eventually **thin** to pointers into catalog, or remain full serving projection?
6. Download counters: store latest-only in SQLite INDEX facets vs versioned snapshots in Dolt?
7. SLA: is catalog allowed to lag INDEX by minutes (async sink) or must be sync?

---

## 10. Concrete schema sketches (if promising)

> **Updated 2026-07-16 reframing:** Prefer the **A′/B′ catalog schemas in §9R.3–9R.4** for new design work. The sketches below remain valid for a combined moniker/edges experiment; they are **not** an invitation to host outbox/jobs on Doltgres.

Design principles:
- CAS holds blobs; Dolt holds **hashes + relational projection**.
- Prefer **stable moniker keys** for versioned rows so diffs are meaningful.
- Keep **ops tables unversioned** (ignore / never stage).
- One commit ≈ one package generation publish (or batch with explicit mapping).

### 10.1 Core identity (can live in non-VC INDEX)

```sql
-- Application multi-tenancy (not DoltHub orgs)
CREATE TABLE tenants (
  id            UUID PRIMARY KEY,
  kind          TEXT NOT NULL CHECK (kind IN ('individual','enterprise')),
  name          TEXT NOT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE packages (
  id                 UUID PRIMARY KEY,
  language           TEXT NOT NULL,
  origin_token       TEXT NOT NULL,
  name_canonical     TEXT NOT NULL,
  name_original      TEXT NOT NULL,
  version_canonical  TEXT NOT NULL,
  visibility         TEXT NOT NULL CHECK (visibility IN ('public','private','unlisted')),
  owner_tenant       UUID REFERENCES tenants(id),
  owner_kind         TEXT NOT NULL,
  toolchain          JSONB NOT NULL DEFAULT '{}',
  created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at         TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX idx_packages_coords
  ON packages (language, origin_token, name_canonical, version_canonical);
```

### 10.2 Ops tables (never `dolt_add`)

```sql
CREATE TABLE parse_status (
  package_id    UUID PRIMARY KEY REFERENCES packages(id),
  state         TEXT NOT NULL,
  phase         TEXT,
  content_hash  BYTEA,              -- 32-byte CAS key
  attempts      INT NOT NULL DEFAULT 0,
  failure       JSONB,
  facets        JSONB,
  needed        BOOLEAN NOT NULL DEFAULT false,
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE outbox (
  seq           BIGSERIAL PRIMARY KEY,
  package_id    UUID NOT NULL REFERENCES packages(id),
  generation    BYTEA NOT NULL,
  sink_kind     TEXT NOT NULL,
  created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
  UNIQUE (package_id, generation, sink_kind)
);

CREATE TABLE sink_watermarks (
  sink_kind     TEXT PRIMARY KEY,
  last_seq      BIGINT NOT NULL DEFAULT 0,
  updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

### 10.3 Versioned symbol projection (stage + commit)

```sql
-- Stable identity across generations (moniker), NOT random UUID per parse
CREATE TABLE monikers (
  moniker_id    UUID PRIMARY KEY,          -- stable
  package_id    UUID NOT NULL REFERENCES packages(id),
  scheme        TEXT NOT NULL,             -- e.g. 'tsc', 'rustc', 'python'
  identifier    TEXT NOT NULL,             -- scheme-specific
  UNIQUE (package_id, scheme, identifier)
);

-- Per-generation materialization (rows rewritten each gen; prolly shares unchanged)
CREATE TABLE symbols (
  moniker_id    UUID NOT NULL REFERENCES monikers(moniker_id),
  package_id    UUID NOT NULL REFERENCES packages(id),
  symbol_id     UUID NOT NULL,             -- serving UUID if needed
  fq_name       TEXT NOT NULL,
  kind          TEXT NOT NULL,
  generation    BYTEA NOT NULL,            -- CAS content hash
  ir_blob_ref   TEXT,                      -- optional pointer into CAS
  PRIMARY KEY (moniker_id, generation)
);
CREATE INDEX idx_symbols_package_gen ON symbols (package_id, generation);
CREATE INDEX idx_symbols_fq ON symbols (package_id, fq_name);

-- GraphStore adjacency (depth-1 optimized)
CREATE TABLE edges (
  package_id    UUID NOT NULL,
  generation    BYTEA NOT NULL,
  from_moniker  UUID NOT NULL,
  to_moniker    UUID NOT NULL,
  kind          TEXT NOT NULL CHECK (kind IN (
                  'Member','Reference','Occurrence','Implements','Extends','ReExport'
                )),
  PRIMARY KEY (package_id, generation, from_moniker, to_moniker, kind)
);
CREATE INDEX idx_edges_to ON edges (package_id, generation, kind, to_moniker);
CREATE INDEX idx_edges_from ON edges (package_id, generation, kind, from_moniker);

-- Explicit lineage (rename / split / merge across generations)
CREATE TABLE lineage_edges (
  from_moniker  UUID NOT NULL,
  to_moniker    UUID NOT NULL,
  relation      TEXT NOT NULL CHECK (relation IN ('same','renamed','split','merged','replaced')),
  at_generation BYTEA NOT NULL,
  confidence    REAL NOT NULL DEFAULT 1.0,
  PRIMARY KEY (from_moniker, to_moniker, relation, at_generation)
);
```

### 10.4 Commit mapping

```sql
CREATE TABLE generation_commits (
  package_id    UUID NOT NULL,
  generation    BYTEA NOT NULL,
  commit_hash   CHAR(32) NOT NULL,         -- dolt commit
  committed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
  message       TEXT,
  PRIMARY KEY (package_id, generation)
);
```

### 10.5 Publish transaction sketch (pseudocode)

```text
BEGIN;
  -- working set: replace projection for this package+generation
  DELETE FROM symbols WHERE package_id=$P AND generation=$H;  -- or upsert strategy
  INSERT INTO symbols ...;
  DELETE FROM edges WHERE package_id=$P AND generation=$H;
  INSERT INTO edges ...;
  -- monikers/lineage upserts
COMMIT;  -- SQL txn only; still unstaged for Dolt

SELECT dolt_add('monikers','symbols','edges','lineage_edges');
SELECT dolt_commit('-m', format('pkg %s gen %s', $P, hex($H)));
-- record mapping
INSERT INTO generation_commits(...);

-- ops path (non-versioned)
UPDATE parse_status SET state='stored', content_hash=$H, ...;
INSERT INTO outbox (...);
```

### 10.6 Query sketches

**Symbols at generation (filter, no VC needed):**
```sql
SELECT * FROM symbols WHERE package_id = $1 AND generation = $2;
```

**Diff two generations via Dolt (if each gen is a commit):**
```sql
SELECT s.commit_hash AS c1 FROM generation_commits s
 WHERE s.package_id=$1 AND s.generation=$g1;
-- similarly c2
SELECT * FROM dolt_diff_symbols WHERE from_commit = $c1 AND to_commit = $c2;
```

**GraphStore get_references:**
```sql
SELECT from_moniker FROM edges
 WHERE package_id=$1 AND generation=$2 AND kind='Reference' AND to_moniker=$3;
```

**BFS expand:** application loop calling `outgoing_edges` — do **not** require recursive CTE.

**Time travel whole catalog:**
```sql
SELECT * FROM symbols AS OF $commit_hash WHERE package_id = $1;
```

### 10.7 What not to put in Dolt

- Raw IR trees, source archives, occurrence posting lists at full fidelity → **CAS**
- Tantivy segments / vectors → their engines
- Per-request session graphs → memory / ephemeral sqlite
- End-user passwords / session tokens → auth service

---

## 11. Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| Pre-1.0 storage format churn | High until Aug 2026 | Wait 1.0; pin version; backup CAS independently |
| Missing Postgres features (CTE, windows, …) | Medium | Stick to simple SQL; re-check support matrix |
| DoltHub remote gap for Doltgres | Medium for collab dreams | Custom S3 remotes; don’t design on DoltHub |
| History bloat / GC pain | Medium | Ignore ops tables; periodic `dolt_gc`; retention policy |
| Wrong PK ⇒ useless diffs | High for (B) | Stable monikers before adopting VC symbols |
| Dual dialect (sqlite REGISTRY + doltgres INDEX) | Medium | Prefer one dialect family; avoid without strong need |
| Team distraction from moniker/IR hard problems | High | Treat as edge-tech watchlist, not roadmap driver |
| Assuming multi-tenant orgs “solved” | High | App ACL remains SoT |
| Embedding Doltgres in desktop | High fail | Use sqlite; maybe DoltLite later |
| Replacing Terminus prematurely | High | Keep 07/20 tiering |

---

## 12. Open questions

1. **Does any product surface require human PR review on symbol tables?** If no → Dolt’s killer workflow is unused.  
2. **Post-1.0 (2026-08+): actual Hosted Doltgres + remotes stability?** Re-measure.  
3. **Will Doltgres gain DoltHub remote parity?** Track README limitations.  
4. **DoltLite Rust bindings quality?** Needed before REGISTRY experiments.  
5. **Moniker stability roadmap** — without it, VC diffs are noise.  
6. **Is generation-column + CAS + `resolution::diff` enough?** Likely yes for GraphStore.  
7. **Legal/compliance:** Apache-2.0 is fine; Hosted Dolt data residency if used.  
8. **Concurrent multi-region INDEX writers** — does branch-per-region ever beat single-writer SQLite? Unlikely near-term.

---

## 13. Suggested decision log (for humans)

| Date | Decision | Owner |
|---|---|---|
| 2026-07-16 | Research complete: **niche/watchlist**, not INDEX SoT, not Terminus replacement | research |
| **2026-07-16 reframing** | Scope narrowed to **A′ package metadata catalog** + **B′ symbol catalog**; reject ops SoT (D) and full graph (C); SQLite INDEX + Terminus/IR unchanged | research / product intent |
| TBD post-1.0 | Spike **A′** (crates.io metadata crawl → Doltgres commits) yes/no | eng+product |
| TBD post-A′ | Spike **B′** symbol catalog diffs for one ecosystem | eng |
| TBD | DoltLite desktop spike only if local branch/merge UX funded | desktop |
| TBD | Public DoltHub mirror path once Doltgres remotes exist (or Dolt export) | eng |

---

## 14. Source appendix (URLs)

| Topic | URL |
|---|---|
| Doltgres repo | https://github.com/dolthub/doltgresql |
| Latest releases | https://github.com/dolthub/doltgresql/releases |
| Docs intro | https://www.doltgres.com/docs/introduction/ |
| Beta launch | https://www.dolthub.com/blog/2025-04-16-doltgres-goes-beta/ |
| 1.0 announcement | https://www.dolthub.com/blog/2026-06-26-doltgres-1-0-coming-this-fall/ |
| 99% SQLLogic | https://www.dolthub.com/blog/2026-07-10-doltgres-99-percent-sql-logic-tests/ |
| Storage / prolly | https://www.dolthub.com/docs/architecture/storage-engine/ |
| Remotes | https://www.doltgres.com/docs/reference/version-control/remotes/ |
| Users/auth blog | https://www.dolthub.com/blog/2024-11-07-doltgres-supports-users/ |
| DoltHub permissions | https://www.dolthub.com/docs/concepts/dolthub/permissions/ |
| Branch permissions | https://www.dolthub.com/docs/sql-reference/server/branch-permissions/ |
| Product home | https://doltgres.com/ / https://www.dolthub.com/ |
| DoltLite | https://github.com/dolthub/doltlite |
| Concurrency blog | https://www.dolthub.com/blog/2026-02-17-dolt-concurrency/ |
| Roadmap | https://www.dolthub.com/docs/other/roadmap/ |

---

## 15. Alignment with existing nudox plans (no rewrites)

| Plan | Interaction with this research |
|---|---|
| **11 SQLite INDEX** | **Remains primary recommendation** for INDEX **ops** SoT. Doltgres A′/B′ is optional **catalog side-store**; see plan 11 addendum “Relationship to Doltgres”. |
| **07 Terminus tiering** | **Remains hot-graph recommendation.** Doltgres is not a Terminus substitute; relational catalog ≠ graph edges. |
| **20 Graph-over-IR** | **Remains cold default.** Depth-1 GraphStore does not need Dolt. |
| **13 Storage / re-parse** | Full treesitter trees stay re-parse/CAS; B′ stores catalog rows + file hashes only. |
| **19 Symbol moniker RFC** | Prerequisite for B′ diffs to be meaningful. |
| **14 Client sync** | Desktop gets catalog via snapshots/API → sqlite REGISTRY, not Doltgres embed. |
| **Resolution / monikers** | Prerequisite for any versioned-symbol strategy; harder problem than storage engine choice. |

---

## 16. Final one-page summary

Doltgres is **Git for Postgres tables**: prolly-tree storage, commit graph, branch/merge/diff/push, Postgres wire. As of **2026-07-16** it is **Beta v0.57.0**, with **1.0 targeted 2026-08-06**, **99% SQL logic tests** claimed, performance ~**3× Postgres** goal, **Apache-2.0**, built by **DoltHub**. It shares Dolt’s storage/VC core but lacks Dolt’s CLI and **cannot yet use DoltHub/DoltLab remotes**.

For nudox, the load-bearing multi-tenant model (users/orgs/packages/visibility) is **application data**, not something Doltgres or even DoltHub fully replaces. Terminus still wins hot document-graph; SQLite+Litestream still wins INDEX **ops**; CAS still holds IR.

**Updated 2026-07-16 reframing:** The promising role is **not** “replace INDEX/Terminus,” but a **versioned package-metadata + symbol catalog** (A′/B′, §9R): crates.io-page fields across ecosystems, moniker catalog rows, branch-per-crawl / commit-per-snapshot, `dolt_diff` for symbol appearance between versions, `AS OF` metadata freezes. Still **reject** full graph (C) and jobs/outbox SoT (D). Desktop: export snapshots to sqlite REGISTRY. Public mirrors: interesting once remotes allow (or via Dolt export).

**Use Doltgres if** you want commit-gated, branchable, O(d)-diffable **relational package/symbol catalogs**. **Do not** use it as operational INDEX SoT or Terminus replacement. Spike A′ post-1.0; B′ after monikers stabilize.

---

---

## 17. Operational playbooks (if ever adopted)

### 17.1 Deploy topology (self-hosted INDEX side-car, not SoT)

```
k8s StatefulSet: doltgres (1 primary writer)
  volume: PVC for .doltgres data dir
  service: ClusterIP :5432
  optional: remotesapi port for clone/pull from workers
app: registry INDEX process → sqlx postgres → doltgres
CAS: S3 (unchanged)
backup: dolt_push to aws://[dynamo:s3]/nudox-index-backup
       OR filesystem snapshot of PVC + remote push
GC: CronJob weekly SELECT dolt_gc(); after retention policy
```

This is **heavier** than Litestream sidecar on SQLite. Only justify if VC features are used weekly by humans/agents.

### 17.2 What to commit vs never commit (runbook)

| Event | Stage tables? | Commit? |
|---|---|---|
| parse_status flip unindexed→progressing | No | No |
| job lease update | No | No |
| outbox append | No | No |
| package ensure (identity row) | Optional | Optional batch daily |
| symbol/edge projection for generation H | **Yes** | **Yes** — one commit |
| lineage moniker updates | Yes | Same commit as symbols |
| admin ACL change | Optional | Yes if audit wants history |

### 17.3 Failure modes

| Failure | Effect | Recovery |
|---|---|---|
| Crash mid-SQL txn | No partial SQL rows | Restart; retry generation |
| Crash after SQL commit, before dolt_commit | Working set dirty | `dolt_status`; commit or reset --hard |
| Crash after dolt_commit, before outbox | Catalog versioned but sinks lag | Reconcile job scans generation_commits vs outbox |
| Remote push lag | Backup RPO grows | Alert on push age |
| GC too aggressive | Lost unreferenced history | Retention tags on important commits |
| Merge conflict on symbol PK | Publish blocked | Resolve via dolt_conflicts_* or regenerate from CAS |

### 17.4 Observability metrics to add

- `dolt_status` dirty table count (should be 0 outside publish window)
- commits/hour, avg rows changed/commit (`dolt_diff_stat`)
- data dir bytes; GC reclaimed bytes
- point-select p99 vs SQLite baseline
- generation publish duration (upsert + commit + outbox)

---

## 18. Worked examples mapped to nudox traits

### 18.1 Implement `GraphStore::get_references` on Doltgres

```sql
-- generation resolved from parse_status.content_hash or request param
SELECT e.from_moniker
  FROM edges e
 WHERE e.package_id = $1
   AND e.generation = $2
   AND e.kind = 'Reference'
   AND e.to_moniker = $3;
```

Maps to `Scored<SymbolId>` after moniker→symbol_id join. **No VC required** for this query — generation column suffices.

### 18.2 When VC actually helps: regression triage

Scenario: customer reports “Router docs wrong after 0.7.5.”

```sql
SELECT commit_hash FROM generation_commits
 WHERE package_id = $axum AND generation = $hash_075;

SELECT * FROM dolt_diff_symbols
 WHERE from_commit = $hash_074_commit
   AND to_commit   = $hash_075_commit
   AND (to_fq_name ILIKE '%Router%' OR from_fq_name ILIKE '%Router%');
```

Compare to cold path: load two IR blobs from CAS, run `resolution::diff` in Rust — **already available**, no Dolt. VC wins if operators live in SQL workbench without IR tooling.

### 18.3 Branch-per-reindex experiment

```sql
SELECT dolt_branch('reindex-v2-pipeline');
SELECT dolt_checkout('reindex-v2-pipeline');
-- run new parser projections for sample packages
SELECT dolt_commit('-Am', 'sample reindex');
-- compare
SELECT * FROM dolt_diff_summary('main', 'reindex-v2-pipeline');
-- merge or abandon
SELECT dolt_checkout('main');
SELECT dolt_merge('reindex-v2-pipeline'); -- or drop branch
```

This is the **strongest unique workflow** vs SQLite. Worth a spike only if pipeline migration risk is high enough to fund the platform cost.

---

## 19. Anti-patterns (explicit)

1. **Storing IR JSON documents as Dolt rows** — use CAS; Dolt is for projections.
2. **Branch-per-customer-tenant at SaaS scale** — use `owner_tenant` column + app ACL.
3. **Expecting DoltHub orgs to be nudox orgs** — different product layer.
4. **Autocommit every parse_status update** — history explosion; ignore ops tables.
5. **Using Dolt merge ancestry as symbol lineage** — encode moniker edges explicitly.
6. **Replacing cold IR graph with always-on Dolt full universe** — economics fail the same way Terminus does; hot tier only.
7. **Embedding Doltgres in GPUI** — spawn cost, memory, no library API.
8. **Choosing Doltgres only to keep Postgres dialect** — plain Postgres is better if you don’t need VC.
9. **Choosing Doltgres only because “versioned DB sounds like Terminus”** — different model; false equivalence.
10. **Blocking monorepo SQLite migration (11) on edge-tech evaluation** — independent tracks.

---

## 20. Relationship to other edge-tech folders

| Folder | Interaction |
|---|---|
| `02-iroh` | P2P/CAS transport — orthogonal; Dolt remotes ≠ Iroh |
| `03-edenfs` | Large worktree / virtual FS — orthogonal to table VC |
| `04-ipfs-and-cas` | Content addressing — **complementary**; Dolt stores hashes, IPFS/S3 store blobs |
| `05-surrounding-edge` | Catch-all — DoltLite may reappear under local-first |

Dolt’s content-addressed chunks are **not** a substitute for package IR CAS; different granularity and query model.

---

## 21. Chronology useful for decision timing

| When | Event | Action for nudox |
|---|---|---|
| 2023-11 | Doltgres alpha | Ignore |
| 2025-04 | Beta | Watch only |
| 2026-04 | DoltLite announced (alpha) | Desktop curiosity |
| 2026-06-26 | 1.0 date set (Aug 6) | Calendar review |
| **2026-07-10** | v0.57.0 + 99% SQLLogic | **This research** |
| **2026-07-16** | Research filed | Default **(D)** ops reject |
| **2026-07-16** | Scope reframed | Promote **(A′)/(B′)** catalog evaluation; keep C/D reject |
| 2026-08-06 | Planned 1.0 | Re-read release notes; **A′ spike gate** |
| 2026-H2 | Post-1.0 field reports | A′ production candidacy; B′ if monikers ready |

---

## 22. Glossary (nudox × Dolt)

| Term | Meaning here |
|---|---|
| **Prolly tree** | Content-addressed B-tree-like structure; enables O(d) diff + structural sharing |
| **Commit graph** | Merkle DAG of database roots (Git commits for tables) |
| **Generation** | nudox content hash of a package parse (CAS key); *not* a Dolt branch |
| **Moniker** | Stable symbol identity across generations |
| **INDEX** | Remote catalog/metadata service |
| **REGISTRY** | Local desktop store |
| **Hot tier** | Terminus-materialized packages with sustained graph demand |
| **Cold path** | Graph-over-IR from CAS |
| **Outbox** | Transactional fan-out cursor table — must not be VC’d |
| **Remote (Dolt)** | Push/pull target for database state (file/S3/HTTP), not git source remote |

---

*End of research document. Dense technical companion to edge-tech evaluation; no build actions implied. Identical twins: `docs/research/edge-tech/01-doltgres.md` and `01-doltgres/PLAN.md`.*
