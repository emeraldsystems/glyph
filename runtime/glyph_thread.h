#ifndef GLYPH_THREAD_H
#define GLYPH_THREAD_H

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
 * Join and consume a handle. The pointer is nulled only after pthread_join
 * succeeds; an error leaves it retryable.
 */
int32_t glyph_thread_join(GlyphThread** handle);

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
