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

// --------------------------------------------------------------------------
// Binary file reading
//
// Glyph could read files before this, but only as text: File::read_to_string
// and nothing else. That ruled out every binary format -- audio, images,
// archives, anything with a header -- because there was no way to get raw
// bytes into a Glyph value.

// A Glyph Vec as the ABI sees it. Duplicated from glyph_audio.c rather than
// shared through a header, matching how this runtime is already organized:
// each file is standalone so a port can take them one at a time.
typedef struct {
    void* data;
    int64_t len;
    int64_t cap;
} GlyphIoVec;

// Reads up to `max` bytes of `path` into `out`, returning the count read or
// a negative errno-style code.
//
// THE CALLER SIZES THE BUFFER. `out` must already hold at least `max`
// elements -- push that many zeros first, or use glyph_file_size to learn
// the length. This function only ever writes into memory Glyph already
// owns; it never grows the Vec and never touches len or cap.
//
// That constraint is deliberate. Nothing else in this runtime allocates
// into a Glyph Vec, and doing so would mean C guessing at Glyph's allocator
// and its ownership rules -- in a language whose whole memory model is
// single-owner moves. A short read is reported honestly instead.
//
// Returns:
//   >= 0  bytes actually read (may be < max at end of file)
//   -1    path or out was NULL
//   -2    the buffer is smaller than max, so the request cannot be honoured
//   -3    the file could not be opened
//   -4    a read error occurred partway through
//
// Distinct codes on purpose: a caller wants to tell a missing asset from a
// corrupt one, and "your buffer was too small" from either.
int64_t glyph_io_read_file_bytes(const char* path, GlyphIoVec* out, int64_t max) {
    if (path == NULL || out == NULL) {
        return -1;
    }
    if (max <= 0) {
        return 0;
    }
    if (out->data == NULL || out->len < max) {
        return -2;
    }

    FILE* f = fopen(path, "rb");
    if (f == NULL) {
        return -3;
    }

    size_t got = fread(out->data, 1, (size_t)max, f);
    // Short reads are normal at end of file; only a real error is a failure,
    // so ferror is checked rather than comparing got against max.
    int failed = ferror(f);
    fclose(f);

    if (failed) {
        return -4;
    }
    return (int64_t)got;
}
