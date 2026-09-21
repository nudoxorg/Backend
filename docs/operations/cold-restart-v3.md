# Cold restart and persistence journey

Run the production-process gate from a clean lane with the pinned tool
closure:

```sh
nix shell '.#luna-tools' --command cargo test --manifest-path tests/journeys/Cargo.toml --test restart_persistence -- --nocapture
```

`restart_persistence` creates a fresh persistent workspace for each test. It
starts the real local service, CLI, MCP adapter, and GPUI capture process with
bounded readiness and process deadlines. GUI launches scrub product selector
environment variables, so first launch must reach native onboarding without a
preconfigured project. A follow-up native add-project journey chooses a real
fixture folder and checks the replacement shell route and published action
tree; the process journey then creates and reopens two shelf identities.

The main journey admits a project, creates two shelf identities, and checks a
stable search row identity across graceful reopen, an in-flight SIGKILL, live
subscription resume, offline warm reopen, and repeated MCP calls after an idle
period. Recovery accepts only the previously committed root or the complete
new root. A deliberately damaged journal must fail closed before it exposes a
listener, after which the exact journal bytes are restored and reopened.

The persisted fixture also keeps the historical identity cases: a Rust trait
method and its impl with the same name (both `method` rows), an inherent Rust
impl method, and a one-word block C# namespace (one `module` row). Search and
name results, outline membership, and graph-query coordinates for those rows
are checked before and after graceful and SIGKILL recovery.

The registry journey serves a real feed and archive over loopback, verifies the
configured authorization header, and checks that an offline process restart
reuses the durable registry archive/cache without advancing the feed journal.
The portable endpoint journey keeps derived socket paths within the native
launcher bound and independent of a long temporary-directory prefix.

Each phase prints `elapsed_ms` and daemon `rss_kib` to stderr. The final JSON
report is tagged `nudox.cold-restart.v3` and includes old, recovered, and new
roots plus all phase observations.
