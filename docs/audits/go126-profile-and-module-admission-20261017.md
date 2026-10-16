# Go 1.26 source and authority admission

This work has two source stages. The first adds the explicit Go 1.26 profile;
it does not establish Go 1.26 application indexing. The second must propagate
the admitted module language through fresh/reused source frontiers and durable
file records before application indexing is claimed. Neither stage has native
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

The existing local index scanner's path-only Go 1.25 default is not a module
version proof. Stage two must resolve each source from bounded, captured
go.mod bytes and retain that literal version across cold reopening. Merely
changing the path default would misstate older module semantics and is not the
index implementation proposed here.
