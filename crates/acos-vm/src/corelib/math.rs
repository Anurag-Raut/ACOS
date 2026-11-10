//! `java.lang.Math` builtins.

use acos_dex::access;

use crate::class::NativeCtx;
use crate::corelib::Vm;
use crate::error::VmResult;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/Math;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    let s = access::PUBLIC | access::STATIC;
    vm.add_native(id, "abs", "(I)I", s, abs_int);
    vm.add_native(id, "abs", "(J)J", s, abs_long);
    vm.add_native(id, "abs", "(D)D", s, abs_double);
    vm.add_native(id, "abs", "(F)F", s, abs_float);
    vm.add_native(id, "max", "(II)I", s, max_int);
    vm.add_native(id, "max", "(JJ)J", s, max_long);
    vm.add_native(id, "max", "(DD)D", s, max_double);
    vm.add_native(id, "max", "(FF)F", s, max_float);
    vm.add_native(id, "min", "(II)I", s, min_int);
    vm.add_native(id, "min", "(JJ)J", s, min_long);
    vm.add_native(id, "min", "(DD)D", s, min_double);
    vm.add_native(id, "min", "(FF)F", s, min_float);
    vm.add_native(id, "sqrt", "(D)D", s, sqrt);
    vm.add_native(id, "cbrt", "(D)D", s, cbrt);
    vm.add_native(id, "exp", "(D)D", s, exp);
    vm.add_native(id, "log", "(D)D", s, log);
    vm.add_native(id, "floor", "(D)D", s, floor);
    vm.add_native(id, "ceil", "(D)D", s, ceil);
    vm.add_native(id, "round", "(D)J", s, round);
    vm.add_native(id, "round", "(F)I", s, round_float);
    vm.add_native(id, "pow", "(DD)D", s, pow);
    vm.add_native(id, "hypot", "(DD)D", s, hypot);
    vm.add_native(id, "signum", "(D)D", s, signum);
    vm.add_native(id, "toDegrees", "(D)D", s, to_degrees);
    vm.add_native(id, "toRadians", "(D)D", s, to_radians);
    vm.link_class(id)?;
    Ok(())
}

fn abs_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    vm.ret_i32(ctx, v.wrapping_abs());
    Ok(())
}
fn abs_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    vm.ret_i64(ctx, v.wrapping_abs());
    Ok(())
}
fn abs_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).abs());
    Ok(())
}
fn abs_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f32(ctx, vm.arg_f32(ctx, 0).abs());
    Ok(())
}
fn max_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i32(ctx, 0);
    let b = vm.arg_i32(ctx, 1);
    vm.ret_i32(ctx, a.max(b));
    Ok(())
}
fn max_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i64(ctx, 0);
    let b = vm.arg_i64(ctx, 1);
    vm.ret_i64(ctx, a.max(b));
    Ok(())
}
fn max_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f64(ctx, 0);
    let b = vm.arg_f64(ctx, 1);
    // Java: a if a >= b, except max(-0.0, +0.0) is +0.0.
    let r = if a.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { b } else { a }
    } else if a >= b {
        a
    } else {
        b
    };
    vm.ret_f64(ctx, r);
    Ok(())
}
fn max_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f32(ctx, vm.arg_f32(ctx, 0).max(vm.arg_f32(ctx, 1)));
    Ok(())
}
fn min_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i32(ctx, 0);
    let b = vm.arg_i32(ctx, 1);
    vm.ret_i32(ctx, a.min(b));
    Ok(())
}
fn min_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i64(ctx, 0);
    let b = vm.arg_i64(ctx, 1);
    vm.ret_i64(ctx, a.min(b));
    Ok(())
}
fn min_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f64(ctx, 0);
    let b = vm.arg_f64(ctx, 1);
    // Java: a if a <= b, except min(+0.0, -0.0) is -0.0.
    let r = if a.is_nan() {
        a
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { a } else { b }
    } else if a <= b {
        a
    } else {
        b
    };
    vm.ret_f64(ctx, r);
    Ok(())
}
fn min_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f32(ctx, vm.arg_f32(ctx, 0).min(vm.arg_f32(ctx, 1)));
    Ok(())
}
fn sqrt(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).sqrt());
    Ok(())
}
fn cbrt(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).cbrt());
    Ok(())
}
fn exp(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).exp());
    Ok(())
}
fn log(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).ln());
    Ok(())
}
fn floor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).floor());
    Ok(())
}
fn ceil(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).ceil());
    Ok(())
}
fn round(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f64(ctx, 0);
    vm.ret_i64(ctx, java_round(v));
    Ok(())
}
fn java_round(v: f64) -> i64 {
    if v.is_nan() {
        0
    } else {
        (v + 0.5).floor() as i64
    }
}
fn round_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f32(ctx, 0);
    vm.ret_i32(ctx, java_round(v as f64) as i32);
    Ok(())
}
fn pow(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).powf(vm.arg_f64(ctx, 1)));
    Ok(())
}
fn hypot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).hypot(vm.arg_f64(ctx, 1)));
    Ok(())
}
fn signum(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f64(ctx, 0);
    let r = if v.is_nan() {
        f64::NAN
    } else if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        v
    };
    vm.ret_f64(ctx, r);
    Ok(())
}
fn to_degrees(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).to_degrees());
    Ok(())
}
fn to_radians(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_f64(ctx, vm.arg_f64(ctx, 0).to_radians());
    Ok(())
}
