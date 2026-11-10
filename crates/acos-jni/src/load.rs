//! Library loading and JNI symbol resolution (`System.loadLibrary`,
//! `Runtime.nativeLoad`, `JNI_OnLoad`, `Java_*` name mangling).

use std::io::{Cursor, Read};
use std::path::Path;

use acos_vm::natives::jni_glue::CallFailure;
use acos_vm::Vm;

use crate::error::JniError;
use crate::refs;
use crate::state::{
    current_vm, runtime_state, LoadedLibrary, State, JNI_OK, JNI_VERSION_1_4, JNI_VERSION_1_6,
};

fn structured(e: JniError) -> CallFailure {
    CallFailure::Structured {
        code: e.code().to_string(),
        message: e.to_string(),
    }
}

fn basename(requested: &str) -> String {
    Path::new(requested)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(requested)
        .to_string()
}

fn library_file_name(name: &str) -> String {
    let base = basename(name);
    if base.ends_with(".so") {
        base
    } else if base.starts_with("lib") {
        format!("{base}.so")
    } else {
        format!("lib{base}.so")
    }
}

fn read_apk_library(apk: &Path, abi: &str, file: &str) -> Option<Vec<u8>> {
    let bytes = std::fs::read(apk).ok()?;
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    let entry = format!("lib/{abi}/{file}");
    let mut f = archive.by_name(&entry).ok()?;
    let mut out = Vec::new();
    f.read_to_end(&mut out).ok()?;
    Some(out)
}

/// Load a guest library by the name a Java caller used.
///
/// Accepts an absolute path, a bare `libfoo.so`, or the `System.loadLibrary`
/// spelling `foo`. Search order: the literal path (if absolute), every
/// configured search directory, then the APK's `lib/<abi>/`.
pub(crate) fn load_named(vm: &mut Vm, requested: &str) -> Result<String, CallFailure> {
    let (search_paths, apk, abi, loader) = {
        let rt = runtime_state().ok_or_else(|| {
            structured(JniError::NoVm {
                function: "loadLibrary".into(),
            })
        })?;
        let st = rt.borrow();
        (
            st.config.search_paths.clone(),
            st.config.apk.clone(),
            st.config.abi.clone(),
            st.loader.clone(),
        )
    };

    let file = library_file_name(requested);
    // A load of the same file under another spelling reuses the object.
    {
        let rt = runtime_state().unwrap();
        let st = rt.borrow();
        if let Some(path) = st.loaded.get(&file) {
            return Ok(path.clone());
        }
        if let Some(path) = st.loaded.get(requested) {
            return Ok(path.clone());
        }
    }

    let mut chosen: Option<(Vec<u8>, Option<String>, String)> = None;
    let direct = Path::new(requested);
    if direct.is_absolute() && direct.is_file() {
        let bytes = std::fs::read(direct).map_err(|e| {
            structured(JniError::LoadFailed {
                message: e.to_string(),
            })
        })?;
        let label = direct
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&file)
            .to_string();
        chosen = Some((bytes, Some(direct.display().to_string()), label));
    }
    if chosen.is_none() {
        for dir in &search_paths {
            let candidate = dir.join(&file);
            if candidate.is_file() {
                let bytes = std::fs::read(&candidate).map_err(|e| {
                    structured(JniError::LoadFailed {
                        message: e.to_string(),
                    })
                })?;
                chosen = Some((bytes, Some(candidate.display().to_string()), file.clone()));
                break;
            }
        }
    }
    if chosen.is_none() {
        if let Some(apk) = &apk {
            if let Some(bytes) = read_apk_library(apk, &abi, &file) {
                let label = format!("{}!/lib/{abi}/{file}", apk.display());
                chosen = Some((bytes, Some(label), file.clone()));
            }
        }
    }

    let Some((bytes, path, name)) = chosen else {
        return Err(structured(JniError::LoadFailed {
            message: format!(
                "library {requested:?} not found (searched {:?}{})",
                search_paths,
                apk.as_ref()
                    .map(|a| format!(" and {}", a.display()))
                    .unwrap_or_default()
            ),
        }));
    };

    // Answer the guest library's NDK imports (`libandroid`/`libjnigraphics`)
    // from the real ACOS substrates before its initializer runs. The host is
    // process-wide and inert until native code actually calls an NDK symbol.
    crate::ndk_host::install();

    let handle = loader.load_bytes(&bytes, &name).map_err(|e| {
        structured(JniError::LoadFailed {
            message: e.to_string(),
        })
    })?;

    // Call the library's initializer before publishing it.
    let on_load = handle.symbol("JNI_OnLoad");
    if let Some(p) = on_load {
        if current_vm().is_null() {
            return Err(structured(JniError::LoadFailed {
                message: "JNI_OnLoad: runtime is not attached to this VM".into(),
            }));
        }
        let rt = runtime_state().unwrap();
        let t = vm.current_thread;
        refs::push_frame(&rt, t, 0);
        let java_vm = rt.borrow().vm_slot as *mut std::os::raw::c_void;
        // SAFETY: `JNI_OnLoad` has the JNI-mandated signature
        // `jint (*)(JavaVM*, void*)`; the pointer came from a real export of a
        // library the loader just mapped.
        let f: unsafe extern "C" fn(*mut std::os::raw::c_void, *mut std::os::raw::c_void) -> i32 =
            unsafe { std::mem::transmute(p) };
        let rc = unsafe { f(java_vm, std::ptr::null_mut()) };
        let error = refs::take_error(&rt, t);
        let pending = refs::take_pending(vm, &rt, t);
        let _ = refs::pop_frame(vm, &rt, t, acos_vm::heap::NULL);
        if let Some(e) = error {
            return Err(structured(e));
        }
        if let Some(h) = pending {
            return Err(CallFailure::JavaException(h));
        }
        if rc != JNI_VERSION_1_4 && rc != JNI_VERSION_1_6 && rc != JNI_OK {
            return Err(structured(JniError::LoadFailed {
                message: format!("JNI_OnLoad of {name} returned {rc:#x} (JNI_ERR)"),
            }));
        }
    }

    let rt = runtime_state().unwrap();
    {
        let mut st = rt.borrow_mut();
        st.loaded
            .insert(file.clone(), path.clone().unwrap_or_else(|| name.clone()));
        st.loaded.insert(
            requested.to_string(),
            path.clone().unwrap_or_else(|| name.clone()),
        );
        st.libs.push(LoadedLibrary {
            name: name.clone(),
            path: path.clone(),
            handle: handle.clone(),
        });
    }
    Ok(path.unwrap_or(name))
}

// --- JNI name mangling ------------------------------------------------------

fn mangle(part: &str) -> String {
    let mut out = String::with_capacity(part.len() + 8);
    for c in part.chars() {
        match c {
            '/' => out.push('_'),
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => out.push_str(&format!("_0{:04x}", c as u32)),
        }
    }
    out
}

pub(crate) fn symbol_names(class: &str, name: &str, sig: &str) -> Vec<String> {
    let class_part = class
        .strip_prefix('L')
        .and_then(|s| s.strip_suffix(';'))
        .unwrap_or(class);
    let base = format!("Java_{}_{}", mangle(class_part), mangle(name));
    let mut out = vec![base.clone()];
    // Long form: argument types only, escaped. Also try the full signature as
    // a lenient fallback for tools that include the return type.
    if let Ok((params, _)) = acos_vm::class::parse_signature(sig) {
        out.push(format!("{base}__{}", mangle(&params.concat())));
    }
    out.push(format!("{base}__{}", mangle(sig)));
    out
}

/// Resolve a DEX native method to a guest function pointer.
pub(crate) fn resolve_native(st: &State, class: &str, name: &str, sig: &str) -> Option<usize> {
    if let Some(p) = st
        .natives
        .get(&(class.to_string(), name.to_string(), sig.to_string()))
    {
        return Some(*p);
    }
    if let Some(p) = st
        .resolved
        .get(&(class.to_string(), name.to_string(), sig.to_string()))
    {
        return Some(*p);
    }
    let mut names = symbol_names(class, name, sig);
    let overloaded = st
        .overloads
        .get(&(class.to_string(), name.to_string()))
        .copied()
        .unwrap_or(0)
        > 1;
    if overloaded && names.len() > 1 {
        // The short `Java_pkg_Class_method` form is ambiguous for overloads;
        // try the long (mangled argument) names first.
        let simple = names.remove(0);
        names.push(simple);
    }
    for lib in st.libs.iter().rev() {
        for sym in &names {
            if let Some(p) = lib.handle.symbol(sym) {
                return Some(p as usize);
            }
        }
    }
    None
}

pub(crate) fn cache_resolved(st: &mut State, class: &str, name: &str, sig: &str, p: usize) {
    st.resolved
        .insert((class.to_string(), name.to_string(), sig.to_string()), p);
}

/// Host callback installed in `acos_vm::natives::jni_glue`.
pub(crate) fn host_load_library(vm: *mut Vm, name: &str) -> Result<String, CallFailure> {
    if vm.is_null() {
        return Err(structured(JniError::NoVm {
            function: "loadLibrary".into(),
        }));
    }
    // SAFETY: the interpreter passes the live VM for this native call. Bind it
    // so `JNI_OnLoad` and any JNI calls it makes see the environment.
    unsafe { crate::state::set_vm(vm) };
    let vm = unsafe { &mut *vm };
    load_named(vm, name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_and_long_jni_names_are_mangled() {
        let names = symbol_names("Ldev/acos/fixtures/hellojni/MainActivity;", "add", "(II)I");
        assert_eq!(names[0], "Java_dev_acos_fixtures_hellojni_MainActivity_add");
        assert!(
            names.iter().any(|n| n.ends_with("__II")),
            "long overload form present: {names:?}"
        );
    }

    #[test]
    fn underscores_and_arrays_are_escaped() {
        let names = symbol_names("Lpkg/My_Class;", "f", "([I)V");
        let short = &names[0];
        assert!(short.contains("My_1Class"), "{short}");
        assert!(names.iter().any(|n| n.contains("_3I")), "{names:?}");
    }
}
