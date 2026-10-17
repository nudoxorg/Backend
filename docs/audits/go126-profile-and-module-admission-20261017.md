# Go 1.26 source and authority admission

This work has two source stages. The first adds the explicit Go 1.26 profile;
it does not establish Go 1.26 application indexing. The second propagates
the admitted module language through fresh/reused source frontiers and durable
file records. Application indexing still requires runtime validation. Neither stage has native
validation in this source-only worktree.

The genuine retained Gitea v1.26.0 checkout is commit
`9b9d1e31aa0bd2e2bb87d2ba329521ed3c5084bd`, tree
`1daa1f138d3065c26a155fc229c7fff3fe0478d2`; its go.mod requires Go 1.26.2.
The earlier retained production setup receipt ran
`/nix/store/1xklj6dmgcpgz60c9w2ynaq7ynzfsrrq-go-1.26.4/bin/go version`
and reported `go version go1.26.4 linux/amd64`. This is historical evidence,
not fresh executable/GOROOT admission or a current application pass.

Stage one appends profile discriminant 4 without changing Go 1.22–1.25 codes.
Canonical byte/string decoding, JSON serialization, capability display,
transport parsing, and native `-lang=go1.26` agree. Go 1.27 and later remain
typed unsupported manifest/profile errors. Current product advertisement uses
1.26; exact older Go capability rows remain separately addressable, with their
own profile/environment/recipe identity. Recipe tests distinguish 1.25/1.26.
Missing go directives keep the explicit historical 1.25 default, independently
of a suggested toolchain directive.

The Go authority continues to use its selected genuine executable and complete
GOROOT content identity, immutable helper source identity, and confined module
witness. Its existing child environment retains `GOTOOLCHAIN=local`,
`GOPROXY=off`, `GOSUMDB=off`, and the explicit module/build caches. No compiler
is downloaded or substituted. The new real-helper integration selector requires
an explicitly admitted compiler; it checks that actual selected output retains
the exact Go 1.26.2 module requirement. Missing tooling fails, never skips.

Stage two retains the path-only Go 1.25 default for historical records and
explicitly observed absent module directives. It parses go.mod once from the
bounded, confined configuration byte pipeline before constructing source keys.
Unique ancestor candidates are bounded and include ignored go.mod files. Safe
NotFound observations require captured parent-directory revisions; unreadable,
symlinked, or unobserved boundaries cannot silently acquire the legacy default.
Positive module inputs participate in the final file/configuration revision
fence. A scan whose bounded Go configuration capture is incomplete is refused.

Each file's analysis identity binds only its nearest owning go.mod content and
literal language profile plus the intervening observed negative ancestor chain.
Adding or editing another module does not invalidate that file's source row.
The complete configuration capture remains a mutation fence; existing workspace,
compiler, GOROOT, and helper authority admission remains required by compilation.
This source-analysis identity does not substitute for native dependency authority.

The established manifest classifier maps explicit Go minors up to 1.22 to the
minimum Go 1.22 profile bucket; this change preserves that historical policy.
The admitted profile is exact for supported Go 1.22–1.26 minors, while the
original directive bytes remain bound by the captured go.mod content identity.
It does not preserve an older directive string as a new language profile.

New PSRH file records retain this admitted Go profile, optional source-fact
identity, exact declaration bindings, and containment. Every compaction probe
includes the new encoding overhead. Historical records without the field keep
their original encoding and reopen as the established Go 1.25 legacy default;
cold reopening never consults a current go.mod. Fresh, reused, and delta-frontier
compiler inputs use the captured/stored literal profile. Semantic completion is
also matched against this literal profile rather than the extension default.

The selected native module metadata is checked before package authority is
returned: a module requiring Go 1.26 cannot enter a requested Go 1.25 authority,
and 1.27 cannot enter 1.26. Older module requirements remain compatible with
explicit higher-version API requests; indexed files select their exact captured
module profile. No current toolchain download or native execution was performed.

Source regressions cover stable codecs, profile boundaries, native flags,
actual-helper module metadata, module-local keys, explicit absence admission,
ignored module creation/deletion and symlink refusal, fresh/reused/cold rows,
manifest-only changes, nested module deletion, and literal-profile completion.
Rust formatting parse checks and diff checks are not type checks or test passes.
