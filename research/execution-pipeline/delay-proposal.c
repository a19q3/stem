#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <string.h>
#include <time.h>

// Delay the worker's first job while the replica's pacemaker remains live.
int clock_gettime(clockid_t clock, struct timespec *result) {
    static __thread int delayed;
    int (*read_clock)(clockid_t, struct timespec *) =
        dlsym(RTLD_NEXT, "clock_gettime");
    if (clock == CLOCK_MONOTONIC && !delayed) {
        char name[16];
        if (pthread_getname_np(pthread_self(), name, sizeof(name)) == 0 &&
            strcmp(name, "stem-proposal") == 0) {
            delayed = 1;
            const struct timespec delay = {2, 0};
            nanosleep(&delay, 0);
        }
    }
    return read_clock(clock, result);
}
