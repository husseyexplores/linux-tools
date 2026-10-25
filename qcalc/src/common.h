/* Shared between the qcalc client and the qcalcd daemon. */
#pragma once
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

/* Abstract unix socket (no file on disk, nothing stale after a crash), one per user. */
static inline socklen_t qc_sock_addr(struct sockaddr_un *a) {
    memset(a, 0, sizeof *a);
    a->sun_family = AF_UNIX;
    int n = snprintf(a->sun_path + 1, sizeof a->sun_path - 1, "qcalc-%u", (unsigned)getuid());
    return (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + n);
}
