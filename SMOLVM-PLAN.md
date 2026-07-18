# SMOLVM-PLAN — Hypervisor-grade compile: smolvm as the one cage, streaming IR into the VCS

**Date:** 2026-07-17 · **Status:** Rev 1 — integration plan
**Companions:** `LIBRARIFICATION-PLAN.md` Rev 2 (this plan amends GD-16/17/18/19/31 and §12/§13/§14/§17), the retired `DAEMON-PLAN.md` (whose Phase-7 "MicroVM cage tier" slot this plan fills), `.research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md` Rev 3.2 + the **as-built** `workspace/nudox-ir-vcs` (libpijul-is-the-engine — where the two disagree, the as-built code wins).
**Pinned upstream:** `smol-machines/smolvm @ 56bb13b99dfe65a58df158bf241077a5024d3a64` (v1.6.1 line; libkrun 1.15.1, libkrunfw 4.x). All file references below are into that tree.

---

## 0. Thesis

> Replace the layered OS-sandbox stack (bwrap + landlock + seccomp + cgroups on Linux,
> Seatbelt on macOS, gVisor on the fleet) with **one hardware-virtualized cage** —
> a smolvm microVM per compile — on **both** desktop and fleet; let that boundary
> carry the trust weight so *anyone can compile their local changes, untrusted deps
> included, without fear*; make the compile **stream IR out of the guest** into an
> **incrementally-written libpijul channel**; and never compile what the INDEX has
> already sealed — a **JobKey handshake** answers "already generated?" before any
> VM boots.

Four moves, one architecture:

1. **One cage** — `SmolvmCage` implements the existing `Cage` trait. Everything above
   `Cage` (SealedInput, CapabilityBudget, Job typestate, Producer, ForgeRuntime) is
   untouched — exactly the payoff DAEMON-PLAN §2.2 promised ("a third `Cage` impl,
   no layer above changes").
2. **One toolchain plane** — smolvm guests are Linux *everywhere*, including macOS
   desktops. Toolchains ship as OCI images: the fleet's L3 image layers **are** the
   desktop language packs. Same image ⇒ same content fingerprint ⇒ **desktop JobKey
   ≡ fleet JobKey**, which is what makes the dedup handshake actually hit.
3. **Streaming compile** — producers emit symbol batches over a dedicated vsock port
   as they lower; the host folds them into the libpijul working copy incrementally
   and records one change at seal. Long compiles get durable checkpoints and live
   GUI progress; crashes resume from the working copy instead of restarting.
4. **Recording stays the host's job** (GD-39 preserved) — the guest emits *candidate
   entries*; the host-side channel writer assigns IntroIds, records, applies, seals.
   Streaming changes emission granularity, never ownership.

---

## 1. What smolvm is (evidence, pinned commit)

| Fact | Evidence | Why it matters here |
|---|---|---|
| Embeddable Rust library: `EmbeddedRuntime` / `MachineSpec` / `VmConfig::builder` / `VmBackend`/`VmHandle` | `src/lib.rs`, `src/embedded/mod.rs` | We embed; no CLI shelling |
| Hypervisors: macOS HVF (arm64 + x86_64), Linux KVM (x86_64 + aarch64), Windows WHP | `src/vm/backend/libkrun.rs`, README | Covers §13 platforms; first real macOS isolation; future Windows answer for §21-Q9 |
| Guest = libkrunfw Linux kernel + Alpine + `smolvm-agent`; JSON-over-vsock protocol (4-byte LE length frames, 32 MiB cap) | `crates/smolvm-protocol/src/lib.rs`, `src/agent/client.rs` | Exec/exit/stdio/file-IO channel exists; our IR stream rides a *separate* vsock port |
| Custom vsock ports: `VmConfig.vsock_ports: Vec<VsockPort { port, socket_path, listen }>` | `src/vm/config.rs:201` | Host-side Unix socket per port — the IR streaming channel |
| Filesystem: virtiofs host mounts with per-mount RO/RW + DAX windows; ext4 storage disk for OCI layers; qcow2 CoW overlays (persistent or ephemeral) | `src/data/storage.rs`, `src/agent/launcher.rs` | `FsGrant` maps 1:1; ephemeral overlay = job scratch that self-destructs |
| Network: **off by default**; `NetworkPolicy::{None, Egress{dns, allowed_cidrs}}` + host-side DNS filter | `src/network/`, `src/dns_filter.rs` | `NetGrant` maps 1:1; Sealed ⇒ `None` enforced by VM config, not seccomp |
| OCI-native: agent pulls/flattens images into the storage disk; registry auth crate | `crates/smolvm-registry/` | The toolchain-image plane comes for free |
| Golden fork: checkpoint a warm VM, CoW-clone per use, ~250 ms restore; cold boot ~100–200 ms | `src/agent/fork.rs`, README | Warm pools without our own pool machinery |
| Elastic memory (virtio balloon), sparse disks | README | Idle warm VMs are cheap; `Limits` → `cpus`/`memory_mib` |
| Licenses: smolvm + libkrun Apache-2.0; libkrunfw LGPL-2.1 + GPL-2.0 kernel, **dlopen'd**, shipped as separate binaries | `Licenses.md`, `src/agent/krun.rs` | Same boundary discipline as libpijul/snix (R12); nothing GPL links into lindsey |
| macOS needs entitlements: `com.apple.security.hypervisor`, `disable-library-validation`, `allow-jit` | `smolvm.entitlements` | Ship a signed helper binary beside lindsey, not entitlements on the GUI |
| Explicit non-claim: smolvm is **not a multi-tenant control plane** | AGENTS.md | Fleet keeps the pod as the tenant boundary; the VM replaces gVisor+bwrap *inside* it |
| Host-side seccomp on the boot subprocess is x86_64-Linux-only; landlock optional | `src/platform/linux.rs` | Residual host hardening is a bonus, never the boundary |

---

## 2. Decisions (SV-1..SV-14)

- **SV-1 One cage, both planes.** `SmolvmCage` is the production `Cage` on desktop *and*
  fleet. `LinuxNamespaces` (bwrap+seccomp+landlock+cgroup orchestration) and
  `MacSeatbelt` are deleted; `DevPassthrough` survives only under
  `Policy::Development` (tests, fixtures). Amends GD-17 ("sandbox layering on the
  fleet") and GD-19 ("gVisor RuntimeClass + in-pod bwrap/landlock" → pod + KVM microVM).
- **SV-2 The budget layer is the contract; the VM is the mechanism.** `SealedInput`,
  `CapabilityBudget { FsGrant, NetGrant, Env, Limits }`, `Job<Acquiring>/Job<Sealed>`,
  `ThreatTier`, `OverrideTable` survive byte-for-byte. `SmolvmCage` *projects* a budget
  into a `VmConfig` (§3.1). No caller changes.
- **SV-3 Linux guests everywhere; toolchains are OCI images.** Desktop compiles run the
  **same** toolchain images as the fleet (L3, GD-31). `ToolchainSet::digest` becomes a
  content fingerprint over image config+layer digests. Consequence: **JobKey parity
  across planes** — the acceptance test is a fixture compiled on a darwin desktop and
  a linux pod yielding the identical JobKey *and* byte-identical sealed archive
  (deterministic seal already guarantees the latter). Strengthens GD-18; its "desktop
  and fleet CAS align" clause becomes literally true instead of aspirational.
- **SV-4 Trust model shift (GD-16 amendment).** Old rule: *untrusted never compiles
  locally.* New rule: **untrusted never executes outside a `Job<Sealed>` microVM.**
  Local compilation of untrusted/unknown code (hostile `build.rs`, proc-macros,
  `postinstall`, third-party deps) is allowed inside the cage. The Workspace-Trust
  gate stops gating *compilation* and starts gating *host-side capabilities*:
  RW mounts beyond the job scratch, `Acquiring`-phase network, artifact publish,
  toolchain discovery on the host. `Compile` exists for both `SourcePath<Trusted>`
  and `SourcePath<Untrusted>`, but the untrusted impl is constructible only over a
  `SmolvmCage` (typestate: `VmForge`), never `DevPassthrough`, never in-process.
- **SV-5 ThreatTier decides in-VM vs in-process.** Producers that *execute
  package-controlled code* (rustc/build.rs/proc-macros, go build, javac, nix eval,
  anything npm lifecycle) are `Hostile` ⇒ VM always. Pure parsers over hostile *input*
  (OXC, pyrefly, tree-sitter) may stay worker-isolated in-process on the trusted
  desktop path for latency; on the fleet everything is in-VM (uniformity beats the
  saved 200 ms).
- **SV-6 No-double-compile handshake.** New INDEX endpoint `POST /v1/compiled/lookup`
  (batch JobKey → hit/miss + generation stamp + manifest ref). The client checks
  *before* booting a VM; hits are pulled over the existing iroh data plane under a
  DownloadGrant and recorded/synced locally like any generation. Misses compile
  locally. **Local artifacts never upload** — shared L0/L1 accept fleet-signed writes
  only (the server cannot trust desktop-produced bytes; GD-31 security clause extended
  to say so explicitly).
- **SV-7 Client may read fleet stage caches.** L0 (JobKey → sealed artifacts) and L1
  (stage postcard CAS) become client-*readable* through the same grant-authorized iroh
  path, so a local compile of a big dep graph hits remote stage caches mid-pipeline.
  Write access remains fleet-only (SV-6).
- **SV-8 Streaming IR is a vsock plane, not a wire-protocol rewrite.** A new
  `ir-stream` postcard frame protocol runs over a dedicated `VsockPort` from guest
  producer to host session (§6). The fleet's `compiler-wire` request/response
  (full sealed artifacts, §2.3 of LIBRARIFICATION) is unchanged in v1; streaming the
  fleet daemon → INDEX writer is an open question (§10-Q4), not a blocker.
- **SV-9 Incremental VCS writes = staging session + one recorded change.** The
  as-built `IrRepository` records O(delta) against the working-copy tip. We add a
  `RecordingSession` that folds streamed batches into the WC *as they arrive*
  (durable staging), records **one** libpijul change at `finish()` (normal path), and
  can `checkpoint()` intermediate changes for very long compiles (each a real,
  invertible change on the channel; the generation pins the final tip via
  `ChangeSetRef`). Fan-out (embed/index/graph) still fires only at generation seal —
  GD-28 committed-only survives.
- **SV-10 Recording ownership unchanged (GD-39).** The guest emits wire-twin entry
  payloads (K27 discipline: `IntroId`/`StableRef`/digests only — never arena indices).
  The host session computes deterministic IntroIds (`intro::determin`), owns the
  channel writer lock, records, seals. A compromised guest can emit garbage IR for
  *its own package* — which it always could, by being the code under analysis — but
  can never touch another channel, the CAS, or the archive seal.
- **SV-11 Warm pools = golden forks.** Per toolchain-image, one golden VM is booted,
  image pulled, agent readiness confirmed, then checkpointed. Jobs fork clones
  (~250 ms) with ephemeral overlays. Replaces cage_pool warm spares at the VM level;
  the in-guest `WorkerPool` (long-lived interpreter workers) survives *inside* the
  guest for `ExecPlan::Library` producers.
- **SV-12 Fleet nodes need `/dev/kvm`.** Karpenter NodePools move to metal or
  nested-virt-enabled instance families for the compile fleet. In exchange: no gVisor
  RuntimeClass, no `hostUsers: false` gymnastics, no in-pod bwrap, one fewer moving
  part in every pod template. Pod remains the tenant boundary (smolvm's own docs
  disclaim multi-tenant control-plane duty — we don't ask it to do that).
- **SV-13 Sources enter the guest RO; artifacts leave via streams, not mounts.** The
  job tree is a virtiofs RO mount (or CAS-hardlink view materialized host-side per
  GD-29); scratch is the ephemeral overlay; outputs exit as ir-stream frames +
  explicit `FileRead`/section uploads. Nothing the guest writes is trusted as
  by-path truth; every artifact is content-hashed at the host boundary.
- **SV-14 Packaging/licensing.** Vendor smolvm at the pin (path or git dep);
  libkrun/libkrunfw ship as platform binaries beside our binaries, dlopen'd at
  runtime (their model). lindsey gains a small signed helper (`nudox-vm-helper`)
  carrying the macOS entitlements; the GUI process itself stays unentitled. License
  CI: Apache-2.0 (smolvm, libkrun) allowlisted; libkrunfw recorded under the same
  GPL-at-a-distance rule as libpijul (K7) and snix (R12).

---

## 3. The cage

### 3.1 Budget → VmConfig projection (normative)

```rust
/// sandbox (slimmed): the one production cage.
pub struct SmolvmCage {
    runtime: EmbeddedRuntime,             // smolvm embedded, in-process
    images: ToolchainImageStore,          // OCI images, content-fingerprinted (SV-3)
    pool: GoldenPool,                     // golden checkpoints by image digest (SV-11)
    policy: Policy,                       // Production | Development
}

impl Cage for SmolvmCage {
    fn run_sealed(&self, cmd: &SealedCommand, token: &CancelToken)
        -> Result<Captured, CageError>;   // unchanged signature
}
```

| Budget field | VM projection |
|---|---|
| `FsGrant` RO roots | virtiofs mounts, `read_only: true`, one tag per root |
| `FsGrant` scratch (the one RW) | ephemeral qcow2 overlay (auto-destroyed) + RW virtiofs only when the host must read results back by path (discouraged — SV-13) |
| `NetGrant::Off` (Sealed) | `NetworkPolicy::None` — no NIC, no TSI; vsock only |
| `NetGrant::On(allowlist)` (Acquiring) | `NetworkPolicy::Egress { allowed_cidrs, dns }` + DNS filter list from the same allowlist |
| `Env` allowlist | exec env vector (agent `RunConfig.env`) — the guest never sees host env |
| `Limits.memory` | `memory_mib` (balloon reclaims idle) + in-guest rlimit as belt-and-braces |
| `Limits.cpu_seconds` / wall | exec `timeout` + host-side `CancelToken` → `VmHandle::kill` |
| `Limits.pids/nofile/fsize` | in-guest rlimits applied by the agent before exec |
| `ThreatTier` | Hostile ⇒ VM mandatory; StaticParser ⇒ may bypass on trusted desktop path (SV-5) |
| cgroup naming (`NodeId+JobKey`) | host cgroup around the **VMM process** — scheduling fairness only, no longer a security layer |

`Job<Sealed>`'s `NetOff` marker now compiles down to "the VM was *constructed* with
`NetworkPolicy::None`" — the network is absent from the machine, not filtered. The
typestate finally states a physical fact.

### 3.2 Guest image = the toolchain story

One base image (`nudox-guest-base`): Alpine + smolvm-agent + `producer-worker` +
mount/exec shims. Per-language toolchain images layer on top (rust+cargo+rustup
pinned, go, jdk, dotnet, node, nix). These are the **same images** GD-31 L3 already
plans for the fleet; the desktop pulls them through smolvm's registry client into the
VM storage disk, content-addressed.

`ToolchainSet::digest` is recomputed as `blake3(domain ‖ image_config_digest ‖
sorted layer digests)` per `nudox-producer/1`. Discovery of *host* toolchains
(rustup on the Mac, etc.) stops feeding sealed compiles entirely — host toolchains
remain only for the trusted in-process fast path (SV-5) and are fingerprinted
separately, so a fast-path JobKey can never collide with a sealed-path JobKey.

### 3.3 What gets deleted / what stays

| Deleted | Replaced by |
|---|---|
| `LinuxNamespaces` (bwrap orchestration), seccomp BPF compile + memfd cache, landlock module, `MacSeatbelt` | `SmolvmCage` + VM boundary |
| Per-job cgroup security wiring, `cgroup.kill` cancellation | `VmHandle::kill`; host cgroup demoted to fairness |
| gVisor RuntimeClass, `hostUsers: false`, in-pod bwrap (fleet templates) | pod + `/dev/kvm` + microVM |
| Host-toolchain `/nix/store` bind-mount provisioning for sealed jobs | OCI toolchain images (SV-3) |
| Escape-suite assertions about namespace/seccomp internals | escape suite retargeted at the VM boundary (§8 P0 gate) |
| Stays | `Cage` trait, budget types, `Sealer`, `Job` typestate, `ThreatTier`, `OverrideTable`, probes (now: kvm/hvf availability), `WorkerPool` (in-guest), `DevPassthrough` (dev only) |

The `sandbox` crate shrinks to: budget + seal + typestates + `SmolvmCage` +
`DevPassthrough` + probes. Its name finally matches its size.

---

## 4. Trust & the "compile without fear" flow (GD-16 amendment)

```
open project root
  ├─ trust gate (host capabilities only: RW mounts, Acquiring net, publish)
  ├─ TRUSTED first-party fast path: in-process EmbeddedForge (unchanged, §12)
  └─ EVERYTHING ELSE — unknown repos, dirty clones, third-party deps, hostile
     build scripts — compiles locally TODAY, inside SmolvmCage:
        seal → JobKey → /v1/compiled/lookup
          ├─ hit  → iroh pull closure → Ready (no VM boots)
          └─ miss → golden fork → Acquiring (Egress allowlist) → seal → produce
                    → ir-stream → RecordingSession → change → archive → Ready
```

What the user experiences: cloning a random repo and hitting "index" **just works**,
immediately, offline-capable, with no trust ceremony — because nothing that repo's
code does can reach outside a kernel-isolated VM whose network doesn't exist and
whose only writable surface is a disposable overlay. The trust prompt appears only
when something wants *more* than that (publish, RW project mounts, host toolchains).

The T1–T10 trust matrix gains VM arms: symlink-escape *into a mount*, exfiltration
attempt under `NetworkPolicy::None`, scratch-overlay persistence check, guest→host
path-traversal on returned artifacts (hash-verify at boundary per SV-13).

---

## 5. No-double-compile: the JobKey handshake

### 5.1 Endpoint

```
POST /v1/compiled/lookup            (control plane, ≤ 256 KiB)
{ "job_keys": ["<blake3 hex>", …] }               // ≤ 1024 per call
→ { "results": [ { "job_key": "…",
                   "hit": true,
                   "generation_stamp": "…",        // package plane stamp
                   "manifest": GenerationManifestDto } | { "hit": false } ] }
```

Hits chain straight into the existing `/v1/sync/plan` want/have flow — the manifest
is the want-set; providers + grant come back as they already do (GD-30). No new data
path, no INDEX blob proxying.

### 5.2 Client flow rules

- Lookup happens **after seal** (the JobKey is a sealed-input hash; there is nothing
  to look up before the input set is pinned) and **before** VM boot.
- Offline ⇒ skip lookup, compile locally (the whole point of SV-4).
- A first-party JobKey can hit too (CI or a registry publish compiled the same
  tree): pull-instead-of-compile applies identically.
- Dedup metrics from day one: `compile.lookup.hit_ratio`, bytes-pulled vs
  cpu-seconds-saved — this ratio justifies (or indicts) the whole handshake.

### 5.3 Write-back (none)

Local results are recorded into the **local** channel/CAS only. The INDEX learns
nothing from desktop compiles. If the same JobKey is later requested remotely, the
fleet compiles it independently — determinism (byte-identical archives, SV-3 parity
gate) makes the redundancy harmless and keeps the server's trust story trivial.

---

## 6. Streaming IR out of the guest

### 6.1 `ir-stream` protocol (new leaf crate)

Postcard frames over one dedicated `VsockPort` (host listens on a Unix socket per
job). Length-prefixed frames ≤ 4 MiB (well under smolvm's 32 MiB cap):

```rust
pub const IR_STREAM_VERSION: u32 = 1;

pub enum StreamFrame {
    Hello { version: u32, job: JobKey, producer: ProducerId },
    /// Wire twins only (K27): no arena indices, no StrIds.
    Symbols { batch: Vec<WireEntry> },        // WireEntry = OwnedEntryPayload wire twin
    Links { batch: Vec<WireLink> },
    SourceDigest { path: String, hash: ContentHash, size: u64 },
    Occurrences { section: Bytes },           // optional, chunked
    Progress { emitted: u64, phase: PhaseWire },
    Finish { emitted: u64, producer_digest: ContentHash },
    Abort { failure: FailureKindWire, message: String },
}
```

Producer-side: a `SymbolSink` handle threaded through lowering — `sink.emit(entry)`
buffers and flushes per file/module or per N entries. Producers that today build the
whole `Index` in memory adopt the sink incrementally; a shim (`sink.emit_all(index)`)
lets un-ported producers stream one giant batch with zero behavior change.

### 6.2 Host side: `RecordingSession` (nudox-ir-vcs addition)

```rust
impl<C: ChangeStore> IrRepository<C> {
    /// Streaming counterpart of record_generation. Holds the channel writer lock.
    pub fn begin_recording(&self, job: JobKey) -> Result<RecordingSession<'_, C>, VcsError>;
}

pub struct RecordingSession<'r, C: ChangeStore> { /* WC staging + tip guard */ }

impl RecordingSession<'_, _> {
    /// Fold a batch into the working copy: assign IntroIds (intro::determin),
    /// write/update symbols/{intro_hex} files. Durable staging, no change yet.
    pub fn stage(&mut self, batch: Vec<WireEntry>) -> Result<StageReport, VcsError>;
    pub fn stage_links(&mut self, batch: Vec<WireLink>) -> Result<(), VcsError>;
    /// Optional durability point for very long compiles: records a real libpijul
    /// change from staged state (invertible; appears on the channel).
    pub fn checkpoint(&mut self, msg: &str) -> Result<Option<ChangeHashHex>, VcsError>;
    /// Deletions = tip symbols never re-staged; computed here, then one change
    /// recorded (or the final checkpoint), tip returned for the generation pin.
    pub fn finish(self) -> Result<FinishReport, VcsError>;   // → IrTip + ChangeHashHex
    /// Crash/cancel path: WC survives; next session resyncs or discards.
    pub fn abandon(self) -> Result<(), VcsError>;
}
```

Facts this leans on (as-built): recording is already O(delta) against
`working_copy_tip` (`repo.rs:259`); `materialize_index_incremental` (`repo.rs:824`)
gives O(delta) reads for the GUI; file-per-IntroId means a streamed batch touches
exactly its own files. The known libpijul directory-inode panic (`repo.rs:~359`)
must stay fenced — sessions only ever create files under the pre-registered
`symbols/` tree.

Deletion semantics: a compile is a *full* enumeration of the package, so symbols
present at tip but never staged by `finish()` are deletions — same rule
`record_generation` applies today, now computed from the session's staged-set.
`checkpoint()` never deletes (a partial enumeration can't distinguish "gone" from
"not yet emitted").

### 6.3 Consequences

- **Live UI:** each `StageReport` (added/updated counts, sample monikers) feeds the
  jobs panel; symbols become browsable pre-seal through the *ephemeral generation*
  id space (§7 of LIBRARIFICATION) — never the durable one (GD-28 intact).
- **Crash recovery:** kill -9 mid-compile leaves a durable WC; on retry the session
  resyncs (stale-WC one-time resync already exists) and the producer's CAS-hit
  sub-stages (surface/cst per-tag keys) skip recomputation.
- **Memory:** the host never holds a whole `Index` for streamed producers; peak
  memory is one batch + WC buffers. The guest may still build its full oracle state —
  that's the producer's affair inside its `Limits`.
- **Generation identity is unchanged:** `GenerationStamp` v3 still hashes manifest
  inputs + `ChangeSetRef{channel, tip}`; whether the tip was reached by one change
  or five checkpoints is visible history, not identity drift. Squash-on-finish
  (unrecord checkpoints, re-record one change) is a **policy toggle**, default off
  for first-party channels (history is the product), default on for registry
  channels (INDEX writers want one change per publish).

---

## 7. Wire & plane amendments (LIBRARIFICATION deltas)

| Item | Amendment |
|---|---|
| GD-16 | "may compile in-process" stays trusted-only; add: *untrusted may compile locally inside a Sealed microVM cage* (SV-4). `node_modules`-root refusal, jail canonicalization, lockfile rules unchanged |
| GD-17 | "Sandbox layering on the fleet: gVisor + hostUsers + bwrap" → "pod + KVM smolvm microVM"; DevPassthrough desktop clause unchanged; add `VmForge` beside `EmbeddedForge` |
| GD-18 | Toolchain fingerprints = OCI image content fingerprints; desktop MVP languages all gain a sealed VM path (remote-only languages become "remote-or-local-VM") |
| GD-19 | Job pod template: RuntimeClass dropped; node prerequisites gain `/dev/kvm`; `podFailurePolicy` unchanged; poison classification gains `VmBootFailure` (infra, retryable elsewhere) vs guest exit codes (unchanged semantics) |
| GD-31 | L0/L1 gain **client read** via DownloadGrant (SV-7); write remains fleet-IAM-only; L3 images double as desktop toolchain packs; L4 warm state generalizes to golden checkpoints |
| §2.2 routes | + `POST /v1/compiled/lookup` (SV-6) |
| §12 | compiler-core unchanged (pure); `sandbox` crate contents per §3.3; `producer-worker` becomes a guest-image component |
| §13 | Language packs = toolchain OCI images; "Remote-only: Java, C#, Nix" relaxed to "fleet-or-local-VM" (snix GPL still never links into the GUI — it runs inside the guest image) |
| Non-existence list (GD-26) | + "desktop-produced artifacts in shared CAS"; + "sealed compiles against host-discovered toolchains" |

---

## 8. Phases (each independently shippable)

**P0 — Cage first-light.** Vendor smolvm @ pin; `SmolvmCage` behind `Cage`;
budget→VmConfig projection (§3.1); probes (kvm/hvf, helper entitlements on macOS);
`nudox-guest-base` image with agent + exec shim.
*Gate:* existing offline compile fixtures pass under `SmolvmCage` on Linux **and**
macOS; escape suite retargeted (net-off exfil attempt, mount-escape, overlay
persistence) green; cold-boot p50 < 300 ms measured.

**P1 — Toolchain images + JobKey parity.** Rust/Go toolchain images; image-digest
`ToolchainSet`; sealed-vs-fast-path fingerprint separation.
*Gate:* same fixture on darwin desktop VM and linux CI runner ⇒ identical JobKey,
byte-identical sealed archive.

**P2 — Streaming.** `ir-stream` crate; `SymbolSink` in one real producer (Rust or
TS) + `emit_all` shim for the rest; `RecordingSession` in nudox-ir-vcs; jobs-panel
progress events.
*Gate:* streamed compile of a 10k-symbol fixture: host peak RSS bounded by batch
size; kill -9 at 50% then rerun completes with all sub-stage cache hits;
`finish()` tip == batch-mode `record_generation` tip on the same input (equivalence
property test).

**P3 — Dedup handshake.** `/v1/compiled/lookup` on INDEX; client precheck flow;
iroh pull-on-hit; hit-ratio metrics.
*Gate:* two desktops, same repo state: second one reaches Ready with zero VM boots;
offline path still compiles.

**P4 — Trust rollout.** `VmForge` for `SourcePath<Untrusted>`; trust-gate rewire to
host-capabilities; T1–T10 matrix + VM arms.
*Gate:* clone-random-repo → index works with no trust prompt and no host writes
outside the job scratch (fs-audit test); matrix green.

**P5 — Fleet cutover + deletions.** Pod template minus gVisor; NodePool `/dev/kvm`;
golden-fork warm pool; then **delete** bwrap/seccomp/landlock/Seatbelt code paths.
*Gate:* fleet chaos suite (spot preemption, VM-boot poison classification) green;
grep-level absence of the deleted modules; L1 cross-plane read hit demonstrated
(desktop pulls a fleet stage blob mid-compile).

---

## 9. Risks

| # | Risk | Sev | Mitigation |
|---|---|---|---|
| V1 | smolvm is young; API churn at our pin | M | Vendored pin; `Cage` trait isolates callers; upgrade = one crate |
| V2 | Fleet nodes without nested virt (no `/dev/kvm`) | H | NodePool constraint to metal/nested-virt families **before** P5; warm-path Deployment migrates first (easier rollback) |
| V3 | virtiofs perf on huge dep trees (first-build cold reads) | M | DAX windows + CAS-hardlink materialization host-side (GD-29) keeps hot files node-local; measure in P1 gate |
| V4 | Guest kernel/agent CVE class replaces syscall-filter CVE class | M | Hypervisor boundary is strictly stronger than shared-kernel; keep host seccomp-on-VMM (free); track libkrun/libkrunfw releases in the vendored pin |
| V5 | Linux-guest-on-macOS battery/thermal cost for background indexing | M | Balloon + idle golden parking; compile bursts are user-initiated; measure in P0 |
| V6 | JobKey parity silently broken (image drift, env leak into key) | H | P1 parity test is release-blocking and runs in CI forever |
| V7 | Checkpoint changes pollute registry channel history | L | Squash-on-finish default for registry writers (§6.3); first-party keeps history by design |
| V8 | 4 GiB vsock transfer cap / 32 MiB frame cap on giant sections | L | Frames chunked at 4 MiB; occurrence sections already sectional; nothing legitimate approaches the caps |
| V9 | Golden checkpoint staleness (image updated, golden not) | M | Golden keyed by image digest; new digest ⇒ new golden; old parked goldens GC'd |

---

## 10. Open questions

1. **Golden pool sizing** — per (image × concurrency) memory budget on desktops; park-after-idle timeout.
2. **Acquiring inside or outside the VM?** v1: acquisition (gix/registry fetch) stays host-side per GD-17 and the tree enters RO — revisit only if a language's acquisition is inseparable from its build tool.
3. **StaticParser tier on the desktop** — keep the in-process fast path (SV-5) or unify everything into the VM once P0 latency numbers exist?
4. **Fleet streaming** — extend `compiler-wire` with the `ir-stream` frames daemon→INDEX writer so registry publishes also record incrementally, or keep fleet batch (full artifacts) forever? Decide after P2 numbers.
5. **Windows** — WHP support exists upstream; does this pull §21-Q9 (Windows desktop) forward?
6. **CUDA/GPU passthrough** (smolvm has it) — irrelevant to compile today; note for future embedding workloads.

---

*End of SMOLVM-PLAN Rev 1. Upstream evidence: smol-machines/smolvm @ 56bb13b (verified against source, §1). Local evidence: `workspace/nudox-ir-vcs/repo.rs` (open/record_generation/materialize_index_incremental at lines 160/259/824), `workspace/compiler/sandbox/` (Cage/budget/typestates), LIBRARIFICATION-PLAN Rev 2 §12/§14/§17, DAEMON-PLAN §2.2/Phase-7.*
