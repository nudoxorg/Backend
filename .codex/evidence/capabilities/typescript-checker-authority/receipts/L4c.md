# L4c receipt

```text
baseline: f1d49bf57 (parent e1b1131a)
card: L4c-r2

command: curl -L --fail --silent --show-error https://registry.npmjs.org/zod/-/zod-3.25.76.tgz -o zod.tgz
sha256: 9e1f1a05f0dd0c1dab64ee91ceb9bf55cd44d35368c70edda80a2fdc70a88377
tar entry: package/index.d.ts
resolved entry: ./index.d.cts

command: curl -L --fail --silent --show-error https://registry.npmjs.org/@babel/parser/-/parser-7.26.8.tgz -o parser.tgz
sha256: d35d24bb0029bdd4932b1705b936b60002f9faa1ac0f166c3cf0ff4b23a7156e
tar entry: package/typings/babel-parser.d.ts
resolved entry: ./typings/babel-parser.d.ts

command: cargo test -p compiler-driver --test typescript_purl_lifecycle -- --nocapture
result: 4 passed; 0 failed
zod index rows: exact=2 lexical=2
babel index rows: exact=226 lexical=226
zod generation 1: pinned_root=02eb5d139833035df028acb18775a0d034f901cb105b01df3138f7f9666dd7d8 dep_set=043fa909b613de2e15684c4a8a29b3d230ef28de1459bb671737ee5a90891c12
zod generation 2: pinned_root=020d2bb730a95ab5cb10bb6bda4178a48456b5839e62d37615397fb8a1f01f50 dep_set=045604d3733c17c42ae98df5b46ccd4d21ca57091299be231b5429699323760e
babel generation 1: pinned_root=0271269d76433fdb2e33fd6fc90d82606bc4ed1d4b23de807c677394d2248823 dep_set=04899a3a80dac21a17d1820d3dc4e7ee5b03ca8bed3e0ac653cd872dbb1bb9e3
babel generation 2: pinned_root=023019b246b0371bc286a5084a23e97bfa7f16fff2505a811875a9c12e5b973f0 dep_set=0483842eaac8c5d66d08d92b73ccde6cb2639dfec6c258a901cbc5eb8ce46d2f
2 MiB falsifier: stack_size=2097152 falsifier=ok

build scout attempt 1: npm:esbuild@0.24.2 — registry tarball contains declaration files; not a no-declaration build-leg candidate
build scout attempt 2: npm:prettier@3.5.3 — no declaration build script suitable for this leg; not a candidate
build-leg command: cargo test -p compiler-driver --test typescript_purl_lifecycle local_build_artifact_leg_requires_the_build_step -- --nocapture
build-leg result: MissingTypes before node build.js; node build.js exit status 0; resolved ./index.d.ts
build-leg fixture manifest sha256: 0bedf6d0f00d65a07cdcda60ae1de8911c76d9ab2e6cd725e6e1879367eadefb

lockfile: compiler-driver dev-dependency serde_json workspace=true; Cargo.lock compiler-driver row includes serde_json
regression: typescript_lower 31 passed; typescript_render 14 passed; typescript_authority 9 passed; typescript_package 1 passed
regression: compiler-driver lib check red (pre-existing lower/rust test compile errors; 11 errors)
```
