// W84-A: confined Hermes runtime probe for the `rn-hello` fixture.
//
// This is *our* fixture library (built with the Android NDK, x86_64). It is
// loaded through the ACOS JNI runtime (`System.loadLibrary("hermesprobe")`)
// right next to the real React Native / Hermes libraries, so it links against
// the same `libhermes.so` / `libjsi.so` / `libc++_shared.so` mappings the app
// uses. It initialises a Hermes runtime, installs a host `acosLog` function so
// JavaScript output is observable, evaluates a trivial expression, and then
// attempts to execute the fixture's bundled Hermes bytecode
// (`assets/index.android.bundle`, HBC96).
//
// Every step prints an `ACOS_HERMES ...` line; the Rust harness captures them
// as evidence and the first failure is recorded as a blocker.

#include <jni.h>

#include <hermes/hermes.h>
#include <jsi/jsi.h>

#include <cstdarg>
#include <cstdio>
#include <fstream>
#include <memory>
#include <string>
#include <vector>

namespace {

void emit(const char* line) {
  std::fprintf(stdout, "ACOS_HERMES %s\n", line);
  std::fflush(stdout);
}

// Escape newlines so every probe line stays one JSON line on stdout.
std::string one_line(const char* s) {
  std::string out;
  for (const char* p = s; *p; ++p) {
    if (*p == '\n') {
      out += "\\n";
    } else if (*p == '\r') {
      out += "\\r";
    } else if (*p == '"') {
      out += "'";
    } else {
      out += *p;
    }
  }
  return out;
}

void emitf(const char* fmt, ...) {
  char buf[1024];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(buf, sizeof(buf), fmt, ap);
  va_end(ap);
  emit(one_line(buf).c_str());
}

std::string js_bytes(JNIEnv* env, jstring s) {
  if (s == nullptr) {
    return {};
  }
  const char* c = env->GetStringUTFChars(s, nullptr);
  std::string out = c ? c : "";
  if (c) {
    env->ReleaseStringUTFChars(s, c);
  }
  return out;
}

int run_probe(const std::string& bundle_path) {
  namespace jsi = facebook::jsi;
  emit("{\"phase\":\"hermes-begin\"}");

  std::unique_ptr<facebook::hermes::HermesRuntime> runtime;
  try {
    runtime = facebook::hermes::makeHermesRuntime();
  } catch (const std::exception& e) {
    emitf("{\"phase\":\"hermes-make-error\",\"error\":\"%s\"}", e.what());
    return -1;
  }
  if (!runtime) {
    emit("{\"phase\":\"hermes-null\"}");
    return -1;
  }
  emit("{\"phase\":\"hermes-runtime-ok\"}");

  // Host function so JS `acosLog(...)` lands in our stdout capture.
  try {
    auto log = jsi::Function::createFromHostFunction(
        *runtime,
        jsi::PropNameID::forAscii(*runtime, "acosLog"),
        1,
        [](jsi::Runtime& rt,
           const jsi::Value&,
           const jsi::Value* args,
           size_t count) -> jsi::Value {
          if (count >= 1) {
            auto text = args[0].toString(rt).utf8(rt);
            emitf("{\"js\":\"%s\"}", text.c_str());
          }
          return jsi::Value::undefined();
        });
    runtime->global().setProperty(*runtime, "acosLog", log);
    emit("{\"phase\":\"hermes-hostfn-ok\"}");
  } catch (const std::exception& e) {
    emitf("{\"phase\":\"hermes-hostfn-error\",\"error\":\"%s\"}", e.what());
    return -1;
  }

  try {
    auto v = runtime->evaluateJavaScript(
        std::make_unique<jsi::StringBuffer>(
            "acosLog('hello from hermes ' + (1 + 1)); 40 + 2"),
        "acos-probe.js");
    emitf("{\"phase\":\"hermes-eval-ok\",\"value\":%g}", v.getNumber());
  } catch (const std::exception& e) {
    emitf("{\"phase\":\"hermes-eval-error\",\"error\":\"%s\"}", e.what());
    return -1;
  }

  if (bundle_path.empty()) {
    emit("{\"phase\":\"hbc-skipped\"}");
    return 0;
  }
  std::ifstream f(bundle_path, std::ios::binary);
  if (!f) {
    emitf("{\"phase\":\"hbc-open-error\",\"path\":\"%s\"}", bundle_path.c_str());
    return -1;
  }
  std::vector<unsigned char> data(
      (std::istreambuf_iterator<char>(f)), std::istreambuf_iterator<char>());
  emitf("{\"phase\":\"hbc-read\",\"bytes\":%zu}", data.size());
  try {
    auto v = runtime->evaluateJavaScript(
        std::make_unique<jsi::StringBuffer>(std::string(
            reinterpret_cast<const char*>(data.data()), data.size())),
        "index.android.bundle");
    (void)v;
    emit("{\"phase\":\"hbc-eval-ok\"}");
  } catch (const std::exception& e) {
    emitf("{\"phase\":\"hbc-eval-error\",\"error\":\"%s\"}", e.what());
  }
  return 0;
}

} // namespace

extern "C" JNIEXPORT jint JNICALL
Java_dev_acos_fixtures_rnhello_HermesProbe_acosRnHermesProbe(
    JNIEnv* env,
    jclass /*clazz*/,
    jstring bundlePath) {
  return run_probe(js_bytes(env, bundlePath));
}
