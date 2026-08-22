#include <stdint.h>
#include <stddef.h>
#include <stdatomic.h>

// Minimal terminal runtime hooks for std/term.
// This first pass keeps behavior deterministic and test-safe:
// - no direct TTY mode changes,
// - idempotent session end,
// - per-process single active UI session.

static _Atomic(int) glyph_term_active_session = 0;

int32_t glyph_term_stdout(void) {
    // Single process-local terminal handle for stdout.
    return 1;
}

int32_t glyph_term_enter_ui_session(int32_t term_id) {
    if (term_id != 1) {
        return -2;
    }
    int expected = 0;
    if (!atomic_compare_exchange_strong_explicit(
            &glyph_term_active_session,
            &expected,
            1,
            memory_order_acq_rel,
            memory_order_acquire)) {
        return -1;
    }
    return 0;
}

int32_t glyph_term_session_end(int32_t term_id) {
    if (term_id != 1) {
        return -2;
    }
    // Idempotent cleanup.
    atomic_store_explicit(&glyph_term_active_session, 0, memory_order_release);
    return 0;
}

int32_t glyph_term_move_to(int32_t term_id, uint32_t row, uint32_t col) {
    (void)row;
    (void)col;
    if (term_id != 1) {
        return -2;
    }
    return 0;
}

int32_t glyph_term_clear_line(int32_t term_id) {
    if (term_id != 1) {
        return -2;
    }
    return 0;
}

int32_t glyph_term_write_str(int32_t term_id, const char* s) {
    (void)s;
    if (term_id != 1) {
        return -2;
    }
    return 0;
}

int32_t glyph_term_flush(int32_t term_id) {
    if (term_id != 1) {
        return -2;
    }
    return 0;
}

int32_t glyph_term_poll_event(int32_t term_id, uint32_t timeout_ms) {
    (void)timeout_ms;
    if (term_id != 1) {
        return -2;
    }
    // No event available in the minimal runtime.
    return 0;
}
