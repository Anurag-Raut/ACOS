/*
 * q3prop.c -- LD_PRELOAD shim that serves exactly one extra property:
 *   servicemanager.ready = true
 *
 * libbinder's defaultServiceManager() (native/libs/binder/IServiceManager.cpp:
 * 148-166) first spins on
 *     while (!WaitForProperty("servicemanager.ready", "true", 1s)) ...
 * and only then asks ProcessState for the context object. In the ACOS sandbox
 * there is no init and no property service, so that loop never terminates and
 * nothing after it is reachable. Serving the property is how the *next* failure
 * (or the SIGSEGV E4H reported) becomes observable.
 *
 * Load it BEFORE libbindershim.so so it wins the symbol, and chain everything
 * else through RTLD_NEXT (bindershim -> bionic).
 */
#include <dlfcn.h>
#include <stdint.h>
#include <string.h>

#define Q3_FAKE_KEY "servicemanager.ready"

static struct { char name[128]; char value[128]; } fake = {
        Q3_FAKE_KEY, "true" };

static int is_fake(const char* key) { return key && strcmp(key, Q3_FAKE_KEY) == 0; }

static int is_ours(const void* pi) {
    return (const char*)pi == (const char*)&fake;
}

int __system_property_get(const char* key, char* value) {
    if (is_fake(key)) {
        memcpy(value, fake.value, strlen(fake.value) + 1);
        return (int)strlen(fake.value);
    }
    static int (*real)(const char*, char*);
    if (!real) real = (int (*)(const char*, char*))dlsym(RTLD_NEXT, "__system_property_get");
    return real ? real(key, value) : 0;
}

const void* __system_property_find(const char* key) {
    if (is_fake(key)) return &fake;
    static const void* (*real)(const char*);
    if (!real) real = (const void* (*)(const char*))dlsym(RTLD_NEXT, "__system_property_find");
    return real ? real(key) : 0;
}

void __system_property_read_callback(const void* pi,
                                     void (*callback)(void*, const char*, const char*, unsigned),
                                     void* cookie) {
    if (is_ours(pi)) {
        callback(cookie, fake.name, fake.value, 1);
        return;
    }
    static void (*real)(const void*, void (*)(void*, const char*, const char*, unsigned), void*);
    if (!real) {
        real = (void (*)(const void*, void (*)(void*, const char*, const char*, unsigned), void*))
                dlsym(RTLD_NEXT, "__system_property_read_callback");
    }
    if (real) real(pi, callback, cookie);
}
