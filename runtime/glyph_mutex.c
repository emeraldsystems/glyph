// Portable heap-stable mutex runtime for compiler-recognized Mutex<T>.

#include "glyph_mutex.h"

#include <errno.h>
#include <stddef.h>
#include <stdlib.h>

static int32_t glyph_mutex_error(int error_code) {
    return error_code == 0 ? 0 : (error_code > 0 ? -(int32_t)error_code : -EIO);
}

#if defined(__APPLE__) || defined(__linux__)

#include <pthread.h>

struct GlyphMutex {
    pthread_mutex_t native;
};

int32_t glyph_mutex_create(GlyphMutex** out) {
    if (out == NULL) {
        return -EINVAL;
    }
    *out = NULL;
    GlyphMutex* mutex = (GlyphMutex*)calloc(1, sizeof(GlyphMutex));
    if (mutex == NULL) {
        return -ENOMEM;
    }
    int rc = pthread_mutex_init(&mutex->native, NULL);
    if (rc != 0) {
        free(mutex);
        return glyph_mutex_error(rc);
    }
    *out = mutex;
    return 0;
}

int32_t glyph_mutex_lock(GlyphMutex* mutex) {
    if (mutex == NULL) {
        return -EINVAL;
    }
    return glyph_mutex_error(pthread_mutex_lock(&mutex->native));
}

int32_t glyph_mutex_try_lock(GlyphMutex* mutex) {
    if (mutex == NULL) {
        return -EINVAL;
    }
    int rc = pthread_mutex_trylock(&mutex->native);
    return rc == EBUSY ? 1 : glyph_mutex_error(rc);
}

int32_t glyph_mutex_unlock(GlyphMutex* mutex) {
    if (mutex == NULL) {
        return -EINVAL;
    }
    return glyph_mutex_error(pthread_mutex_unlock(&mutex->native));
}

int32_t glyph_mutex_destroy(GlyphMutex** mutex_ptr) {
    if (mutex_ptr == NULL || *mutex_ptr == NULL) {
        return -EINVAL;
    }
    GlyphMutex* mutex = *mutex_ptr;
    int rc = pthread_mutex_destroy(&mutex->native);
    if (rc != 0) {
        return glyph_mutex_error(rc);
    }
    free(mutex);
    *mutex_ptr = NULL;
    return 0;
}

#else

struct GlyphMutex {
    uint8_t unavailable;
};

int32_t glyph_mutex_create(GlyphMutex** out) {
    if (out != NULL) {
        *out = NULL;
    }
    return -ENOSYS;
}

int32_t glyph_mutex_lock(GlyphMutex* mutex) {
    (void)mutex;
    return -ENOSYS;
}

int32_t glyph_mutex_try_lock(GlyphMutex* mutex) {
    (void)mutex;
    return -ENOSYS;
}

int32_t glyph_mutex_unlock(GlyphMutex* mutex) {
    (void)mutex;
    return -ENOSYS;
}

int32_t glyph_mutex_destroy(GlyphMutex** mutex) {
    (void)mutex;
    return -ENOSYS;
}

#endif
