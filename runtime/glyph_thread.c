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
#include <string.h>

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
    GlyphThreadResultEntry result_entry;
    int has_result;
    void* invoke;
    void* env;
    GlyphThreadDropUnstarted drop_unstarted;

    // The slot is uninitialized until result_entry returns. Once initialized,
    // it is owned by this state until join moves it or final release drops it.
    void* result;
    size_t result_size;
    int result_initialized;
    GlyphThreadDropResult drop_result;
};

struct GlyphScopedThread {
    GlyphThread* thread;
    struct GlyphScopedThread* previous;
    struct GlyphScopedThread* next;
    struct GlyphThreadScope* scope;
};

struct GlyphThreadScope {
    GlyphScopedThread* first;
    GlyphScopedThread* last;
};

static int32_t glyph_thread_error(int error_code) {
    return error_code > 0 ? -(int32_t)error_code : -EIO;
}

static void glyph_thread_release(GlyphThread* state) {
    int free_state = 0;
    int drop_result = 0;
    void* result = NULL;
    GlyphThreadDropResult drop_result_fn = NULL;
    pthread_mutex_lock(&state->lock);
    if (state->references > 0) {
        state->references--;
    }
    free_state = state->references == 0;
    if (free_state) {
        drop_result = state->result_initialized;
        result = state->result;
        drop_result_fn = state->drop_result;
        state->result_initialized = 0;
        state->result = NULL;
    }
    pthread_mutex_unlock(&state->lock);

    if (free_state) {
        if (drop_result && drop_result_fn != NULL) {
            drop_result_fn(result);
        }
        free(result);
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
    GlyphThreadResultEntry result_entry;
    void* invoke;
    void* env;
    void* result;

    pthread_mutex_lock(&state->lock);
    state->lifecycle = GLYPH_THREAD_RUNNING;
    entry = state->entry;
    result_entry = state->result_entry;
    invoke = state->invoke;
    env = state->env;
    result = state->result;
    state->entry = NULL;
    state->result_entry = NULL;
    state->invoke = NULL;
    state->env = NULL;
    state->drop_unstarted = NULL;
    pthread_mutex_unlock(&state->lock);

    // The entry thunk consumes its FnOnce environment, including its drop.
    if (result_entry != NULL) {
        result_entry(invoke, env, result);
    } else {
        entry(env);
    }

    pthread_mutex_lock(&state->lock);
    if (result_entry != NULL) {
        state->result_initialized = 1;
    }
    state->lifecycle = GLYPH_THREAD_FINISHED;
    pthread_mutex_unlock(&state->lock);
    glyph_thread_release(state);
    return NULL;
}

static int32_t glyph_thread_spawn_impl(
    GlyphThread** out,
    GlyphThreadEntry entry,
    GlyphThreadResultEntry result_entry,
    void* invoke,
    void* env,
    GlyphThreadDropUnstarted drop_unstarted,
    size_t result_size,
    GlyphThreadDropResult drop_result) {
    if (out != NULL) {
        *out = NULL;
    }
    if (out == NULL || (entry == NULL && result_entry == NULL) ||
        (entry != NULL && result_entry != NULL)) {
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
    state->result_entry = result_entry;
    state->has_result = result_entry != NULL;
    state->invoke = invoke;
    state->env = env;
    state->drop_unstarted = drop_unstarted;
    state->result_size = result_size;
    state->drop_result = drop_result;
    if (result_size != 0) {
        state->result = malloc(result_size);
        if (state->result == NULL) {
            glyph_thread_drop_owned(state->env, state->drop_unstarted);
            pthread_mutex_destroy(&state->lock);
            free(state);
            return -ENOMEM;
        }
    }

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
        free(state->result);
        pthread_mutex_destroy(&state->lock);
        free(state);
        return glyph_thread_error(rc);
    }

    *out = state;
    return 0;
}

int32_t glyph_thread_spawn(GlyphThread** out,
                           GlyphThreadEntry entry,
                           void* env,
                           GlyphThreadDropUnstarted drop_unstarted) {
    return glyph_thread_spawn_impl(out, entry, NULL, NULL, env, drop_unstarted,
                                   0, NULL);
}

int32_t glyph_thread_spawn_result(GlyphThread** out,
                                  GlyphThreadResultEntry entry,
                                  void* invoke,
                                  void* env,
                                  GlyphThreadDropUnstarted drop_unstarted,
                                  size_t result_size,
                                  GlyphThreadDropResult drop_result) {
    return glyph_thread_spawn_impl(out, NULL, entry, invoke, env,
                                   drop_unstarted, result_size, drop_result);
}

static void glyph_thread_restore_owner_state(GlyphThread* state,
                                             GlyphThreadOwnerState expected) {
    pthread_mutex_lock(&state->lock);
    if (state->owner_state == expected) {
        state->owner_state = GLYPH_THREAD_JOINABLE;
    }
    pthread_mutex_unlock(&state->lock);
}

static int32_t glyph_thread_join_native(GlyphThread* state) {
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
    return 0;
}

int32_t glyph_thread_join(GlyphThread** handle) {
    if (handle == NULL || *handle == NULL) {
        return -EINVAL;
    }
    GlyphThread* state = *handle;
    if (state->has_result) {
        return -EINVAL;
    }
    int32_t status = glyph_thread_join_native(state);
    if (status != 0) {
        return status;
    }
    *handle = NULL;
    glyph_thread_release(state);
    return 0;
}

int32_t glyph_thread_join_result(GlyphThread** handle, void* out_result) {
    if (handle == NULL || *handle == NULL) {
        return -EINVAL;
    }
    GlyphThread* state = *handle;
    if (!state->has_result ||
        (state->result_size != 0 && out_result == NULL)) {
        return -EINVAL;
    }
    int32_t status = glyph_thread_join_native(state);
    if (status != 0) {
        return status;
    }

    pthread_mutex_lock(&state->lock);
    int initialized = state->result_initialized;
    if (initialized && state->result_size != 0) {
        memcpy(out_result, state->result, state->result_size);
    }
    state->result_initialized = 0;
    pthread_mutex_unlock(&state->lock);

    *handle = NULL;
    glyph_thread_release(state);
    return initialized ? 0 : -EIO;
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

int32_t glyph_thread_scope_create(GlyphThreadScope** out) {
    if (out == NULL) {
        return -EINVAL;
    }
    *out = (GlyphThreadScope*)calloc(1, sizeof(GlyphThreadScope));
    return *out == NULL ? -ENOMEM : 0;
}

static int32_t glyph_thread_scope_spawn_impl(
    GlyphThreadScope* scope,
    GlyphScopedThread** out,
    GlyphThreadEntry entry,
    GlyphThreadResultEntry result_entry,
    void* invoke,
    void* env,
    size_t result_size,
    GlyphThreadDropResult drop_result) {
    if (out != NULL) {
        *out = NULL;
    }
    if (scope == NULL || out == NULL) {
        return -EINVAL;
    }

    GlyphScopedThread* child =
        (GlyphScopedThread*)calloc(1, sizeof(GlyphScopedThread));
    if (child == NULL) {
        return -ENOMEM;
    }

    int32_t status = glyph_thread_spawn_impl(
        &child->thread, entry, result_entry, invoke, env, NULL, result_size,
        drop_result);
    if (status != 0) {
        free(child);
        return status;
    }

    child->scope = scope;
    child->previous = scope->last;
    if (scope->last != NULL) {
        scope->last->next = child;
    } else {
        scope->first = child;
    }
    scope->last = child;
    *out = child;
    return 0;
}

int32_t glyph_thread_scope_spawn(GlyphThreadScope* scope,
                                 GlyphScopedThread** out,
                                 GlyphThreadEntry entry,
                                 void* env) {
    return glyph_thread_scope_spawn_impl(scope, out, entry, NULL, NULL, env,
                                         0, NULL);
}

int32_t glyph_thread_scope_spawn_result(GlyphThreadScope* scope,
                                        GlyphScopedThread** out,
                                        GlyphThreadResultEntry entry,
                                        void* invoke,
                                        void* env,
                                        size_t result_size,
                                        GlyphThreadDropResult drop_result) {
    return glyph_thread_scope_spawn_impl(scope, out, NULL, entry, invoke, env,
                                         result_size, drop_result);
}

static int glyph_thread_scope_owns(const GlyphThreadScope* scope,
                                   const GlyphScopedThread* child) {
    return scope != NULL && child != NULL && child->scope == scope;
}

static void glyph_thread_scope_unlink(GlyphThreadScope* scope,
                                      GlyphScopedThread* child) {
    if (child->previous != NULL) {
        child->previous->next = child->next;
    } else {
        scope->first = child->next;
    }
    if (child->next != NULL) {
        child->next->previous = child->previous;
    } else {
        scope->last = child->previous;
    }
    child->scope = NULL;
    child->previous = NULL;
    child->next = NULL;
}

static int32_t glyph_thread_scope_join_drop(GlyphThreadScope* scope,
                                            GlyphScopedThread* child) {
    if (!glyph_thread_scope_owns(scope, child)) {
        return -EINVAL;
    }
    int32_t status = glyph_thread_join_native(child->thread);
    if (status != 0) {
        return status;
    }
    GlyphThread* thread = child->thread;
    child->thread = NULL;
    glyph_thread_scope_unlink(scope, child);
    glyph_thread_release(thread);
    free(child);
    return 0;
}

int32_t glyph_thread_scope_join(GlyphScopedThread** child_ptr) {
    if (child_ptr == NULL || *child_ptr == NULL ||
        (*child_ptr)->scope == NULL || (*child_ptr)->thread->has_result) {
        return -EINVAL;
    }
    GlyphThreadScope* scope = (*child_ptr)->scope;
    int32_t status = glyph_thread_scope_join_drop(scope, *child_ptr);
    if (status == 0) {
        *child_ptr = NULL;
    }
    return status;
}

int32_t glyph_thread_scope_join_result(GlyphScopedThread** child_ptr,
                                       void* out_result) {
    if (child_ptr == NULL || *child_ptr == NULL || (*child_ptr)->scope == NULL) {
        return -EINVAL;
    }
    GlyphThreadScope* scope = (*child_ptr)->scope;
    GlyphThread* thread = (*child_ptr)->thread;
    if (!thread->has_result || (thread->result_size != 0 && out_result == NULL)) {
        return -EINVAL;
    }

    int32_t status = glyph_thread_join_native(thread);
    if (status != 0) {
        return status;
    }
    pthread_mutex_lock(&thread->lock);
    int initialized = thread->result_initialized;
    if (initialized && thread->result_size != 0) {
        memcpy(out_result, thread->result, thread->result_size);
    }
    thread->result_initialized = 0;
    pthread_mutex_unlock(&thread->lock);

    GlyphScopedThread* child = *child_ptr;
    child->thread = NULL;
    glyph_thread_scope_unlink(scope, child);
    glyph_thread_release(thread);
    free(child);
    *child_ptr = NULL;
    return initialized ? 0 : -EIO;
}

int32_t glyph_thread_scope_drain(GlyphThreadScope* scope) {
    if (scope == NULL) {
        return -EINVAL;
    }
    while (scope->first != NULL) {
        int32_t status = glyph_thread_scope_join_drop(scope, scope->first);
        if (status != 0) {
            return status;
        }
    }
    return 0;
}

void glyph_thread_scope_drain_or_abort(GlyphThreadScope* scope) {
    int32_t status = glyph_thread_scope_drain(scope);
    if (status != 0) {
        status = glyph_thread_scope_drain(scope);
    }
    if (status != 0) {
        abort();
    }
}

int32_t glyph_thread_scope_join_all(GlyphThreadScope** scope_ptr) {
    if (scope_ptr == NULL || *scope_ptr == NULL) {
        return -EINVAL;
    }
    GlyphThreadScope* scope = *scope_ptr;
    int32_t status = glyph_thread_scope_drain(scope);
    if (status != 0) {
        return status;
    }
    free(scope);
    *scope_ptr = NULL;
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

struct GlyphThreadScope {
    uint8_t unavailable;
};

struct GlyphScopedThread {
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

int32_t glyph_thread_spawn_result(GlyphThread** out,
                                  GlyphThreadResultEntry entry,
                                  void* invoke,
                                  void* env,
                                  GlyphThreadDropUnstarted drop_unstarted,
                                  size_t result_size,
                                  GlyphThreadDropResult drop_result) {
    (void)entry;
    (void)invoke;
    (void)result_size;
    (void)drop_result;
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

int32_t glyph_thread_join_result(GlyphThread** handle, void* out_result) {
    (void)handle;
    (void)out_result;
    return -ENOSYS;
}

int32_t glyph_thread_detach(GlyphThread** handle) {
    (void)handle;
    return -ENOSYS;
}

int32_t glyph_thread_scope_create(GlyphThreadScope** out) {
    if (out != NULL) {
        *out = NULL;
    }
    return -ENOSYS;
}

int32_t glyph_thread_scope_spawn(GlyphThreadScope* scope,
                                 GlyphScopedThread** out,
                                 GlyphThreadEntry entry,
                                 void* env) {
    (void)scope;
    (void)entry;
    (void)env;
    if (out != NULL) {
        *out = NULL;
    }
    return -ENOSYS;
}

int32_t glyph_thread_scope_spawn_result(GlyphThreadScope* scope,
                                        GlyphScopedThread** out,
                                        GlyphThreadResultEntry entry,
                                        void* invoke,
                                        void* env,
                                        size_t result_size,
                                        GlyphThreadDropResult drop_result) {
    (void)scope;
    (void)entry;
    (void)invoke;
    (void)env;
    (void)result_size;
    (void)drop_result;
    if (out != NULL) {
        *out = NULL;
    }
    return -ENOSYS;
}

int32_t glyph_thread_scope_join(GlyphScopedThread** child) {
    (void)child;
    return -ENOSYS;
}

int32_t glyph_thread_scope_join_result(GlyphScopedThread** child,
                                       void* out_result) {
    (void)child;
    (void)out_result;
    return -ENOSYS;
}

int32_t glyph_thread_scope_drain(GlyphThreadScope* scope) {
    (void)scope;
    return -ENOSYS;
}

void glyph_thread_scope_drain_or_abort(GlyphThreadScope* scope) {
    (void)scope;
    abort();
}

int32_t glyph_thread_scope_join_all(GlyphThreadScope** scope) {
    (void)scope;
    return -ENOSYS;
}

#endif
