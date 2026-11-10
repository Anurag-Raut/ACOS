/*
 * ACOS native-loader test library (C++).
 *
 * Exercises std::string, std::vector, exceptions thrown and caught inside the
 * library, a static object with a destructor, and new/delete.
 *
 * Built with the NDK clang++ using static libc++ + static libunwind so the
 * library is self-contained (no external libc++_shared), while still relying
 * on the loader for `dl_iterate_phdr` so the unwinder can find EH frames.
 */

#include <android/log.h>
#include <cstdint>
#include <new>
#include <stdexcept>
#include <string>
#include <vector>

#define TAG "acos-testcpp"

static int dtor_count = 0;

struct Tracker {
    ~Tracker() {
        dtor_count++;
        __android_log_print(ANDROID_LOG_INFO, TAG, "static object destructor");
    }
};

static Tracker tracker;

extern "C" int cpp_run_all(void) {
    int mask = 0;

    std::string s = "abc";
    s += "def";
    if (s != "abcdef") mask |= 1;

    std::vector<int> v{1, 2, 3, 4};
    v.push_back(5);
    int sum = 0;
    for (int x : v) sum += x;
    if (sum != 15) mask |= 2;

    int *p = new int(7);
    if (!p || *p != 7) mask |= 4;
    delete p;

    try {
        throw std::runtime_error("boom");
    } catch (const std::exception &e) {
        if (std::string(e.what()) != "boom") mask |= 8;
    }

    try {
        std::string t;
        (void)t.at(100);
        mask |= 16;
    } catch (const std::out_of_range &) {
        /* expected */
    }

    return mask;
}

extern "C" int cpp_dtor_count(void) { return dtor_count; }
