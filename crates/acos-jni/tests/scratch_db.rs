use acos_jni::jni;
use acos_jni::{JniConfig, JniRuntime};
use acos_vm::{Vm, VmOptions};

#[test]
fn scratch() {
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 64 * 1024 * 1024,
        ..VmOptions::default()
    })
    .unwrap();
    let rt = JniRuntime::new(JniConfig::default());
    rt.attach(&mut vm).unwrap();
    rt.bind(&mut vm);
    let env = rt.env() as usize;
    eprintln!("step1");
    let buf = unsafe { jni::jni_NewDirectByteBuffer(env, 0, 16) };
    eprintln!("step2 buf={buf}");
    let addr = unsafe { jni::jni_GetDirectBufferAddress(env, buf) };
    eprintln!("step3 addr={addr}");
    let cap = unsafe { jni::jni_GetDirectBufferCapacity(env, buf) };
    eprintln!("step4 cap={cap}");
    let payload = b"acos-direct-buff";
    acos_vm::natives::jni_glue::native_mem_write(addr as i64, payload).unwrap();
    eprintln!("step5");
    let back = acos_vm::natives::jni_glue::native_mem_read(addr as i64, payload.len()).unwrap();
    eprintln!("step6 back={back:?}");
    assert!(!acos_vm::natives::jni_glue::native_mem_check(addr as i64, 17));
    let _ = acos_vm::natives::jni_glue::native_mem_free(addr as i64);
    eprintln!("step7");
    drop(rt);
    drop(vm);
    eprintln!("step8");
}
