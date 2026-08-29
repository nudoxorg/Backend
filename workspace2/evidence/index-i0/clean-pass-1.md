# I0_CLOSURE_PASS_1

- source candidate: `526ad7a7e5bae982ff528132b6c6775d4f5ddbc9`
- source tree: `f50541fef7b9f82adda86f2412de943967316730`
- status before/after: `` / ``
- result: `PASS`
- raw summary SHA-256: `107d7035dfd510ceaa653997caebeb9b176c3f604c3c6efdb7a92548317c9c7d`
- raw logs retained through receipt review under `/private/tmp/nudox-i0-gate-pass1-71890a1a`

| # | exact argv | exit | bytes | raw-log SHA-256 |
|---:|---|---:|---:|---|
| 1 | `cargo fmt --manifest-path workspace2/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 2 | `cargo test --manifest-path workspace2/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 20984 | `3288eb8e84d2c5afe308dc125188cee29db9bb5aa868fa0a00a87bc85ed92fd0` |
| 3 | `cargo test --manifest-path workspace2/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 2090 | `99ec60e651fa91114b05b40c913ebfa068e55051ac3bf94c7d7362b0552698b3` |
| 4 | `cargo clippy --manifest-path workspace2/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `d7dae192715495fa89ccbe279af3efb9792c1dae1768228e369035e68a1df673` |
| 5 | `cargo doc --manifest-path workspace2/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 166 | `8f476adbaa0f0a545b8ccdc59182deaef5c11c937e6fc5dfa63fab95bf0e1054` |
| 6 | `cargo fmt --manifest-path workspace2/adapters/observability/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 7 | `cargo test --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 1123 | `5fcf130622df02a3f1eed6b8de932d402792c2137573a11992cb35c6d706161d` |
| 8 | `cargo test --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 227 | `c3fe5ee68e0b52c2ea8152cf1f764a2c7a57c0b3800850f4c9f9e0451062dc1b` |
| 9 | `cargo clippy --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `c92508a29848820f033cd2ea015c85dde0953c88b07cc967ce34b7c08f610549` |
| 10 | `cargo doc --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 163 | `cc9d0cfb6ce0da29b8b73c285b38b6d037e73f94901fe6af619c2e45de8d5e27` |
| 11 | `cargo fmt --manifest-path workspace2/domains/ir/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 12 | `cargo test --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 588 | `39876481592efd0b53f3c3b0af477be2eeacd3b68996dc68ffdd78c5484a1da1` |
| 13 | `cargo test --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 293 | `42a33c6ba9ce0a8652f6f0e5e52a43e73c8fe4511c72e7ea4d99a26081186bc3` |
| 14 | `cargo clippy --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `736e2582f563605dd272e5fb977840b0c0767377d27b6940c440caf75eec7157` |
| 15 | `cargo doc --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 150 | `8cdc7fe2ed391b2f15cd15f153875c03ed697197b2221d3e636533474493b67e` |
| 16 | `cargo fmt --manifest-path workspace2/planes/compiler/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 17 | `cargo test --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 1089 | `83a51b6cbe2b7b2401113cf7a30c32fc7b633631f7852a6869b9b0572c2361f1` |
| 18 | `cargo test --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 462 | `a79a9104c2620d19da174360da064e6376effee91d33f7a914e16a682b5bfc2e` |
| 19 | `cargo clippy --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `02db5a66fa0c60ca8618bd08aac556eac4f3ec863e8630a0d353dba6a0f6e126` |
| 20 | `cargo doc --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 175 | `271c68c618e3431b00e6c70002b5d1ace6e09ef16cc02c1932a7c7236ef9b737` |
| 21 | `cargo fmt --manifest-path workspace2/planes/index/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 22 | `cargo test --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 773 | `1e3430667dc8d53bcb94d2bd708eb8b13ebdd4087a969ab23206bd8e7962df14` |
| 23 | `cargo test --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 441 | `5b18937b96e9cb669f75e9d33ca8df9627157b69a00a3169d3bba102d71aea74` |
| 24 | `cargo clippy --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `ee1d3fffa2022e962e63e0dbe4d35c9c198450a20c29fc09030f635dbb38bfcd` |
| 25 | `cargo doc --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 153 | `60459050468c95b3261a656342d42b2dda2b9ad8fa4072635c3a7133e8c9cc87` |
