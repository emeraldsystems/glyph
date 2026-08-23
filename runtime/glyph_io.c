// Standard-input support for std/io.
//
// Deliberately written against plain C89/C99 stdio — `fgets` plus manual
// growth rather than POSIX `getline` — so this file needs no POSIX headers
// and is one of the pieces that does NOT have to be ported for Windows.
// The engine's control protocol runs over stdio precisely so that stays
// true (see glyph_audio/docs/design/APP_ARCHITECTURE.md §6).

#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_MSC_VER)
#define GLYPH_THREAD_LOCAL __declspec(thread)
#else
#define GLYPH_THREAD_LOCAL _Thread_local
#endif

// The returned view remains valid until this thread calls the function
// again — the same contract as glyph_time_to_human_readable. Thread-local
// storage keeps one reader from invalidating another's line, which matters
// because the sequencer runs its stdin reader on its own thread.
static GLYPH_THREAD_LOCAL char* glyph_io_line_buf = NULL;
static GLYPH_THREAD_LOCAL size_t glyph_io_line_cap = 0;

// Reads one line from stdin, INCLUDING its trailing newline.
//
// The newline is kept on purpose: it is what makes end-of-input
// unambiguous. A blank input line returns "\n" (length 1), while EOF
// returns "" (length 0). Callers can therefore test `len() == 0` for EOF
// without a second out-parameter, which Glyph has no good way to express.
//
// A final line with no trailing newline is returned as-is; the following
// call then reports EOF.
const char* glyph_io_read_line(void) {
    if (glyph_io_line_buf == NULL) {
        glyph_io_line_cap = 256;
        glyph_io_line_buf = (char*)malloc(glyph_io_line_cap);
        if (glyph_io_line_buf == NULL) {
            glyph_io_line_cap = 0;
            return "";
        }
    }

    glyph_io_line_buf[0] = '\0';
    size_t len = 0;

    for (;;) {
        size_t room = glyph_io_line_cap - len;
        if (room < 2) {
            // Cannot make progress without more space.
            size_t new_cap = glyph_io_line_cap * 2;
            char* grown = (char*)realloc(glyph_io_line_buf, new_cap);
            if (grown == NULL) {
                break;
            }
            glyph_io_line_buf = grown;
            glyph_io_line_cap = new_cap;
            continue;
        }

        if (fgets(glyph_io_line_buf + len, (int)room, stdin) == NULL) {
            // EOF or error. With nothing buffered this is end-of-input;
            // with a partial line buffered, hand that back and report EOF
            // on the next call.
            if (len == 0) {
                glyph_io_line_buf[0] = '\0';
            }
            break;
        }

        len += strlen(glyph_io_line_buf + len);

        if (len > 0 && glyph_io_line_buf[len - 1] == '\n') {
            break;
        }

        // No newline yet: either the buffer filled up or the stream ended
        // mid-line. Grow and continue; the fgets above settles which.
        size_t new_cap = glyph_io_line_cap * 2;
        char* grown = (char*)realloc(glyph_io_line_buf, new_cap);
        if (grown == NULL) {
            break;
        }
        glyph_io_line_buf = grown;
        glyph_io_line_cap = new_cap;
    }

    return glyph_io_line_buf;
}

// No flush counterpart is needed: std/io's print_str lowers to glyph_print,
// which is an unbuffered write(2) straight to fd 1 rather than stdio. That
// also makes a single print_str of a whole line atomic against other
// threads for lines under PIPE_BUF, which is how the sequencer's stdio
// server can have its reply writer and its report writer share stdout.

// Stops a closed stdout from killing the process.
//
// A program that streams to a pipe will eventually write to one whose
// reader has gone away, and the default SIGPIPE action is to terminate —
// immediately, with no chance to clean up. For the sequencer that would
// mean dying mid-render while still holding the WAV, leaving an unpatched
// RIFF header and an unreadable file. Ignoring the signal turns the same
// event into a plain write() error the caller can shrug off, so shutdown
// stays orderly.
//
// Opt-in rather than a runtime-wide default: a filter-style program may
// legitimately want the default behaviour of dying quietly when its reader
// closes. Windows has no SIGPIPE, so this compiles to a no-op there.
int32_t glyph_io_ignore_sigpipe(void) {
#if defined(SIGPIPE)
    return signal(SIGPIPE, SIG_IGN) == SIG_ERR ? -1 : 0;
#else
    return 0;
#endif
}
