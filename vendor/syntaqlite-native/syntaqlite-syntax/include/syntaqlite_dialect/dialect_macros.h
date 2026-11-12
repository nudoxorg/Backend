// Copyright 2025 The syntaqlite Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0.

// Compile-time / runtime gating macros for version and cflag checks.
//
// When SYNTAQLITE_SQLITE_VERSION is defined (compile-time pinning), these
// expand to integer constants and the compiler eliminates dead branches.
// When not defined, they check through the runtime config pointer.
//
// Include this header in .c files that perform version/cflag gating;
// do not expose it in public headers.

#ifndef SYNTAQLITE_INTERNAL_DIALECT_MACROS_H
#define SYNTAQLITE_INTERNAL_DIALECT_MACROS_H

// True if the target version is older than `ver`.
#ifdef SYNTAQLITE_SQLITE_VERSION
#define SYNQ_VER_LT(env, ver) (SYNTAQLITE_SQLITE_VERSION < (ver))
#else
#define SYNQ_VER_LT(env, ver) ((env)->sqlite_version < (ver))
#endif

// True if cflag at index `idx` is set in the env.
//
// When SYNTAQLITE_SQLITE_CFLAGS is defined (compile-time cflag pinning),
// reads from the synq_pinned_cflags struct built in sqlite_cflags.h from
// individual SYNTAQLITE_CFLAG_* defines. The compiler constant-folds the
// bit extraction and eliminates dead branches.
#ifdef SYNTAQLITE_SQLITE_CFLAGS
#define SYNQ_HAS_CFLAG(env, idx) synq_has_cflag(&synq_pinned_cflags, (idx))
#else
#define SYNQ_HAS_CFLAG(env, idx) synq_has_cflag(&(env)->cflags, (idx))
#endif

#endif  // SYNTAQLITE_INTERNAL_DIALECT_MACROS_H
