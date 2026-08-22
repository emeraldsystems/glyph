// Portable Glyph native-thread runtime for macOS and Linux.
//
// The public ABI deliberately uses an opaque allocation instead of exposing
// pthread_t. A handle and its worker each own one state reference. Joining or
// detaching consumes the handle reference; a detached worker keeps the state
// alive until its entry function returns.

#include "glyph_thread.h"

#include <errno.h>
#include <stddef.h>
#include <stdlib.h>

static void glyph_thread_drop_owned(void* env,
                                    GlyphThreadDropUnstarted drop_unstarted) {
    if (drop_unstarted != NULL) {
        drop_unstarted(env);
    }
}

#if defined(__APPLE__) || defined(__linux__)

#include <pthread.h>
#include <time.h>

typedef enum {
    GLYPH_THREAD_STARTING = 0,
    GLYPH_THREAD_RUNNING = 1,
    GLYPH_THREAD_FINISHED = 2,
} GlyphThreadLifecycle;

typedef enum {
    GLYPH_THREAD_JOINABLE = 0,
    GLYPH_THREAD_JOINING = 1,
    GLYPH_THREAD_DETACHING = 2,
    GLYPH_THREAD_JOINED = 3,
    GLYPH_THREAD_DETACHED = 4,
} GlyphThreadOwnerState;

struct GlyphThread {
    pthread_t native;
    pthread_mutex_t lock;
    uint32_t references;
    GlyphThreadLifecycle lifecycle;
    GlyphThreadOwnerState owner_state;

    // The state owns this start package until the worker claims it. If native
    // creation fails, the creator still owns it and performs unstarted-drop.
    GlyphThreadEntry entry;
    void* env;
    GlyphThreadDropUnstarted drop_unstarted;
};

static int32_t glyph_thread_error(int error_code) {
    return error_code > 0 ? -(int32_t)error_code : -EIO;
}

static void glyph_thread_release(GlyphThread* state) {
    int free_state = 0;
    pthread_mutex_lock(&state->lock);
    if (state->references > 0) {
        state->references--;
    }
    free_state = state->references == 0;
    pthread_mutex_unlock(&state->lock);

    if (free_state) {
        pthread_mutex_destroy(&state->lock);
        free(state);
    }
}

#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)

#if defined(__GNUC__) || defined(__clang__)
#define GLYPH_THREAD_TEST_HIDDEN __attribute__((visibility("hidden")))
#else
#define GLYPH_THREAD_TEST_HIDDEN
#endif

static pthread_mutex_t glyph_thread_test_failure_lock = PTHREAD_MUTEX_INITIALIZER;
static int32_t glyph_thread_test_failure_operation = 0;
static int32_t glyph_thread_test_failure_error = 0;

static int32_t glyph_thread_take_test_failure(int32_t operation) {
    int32_t error_code = 0;
    pthread_mutex_lock(&glyph_thread_test_failure_lock);
    if (glyph_thread_test_failure_operation == operation) {
        error_code = glyph_thread_test_failure_error;
        glyph_thread_test_failure_operation = 0;
        glyph_thread_test_failure_error = 0;
    }
    pthread_mutex_unlock(&glyph_thread_test_failure_lock);
    return error_code;
}

GLYPH_THREAD_TEST_HIDDEN int32_t
glyph_thread_test_fail_next(int32_t operation, int32_t error_code) {
    if (operation < GLYPH_THREAD_TEST_FAIL_ALLOC ||
        operation > GLYPH_THREAD_TEST_FAIL_DETACH || error_code <= 0) {
        return -EINVAL;
    }

    pthread_mutex_lock(&glyph_thread_test_failure_lock);
    glyph_thread_test_failure_operation = operation;
    glyph_thread_test_failure_error = error_code;
    pthread_mutex_unlock(&glyph_thread_test_failure_lock);
    return 0;
}

#else

static int32_t glyph_thread_take_test_failure(int32_t operation) {
    (void)operation;
    return 0;
}

#endif

static void* glyph_thread_start(void* raw_state) {
    GlyphThread* state = (GlyphThread*)raw_state;
    GlyphThreadEntry entry;
    void* env;

    pthread_mutex_lock(&state->lock);
    state->lifecycle = GLYPH_THREAD_RUNNING;
    entry = state->entry;
    env = state->env;
    state->entry = NULL;
    state->env = NULL;
    state->drop_unstarted = NULL;
    pthread_mutex_unlock(&state->lock);

    // The entry thunk consumes its FnOnce environment, including its drop.
    entry(env);

    pthread_mutex_lock(&state->lock);
    state->lifecycle = GLYPH_THREAD_FINISHED;
    pthread_mutex_unlock(&state->lock);
    glyph_thread_release(state);
    return NULL;
}

int32_t glyph_thread_spawn(GlyphThread** out,
                           GlyphThreadEntry entry,
                           void* env,
                           GlyphThreadDropUnstarted drop_unstarted) {
    if (out != NULL) {
        *out = NULL;
    }
    if (out == NULL || entry == NULL) {
        glyph_thread_drop_owned(env, drop_unstarted);
        return -EINVAL;
    }

    int injected = glyph_thread_take_test_failure(
#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)
        GLYPH_THREAD_TEST_FAIL_ALLOC
#else
        0
#endif
    );
    if (injected != 0) {
        glyph_thread_drop_owned(env, drop_unstarted);
        return glyph_thread_error(injected);
    }

    GlyphThread* state = (GlyphThread*)calloc(1, sizeof(GlyphThread));
    if (state == NULL) {
        glyph_thread_drop_owned(env, drop_unstarted);
        return -ENOMEM;
    }

    int rc = pthread_mutex_init(&state->lock, NULL);
    if (rc != 0) {
        free(state);
        glyph_thread_drop_owned(env, drop_unstarted);
        return glyph_thread_error(rc);
    }

    state->references = 2; // one handle reference plus one worker reference
    state->lifecycle = GLYPH_THREAD_STARTING;
    state->owner_state = GLYPH_THREAD_JOINABLE;
    state->entry = entry;
    state->env = env;
    state->drop_unstarted = drop_unstarted;

    injected = glyph_thread_take_test_failure(
#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)
        GLYPH_THREAD_TEST_FAIL_CREATE
#else
        0
#endif
    );
    rc = injected != 0 ? injected
                       : pthread_create(&state->native, NULL, glyph_thread_start, state);
    if (rc != 0) {
        glyph_thread_drop_owned(state->env, state->drop_unstarted);
        pthread_mutex_destroy(&state->lock);
        free(state);
        return glyph_thread_error(rc);
    }

    *out = state;
    return 0;
}

static void glyph_thread_restore_owner_state(GlyphThread* state,
                                             GlyphThreadOwnerState expected) {
    pthread_mutex_lock(&state->lock);
    if (state->owner_state == expected) {
        state->owner_state = GLYPH_THREAD_JOINABLE;
    }
    pthread_mutex_unlock(&state->lock);
}

int32_t glyph_thread_join(GlyphThread** handle) {
    if (handle == NULL || *handle == NULL) {
        return -EINVAL;
    }
    GlyphThread* state = *handle;

    pthread_mutex_lock(&state->lock);
    if (state->owner_state != GLYPH_THREAD_JOINABLE) {
        pthread_mutex_unlock(&state->lock);
        return -EINVAL;
    }
    state->owner_state = GLYPH_THREAD_JOINING;
    pthread_mutex_unlock(&state->lock);

    int rc = glyph_thread_take_test_failure(
#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)
        GLYPH_THREAD_TEST_FAIL_JOIN
#else
        0
#endif
    );
    if (rc == 0) {
        rc = pthread_join(state->native, NULL);
    }
    if (rc != 0) {
        glyph_thread_restore_owner_state(state, GLYPH_THREAD_JOINING);
        return glyph_thread_error(rc);
    }

    pthread_mutex_lock(&state->lock);
    state->owner_state = GLYPH_THREAD_JOINED;
    pthread_mutex_unlock(&state->lock);
    *handle = NULL;
    glyph_thread_release(state);
    return 0;
}

int32_t glyph_thread_detach(GlyphThread** handle) {
    if (handle == NULL || *handle == NULL) {
        return -EINVAL;
    }
    GlyphThread* state = *handle;

    pthread_mutex_lock(&state->lock);
    if (state->owner_state != GLYPH_THREAD_JOINABLE) {
        pthread_mutex_unlock(&state->lock);
        return -EINVAL;
    }
    state->owner_state = GLYPH_THREAD_DETACHING;
    pthread_mutex_unlock(&state->lock);

    int rc = glyph_thread_take_test_failure(
#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)
        GLYPH_THREAD_TEST_FAIL_DETACH
#else
        0
#endif
    );
    if (rc == 0) {
        rc = pthread_detach(state->native);
    }
    if (rc != 0) {
        glyph_thread_restore_owner_state(state, GLYPH_THREAD_DETACHING);
        return glyph_thread_error(rc);
    }

    pthread_mutex_lock(&state->lock);
    state->owner_state = GLYPH_THREAD_DETACHED;
    pthread_mutex_unlock(&state->lock);
    *handle = NULL;
    glyph_thread_release(state);
    return 0;
}

#if defined(GLYPH_THREAD_ENABLE_TEST_HOOKS)

struct GlyphThreadTestLatch {
    pthread_mutex_t lock;
    pthread_cond_t changed;
    uint32_t count;
    uint32_t waiters;
};

GLYPH_THREAD_TEST_HIDDEN int32_t
glyph_thread_test_latch_create(uint32_t initial_count,
                               GlyphThreadTestLatch** out) {
    if (out == NULL) {
        return -EINVAL;
    }
    *out = NULL;
    GlyphThreadTestLatch* latch =
        (GlyphThreadTestLatch*)calloc(1, sizeof(GlyphThreadTestLatch));
    if (latch == NULL) {
        return -ENOMEM;
    }
    int rc = pthread_mutex_init(&latch->lock, NULL);
    if (rc != 0) {
        free(latch);
        return glyph_thread_error(rc);
    }
    rc = pthread_cond_init(&latch->changed, NULL);
    if (rc != 0) {
        pthread_mutex_destroy(&latch->lock);
        free(latch);
        return glyph_thread_error(rc);
    }
    latch->count = initial_count;
    *out = latch;
    return 0;
}

GLYPH_THREAD_TEST_HIDDEN int32_t
glyph_thread_test_latch_count_down(GlyphThreadTestLatch* latch) {
    if (latch == NULL) {
        return -EINVAL;
    }
    pthread_mutex_lock(&latch->lock);
    if (latch->count == 0) {
        pthread_mutex_unlock(&latch->lock);
        return -EALREADY;
    }
    latch->count--;
    if (latch->count == 0) {
        pthread_cond_broadcast(&latch->changed);
    }
    pthread_mutex_unlock(&latch->lock);
    return 0;
}

static struct timespec glyph_thread_deadline(uint32_t timeout_ms) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += (time_t)(timeout_ms / 1000U);
    deadline.tv_nsec += (long)(timeout_ms % 1000U) * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_sec++;
        deadline.tv_nsec -= 1000000000L;
    }
    return deadline;
}

GLYPH_THREAD_TEST_HIDDEN int32_t
glyph_thread_test_latch_wait(GlyphThreadTestLatch* latch, uint32_t timeout_ms) {
    if (latch == NULL) {
        return -EINVAL;
    }
    struct timespec deadline = glyph_thread_deadline(timeout_ms);
    pthread_mutex_lock(&latch->lock);
    int rc = 0;
    latch->waiters++;
    while (latch->count != 0 && rc == 0) {
        rc = pthread_cond_timedwait(&latch->changed, &latch->lock, &deadline);
    }
    latch->waiters--;
    pthread_mutex_unlock(&latch->lock);
    return rc == 0 ? 0 : glyph_thread_error(rc);
}

GLYPH_THREAD_TEST_HIDDEN int32_t
glyph_thread_test_latch_destroy(GlyphThreadTestLatch** latch_ptr) {
    if (latch_ptr == NULL || *latch_ptr == NULL) {
        return -EINVAL;
    }
    GlyphThreadTestLatch* latch = *latch_ptr;
    pthread_mutex_lock(&latch->lock);
    if (latch->waiters != 0) {
        pthread_mutex_unlock(&latch->lock);
        return -EBUSY;
    }
    pthread_mutex_unlock(&latch->lock);
    int rc = pthread_cond_destroy(&latch->changed);
    if (rc != 0) {
        return glyph_thread_error(rc);
    }
    rc = pthread_mutex_destroy(&latch->lock);
    if (rc != 0) {
        return glyph_thread_error(rc);
    }
    free(latch);
    *latch_ptr = NULL;
    return 0;
}

#endif

#else

// Unsupported targets retain a stable symbol surface and report ENOSYS. This
// keeps cross-target diagnostics explicit instead of exposing pthread layout.
struct GlyphThread {
    uint8_t unavailable;
};

int32_t glyph_thread_spawn(GlyphThread** out,
                           GlyphThreadEntry entry,
                           void* env,
                           GlyphThreadDropUnstarted drop_unstarted) {
    (void)entry;
    if (out != NULL) {
        *out = NULL;
    }
    glyph_thread_drop_owned(env, drop_unstarted);
    return -ENOSYS;
}

int32_t glyph_thread_join(GlyphThread** handle) {
    (void)handle;
    return -ENOSYS;
}

int32_t glyph_thread_detach(GlyphThread** handle) {
    (void)handle;
    return -ENOSYS;
}

#endif
