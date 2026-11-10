/*
 * ACOS software GLES2 fixture (W86-A): clear + draw a triangle through EGL.
 *
 * Compiled with the NDK and loaded through `acos-native-loader` exactly like a
 * third-party Flutter/React Native `.so`: every `egl*`/`gl*` call below is an
 * import from `libEGL.so`/`libGLESv2.so`, which the loader binds to
 * `acos-gles` when the native path installed its entry-point table.
 *
 * The `ANativeWindow` is passed in by the Rust driver (the loader/registry
 * created it), because the fixture is driven from a Rust test rather than from
 * Java; on a device the same code fills that argument with
 * `ANativeWindow_fromSurface`.
 *
 * Returns 0 when every check passed, otherwise a bitmask of the failing steps.
 */
#include <EGL/egl.h>
#include <GLES2/gl2.h>
#include <stdint.h>
#include <string.h>

#define FAIL(bit) do { failures |= (1u << (bit)); } while (0)

#ifndef ACOS_GLES_FIXTURE_WIDTH
#define ACOS_GLES_FIXTURE_WIDTH 32
#endif
#ifndef ACOS_GLES_FIXTURE_HEIGHT
#define ACOS_GLES_FIXTURE_HEIGHT 32
#endif

static const char *VERTEX_SRC =
    "attribute vec2 a_pos;\n"
    "void main() {\n"
    "    gl_Position = vec4(a_pos, 0.0, 1.0);\n"
    "}\n";

static const char *FRAGMENT_SRC =
    "precision mediump float;\n"
    "uniform vec4 u_color;\n"
    "void main() {\n"
    "    gl_FragColor = u_color;\n"
    "}\n";

static GLuint compile(GLenum kind, const char *source, int *failures, unsigned bit) {
    GLuint shader = glCreateShader(kind);
    if (shader == 0) {
        *failures |= (int)(1u << bit);
        return 0;
    }
    glShaderSource(shader, 1, &source, NULL);
    glCompileShader(shader);
    GLint ok = 0;
    glGetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[256];
        GLsizei len = 0;
        glGetShaderInfoLog(shader, (GLsizei)sizeof(log), &len, log);
        (void)len;
        *failures |= (int)(1u << (bit + 1));
        return 0;
    }
    return shader;
}

int acos_gles_test(void *native_window) {
    int failures = 0;
    if (native_window == NULL) return (int)(1u << 0);

    EGLDisplay display = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    if (display == EGL_NO_DISPLAY) return (int)(1u << 1);

    EGLint major = 0, minor = 0;
    if (!eglInitialize(display, &major, &minor)) return (int)(1u << 2);

    static const EGLint cfg_attribs[] = {
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_RED_SIZE, 8, EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_ALPHA_SIZE, 8,
        EGL_DEPTH_SIZE, 16,
        EGL_NONE,
    };
    EGLConfig config = NULL;
    EGLint num_configs = 0;
    if (!eglChooseConfig(display, cfg_attribs, &config, 1, &num_configs) || num_configs < 1) {
        FAIL(3);
    }

    EGLint surface_width = 0;
    EGLSurface surface = eglCreateWindowSurface(display, config,
                                                (EGLNativeWindowType)native_window, NULL);
    if (surface == EGL_NO_SURFACE) FAIL(4);
    if (!eglQuerySurface(display, surface, EGL_WIDTH, &surface_width)) FAIL(5);

    static const EGLint ctx_attribs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, ctx_attribs);
    if (context == EGL_NO_CONTEXT) FAIL(6);
    if (!eglMakeCurrent(display, surface, surface, context)) FAIL(7);

    GLuint vs = compile(GL_VERTEX_SHADER, VERTEX_SRC, &failures, 8);
    GLuint fs = compile(GL_FRAGMENT_SHADER, FRAGMENT_SRC, &failures, 10);
    GLuint program = glCreateProgram();
    glAttachShader(program, vs);
    glAttachShader(program, fs);
    glLinkProgram(program);
    GLint linked = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) FAIL(12);
    glUseProgram(program);

    GLint color = glGetUniformLocation(program, "u_color");
    if (color < 0) FAIL(13);
    glUniform4f(color, 1.0f, 0.0f, 0.0f, 1.0f); /* red triangle */

    static const GLfloat vertices[] = {
        -1.0f, -1.0f,
         1.0f, -1.0f,
        -1.0f,  1.0f,
    };
    GLuint buffer = 0;
    glGenBuffers(1, &buffer);
    glBindBuffer(GL_ARRAY_BUFFER, buffer);
    glBufferData(GL_ARRAY_BUFFER, (GLsizeiptr)sizeof(vertices), vertices, GL_STATIC_DRAW);
    GLint pos = glGetAttribLocation(program, "a_pos");
    if (pos < 0) FAIL(14);
    glEnableVertexAttribArray((GLuint)pos);
    glVertexAttribPointer((GLuint)pos, 2, GL_FLOAT, GL_FALSE, 0, 0);

    glViewport(0, 0, ACOS_GLES_FIXTURE_WIDTH, ACOS_GLES_FIXTURE_HEIGHT);
    glClearColor(0.0f, 0.0f, 1.0f, 1.0f); /* blue background */
    glClear(GL_COLOR_BUFFER_BIT);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    if (glGetError() != GL_NO_ERROR) FAIL(15);

    /* The triangle covers the lower-left half: read back both corners. */
    unsigned char pixel[4];
    memset(pixel, 0, sizeof(pixel));
    glReadPixels(1, 1, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
    if (!(pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40 && pixel[3] > 200)) FAIL(16);
    memset(pixel, 0, sizeof(pixel));
    glReadPixels(ACOS_GLES_FIXTURE_WIDTH - 2, ACOS_GLES_FIXTURE_HEIGHT - 2, 1, 1,
                 GL_RGBA, GL_UNSIGNED_BYTE, pixel);
    if (!(pixel[0] < 40 && pixel[2] > 200)) FAIL(17);

    if (!eglSwapBuffers(display, surface)) FAIL(18);

    glDeleteBuffers(1, &buffer);
    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(display, context);
    eglDestroySurface(display, surface);
    eglTerminate(display);
    (void)surface_width;
    return (int)failures;
}
