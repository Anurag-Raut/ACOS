/*
 * ACOS hostile-native fixture library (owned test code).
 *
 * Compiled by `tools/build-apps.sh` and loaded under the ADR-0022 confinement.
 * `hostileProbe` attempts one action from each denied class and returns a
 * bitmask of what *succeeded*; the confinement must keep it zero:
 *   bit 0  open("/etc/passwd")              (Landlock filesystem rule)
 *   bit 1  socket(AF_INET, SOCK_STREAM, 0)  (seccomp network rule)
 *   bit 2  create+write "/tmp/acos-escape"  (Landlock write outside the data dir)
 *
 * The seccomp `execve`/`ptrace`/`process_vm_*` rules are exercised by the
 * `acos-sandbox` layer tests (ADR-0022 evidence); the ACOS bionic symbol table
 * does not export `execve`/`ptrace`, so a guest ELF cannot import them here.
 */
#include <jni.h>
#include <fcntl.h>
#include <sys/socket.h>
#include <unistd.h>

static jint probe(void) {
    jint bits = 0;

    int passwd = open("/etc/passwd", O_RDONLY);
    if (passwd >= 0) {
        bits |= 1;
        close(passwd);
    }

    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock >= 0) {
        bits |= 2;
        close(sock);
    }

    int esc = open("/tmp/acos-escape", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (esc >= 0) {
        if (write(esc, "x", 1) == 1) {
            bits |= 4;
        }
        close(esc);
    }

    return bits;
}

JNIEXPORT jint JNICALL
Java_dev_acos_fixtures_nativehostile_HostileApp_hostileProbe(JNIEnv *env, jobject thiz) {
    (void) env;
    (void) thiz;
    return probe();
}
