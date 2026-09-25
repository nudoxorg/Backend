# MCP and TypeScript corpus — where it stands

Measured 2026-09-25 on this checkout. One hundred npm packages, each the registry `latest` tarball (not a git checkout, not `node_modules`). The producer is `TypescriptProducer` / OXC, the same path `index` uses for a local `package.json`. The agent walk calls `NudoxTools` directly (`packages`, `search`, `read`, `refs`, `graph`, and the schema card the `schema` tool serves).

Wall times are a debug build on a 4-core VM. They are real measurements of this run, not a release-profile budget.

Harness:

- `.config/scripts/npm-popular-census.py` downloads and flattens tarballs.
- `cargo test -p nudox-languages --test typescript_popular_census -- --ignored`
- `cargo test -p nudox-engine --test mcp_ts_agent_walk -- --ignored`

## What sealed

79 of 100 packages sealed. Together they contributed **252,048** entries, **186,754** of them public and not a synthetic module, **129,625** occurrences, **31,461** bodies, and source excerpts for **252,046** entries. One source-excerpt failure was recorded (zero `source_issues`). Total producer wall time for the sweep, including the failures, was **55.6 s**.

Kind mix of sealed entries:

| Kind | Count | Share |
| --- | ---: | ---: |
| Param | 99,720 | 40% |
| Function | 47,785 | 19% |
| Field | 33,795 | 13% |
| Static | 18,664 | 7% |
| Const | 18,348 | 7% |
| Module | 11,426 | 5% |
| Trait | 7,356 | 3% |
| Reference | 5,791 | 2% |
| Alias | 5,608 | 2% |
| Record | 2,253 | 1% |
| Variant | 1,257 | <1% |
| Enum | 45 | |
| Impl | 0 | |
| Reexport | 0 | |

`Impl` and `Reexport` never appear. TypeScript `implements` / `extends` and `export { X } from` are not landing as those kinds. The graph card already says `Trait.implementors` is Rust-only and points at `subtypes`. On this corpus `implementors` correctly returned an empty page **with** an `edge_empty` note naming `subtypes`. `returnedBy` of `AxiosResponse` returned an empty page **with no note**. An agent cannot tell "this type is not returned" from "return types were not linked".

Documentation is sparse: **15,323 / 252,048** entries (6%) carry a non-empty doc string. Semantic search cannot recover that. With no `NUDOX_EMBED_MODEL_DIR`, every `search` prepends a 248-character status line, **55 o200k tokens / 57 cl100k tokens**, and then answers from names only.

Largest sealed packages in this run: `webpack` 29,344 entries / 4.8 s, `fp-ts` 26,894 / 2.6 s, `prettier` 14,662 / 2.5 s, `react-dom` 10,181 / 2.0 s, `@types/node` 59,656 / (retry, declaration-heavy). `@types/node` also has **39,579 unlocated** entries. Everywhere else, locations are almost all `Declared`.

## What does not seal

21 packages fail inside `Lowering::finish`. The failure is package-fatal: nothing from that package is searchable. Two classes, both already patched for earlier fixtures and still hit by current `latest` tarballs.

**Referred but never declared** (11): `next`, `zod`, `lodash-es`, `vitest`, `mocha`, `ws`, `@types/react`, `@nestjs/common`, `@trpc/client`, `typeorm`, `tslib`.

The dangling ids are not one bug:

- `*` — star imports/exports (`zod` `src/v4/core/util.ts`, `mocha` `lib/cli/init.js`, `tslib` `tslib.d.ts`, `typeorm` `bson.typings.d.ts`).
- `default` — CJS/ESM default interop (`lodash-es` `lodash.default.js`, `mocha` `lib/mocha.cjs`, `ws` `lib/extension.js`).
- A real type the other file never declared under that id (`next` `Image`, `renderToReadableStream`; `vitest` `Plugin`, `mergeConfig`; `@types/react` `Fragment`, `JSX`; `@nestjs/common` `BeforeApplicationShutdown`).
- A minified name (`@trpc/client` `dist/wsLink-*.d.cts` name `s`).

**Declared more than once** (10): `@angular/core`, `ts-node`, `@types/lodash`, `graphql`, `@apollo/client`, `mongoose`, `ioredis`, `solid-js`, `three`, `openai`.

Same pattern: a dual publish (`.d.ts` and `.mjs` / `.d.mts` and `.d.ts` / `__cjs` and the ESM twin) or a merged namespace emits the same `(module, name, discriminant)`. Examples: `graphql` `language/kinds.d.ts` and `kinds.d.mts` both declare `Kind`; `@angular/core` `fesm2022/core.mjs` and `types/core.d.ts` both declare `enableProfiling`; `three` `build/three.webgpu.js` declares `Uniform` twice; `openai` field ids under `Response::Moderation::ModerationResult` collide across the `.d.mts` twin.

`next@16.3.6` is in the first class and is the package an agent is most likely to ask for. It spent 18 s extracting, then the seal rejected it. There is no partial corpus.

`typeorm`'s `latest` dist-tag resolved to **1.1.1**, not the 0.3 line. That is what the registry served. The failure (`*` in `bson.typings.d.ts`) is still a producer bug against that tarball.

## DefinitelyTyped layout

`@types/*` tarballs are rooted at the unscoped name (`node/`, `react/`), not npm's usual `package/`. A flattener that only lifts `package/` leaves `package.json` one directory down, and the producer reports a missing manifest. The census script now lifts a single nested root when that root contains `package.json`. After that, `@types/node`, `@types/react-dom`, and `@types/express` seal. `@types/react` and `@types/lodash` still die in `finish`, as above.

## What an agent actually gets

Eight packages that did seal were loaded together: `react` 162 symbols, `zustand` 30, `express` 56, `immer` 783, `commander` 1,097, `dayjs` 2,210, `axios` 2,998, `hono` 6,896. Load of all eight in the debug engine was under a second after the producer had already been warm; the census times above are the cold producer cost.

### `packages` and `graph`

`packages` listed all eight with version and symbol count (780 bytes, 285 o200k tokens).

`graph` `{ Packages { name ecosystem } }` returned the same eight. A filtered function query for `request` returned the four axios `request` declarations (CJS bundle and `.d.ts`, each twice). That duplication is the sealed form of the dual-publish problem: when the ids happen not to collide, the agent sees two or four copies of one API instead of a failed package.

### `search`

`search` for `useState` in `npm:react` returned **no matches**. The symbol is real and is the entire point of the package:

```js
exports.useState = function (initialState) {
  return resolveDispatcher().useState(initialState);
}
```

in `cjs/react.development.js`. It is an anonymous function assigned to `exports`, not `function useState`. The producer records 33 functions in react and none of them is `useState`. `react-dom` sealed 10,181 entries and has the same shape of export.

`search` for `Router` in `npm:express` is the same hole from the other direction. The package's public API is:

```js
var Router = require('router');
exports.Router = Router;
```

`require` bindings are intentionally not invented as consts, and the re-export is not a `Reexport` entry (the kind count above is zero). So `Router` is not in the index.

Before the fix in this change, that query was worse than empty. Passing `kinds: ["Record", "Function"]` armed the type section's kind facet, which lists every symbol of those kinds and ignores the query text. The page came back as `View`, `acceptParams`, `logerror`, `stringify`, … — eight functions, a `next` cursor, and no indication they were not matches. `useState` looked honest only because that call did not pass `kinds`. Explicit `kinds` is now encoded as an exclusion of the other kinds, which is how the default scope already avoided this. The same query is now `(no matches)`.

`search` for `dayjs` inside `npm:dayjs` returned modules and traits whose leaf name is `dayjs` (`isLeapYear.dayjs`, `weekOfYear.dayjs`, `trait Dayjs`), not a function you can call. The default scope already hides `Field` / `Variant` / `Param`. It does not hide the per-file `Module` named after the package.

`search` for `request` with `kinds: ["Param"]` did return axios parameters named `request`. Name prefix matching works when the declaration was actually emitted.

### `read` and addresses

The first axios hit's address was:

`npm:axios::axios.axios.Axios.request[function]#4163ceb9…` (64 hex)

o200k cost: **51 tokens** for that address, **40** for the legacy key `npm:axios#<64 hex>`. The readable path is not cheaper once the disambiguator hash is attached, because high-entropy hex still dominates. `SymbolKeyDto::to_wire` treated any `#` plus 64 hex as a legacy key, so the package name became `axios::axios.axios.Axios.request[function]`. `read` then failed with `package npm:axios::… is not loaded` — the address `search` had just printed. `to_wire` now rejects a name containing `::` and the address resolver runs. `read` of that address returns `axios.axios.Axios.request` at `dist/browser/axios.cjs:5243`.

`read` with `format=source` on the legacy key included the function body (`return await this._request(...)`). `format=signature` on the address rendered only `pub async fn request(configOrUrl, config)`.

### `refs`

`refs` in and out on that same function returned **zero rows**. Out said `status: complete`. The body that `read` just showed contains a call. Axios as a package has thousands of occurrences (3,065 in the census), so this is not "the language records nothing". It is a specific declaration whose body references were not attached, reported with the same shape as a function nothing calls.

### `schema` tokens

Measured with tiktoken on the strings the tools return:

| Payload | chars | cl100k | o200k |
| --- | ---: | ---: | ---: |
| schema card | 4,446 | 1,154 | 1,154 |
| full SDL (`schema.graphql`) | ~62,148 bytes of rendered markdown | 12,897 estimated by `heart::cost` | 12,897 on the rendered tool text |

`heart::cost::estimated_text_tokens` tracked the card (1,133 vs 1,176 o200k) and matched the full schema. It undercounted the axios search page: 233 estimated vs **389 o200k**, because that page is mostly hex keys. The plan's warning still holds for any payload that carries addresses.

The semantic-unavailable banner alone is 55–57 tokens on every search until a model directory is configured.

## Tool-by-tool limit

| Tool | Works | Falls apart |
| --- | --- | --- |
| `index` / producer | 79/100 `latest` tarballs seal, with real spans and some occurrences | 21/100 fail closed. Star/default imports, dual `.d.ts`/`.mjs` publishes, and cross-file type ids abort the package |
| `packages` | Honest version and symbol counts for what loaded | A package that failed to seal is simply absent. `index` of `next` would surface the lowering error; `packages` cannot say "tried and rejected" |
| `search` | Prefix match on names that were emitted. `kinds` now restricts instead of padding | Anonymous `exports.useState = function` and `exports.Router = require(...)` are invisible. Per-file modules named after the package crowd a query for the package. Semantic ranking is off without a model, and says so in 55 tokens every call |
| `read` | Source for a sealed declaration, including a CJS body. Hashed addresses resolve after this fix | Signature and source disagree in usefulness: the signature path drops the body. Duplicate twins (four `request`s) make the "one symbol" read ambiguous |
| `refs` | The in/out split and the `implementors` coverage note are the right shape | A body `read` can show still has `refs` out `status: complete` and zero rows |
| `graph` | `Packages` and name-filtered `Function` queries match the sealed IR | `implementors` is empty by construction (noted). `returnedBy` is empty with no note. No `Reexport` or `Impl` rows exist to query |
| `schema` | The card is ~1,150 tokens and is what a query should be written from | `full: true` is still ~13k tokens. The card's `subtypes` advice does not fire for `returnedBy` |

## Fixes in this change

1. `SymbolKeyDto::to_wire` rejects a hashed address (`::` in the package-name half) so `read`/`refs` resolve it instead of looking up a package that does not exist.
2. An explicit MCP `kinds` list is applied as an exclusion of every other kind, so a name query cannot be filled with unrelated symbols of the requested kind.
