
/* SA spike: fake EGL + GLES used ONLY to answer "can a null EGL/GL satisfy
 * SurfaceFlinger's RenderEngine init?". No rendering is implemented; every
 * drawing entry point is a no-op. Built with the NDK clang for android-34 x86_64. */
#include <stddef.h>
#include <stdint.h>
#include <string.h>

typedef int32_t GLint; typedef uint32_t GLenum, GLuint, GLbitfield, GLboolean;
typedef float GLfloat; typedef intptr_t GLsizeiptr, GLintptr;
typedef void* EGLDisplay; typedef void* EGLContext; typedef void* EGLSurface;
typedef void* EGLConfig; typedef void* EGLImageKHR; typedef void* EGLSyncKHR;
typedef void* EGLNativeDisplayType; typedef void* EGLClientBuffer;
typedef int32_t EGLint; typedef uint32_t EGLBoolean, EGLenum;

static char s_version[]   = "OpenGL ES 3.2 ACOS-fake";
static char s_slversion[] = "OpenGL ES GLSL ES 3.20";
static char s_vendor[]    = "ACOS";
static char s_renderer[]  = "ACOS null GL";
static char s_ext[]       = "GL_OES_EGL_image GL_OES_EGL_image_external GL_EXT_buffer_storage";

/* ---- EGL: just enough for RenderEngine's pbuffer + context ---- */
EGLDisplay eglGetDisplay(EGLNativeDisplayType d) { (void)d; return (EGLDisplay)1; }
EGLDisplay eglGetCurrentDisplay(void) { return (EGLDisplay)1; }
EGLBoolean eglInitialize(EGLDisplay d, EGLint* maj, EGLint* min) { (void)d; if (maj) *maj = 1; if (min) *min = 5; return 1; }
EGLBoolean eglTerminate(EGLDisplay d) { (void)d; return 1; }
EGLBoolean eglReleaseThread(void) { return 1; }
const char* eglQueryString(EGLDisplay d, EGLint name) {
    (void)d; (void)name;
    return "EGL_KHR_image EGL_KHR_image_base EGL_KHR_fence_sync EGL_KHR_wait_sync "
           "EGL_ANDROID_native_fence_sync EGL_KHR_gl_colorspace EGL_KHR_no_config_context "
           "EGL_ANDROID_image_native_buffer EGL_EXT_image_dma_buf_import";
}
EGLBoolean eglGetConfigs(EGLDisplay d, EGLConfig* cfgs, EGLint sz, EGLint* n) {
    (void)d; (void)sz; if (cfgs && sz > 0) cfgs[0] = (EGLConfig)1; if (n) *n = 1; return 1;
}
EGLBoolean eglChooseConfig(EGLDisplay d, const EGLint* a, EGLConfig* cfgs, EGLint sz, EGLint* n) {
    (void)d; (void)a; (void)sz; if (cfgs && sz > 0) cfgs[0] = (EGLConfig)1; if (n) *n = 1; return 1;
}
EGLBoolean eglGetConfigAttrib(EGLDisplay d, EGLConfig c, EGLint attr, EGLint* v) {
    (void)d; (void)c; (void)attr; if (v) *v = 8; return 1;
}
EGLSurface eglCreatePbufferSurface(EGLDisplay d, EGLConfig c, const EGLint* a) { (void)d;(void)c;(void)a; return (EGLSurface)2; }
EGLSurface eglCreateWindowSurface(EGLDisplay d, EGLConfig c, void* w, const EGLint* a) { (void)d;(void)c;(void)w;(void)a; return (EGLSurface)2; }
EGLContext eglCreateContext(EGLDisplay d, EGLConfig c, EGLContext s, const EGLint* a) { (void)d;(void)c;(void)s;(void)a; return (EGLContext)3; }
EGLBoolean eglDestroyContext(EGLDisplay d, EGLContext c) { (void)d;(void)c; return 1; }
EGLBoolean eglDestroySurface(EGLDisplay d, EGLSurface s) { (void)d;(void)s; return 1; }
EGLBoolean eglMakeCurrent(EGLDisplay d, EGLSurface s, EGLSurface r, EGLContext c) { (void)d;(void)s;(void)r;(void)c; return 1; }
EGLContext eglGetCurrentContext(void) { return (EGLContext)3; }
EGLint eglGetError(void) { return 0x3000; }
EGLBoolean eglBindAPI(EGLenum a) { (void)a; return 1; }
EGLBoolean eglSwapInterval(EGLDisplay d, EGLint i) { (void)d;(void)i; return 1; }
EGLBoolean eglQueryContext(EGLDisplay d, EGLContext c, EGLint a, EGLint* v) { (void)d;(void)c;(void)a; if (v) *v = 0; return 1; }
EGLImageKHR eglCreateImageKHR(EGLDisplay d, EGLContext c, EGLenum t, EGLClientBuffer b, const EGLint* a) { (void)d;(void)c;(void)t;(void)b;(void)a; return (EGLImageKHR)4; }
EGLBoolean eglDestroyImageKHR(EGLDisplay d, EGLImageKHR i) { (void)d;(void)i; return 1; }
EGLSyncKHR eglCreateSyncKHR(EGLDisplay d, EGLenum t, const EGLint* a) { (void)d;(void)t;(void)a; return (EGLSyncKHR)5; }
EGLBoolean eglDestroySyncKHR(EGLDisplay d, EGLSyncKHR s) { (void)d;(void)s; return 1; }
EGLint eglClientWaitSyncKHR(EGLDisplay d, EGLSyncKHR s, EGLint f, uint64_t t) { (void)d;(void)s;(void)f;(void)t; return 0x30F7; /* CONDITION_SATISFIED */ }
EGLBoolean eglWaitSyncKHR(EGLDisplay d, EGLSyncKHR s, EGLint f) { (void)d;(void)s;(void)f; return 1; }
EGLint eglDupNativeFenceFDANDROID(EGLDisplay d, EGLSyncKHR s) { (void)d;(void)s; return -1; }


/* generic EGL entry points not implemented above */
static void* egl_noop(void) { return 0; }
void* eglGetProcAddress(const char* name) { (void)name; return (void*)egl_noop; }
EGLClientBuffer eglGetNativeClientBufferANDROID(const void* buffer) { return (EGLClientBuffer)buffer; }

/* ---- GL: state queries Skia actually reads; everything else is a no-op ---- */
const unsigned char* glGetString(GLenum name) {
    switch (name) {
        case 0x1F02: return (const unsigned char*)s_version;      /* GL_VERSION */
        case 0x8B8C: return (const unsigned char*)s_slversion;    /* GL_SHADING_LANGUAGE_VERSION */
        case 0x1F00: return (const unsigned char*)s_vendor;       /* GL_VENDOR */
        case 0x1F01: return (const unsigned char*)s_renderer;     /* GL_RENDERER */
        case 0x1F03: return (const unsigned char*)s_ext;          /* GL_EXTENSIONS */
        default: return (const unsigned char*)"";
    }
}
const unsigned char* glGetStringi(GLenum name, GLuint i) { (void)name; (void)i; return (const unsigned char*)""; }
void glGetIntegerv(GLenum p, GLint* v) {
    if (!v) return;
    switch (p) {
        case 0x0D33: *v = 16384; break;   /* MAX_TEXTURE_SIZE */
        case 0x84E8: case 0x8073: *v = 16384; break; /* MAX_RENDERBUFFER_SIZE / 3D */
        case 0x851C: *v = 16384; break;   /* MAX_CUBE_MAP_TEXTURE_SIZE */
        case 0x0D3A: v[0] = 16384; v[1] = 16384; break; /* MAX_VIEWPORT_DIMS */
        case 0x8872: *v = 16; break;      /* MAX_TEXTURE_IMAGE_UNITS */
        case 0x8869: *v = 16; break;      /* MAX_VERTEX_ATTRIBS */
        case 0x8B4D: *v = 32; break;      /* MAX_COMBINED_TEXTURE_IMAGE_UNITS */
        case 0x8DFB: *v = 256; break;     /* MAX_VERTEX_UNIFORM_VECTORS */
        case 0x8DFD: *v = 224; break;     /* MAX_FRAGMENT_UNIFORM_VECTORS */
        case 0x8DFC: *v = 15; break;      /* MAX_VARYING_VECTORS */
        case 0x8D57: *v = 4; break;       /* MAX_SAMPLES */
        case 0x88FF: *v = 16; break;      /* MAX_ARRAY_TEXTURE_LAYERS */
        case 0x8CDF: *v = 8; break;       /* MAX_COLOR_ATTACHMENTS */
        case 0x8824: *v = 8; break;       /* MAX_DRAW_BUFFERS */
        case 0x821D: *v = 0; break;       /* NUM_EXTENSIONS */
        case 0x8CA6: *v = 0; break;       /* FRAMEBUFFER_BINDING */
        case 0x8B4C: *v = 0; break;
        case 0x86A3: *v = 64; break;      /* MAX_ELEMENT_INDEX-ish */
        default: *v = 0; break;
    }
}
void glGetFloatv(GLenum p, GLfloat* v) { (void)p; if (v) *v = 1.0f; }
void glGetBooleanv(GLenum p, GLboolean* v) { (void)p; if (v) *v = 1; }
GLenum glGetError(void) { return 0; }
void glGetShaderPrecisionFormat(GLenum a, GLenum b, GLint* range, GLint* precision) {
    (void)a; (void)b; if (range) { range[0] = 127; range[1] = 127; } if (precision) *precision = 23;
}
void glGetInternalformativ(GLenum t, GLenum f, GLenum p, int n, GLint* v) { (void)t;(void)f;(void)p; for (int i=0;i<n;i++) v[i]=0; }
void glGetTexLevelParameteriv(GLenum t, GLint l, GLenum p, GLint* v) { (void)t;(void)l;(void)p; if (v) *v = 1; }
void glGetBufferParameteriv(GLenum t, GLenum p, GLint* v) { (void)t;(void)p; if (v) *v = 1; }
static GLuint s_next = 100;
GLuint glCreateShader(GLenum t) { (void)t; return ++s_next; }
GLuint glCreateProgram(void) { return ++s_next; }
void glGenBuffers(int n, GLuint* v) { for (int i=0;i<n;i++) v[i]=++s_next; }
void glGenTextures(int n, GLuint* v) { for (int i=0;i<n;i++) v[i]=++s_next; }
void glGenFramebuffers(int n, GLuint* v) { for (int i=0;i<n;i++) v[i]=++s_next; }
void glGenRenderbuffers(int n, GLuint* v) { for (int i=0;i<n;i++) v[i]=++s_next; }
void glGenVertexArrays(int n, GLuint* v) { for (int i=0;i<n;i++) v[i]=++s_next; }
void glGetShaderiv(GLuint s, GLenum p, GLint* v) { (void)s;(void)p; if (v) *v = 1; }
void glGetProgramiv(GLuint s, GLenum p, GLint* v) { (void)s;(void)p; if (v) *v = 1; }
void glGetShaderInfoLog(GLuint s, int b, int* l, char* log) { (void)s;(void)b; if (l) *l = 0; if (log && b) log[0]=0; }
void glGetProgramInfoLog(GLuint s, int b, int* l, char* log) { (void)s;(void)b; if (l) *l = 0; if (log && b) log[0]=0; }
GLenum glCheckFramebufferStatus(GLenum t) { (void)t; return 0x8CD5; }
GLboolean glIsShader(GLuint s) { (void)s; return 1; }
GLboolean glIsProgram(GLuint s) { (void)s; return 1; }
GLboolean glIsTexture(GLuint s) { (void)s; return 1; }
GLboolean glIsFramebuffer(GLuint s) { (void)s; return 1; }
GLint glGetAttribLocation(GLuint p, const char* n) { (void)p;(void)n; return 0; }
GLint glGetUniformLocation(GLuint p, const char* n) { (void)p;(void)n; return 0; }
void* glMapBufferRange(GLenum t, GLintptr o, GLsizeiptr l, GLbitfield a) { (void)t;(void)o;(void)l;(void)a; return 0; }
void glShaderSource(GLuint s, int c, const char* const* str, const GLint* len) { (void)s;(void)c;(void)str;(void)len; }
void glBindAttribLocation(GLuint p, GLuint i, const char* n) { (void)p;(void)i;(void)n; }
void glGetActiveUniform(GLuint p, GLuint i, int b, int* len, int* size, GLenum* type, char* name) {
    (void)p;(void)i; if (len) *len=0; if (size) *size=1; if (type) *type=0x1406; if (name && b) name[0]=0;
}
void glActiveTexture(void) { }
void glAttachShader(void) { }
void glBindBuffer(void) { }
void glBindFramebuffer(void) { }
void glBindRenderbuffer(void) { }
void glBindTexture(void) { }
void glBlendColor(void) { }
void glBlendEquation(void) { }
void glBlendFunc(void) { }
void glBlendFuncSeparate(void) { }
void glBlitFramebuffer(void) { }
void glBufferData(void) { }
void glBufferSubData(void) { }
void glClear(void) { }
void glClearColor(void) { }
void glClearStencil(void) { }
void glColorMask(void) { }
void glCompileShader(void) { }
void glCompressedTexImage2D(void) { }
void glCompressedTexSubImage2D(void) { }
void glCopyTexSubImage2D(void) { }
void glCullFace(void) { }
void glDeleteBuffers(void) { }
void glDeleteFramebuffers(void) { }
void glDeleteProgram(void) { }
void glDeleteRenderbuffers(void) { }
void glDeleteShader(void) { }
void glDeleteTextures(void) { }
void glDepthMask(void) { }
void glDetachShader(void) { }
void glDisable(void) { }
void glDisableVertexAttribArray(void) { }
void glDrawArrays(void) { }
void glDrawElements(void) { }
void glEGLImageTargetTexture2DOES(void) { }
void glEnable(void) { }
void glEnableVertexAttribArray(void) { }
void glFinish(void) { }
void glFlush(void) { }
void glFramebufferRenderbuffer(void) { }
void glFramebufferTexture2D(void) { }
void glFrontFace(void) { }
void glGenerateMipmap(void) { }
void glGetFramebufferAttachmentParameteriv(void) { }
void glGetRenderbufferParameteriv(void) { }
void glLineWidth(void) { }
void glLinkProgram(void) { }
void glPixelStorei(void) { }
void glReadPixels(void) { }
void glRenderbufferStorage(void) { }
void glScissor(void) { }
void glStencilFunc(void) { }
void glStencilFuncSeparate(void) { }
void glStencilMask(void) { }
void glStencilMaskSeparate(void) { }
void glStencilOp(void) { }
void glStencilOpSeparate(void) { }
void glTexImage2D(void) { }
void glTexParameterf(void) { }
void glTexParameterfv(void) { }
void glTexParameteri(void) { }
void glTexParameteriv(void) { }
void glTexSubImage2D(void) { }
void glUniform1f(void) { }
void glUniform1fv(void) { }
void glUniform1i(void) { }
void glUniform1iv(void) { }
void glUniform2f(void) { }
void glUniform2fv(void) { }
void glUniform2i(void) { }
void glUniform2iv(void) { }
void glUniform3f(void) { }
void glUniform3fv(void) { }
void glUniform3i(void) { }
void glUniform3iv(void) { }
void glUniform4f(void) { }
void glUniform4fv(void) { }
void glUniform4i(void) { }
void glUniform4iv(void) { }
void glUniformMatrix2fv(void) { }
void glUniformMatrix3fv(void) { }
void glUniformMatrix4fv(void) { }
void glUseProgram(void) { }
void glVertexAttrib1f(void) { }
void glVertexAttrib2fv(void) { }
void glVertexAttrib3fv(void) { }
void glVertexAttrib4fv(void) { }
void glVertexAttribPointer(void) { }
void glViewport(void) { }
