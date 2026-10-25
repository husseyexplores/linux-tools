/* qcalc: the command you bind to a hotkey.
 * Sends a command to the running qcalcd, or starts it if it is not running.
 * Depends on libc only, so it starts in well under a millisecond. */
#include "common.h"

#include <fcntl.h>
#include <stdlib.h>

static void usage(void) {
    puts("usage: qcalc [toggle|show|hide|quit|reload|clear-history|update-rates]\n"
         "  toggle  show the calculator, or hide it if it is focused (default)\n"
         "  show    show and focus the calculator\n"
         "  hide    hide it\n"
         "  quit    stop the background process\n"
         "  reload  re-read ~/.config/qcalc/config\n"
         "  clear-history  forget all saved calculations\n"
         "  update-rates   download fresh currency exchange rates");
}

static void spawn_daemon(const char *self) {
    char dir[4096];
    ssize_t n = readlink("/proc/self/exe", dir, sizeof dir - 16);
    if (n <= 0) {
        n = (ssize_t)strlen(self);
        if ((size_t)n >= sizeof dir - 16) n = 0;
        memcpy(dir, self, (size_t)n);
    }
    dir[n] = 0;
    char *slash = strrchr(dir, '/');

    pid_t pid = fork();
    if (pid != 0) return; /* parent (or fork failure): done */
    setsid();
    int fd = open("/dev/null", O_RDWR);
    if (fd >= 0) {
        dup2(fd, 0);
        dup2(fd, 1);
        dup2(fd, 2);
        if (fd > 2) close(fd);
    }
    char *argv[] = {(char *)"qcalcd", (char *)"--show", NULL};
    if (slash) {
        strcpy(slash + 1, "qcalcd");
        execv(dir, argv);
    }
    execvp("qcalcd", argv);
    _exit(127);
}

int main(int argc, char **argv) {
    const char *cmd = argc > 1 ? argv[1] : "toggle";
    if (!strcmp(cmd, "-h") || !strcmp(cmd, "--help")) {
        usage();
        return 0;
    }
    if (strcmp(cmd, "toggle") && strcmp(cmd, "show") && strcmp(cmd, "hide") && strcmp(cmd, "quit") &&
        strcmp(cmd, "reload") && strcmp(cmd, "clear-history") && strcmp(cmd, "update-rates")) {
        usage();
        return 2;
    }

    int fd = socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
    struct sockaddr_un a;
    socklen_t len = qc_sock_addr(&a);
    if (fd >= 0 && sendto(fd, cmd, strlen(cmd), 0, (struct sockaddr *)&a, len) >= 0) return 0;

    /* Not running: clear the file directly. */
    if (!strcmp(cmd, "clear-history")) {
        char path[4096];
        const char *st = getenv("XDG_STATE_HOME"), *home = getenv("HOME");
        if (st && *st == '/') snprintf(path, sizeof path, "%s/qcalc/history", st);
        else snprintf(path, sizeof path, "%s/.local/state/qcalc/history", home ? home : "/tmp");
        int h = open(path, O_WRONLY | O_TRUNC | O_CLOEXEC);
        if (h >= 0) close(h);
        return 0;
    }
    /* Not running. Only commands that should show the window start it. */
    if (!strcmp(cmd, "toggle") || !strcmp(cmd, "show")) spawn_daemon(argv[0]);
    return 0;
}
