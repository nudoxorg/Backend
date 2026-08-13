# API contract

**This document is the canonical definition of the Nudox Backend wire
contract.** The Nudox/Web documentation links here and does not restate these
schemas. If this document and any other document disagree, this one is wrong
and must be fixed — do not fork the definition.

Defined against `main` at `b388f36d` on 2026-08-13. Source of truth:
`workspace/driver/http/`. Every route below was read from
`workspace/driver/http/router.rs` and its handler in
`workspace/driver/http/handlers/` at write time.

Read the [Surfaces that return empty](#surfaces-that-return-empty) section
before you build a client. Most of the symbol plane answers `200` with nothing
in it, or `404`, and the reason is not a bug in your request.

---

## Transport

| | |
| --- | --- |
| Binary | `driver` (`workspace/driver/main.rs`) |
| Bind address | `config.serving_address`, default `127.0.0.1:8080` (`workspace/driver/config.rs:374`) |
| Scheme | plain HTTP. The binary terminates no TLS. |
| Path prefix | **none.** There is no `/api` prefix on any route. |
| Request bodies | `application/json` on every POST |
| Response bodies | `application/json`, except `POST /search` and `POST /search/semantic` which are `application/x-ndjson`, and `GET /metrics` which is Prometheus text |
| Path parameter syntax | axum 0.7 colon form (`/symbols/:id`). `driver` pins `axum = "0.7"` (`workspace/driver/Cargo.toml`); `nudox-mcp` pins `axum = "0.8"` — a different crate, a different plane. |
| Path parameter types | every `:id` is a **UUID**, parsed by `Path<uuid::Uuid>`. Numeric ids are rejected with a 400. |

Middleware, in `router()` (`workspace/driver/http/router.rs:57`):

- RED metrics through `route_layer` (`router.rs:72`, handler at `:92`) — counter
  `http_requests`, histogram `http_request_duration_seconds`, both labelled
  `http_route` / `method` / `status`. `http_route` is the matched route
  *template*, so `/symbols/:id` is one series, not one per id.
- `tower_http::trace::TraceLayer` on every route (`router.rs:73`, built at
  `:133`), emitting one `http.server.request` span per request. Bodies and
  headers are never logged.

## Authentication

**A caller sends nothing today.** No bearer token, no header, no cookie.

The vocabulary exists and is wired, but every policy predicate returns `true`:

| Type | Where | Behaviour today |
| --- | --- | --- |
| `Principal` extractor | `workspace/driver/authz.rs:70` | always succeeds, yielding `TenantId::anonymous()` |
| `AdminPrincipal` extractor | `workspace/driver/authz.rs:87` | always succeeds — **`/admin/*` is not protected** |
| `Server::authorize_read` | `workspace/driver/authz.rs:113` | mints a `ReadCap` iff `read_allowed`, which is `true` (`authz.rs:167`) |
| `Server::authorize_write` | `workspace/driver/authz.rs:130` | `write_allowed` is `true` (`authz.rs:172`) |
| `Server::authorize_admin` | `workspace/driver/authz.rs:146` | `admin_allowed` is `true` (`authz.rs:177`) |

The deny arm is reachable and returns `ForbiddenReason::ActionDenied` → `403`
(`workspace/driver/error.rs:328`), but nothing constructs it in a serving path.
`workspace/driver/main.rs:47-48` states the intent: access control happens at a
fronting proxy, and "in-process, everyone authenticated to reach us may act."

**Do not expose this binary to an untrusted network.** `POST /admin/packages/:id/rebuild`
is as open as `GET /healthz`.

## Route table

### Read plane — body limit 2 MiB

`READ_PLANE_BODY_CEILING = 2 * 1024 * 1024` (`router.rs:33`), applied at
`router.rs:60`. Routes declared in `read_plane()` (`router.rs:179`).

| Method | Path | Request body | Response | Handler |
| --- | --- | --- | --- | --- |
| POST | `/search` | `heart::query::Query`; `target` **must** be `"Symbols"` | NDJSON, `application/x-ndjson`, one `Scored<Symbol>` per line | `search::search` (`handlers/search.rs:45`) |
| POST | `/search/semantic` | same; the handler forces `mode = Semantic` | same | `search::search_semantic` (`handlers/search.rs:63`) |
| POST | `/packages/search` | `heart::query::Query` | JSON `Page<GlobalPackage>` | `search::search_packages` (`handlers/search.rs:75`) |
| POST | `/usages` | `Query` with `target: {"Usages": {"of": "F:…"}}` | JSON `Page<Usage>` | `search::usages` (`handlers/search.rs:91`) |
| POST | `/expand` | `Query`; `text` is the seed symbol UUID | JSON `Page<Symbol>` | `search::expand` (`handlers/search.rs:111`) |
| GET | `/symbols/:id` (UUID) | — | JSON `Sourced<Symbol>` | `search::get_symbol` (`handlers/search.rs:147`) |
| GET | `/sessions/:id` (UUID) | — | JSON `SessionGraph` | `search::get_session` (`handlers/search.rs:163`) |

`GET /sessions/:id` is the one read route that takes **no** `Principal`
extractor and mints no capability (`handlers/search.rs:163-166`).

### Write plane — body limit `min(limits.max_request_bytes, 64 KiB)`

`WRITE_PLANE_BODY_CEILING = 64 * 1024` (`router.rs:38`); the effective ceiling
is `min(config, 64 KiB)` (`router.rs:196`). Raising `limits.max_request_bytes`
therefore cannot widen this plane. A request that outlives
`limits.upload_timeout` (default 2 s) answers **504** with a plain-text body
(`router.rs:204`, `admission_timed_out` at `router.rs:269`).

Declared in `write_plane()` (`router.rs:195`).

| Method | Path | Request | Response |
| --- | --- | --- | --- |
| POST | `/packages` | `AddPackageDto` | `Initialized` |
| GET | `/packages/:id` (UUID) | — | `Initialized` with `enqueued: false` |
| POST | `/packages/:id/sync` | — | `Initialized`; recomputes the content hash and re-enqueues if stale |

`POST /packages` is **idempotent**: `ensure_initialized`
(`workspace/driver/coordination/initialization.rs:94`) reads the current
lifecycle state, runs the decision table, and only publishes + enqueues on
`Enqueue`. A duplicate returns the existing `PackageId` with `enqueued: false`.

### Operational plane — unbounded body, no timeout layer

Merged into the write plane's router but deliberately outside its middleware
(`router.rs:208-211`), so a slow backend can never mask its own readiness
report.

| Method | Path | Response |
| --- | --- | --- |
| GET | `/healthz` | `200`, empty body. Liveness only. (`handlers/health.rs:20`) |
| GET | `/readyz` | `HealthDto { ready, degraded }`. `200` while the definitive base serves — a degraded overlay is *reported*, not fatal; `503` once the base is down. (`handlers/health.rs:26`) |
| GET | `/metrics` | Prometheus exposition text, or `503` if the recorder is not installed. (`handlers/health.rs:44`) |

### Admin plane — same limits as write, `AdminPrincipal` extractor

Declared in `admin_plane()` (`router.rs:217`). Body ceiling and timeout are
identical to the write plane (`router.rs:218-228`).

| Method | Path | Response |
| --- | --- | --- |
| POST | `/admin/packages/:id/verify` | `VerifyResponse` — deep blob integrity audit (`handlers/admin.rs:32`) |
| POST | `/admin/packages/:id/rebuild` | re-emits fan-out intents for every derived store |

The `AdminPrincipal` extractor admits everyone (see
[Authentication](#authentication)).

### `/v1` planes

| Method | Path | Limit | Notes |
| --- | --- | --- | --- |
| POST | `/v1/compiled/lookup` | 256 KiB (`router.rs:44`, applied `:243`) | `CompiledLookupRequest` → `CompiledLookupResponse`. Handler `handlers/compiled.rs`. |
| GET | `/v1/depshards/:package/:version/manifest` | 256 KiB (`router.rs:248`, applied `:264`) | `DepshardManifestDto` |
| POST | `/v1/rerank` | 256 KiB | `RerankRequestDto` → `RerankResponseDto`, or `RerankUnavailableDto` |

`compiled_plane()` is `router.rs:240`; `vector_plane()` is `router.rs:257`.

---

## The query algebra — `heart::query::Query`

`heart::query::Query` (`workspace/heart/query.rs:264`) is **both** the domain
type and the wire type. There is no request DTO for any search surface — the
handler deserializes `Json<Query>` directly (`workspace/driver/http/dto.rs:7-11`
records why). Every field but `target` and `text` defaults, so the minimal body
is two fields.

| Field | Type | Required | Default |
| --- | --- | --- | --- |
| `target` | `Target` | **yes** | — |
| `text` | `String` | **yes** | — |
| `scope` | `Scope` | no | all-empty |
| `rank` | `RankSpecification` | no | `"Fused"` |
| `mode` | `QueryMode` | no | `"precise"` |
| `routing` | `Routing` | no | `{ quality: "parity", reach: "org", hot_packages: [] }` |
| `session` | `Guid` | no | absent |
| `at` | `AsOf` | no | absent |
| `page` | `PageSpecification` | no | `{ "limit": 30 }` |

### Exact serde spelling

This is the single easiest thing for a client to get wrong: **some of these
enums are lowercased and some are not.** The table is derived from the
`#[serde(...)]` attributes in `workspace/heart/query.rs`.

| Type | Source | `rename_all`? | Wire spelling |
| --- | --- | --- | --- |
| `Target` | `query.rs:129` | **no** | `"Packages"`, `"Symbols"`, `{"Usages":{"of":"F:rust/serde#ab12"}}` |
| `RankSpecification` | `query.rs:152` | **no** | `"Fused"`, `"TextOnly"` |
| `QueryMode` | `query.rs:167` | **yes**, lowercase | `"precise"`, `"semantic"` |
| `QualityMode` | `query.rs:183` | **yes**, lowercase | `"local"`, `"parity"`, `"premium"`, `"deep"` |
| `QueryReach` | `query.rs:207` | **yes**, lowercase | `"project"`, `"deps"`, `"org"` |
| `AsOf` | `query.rs:35` | **no** | `{"Commit":"<hex>"}` or `{"Time":1723500000000}` |
| `SymbolKind` | `workspace/heart/symbol.rs:58` | **no** | `"Function"`, `"Type"`, `"Module"`, `"Constant"`, `"Variable"`, `"Trait"`, `"Impl"`, `"Other"` |
| `Language` (in `scope.ecosystems`, and `AddPackageDto.ecosystem`) | `workspace/heart/ecosystem.rs:35` | **yes**, lowercase (`ecosystem.rs:34`) | `"rust"`, `"typescript"`, `"python"`, `"go"`, `"java"`, `"nix"`, `"csharp"`, `"cpp"` |

`Scope` (`query.rs:140`): `{ ecosystems: [Language], packages: [String],
include_withdrawn: bool }` — all default to empty/false.

`Routing` (`query.rs:227`): `quality`, `reach`, and `hot_packages`
(`[PackageId]`, package UUIDs the client claims it already serves from local
baked shards; claimed, never verified).

`PageSpecification` (`query.rs:244`): `{ limit: u32, cursor: Option<String> }`,
default limit `30` (`query.rs:252`). `cursor` is an opaque token from a previous
response's `next`.

### `StableReference`

The frozen F1 grammar, `workspace/heart/query.rs:48`, parser at `:71`:

```
F:<ecosystem>/<package>#<intro_hex>
```

- Must start with `F:`.
- `#` is the **sole** terminator, matched with `rsplit_once`, so `<package>` may
  itself contain `/` (registryless repo-slug stems).
- `<intro_hex>` must be **lowercase** hex. Uppercase is a parse error
  (`StableReferenceError::MalformedIntro`).
- No component may be empty.

It serializes as a bare JSON string (`#[serde(try_from = "String", into =
"String")]`, `query.rs:47`).

### Example bodies

Minimal:

```json
{ "target": "Symbols", "text": "HashMap::insert" }
```

Fully populated:

```json
{
  "target": "Symbols",
  "text": "parse a toml file into a struct",
  "scope": {
    "ecosystems": ["rust"],
    "packages": ["toml", "serde"],
    "include_withdrawn": false
  },
  "rank": "Fused",
  "mode": "semantic",
  "routing": {
    "quality": "premium",
    "reach": "deps",
    "hot_packages": ["4b3f1c8e-0000-4000-8000-000000000001"]
  },
  "session": "9f1d2c3b-0000-4000-8000-00000000000a",
  "at": { "Time": 1723500000000 },
  "page": { "limit": 50, "cursor": null }
}
```

A usages query:

```json
{ "target": { "Usages": { "of": "F:rust/serde#9ab31c" } }, "text": "" }
```

`/usages` validates the target shape twice — in the handler
(`handlers/search.rs:97`) and again in `Server::usages`
(`workspace/driver/coordination/search.rs:73`).

---

## Response shapes

### `Page<T>` — `workspace/heart/search.rs:7`

```json
{ "items": [ { "value": { }, "score": 0.87 } ], "next": "opaque-cursor-or-null" }
```

`items` is `Vec<Scored<T>>`, **not** `Vec<T>`. `next` is `null` on the last
page. Pagination happens only inside the engine, after ranking — no handler
touches a cursor.

### `Scored<T>` — `workspace/heart/score.rs:25`

`{ "value": T, "score": Score }`. `Score` (`score.rs:21`) is a `nutype`-wrapped
`f32` validated finite; it serializes as a bare JSON number.

### `Sourced<T>` — `workspace/heart/access/federation.rs:17`

`{ "value": T, "source": SourceId, "role": "Definitive" | "Overlay" }`. The
federation tag on a resolved symbol.

### `Symbol` — `workspace/heart/symbol.rs:21`

```json
{
  "id": "…uuid…",
  "package": "…uuid…",
  "ecosystem": "rust",
  "name": { "plain": "insert", "fully_qualified": "std::collections::HashMap::insert" },
  "kind": "Function"
}
```

`Name` is `workspace/heart/symbol.rs:14`; `SymbolKind` is `:58`.

### `GlobalPackage` — `workspace/index/lib.rs:142`

```json
{
  "id": "…uuid…",
  "package": { "coordinates": { }, "toolchain": { } },
  "state": { },
  "facets": null
}
```

`Package` is `workspace/index/lib.rs:122`; `facets` is
`Option<SearchFacets>` (`workspace/index/metadata/rich.rs:278`) and is `null`
until rich metadata extraction has run.

### `ResolutionState` — `workspace/heart/error/failure.rs:88`

An externally-tagged enum, five variants:

| Variant | JSON |
| --- | --- |
| `Unindexed { needed }` | `{"Unindexed":{"needed":true}}` |
| `Progressing(Phase)` | `{"Progressing":"Extracting"}` |
| `Stored { hash }` | `{"Stored":{"hash":"…"}}` |
| `Failed(Failure)` | `{"Failed":{…}}` |
| `DeadLettered(Failure)` | `{"DeadLettered":{…}}` |

`Phase` (`failure.rs:27`) is `Acquiring` | `Extracting` | `Compiling` |
`Emitting`. `Failure` is `failure.rs:52` and carries `attempts`, `phase`,
`message`, `cause`, `at`.

### `Initialized` — `workspace/driver/coordination/initialization.rs:17`

```json
{ "package": "…uuid…", "state": {"Unindexed":{"needed":true}}, "enqueued": true }
```

The response of all three `/packages` routes.

### `AddPackageDto` — `workspace/driver/http/dto.rs:30`

```json
{ "ecosystem": "rust", "name": "serde", "version": "1.0.219", "origin": null }
```

- `ecosystem` is a `heart::Language` token (lowercase).
- `origin` is optional; when absent, the canonical registry for that ecosystem
  is used (`dto.rs:55`): crates.io, npm public, PyPI, FlakeHub, NuGet,
  proxy.golang.org, Maven Central, and — for `cpp` — a git repository, because
  that plane is registry-less.
- When present, `origin` names a **custom registry** from
  `config.custom_registries`; an unknown name is a 400
  (`BadRequestReason::UnknownCustomRegistry`, `dto.rs:92`).

### `Usage` — `workspace/index/search/usages.rs:28`

```json
{ "within": "F:rust/serde#9ab31c", "kind": "call", "relative_span": [12, 27] }
```

`relative_span` is `(start, end)` **relative to the enclosing entry's
declaration span start**, not an absolute file offset.

### `SessionGraph` — `workspace/driver/session.rs:80`

```json
{ "nodes": ["…uuid…"], "edges": [ { "from": "…", "kind": "…", "to": "…" } ] }
```

Both are sorted sets; the type is a join-semilattice under `merge`
(`session.rs:105`), so repeated `/expand` calls against the same session
converge.

### `HealthDto` — `workspace/driver/http/dto.rs:247`

`{ "ready": bool, "degraded": [BackendKind] }`.

### `/v1` DTOs

- `CompiledLookupRequest` (`dto.rs:165`): `{ "job_keys": ["<64 lowercase hex>"] }`.
  Each key is validated at deserialize time — exactly 64 chars, lowercase hex
  only (`dto.rs:130`). Non-empty and ≤ 1024 entries, checked in the handler
  (`dto.rs:172`).
- `CompiledLookupResponse` (`dto.rs:242`): `{ "results": [CompiledLookupEntry] }`.
  A hit is `{job_key, hit:true, package, channel, tip, generation_stamp}`; a
  miss is `{job_key, hit:false}` with the optional fields elided (`dto.rs:198`).
  A backend error on one key is **downgraded to a miss**, not an error
  (`handlers/compiled.rs`, the `Err` arm).
- `DepshardManifestDto` (`dto.rs:259`): `artifact_id` and `ram_estimate` are
  absent until `status == "ready"`; `status` is `"pending"` | `"ready"` |
  `"failed"`.
- `RerankRequestDto` (`dto.rs:336`): `{query, documents, top_k}`, ≤ 256
  documents (`dto.rs:332`).
- `RerankUnavailableDto` (`workspace/driver/http/handlers/rerank.rs`):
  `{ "rerank_unavailable": true, "reason": "…" }`, returned with **status 200**.
  A client must check the field, not the status.

---

## NDJSON on `/search`

`POST /search` and `POST /search/semantic` do **not** return a `Page`. They
return `application/x-ndjson`: one JSON object per line, newline-terminated, no
enclosing array, no envelope, **no cursor**. A client that parses the body as a
single JSON document breaks on the second hit.

The serializer is `ndjson()` at `workspace/driver/http/handlers/search.rs:230`.
Each line is one `heart::Scored<Symbol>`:

```
{"value":{"id":"…","package":"…","ecosystem":"rust","name":{"plain":"insert","fully_qualified":"…"},"kind":"Function"},"score":0.91}
{"value":{…},"score":0.83}
```

A correct client:

```ts
const res = await fetch(`${base}/search`, {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ target: 'Symbols', text: query, mode: 'semantic' }),
});
if (!res.ok) throw new Error(await res.text());

const reader = res.body!.getReader();
const decoder = new TextDecoder();
let buffered = '';
const hits: Scored<Symbol>[] = [];

for (;;) {
  const { done, value } = await reader.read();
  if (done) break;
  buffered += decoder.decode(value, { stream: true });
  const lines = buffered.split('\n');
  buffered = lines.pop() ?? '';           // keep the partial line
  for (const line of lines) if (line) hits.push(JSON.parse(line));
}
if (buffered.trim()) hits.push(JSON.parse(buffered));
```

Note that the "stream" is currently cosmetic: the handler collects every hit
into a `Vec` before serializing (`handlers/search.rs:55-57`), so the whole
response is produced before the first byte is written. The framing is still
NDJSON and the parser above is still the correct one — but backpressure is not
a property this surface has today.

There is no pagination on this surface. `page.limit` is honoured; `page.cursor`
is consumed by the semantic arm (`workspace/driver/coordination/search.rs:116`)
but no `next` token is ever handed back, because NDJSON has no envelope to
carry one.

---

## Errors

Every handler returns `ServerError` (`workspace/driver/error.rs:17`), projected
by `ServerError::status` (`error.rs:317`) and rendered by `into_response`
(`error.rs:363`) as:

```json
{ "status": 400, "error": "missing field: target must be Symbols for /search" }
```

The `error` string is `ServerError::to_string()` — the top of the chain only.
The full typed source chain is logged server-side (`error.rs:365-379`), never
returned.

| Status | When | Source |
| --- | --- | --- |
| **400** | most `BadRequestReason` variants — malformed query, bad UUID, bad cursor, unknown custom registry, invalid package name/version, no valid package selectors, too many job keys, snapshot mismatch | `error.rs:320-327` |
| **400** | `UsageQueryError::UnresolvableTarget` — the `StableReference` is not well-formed | `error.rs:347` |
| **403** | `ForbiddenReason::ActionDenied`. Unreachable today (allow-all policy). | `error.rs:328` |
| **404** | `ServerError::NotFound` — an unknown package/symbol/session id | `error.rs:329` |
| **413** | `ArchiveTooLarge`, `ArchiveExceedsLimit` | `error.rs:322-323` |
| **500** | every `InternalError` variant, and any `ConfigError` | `error.rs:335` |
| **501** | `UsageQueryError::UnsupportedTarget`. Only the `Unsupported` backend produces it (`workspace/index/search/usages.rs:95`), and `driver` does not construct that backend — see below. | `error.rs:345` |
| **503** | `Connect`, `Runtime`, `Registry`, `Vector`, `Embed` — a backing store is unavailable | `error.rs:330-334` |
| **503** | `UsageQueryError::IndexUnavailable` — the route is wired but no reverse index is loaded for the scope | `error.rs:346` |
| **504** | a write- or admin-plane request outlived `limits.upload_timeout`. Plain text, not JSON — it is produced by the tower error layer, not `ServerError`. | `router.rs:269` |

`ServerError::is_retryable` (`error.rs:388`) marks exactly the 503 family as
retryable.

---

## Surfaces that return empty

These compile, answer `200` (or `404`, or `503`), and carry no data. **A client
that reads an empty result here as "nothing exists" draws a wrong conclusion.**
Every entry was read from source at write time.

| Surface | What a caller actually gets | Why | Evidence |
| --- | --- | --- | --- |
| `POST /search`, `mode: "precise"` (**the default**) | `200`, zero NDJSON lines, always | `SearchTarget::search for SourceStores` returns `futures::stream::empty()`. The replica-local tantivy *symbol* index was removed. | `workspace/driver/search/mod.rs:117` |
| `POST /search`, `mode: "semantic"` / `POST /search/semantic` | `200`, zero NDJSON lines, always | Qdrant may return symbol identities, but the hydration step calls `SourceStores::symbol_by_id`, which returns `Ok(None)` unconditionally — the catalog `symbols_proj` lookup that replaced the removed tantivy index is not wired. Every identity is dropped. | hydration at `workspace/driver/coordination/search.rs:149`; `symbol_by_id` at `workspace/driver/search/mod.rs:96-98` |
| `GET /symbols/:id` | **`404`, always** | `resolve_symbol` walks the federation calling the same `symbol_by_id`; every source answers `None`, so the handler's `ok_or(ServerError::NotFound)` always fires. | `workspace/driver/coordination/search.rs:37-48`; `handlers/search.rs:157` |
| `POST /expand` | **`404`, always** | `expand` resolves the seed symbol first, through the same dead path, before it ever reaches the neighbour walk. | `handlers/search.rs:125` |
| `POST /expand` (if the seed ever resolved) | empty `items` | `SymbolStore::related_hits` returns `Vec::new()`. The Terminus graph store is gone and the IR reverse-position index is not materialized in-process. | `workspace/driver/search/mod.rs:134` |
| `POST /usages` | **`503`, always** — `IndexUnavailable` | `Driver::assemble` constructs `ReverseIndexUsageBackend::empty()`, and an empty backend answers `IndexUnavailable` for every query. Not 501: the `Unsupported` backend that would give 501 is never constructed by `driver`. | `workspace/driver/lib.rs:339`; `workspace/index/search/usages.rs:178-179` |
| `POST /v1/rerank` with no `[rerank] endpoint` | `200 {"rerank_unavailable":true}` | By design (`§20.9` explicit-unavailability rule): a timeout or transport error also answers this way, never a 5xx. | `handlers/rerank.rs`, the `None` arm |
| `GET /v1/depshards/…` with `bakery.enabled = false` | `200`, `status: "pending"` forever | Nothing bakes the artifact. `depshards.enabled` defaults `true`, `bakery.enabled` defaults `false`. | `dto.rs:288-297`; `config.rs:277`, `:331` |
| `SinkKind::Graph` outbox fan-out | no-op | The graph/usage plane moved off Terminus onto the IR reverse-position index, which is not populated. | `workspace/driver/poll.rs:199`, `:218` |

**Net effect for a client author: there is currently no request that returns a
symbol.** `POST /packages/search` is the only search surface that can return
data, and only after a package has completed the ingest pipeline. The write
plane (`POST /packages`, `GET /packages/:id`, `POST /packages/:id/sync`) works
end to end. `/healthz`, `/readyz`, `/metrics` work.

This is a wiring gap, not a missing feature: the Qdrant search itself runs and
returns scored identities; only the identity → `Symbol` hydration is absent.

---

## Per-surface: what breaks in Web if this changes

| Change | What breaks | Blast radius |
| --- | --- | --- |
| Renaming `Target::Symbols` or making `Target` `rename_all = "lowercase"` | every `/search`, `/usages`, `/expand` request body. `Target` is currently PascalCase while `mode` is lowercase; a client that guesses uniformly is already broken. | every search call |
| Changing `/search` from NDJSON to JSON | any streaming line-parser. A client using `res.json()` today already breaks on multi-hit responses. | search UI |
| Adding a `next` cursor to `/search` | nothing breaks, but a client with no envelope-aware parser silently keeps showing page one. | search pagination |
| Changing `PageSpecification.limit` from `u32` | request serialization | all paged surfaces |
| Making `:id` non-UUID (e.g. numeric) | every `GET /packages/:id`, `/symbols/:id`, `/sessions/:id` link, and every `Initialized.package` round-trip | package detail pages, deep links |
| Renaming `AddPackageDto.ecosystem` → `language` | `POST /packages` | the add-package form |
| Changing `Initialized` to include the full `GlobalPackage` | nothing breaks (additive), but a client that re-fetches `GET /packages/:id` to get state can stop | add-package flow |
| Changing `ResolutionState` from externally-tagged to internally-tagged | every lifecycle-state render (`{"Progressing":"Compiling"}` → `{"type":"Progressing",…}`) | package status UI |
| Introducing real auth on `Principal` | **every route at once.** Today a client sends no credentials; the day the extractor enforces a token, everything 403s. | everything |
| Tightening `limits.max_request_bytes` below 64 KiB | `POST /packages` with a long custom-origin URL | add-package |
| Wiring `symbol_by_id` to the catalog | nothing breaks — surfaces that answer empty/404 start answering. A client must not treat "no results" as a stable fact. | search, symbol pages |

---

## Current desynchronization with Nudox/Web

**This section documents an open discrepancy. It is not resolved, and this
document does not declare which side is stale.**

Nudox/Web's `docs/` SvelteKit app calls a backend whose routes do not exist in
this repo. Both sides' evidence, read at write time:

| | Nudox/Web `docs/` calls | Nudox/Backend serves |
| --- | --- | --- |
| Base URL | `NUDOX_BACKEND_URL`, defaulting to `http://87.99.136.215:3001` (`docs/src/lib/server/terminus-config.ts:36-41`); `.env.example` suggests `http://127.0.0.1:3001` | `config.serving_address`, default `127.0.0.1:8080` (`workspace/driver/config.rs:374`) |
| Path prefix | `/api/…` on every package route | **no `/api` prefix exists**; `router.rs` declares none |
| List packages | `GET /api/packages` → `PackageSnapshot[]` (`docs/src/lib/server/backend.ts:87`) | **no list route at all.** `router.rs` has no `GET /packages` |
| Get package | `GET /api/packages/{id}` with `id: number` (`backend.ts:96`, `PackageSnapshot.id: number` at `backend.ts:24`) | `GET /packages/:id` where `:id` is a **UUID** (`handlers/indexing.rs:52`) |
| Add package | `POST /api/packages` with `{ language, name, version, source?, entry_point?, branch?, sync_on_add? }` (`backend.ts:100`, `NewPackagePayload` at `backend.ts:33`) | `POST /packages` with `{ ecosystem, name, version, origin? }` (`dto.rs:30`) |
| Search | `GET /search?q=…&limit=…` → `{ query, results: SearchHit[] }` (`backend.ts:110`) | `POST /search`, a `heart::query::Query` JSON body → **NDJSON stream** of `Scored<Symbol>` |
| Symbol lookup | `GET /terminus_search?q=<uri>` → `{ uri, document, kind }` (`backend.ts:114`) | **`terminus_search` appears in zero Rust source files in this repo** — verified by `grep -rn 'terminus_search' --include=*.rs`, which returns 0 matches. It appears only in four stale planning markdown files. |
| Graph | TerminusDB GraphQL at `{serverUrl}/api/graphql/{org}/{db}`, port 6363, basic auth (`docs/src/lib/server/terminus-config.ts:28`) | **no GraphQL endpoint and no TerminusDB.** See below. |
| Package shape | `PackageSnapshot { id: number, language, name, slug, source, version, branch, state: { health, sync_status, sync_phase, entry_count, document_count, vector_count, … } }` (`backend.ts:22`) | `Initialized { package: Uuid, state: ResolutionState, enqueued: bool }` — no slug, no branch, no counts |

Web's `docs/` app also carries `@graphql-codegen/*` devDependencies and a
`codegen` task; those target the TerminusDB schema, not anything this backend
serves.

**What is not known:** whether Web's `docs/` app is stale, whether it targets a
separate legacy service still running at `87.99.136.215:3001`, or whether an
adapter exists outside both repositories. The hard-coded production IP and the
non-default port `3001` are consistent with all three readings. This is
[OQ-1](open-questions.md#oq-1--which-side-of-the-backendweb-contract-is-stale)
and it must not be guessed at.

## There is no GraphQL surface

This question keeps coming back, so: **Nudox/Backend serves no GraphQL over
HTTP.** No route in `workspace/driver/http/router.rs` speaks it, and nothing in
the workspace links a GraphQL server.

The only GraphQL artifact in the repo is `crates/nudox-graph/schema.graphql` — a
**Trustfall** SDL schema, exposed over **MCP** (the `graph_schema` tool and
`crate::SCHEMA_SDL`), never over HTTP. It is also *missing from the working
tree and from git*; see
[building-and-testing.md](building-and-testing.md#the-repo-does-not-build-from-a-clean-checkout).

TerminusDB was removed. `grep -rni 'terminusdb\|postgres' --include=*.rs` finds
184 hits and every one of them is a comment or an identifier name — there is no
TerminusDB or Postgres dependency in any workspace `Cargo.toml`.

## MCP surface

Entirely separate from HTTP, on the other dependency plane. Covered in
[flows/04-local-first-mcp.md](flows/04-local-first-mcp.md).

| | |
| --- | --- |
| Bind | `LOOPBACK_BIND` = `127.0.0.1:0` — a **constant, not a setting** (`crates/nudox-mcp/src/endpoint.rs:56`). Port 0 asks the kernel for a free port. |
| Path | `MCP_PATH` = `/mcp` (`endpoint.rs:60`) |
| Start | `McpEndpoint::start(NudoxMcpServer::new(engine))` (`endpoint.rs:87`), or `start_with_token` (`:94`) |
| Discover | `endpoint.url()` (`endpoint.rs:143`) → `http://127.0.0.1:<port>/mcp` |
| Tools | `search_symbols`, `get_symbol`, `find_usages`, `list_packages`, `graph_query`, `graph_schema` (`crates/nudox-mcp/src/server.rs:146-251`) |
| Symbol key grammar | `ecosystem:name#introhex` — the tool argument *is* a `SymbolKey`, with no parallel id type |

**No binary in this repository starts the MCP endpoint.** `McpEndpoint::start`
is called only from `crates/nudox-mcp/tests/endpoint.rs`; `workspace/gui`
(`lindsey`) does not depend on `nudox-mcp` (`workspace/gui/Cargo.toml`). See
[OQ-11](open-questions.md#oq-11--what-is-meant-to-start-the-mcp-endpoint).
