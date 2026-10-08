# Virtual Cargo workspace checkpoint

Reviewed integration base: `88263669f3cfd44a8db3bece8118763bbbd6a94a`.
Source atom: `c835cf09bddb8eef3e8bfa9a181df24137eed935`, replaying
`d65fdc7a0c5f141219a628dde48a83118d1ab7b3` without conflicts. All three
changed files have exactly the native-tested before and after blob identities;
Cargo.lock remains `de731929bbf72c5220e59c0543aaddd06fcc2bc16899741a9d6cb5546e60780e`.
The index projection and runtime checkpoint files outside these three paths
remain unchanged. The user's separate Nix edit was not touched.

A Cargo root is now classified as a package or a virtual workspace. A valid
virtual workspace contributes no invented package identity, version, exports,
or dependency facts. Members retain their own manifest facts and dependency
edges. An operation that requires an actual package receives an explicit
member-selection error at the virtual root. Staged members continue to resolve
under a virtual workspace wrapper.

Four focused backend-local-service library tests passed on h16001mac against
the exact source d65fdc7a: manifest classification, root/member dependency
facts, public search projection of member declarations, and staged member
resolution. Each ran one test with zero failures or ignored tests. The public
search test uses a constructed ViewRoot; it is not native Rust compilation or
acceptance of a real repository. No full library suite, Clippy, app build,
installer, or production-readiness claim follows from this checkpoint.

The remote Mach-O arm64 test executable is
`backend_local_service-fb339fe309d09189`, SHA256
`2f220c5a174bbb50fe8c70479a4a87f2c8a83cc519428791d5e825a801f20dc0`,
723,284,824 bytes. Root independently checked 43 retained file bindings, exact
test results, successful fresh fleet admissions, kernel-wait retirement, empty
leases, unchanged source and unchanged build graph. The large remote executable
was not copied or independently rehashed locally. Cargo ran in verbose mode
without JSON artifact events: the original supervisor's incomplete artifact
mapping is preserved, with four separate crosswalks binding exact Running
lines and results to the recorded executable identity.

The evidence is in
`docs/operations/evidence/virtual-cargo-manifest-20261008`. Reproduce Root's
byte/result/source-map audit from a checkout containing the source atom:

```sh
python3 docs/operations/evidence/virtual-cargo-manifest-20261008/root-audit.py \
  --evidence docs/operations/evidence/virtual-cargo-manifest-20261008/native \
  --filemap docs/operations/evidence/virtual-cargo-manifest-20261008/source-filemap.json \
  --repo . --integration-ref c835cf09bddb8eef3e8bfa9a181df24137eed935 \
  --output /tmp/nudox-virtual-workspace-audit.json
```

The broader TypeScript/Python/Go/Rust readiness work remains open. In particular,
request-owned deferred Rust toolchain authority and actual native Rust service
indexing controls are separate candidates; this checkpoint does not claim
their unrun results.
