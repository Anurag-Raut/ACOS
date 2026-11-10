//! `java.lang.Math` / `java.lang.StrictMath` / bit-conversion natives.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

macro_rules! unary {
    ($name:ident, $op:path) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let a = vm.arg_f64(ctx, 0);
            vm.ret_f64(ctx, $op(a));
            Ok(())
        }
    };
}

unary!(m_sin, f64::sin);
unary!(m_cos, f64::cos);
unary!(m_tan, f64::tan);
unary!(m_asin, f64::asin);
unary!(m_acos, f64::acos);
unary!(m_atan, f64::atan);
unary!(m_exp, f64::exp);
unary!(m_log, f64::ln);
unary!(m_log10, f64::log10);
unary!(m_sqrt, f64::sqrt);
unary!(m_cbrt, f64::cbrt);
unary!(m_sinh, f64::sinh);
unary!(m_cosh, f64::cosh);
unary!(m_tanh, f64::tanh);
unary!(m_expm1, f64::exp_m1);
unary!(m_log1p, f64::ln_1p);
unary!(m_ceil, f64::ceil);
unary!(m_floor, f64::floor);
unary!(m_rint, f64::round);

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    for c in ["Ljava/lang/Math;", "Ljava/lang/StrictMath;"] {
        vm.register_native(c, "sin", "(D)D", m_sin);
        vm.register_native(c, "cos", "(D)D", m_cos);
        vm.register_native(c, "tan", "(D)D", m_tan);
        vm.register_native(c, "asin", "(D)D", m_asin);
        vm.register_native(c, "acos", "(D)D", m_acos);
        vm.register_native(c, "atan", "(D)D", m_atan);
        vm.register_native(c, "exp", "(D)D", m_exp);
        vm.register_native(c, "log", "(D)D", m_log);
        vm.register_native(c, "log10", "(D)D", m_log10);
        vm.register_native(c, "sqrt", "(D)D", m_sqrt);
        vm.register_native(c, "cbrt", "(D)D", m_cbrt);
        vm.register_native(c, "sinh", "(D)D", m_sinh);
        vm.register_native(c, "cosh", "(D)D", m_cosh);
        vm.register_native(c, "tanh", "(D)D", m_tanh);
        vm.register_native(c, "expm1", "(D)D", m_expm1);
        vm.register_native(c, "log1p", "(D)D", m_log1p);
        vm.register_native(c, "ceil", "(D)D", m_ceil);
        vm.register_native(c, "floor", "(D)D", m_floor);
        vm.register_native(c, "rint", "(D)D", m_rint);
        vm.register_native(c, "pow", "(DD)D", pow);
        vm.register_native(c, "atan2", "(DD)D", atan2);
        vm.register_native(c, "hypot", "(DD)D", hypot);
        vm.register_native(c, "IEEEremainder", "(DD)D", ieee_remainder);
    }
    vm.register_native("Ljava/lang/Double;", "doubleToRawLongBits", "(D)J", d2l);
    vm.register_native("Ljava/lang/Double;", "longBitsToDouble", "(J)D", l2d);
    vm.register_native("Ljava/lang/Float;", "floatToRawIntBits", "(F)I", f2i);
    vm.register_native("Ljava/lang/Float;", "intBitsToFloat", "(I)F", i2f);
    Ok(())
}

fn pow(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f64(ctx, 0);
    let b = vm.arg_f64(ctx, 1);
    vm.ret_f64(ctx, a.powf(b));
    Ok(())
}

fn atan2(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f64(ctx, 0);
    let b = vm.arg_f64(ctx, 1);
    vm.ret_f64(ctx, a.atan2(b));
    Ok(())
}

fn hypot(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f64(ctx, 0);
    let b = vm.arg_f64(ctx, 1);
    vm.ret_f64(ctx, a.hypot(b));
    Ok(())
}

fn ieee_remainder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_f64(ctx, 0);
    let b = vm.arg_f64(ctx, 1);
    vm.ret_f64(ctx, ieee754_remainder(a, b));
    Ok(())
}

fn ieee754_remainder(x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() || x.is_infinite() || y == 0.0 {
        return f64::NAN;
    }
    if y.is_infinite() {
        return x;
    }
    x - (x / y).round_ties_even() * y
}

fn d2l(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let d = vm.arg_f64(ctx, 0);
    vm.ret_i64(ctx, d.to_bits() as i64);
    Ok(())
}

fn l2d(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let l = vm.arg_i64(ctx, 0);
    vm.ret_f64(ctx, f64::from_bits(l as u64));
    Ok(())
}

fn f2i(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let f = vm.arg_f32(ctx, 0);
    vm.ret_i32(ctx, f.to_bits() as i32);
    Ok(())
}

fn i2f(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let i = vm.arg_i32(ctx, 0);
    vm.ret_f32(ctx, f32::from_bits(i as u32));
    Ok(())
}
