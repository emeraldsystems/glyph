#ifndef GLYPH_THREAD_H
#define GLYPH_THREAD_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Opaque native-thread state. Its representation is never part of Glyph ABI. */
typedef struct GlyphThread GlyphThread;

/**
 * Invokes an owned, erased FnOnce environment.
 *
 * Once called, `entry` owns `env` and must release it according to the
 * callable ABI. The runtime never calls `drop_unstarted` after entry begins.
 */
typedef void (*GlyphThreadEntry)(void* env);
typedef void (*GlyphThreadDropUnstarted)(void* env);
/**
 * ABI-neutral adapter emitted once per concrete Glyph result type.
 *
 * `entry` consumes the callable environment and initializes `out_result`
 * exactly once before returning. The runtime owns that initialized result
 * until join transfers it or detached-state cleanup destroys it.
 */
typedef void (*GlyphThreadResultEntry)(void* invoke,
                                       void* env,
                                       void* out_result);
typedef void (*GlyphThreadDropResult)(void* result);

/**
 * Start a joinable native thread.
 *
 * Returns zero and writes an opaque handle on success. On every failure,
 * returns a negative errno-style value, leaves `*out` null, and calls
 * `drop_unstarted(env)` when a drop callback was supplied.
 */
int32_t glyph_thread_spawn(GlyphThread** out,
                           GlyphThreadEntry entry,
                           void* env,
                           GlyphThreadDropUnstarted drop_unstarted);

/**
 * Start a joinable thread whose callable returns a concrete value.
 *
 * The compiler-provided adapter hides scalar-versus-sret ABI details from C.
 * `result_size` bytes remain runtime-owned until successful typed join. On a
 * detached path, `drop_result` runs after the worker has initialized the slot.
 */
int32_t glyph_thread_spawn_result(GlyphThread** out,
                                  GlyphThreadResultEntry entry,
                                  void* invoke,
                                  void* env,
                                  GlyphThreadDropUnstarted drop_unstarted,
                                  size_t result_size,
                                  GlyphThreadDropResult drop_result);

/**
 * Join and consume a handle. The pointer is nulled only after pthread_join
 * succeeds; an error leaves it retryable.
 */
int32_t glyph_thread_join(GlyphThread** handle);

/**
 * Join and move a typed result into caller-owned uninitialized storage.
 * OS join errors leave the handle and runtime-owned result retryable.
 */
int32_t glyph_thread_join_result(GlyphThread** handle, void* out_result);

/**
 * Detach and consume a handle. The pointer is nulled only after pthread_detach
 * succeeds; an error leaves it retryable.
 */
int32_t glyph_thread_detach(GlyphThread** handle);

#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)

enum {
    GLYPH_THREAD_TEST_FAIL_ALLOC = 1,
    GLYPH_THREAD_TEST_FAIL_CREATE = 2,
    GLYPH_THREAD_TEST_FAIL_JOIN = 3,
    GLYPH_THREAD_TEST_FAIL_DETACH = 4,
};

typedef struct GlyphThreadTestLatch GlyphThreadTestLatch;

/* Debug-only, hidden hooks for deterministic runtime tests. */
int32_t glyph_thread_test_fail_next(int32_t operation, int32_t error_code);
int32_t glyph_thread_test_latch_create(uint32_t initial_count,
                                       GlyphThreadTestLatch** out);
int32_t glyph_thread_test_latch_count_down(GlyphThreadTestLatch* latch);
int32_t glyph_thread_test_latch_wait(GlyphThreadTestLatch* latch,
                                     uint32_t timeout_ms);
int32_t glyph_thread_test_latch_destroy(GlyphThreadTestLatch** latch);

#endif

#ifdef __cplusplus
}
#endif

#endif
