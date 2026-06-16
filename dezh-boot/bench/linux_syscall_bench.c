/* Linux syscall-floor baseline for BENCH.md (D015).
 * Times the cheapest real syscall (getpid, forced via syscall() to defeat the
 * glibc cache) on real hardware. Compile and run on a real Linux box:
 *   gcc -O2 linux_syscall_bench.c -o lsb && ./lsb
 */
#define _GNU_SOURCE
#include <unistd.h>
#include <sys/syscall.h>
#include <stdio.h>
#include <time.h>

int main(void) {
    long n = 5000000;
    struct timespec a, b;
    clock_gettime(CLOCK_MONOTONIC, &a);
    for (long i = 0; i < n; i++) {
        syscall(SYS_getpid);
    }
    clock_gettime(CLOCK_MONOTONIC, &b);
    double ns = ((double)(b.tv_sec - a.tv_sec) * 1e9 +
                 (double)(b.tv_nsec - a.tv_nsec)) /
                (double)n;
    printf("linux getpid raw syscall: %.1f ns/call over %ld iters\n", ns, n);
    return 0;
}
