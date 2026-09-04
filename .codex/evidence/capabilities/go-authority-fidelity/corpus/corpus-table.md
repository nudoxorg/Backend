| purl | outcome | entities | wall seconds | oracle seconds | asserted symbols | source-truth corrections |
|---|---:|---:|---:|---:|---|---|
| golang:github.com/google/uuid@v1.6.0 | CapacityTerminal | 411 | 1.733 | 1.084 | UUID, New, Parse, NewString, FromBytes | none |
| golang:gopkg.in/yaml.v3@v3.0.1 | CapacityTerminal | 1774 | 4.293 | 0.599 | Node, Decoder, Encode, Decode, Unmarshal | none |
| golang:github.com/gorilla/mux@v1.8.1 | CapacityTerminal | 694 | 2.469 | 1.178 | Router, NewRouter, Handle, ServeHTTP, Use | none |
| golang:github.com/google/go-cmp@v0.6.0 | CapacityTerminal | 2461 | 4.305 | 0.568 | Comparer, Options, Diff, Equal, FilterPath | none |
| golang:golang.org/x/sync@v0.10.0 | CapacityTerminal | 334 | 1.407 | 0.800 | Group, Go, TryGo, Wait, SetLimit | none |
| golang:golang.org/x/mod@v0.17.0 | CapacityTerminal | 2284 | 6.326 | 0.959 | File, Parse, Format, AddRequire, SortBlocks | none |
| golang:golang.org/x/time@v0.5.0 | Complete | 253 | 1.659 | 0.557 | Limiter, NewLimiter, Allow, Reserve, Wait | none |
| golang:golang.org/x/tools@v0.30.0 | FactCapacity | 16384 | 20.830 | 1.934 | Pass, Analyzer, Inspect, Reportf, ResultOf | typed terminal: demand note 22,390 facts exceeds 16,384 envelope |
| golang:github.com/pelletier/go-toml/v2@v2.2.2 | CapacityTerminal | 3135 | 5.662 | 0.939 | Encoder, Decoder, Marshal, Unmarshal, Encode | primary corrected marshal.go -> decode.go |
| golang:github.com/oklog/ulid/v2@v2.1.0 | CapacityTerminal | 257 | 1.010 | 0.461 | ULID, Make, Parse, Entropy, Time | none |
| golang:github.com/cespare/xxhash/v2@v2.3.0 | Complete | 159 | 1.178 | 0.654 | Digest, Sum64, Write, Reset, BlockSize | none |
| golang:github.com/BurntSushi/toml@v1.4.0 | CapacityTerminal | 1624 | 2.349 | 0.546 | MetaData, Decode, DecodeFile, Primitive, Undecoded | case-escaped proxy path verified |
| golang:github.com/zeebo/xxh3@v1.0.2 | CapacityTerminal | 283 | 1.177 | 0.509 | Hash, HashSeed, Hasher, Write, Sum64 | primary corrected xxh3.go -> hash64.go |
| golang:github.com/minio/highwayhash@v1.0.2 | Complete | 184 | 1.294 | 0.691 | New, New64, Write, Sum, Reset | HighwayHash corrected to New64; source has no HighwayHash symbol |
| golang:github.com/julienschmidt/httprouter@v1.3.0 | Complete | 256 | 1.759 | 1.170 | Router, GET, Lookup, ServeHTTP, RedirectFixedPath | none |
| golang:github.com/go-chi/chi/v5@v5.0.12 | CapacityTerminal | 1162 | 3.617 | 1.187 | Router, NewRouter, Use, Route, Mount | primary corrected mux.go -> chi.go |
| golang:github.com/rs/zerolog@v1.33.0 | CapacityTerminal | 2987 | 5.387 | 1.089 | Logger, New, With, Info, Msg | primary corrected zerolog.go -> log.go |
| golang:github.com/davecgh/go-spew@v1.1.1 | CapacityTerminal | 571 | 1.446 | 0.585 | ConfigState, Sdump, Dump, Fdump, NewDefaultConfig | primary corrected spew.go -> spew/spew.go |
| golang:github.com/pkg/errors@v0.9.1 | CapacityTerminal | 263 | 1.406 | 0.564 | New, Errorf, Wrap, Cause, WithStack | synthesized go.mod |
| golang:github.com/mitchellh/go-homedir@v1.1.0 | Complete | 27 | 1.185 | 0.570 | Dir, Expand, DisableCache, Reset | synthesized go.mod |
| golang:rsc.io/quote@v1.5.2 | Complete | 18 | 1.038 | 0.499 | Hello, Glass, Go, Opt | removed nonexistent Con; source has four functions |
