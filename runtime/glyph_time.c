#include <stdint.h>
#include <time.h>
#include <stdio.h>
#include <errno.h>

uint64_t glyph_time_now(void) {
    time_t t = time(NULL);
    if (t == (time_t)-1) {
        return 0;
    }
    return (uint64_t)t;
}

const char* glyph_time_to_human_readable(uint64_t ts) {
    static char buffer[20];
    buffer[0] = '\0';

    time_t t = (time_t)ts;
    if ((uint64_t)t != ts) {
        return buffer;
    }

    struct tm tm_buf;
    struct tm* tm_ptr = NULL;

#if defined(_WIN32)
    if (gmtime_s(&tm_buf, &t) != 0) {
        return buffer;
    }
    tm_ptr = &tm_buf;
#else
    tm_ptr = gmtime_r(&t, &tm_buf);
    if (tm_ptr == NULL) {
        return buffer;
    }
#endif

    int len = snprintf(
        buffer,
        sizeof(buffer),
        "%02d/%02d/%04d %02d:%02d:%02d",
        tm_ptr->tm_mday,
        tm_ptr->tm_mon + 1,
        tm_ptr->tm_year + 1900,
        tm_ptr->tm_hour,
        tm_ptr->tm_min,
        tm_ptr->tm_sec
    );

    if (len != 19) {
        buffer[0] = '\0';
        return buffer;
    }

    return buffer;
}

// Monotonic clock in nanoseconds. Unaffected by wall-clock adjustments;
// only differences between two readings are meaningful. This is the timing
// source for sequencer ticks and latency measurement.
uint64_t glyph_time_monotonic_ns(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

int32_t glyph_time_sleep_ms(uint32_t ms) {
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    if (nanosleep(&ts, NULL) != 0) {
        return -1;
    }
    return 0;
}

int32_t glyph_time_sleep_us(uint32_t us) {
    struct timespec ts;
    ts.tv_sec = us / 1000000;
    ts.tv_nsec = (long)(us % 1000000) * 1000L;
    if (nanosleep(&ts, NULL) != 0) {
        return -1;
    }
    return 0;
}
