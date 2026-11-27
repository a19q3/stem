#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static __thread int entered;

static void hold(const char *phase) {
    if (entered) return;
    const char *mode = getenv("STEM_COMMIT_FAULT_MODE");
    const char *arm = getenv("STEM_COMMIT_FAULT_ARM");
    if (!mode || !arm || strcmp(mode, phase) || access(arm, F_OK)) return;
    char name[16];
    if (pthread_getname_np(pthread_self(), name, sizeof(name)) ||
        strcmp(name, "stem-commit")) return;
    entered = 1;
    int fd = open(getenv("STEM_COMMIT_FAULT_REACHED"), O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (fd < 0) _exit(97);
    close(fd);
    const char *release = getenv("STEM_COMMIT_FAULT_RELEASE");
    const struct timespec poll = {0, 1000000};
    while (access(release, F_OK)) nanosleep(&poll, 0);
}

static int wal(int fd) {
    char path[64], value[512];
    snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
    ssize_t size = readlink(path, value, sizeof(value));
    return size >= 4 && !memcmp(value + size - 4, ".log", 4);
}

int clock_gettime(clockid_t clock, struct timespec *result) {
    int (*real_clock)(clockid_t, struct timespec *) = dlsym(RTLD_NEXT, "clock_gettime");
    if (clock == CLOCK_MONOTONIC) hold("before");
    return real_clock(clock, result);
}

static int sync_with_gate(const char *symbol, int fd) {
    int (*real_sync)(int) = dlsym(RTLD_NEXT, symbol);
    int is_wal = wal(fd);
    if (is_wal) hold("before-sync");
    int result = real_sync(fd);
    if (result == 0 && is_wal) hold("after-sync");
    return result;
}

int fsync(int fd) { return sync_with_gate("fsync", fd); }
int fdatasync(int fd) { return sync_with_gate("fdatasync", fd); }
