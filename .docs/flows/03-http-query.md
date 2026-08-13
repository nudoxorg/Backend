# Flow: HTTP query → NDJSON

**Entry point:** `workspace/driver/http/router.rs:179` — `read_plane()`, route
`/search` at `:181` → `workspace/driver/http/handlers/search.rs:45` — `search`
**Terminates at:** an `application/x-ndjson` response body, one
`heart::Scored<Symbol>` per line
**Crosses:** HTTP → the query planner → (semantic arm only) an HTTP embeddings
endpoint and a Qdrant gRPC client

> **`POST /search` returns zero results today, in both modes.** The precise arm
> is an explicit `futures::stream::empty()`
> (`workspace/driver/search/mod.rs:117`). The semantic arm queries Qdrant for
> real, gets scored symbol identities back, and then drops every one of them,
> because the hydration step calls `SourceStores::symbol_by_id`, which returns
> `Ok(None)` unconditionally (`workspace/driver/search/mod.rs:96-98`). Neither
> arm can produce a hit. Read [What is a stub](#what-is-a-stub) before you
> conclude a query is wrong.

## Why this flow matters

This is the read path the whole product is for, and it is the surface most
likely to mislead a reader — it responds `200` with a well-formed, empty NDJSON
body, which is indistinguishable from "your query matched nothing." A client
author who does not know this will build retry logic, tune the query, and chase
a ranking bug that does not exist. Everything else on this page is downstream of
getting that fact stated first.

## The path

1. **`workspace/driver/http/router.rs:181`** — `.route("/search", post(search::search))`,
   inside `read_plane()`, under `DefaultBodyLimit::max(2 MiB)` (`router.rs:60`,
   const at `:33`), the RED metrics layer (`router.rs:72`) and the trace layer
   (`:73`).

2. **`workspace/driver/http/handlers/search.rs:45` — `search`**
   Extracts `State<Arc<Server<M>>>`, `Principal` (always succeeds, always
   anonymous), and `Json<Query>` — **`heart::query::Query` deserialized
   directly. The wire is the domain; there is no request DTO**
   (`handlers/search.rs:5-11`). Mints a `ReadCap` for `"search.symbols"`.
   Rejects any `target` other than `Target::Symbols` with a typed 400
   (`:51-53`). → `Query`

   `/search/semantic` (`handlers/search.rs:63`) is a two-line wrapper: it sets
   `query.mode = QueryMode::Semantic` and calls `search`. Same pipeline, same
   everything.

3. **`workspace/driver/http/handlers/search.rs:180` — `lower_to_symbol_search`**
   Wire `Query` → internal `Search<'static>`:
   - `mode == Semantic` → `ExecutionQuery::Abstract(AbstractQuery::NaturalLanguage(text))`.
     Only the planner may escalate an abstract query.
   - anything else → `ExecutionQuery::Literal(LiteralQuery::parse(&text)?)`,
     which validates and operator-escapes; a parse failure is a 400
     (`BadRequestReason::MalformedQuery`).
   - `scope.packages` are ecosystem-scoped names, so each bare stem is tried
     against every requested ecosystem's grammar — or against **every known
     ecosystem** when `scope.ecosystems` is empty (`:195-199`). If packages were
     requested and none resolved for any ecosystem, that is a 400
     (`NoValidPackageSelectors`, `:208-210`).
   - `page` is carried through unchanged.
   → `Search { query, filter: Filter { ecosystems, packages }, page }`

4. **`workspace/driver/coordination/search.rs:24` — `Server::search_symbols`**
   Takes the `ReadCap` as proof authorization already happened at the HTTP
   boundary. Calls the planner, branches on the plan, and wraps the resulting
   `Vec` back into a stream (`:33`).

5. **`workspace/driver/search/planner.rs:41` — `SearchPlanner::plan`**
   The only place a user-facing `SemanticGate` is minted.

   | Input | Plan |
   | --- | --- |
   | `Query::Literal(_)` | `Plan::Precise` |
   | `Query::Abstract(_)`, quota admits | `Plan::Semantic(gate)` |
   | `Query::Abstract(_)`, quota exhausted | `Plan::Precise` — **degrade down** |

   The budget is a fixed window: 64 admissions per 60 s
   (`planner.rs:22-23`), enforced by `Quota::admit` (`planner.rs:110`). A
   poisoned mutex denies rather than running unmetered (`planner.rs:113-115`).
   The degradation is one-directional by design: a literal query never silently
   escalates to semantic (`planner.rs:56-57`).

6a. **`workspace/driver/coordination/search.rs:89` — `precise_hits`** (the default arm)
   Walks the federation in precedence order, calls `SearchTarget::search` on
   each source, drains the stream, and merges overlay-first.

   **`SearchTarget::search for SourceStores` is
   `Ok(futures::stream::empty())`** (`workspace/driver/search/mod.rs:110-118`).
   The replica-local tantivy *symbol* index that used to back this arm was
   removed. `precise_hits` therefore merges N empty groups and returns an empty
   `Vec`. The federation shape is kept so a future catalog/IR name surface can
   drop in unchanged.

6b. **`workspace/driver/coordination/search.rs:106` — `semantic_hits`** (`mode: "semantic"`)
   1. Assert the query is `Abstract` — a `Literal` here is
      `InternalError::PlannerInvariantSemanticForLiteral` → 500 (`:111-114`).
   2. Decode `page.cursor` into a `heart::Cursor<_, heart::Advisory>`; a
      malformed token is a 400 (`:116-127`).
   3. For each federated source: take the planner's gate for the first source,
      and re-issue one per additional source via
      `SearchPlanner::extend_across_federation` (`planner.rs:78`) — one
      user-visible query, one audit reason, one single-use token per source.
   4. `SemanticSurface::new(&source.semantics, &self.embedder, &self.embedding_cache)`
      then `.search(gate, query, limit, after)`
      (`workspace/driver/search/semantic/mod.rs:43`). The query text is
      embedded once, cache-first, always with `EmbedRole::Query`
      (`semantic/mod.rs:116-118`) so Voyage-class models select the right
      prompt. The cache holds 65 536 entries (`workspace/driver/lib.rs:149`).
      The embedding goes to Qdrant as a vector search.
      → a stream of `Scored<SymbolId>`
   5. **Hydration** (`coordination/search.rs:149`): for each returned identity,
      `sourced.value.symbol_by_id(id)`, keeping it only if
      `request.filter.admits(&symbol)`.
   6. Merge overlay-first across sources.

7. **`workspace/driver/search/mod.rs:66` — `merge_overlay_first`**
   Groups arrive in federation precedence order (overlays first, then the
   definitive base). The first source to claim a key wins — overlay override —
   then everything sorts by `Reverse(score)` and truncates to `limit`.

8. **`workspace/driver/http/handlers/search.rs:55`** — the handler collects the
   stream with `try_collect().await` into a `Vec<Scored<Symbol>>`.

9. **`workspace/driver/http/handlers/search.rs:230` — `ndjson`**
   Serializes each item, appends `\n`, and wraps the iterator as a
   `Body::from_stream` under `content-type: application/x-ndjson`.

## Diagram

```
POST /search  {target:"Symbols", text, mode}
        │
        ▼
   search()  ── target != Symbols ──▶ 400
        │
        ▼
   lower_to_symbol_search()
        │        ├── parse error ──▶ 400
        │        └── no valid package selectors ──▶ 400
        ▼
   Search { query: Literal | Abstract, filter, page }
        │
        ▼
   SearchPlanner::plan()
        │
   ┌────┴──────────────────────────┐
   │ Literal                        │ Abstract + quota
   │ OR Abstract + quota exhausted  │
   ▼                                ▼
Plan::Precise                  Plan::Semantic(gate)
   │                                │
   ▼                                ▼
precise_hits()                 semantic_hits()
   │                                │
   │  per source:                   │  per source (gate re-issued):
   │  SearchTarget::search           │    embed(text) ─▶ HTTP /v1/embeddings
   │        │                       │    qdrant search ─▶ Scored<SymbolId>
   │        ▼                       │         │
   │  stream::empty()   ◀── DEAD    │         ▼
   │        │                       │    symbol_by_id(id) ─▶ Ok(None) ◀── DEAD
   │        ▼                       │         │
   │     Vec::new()                 │         ▼
   │                                │     nothing collected
   └───────────────┬────────────────┘
                   ▼
          merge_overlay_first()
                   │
                   ▼
            try_collect() ─▶ ndjson() ─▶ 200 application/x-ndjson
                                          (zero lines)
```

## What is a stub

Every dead hop on this path, with its evidence:

| Hop | Behaviour | Evidence |
| --- | --- | --- |
| `SearchTarget::search for SourceStores<M>` | `Ok(futures::stream::empty())`. The comment names the cause: "Precise symbol search (tantivy TextIndex) was removed." | `workspace/driver/search/mod.rs:110-118`, the `Ok` at `:117` |
| `SourceStores::symbol_by_id` | `Ok(None)`, unconditionally. Symbol identity used to come from the same removed tantivy index; the intended replacement — a catalog `symbols_proj` lookup — "is not wired yet". This single function is what makes the semantic arm, `GET /symbols/:id`, and `POST /expand` all return nothing. | `workspace/driver/search/mod.rs:96-98` |
| `SymbolStore::related_hits` | `Ok(Vec::new())`. Symbol-graph expansion moved off the removed Terminus store onto the IR reverse-position index, which is not materialized in-process (IP-7). | `workspace/driver/search/mod.rs:126-135`, the `Ok` at `:134` |
| `lower_to_symbol_search`'s `server` parameter | `let _ = server;` — "reserved for future ecosystem-default resolution." | `workspace/driver/http/handlers/search.rs:184` |
| `Driver::reranker()` | returns `None` always; the rerank handler builds its own client from config per request instead. | `workspace/driver/lib.rs:490-494` |
| NDJSON streaming | the handler `try_collect()`s the entire result before serializing, so nothing is actually incremental. The framing is real; the backpressure is not. | `handlers/search.rs:55-57` |

The Qdrant half is **not** a stub. `SemanticSurface::search` really embeds,
really calls Qdrant, and really returns scored identities. The gap is exactly
one function wide.

## What still works on the read plane

| Surface | Works? |
| --- | --- |
| `POST /packages/search` | **yes.** `Server::search_packages` (`workspace/driver/coordination/packages.rs:38`) runs the frozen fusion ranking over each source's replica-local package tantivy index, merges overlay-first, applies the keyset cursor, and hands back a real `Page<GlobalPackage>` with a `next` token. |
| `GET /sessions/:id` | yes — reads the scratch-backed session store. Creates an empty graph on first touch. |
| `POST /usages` | routed and typed, but always `503 IndexUnavailable` — see [api-contract.md](../api-contract.md#surfaces-that-return-empty). |
| `POST /expand` | always `404`: it resolves the seed symbol through `symbol_by_id` before it ever reaches `related_hits`. |
| `GET /symbols/:id` | always `404`, same cause. |

Note the asymmetry: package search has a live tantivy index (fed by
`package_index_poller`, `workspace/driver/poll.rs:395`); symbol search does
not, because the symbol index was removed and its replacement is unwired.

## Failure modes

| What fails | What the caller sees | Where logged | Retried? |
| --- | --- | --- | --- |
| `target` is not `"Symbols"` | `400` — `missing field: target must be Symbols for /search` | `error.rs:378` (`warn`) | no |
| Body is not valid `Query` JSON | `400` from axum's `Json` rejection | — | no |
| Body over 2 MiB | `413` | — | no |
| `LiteralQuery::parse` rejects the text (empty, control chars, unbalanced parens, too long) | `400 MalformedQuery`, carrying position and snippet where known (`error.rs:73`) | `error.rs:378` | no |
| `scope.packages` names nothing valid for any ecosystem | `400 NoValidPackageSelectors` | same | no |
| Malformed `page.cursor` (semantic arm) | `400 InvalidCursor` | same | no |
| Semantic quota exhausted | **no error** — silently degrades to the precise arm | `planner.rs:57` (`debug`) | n/a |
| Embeddings endpoint unreachable | `503` (`ServerError::Embed`) | `error.rs:376` (`error`) | yes — `is_retryable` |
| Qdrant unreachable | `503` (`ServerError::Vector`) | same | yes |
| Planner emits a semantic plan for a literal query | `500 PlannerInvariantSemanticForLiteral` — an internal invariant break | same | no |
| No hits | `200`, empty body. **Indistinguishable from the two dead paths above.** | `handlers/search.rs:56` (`debug`, `hits = 0`) | n/a |

That last row is why this document opens the way it does.

## Where to start reading

1. `workspace/driver/search/mod.rs` — 136 lines, and three of them are the
   whole story. Read the module doc comment first.
2. `workspace/driver/coordination/search.rs` — 161 lines, both arms.
3. `workspace/driver/http/handlers/search.rs` — the handlers and the
   wire→execution lowering.
4. `workspace/driver/search/planner.rs` — the gate and the budget, if you are
   touching escalation policy.
