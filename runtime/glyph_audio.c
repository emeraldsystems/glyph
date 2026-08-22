// Glyph Audio Runtime Library
//
// Two surfaces:
//   1. Offline WAV render (all platforms): 16-bit PCM writer fed f64 sample
//      blocks in [-1, 1]. Deterministic and dependency-free, so the synth
//      core is testable in CI.
//   2. Live output (macOS): an AudioQueue-based shim. The queue's buffer
//      pool acts as the ring buffer; glyph_audio_out_write() blocks until a
//      buffer is free, so Glyph's single-threaded engine loop is naturally
//      paced by the audio clock. The real-time thread lives entirely inside
//      AudioQueue - Glyph code never runs on it.
//
// Sample blocks arrive as &Vec<f64>: a pointer to { data, len, cap } with
// len f64 samples (interleaved when channels > 1), matching the layout used
// by glyph_process_run.

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    void* data;
    int64_t len;
    int64_t cap;
} GlyphVec;

// --------------------------------------------------------------------------
// WAV writer
// --------------------------------------------------------------------------

#define GLYPH_WAV_MAX_HANDLES 16

typedef struct {
    FILE* file;
    uint32_t sample_rate;
    uint16_t channels;
    uint64_t data_bytes;
    int used;
} GlyphWavState;

static GlyphWavState wav_states[GLYPH_WAV_MAX_HANDLES];

static void wav_write_u16(FILE* f, uint16_t v) {
    unsigned char b[2] = {(unsigned char)(v & 0xff), (unsigned char)(v >> 8)};
    fwrite(b, 1, 2, f);
}

static void wav_write_u32(FILE* f, uint32_t v) {
    unsigned char b[4] = {
        (unsigned char)(v & 0xff),
        (unsigned char)((v >> 8) & 0xff),
        (unsigned char)((v >> 16) & 0xff),
        (unsigned char)((v >> 24) & 0xff),
    };
    fwrite(b, 1, 4, f);
}

static void wav_write_header(GlyphWavState* st) {
    FILE* f = st->file;
    uint32_t data_bytes = (uint32_t)st->data_bytes;
    uint16_t block_align = (uint16_t)(st->channels * 2);

    fseek(f, 0, SEEK_SET);
    fwrite("RIFF", 1, 4, f);
    wav_write_u32(f, 36 + data_bytes);
    fwrite("WAVE", 1, 4, f);
    fwrite("fmt ", 1, 4, f);
    wav_write_u32(f, 16);                                  // fmt chunk size
    wav_write_u16(f, 1);                                   // PCM
    wav_write_u16(f, st->channels);
    wav_write_u32(f, st->sample_rate);
    wav_write_u32(f, st->sample_rate * block_align);       // byte rate
    wav_write_u16(f, block_align);
    wav_write_u16(f, 16);                                  // bits per sample
    fwrite("data", 1, 4, f);
    wav_write_u32(f, data_bytes);
}

// Returns a handle >= 0, or a negative errno-style code.
int32_t glyph_audio_wav_open(const char* path, uint32_t sample_rate, uint32_t channels) {
    if (path == NULL || sample_rate == 0 || channels == 0 || channels > 8) {
        return -EINVAL;
    }

    int32_t slot = -1;
    for (int32_t i = 0; i < GLYPH_WAV_MAX_HANDLES; i++) {
        if (!wav_states[i].used) {
            slot = i;
            break;
        }
    }
    if (slot < 0) {
        return -EMFILE;
    }

    FILE* f = fopen(path, "wb");
    if (f == NULL) {
        return errno > 0 ? -errno : -EIO;
    }

    GlyphWavState* st = &wav_states[slot];
    st->file = f;
    st->sample_rate = sample_rate;
    st->channels = (uint16_t)channels;
    st->data_bytes = 0;
    st->used = 1;
    wav_write_header(st); // placeholder sizes; patched on close
    return slot;
}

// Appends len f64 samples (clamped to [-1, 1], converted to 16-bit PCM).
// Returns the number of samples written, or a negative errno-style code.
int32_t glyph_audio_wav_write(int32_t handle, const GlyphVec* samples) {
    if (handle < 0 || handle >= GLYPH_WAV_MAX_HANDLES || !wav_states[handle].used) {
        return -EBADF;
    }
    if (samples == NULL || (samples->data == NULL && samples->len > 0)) {
        return -EINVAL;
    }

    GlyphWavState* st = &wav_states[handle];
    const double* src = (const double*)samples->data;
    for (int64_t i = 0; i < samples->len; i++) {
        double s = src[i];
        if (s > 1.0) {
            s = 1.0;
        } else if (s < -1.0) {
            s = -1.0;
        }
        int16_t pcm = (int16_t)(s * 32767.0);
        unsigned char b[2] = {(unsigned char)((uint16_t)pcm & 0xff),
                              (unsigned char)((uint16_t)pcm >> 8)};
        if (fwrite(b, 1, 2, st->file) != 2) {
            return -EIO;
        }
        st->data_bytes += 2;
    }
    return (int32_t)samples->len;
}

int32_t glyph_audio_wav_close(int32_t handle) {
    if (handle < 0 || handle >= GLYPH_WAV_MAX_HANDLES || !wav_states[handle].used) {
        return -EBADF;
    }
    GlyphWavState* st = &wav_states[handle];
    wav_write_header(st); // patch RIFF/data sizes
    int rc = fclose(st->file);
    st->file = NULL;
    st->used = 0;
    return rc == 0 ? 0 : -EIO;
}

// --------------------------------------------------------------------------
// Live output (macOS AudioQueue; -ENOSYS elsewhere)
// --------------------------------------------------------------------------

#if defined(__APPLE__)

#include <AudioToolbox/AudioToolbox.h>
#include <pthread.h>

#define GLYPH_OUT_MAX_HANDLES 4
#define GLYPH_OUT_BUF_COUNT 4
#define GLYPH_OUT_BUF_FRAMES 1024

typedef struct {
    AudioQueueRef queue;
    AudioQueueBufferRef free_bufs[GLYPH_OUT_BUF_COUNT];
    int free_count;
    pthread_mutex_t lock;
    pthread_cond_t cond;
    uint32_t channels;
    int used;
} GlyphOutState;

static GlyphOutState out_states[GLYPH_OUT_MAX_HANDLES];

static void glyph_out_callback(void* user_data, AudioQueueRef queue, AudioQueueBufferRef buf) {
    (void)queue;
    GlyphOutState* st = (GlyphOutState*)user_data;
    pthread_mutex_lock(&st->lock);
    if (st->free_count < GLYPH_OUT_BUF_COUNT) {
        st->free_bufs[st->free_count++] = buf;
    }
    pthread_cond_signal(&st->cond);
    pthread_mutex_unlock(&st->lock);
}

// Returns a handle >= 0, or a negative errno-style code.
int32_t glyph_audio_out_open(uint32_t sample_rate, uint32_t channels) {
    if (sample_rate == 0 || channels == 0 || channels > 8) {
        return -EINVAL;
    }

    int32_t slot = -1;
    for (int32_t i = 0; i < GLYPH_OUT_MAX_HANDLES; i++) {
        if (!out_states[i].used) {
            slot = i;
            break;
        }
    }
    if (slot < 0) {
        return -EMFILE;
    }

    GlyphOutState* st = &out_states[slot];
    memset(st, 0, sizeof(*st));
    pthread_mutex_init(&st->lock, NULL);
    pthread_cond_init(&st->cond, NULL);
    st->channels = channels;

    AudioStreamBasicDescription fmt;
    memset(&fmt, 0, sizeof(fmt));
    fmt.mSampleRate = (Float64)sample_rate;
    fmt.mFormatID = kAudioFormatLinearPCM;
    fmt.mFormatFlags = kLinearPCMFormatFlagIsFloat | kLinearPCMFormatFlagIsPacked;
    fmt.mChannelsPerFrame = channels;
    fmt.mBitsPerChannel = 32;
    fmt.mBytesPerFrame = 4 * channels;
    fmt.mFramesPerPacket = 1;
    fmt.mBytesPerPacket = 4 * channels;

    OSStatus rc = AudioQueueNewOutput(&fmt, glyph_out_callback, st, NULL, NULL, 0, &st->queue);
    if (rc != noErr) {
        return -EIO;
    }

    UInt32 buf_bytes = GLYPH_OUT_BUF_FRAMES * fmt.mBytesPerFrame;
    for (int i = 0; i < GLYPH_OUT_BUF_COUNT; i++) {
        AudioQueueBufferRef buf;
        rc = AudioQueueAllocateBuffer(st->queue, buf_bytes, &buf);
        if (rc != noErr) {
            AudioQueueDispose(st->queue, true);
            return -EIO;
        }
        st->free_bufs[st->free_count++] = buf;
    }

    rc = AudioQueueStart(st->queue, NULL);
    if (rc != noErr) {
        AudioQueueDispose(st->queue, true);
        return -EIO;
    }

    st->used = 1;
    return slot;
}

// Blocking push: converts f64 samples to f32 and enqueues them, waiting for
// queue buffers as needed. Returns samples written or a negative code.
int32_t glyph_audio_out_write(int32_t handle, const GlyphVec* samples) {
    if (handle < 0 || handle >= GLYPH_OUT_MAX_HANDLES || !out_states[handle].used) {
        return -EBADF;
    }
    if (samples == NULL || (samples->data == NULL && samples->len > 0)) {
        return -EINVAL;
    }

    GlyphOutState* st = &out_states[handle];
    const double* src = (const double*)samples->data;
    int64_t remaining = samples->len;

    while (remaining > 0) {
        pthread_mutex_lock(&st->lock);
        while (st->free_count == 0) {
            pthread_cond_wait(&st->cond, &st->lock);
        }
        AudioQueueBufferRef buf = st->free_bufs[--st->free_count];
        pthread_mutex_unlock(&st->lock);

        int64_t cap_samples = buf->mAudioDataBytesCapacity / 4;
        int64_t n = remaining < cap_samples ? remaining : cap_samples;
        float* dst = (float*)buf->mAudioData;
        for (int64_t i = 0; i < n; i++) {
            dst[i] = (float)src[i];
        }
        buf->mAudioDataByteSize = (UInt32)(n * 4);

        OSStatus rc = AudioQueueEnqueueBuffer(st->queue, buf, 0, NULL);
        if (rc != noErr) {
            // Return the buffer to the free list so close() doesn't hang.
            pthread_mutex_lock(&st->lock);
            st->free_bufs[st->free_count++] = buf;
            pthread_mutex_unlock(&st->lock);
            return -EIO;
        }
        src += n;
        remaining -= n;
    }
    return (int32_t)samples->len;
}

// Lets queued audio finish playing, then tears the queue down.
int32_t glyph_audio_out_close(int32_t handle) {
    if (handle < 0 || handle >= GLYPH_OUT_MAX_HANDLES || !out_states[handle].used) {
        return -EBADF;
    }
    GlyphOutState* st = &out_states[handle];

    AudioQueueFlush(st->queue);

    // Wait until every buffer has been played and returned by the callback.
    pthread_mutex_lock(&st->lock);
    while (st->free_count < GLYPH_OUT_BUF_COUNT) {
        pthread_cond_wait(&st->cond, &st->lock);
    }
    pthread_mutex_unlock(&st->lock);

    AudioQueueStop(st->queue, true);
    AudioQueueDispose(st->queue, true);
    pthread_mutex_destroy(&st->lock);
    pthread_cond_destroy(&st->cond);
    st->used = 0;
    return 0;
}

#else // !__APPLE__

int32_t glyph_audio_out_open(uint32_t sample_rate, uint32_t channels) {
    (void)sample_rate;
    (void)channels;
    return -ENOSYS;
}

int32_t glyph_audio_out_write(int32_t handle, const GlyphVec* samples) {
    (void)handle;
    (void)samples;
    return -ENOSYS;
}

int32_t glyph_audio_out_close(int32_t handle) {
    (void)handle;
    return -ENOSYS;
}

#endif
