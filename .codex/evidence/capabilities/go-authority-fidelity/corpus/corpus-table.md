| purl | outcome | entities | wall seconds | oracle seconds | asserted symbols | source-truth corrections |
|---|---:|---:|---:|---:|---|---|
| golang:github.com/google/uuid@v1.6.0 | CapacityTerminal | 411 | 1.378 | 0.626 | UUID, New, Parse, NewString, FromBytes | none |
| golang:gopkg.in/yaml.v3@v3.0.1 | CapacityTerminal | 1774 | 1.989 | 0.493 | Node, Decoder, Encode, Decode, Unmarshal | none |
| golang:github.com/gorilla/mux@v1.8.1 | CapacityTerminal | 694 | 1.704 | 0.731 | Router, NewRouter, Handle, ServeHTTP, Use | none |
| golang:github.com/google/go-cmp@v0.6.0 | CapacityTerminal | 2461 | 2.444 | 0.525 | Comparer, Options, Diff, Equal, FilterPath | primary corrected cmpopts/compare.go -> cmp/compare.go |
| golang:golang.org/x/sync@v0.10.0 | CapacityTerminal | 334 | 1.334 | 0.708 | Group, Go, TryGo, Wait, SetLimit | none |
| golang:golang.org/x/mod@v0.17.0 | CapacityTerminal | 2284 | 3.116 | 0.715 | File, Parse, Format, AddRequire, SortBlocks | primary corrected module.go -> modfile/read.go |
| golang:golang.org/x/time@v0.5.0 | Complete | 253 | 1.049 | 0.443 | Limiter, NewLimiter, Allow, Reserve, Wait | none |
| golang:golang.org/x/tools@v0.30.0 | AuthorityRefusal | 0 | 4.754 | 1.726 | Pass, Analyzer, Inspect, Reportf, ResultOf | typed refusal: package load rejects internal/tokeninternal invalid array length under pinned Go toolchain |
| golang:github.com/pelletier/go-toml/v2@v2.2.2 | CapacityTerminal | 3135 | 4.925 | 0.838 | Encoder, Decoder, Marshal, Unmarshal, Encode | primary corrected marshal.go -> decode.go |
| golang:github.com/oklog/ulid/v2@v2.1.0 | CapacityTerminal | 257 | 1.303 | 0.622 | ULID, Make, Parse, Entropy, Time | none |
| golang:github.com/cespare/xxhash/v2@v2.3.0 | Complete | 159 | 1.306 | 0.515 | Digest, Sum64, Write, Reset, BlockSize | none |
| golang:github.com/BurntSushi/toml@v1.4.0 | CapacityTerminal | 1624 | 2.154 | 0.560 | MetaData, Decode, DecodeFile, Primitive, Undecoded | case-escaped proxy path verified |
| golang:github.com/zeebo/xxh3@v1.0.2 | CapacityTerminal | 283 | 1.296 | 0.491 | Hash, HashSeed, Hasher, Write, Sum64 | primary corrected xxh3.go -> hash64.go |
| golang:github.com/minio/highwayhash@v1.0.2 | Complete | 184 | 1.188 | 0.481 | New, New64, Write, Sum, Reset | HighwayHash corrected to New64; source has no HighwayHash symbol |
| golang:github.com/julienschmidt/httprouter@v1.3.0 | Complete | 256 | 1.407 | 0.726 | Router, GET, Lookup, ServeHTTP, RedirectFixedPath | none |
| golang:github.com/go-chi/chi/v5@v5.0.12 | CapacityTerminal | 1162 | 2.002 | 0.796 | Router, NewRouter, Use, Route, Mount | primary corrected mux.go -> chi.go |
| golang:github.com/rs/zerolog@v1.33.0 | CapacityTerminal | 2987 | 4.415 | 0.720 | Logger, New, With, Info, Msg | primary corrected zerolog.go -> log.go |
| golang:github.com/davecgh/go-spew@v1.1.1 | CapacityTerminal | 571 | 1.344 | 0.537 | ConfigState, Sdump, Dump, Fdump, NewDefaultConfig | primary corrected spew.go -> spew/spew.go |
| golang:github.com/pkg/errors@v0.9.1 | CapacityTerminal | 263 | 0.983 | 0.453 | New, Errorf, Wrap, Cause, WithStack | synthesized go.mod |
| golang:github.com/mitchellh/go-homedir@v1.1.0 | Complete | 27 | 0.923 | 0.446 | Dir, Expand, DisableCache, Reset | synthesized go.mod |
| golang:rsc.io/quote@v1.5.2 | Complete | 18 | 1.147 | 0.470 | Hello, Glass, Go, Opt | removed nonexistent Con; source has four functions |
