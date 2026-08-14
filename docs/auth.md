# docs/auth.md — account authentication for nudox

> **Status.** The contract at the bottom of this file is the user's. Everything
> above it is the design decided against that contract, the research it came
> from, and the gaps in the contract that this client had to route around. If
> you are here to change the offline policy, the batching, or the error
> vocabulary, the reasoning is here so you do not have to re-derive it.
>
> Implementation: `crates/nudox-mcp/src/account/`, `workspace/gui/src/app/account.rs`,
> `workspace/gui/src/views/sign_in.rs`.

---

## 0. The shape, in one diagram

```
        ┌──────────────── lindsey (GUI) ────────────────┐
        │  cmd-shift-A → SignInView                     │
        │  status bar  → "account · signed in"          │
        └───────────────┬───────────────────────────────┘
                        │  AccountHost (sync surface, mirrors McpHost)
                        ▼
   ┌──────────────── AccountGate ────────────────┐        ┌─────────────────┐
   │  admit() ──► ToolCallPermit  (no network)   │◄───────┤  NudoxMcpServer │
   │  posture() ──► one of nine named states     │        │  every tools/call│
   └───┬──────────────┬───────────────┬──────────┘        └─────────────────┘
       │              │               │
  CredentialStore   UsageLedger   AccountService
  (macOS Keychain)  (disk, batched)  (reqwest → api.nudox.org)
```

Two background loops, neither on the request path:

| Loop | Cadence | Does |
|---|---|---|
| refresh | wakes every 5 s, acts every 15 min | `POST /v1/authorize`, then `GET /v1/usage` |
| flush | wakes every 5 s, acts on 25 calls **or** 30 s | `POST /v1/usage/record`, then settle |

---

## 1. Where the key lives

**Decision: the macOS Keychain**, a generic-password item under service
`org.nudox.api`, account `default`, reached through `security-framework`'s
`passwords` module.

### The alternatives, and why they lost

**A dotfile** (`~/.config/nudox/credentials.json`). This is what most tools
reach for and it is the single most common way a key escapes: it is in the frame
of a screen recording, it is in `git add -A` if the user's home is a dotfiles
repo, it is in a tarball'd bug report, and it is readable by every process
running as that user. The GitHub CLI's own history is the cautionary tale — `gh`
wrote OAuth tokens in cleartext to `hosts.yml` until **v2.26.0 (2023-04-04)**
flipped OS credential storage on by default
(<https://github.com/cli/cli/releases/tag/v2.26.0>), and the plaintext fallback
it kept for keyring-less environments is still an open complaint
(<https://github.com/cli/cli/issues/10108>, <https://github.com/cli/cli/issues/7757>).

**SQLite.** Buys transactions and indexes for a single row written on sign-in
and read on launch, and it is still a file readable by this user. Every
objection to the dotfile survives, with a schema on top. The *non-secret* half
of our state genuinely is a file — see §4 — it just contains no key.

### What everyone else does

| Tool | Where the credential lives | Source |
|---|---|---|
| Zed | `CredentialsProvider` trait → macOS Keychain / Linux Secret Service; "never saved as plaintext in configuration files" | <https://zed.dev/docs/ai/use-api-access> |
| GitHub CLI | OS credential store by default since v2.26.0; `--insecure-storage` writes `hosts.yml` in cleartext; `GH_TOKEN` bypasses both | <https://github.com/cli/cli/releases/tag/v2.26.0> |
| Docker Desktop | `docker-credential-osxkeychain` helper, selected by `"credsStore": "osxkeychain"` | <https://docs.docker.com/reference/cli/docker/login/> |
| Warp | macOS login keychain, service `dev.warp.Warp-Stable` — their own troubleshooting doc gives the `security delete-generic-password` recovery command | <https://docs.warp.dev/support-and-community/troubleshooting-and-support/troubleshooting-login-issues/> |
| Raycast | extension-scoped encrypted store; the Store review policy explicitly **rejects** extensions requesting raw Keychain entitlements | <https://developers.raycast.com/basics/prepare-an-extension-for-store> |
| Sublime Text | machine-bound encoding, **plus** a documented plaintext `License.sublime_license` that Sublime *reads and never writes* | <https://www.sublimetext.com/docs/portable_license_keys.html> |
| TablePlus, Dash | vendor claims Keychain (TablePlus); nothing authoritative found for Dash | — (see §9) |

Sublime's read-never-write plaintext file is the precedent for our
`NUDOX_API_KEY` (§1.3): an explicitly-opted-into escape hatch that the app
honours and never creates.

### 1.1 The trait, and why it is not three functions

`CredentialStore` is a trait with `KeychainStore` (production) and
`MemoryStore` / `FailingStore` (tests). That seam is not taste; it is forced by
three documented facts about the macOS Keychain:

1. **A keychain item's ACL is bound to the Designated Requirement of the process
   that wrote it.** An ad-hoc-signed binary — which is what every `cargo build`
   produces — gets a new code hash on every rebuild, so the ACL stops matching
   and macOS re-prompts *"wants to use your confidential information"* on each
   build, even after "Always Allow"
   (<https://github.com/openclaw/gogcli/issues/569>; same class reported for
   Firebase iOS SDK <https://github.com/firebase/firebase-ios-sdk/issues/8950>).
2. **A headless macOS session cannot unlock the login keychain at all.**
   `security` calls fail with "User interaction is not allowed"
   (<https://developer.apple.com/forums/thread/690665>), and GitHub Actions'
   macOS runners have shipped with a locked or entirely absent default keychain
   (<https://github.com/actions/runner-images/issues/4519>).
3. Therefore **`cargo test` cannot reliably touch the login keychain**, and a
   test that popped a system modal or wrote into a developer's real keychain
   would be worse than no test.

Every automated test in this repository uses `MemoryStore`.
`tests/account_secret_hygiene.rs` and `tests/account_against_a_fake_service.rs`
never construct `KeychainStore`.

**`security-framework`, not `keyring`.** `keyring`'s macOS backend *is*
`security-framework`, so the cross-platform abstraction buys nothing for a
macOS-only app, and `security-framework` 3.7 is already in this workspace's
lockfile via the TLS stack. Adding `keyring` would pull a second dependency tree
to reach the same `SecItem*` calls.

### 1.2 The failure this design exists to *not* have

`gh` conflated **"the keyring read failed"** with **"no token is configured"**:
both returned an empty string from `ActiveToken()`, and the HTTP round-tripper
then treated an empty token as "this request needs no auth" and sent it
unauthenticated (<https://github.com/cli/cli/issues/13317>). That is
AGENTS-DOCTRINE §8's `map_err(|_|)` shape wearing a different hat — a real
failure converted into a benign-looking value.

So `CredentialStore::load` returns `Result<Option<ApiKey>, _>` and the two
negatives are **different states all the way up**:

* `Ok(None)` → `GateState::SignedOut` → `McpError::NotSignedIn`
  (`kind: "not_signed_in"`, help: "sign in").
* `Err(_)` → `GateState::StoreUnavailable` →
  `McpError::CredentialStoreUnavailable` (`kind:
  "credential_store_unavailable"`, help: "unlock your login keychain").

"Sign in" is useless advice to someone whose keychain is locked, and this is the
assertion that keeps them apart:
`state.rs::signed_out_and_store_unavailable_never_collapse`.

### 1.3 `NUDOX_API_KEY` wins over the Keychain

Precedence: environment, then store. The environment is a *deliberate act by
whoever launched the process*; the keychain entry may be months old. The inverse
precedence produces the worst available outcome — a user sets the variable to
switch accounts, nothing changes, and no surface anywhere says the variable was
read and ignored.

Two consequences, both implemented:

* A **malformed** value in the environment is reported, never silently skipped
  in favour of the keychain. Falling back would sign the user in as whoever the
  *old* key belongs to while they believe the variable took effect.
* The UI says which source is in force (`KeySource::{Keychain, Environment}`),
  and **the sign-out button is hidden for an environment key** — the app cannot
  unset a variable it did not set, and a button that silently does nothing is
  worse than no button.

### 1.4 Key hygiene: what is protected, and what is not claimed

`ApiKey` has a redacted `Debug`, **no** `Display`, **no** `Serialize`/
`Deserialize`, a single `expose()` accessor named to be greppable, and a
constant-time `PartialEq` that reuses `session::constant_time_eq` rather than
deriving a short-circuiting one.

The concrete failure being defended against has a CVE number: **Docker Desktop
CVE-2025-13743** — on a failed Hub auth the client serialized an error object
that had *retained the PAT which produced it*, wrote it to the app log stream,
and the "Export diagnostics" flow then bundled that log into a shareable archive
(CWE-532, "Insertion of Sensitive Information into Log File" —
<https://cwe.mitre.org/data/definitions/532.html>,
<https://www.sentinelone.com/vulnerability-database/cve-2025-13743/>). Nothing
about that required carelessness. It required one error type to hold one field.

So `ApiKeyError` carries a **position and a character class, never a value**, and
`tests/account_secret_hygiene.rs` renders every rejection through `Display`,
`Debug` and `help` and greps for a fragment of the key.

**What is deliberately not claimed: memory zeroization.** The secret is a plain
`String` and is not scrubbed on drop. `String` reallocates and moves; a `Drop`
impl that overwrote the final buffer would leave every earlier buffer intact and
would buy a *claim* of scrubbing rather than scrubbing. OWASP's Secrets
Management cheat sheet does recommend byte arrays over immutable strings for
exactly this reason
(<https://cheatsheetseries.owasp.org/cheatsheets/Secrets_Management_Cheat_Sheet.html>)
— worth revisiting if the threat model ever includes local memory inspection; it
does not today, because an attacker who can read this process's heap can also
read the Keychain item it came from.

*(Noted while researching: OWASP's cheat sheet is almost entirely server/CI
oriented and contains no guidance on desktop-client credential storage at all.
CWE-532 is the better canonical citation for this threat.)*

---

## 2. Two secrets, kept structurally apart

This process handles exactly two secrets and they have **opposite disclosure
rules**.

| | `session::SessionToken` | `account::ApiKey` |
|---|---|---|
| Authenticates | a *transport*: one local process → this loopback socket | an *account*: this installation → `api.nudox.org` |
| Lifetime | one launch; regenerated every start, never persisted | until revoked in the dashboard |
| At rest | nowhere | macOS Keychain |
| Disclosure | **printed on purpose** — status bar, Settings → Connection, the client-config snippet | never, anywhere |
| Blast radius | another local process reads this machine's corpus | someone else spends this user's quota |

The distinction is structural, not documentary:

* Different types in different modules, with **no `From` in either direction**.
* A `SessionToken` does not even *parse* as an `ApiKey` — it is bare hex, so
  `ApiKey::parse` rejects it with `MissingPrefix`, whose help names the mistake
  explicitly ("a bare hex string is the local MCP session token … a different
  secret").
* `tests/account_secret_hygiene.rs::the_client_config_snippet_carries_the_session_token_and_never_the_account_key`
  builds a real endpoint and asserts the snippet contains one and not the other.

### Rejected: making the `ndx_` key the loopback bearer

The tempting simplification is to have the agent present its `ndx_` key
*directly* to the loopback MCP endpoint, deleting `SessionToken`. Rejected:

* It would put an account credential into an MCP client config file — the exact
  dotfile-plaintext failure §1 rules out, reintroduced through the front door.
* It would make every local process that can reach 127.0.0.1 able to *spend the
  user's quota*, not merely read their corpus. The threat model of the loopback
  token is other local users; escalating its blast radius to billing is a strict
  downgrade.
* Rotating one would rotate the other, and they have different natural
  lifetimes (per-launch vs per-revocation).

The gate therefore sits **below** the session check: a caller that cannot prove
it is local never reaches a question about which account it is.

---

## 3. The offline / quota state machine

### 3.1 Five stored states, nine named postures

`GateState` (stored, exhaustive, five variants):

| State | Means |
|---|---|
| `SignedOut` | store answered successfully and holds nothing |
| `StoreUnavailable { message, help }` | store could not be consulted; existence of a key is *unknown* |
| `Unverified { fingerprint, source, probe }` | key present, never confirmed; `probe` is `Pending` or `Failed(cause)` |
| `Authorized { account, source, quota, last_failure }` | confirmed at least once |
| `Revoked { fingerprint, reason, at }` | service said `allowed: false` |

`Posture` (derived, exhaustive, **nine** variants) — what the gate and the UI
both read:

| Posture | `kind` on the wire | Tool calls | Recoverable by |
|---|---|---|---|
| `SignedOut` | `not_signed_in` | ✗ | signing in |
| `StoreUnavailable` | `credential_store_unavailable` | ✗ | unlocking the keychain |
| `AwaitingFirstVerification` | `authorization_pending` | ✗ | **retrying** (the only one) |
| `NeverVerified` | `never_verified` | ✗ | one successful online check |
| `Active` | — | ✓ | — |
| `GraceOffline` | — | ✓ | (warns; see below) |
| `GraceExpired` | `offline_grace_expired` | ✗ | reaching the network |
| `Revoked` | `key_revoked` | ✗ | a new key |
| `OverLimit` | `quota_exceeded` | ✗ | period rollover / upgrade |

**Why the extra four are derived rather than stored.** `Active` /
`GraceOffline` / `GraceExpired` are functions of `account.verified_at` and the
clock, and `OverLimit` is a function of the last quota snapshot. If
`GraceExpired` were its own stored variant, *something* would have to notice the
moment it became true — a timer, a tick, a refresh — and every path that forgot
to run that something would serve a user whose grace ran out three days ago.
Deriving it means expiry happens whether or not anybody was watching. This is
AGENTS-DOCTRINE §8's counting rule applied to a state machine: a tally kept
*beside* the thing it counts can drift from it; one computed *from* it cannot.

**Precedence inside `Authorized`:** `GraceExpired` → `OverLimit` →
`GraceOffline` → `Active`. Expiry outranks over-limit because once grace has
lapsed our quota knowledge is at least a week stale, and reporting a stale
`over_limit: true` would send the user to the billing page to fix a network
problem. Pinned by `state.rs::expiry_outranks_a_stale_over_limit_reading`.

### 3.2 The policy, in four numbers

| Constant | Value | Governs |
|---|---|---|
| `FRESH_FOR` | **12 h** | how long a successful `authorize` is "current" |
| `GRACE_WINDOW` | **7 days** | how long past `verified_at` tool calls keep working unreachable |
| `GRACE_WARNING_AT` | **48 h remaining** | when the UI escalates from a quiet badge to a warning |
| `NEAR_LIMIT_MARGIN` | **25 calls** | how close to the quota before batching collapses to one |

#### Why seven days

Commercial practice clusters into two bands, and this sits deliberately between
them.

*The short band* is floating-licence keep-alive: JetBrains' on-prem License
Server gives clients a **48-hour** grace so a maintenance window does not strand
everyone, and its newer License Vault reclaims a seat within ~20–30 minutes of a
client going quiet
(<https://www.jetbrains.com/help/ide-services/floating-licenses.html>). Those
numbers are about *reclaiming a shared seat*, which is not our problem — an
`ndx_` key is one user's.

*The long band* is consumer subscription tolerance: Adobe's desktop apps
revalidate roughly monthly and allow **30 days** offline for month-to-month
members with a further **99** for annual members
(<https://helpx.adobe.com/creative-cloud/kb/internet-connection-creative-cloud-apps.html>),
and JetBrains gives a lapsed subscription a **one-week** grace with an in-IDE
notice at every launch. Those are about not punishing a billing hiccup.

Ours is neither: it is *"this developer is on a plane, at a conference, or on a
locked-down network"*. Seven days covers every real instance of that with room
to spare — the longest commercial flight is under 20 hours — while keeping the
licence meaningful, which a 30-day window would not. It also matches JetBrains'
subscription grace, the closest comparable single-seat number.

#### The anti-pattern being avoided

Tailscale node keys expire after **180 days** with **no grace period at all**;
the admin console offers a 30-minute "temporarily extend key" window *after* the
device is already locked out (<https://tailscale.com/kb/1028/key-expiry>), and
unattended devices simply stop working
(<https://github.com/tailscale/tailscale/issues/19785>,
<https://github.com/tailscale/tailscale/issues/6786>). No documentation of a
proactive pre-expiry notification was found.

**A long window with no warning is worse than a short window with one.** That is
why `GRACE_WARNING_AT` exists, why `Posture::GraceOffline` carries `remaining:
Duration` rather than a bare flag, and why
`Posture::needs_urgent_attention()` flips **before** the cut-off, not at it —
pinned by `state.rs::grace_reports_the_time_remaining_and_escalates_before_the_cutoff`.

#### Why a never-verified key is a hard stop

`NeverVerified` denies. An account relationship cannot be *established* offline,
because nothing on this machine knows whether the key is real. Note the
distinction from `AwaitingFirstVerification`, which also denies but tells the
caller to retry: that state lives for about a second after launch on a machine
with no cached verdict, and collapsing the two would make a first launch look
broken.

#### Why a revocation is never softened by grace

`Revoked` is sticky and is cached to disk. A revocation is an **answer**, not a
failure to get one. Extending offline tolerance to it would mean a key revoked
*because it leaked* keeps working for a week; caching it means "quit and relaunch"
is not a workaround. Pinned by `state.rs::a_revocation_is_never_softened_by_grace`
and by the integration test that pulls the plug after a revocation and asserts it
stays revoked.

### 3.3 Client state is advisory; the server is the authority

A user with a text editor can move `verified_at` forward and extend their own
grace. This design does the two cheap things that stop *accident* rather than
*intent*:

* a cached verdict is bound to the **fingerprint** of the key that earned it, so
  it cannot be transplanted onto a different key (paste a revoked key over a
  good one and you do not inherit its week);
* `verified_at` is **clamped to "not in the future"** on ingest, so a clock skew
  does not mint grace.

It does not pretend to be tamper-proof. Metering is enforced server-side or it is
not enforced.

---

## 4. Usage metering: batching, crash safety, and the ambiguity

### 4.1 Nothing on the request path

Every `tools/call` is a billable `tool_call`. A synchronous
`POST /v1/usage/record` before answering would add a full internet round trip —
tens to hundreds of milliseconds — to every query against a **local**
documentation index, which is the one thing this product is supposed to be fast
at.

So `AccountGate::admit()` is synchronous, does no network, and does exactly two
things: consult the posture, and increment a counter. The counter's only I/O is a
~300-byte atomic file write with **no `fsync`**.

`tests/account_against_a_fake_service.rs::tool_calls_accumulate_locally_and_flush_in_one_batch`
is the assertion: twenty admitted tool calls cost **zero** requests, then exactly
one request carrying twenty.

### 4.2 The batching shape, and where the numbers came from

Size trigger *or* time trigger, whichever comes first, plus an explicit flush on
shutdown — the shape every metering and telemetry client converges on.

| Client | Batch | Interval |
|---|---|---|
| OpenTelemetry `BatchSpanProcessor` | `maxExportBatchSize` **512**, `maxQueueSize` **2048** | `scheduledDelayMillis` **5000**, `exportTimeoutMillis` **30000** (<https://opentelemetry.io/docs/specs/otel/trace/sdk/>) |
| Amberflo metering SDK | `batch_size` **100**, `max_queue_size` **100000** | `send_interval_in_secs` **0.5** (<https://github.com/amberflo/metering-python>) |
| Lago batch endpoint | **100** events/request, atomic (one bad event 422s the whole batch) | — (<https://getlago.com/docs/api-reference/events/batch>) |
| **nudox** | `FLUSH_THRESHOLD` **25**, `MAX_BATCH` **100** | `FLUSH_INTERVAL` **30 s** |

Ours is longer in time and smaller in size because a billing unit is not a span:
nobody is waiting for it, and a chatty billing endpoint is a cost centre. At one
request per 30 s an all-day session costs the service ~1,000 writes — the same
order as the tool calls themselves.

**Adaptive near the limit.** Batching's cost is that the over-limit boundary is
blurred by up to one batch. Within `NEAR_LIMIT_MARGIN` (25) of the quota the
batch size collapses to **one**, so the user learns they are over within a single
tool call instead of up to 25 later. Pinned by
`ledger.rs::near_the_limit_every_call_is_flushed_on_its_own`.

### 4.3 Durability: what a crash actually loses

OpenTelemetry's queue is in-memory and its spec says so — it batches for network
efficiency, not durability. Sentry writes each envelope to disk *before* sending
so failed sends survive
(<https://develop.sentry.dev/sdk/foundations/transport/offline-caching/>).
Segment's Android SDK uses Square's Tape `QueueFile`, explicitly "designed to
survive process and system crashes"
(<https://github.com/segmentio/analytics-android/blob/master/analytics/src/main/java/com/segment/analytics/QueueFile.java>).

We are closer to Sentry. `record_call` persists on **every call** with
`Durability::Fast` — temp file + atomic `rename`, no `fsync`. So:

* a **process** crash (the realistic loss event for a desktop app) loses
  **nothing** — pinned by `ledger.rs::a_crash_before_sealing_loses_nothing`;
* a **power cut** can lose the last few calls;
* sealing a batch and shutting down use `Durability::Durable` (with `fsync`),
  because those are the moments where a loss costs money in a specific
  direction.

### 4.4 The hard part: a POST that times out but may have succeeded

`POST /v1/usage/record` returns 204 or 429, **no body**, and the contract offers
no idempotency key. This is not an implementation detail we can engineer around;
it is a property of the protocol we were handed.

Every metering vendor that solves this solves it the same way — by having the
client supply a key the **server** stores and deduplicates on:

* Stripe caches the full response (status *and* body) under an
  `Idempotency-Key` for at least **24 hours**, and explicitly advises *"retry
  such requests with the same idempotency keys … until they're able to receive a
  result"* (<https://docs.stripe.com/api/idempotent_requests>,
  <https://docs.stripe.com/error-low-level>). That advice presupposes the key.
* Stripe's Meter Events API dedups on an optional `identifier`, unique over a
  rolling ≥24 h window (<https://docs.stripe.com/api/billing/meter-event/create>).
* OpenMeter dedups CloudEvents by `source` + `id` over a **32-day** window
  (<https://openmeter.io/docs/getting-started/event-ingestion>).
* Lago dedups on `transaction_id`
  (<https://getlago.com/docs/api-reference/events/batch>).

Without one, a client is **choosing a bias, not eliminating an ambiguity**:

* **at-least-once** — retry the timeout, risk charging the user for work they
  did not do;
* **at-most-once** — drop the batch, risk not charging for work they did.

#### This client chooses at-most-once

Over-billing a developer for calls they never made is a support ticket and a
trust problem; under-billing is a cost. OpenMeter argues the opposite default —
*"it's better to report usage twice and filter out duplicates later than to
underreport it"* (<https://openmeter.io/blog/usage-deduplication>) — and they
are right, **given a server that can filter**. We have not got one. See §8 gap 2;
the day `/v1/usage/record` accepts an idempotency key, this policy flips.

The 429 case follows the same rule: the batch is **settled, not re-queued**,
because the contract does not say whether a 429 counted it (§8 gap 3) and
re-sending is the one outcome that can bill twice. Sentry's transport spec makes
the identical call for its own 429s — discard, respect the limit, do not retry.

#### The delivery-certainty classification

The retry policy lives in exactly one place, `service.rs::classify`:

| `reqwest` signal | `ProbeFailure` | May retry? |
|---|---|---|
| `is_connect()` — DNS, refused, TLS handshake | `NotDelivered` | **yes** — the server never saw a byte |
| timeout, reset, 5xx | `Indeterminate` | **no** |
| decode/body error | `MalformedResponse` | **no** |

`is_timeout()` is deliberately **not** in the retryable bucket even though many
timeouts are connect timeouts, because `reqwest` reports a *read* timeout the
same way and a read timeout means the request was written.

`tests/account_against_a_fake_service.rs::a_hung_request_drops_the_batch_rather_than_double_billing`
reproduces this against a real socket that reads the request, records it, and
never answers — the fake really did process the batch, which is what makes the
case ambiguous rather than merely failed.

#### Reconciliation makes the drop rare rather than routine

Before dropping, the ledger asks `GET /v1/usage` — the only endpoint
authoritative about how much the account has actually been charged — and
compares. `ledger::reconcile` is that comparison as a pure function:

```
without = anchor.tool_calls + acked_since
with    = without + batch
observed >= with      → Landed       (settle; do not re-send)
observed <= without   → NotLanded    (re-queue; safe)
otherwise             → Indeterminate (drop, and count it)
```

Inequalities rather than equality because the account is not ours alone to move:
a second machine or the dashboard can record between our anchor and our
observation, and exact equality would make every reconciliation `Indeterminate`
— the answer that costs money.

**The bias is stated and tested.** Concurrent activity inflates `observed`, which
pushes an ambiguous batch toward `Landed`, which means we do *not* re-send, which
means we under-count. Every uncertainty in this module resolves in the user's
favour. That is a design property, not a coincidence —
`ledger.rs::concurrent_activity_biases_toward_not_re_sending`.

*Research note.* No source was found that addresses this exact scenario — a bare
counter, 204/429-only, no idempotency, reconciled against a server total. The
closest mechanism with a real spec is **Kafka's idempotent producer**, where the
broker tracks a per-`(pid, topic, partition)` highwater sequence number and drops
a retry carrying a sequence it has already accepted
(<https://cwiki.apache.org/confluence/display/JJKafka/Idempotent+Producer>) —
which requires the *server* to hold new per-client state, i.e. it is the same ask
as §8 gap 2 wearing a different hat.

#### Drops are counted and visible, never silent

AGENTS-DOCTRINE §8: a repair — and dropping a billable batch is one — is
permitted only when it is **typed, counted, bounded and visible**.

* *Typed* — `DroppedTally { batches, calls }`, on the ledger.
* *Counted* — derived from the batches actually dropped, persisted, and
  surviving a key change (an orphaned ledger's calls are added to the tally
  rather than forgotten).
* *Bounded* — three call sites: an ambiguous flush, an unreconcilable orphan, and
  a sign-out with unflushed calls. No wildcard.
* *Visible* — `UsageLedger::unreported_calls()` feeds `AccountPresentation::dropped`,
  which the account panel paints in the warning colour.

### 4.5 Crash recovery

An `inflight` batch found at startup means the process died between sealing and
settling. The next flush resolves it via `GET /v1/usage` **before sealing
anything new** — two unknowns at once would make the reconciliation arithmetic
unsound, which is why `seal()` refuses while one is outstanding.

With **no anchor** (we have never seen a `GET /v1/usage` for this account) every
observed total is consistent with both outcomes, so the batch is dropped and
counted rather than guessed at —
`ledger.rs::an_orphan_with_no_anchor_is_dropped_rather_than_guessed_at`.

---

## 5. 429 is a product state, not an error string

Seven new `McpError` variants, not one `Account { message }`. The test is
whether an agent and a human do *different things* about each, and they do: sign
in, unlock a keychain, wait a second, get online, get online **within the week**,
replace a revoked key, or wait for the period to roll over.

Each extends the existing typed vocabulary — a `kind` machine tag distinct from
the JSON-RPC `code`, and a `help` field distinct from `message`:

```jsonc
// error.data for McpError::QuotaExceeded
{
  "kind": "quota_exceeded",
  "help": "This is a plan limit, not a bug — the account has used its full
           allowance for the current billing period. Every tool call will fail
           identically until the period rolls over or the plan is upgraded at
           https://nudox.org/dashboard/billing. Do not retry; tell the user.",
  "tier": "free", "used": 1000, "limit": 1000, "remaining": 0,
  "toolCalls": 700, "periodStart": "2026-08-01T00:00:00Z",
  "isQuotaNotFault": true,
  "upgradeUrl": "https://nudox.org/dashboard/billing"
}
```

Three deliberate choices in there:

* **`isQuotaNotFault: true`** — an agent must be able to tell "the plan ran out"
  from "something broke" without parsing prose.
* **"Do not retry; tell the user."** — telling a model to retry into a quota wall
  turns a rate limit into a denial-of-service against our own API.
* **No `periodEnd`.** `GET /v1/usage` does not return one (§8 gap 1), and
  computing a plausible date from `periodStart` would be a fabrication an agent
  would repeat to a user as fact.

The offline case is worded, `kind`ed and *flagged* so it can never be read as a
revocation:

```jsonc
{ "kind": "offline_grace_expired", "recoverable": true,
  "offlineForDays": 7, "graceWindowDays": 7, "lastVerifiedAt": 1754870400,
  "help": "This is a connectivity problem, not a revoked key — nothing is wrong
           with the account. …" }
```

versus `{ "kind": "key_revoked", "recoverable": false, … }`. The assertion that
these never blur is
`account_against_a_fake_service.rs::expired_grace_denies_with_an_error_that_cannot_be_read_as_a_revocation`,
which greps the message for "reject", "revok" and "invalid" and fails if any
appear.

**All seven map to JSON-RPC `invalid_request`** — the code `Unauthenticated`
already uses. No new code is invented: the seven-way distinction lives in
`data.kind`, which is what a stable machine tag is *for*. An agent that only
understands codes learns "do not retry with different arguments"; one that reads
`data` learns "you are over quota until the period rolls over".

### Where the gate sits

`NudoxMcpServer::new(engine, gate)` — the gate is a **mandatory constructor
argument**. There is no `Default`, no `Option`, and no "unauthenticated mode"
that happens to serve. A build that wants no metering asks for
`AccountGate::unmetered(reason: &'static str)` and states why, which is one grep
away from anyone asking "where did metering go?".

All ten `#[tool]` bodies go through one `NudoxMcpServer::metered` helper. Ten
copies of "check the gate, then increment, then call" is ten chances to write
nine — and the tenth, an unmetered tool, would be invisible in review because it
looks exactly like the others minus a line that is not there.

`graph_schema` is metered too. It answers from a constant, but `docs/auth.md` bills a
`tool_call` and an agent cannot tell which of our tools are cheap for us to
serve. Exempting it would put a hole in the meter whose size is set by how often
agents call it — once per session, and therefore not small.

---

## 6. The sign-in surface

`cmd-shift-A` opens one overlay that is **both** sign-in and signed-in;
`SignInView::new` derives which from the presentation it is handed, so no caller
has to know which state the account is in before it can pick.

Four phases, exhaustive (`Editing { problem } | Checking | Accepted { account } |
Unavailable`) — not three bools, which would make three of eight combinations
reachable and meaningless.

**The field renders a mask, never the key.** `ndx_` in clear (a public prefix),
one bullet per hidden character, and the last four in clear once the value is
long enough for that to reveal nothing useful. A sign-in form is the most
photographed surface in any application — it is in every screen recording of a
first run — and a plaintext secret in it is CWE-532 reached by a different route.

**Validation is offline and first.** `ApiKey::parse` catches the four real paste
failures — a truncated selection, a JSON string literal with its quotes, `Bearer
` copied along, the loopback session token in the wrong field — instantly and
specifically, instead of costing a round trip and coming back as the service's
generic `invalid token`.

### The animation

`src/motion/spring.rs` is a physically-based solver this app mostly uses for
docks and scrims. Sign-in is the one moment where the user has *handed over a
secret and is waiting to be told whether it was right*, and motion can answer
faster than text can be read. Three springs, each carrying one fact:

* **`lift`** (`Spring::SNAPPY`) — the panel rises and settles rather than
  jump-cutting in. Entrance; unremarkable.
* **`checking`** (`Spring::GENTLE`) — a sweep under the field while the round
  trip is out. It *travels*, it does not fill: we do not know how long
  `POST /v1/authorize` will take, and a bar creeping to 90% and stopping would be
  a lie about progress.
* **`accepted`** (`Spring::DEFAULT`) — the one that earns its place. A single
  spring from 0 to 1 drives the form's presence down and the identity row's up,
  so the panel **becomes** the account rather than being replaced by it. That is
  the only information the user wants at that instant, and it arrives before the
  sentence underneath can be read.

Every one checks `reduced_motion()` and calls `snap_to` instead. A user who has
asked the system for less motion has asked for it here most of all, because this
is a modal they cannot dismiss until it finishes.

### The signed-in state, and signing out

The account panel shows the user id, `ndx_…4517`, the storage source, a usage
meter from a real `GET /v1/usage`, any locally-pending calls, and — in the
warning colour — any calls we failed to report. Sign-out deletes from the
Keychain, clears the cached verdict, and resets the ledger (counting unflushed
calls as unreportable, because they belong to the account being left).

The status bar carries an `account · …` segment for **every** posture the process
actually has, including a healthy one. A signed-in state that renders as nothing
is a signed-out state that renders as nothing, and the first time the user would
learn the difference is when an agent's tool call is refused — the same defect
docs/LIMITATIONS.md L35 records for `Option<SharedString>`.

---

## 7. Rejected: OAuth 2.0 device flow (RFC 8628)

**Not adopted, and not as an oversight.**

The flow (<https://www.rfc-editor.org/rfc/rfc8628.html>): the client POSTs to a
device-authorization endpoint and receives a `device_code`, a short human-typed
`user_code` (the RFC's example is `WDJB-MJHT`; §6.1 recommends a 20-character
consonant-only alphabet so codes cannot spell words), a `verification_uri`, an
`expires_in` (example: 1800 s) and a poll `interval` (default **5 s** if
omitted). The client polls the token endpoint, treating `authorization_pending`
as "keep going", `slow_down` as "add 5 s to the interval, cumulatively",
`expired_token` as "start over" and `access_denied` as "the user said no".

Every tool surveyed ships it as the *interactive default* with a
paste-a-credential escape hatch beside it: `gh auth login --with-token`
(<https://cli.github.com/manual/gh_auth_login>), `docker login -u`
(<https://docs.docker.com/reference/cli/docker/login/>), `tailscale up
--auth-key`, `VERCEL_TOKEN`.

**Why it is the wrong primary flow here.** The premise of device flow is that
the client cannot receive a browser redirect and the user has no credential to
paste. Our user *already has one* — they created it in a web dashboard, and
`docs/auth.md` says they paste it into an agent-harness config file by hand at the
same time as the MCP setup. Device flow would add a second acquisition path for a
credential that has already been acquired.

**And it carries a risk a static key does not: phishing.** An attacker runs the
real CLI, obtains a genuine `user_code`, and relays it to a victim as a routine
authentication request — which succeeds precisely because every defensive signal
(real domain, real MFA) is correct. Microsoft Entra ID added a conditional-access
option to **block device code flow entirely**, and AWS moved its CLI *away* from
device grant to authorization-code + PKCE in **v2.22.0**, keeping device code
behind an explicit `--use-device-code` flag for headless environments only
(<https://aws.amazon.com/blogs/developer/aws-cli-adds-pkce-based-authorization-for-sso/>,
<https://docs.aws.amazon.com/cli/latest/reference/sso/login.html>,
<https://workos.com/blog/pkce-vs-device-flow-cli-auth>,
<https://blog.logto.io/cli-authentication-methods>).

**What we borrowed instead.** The *dual-path* structure, which is the part every
one of those tools actually shares: an interactive surface (the sign-in overlay)
plus a non-interactive one (`NUDOX_API_KEY`) for headless and CI. That is `gh`'s
trichotomy minus the flow we do not need.

**When to revisit.** If dashboard keys ever become short-lived or rotating, a
browser handoff becomes the better acquisition path and this decision should be
re-opened — with `verification_uri_complete` (Tailscale's single-URL style)
rather than a split code, since lindsey can open a browser itself.

---

## 8. Gaps in the contract

Found while implementing. Each has a client-side workaround in place and each
would be materially better fixed server-side.

1. **`GET /v1/usage` returns `period_start` but no `period_end`.** The client
   therefore cannot tell an over-limit user *when they get unblocked* — the one
   fact they want. Every message says "when the period rolls over".
   **Ask:** add `period_end`.

2. **`POST /v1/usage/record` has no idempotency key.** This is the single most
   consequential gap: it is why §4.4 has to choose a billing bias instead of
   being correct. Every metering vendor solves it the same way (Stripe
   `Idempotency-Key` / meter-event `identifier`, OpenMeter `source`+`id`, Lago
   `transaction_id`).
   **Ask:** accept an `Idempotency-Key` header, or a client-supplied `event_id`
   in the body, deduplicated over ≥24 h. The client already has a durable
   monotonic batch identity to send.

3. **429 semantics are unspecified.** Does it mean "the batch was recorded and
   you are now over" or "the batch was rejected"? The client assumes the former
   (settles, then reconciles) because that is the assumption that cannot
   double-bill.
   **Ask:** state it, ideally in a response body.

4. **`allowed: false` carries only a free-form `reason` string.** The client
   cannot distinguish revoked / never-existed / suspended / wrong-environment, so
   all four get one message and the service's prose is shown verbatim.
   **Ask:** add a machine-readable `code`.

5. **`POST /v1/authorize` returns no revalidation hint.** The client had to
   invent `FRESH_FOR = 12 h` and `GRACE_WINDOW = 7 days`, which means the
   licence policy is baked into a shipped binary and cannot be changed without a
   release.
   **Ask:** return `revalidate_after` (seconds) and `grace_seconds`.

6. **No documented rate limit on `/v1/authorize`.** The refresh cadence
   (15 min) is a guess made to be polite.
   **Ask:** document it, or return `Retry-After`.

7. **`ndx_` keys carry no checksum.** GitHub's token formats put a CRC32 in the
   last six base62 characters *specifically* so a client can reject a mistyped
   token offline, and reported it cut secret-scanning false positives to 0.5%
   (<https://github.blog/engineering/platform-security/behind-githubs-new-authentication-token-formats/>).
   We can validate prefix, charset and length; we cannot catch a single
   transposed character.
   **Ask:** adopt GitHub's CRC32→base62 suffix. (Note: Stripe, Slack and OpenAI
   use prefixes for *identification* only and document no checksum — do not cite
   "Stripe-style" for this.)

8. **Paths are written relative** (`POST v1/authorize`). Assumed to be
   `https://api.nudox.org/v1/authorize`.

9. **`user_id` stability is unstated.** The client persists it in the
   authorisation cache and treats it as stable.

10. **Nothing says whether `count` is applied atomically at the limit boundary.**
    If a batch of 25 straddles the limit, is all of it recorded, none, or a
    prefix? The client's adaptive near-limit batching (§4.2) reduces the blast
    radius to one call, but the semantics should be stated.

### One thing the brief got wrong

The brief says *"the user creates an API key from the dashboard, then adds it
along with the MCP setup into their agent harness config"*. Read literally that
puts the account key into an MCP client config file — which is the dotfile
plaintext storage §1 rejects, and which would additionally conflate it with the
loopback session token that config *does* legitimately carry. This client
therefore treats the key as **lindsey's** to hold (Keychain, via the sign-in
overlay), with `NUDOX_API_KEY` as the documented path for harnesses that have no
GUI. If the intent really is "paste it into the MCP config", that should be
reconsidered before it ships.

---

## 9. Research that came back empty

Recorded so nobody re-runs it:

* **Tailscale's macOS client**: no authoritative documentation of how the GUI app
  stores its own node identity on disk (Keychain vs state file). The auth-key
  guidance is documented; the node key's storage is not.
* **Dash (Kapeli)**: nothing authoritative on license-token storage.
* **TablePlus**: vendor claims Keychain; no primary technical documentation, and
  several user-reported bugs where passwords fail to persist.
* **Setapp**: publishes a 14-day grace period, but that is a *billing* grace, not
  an offline-connectivity one. No offline behaviour documented.
* **`security-framework` vs `keyring-rs`**: no third-party head-to-head
  comparison exists; §1.1's reasoning is derived from each crate's own docs.
* **Cloud-vendor metering agents** (CloudWatch, Datadog, New Relic, Honeycomb):
  nothing published on how their own agents avoid double-counting on retry.
* **A source arguing device flow should replace a working paste-a-key path**:
  none found. Every source that discusses both frames device flow as solving the
  *browser-required-OAuth* problem, not as categorically superior.
* **Adobe's 30/99/129-day figures** come from a search summary of Adobe's help
  page; the direct fetch timed out. Verify before quoting them anywhere binding.

---

## 10. Test map

| File | Covers |
|---|---|
| `crates/nudox-mcp/src/account/credential.rs` (unit) | key parsing, every rejection shape, redaction, fingerprinting |
| `.../store.rs` (unit) | round trip, absence-vs-unavailability, environment precedence |
| `.../state.rs` (unit) | all nine postures, the three grace boundaries, precedence, clock skew, tag/kind uniqueness |
| `.../cache.rs` (unit) | disk round trip, fingerprint transplant defence, corrupt-cache tolerance, atomic write |
| `.../ledger.rs` (unit) | reconciliation arithmetic, seal/settle/requeue/drop, crash recovery, flush triggers |
| `.../gate.rs` (unit) | admit/deny, denial → wire `kind`, no counting on denial |
| `crates/nudox-mcp/tests/account_against_a_fake_service.rs` | the whole thing over **real HTTP** against a controllable loopback fake: sign-in, revocation, 429, connection-refused, hang-up, expired grace, sign-out |
| `crates/nudox-mcp/tests/account_secret_hygiene.rs` | the key never reaches the config snippet, a `Debug`, an error rendering, or disk |
| `workspace/gui/tests/screenshots.rs` frames 32–34 | the sign-in flow, driven through the real view against a real loopback service |

Nothing in the suite resolves `api.nudox.org`;
`the_suite_never_points_at_production` asserts it.

---
---

# The contract (as written by the user)

> kind must be api_request or tool_call
>
> for mcp it will all be just tool_call
> So when an agent does a tool call on the mcp we record that with that endpoint
> User creates api key from dashboard, then adds it along with the mcp setup into their agent harness of choice config
> There are like 2 or 3 other endpoints that you can use to get current user's data via their api key but they won't be important
> Url for service is api.nudox.org
> Only endpoints you need to worry about are:
> POST v1/authorize
> GET v1/usage
> POST v1/usage/record
>
> i will go through full pattern on how to use them in a little bit and it will be up to you to figure out offline usage tracking (cache or sqlite store or some bs to track batch usage then when stable internet comes back then it the record endpoint with the amount of requests or something idk)
>
> **POST v1/authorize**
> `Authorization: Bearer ndx_67…`
> this just checks if the key is legit, makes sure user didn't make type when setting up mcp, or they didn't revoke the key or anything, also returns some basic user info
> Response:
> ```json
> { "allowed": true, "user_id": 24, "scopes": [] }
> ```
> or
> ```json
> { "allowed": false, "reason": "invalid token" }
> ```
>
> ignore scopes and org stuff that is for later
>
> **POST v1/usage/record**
> `Authorization: Bearer ndx_420…`
> request body:
> ```json
> { "kind": "tool_call", "count": 1 }
> ```
> this record unit of work, 429 status code means that user has now hit their limit and their broke ahh cannot use shi anymore, 204 is success
>
> no response body just status code, and logs usage event to db
>
> **GET v1/usage**
> `Authorization: Bearer ndx_42069`
> also can check usage data if user is over limit
>
> response:
> ```json
> {
>   "tier": "free",
>   "period_start": "2026-08-01T00:00:00Z",
>   "api_requests": 300,
>   "tool_calls": 700,
>   "used": 1000,
>   "limit": 1000,
>   "remaining": 0,
>   "over_limit": true
> }
> ```
>
> That is literally it, very simple, you decide how to handle things like a simple middleware pass with those and offline usage and stuff
