/* SPIKE-NET probe (spike only, not product code).
 *
 * Proves, from inside a network namespace, three things at once:
 *   1. a TCP connect to a public address succeeds (internet reachable),
 *   2. a TCP connect to the host's 127.0.0.1 does NOT succeed (host loopback
 *      not reachable through pasta),
 *   3. a UDP DNS query to 1.1.1.1 answers (pasta DNS forwarding).
 *
 * Static build so it can run inside a sandbox with no host libc:
 *   gcc -static -O2 -o probe probe.c
 */
#include <arpa/inet.h>
#include <errno.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <poll.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <unistd.h>

static int connect_tcp(const char *ip, int port, int timeout_ms) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) { printf("  socket: %s\n", strerror(errno)); return -1; }
    struct timeval tv = { timeout_ms / 1000, (timeout_ms % 1000) * 1000 };
    setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv);
    struct sockaddr_in sa;
    memset(&sa, 0, sizeof sa);
    sa.sin_family = AF_INET;
    sa.sin_port = htons(port);
    inet_pton(AF_INET, ip, &sa.sin_addr);
    int rc = connect(fd, (struct sockaddr *)&sa, sizeof sa);
    if (rc != 0 && errno == EINPROGRESS) {
        /* SO_SNDTIMEO makes connect() non-blocking on Linux. */
        struct pollfd p = { fd, POLLOUT, 0 };
        rc = -1;
        if (poll(&p, 1, timeout_ms) == 1) {
            int soerr = 0;
            socklen_t sl = sizeof soerr;
            if (getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &sl) == 0 && soerr == 0) rc = 0;
            else errno = soerr;
        } else {
            errno = ETIMEDOUT;
        }
    }
    int err = errno;
    close(fd);
    printf("  connect %s:%d -> %s\n", ip, port, rc == 0 ? "OK" : strerror(err));
    return rc;
}

int main(int argc, char **argv) {
    const char *public_ip = argc > 1 ? argv[1] : "93.184.215.14"; /* example.com */
    int public_port = argc > 2 ? atoi(argv[2]) : 443;
    int host_port = argc > 3 ? atoi(argv[3]) : 1;

    printf("== SPIKE-NET probe ==\n");
    printf("public tcp:\n");
    int internet = connect_tcp(public_ip, public_port, 4000) == 0;
    printf("host loopback tcp (must fail):\n");
    int loopback = connect_tcp("127.0.0.1", host_port, 2000) == 0;
    printf("lan gateway tcp (must fail):\n");
    int lan = connect_tcp("192.168.31.1", 80, 2000) == 0;

    /* DNS over UDP through pasta's forwarder. */
    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    int dns_ok = 0;
    if (fd >= 0) {
        struct timeval tv = { 4, 0 };
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv);
        struct sockaddr_in sa;
        memset(&sa, 0, sizeof sa);
        sa.sin_family = AF_INET;
        sa.sin_port = htons(53);
        inet_pton(AF_INET, "1.1.1.1", &sa.sin_addr);
        /* Minimal A query for example.com. */
        unsigned char q[] = { 0xab,0xcd, 0x01,0x00, 0,1, 0,0, 0,0, 0,0,
            7,'e','x','a','m','p','l','e', 3,'c','o','m', 0, 0,1, 0,1 };
        if (sendto(fd, q, sizeof q, 0, (struct sockaddr *)&sa, sizeof sa) > 0) {
            unsigned char buf[512];
            ssize_t n = recv(fd, buf, sizeof buf, 0);
            dns_ok = n > 0 && (buf[3] & 0x0f) == 0;
            printf("  udp dns 1.1.1.1:53 -> %s (n=%zd)\n", dns_ok ? "OK" : "FAIL", n);
        } else {
            printf("  udp dns send: %s\n", strerror(errno));
        }
        close(fd);
    }
    printf("RESULT internet=%s host_loopback=%s lan=%s dns=%s\n",
           internet ? "reachable" : "unreachable",
           loopback ? "REACHABLE_BAD" : "blocked",
           lan ? "REACHABLE_BAD" : "blocked",
           dns_ok ? "ok" : "fail");
    return internet && !loopback && !lan && dns_ok ? 0 : 1;
}
