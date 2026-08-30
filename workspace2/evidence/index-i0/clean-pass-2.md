# I0_CLOSURE_PASS_2

- source candidate: `526ad7a7e5bae982ff528132b6c6775d4f5ddbc9`
- source tree: `f50541fef7b9f82adda86f2412de943967316730`
- status before/after: `` / ``
- result: `PASS`
- raw summary SHA-256: `cbf0830385e0dc165bafecd268d6e0654758e8659f17ee6a6409821f96b03e78`
- raw logs retained through receipt review under `/private/tmp/nudox-i0-gate-pass2-71890a1a`

| # | exact argv | exit | bytes | raw-log SHA-256 |
|---:|---|---:|---:|---|
| 1 | `cargo fmt --manifest-path workspace2/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 2 | `cargo test --manifest-path workspace2/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 20984 | `7d92365930d8970278d282bd22482bab44b91196c0b5089426af43de2007004d` |
| 3 | `cargo test --manifest-path workspace2/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 2090 | `499b6faf9ca85442bf6b02fc4d4ba45c8c042234d6e750de6e668fe5efef99cc` |
| 4 | `cargo clippy --manifest-path workspace2/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `4cf86278c1e6d659943a46a3d7e3da429ac15be90f0f17742e24ef85228e7d3e` |
| 5 | `cargo doc --manifest-path workspace2/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 166 | `e7069ed59c16e4025dca56f5489131dc6c370481a762f622c48d35789f8f5fb4` |
| 6 | `cargo fmt --manifest-path workspace2/adapters/observability/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 7 | `cargo test --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 1123 | `0152cd37da3b074413d9838dda670e669f99092ef96af46c537d8e71ef686901` |
| 8 | `cargo test --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 227 | `ba60d9d59ac9667a40a407e08e17a609143a39b098edc94e550bb0e9646a254f` |
| 9 | `cargo clippy --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `732186322402325172940531d8d6d7cfac85dc902585b2bef6715ad6839dacfd` |
| 10 | `cargo doc --manifest-path workspace2/adapters/observability/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 163 | `347de44607b510e3b110976d76b131258bc9699efdfb0267c3f276c7df7b87d3` |
| 11 | `cargo fmt --manifest-path workspace2/domains/ir/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 12 | `cargo test --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 588 | `39876481592efd0b53f3c3b0af477be2eeacd3b68996dc68ffdd78c5484a1da1` |
| 13 | `cargo test --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 293 | `13abbe47e7ee81628207dc37de0559514f49f4c5775b203c59fc9969ec18fe53` |
| 14 | `cargo clippy --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `99f71393b9903dca098310bb446022a4bdee195ea9996f351a787195555210a6` |
| 15 | `cargo doc --manifest-path workspace2/domains/ir/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 150 | `35d15a74feab85089335feaf809ef9a929cdbdecdeeba2adeb962b56a8b52037` |
| 16 | `cargo fmt --manifest-path workspace2/planes/compiler/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 17 | `cargo test --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 1089 | `f04d81941b9e7f55389f1159e6677605899ca1592b9ebfb09082ed95ed5b196d` |
| 18 | `cargo test --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 462 | `c18e27a9b14e93f96b32cb85b93dd8679c0ec30a6d0a066703c91fd0fb360a36` |
| 19 | `cargo clippy --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `ee1d3fffa2022e962e63e0dbe4d35c9c198450a20c29fc09030f635dbb38bfcd` |
| 20 | `cargo doc --manifest-path workspace2/planes/compiler/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 175 | `c5031d1f78716f590b1c1a27078587353c3cd032ba508fc758bd7aa1d64513e5` |
| 21 | `cargo fmt --manifest-path workspace2/planes/index/Cargo.toml --all -- --check` | 0 | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| 22 | `cargo test --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --all-targets --no-fail-fast -- --test-threads=1` | 0 | 773 | `1e3430667dc8d53bcb94d2bd708eb8b13ebdd4087a969ab23206bd8e7962df14` |
| 23 | `cargo test --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --doc --no-fail-fast` | 0 | 441 | `872f2ef4c6dca2764a8f6d941dd9745850baa54b79c2bb5ed744f7bea2bca797` |
| 24 | `cargo clippy --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --all-targets -- -D warnings` | 0 | 72 | `ee1d3fffa2022e962e63e0dbe4d35c9c198450a20c29fc09030f635dbb38bfcd` |
| 25 | `cargo doc --manifest-path workspace2/planes/index/Cargo.toml --offline --locked --workspace --no-deps` | 0 | 153 | `b8e6294f9673da1f9359ff77cf6f51271c5da21c439a30a32de4d93dd32cb6fa` |
