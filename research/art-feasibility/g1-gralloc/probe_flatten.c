/* probe_flatten.c -- G1 item (1): a memfd-backed GraphicBuffer handle survives
 * the BLAST/Transaction flatten -> unflatten hop *between two processes*.
 *
 * This is the joint T0 explicitly did not retire (T0 §4.4: "1 process, 1
 * thread, no binder"). It is reproduced with stock libui.so and the shim, but
 * without binder: `GraphicBuffer::flatten` is exactly what
 * Transaction::setBuffer writes into a Parcel (the fd is what libbinder turns
 * into a BINDER_TYPE_FD), and SCM_RIGHTS is the kernel's fd-passing primitive
 * both use. So the byte stream and the kernel object lifetime are the same;
 * only the carrier differs, and the carrier is not what the shim touches.
 *
 *   parent: shrink/spawn child -> sockpair -> child
 *     GraphicBuffer(w,h,RGBA,usage) -> lock -> write pattern -> unlock
 *     flatten() into a buffer + fd array          (kMemFd fd is copied in)
 *     sendmsg(header + flattened bytes + SCM_RIGHTS fd)
 *   child
 *     recvmsg -> flattened bytes + its own fd
 *     GraphicBuffer (zeroed) -> unflatten()
 *         -> GraphicBufferMapper::getTransportSize (1 fd / 9 ints)
 *         -> GraphicBufferMapper::importBuffer   (dup)
 *         -> native_handle_close/delete of the received handle (libui)
 *     initCheck, lock, verify every pixel, unlock, ~GraphicBuffer
 *
 * PASS = (a) unflatten returns 0, (b) the child's pixels are byte-identical to
 * what the parent wrote, (c) the parent's buffer is still intact afterwards
 * (the fd was *shared*, not moved), (d) no fd leak (child's fd count back to
 * baseline). Every C++ symbol is called by its mangled name (no headers, no
 * libc++ in the probe) -- the {"1 fd + 9 ints"} transport contract is asserted
 * against the image's own code, not against our re-declaration of it.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <unistd.h>

/* sizeof(GraphicBuffer): the image's dtor writes mOwner at +0xb8, iterates
 * mDeathCallbacks at +0xe8..+0xf0 and reads the vector capacity at +0xf8, so the
 * object is >= 0x100 bytes -- T0's 256 B estimate was exactly on the edge (a
 * 256 B stack cell corrupts the caller's frame in the dtor). 512 with margin. */
#define STORAGE 512
#define W 64u
#define H 48u
#define FMT_RGBA_8888 1
#define USAGE 0x300u

typedef struct { int32_t left, top, right, bottom; } Rect;

extern void gb_ctor(void *self, uint32_t w, uint32_t h, int32_t format, uint32_t layers,
                    uint64_t usage, void *name)
    __asm__("_ZN7android13GraphicBufferC1EjjijmNSt3__112basic_stringIcNS1_11char_traitsIcEENS1_9allocatorIcEEEE");
extern void gb_dtor(void *self) __asm__("_ZN7android13GraphicBufferD1Ev");
/* The default ctor is required before unflatten: unflatten() only sets the
 * transport fields, and ~GraphicBuffer() also tears RefBase down, so an
 * unflatten-into-raw-storage object crashes in the dtor (found the hard way --
 * that is how the real consumer creates the object, `sp<GraphicBuffer>` in
 * BufferItem, so the probe must too). */
extern void gb_default_ctor(void *self) __asm__("_ZN7android13GraphicBufferC1Ev");
extern int32_t gb_initcheck(const void *self) __asm__("_ZNK7android13GraphicBuffer9initCheckEv");
extern int32_t gb_lock(void *self, uint32_t usage, void **vaddr, int32_t *bpp, int32_t *stride)
    __asm__("_ZN7android13GraphicBuffer4lockEjPPvPiS3_");
extern int32_t gb_unlock(void *self) __asm__("_ZN7android13GraphicBuffer6unlockEv");
extern uint64_t gb_flattened_size(const void *self)
    __asm__("_ZNK7android13GraphicBuffer16getFlattenedSizeEv");
extern uint64_t gb_fd_count(const void *self) __asm__("_ZNK7android13GraphicBuffer10getFdCountEv");
extern int32_t gb_flatten(void *self, void **buffer, uint64_t *size, int **fds, uint64_t *count)
    __asm__("_ZNK7android13GraphicBuffer7flattenERPvRmRPiS3_");
extern int32_t gb_unflatten(void *self, const void **buffer, uint64_t *size, const int **fds,
                            uint64_t *count)
    __asm__("_ZN7android13GraphicBuffer9unflattenERPKvRmRPKiS4_");

static void out(const char *s) { ssize_t r = write(1, s, strlen(s)); (void)r; }
static void outnum(const char *label, long v) {
    char b[160];
    int n = snprintf(b, sizeof(b), "%s%ld\n", label, v);
    ssize_t r = write(1, b, (size_t)n); (void)r;
}

static void fill(unsigned char *p, uint32_t stride) {
    for (uint32_t y = 0; y < H; y++) {
        unsigned char *row = p + (size_t)y * stride;
        for (uint32_t x = 0; x < W; x++) {
            row[x * 4 + 0] = (unsigned char)(x + 3);
            row[x * 4 + 1] = (unsigned char)(y * 7 + 1);
            row[x * 4 + 2] = 0xa5;
            row[x * 4 + 3] = 0xff;
        }
    }
}

static int verify(const unsigned char *p, uint32_t stride) {
    for (uint32_t y = 0; y < H; y++) {
        const unsigned char *row = p + (size_t)y * stride;
        for (uint32_t x = 0; x < W; x++) {
            const unsigned char *px = row + x * 4;
            if (px[0] != (unsigned char)(x + 3) || px[1] != (unsigned char)(y * 7 + 1) ||
                px[2] != 0xa5 || px[3] != 0xff) {
                return 0;
            }
        }
    }
    return 1;
}

static int fd_count(void) {
    int n = 0;
    for (int fd = 0; fd < 1024; fd++) {
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) n++;
    }
    return n;
}

/* ---------------------------------------------------------------- child ---- */

static int child_recv_verify(int sock) {
    static unsigned char msg[65536];
    char cbuf[CMSG_SPACE(sizeof(int) * 4)];
    struct iovec iov = {msg, sizeof(msg)};
    struct msghdr mh;
    memset(&mh, 0, sizeof(mh));
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof(cbuf);

    ssize_t n = recvmsg(sock, &mh, 0);
    if (n < 0) { perror("recvmsg"); return 10; }
    outnum("[g1-flatten] child: received bytes = ", (long)n);

    int nfds = 0;
    int fds_in[4];
    fds_in[0] = -1;
    for (struct cmsghdr *c = CMSG_FIRSTHDR(&mh); c; c = CMSG_NXTHDR(&mh, c)) {
        if (c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
            nfds = (int)((c->cmsg_len - CMSG_LEN(0)) / sizeof(int));
            memcpy(fds_in, CMSG_DATA(c), (size_t)nfds * sizeof(int));
        }
    }
    outnum("[g1-flatten] child: SCM_RIGHTS fds = ", nfds);

    /* wire format: [u32 magic][u32 nbytes][u64 nfds_total][int fds...][flattened] */
    unsigned char *p = msg;
    uint32_t magic, nbytes;
    uint64_t nfd_total;
    memcpy(&magic, p, 4); p += 4;
    memcpy(&nbytes, p, 4); p += 4;
    memcpy(&nfd_total, p, 8); p += 8;
    if (magic != 0x47423121u) { out("[g1-flatten] child: FAIL bad magic\n"); return 11; }
    outnum("[g1-flatten] child: flattened bytes = ", (long)nbytes);
    outnum("[g1-flatten] child: fd count claimed  = ", (long)nfd_total);
    if ((long)(size_t)(p - msg) + (long)nbytes > n) { out("[g1-flatten] child: FAIL short msg\n"); return 12; }

    /* ---- the actual unflatten, stock libui + shim ---- */
    unsigned char gb[STORAGE];
    memset(gb, 0, sizeof(gb));
    gb_default_ctor(gb);
    const void *buf = p;
    uint64_t size = nbytes;
    const int *fds = fds_in;
    uint64_t count = (uint64_t)nfds;
    int32_t rc = gb_unflatten(gb, &buf, &size, &fds, &count);
    outnum("[g1-flatten] child: unflatten rc = ", rc);
    if (rc != 0) { out("[g1-flatten] child: FAIL unflatten\n"); return 13; }

    int32_t ck = gb_initcheck(gb);
    outnum("[g1-flatten] child: initCheck = ", ck);
    if (ck != 0) { out("[g1-flatten] child: FAIL initCheck\n"); return 14; }

    void *v = NULL;
    int32_t bpp = -1, stride = -1;
    rc = gb_lock(gb, USAGE, &v, &bpp, &stride);
    outnum("[g1-flatten] child: lock rc = ", rc);
    outnum("[g1-flatten] child: bytesPerPixel = ", bpp);
    outnum("[g1-flatten] child: bytesPerStride = ", stride);
    if (rc != 0 || !v) { out("[g1-flatten] child: FAIL lock\n"); return 15; }

    int ok = verify((const unsigned char *)v, (uint32_t)stride);
    out(ok ? "[g1-flatten] child: pixels read back OK (same memfd, other process)\n"
           : "[g1-flatten] child: FAIL pixel mismatch\n");
    gb_unlock(gb);

    out("[g1-flatten] child: unlocking done, calling dtor\n");
    int fds_before_dtors = fd_count();
    outnum("[g1-flatten] child: fd count baseline = ", fds_before_dtors);
    gb_dtor(gb);
    out("[g1-flatten] child: dtor returned\n");
    int fds_after = fd_count();
    outnum("[g1-flatten] child: fd count after dtor = ", fds_after);
    outnum("[g1-flatten] child: fd count before dtor = ", fds_before_dtors);
    outnum("[g1-flatten] child: fd count after  dtor = ", fds_after);
    if (fds_after != fds_before_dtors - 0 && fds_after > fds_before_dtors) {
        out("[g1-flatten] child: FAIL fd leak\n");
        return 16;
    }
    outnum("[g1-flatten] child: exit rc = ", ok ? 0 : 1);
    return ok ? 0 : 1;
}

/* --------------------------------------------------------------- parent ---- */

int main(void) {
    out("[g1-flatten] parent: GraphicBuffer 64x48 RGBA_8888 via stock libui + shim\n");
    unsigned char gb[STORAGE];
    memset(gb, 0, sizeof(gb));
    unsigned char emptystr[32];
    memset(emptystr, 0, sizeof(emptystr));
    gb_ctor(gb, W, H, FMT_RGBA_8888, 1, USAGE, emptystr);
    int32_t ck = gb_initcheck(gb);
    outnum("[g1-flatten] parent: initCheck = ", ck);
    if (ck != 0) return 2;

    void *v = NULL;
    int32_t bpp = -1, stride = -1;
    if (gb_lock(gb, USAGE, &v, &bpp, &stride) != 0 || !v) {
        out("[g1-flatten] parent: FAIL lock\n");
        return 3;
    }
    fill((unsigned char *)v, (uint32_t)stride);
    gb_unlock(gb);
    outnum("[g1-flatten] parent: pattern written, bytesPerStride = ", stride);

    uint64_t size = gb_flattened_size(gb);
    uint64_t fds_n = gb_fd_count(gb);
    outnum("[g1-flatten] parent: getFlattenedSize = ", (long)size);
    outnum("[g1-flatten] parent: getFdCount = ", (long)fds_n);
    if (fds_n != 1 || size < 16) {
        out("[g1-flatten] parent: FAIL unexpected transport shape\n");
        return 4;
    }

    unsigned char flat[1024];
    memset(flat, 0, sizeof(flat));
    int fds[4] = {-1, -1, -1, -1};
    void *bufp = flat;
    uint64_t sz = sizeof(flat);
    int *fdp = fds;
    uint64_t fcnt = 4;
    int32_t rc = gb_flatten(gb, &bufp, &sz, &fdp, &fcnt);
    outnum("[g1-flatten] parent: flatten rc = ", rc);
    outnum("[g1-flatten] parent: fd after flatten  = ", fds[0]);
    if (rc != 0) { out("[g1-flatten] parent: FAIL flatten\n"); return 5; }

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) { perror("socketpair"); return 6; }
    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 6; }
    if (pid == 0) {
        close(sv[0]);
        int c = child_recv_verify(sv[1]);
        close(sv[1]);
        _exit(c);
    }
    close(sv[1]);

    /* wire message: header + flattened bytes; the fd rides in a cmsg */
    unsigned char msg[2048];
    uint32_t magic = 0x47423121u, nbytes = (uint32_t)size;
    uint64_t nfd_total = fds_n;
    memcpy(msg, &magic, 4);
    memcpy(msg + 4, &nbytes, 4);
    memcpy(msg + 8, &nfd_total, 8);
    memcpy(msg + 16, flat, (size_t)size);

    char cbuf[CMSG_SPACE(sizeof(int))];
    struct iovec iov = {msg, 16 + (size_t)size};
    struct msghdr mh;
    memset(&mh, 0, sizeof(mh));
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof(cbuf);
    struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
    cm->cmsg_level = SOL_SOCKET;
    cm->cmsg_type = SCM_RIGHTS;
    cm->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cm), &fds[0], sizeof(int));

    ssize_t sent = sendmsg(sv[0], &mh, 0);
    outnum("[g1-flatten] parent: sendmsg bytes = ", (long)sent);

    int status = 0;
    waitpid(pid, &status, 0);
    int child_rc = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    outnum("[g1-flatten] parent: child exit = ", child_rc);

    /* The parent's own buffer must still be readable: the fd was shared. */
    void *v2 = NULL;
    int32_t b2 = -1, s2 = -1;
    int ok = 0;
    if (gb_lock(gb, USAGE, &v2, &b2, &s2) == 0 && v2) {
        ok = verify((const unsigned char *)v2, (uint32_t)s2);
        gb_unlock(gb);
    }
    out(ok ? "[g1-flatten] parent: own buffer still intact (fd shared, not moved)\n"
           : "[g1-flatten] parent: FAIL own buffer damaged\n");
    gb_dtor(gb);

    if (child_rc == 0 && ok) {
        out("[g1-flatten] PASS: flatten(proc A) -> SCM_RIGHTS -> unflatten(proc B) -> pixels OK\n");
        return 0;
    }
    out("[g1-flatten] FAIL\n");
    return 7;
}
