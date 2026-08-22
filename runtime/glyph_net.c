// Glyph Networking Runtime Library
// Provides TCP and UDP socket primitives via POSIX socket API.
// Link against this library when building executables that use std/net.

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <errno.h>

#if defined(_MSC_VER)
#define GLYPH_THREAD_LOCAL __declspec(thread)
#else
#define GLYPH_THREAD_LOCAL _Thread_local
#endif

#if defined(__APPLE__) || defined(__linux__)

#include <sys/socket.h>
#include <sys/types.h>
#include <sys/stat.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <unistd.h>

// String-returning receive functions cannot return an errno alongside their
// owned string. Keep that compatibility cache per-thread so another socket
// operation cannot overwrite the caller's result before it is queried.
static GLYPH_THREAD_LOCAL int32_t glyph_net_last_errno = 0;

// Helper: allocate an empty heap string (never returns NULL)
static char* empty_string(void) {
    char* s = (char*)malloc(1);
    if (s) s[0] = '\0';
    return s;
}

// --- Error query ---

int32_t glyph_net_get_last_error(void) {
    return glyph_net_last_errno;
}

// --- TCP ---

int32_t glyph_net_tcp_socket(void) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        return -(int32_t)errno;
    }
    return (int32_t)fd;
}

int32_t glyph_net_tcp_connect(const char* host, uint32_t port) {
    struct addrinfo hints, *res, *rp;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;

    char port_str[6];
    snprintf(port_str, sizeof(port_str), "%u", port);

    int gai_err = getaddrinfo(host, port_str, &hints, &res);
    if (gai_err != 0) {
        return -(int32_t)ENOENT;
    }

    int fd = -1;
    int last_errno = ECONNREFUSED;
    for (rp = res; rp != NULL; rp = rp->ai_next) {
        fd = socket(rp->ai_family, rp->ai_socktype, rp->ai_protocol);
        if (fd < 0) {
            last_errno = errno;
            continue;
        }
        if (connect(fd, rp->ai_addr, rp->ai_addrlen) == 0) {
            break;
        }
        last_errno = errno;
        close(fd);
        fd = -1;
    }
    freeaddrinfo(res);

    if (fd < 0) {
        return -(int32_t)last_errno;
    }
    return (int32_t)fd;
}

int32_t glyph_net_bind(int32_t fd, const char* host, uint32_t port) {
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);

    if (host == NULL || host[0] == '\0' || strcmp(host, "0.0.0.0") == 0) {
        addr.sin_addr.s_addr = INADDR_ANY;
    } else if (strcmp(host, "127.0.0.1") == 0 || strcmp(host, "localhost") == 0) {
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    } else {
        if (inet_pton(AF_INET, host, &addr.sin_addr) != 1) {
            return -(int32_t)EINVAL;
        }
    }

    if (bind(fd, (struct sockaddr*)&addr, sizeof(addr)) != 0) {
        return -(int32_t)errno;
    }
    return 0;
}

int32_t glyph_net_listen(int32_t fd, int32_t backlog) {
    if (listen(fd, backlog) != 0) {
        return -(int32_t)errno;
    }
    return 0;
}

int32_t glyph_net_accept(int32_t fd) {
    struct sockaddr_in client_addr;
    socklen_t client_len = sizeof(client_addr);
    int client_fd;

    do {
        client_fd = accept(fd, (struct sockaddr*)&client_addr, &client_len);
    } while (client_fd < 0 && errno == EINTR);

    if (client_fd < 0) {
        return -(int32_t)errno;
    }
    return (int32_t)client_fd;
}

int32_t glyph_net_tcp_send(int32_t fd, const char* data) {
    if (data == NULL) return -(int32_t)EINVAL;
    size_t len = strlen(data);
    ssize_t n;
    do {
        n = write(fd, data, len);
    } while (n < 0 && errno == EINTR);

    if (n < 0) {
        return -(int32_t)errno;
    }
    return (int32_t)n;
}

char* glyph_net_tcp_recv(int32_t fd, uint32_t max_bytes) {
    glyph_net_last_errno = 0;

    if (max_bytes == 0) {
        return empty_string();
    }

    char* buf = (char*)malloc((size_t)max_bytes + 1);
    if (!buf) {
        glyph_net_last_errno = ENOMEM;
        return empty_string();
    }

    ssize_t n;
    do {
        n = read(fd, buf, (size_t)max_bytes);
    } while (n < 0 && errno == EINTR);

    if (n < 0) {
        glyph_net_last_errno = errno;
        free(buf);
        return empty_string();
    }

    buf[n] = '\0';
    // Shrink to actual size
    if ((size_t)n + 1 < (size_t)max_bytes + 1) {
        char* trimmed = (char*)realloc(buf, (size_t)n + 1);
        if (trimmed) buf = trimmed;
    }
    return buf;
}

int32_t glyph_net_close(int32_t fd) {
    if (close(fd) != 0) {
        return -(int32_t)errno;
    }
    return 0;
}

int32_t glyph_net_set_reuse_addr(int32_t fd) {
    int opt = 1;
    if (setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt)) != 0) {
        return -(int32_t)errno;
    }
    return 0;
}

// --- UDP ---

int32_t glyph_net_udp_socket(void) {
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        return -(int32_t)errno;
    }
    return (int32_t)fd;
}

int32_t glyph_net_udp_bind(int32_t fd, const char* host, uint32_t port) {
    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);

    if (host == NULL || host[0] == '\0' || strcmp(host, "0.0.0.0") == 0) {
        addr.sin_addr.s_addr = INADDR_ANY;
    } else if (strcmp(host, "127.0.0.1") == 0 || strcmp(host, "localhost") == 0) {
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    } else {
        if (inet_pton(AF_INET, host, &addr.sin_addr) != 1) {
            return -(int32_t)EINVAL;
        }
    }

    if (bind(fd, (struct sockaddr*)&addr, sizeof(addr)) != 0) {
        return -(int32_t)errno;
    }
    return 0;
}

int32_t glyph_net_udp_send_to(int32_t fd, const char* data,
                               const char* host, uint32_t port) {
    if (data == NULL || host == NULL || host[0] == '\0') {
        return -(int32_t)EINVAL;
    }

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);

    if (strcmp(host, "127.0.0.1") == 0 || strcmp(host, "localhost") == 0) {
        addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    } else {
        if (inet_pton(AF_INET, host, &addr.sin_addr) != 1) {
            return -(int32_t)EINVAL;
        }
    }

    size_t data_len = strlen(data);
    ssize_t n = sendto(fd, data, data_len, 0,
                       (struct sockaddr*)&addr, sizeof(addr));
    if (n < 0) {
        return -(int32_t)errno;
    }
    return (int32_t)n;
}

char* glyph_net_udp_recv(int32_t fd, uint32_t max_bytes) {
    glyph_net_last_errno = 0;

    if (max_bytes == 0) {
        return empty_string();
    }

    char* buf = (char*)malloc((size_t)max_bytes + 1);
    if (!buf) {
        glyph_net_last_errno = ENOMEM;
        return empty_string();
    }

    struct sockaddr_in from_addr;
    socklen_t from_len = sizeof(from_addr);

    ssize_t n = recvfrom(fd, buf, (size_t)max_bytes, 0,
                         (struct sockaddr*)&from_addr, &from_len);
    if (n < 0) {
        glyph_net_last_errno = errno;
        free(buf);
        return empty_string();
    }

    buf[n] = '\0';
    if ((size_t)n + 1 < (size_t)max_bytes + 1) {
        char* trimmed = (char*)realloc(buf, (size_t)n + 1);
        if (trimmed) buf = trimmed;
    }
    return buf;
}

// --- Utility ---

uint32_t glyph_net_local_port(int32_t fd) {
    struct sockaddr_in addr;
    socklen_t len = sizeof(addr);
    if (getsockname(fd, (struct sockaddr*)&addr, &len) != 0) {
        return 0;
    }
    return (uint32_t)ntohs(addr.sin_port);
}

// --- File serving ---

int64_t glyph_file_size(const char* path) {
    if (path == NULL) return -1;
    struct stat st;
    if (stat(path, &st) != 0) return -1;
    return (int64_t)st.st_size;
}

int64_t glyph_net_tcp_send_file(int32_t fd, const char* path) {
    if (path == NULL) return -(int64_t)EINVAL;

    FILE* f = fopen(path, "rb");
    if (!f) return -(int64_t)errno;

    char buf[8192];
    int64_t total = 0;
    size_t nread;

    while ((nread = fread(buf, 1, sizeof(buf), f)) > 0) {
        size_t written = 0;
        while (written < nread) {
            ssize_t n;
            do {
                n = write(fd, buf + written, nread - written);
            } while (n < 0 && errno == EINTR);
            if (n < 0) {
                int err = errno;
                fclose(f);
                return -(int64_t)err;
            }
            written += (size_t)n;
            total += (int64_t)n;
        }
    }

    fclose(f);
    return total;
}

#else

// Stub implementations for unsupported platforms
int32_t glyph_net_get_last_error(void) { return 0; }
int32_t glyph_net_tcp_socket(void) { return -ENOSYS; }
int32_t glyph_net_tcp_connect(const char* h, uint32_t p) { (void)h;(void)p; return -ENOSYS; }
int32_t glyph_net_bind(int32_t fd, const char* h, uint32_t p) { (void)fd;(void)h;(void)p; return -ENOSYS; }
int32_t glyph_net_listen(int32_t fd, int32_t b) { (void)fd;(void)b; return -ENOSYS; }
int32_t glyph_net_accept(int32_t fd) { (void)fd; return -ENOSYS; }
int32_t glyph_net_tcp_send(int32_t fd, const char* d) { (void)fd;(void)d; return -ENOSYS; }
char* glyph_net_tcp_recv(int32_t fd, uint32_t m) { (void)fd;(void)m; char* s=malloc(1); if(s)s[0]='\0'; return s; }
int32_t glyph_net_close(int32_t fd) { (void)fd; return -ENOSYS; }
int32_t glyph_net_set_reuse_addr(int32_t fd) { (void)fd; return -ENOSYS; }
int32_t glyph_net_udp_socket(void) { return -ENOSYS; }
int32_t glyph_net_udp_bind(int32_t fd, const char* h, uint32_t p) { (void)fd;(void)h;(void)p; return -ENOSYS; }
int32_t glyph_net_udp_send_to(int32_t fd, const char* d, const char* h, uint32_t p) { (void)fd;(void)d;(void)h;(void)p; return -ENOSYS; }
char* glyph_net_udp_recv(int32_t fd, uint32_t m) { (void)fd;(void)m; char* s=malloc(1); if(s)s[0]='\0'; return s; }
uint32_t glyph_net_local_port(int32_t fd) { (void)fd; return 0; }
int64_t glyph_file_size(const char* p) { (void)p; return -ENOSYS; }
int64_t glyph_net_tcp_send_file(int32_t fd, const char* p) { (void)fd;(void)p; return -ENOSYS; }

#endif
