/*
 * Journey-only durability barriers for production owner and worker journals.
 *
 * `write_private_atomic` writes and syncs the replacement file, renames it, then
 * syncs the parent directory. The worker barrier verifies a durable `.pending`
 * result before ResultReceipt. Owner barriers inspect v4 selection rows: state
 * 0 is held before Turso selection, state 1 before Stored ACK, and state 3
 * after the worker tombstone is durable but before retirement confirmation.
 * SIGUSR1 releases the confirmation barrier. The owner journal uses the v4
 * source-root-bound row format.
 */
#define _GNU_SOURCE

#include <dlfcn.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <limits.h>
#include <time.h>

#if defined(__APPLE__)
#include <sys/param.h>
#endif

typedef int (*fsync_function)(int);

static pthread_once_t resolve_fsync_once = PTHREAD_ONCE_INIT;
static fsync_function next_fsync = NULL;

static void resolve_next_fsync(void) {
    next_fsync = (fsync_function)dlsym(RTLD_NEXT, "fsync");
}

static int path_for_fd(int fd, char *path, size_t capacity) {
#if defined(__APPLE__)
    (void)capacity;
    return fcntl(fd, F_GETPATH, path) == 0;
#elif defined(__linux__)
    char descriptor[64];
    int length = snprintf(descriptor, sizeof(descriptor), "/proc/self/fd/%d", fd);
    if (length <= 0 || (size_t)length >= sizeof(descriptor)) {
        return 0;
    }
    ssize_t path_length = readlink(descriptor, path, capacity - 1);
    if (path_length <= 0 || (size_t)path_length >= capacity) {
        return 0;
    }
    path[path_length] = '\0';
    return 1;
#else
    (void)fd;
    (void)path;
    (void)capacity;
    return 0;
#endif
}

static int pending_stored_ack_state(const char *directory) {
    char journal[PATH_MAX];
    int length = snprintf(
        journal,
        sizeof(journal),
        "%s/pending-stored-acks.v4",
        directory
    );
    if (length <= 0 || (size_t)length >= sizeof(journal)) {
        return 0;
    }

    int fd = open(journal, O_RDONLY);
    if (fd < 0) {
        return 0;
    }
    uint8_t header[22] = {0};
    ssize_t bytes = pread(fd, header, sizeof(header), 0);
    (void)close(fd);
    uint16_t version = (uint16_t)(((uint16_t)header[8] << 8) | header[9]);
    uint16_t rows = (uint16_t)(((uint16_t)header[10] << 8) | header[11]);
    uint32_t body_length = ((uint32_t)header[12] << 24)
        | ((uint32_t)header[13] << 16)
        | ((uint32_t)header[14] << 8)
        | (uint32_t)header[15];
    uint32_t record_length = ((uint32_t)header[16] << 24)
        | ((uint32_t)header[17] << 16)
        | ((uint32_t)header[18] << 8)
        | (uint32_t)header[19];
    if (bytes != (ssize_t)sizeof(header)
        || memcmp(header, "BKPSACK4", 8) != 0
        || version != 4
        || rows != 1
        || record_length == 0
        || record_length > UINT32_MAX - 4
        || body_length != record_length + 4
        || header[20] != 1
        || (header[21] != 0 && header[21] != 1 && header[21] != 3)) {
        return -1;
    }
    return header[21];
}

static int is_ack_journal_directory(int fd, char *directory, size_t capacity) {
    if (!path_for_fd(fd, directory, capacity)) {
        return 0;
    }
    static const char suffix[] = "/compiler-pending-stored-acks";
    size_t length = strlen(directory);
    size_t suffix_length = sizeof(suffix) - 1;
    return length >= suffix_length
        && memcmp(directory + length - suffix_length, suffix, suffix_length) == 0;
}

static int is_worker_result_directory(int fd, char *directory, size_t capacity) {
    if (!path_for_fd(fd, directory, capacity)) {
        return 0;
    }
    static const char suffix[] = "/result-cas/worker-results";
    size_t length = strlen(directory);
    size_t suffix_length = sizeof(suffix) - 1;
    return length >= suffix_length
        && memcmp(directory + length - suffix_length, suffix, suffix_length) == 0;
}

static int has_durable_pending_result(const char *directory) {
    DIR *stream = opendir(directory);
    if (stream == NULL) {
        return 0;
    }
    int found = 0;
    struct dirent *entry;
    while ((entry = readdir(stream)) != NULL) {
        size_t length = strlen(entry->d_name);
        if (length <= 8 || strcmp(entry->d_name + length - 8, ".pending") != 0) {
            continue;
        }
        char path[PATH_MAX];
        int written = snprintf(path, sizeof(path), "%s/%s", directory, entry->d_name);
        if (written <= 0 || (size_t)written >= sizeof(path)) {
            continue;
        }
        int fd = open(path, O_RDONLY);
        if (fd < 0) {
            continue;
        }
        uint8_t magic[8] = {0};
        ssize_t count = pread(fd, magic, sizeof(magic), 0);
        (void)close(fd);
        if (count == (ssize_t)sizeof(magic) && memcmp(magic, "BKCWRJ02", 8) == 0) {
            found = 1;
            break;
        }
    }
    (void)closedir(stream);
    return found;
}

static volatile sig_atomic_t resume_requested = 0;

static void release_pause(int signal_number) {
    if (signal_number == SIGUSR1) {
        resume_requested = 1;
    }
}

static void publish_ready_marker_and_block(const char *marker, int state) {
    if (marker == NULL || marker[0] == '\0' || access(marker, F_OK) == 0) {
        return;
    }

    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = release_pause;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0) {
        return;
    }

    int fd = open(marker, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) {
        return;
    }
    const char *ready = "worker-pending-before-result-receipt\n";
    switch (state) {
        case 0:
            ready = "awaiting-selection\n";
            break;
        case 1:
            ready = "stored-ack-pending\n";
            break;
        case 3:
            ready = "stored-awaiting-retirement-confirm\n";
            break;
        default:
            break;
    }
    ssize_t written = write(fd, ready, strlen(ready));
    (void)close(fd);
    if (written != (ssize_t)strlen(ready)) {
        (void)unlink(marker);
        return;
    }

    /* The parent sees the marker while this fsync call is still blocked. */
    const struct timespec interval = { .tv_sec = 0, .tv_nsec = 1000000 };
    while (!resume_requested) {
        (void)nanosleep(&interval, NULL);
    }
}

static int journey_fsync(int fd) {
    (void)pthread_once(&resolve_fsync_once, resolve_next_fsync);
    if (next_fsync == NULL) {
        errno = ENOSYS;
        return -1;
    }

    int result = next_fsync(fd);
    int saved_errno = errno;
    if (result == 0) {
        char directory[PATH_MAX];
        if (is_ack_journal_directory(fd, directory, sizeof(directory))) {
            int state = pending_stored_ack_state(directory);
            const char *marker = NULL;
            if (state == 0) {
                marker = getenv("BACKEND_JOURNEY_ACK_PAUSE_STATE0_MARKER");
            } else if (state == 1) {
                marker = getenv("BACKEND_JOURNEY_ACK_PAUSE_STATE1_MARKER");
            } else if (state == 3) {
                marker = getenv("BACKEND_JOURNEY_ACK_PAUSE_STATE3_MARKER");
            }
            if (marker != NULL) {
                publish_ready_marker_and_block(marker, state);
            }
        } else if (is_worker_result_directory(fd, directory, sizeof(directory))
            && has_durable_pending_result(directory)) {
            const char *marker = getenv("BACKEND_JOURNEY_OFFER_PENDING_MARKER");
            if (marker != NULL) {
                publish_ready_marker_and_block(marker, 5);
            }
        }
    }
    errno = saved_errno;
    return result;
}

#if defined(__APPLE__)
/* DYLD_INSERT_LIBRARIES uses this Mach-O interpose section on macOS. */
__attribute__((used, section("__DATA,__interpose")))
static struct {
    const void *replacement;
    const void *replacee;
} fsync_interpose = {
    (const void *)journey_fsync,
    (const void *)fsync,
};
#else
/* LD_PRELOAD resolves this symbol before libc on Linux. */
int fsync(int fd) {
    return journey_fsync(fd);
}
#endif
