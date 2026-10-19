# macOS readiness checkpoint

Updated 2026-10-19, 13:07 UTC. This report tracks the October 18 macOS QA findings and the current Report 29 evidence. Overall readiness remains open; Linux source and installer results do not establish macOS GUI behavior.

## Source identities and tested binaries

The current canonical source is `f290743905fd2188f9aeb3dc4b0da91e896df480`, tree `25ad3a44bbc96aabf40f12988de720779bb0939e`. It includes PR611 Go test unwrap cleanups, PR612’s authored-coverage directory correction, and PR613’s Linux519 catalog source. These changes have not had a native test run on this canonical revision.

The latest complete default-feature Linux application trio is from the earlier source `ee69b851fb23d987ba828b554679ba2eb223efcc`, tree `dbd4b03974297c4f7d41e16835cfc919f6a46547`. Root independently replayed its Cargo metadata, compilation, four startup checks, complete source preservation, copied artifacts, and normal scope closure. These are diagnostic development-profile binaries, not optimized releases.

| Role | Bytes | SHA-256 |
| --- | ---: | --- |
| CLI | 204,635,392 | `a152574cd69f65a30680246f66d00ea6c13908c4e6ac19322d7cd875be74ccfd` |
| locald | 435,747,104 | `5abf91bde90e41cdbf6474724219ecd1a022cac037d029924679b2946df74359` |
| MCP | 153,271,032 | `c7bc2dc96a1291ce4839201f58fc32402037001bb64d0e87e42d781045477a70` |

The strict replay is `ROOT-COMBINED40-INDEPENDENT-STRICT-REPLAY01.json`, SHA-256 `496ad70433e78579a1f62e3ca087fd55ade5caa7164e910ff357f777246d3473`. Startup success does not establish indexing, search, GUI, or all-tools acceptance.

## Current application evidence

The current-source HTTPie run is now closed and independently replayed. Its strict record is `ROOT-HTTPIE-CURRENT40-INDEPENDENT-STRICT-REPLAY01.json`, SHA-256 `a8700b8ad51ea8919946577f2b639a53c5bfdfb3a71bec0bbb2b9d6a16856a88`. The replay verifies all 18 useful routes, all 18 ledger checks, and all requested runtime phases over 350 raw tool calls. Root separately reviewed the source oracles and adopted the scoped result in `ROOT-HTTPIE-CURRENT40-SCOPED-ACCEPTANCE01.json`, SHA-256 `2dfca0c59aa571e6eb907f8631dbb0faba74ebc80cbc70848fe0b5511858fabd`; scoped CLI/MCP acceptance is true for this 265-file, 133-Python-file project. The measured warm full search took 1.31 seconds; the cold full search took 15.50 seconds under a 30-second bound. The first cold CLI `add . --wait` completed in 86.44 seconds. A separate first full lexical-search multi-command chain took 17.79 seconds. Individual `backend.search` RPCs were 1.65–65.04 ms; `backend.index_search` was 8.36 seconds. These timings describe this HTTPie case and its observed environment, not a macOS guarantee. Vector preparation was unconfigured, and CLI/MCP active overlap is unproven.

The public Ubuntu 24.04 x64 current519 installation passed four installations, 16 startup checks, four PATH checks, and real Python CLI/MCP reads. Strict replay: `ROOT-CURRENT519-PUBLIC-UBUNTU-INDEPENDENT-STRICT-REPLAY01.json`, SHA-256 `8b45dee03aed3981265b8c39c0eb6e1a0feed36c7842556a1406f518ed1aefd0`. The tested source is `519a137628a650cac67219f23645a50d091617e2`, tree `34e59211b2544114b9898afea7589cfe266c4835`. This is installation evidence for that platform and source, not all-tools or GUI acceptance.

The generic preview catalog alias has been promoted and anonymously verified. Promotion result `ROOT-CURRENT519-PREVIEW-PROMOTION01/RESULT.json` records catalog SHA-256 `533dd156527d28fba5fe9580a9c1b60d62410df4fe124c30bfd869c82abfb986`. The generic Ubuntu24 `install.sh` case has now passed four installs, 16 startups, four PATH checks, full installed-file rehash, all nine release assets, and Python CLI/MCP reads. Strict replay `ROOT-CURRENT519-GENERIC-UBUNTU-INDEPENDENT-STRICT-REPLAY01.json` is SHA-256 `2cefa1b807282c48c5dc1b88f3653bcedd1a753e7f6b7056c13694c1bcc931e5`; source `519a137628a650cac67219f23645a50d091617e2`, tree `34e59211b2544114b9898afea7589cfe266c4835`. The alias remains a diagnostic preview, not production-ready; embedding and all18 application acceptance are false. A generic-channel example is:
```sh
curl -fsSL https://github.com/nudoxorg/Backend/releases/download/nudox-preview-installer/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
nudox --version
```

GUI02 also closed with exit 1 before interaction checks. Root independently replayed `ROOT-PROD-EE69-GUI-RESUME02-INDEPENDENT-STRICT-REPLAY01.json`, SHA-256 `c882c18ab83481436f604946b6526fa0de20e3f833154dacc45fd0cbc9d97cc8`: the real loader succeeded, but the private runner's admitted library inventory omitted transitive `libXau.so.6`, `libXdmcp.so.6`, and `libxcb-xkb.so.1`. The existing quartet and full source were preserved; no GUI success is credited. GUI01's separate copy-helper failure remains recorded in `ROOT-PROD-EE69-GUI-FAILED01-INDEPENDENT-STRICT-REPLAY01.json`, SHA-256 `63db0ec93df497d5a9f12b71483c38b7796664f4cd0ca93319aae8928faccf89`. Four targets code-generated in 416.7 seconds in GUI01; GUI02 reused those actual binaries without recompilation. Default-discovery equivalence remains unproved. No macOS `.ips` report or crash stack is available, so the reported macOS crash's cause and resolution remain unknown.


## Open product findings

| Finding | What the evidence says | Remaining work |
| --- | --- | --- |
| Sidebar click differs from search-result click | The source repair is present in the tested ee69 source. GUI01/02 failed in private runner preparation before physical GUI acceptance. | Obtain a successful current GUI run and reproduce on macOS. |
| GUI and MCP shelves diverge | Source inspection identified different default store selection between GUI ambient startup and CLI/MCP project selection. | The authenticated default-discovery/open repair has not passed a same-owner default-discovery test. |
| macOS crash | No crash report or stack is available locally. | Reproduce on macOS or obtain the original crash artifact; cause remains unknown. |
| Rust crate coverage and large-blob admission | PR612 corrects the authored-coverage directory omission. The canonical f290 change has no native run yet. The new real 2,204-file / 53,040,364-byte crates case cancelled successfully, then stopped at a CLI/MCP join assertion: natural CLI discovery selected the Git-parent owner while the runner explicitly selected the crates owner for MCP. Full source stayed unchanged. | Correct this runner owner-context inconsistency and rerun; publication, journal reopening and positive source counts remain unproved on the real workspace. |
| Cal.com large-project scan | The 7,702-file case reached its 900-second limit after cancellation/retry. The observed terminal reason for the current long scan remains unknown; no cause is assigned. | Diagnose with the bounded private work observation and rerun only after the diagnosis is reviewed. Current large-source closure is unproved. |
| Go / PocketBase case | Strict replay `ROOT-POCKETBASE-COMBINED40-INDEPENDENT-STRICT-REPLAY01.json`, SHA-256 `4f5920d68ee62ba171c3f56eb41a62846ecdc462956580aeb1d49bd66b1818a9`, covers 100 calls across five routes; only cancellation was useful. The 900-second retry budget ended while Go 2/2 remained pending. The scope closed normally, but there is no Go compiler rejection finding and no application acceptance. | Keep the terminal reason separate from a compiler failure; full Go application and all18 acceptance remain false. |
| Direct picker | [PR614](https://dev.nudox.org/git/Nudox/Backend/pulls/614) is a non-draft review checkpoint at `d0f99f0be79a1b5371c85d163a5a73da541d6564`, tree `462f4602dd31a0cd460145b09d6e857d192bcae7`. Its three atomic commits contain the picker, opt-in Cal work diagnostics, and the 59-site test cleanup. | The 25 selected native tests across six artifacts remain unrun. The PR is unmerged; no new product acceptance credit. |

The PocketBase retry contains 45 TypeScript and 433 Go sources in 43 Go directories. The 900 seconds cover the complete retry, including substantial work before Go admission; there are no per-compiler CPU or exact phase timings. Source review found that each Go directory load requests dependency syntax even though output extraction consumes root syntax. A narrow root-syntax loader proposal and image-equivalence regressions are source-reviewed but have not run. They do not establish the cause or resolution of this timeout.

## Test results and source boundaries

There are **65 selected native test passes** across distinct source identities (44 earlier selected passes plus 21 Go native bodies). The added 21 passed on source `0816640f6681f2cda95ed3f8a53808c05463ba40`, tree `4ff39662c2a34b789f8ed8178f7c69390bcfae43`, with 0 failures and 0 ignored. These are separate-source results, not one 65-test run on f290 and not a full workspace suite.

| Selected slice | Tested source | Pass / fail / ignored |
| --- | --- | --- |
| Mac-report paths, journals, GUI regressions | `6e914a405df5c85b73157fca661c0399defe65b0` | 15 / 0 / 0 |
| Positive Rust source counts | `519a137628a650cac67219f23645a50d091617e2` | 3 / 0 / 0 |
| Go same-package authority reuse | `37199ca508d79f0a60230362b02bbef88e639c5c` | 6 / 0 / 0 |
| Duplicate diagnostic agreement | `f9d55b4b37e0a4b8cffe0b9ecaaa9b3f732cc6b4` | 3 / 0 / 0 |
| TypeScript synthetic-result identity and stability | `5751138bab9cee57d799b8e7c77bd40e7d36351d` | 7 / 0 / 0 |
| Tantivy reader retention | `5d99c2bbe7003e240a587135ccca5a8cbb258f85` | 10 / 0 / 0 |
| Go oracle native bodies | `0816640f6681f2cda95ed3f8a53808c05463ba40` | 21 / 0 / 0 |

The seven-test TypeScript slice retained the unchanged 766,861-byte PocketBase declaration file. Its Root replay is `1538e6981f76c5af1fffd8711c1b69090d16688752ea46a214bb9b86dd9009f1`. The ten Tantivy tests establish functional coverage, not large-project latency improvement; replay `894b6ee0a3e5180842cf4b061fa6868345c95b1c0b365d0f9eccde239cdf7512`.

Separately, **67 Python regressions** passed on their recorded Python 3.12 fixtures: Linux installer 32, checkpoint installer 6, package builder 28, and installer serialization 1. These are not native tests and give no application indexing or macOS credit.

Clippy04 completed on source `0816640f6681f2cda95ed3f8a53808c05463ba40`, tree `4ff39662c2a34b789f8ed8178f7c69390bcfae43`. Its strict replay, `ROOT-ILO-CURRENT-CLIPPY15-ACTUAL04-INDEPENDENT-STRICT-REPLAY01.json`, is SHA-256 `6407d1ed9fec51a922187511321f3ac13fdc7157d16aca9f3806e36d396d32a7`. All 21 selected Go native bodies passed (0 failed, 0 ignored, 0 unrun), but the broader Clippy command over 15 selected workspace packages with `--all-targets` exited 101 after 13,686 Cargo events. It reported 59 `clippy::unwrap_used` errors (one in `discovery/python_context.rs`, 58 in `runtime/lib.rs`) and 12,490 warnings: 11,327 from selected packages and 1,163 from dependencies, with 9,044 unique warning identities. This is not a clean Clippy result and does not validate f290. The separate Clippy59 cleanup SOURCE1 is source-reviewed (two files, five affected functions), but native validation remains unrun. PR611 changes remain untested on the current canonical source.

## Installer artifact and platform limits

The immutable Linux x64 diagnostic checkpoint is published from source `519a137628a650cac67219f23645a50d091617e2`, tree `34e59211b2544114b9898afea7589cfe266c4835`: [checkpoint release](https://github.com/nudoxorg/Backend/releases/tag/checkpoint-20261019-519a137628-linux-x64). It targets Ubuntu 24.04 x64 / glibc 2.39+, not macOS or every Linux distribution.

The archive is 176,255,054 bytes, SHA-256 `cdd644e31e2bca827d8601394ad9f28cdff46767ad6c8d74e9b8d80f5a6e7dee`; the installer SHA-256 is `4b167d4816ab4ff82e628acc62695ef7b68719e6078a4ffa0c297be8f0245edc`. Public assets matched the tested package. The public Ubuntu acceptance replay above covers installation, startup, PATH lookup, and Python CLI/MCP reads only. The generic alias points to a diagnostic preview. No current Mac bundle, configured embedding/vector acceptance, broad multi-project acceptance, or production readiness is established.

Heavy work ran on ILO/ILO PROD under managed fleet accounting and scoped permits; this report records evidence, not a new launch authorization. The 65 native passes across distinct sources, 67 Python passes, current HTTPie replay, public installer replay, GUI failures, Clippy results, and canonical f290 source each retain their separate source and acceptance boundaries.
