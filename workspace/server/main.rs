//! Binary entrypoint: build the `ServerConfiguration`, initialise tracing,
//! construct the `Server` over its (already-verified) backing infrastructure,
//! and serve until shutdown.
