//! `java.lang.Character` natives.
//!
//! API 34 routes the `Character` predicates through ICU (`CharacterData`), but
//! the M1 console tier only needs the Unicode properties Rust's `char` already
//! exposes. Where Rust has no direct equivalent (`getTypeImpl`,
//! `getDirectionalityImpl`) the result is an honest approximation documented in
//! `results/m1/README.md`.

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::vm::Vm;

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Ljava/lang/Character;";
    vm.register_native(c, "isDigitImpl", "(I)Z", is_digit);
    vm.register_native(c, "isLetterImpl", "(I)Z", is_letter);
    vm.register_native(c, "isLetterOrDigitImpl", "(I)Z", is_letter_or_digit);
    vm.register_native(c, "isWhitespaceImpl", "(I)Z", is_whitespace);
    vm.register_native(c, "isSpaceCharImpl", "(I)Z", is_space_char);
    vm.register_native(c, "isUpperCaseImpl", "(I)Z", is_upper);
    vm.register_native(c, "isLowerCaseImpl", "(I)Z", is_lower);
    vm.register_native(c, "isTitleCaseImpl", "(I)Z", is_title);
    vm.register_native(c, "isAlphabeticImpl", "(I)Z", is_letter);
    vm.register_native(c, "isDefinedImpl", "(I)Z", is_defined);
    vm.register_native(c, "isIdeographicImpl", "(I)Z", is_ideographic);
    vm.register_native(c, "isMirroredImpl", "(I)Z", ret_false);
    vm.register_native(c, "isIdentifierIgnorableImpl", "(I)Z", is_identifier_ignorable);
    vm.register_native(c, "isUnicodeIdentifierStartImpl", "(I)Z", is_letter);
    vm.register_native(c, "isUnicodeIdentifierPartImpl", "(I)Z", is_letter_or_digit);
    vm.register_native(c, "toUpperCaseImpl", "(I)I", to_upper);
    vm.register_native(c, "toLowerCaseImpl", "(I)I", to_lower);
    vm.register_native(c, "toTitleCaseImpl", "(I)I", to_upper);
    vm.register_native(c, "digitImpl", "(II)I", digit);
    vm.register_native(c, "getNumericValueImpl", "(I)I", numeric_value);
    vm.register_native(c, "getTypeImpl", "(I)I", get_type);
    vm.register_native(c, "getDirectionalityImpl", "(I)B", get_directionality);
    vm.register_native(c, "getNameImpl", "(I)Ljava/lang/String;", get_name);
    Ok(())
}

fn ch(vm: &Vm, ctx: NativeCtx) -> Option<char> {
    char::from_u32(vm.arg_i32(ctx, 0) as u32)
}

macro_rules! pred {
    ($name:ident, $body:expr) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let v = ch(vm, ctx).map($body).unwrap_or(false);
            vm.ret_bool(ctx, v);
            Ok(())
        }
    };
}

pred!(is_digit, |c: char| c.is_ascii_digit() || (c as u32 > 0x7f && c.is_numeric()));
pred!(is_letter, |c: char| c.is_alphabetic());
pred!(is_letter_or_digit, |c: char| c.is_alphanumeric());
pred!(is_whitespace, |c: char| c.is_whitespace());
pred!(is_space_char, |c: char| {
    c.is_whitespace() && !matches!(c, '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}')
});
pred!(is_upper, |c: char| c.is_uppercase());
pred!(is_lower, |c: char| c.is_lowercase());
pred!(is_title, |c: char| {
    c.to_uppercase().next().map(|u| u != c).unwrap_or(false)
});
pred!(is_defined, |c: char| !c.is_control() || c as u32 >= 0x20);
pred!(is_ideographic, |c: char| {
    let u = c as u32;
    (0x4E00..=0x9FFF).contains(&u)
        || (0x3400..=0x4DBF).contains(&u)
        || (0x20000..=0x2FA1F).contains(&u)
});
pred!(is_identifier_ignorable, |c: char| c.is_control());

fn ret_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn to_upper(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = ch(vm, ctx)
        .and_then(|c| c.to_uppercase().next())
        .map(|c| c as i32)
        .unwrap_or_else(|| vm.arg_i32(ctx, 0));
    vm.ret_i32(ctx, v);
    Ok(())
}

fn to_lower(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = ch(vm, ctx)
        .and_then(|c| c.to_lowercase().next())
        .map(|c| c as i32)
        .unwrap_or_else(|| vm.arg_i32(ctx, 0));
    vm.ret_i32(ctx, v);
    Ok(())
}

fn digit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let cp = vm.arg_i32(ctx, 0) as u32;
    let radix = vm.arg_i32(ctx, 1);
    let v = char::from_u32(cp)
        .and_then(|c| c.to_digit(radix.max(2) as u32))
        .map(|d| d as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn numeric_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = ch(vm, ctx)
        .and_then(|c| c.to_digit(10))
        .map(|d| d as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}

/// Approximate Java's `Character.getType` general-category codes.
fn get_type(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = match ch(vm, ctx) {
        Some(c) => c,
        None => {
            vm.ret_i32(ctx, 0);
            return Ok(());
        }
    };
    let t = if c.is_uppercase() {
        1
    } else if c.is_lowercase() {
        2
    } else if c.is_ascii_digit() {
        9
    } else if c.is_alphabetic() {
        5
    } else if c.is_whitespace() {
        12
    } else if c.is_control() {
        15
    } else {
        0
    };
    vm.ret_i32(ctx, t);
    Ok(())
}

fn get_directionality(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // DIRECTIONALITY_UNDEFINED
    vm.ret_i32(ctx, 0);
    Ok(())
}

fn get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, crate::heap::NULL);
    Ok(())
}
