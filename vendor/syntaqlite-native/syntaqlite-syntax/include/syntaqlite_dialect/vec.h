// Copyright 2025 The syntaqlite Authors. All rights reserved.
// Licensed under the Apache License, Version 2.0.

// Header-only type-generic dynamic array.
// Works on any struct with {T *data; uint32_t count; uint32_t capacity;}.
//
// All mutating operations take a SyntaqliteMemMethods parameter for
// allocation. This lets the vec use the caller's configured allocator.

#ifndef SYNTAQLITE_EXT_VEC_H
#define SYNTAQLITE_EXT_VEC_H

#include <stdint.h>
#include <string.h>

#ifdef __cplusplus
extern "C" {
#endif

// Type constructor
#define SYNQ_VEC(T)    \
  struct {             \
    T* data;           \
    uint32_t count;    \
    uint32_t capacity; \
  }

// Zero-init
#define syntaqlite_vec_init(v) \
  do {                         \
    (v)->data = NULL;          \
    (v)->count = 0;            \
    (v)->capacity = 0;         \
  } while (0)

// Free + zero
#define syntaqlite_vec_free(v, mem) \
  do {                              \
    (mem).xFree((v)->data);         \
    (v)->data = NULL;               \
    (v)->count = 0;                 \
    (v)->capacity = 0;              \
  } while (0)

// Reset count, keep allocation
#define syntaqlite_vec_clear(v) \
  do {                          \
    (v)->count = 0;             \
  } while (0)

// Ensure capacity >= needed (capacity is always a power of two).
#define syntaqlite_vec_ensure(v, needed, mem)             \
  do {                                                    \
    if ((needed) > (v)->capacity) {                       \
      uint32_t _cap = (v)->capacity ? (v)->capacity : 16; \
      while (_cap < (needed))                             \
        _cap *= 2;                                        \
      (v)->data = (__typeof__((v)->data))(mem).xRealloc(  \
          (v)->data, (size_t)_cap * sizeof(*(v)->data));  \
      (v)->capacity = _cap;                               \
    }                                                     \
  } while (0)

// Append one element, grow if needed
#define syntaqlite_vec_push(v, val, mem)             \
  do {                                               \
    syntaqlite_vec_ensure((v), (v)->count + 1, mem); \
    (v)->data[(v)->count++] = (val);                 \
  } while (0)

// Element count
#define syntaqlite_vec_len(v) ((v)->count)

// Lvalue access to element at index
#define syntaqlite_vec_at(v, i) ((v)->data[i])

// Set count to n, discarding trailing elements
#define syntaqlite_vec_truncate(v, n) \
  do {                                \
    (v)->count = (n);                 \
  } while (0)

// Decrement count, evaluate to last element
#define syntaqlite_vec_pop(v) ((v)->data[--(v)->count])

// Bulk append via memcpy
#define syntaqlite_vec_push_n(v, src, n, mem)                               \
  do {                                                                      \
    uint32_t _n = (n);                                                      \
    syntaqlite_vec_ensure((v), (v)->count + _n, mem);                       \
    memcpy((v)->data + (v)->count, (src), (size_t)_n * sizeof(*(v)->data)); \
    (v)->count += _n;                                                       \
  } while (0)

#ifdef __cplusplus
}
#endif

#endif  // SYNTAQLITE_EXT_VEC_H
