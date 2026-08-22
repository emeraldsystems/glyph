#ifndef GLYPH_MUTEX_H
#define GLYPH_MUTEX_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/** Opaque, heap-stable native mutex. Its representation is not Glyph ABI. */
typedef struct GlyphMutex GlyphMutex;

/** Allocate and initialize a non-recursive mutex. */
int32_t glyph_mutex_create(GlyphMutex** out);
/** Block until the mutex is acquired. */
int32_t glyph_mutex_lock(GlyphMutex* mutex);
/** Acquire without blocking; returns 1 when another owner holds it. */
int32_t glyph_mutex_try_lock(GlyphMutex* mutex);
/** Release one successful lock acquisition. */
int32_t glyph_mutex_unlock(GlyphMutex* mutex);
/** Destroy an unlocked mutex. On failure the pointer remains retryable. */
int32_t glyph_mutex_destroy(GlyphMutex** mutex);

#ifdef __cplusplus
}
#endif

#endif
