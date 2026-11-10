//! `java.lang` builtins: Object, Class, String, StringBuilder, System,
//! PrintStream, Throwable and the boxed primitives.

use acos_dex::access;

use crate::class::NativeCtx;
use crate::corelib::Vm;
use crate::error::{VmError, VmResult};
use crate::format;
use crate::heap::{ArrayData, ArrayObj, BuiltinData, Handle, Object, NULL};
use crate::vm::{array_descriptor_to_name, descriptor_to_dotted};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    install_interfaces(vm)?;
    install_primitives(vm)?;
    install_object(vm)?;
    install_class(vm)?;
    install_string(vm)?;
    install_string_builder(vm)?;
    install_print_stream(vm)?;
    install_system(vm)?;
    install_throwables(vm)?;
    install_boxes(vm)?;
    Ok(())
}

fn install_interfaces(vm: &mut Vm) -> VmResult<()> {
    let iface = access::PUBLIC | access::INTERFACE | access::ABSTRACT;
    let def = |vm: &mut Vm, desc: &str, ifaces: &[&str]| -> VmResult<()> {
        let id = vm.def_class(desc, iface, None, ifaces)?;
        vm.link_class(id)?;
        Ok(())
    };
    def(vm, "Ljava/lang/Cloneable;", &[])?;
    def(vm, "Ljava/io/Serializable;", &[])?;
    def(vm, "Ljava/lang/CharSequence;", &[])?;
    def(vm, "Ljava/lang/Comparable;", &[])?;
    def(vm, "Ljava/lang/Runnable;", &[])?;
    def(vm, "Ljava/lang/Iterable;", &[])?;
    def(vm, "Ljava/util/Iterator;", &[])?;
    def(vm, "Ljava/util/Collection;", &["Ljava/lang/Iterable;"])?;
    def(vm, "Ljava/util/List;", &["Ljava/util/Collection;"])?;
    def(vm, "Ljava/util/Set;", &["Ljava/util/Collection;"])?;
    def(vm, "Ljava/util/Map;", &[])?;
    def(vm, "Ljava/util/Map$Entry;", &[])?;
    def(
        vm,
        "Ljava/util/function/Function;",
        &[],
    )?;
    def(vm, "Ljava/util/function/BiFunction;", &[])?;
    def(vm, "Ljava/util/function/Predicate;", &[])?;
    def(vm, "Ljava/util/function/Supplier;", &[])?;
    // Declare the interface methods (abstract; implementations provide code).
    let abs = access::PUBLIC | access::ABSTRACT;
    let iterable = vm.load_class("Ljava/lang/Iterable;")?;
    vm.add_native(iterable, "iterator", "()Ljava/util/Iterator;", abs, unimpl);
    let iterator = vm.load_class("Ljava/util/Iterator;")?;
    vm.add_native(iterator, "hasNext", "()Z", abs, unimpl);
    vm.add_native(iterator, "next", "()Ljava/lang/Object;", abs, unimpl);
    let list = vm.load_class("Ljava/util/List;")?;
    for (n, s) in [
        ("add", "(Ljava/lang/Object;)Z"),
        ("get", "(I)Ljava/lang/Object;"),
        ("size", "()I"),
        ("isEmpty", "()Z"),
        ("contains", "(Ljava/lang/Object;)Z"),
        ("indexOf", "(Ljava/lang/Object;)I"),
        ("lastIndexOf", "(Ljava/lang/Object;)I"),
        ("remove", "(Ljava/lang/Object;)Z"),
        ("iterator", "()Ljava/util/Iterator;"),
        ("toArray", "()[Ljava/lang/Object;"),
    ] {
        vm.add_native(list, n, s, abs, unimpl);
    }
    let set = vm.load_class("Ljava/util/Set;")?;
    for (n, s) in [
        ("iterator", "()Ljava/util/Iterator;"),
        ("contains", "(Ljava/lang/Object;)Z"),
        ("size", "()I"),
    ] {
        vm.add_native(set, n, s, abs, unimpl);
    }
    let map = vm.load_class("Ljava/util/Map;")?;
    for (n, s) in [
        ("put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;"),
        ("get", "(Ljava/lang/Object;)Ljava/lang/Object;"),
        ("containsKey", "(Ljava/lang/Object;)Z"),
        ("size", "()I"),
        ("keySet", "()Ljava/util/Set;"),
        ("entrySet", "()Ljava/util/Set;"),
    ] {
        vm.add_native(map, n, s, abs, unimpl);
    }
    let entry = vm.load_class("Ljava/util/Map$Entry;")?;
    vm.add_native(entry, "getKey", "()Ljava/lang/Object;", abs, unimpl);
    vm.add_native(entry, "getValue", "()Ljava/lang/Object;", abs, unimpl);
    let func = vm.load_class("Ljava/util/function/Function;")?;
    vm.add_native(
        func,
        "apply",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        abs,
        unimpl,
    );
    let bifunc = vm.load_class("Ljava/util/function/BiFunction;")?;
    vm.add_native(
        bifunc,
        "apply",
        "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
        abs,
        unimpl,
    );
    let pred = vm.load_class("Ljava/util/function/Predicate;")?;
    vm.add_native(pred, "test", "(Ljava/lang/Object;)Z", abs, unimpl);
    let sup = vm.load_class("Ljava/util/function/Supplier;")?;
    vm.add_native(sup, "get", "()Ljava/lang/Object;", abs, unimpl);
    let runnable = vm.load_class("Ljava/lang/Runnable;")?;
    vm.add_native(runnable, "run", "()V", abs, unimpl);
    let comparable = vm.load_class("Ljava/lang/Comparable;")?;
    vm.add_native(
        comparable,
        "compareTo",
        "(Ljava/lang/Object;)I",
        abs,
        unimpl,
    );
    let charseq = vm.load_class("Ljava/lang/CharSequence;")?;
    vm.add_native(charseq, "length", "()I", abs, unimpl);
    Ok(())
}

fn unimpl(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Err(VmError::Internal("abstract interface method invoked".into()))
}

fn install_primitives(vm: &mut Vm) -> VmResult<()> {
    for (desc, _name) in [
        ("Z", "boolean"),
        ("B", "byte"),
        ("C", "char"),
        ("S", "short"),
        ("I", "int"),
        ("J", "long"),
        ("F", "float"),
        ("D", "double"),
        ("V", "void"),
    ] {
        let id = vm.def_root_class(desc, access::PUBLIC | access::FINAL | access::ABSTRACT);
        vm.classes[id as usize].is_primitive = true;
        vm.link_class(id)?;
    }
    Ok(())
}

fn primitive_class_name(desc: &str) -> &'static str {
    match desc {
        "Z" => "boolean",
        "B" => "byte",
        "C" => "char",
        "S" => "short",
        "I" => "int",
        "J" => "long",
        "F" => "float",
        "D" => "double",
        "V" => "void",
        _ => "void",
    }
}

fn set_type_field(vm: &mut Vm, box_desc: &str, prim_desc: &str) -> VmResult<()> {
    let box_id = vm.load_class(box_desc)?;
    vm.add_static_field(
        box_id,
        "TYPE",
        "Ljava/lang/Class;",
        access::PUBLIC | access::STATIC | access::FINAL,
    );
    let prim = vm.load_class(prim_desc)?;
    let obj = vm.class_object(prim)?;
    vm.native_roots.push(obj);
    let off = vm.classes[box_id as usize]
        .declared_static_fields
        .iter()
        .find(|f| f.name == "TYPE")
        .map(|f| f.offset as usize)
        .unwrap_or(0);
    vm.classes[box_id as usize].statics[off] = obj;
    vm.native_roots.pop();
    Ok(())
}

fn install_object(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_root_class("Ljava/lang/Object;", access::PUBLIC);
    vm.c_object = id;
    vm.add_native(id, "<init>", "()V", access::PUBLIC, obj_init);
    vm.add_native(id, "getClass", "()Ljava/lang/Class;", access::PUBLIC, obj_get_class);
    vm.add_native(id, "hashCode", "()I", access::PUBLIC, obj_hash_code);
    vm.add_native(
        id,
        "equals",
        "(Ljava/lang/Object;)Z",
        access::PUBLIC,
        obj_equals,
    );
    vm.add_native(
        id,
        "toString",
        "()Ljava/lang/String;",
        access::PUBLIC,
        obj_to_string,
    );
    vm.add_native(id, "clone", "()Ljava/lang/Object;", access::PUBLIC, obj_clone);
    vm.link_class(id)?;
    Ok(())
}

fn obj_init(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn obj_get_class(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = vm
        .class_of_object(this)
        .ok_or_else(|| VmError::Internal("getClass on non-object".into()))?;
    let h = vm.class_object(cid)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn obj_hash_code(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, this as i32);
    Ok(())
}

fn obj_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    vm.ret_bool(ctx, this == other);
    Ok(())
}

fn obj_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = vm.class_of_object(this).unwrap_or(vm.c_object);
    let name = vm.class_desc_of_id(cid);
    let s = format!("{name}@{this:x}");
    let h = vm.new_string(&s)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn obj_clone(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cloned = clone_object(vm, this)?;
    vm.native_roots.push(cloned);
    vm.ret_ref(ctx, cloned);
    vm.native_roots.pop();
    Ok(())
}

pub(crate) fn clone_object(vm: &mut Vm, this: Handle) -> VmResult<Handle> {
    let obj = vm
        .heap
        .get(this)
        .cloned()
        .ok_or_else(|| VmError::Internal("clone of dead object".into()))?;
    match obj {
        Object::Array(a) => vm.alloc(Object::Array(clone_array(a))),
        Object::Instance { class, fields } => vm.alloc(Object::Instance { class, fields }),
        Object::String(u) => vm.alloc(Object::String(u)),
        Object::Class(c) => vm.alloc(Object::Class(c)),
        Object::Builtin { class, data } => vm.alloc(Object::Builtin { class, data }),
    }
}

fn clone_array(a: ArrayObj) -> ArrayObj {
    ArrayObj {
        elem_desc: a.elem_desc,
        data: match a.data {
            ArrayData::Bool(v) => ArrayData::Bool(v),
            ArrayData::Byte(v) => ArrayData::Byte(v),
            ArrayData::Char(v) => ArrayData::Char(v),
            ArrayData::Short(v) => ArrayData::Short(v),
            ArrayData::Int(v) => ArrayData::Int(v),
            ArrayData::Long(v) => ArrayData::Long(v),
            ArrayData::Float(v) => ArrayData::Float(v),
            ArrayData::Double(v) => ArrayData::Double(v),
            ArrayData::Ref(v) => ArrayData::Ref(v),
        },
    }
}

fn install_class(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class("Ljava/lang/Class;", access::PUBLIC, Some("Ljava/lang/Object;"), &[])?;
    vm.c_class = id;
    vm.add_native(id, "getName", "()Ljava/lang/String;", access::PUBLIC, class_get_name);
    vm.add_native(
        id,
        "getSimpleName",
        "()Ljava/lang/String;",
        access::PUBLIC,
        class_get_simple_name,
    );
    vm.add_native(id, "isInterface", "()Z", access::PUBLIC, class_is_interface);
    vm.add_native(id, "isEnum", "()Z", access::PUBLIC, class_is_enum);
    vm.add_native(id, "isArray", "()Z", access::PUBLIC, class_is_array);
    vm.add_native(id, "isPrimitive", "()Z", access::PUBLIC, class_is_primitive_false);
    vm.add_native(
        id,
        "getSuperclass",
        "()Ljava/lang/Class;",
        access::PUBLIC,
        class_get_superclass,
    );
    vm.add_native(
        id,
        "getComponentType",
        "()Ljava/lang/Class;",
        access::PUBLIC,
        class_get_component,
    );
    vm.add_native(
        id,
        "isAssignableFrom",
        "(Ljava/lang/Class;)Z",
        access::PUBLIC,
        class_is_assignable_from,
    );
    vm.link_class(id)?;
    Ok(())
}

fn class_desc_of(vm: &Vm, this: Handle) -> VmResult<u32> {
    match vm.heap.get(this) {
        Some(Object::Class(c)) => Ok(*c),
        _ => Err(VmError::Internal("Class method on non-Class".into())),
    }
}

fn class_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    let desc = vm.class_desc(cid).to_string();
    let name = if vm.classes[cid as usize].is_primitive {
        primitive_class_name(&desc).to_string()
    } else if desc.starts_with('[') {
        desc.replace('/', ".")
    } else {
        descriptor_to_dotted(&desc)
    };
    let h = vm.new_string(&name)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn class_get_simple_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    let desc = vm.class_desc(cid).to_string();
    let name = if vm.classes[cid as usize].is_primitive {
        primitive_class_name(&desc).to_string()
    } else if desc.starts_with('[') {
        array_descriptor_to_name(&desc)
    } else {
        let dotted = descriptor_to_dotted(&desc);
        let part = dotted.rsplit('.').next().unwrap_or(&dotted);
        match part.rfind('$') {
            Some(i) => part[i + 1..].to_string(),
            None => part.to_string(),
        }
    };
    let h = vm.new_string(&name)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn class_is_interface(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    let v = vm.classes[cid as usize].is_interface && !vm.classes[cid as usize].is_array;
    vm.ret_bool(ctx, v);
    Ok(())
}

fn class_is_enum(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    let v = vm.classes[cid as usize].access & access::ENUM != 0;
    vm.ret_bool(ctx, v);
    Ok(())
}

fn class_is_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    vm.ret_bool(ctx, vm.classes[cid as usize].is_array);
    Ok(())
}

fn class_is_primitive_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    vm.ret_bool(ctx, vm.classes[cid as usize].is_primitive);
    Ok(())
}

fn class_get_superclass(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    match vm.classes[cid as usize].super_class {
        Some(s) => {
            let h = vm.class_object(s)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn class_get_component(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = class_desc_of(vm, this)?;
    let desc = vm.class_desc(cid).to_string();
    if !desc.starts_with('[') {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    }
    let comp = &desc[1..];
    let cid2 = vm.load_class(comp)?;
    let h = vm.class_object(cid2)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn class_is_assignable_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let to = class_desc_of(vm, this)?;
    let other = vm.arg_ref(ctx, 0);
    if other == NULL {
        vm.ret_bool(ctx, false);
        return Ok(());
    }
    let from = class_desc_of(vm, other)?;
    let v = vm.is_assignable(from, to)?;
    vm.ret_bool(ctx, v);
    Ok(())
}

// ---------------------------------------------------------------------------
// String
// ---------------------------------------------------------------------------

fn install_string(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/String;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &[
            "Ljava/lang/CharSequence;",
            "Ljava/lang/Comparable;",
            "Ljava/io/Serializable;",
        ],
    )?;
    vm.c_string = id;
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "()V", p, str_init_empty);
    vm.add_native(id, "<init>", "(Ljava/lang/String;)V", p, str_init_copy);
    vm.add_native(id, "length", "()I", p, str_length);
    vm.add_native(id, "isEmpty", "()Z", p, str_is_empty);
    vm.add_native(id, "charAt", "(I)C", p, str_char_at);
    vm.add_native(id, "codePointAt", "(I)I", p, str_code_point_at);
    vm.add_native(id, "equals", "(Ljava/lang/Object;)Z", p, str_equals);
    vm.add_native(id, "equalsIgnoreCase", "(Ljava/lang/String;)Z", p, str_equals_ignore_case);
    vm.add_native(id, "hashCode", "()I", p, str_hash_code);
    vm.add_native(id, "compareTo", "(Ljava/lang/String;)I", p, str_compare_to);
    vm.add_native(id, "compareTo", "(Ljava/lang/Object;)I", p, str_compare_to_obj);
    vm.add_native(id, "toString", "()Ljava/lang/String;", p, str_self);
    vm.add_native(id, "substring", "(I)Ljava/lang/String;", p, str_substring_from);
    vm.add_native(id, "substring", "(II)Ljava/lang/String;", p, str_substring);
    vm.add_native(id, "indexOf", "(I)I", p, str_index_of_char);
    vm.add_native(id, "indexOf", "(Ljava/lang/String;)I", p, str_index_of_str);
    vm.add_native(id, "lastIndexOf", "(I)I", p, str_last_index_of_char);
    vm.add_native(id, "startsWith", "(Ljava/lang/String;)Z", p, str_starts_with);
    vm.add_native(id, "endsWith", "(Ljava/lang/String;)Z", p, str_ends_with);
    vm.add_native(id, "contains", "(Ljava/lang/CharSequence;)Z", p, str_contains);
    vm.add_native(id, "toUpperCase", "()Ljava/lang/String;", p, str_to_upper);
    vm.add_native(id, "toLowerCase", "()Ljava/lang/String;", p, str_to_lower);
    vm.add_native(id, "trim", "()Ljava/lang/String;", p, str_trim);
    vm.add_native(id, "replace", "(CC)Ljava/lang/String;", p, str_replace_char);
    vm.add_native(
        id,
        "replace",
        "(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;",
        p,
        str_replace_seq,
    );
    vm.add_native(id, "split", "(Ljava/lang/String;)[Ljava/lang/String;", p, str_split);
    vm.add_native(
        id,
        "join",
        "(Ljava/lang/CharSequence;[Ljava/lang/CharSequence;)Ljava/lang/String;",
        access::PUBLIC | access::STATIC,
        str_join,
    );
    vm.add_native(id, "valueOf", "(Ljava/lang/Object;)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_object);
    vm.add_native(id, "valueOf", "(C)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_char);
    vm.add_native(id, "valueOf", "(I)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_int);
    vm.add_native(id, "valueOf", "(J)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_long);
    vm.add_native(id, "valueOf", "(Z)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_bool);
    vm.add_native(id, "valueOf", "(D)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_double);
    vm.add_native(id, "valueOf", "(F)Ljava/lang/String;", access::PUBLIC | access::STATIC, str_value_of_float);
    vm.link_class(id)?;
    Ok(())
}

fn self_units(vm: &Vm, this: Handle) -> VmResult<&[u16]> {
    match vm.heap.get(this) {
        Some(Object::String(u)) => Ok(u),
        _ => Err(VmError::Internal("not a String".into())),
    }
}

fn make_string(vm: &mut Vm, units: Vec<u16>) -> VmResult<Handle> {
    vm.new_string_utf16(units)
}

fn str_init_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    if let Some(Object::String(u)) = vm.heap.get_mut(this) {
        u.clear();
    }
    Ok(())
}

fn str_init_copy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let src = vm.arg_ref(ctx, 0);
    if src == NULL {
        return Err(vm.throw_npe("src"));
    }
    let units = self_units(vm, src)?.to_vec();
    if let Some(Object::String(u)) = vm.heap.get_mut(this) {
        *u = units;
    }
    Ok(())
}

fn str_self(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_ref(ctx, this);
    Ok(())
}

fn str_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let n = if this == NULL { 0 } else { self_units(vm, this)?.len() as i32 };
    vm.ret_i32(ctx, n);
    Ok(())
}

fn str_is_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let n = if this == NULL { 0 } else { self_units(vm, this)?.len() };
    vm.ret_bool(ctx, n == 0);
    Ok(())
}

fn str_char_at(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let idx = vm.arg_i32(ctx, 0);
    let units = self_units(vm, this)?.to_vec();
    if idx < 0 || idx as usize >= units.len() {
        let msg = format!("String index out of range: {idx}");
        return Err(vm.throw_new("Ljava/lang/StringIndexOutOfBoundsException;", Some(&msg)));
    }
    vm.ret_u16(ctx, units[idx as usize]);
    Ok(())
}

fn str_code_point_at(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let idx = vm.arg_i32(ctx, 0);
    let units = self_units(vm, this)?.to_vec();
    if idx < 0 || idx as usize >= units.len() {
        return Err(vm.throw_new("Ljava/lang/StringIndexOutOfBoundsException;", None));
    }
    let c = units[idx as usize];
    let cp = if (0xD800..0xDC00).contains(&c) && (idx as usize + 1) < units.len() {
        let c2 = units[idx as usize + 1];
        if (0xDC00..0xE000).contains(&c2) {
            0x10000 + (((c as u32 - 0xD800) << 10) | (c2 as u32 - 0xDC00))
        } else {
            c as u32
        }
    } else {
        c as u32
    };
    vm.ret_i32(ctx, cp as i32);
    Ok(())
}

fn str_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    if this == other {
        vm.ret_bool(ctx, true);
        return Ok(());
    }
    if other == NULL {
        vm.ret_bool(ctx, false);
        return Ok(());
    }
    let a = match self_units(vm, this) {
        Ok(u) => u,
        Err(_) => {
            vm.ret_bool(ctx, false);
            return Ok(());
        }
    };
    let b = match self_units(vm, other) {
        Ok(u) => u,
        Err(_) => {
            vm.ret_bool(ctx, false);
            return Ok(());
        }
    };
    vm.ret_bool(ctx, a == b);
    Ok(())
}

fn str_equals_ignore_case(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    if other == NULL {
        vm.ret_bool(ctx, false);
        return Ok(());
    }
    let a = self_units(vm, this)?.to_vec();
    let b = self_units(vm, other)?.to_vec();
    let eq = a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(x, y)| {
            let cx = char::from_u32(*x as u32).map(|c| c.to_ascii_lowercase() as u16).unwrap_or(*x);
            let cy = char::from_u32(*y as u32).map(|c| c.to_ascii_lowercase() as u16).unwrap_or(*y);
            cx == cy
        });
    vm.ret_bool(ctx, eq);
    Ok(())
}

fn str_hash_code(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let units = self_units(vm, this)?.to_vec();
    let mut h: i32 = 0;
    for c in units {
        h = h.wrapping_mul(31).wrapping_add(c as i32);
    }
    vm.ret_i32(ctx, h);
    Ok(())
}

fn str_compare_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    let a = self_units(vm, this)?.to_vec();
    let b = self_units(vm, other)?.to_vec();
    let n = a.len().min(b.len());
    for i in 0..n {
        if a[i] != b[i] {
            vm.ret_i32(ctx, a[i] as i32 - b[i] as i32);
            return Ok(());
        }
    }
    vm.ret_i32(ctx, a.len() as i32 - b.len() as i32);
    Ok(())
}
fn str_compare_to_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    str_compare_to(vm, ctx)
}

fn str_substring_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let start = vm.arg_i32(ctx, 0);
    let units = self_units(vm, this)?.to_vec();
    if start < 0 || start as usize > units.len() {
        let msg = format!("begin {start}, end {}, length {}", units.len(), units.len());
        return Err(vm.throw_new("Ljava/lang/StringIndexOutOfBoundsException;", Some(&msg)));
    }
    let h = make_string(vm, units[start as usize..].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_substring(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let start = vm.arg_i32(ctx, 0);
    let end = vm.arg_i32(ctx, 1);
    let units = self_units(vm, this)?.to_vec();
    if start < 0 || end > units.len() as i32 || start > end {
        let msg = format!("begin {start}, end {end}, length {}", units.len());
        return Err(vm.throw_new("Ljava/lang/StringIndexOutOfBoundsException;", Some(&msg)));
    }
    let h = make_string(vm, units[start as usize..end as usize].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_index_of_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_u16(ctx, 0);
    let units = self_units(vm, this)?.to_vec();
    let pos = units.iter().position(|&x| x == c).map(|i| i as i32).unwrap_or(-1);
    vm.ret_i32(ctx, pos);
    Ok(())
}

fn str_index_of_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let needle = vm.arg_ref(ctx, 0);
    let hay = self_units(vm, this)?.to_vec();
    let pat = self_units(vm, needle)?.to_vec();
    vm.ret_i32(ctx, find_sub(&hay, &pat) as i32);
    Ok(())
}

fn str_last_index_of_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_u16(ctx, 0);
    let units = self_units(vm, this)?.to_vec();
    let pos = units.iter().rposition(|&x| x == c).map(|i| i as i32).unwrap_or(-1);
    vm.ret_i32(ctx, pos);
    Ok(())
}

fn find_sub(hay: &[u16], pat: &[u16]) -> isize {
    if pat.is_empty() {
        return 0;
    }
    if pat.len() > hay.len() {
        return -1;
    }
    for i in 0..=(hay.len() - pat.len()) {
        if &hay[i..i + pat.len()] == pat {
            return i as isize;
        }
    }
    -1
}

fn str_starts_with(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let prefix = vm.arg_ref(ctx, 0);
    let a = self_units(vm, this)?.to_vec();
    let b = self_units(vm, prefix)?.to_vec();
    vm.ret_bool(ctx, a.starts_with(&b));
    Ok(())
}

fn str_ends_with(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let suffix = vm.arg_ref(ctx, 0);
    let a = self_units(vm, this)?.to_vec();
    let b = self_units(vm, suffix)?.to_vec();
    vm.ret_bool(ctx, a.ends_with(&b));
    Ok(())
}

fn str_contains(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let needle = vm.arg_ref(ctx, 0);
    let a = self_units(vm, this)?.to_vec();
    let b = self_units(vm, needle)?.to_vec();
    // CharSequence may be a StringBuilder.
    let b = if b.is_empty() {
        match vm.string_to_rust(needle) {
            Some(s) => s.encode_utf16().collect::<Vec<u16>>(),
            None => b,
        }
    } else {
        b
    };
    vm.ret_bool(ctx, find_sub(&a, &b) >= 0);
    Ok(())
}

fn str_to_upper(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let units = self_units(vm, this)?.to_vec();
    let out: Vec<u16> = units
        .iter()
        .map(|&c| {
            char::from_u32(c as u32)
                .and_then(|ch| ch.to_uppercase().next())
                .map(|ch| ch as u32 as u16)
                .unwrap_or(c)
        })
        .collect();
    let h = make_string(vm, out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_to_lower(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let units = self_units(vm, this)?.to_vec();
    let out: Vec<u16> = units
        .iter()
        .map(|&c| {
            char::from_u32(c as u32)
                .and_then(|ch| ch.to_lowercase().next())
                .map(|ch| ch as u32 as u16)
                .unwrap_or(c)
        })
        .collect();
    let h = make_string(vm, out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_trim(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let units = self_units(vm, this)?.to_vec();
    let mut start = 0;
    let mut end = units.len();
    while start < end && units[start] <= 0x20 {
        start += 1;
    }
    while end > start && units[end - 1] <= 0x20 {
        end -= 1;
    }
    let h = make_string(vm, units[start..end].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_replace_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let from = vm.arg_u16(ctx, 0);
    let to = vm.arg_u16(ctx, 1);
    let units = self_units(vm, this)?.to_vec();
    let out: Vec<u16> = units.into_iter().map(|c| if c == from { to } else { c }).collect();
    let h = make_string(vm, out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_replace_seq(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let from_h = vm.arg_ref(ctx, 0);
    let to_h = vm.arg_ref(ctx, 1);
    let hay = self_units(vm, this)?.to_vec();
    let from = charseq_units(vm, from_h)?;
    let to = charseq_units(vm, to_h)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i < hay.len() {
        if !from.is_empty() && i + from.len() <= hay.len() && &hay[i..i + from.len()] == from.as_slice() {
            out.extend_from_slice(&to);
            i += from.len();
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    let h = make_string(vm, out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn charseq_units(vm: &Vm, h: Handle) -> VmResult<Vec<u16>> {
    match vm.heap.get(h) {
        Some(Object::String(u)) => Ok(u.clone()),
        Some(Object::Builtin {
            data: BuiltinData::StringBuilder(u),
            ..
        }) => Ok(u.clone()),
        _ => Ok(Vec::new()),
    }
}

fn str_split(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let regex = vm.arg_ref(ctx, 0);
    let text = String::from_utf16_lossy(self_units(vm, this)?);
    let pat = String::from_utf16_lossy(self_units(vm, regex)?);
    // The fixture only uses a single literal comma; handle a one-char literal
    // separator generically for non-regex literals.
    let parts: Vec<String> = if pat.len() == 1 {
        text.split(pat.chars().next().unwrap()).map(|s| s.to_string()).collect()
    } else {
        text.split(&pat).map(|s| s.to_string()).collect()
    };
    let desc = "Ljava/lang/String;";
    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: desc.to_string(),
        data: ArrayData::Ref(vec![NULL; parts.len()]),
    }))?;
    vm.native_roots.push(arr);
    let mut handles = Vec::new();
    let parts_count = parts.len();
    for p in parts {
        let h = vm.new_string(&p)?;
        vm.native_roots.push(h);
        handles.push(h);
    }
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Ref(v),
        ..
    })) = vm.heap.get_mut(arr)
    {
        *v = handles;
    }
    for _ in 0..parts_count {
        vm.native_roots.pop();
    }
    vm.native_roots.pop();
    vm.ret_ref(ctx, arr);
    Ok(())
}

fn str_join(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let delim = vm.arg_ref(ctx, 0);
    let elems = vm.arg_ref(ctx, 1);
    let delim_units = charseq_units(vm, delim)?;
    let n = match vm.heap.get(elems) {
        Some(Object::Array(a)) => a.len(),
        _ => 0,
    };
    let mut all = Vec::new();
    for i in 0..n {
        if i > 0 {
            all.extend_from_slice(&delim_units);
        }
        let e = match vm.heap.get(elems) {
            Some(Object::Array(a)) => match &a.data {
                ArrayData::Ref(v) => v[i],
                _ => NULL,
            },
            _ => NULL,
        };
        let u = charseq_units(vm, e)?;
        all.extend_from_slice(&u);
    }
    let h = make_string(vm, all)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_value_of_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let o = vm.arg_ref(ctx, 0);
    if o == NULL {
        let h = vm.intern_str("null")?;
        vm.ret_ref(ctx, h);
        return Ok(());
    }
    if let Some(Object::String(_)) | Some(Object::Builtin { .. }) = vm.heap.get(o) {
        if vm.str_units(o).is_some() {
            vm.ret_ref(ctx, o);
            return Ok(());
        }
    }
    let s = call_to_string(vm, o)?;
    vm.ret_ref(ctx, s);
    Ok(())
}

pub(crate) fn call_to_string(vm: &mut Vm, o: Handle) -> VmResult<Handle> {
    if o == NULL {
        return vm.intern_str("null");
    }
    if let Some(u) = vm.str_units(o) {
        let _ = u;
        return Ok(o);
    }
    let m = vm.dispatch(o, "toString", "()Ljava/lang/String;")?;
    let r = crate::interp::call_method(vm, m, o, &[])?;
    Ok(r as Handle)
}

fn str_value_of_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    let h = make_string(vm, vec![c])?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_value_of_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let h = vm.new_string(&v.to_string())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_value_of_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let h = vm.new_string(&v.to_string())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_value_of_bool(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_bool(ctx, 0);
    let h = vm.intern_str(if v { "true" } else { "false" })?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_value_of_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f64(ctx, 0);
    let h = vm.new_string(&format::double_to_string(v))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_value_of_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f32(ctx, 0);
    let h = vm.new_string(&format::float_to_string(v))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// StringBuilder
// ---------------------------------------------------------------------------

fn install_string_builder(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/StringBuilder;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &["Ljava/lang/CharSequence;"],
    )?;
    vm.c_string_builder = id;
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "()V", p, sb_init);
    vm.add_native(id, "<init>", "(Ljava/lang/String;)V", p, sb_init_str);
    vm.add_native(id, "append", "(Ljava/lang/String;)Ljava/lang/StringBuilder;", p, sb_append_str);
    vm.add_native(id, "append", "(Ljava/lang/Object;)Ljava/lang/StringBuilder;", p, sb_append_obj);
    vm.add_native(id, "append", "(C)Ljava/lang/StringBuilder;", p, sb_append_char);
    vm.add_native(id, "append", "(I)Ljava/lang/StringBuilder;", p, sb_append_int);
    vm.add_native(id, "append", "(J)Ljava/lang/StringBuilder;", p, sb_append_long);
    vm.add_native(id, "append", "(Z)Ljava/lang/StringBuilder;", p, sb_append_bool);
    vm.add_native(id, "append", "(F)Ljava/lang/StringBuilder;", p, sb_append_float);
    vm.add_native(id, "append", "(D)Ljava/lang/StringBuilder;", p, sb_append_double);
    vm.add_native(id, "toString", "()Ljava/lang/String;", p, sb_to_string);
    vm.add_native(id, "length", "()I", p, sb_length);
    vm.add_native(id, "capacity", "()I", p, sb_capacity);
    vm.add_native(id, "reverse", "()Ljava/lang/StringBuilder;", p, sb_reverse);
    vm.add_native(
        id,
        "insert",
        "(ILjava/lang/String;)Ljava/lang/StringBuilder;",
        p,
        sb_insert,
    );
    vm.add_native(
        id,
        "replace",
        "(IILjava/lang/String;)Ljava/lang/StringBuilder;",
        p,
        sb_replace,
    );
    vm.link_class(id)?;
    Ok(())
}

fn sb_buf(vm: &mut Vm, this: Handle) -> VmResult<&mut Vec<u16>> {
    match vm.heap.get_mut(this) {
        Some(Object::Builtin {
            data: BuiltinData::StringBuilder(u),
            ..
        }) => Ok(u),
        _ => Err(VmError::Internal("not a StringBuilder".into())),
    }
}

fn sb_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let _ = sb_buf(vm, this)?;
    Ok(())
}

fn sb_init_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let s = vm.arg_ref(ctx, 0);
    let units = self_units(vm, s)?.to_vec();
    *sb_buf(vm, this)? = units;
    Ok(())
}

fn sb_append_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let s = vm.arg_ref(ctx, 0);
    if s == NULL {
        sb_buf(vm, this)?.extend_from_slice(&"null".encode_utf16().collect::<Vec<_>>());
    } else {
        let units = self_units(vm, s)?.to_vec();
        sb_buf(vm, this)?.extend_from_slice(&units);
    }
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_append_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    if o == NULL {
        sb_buf(vm, this)?.extend_from_slice(&"null".encode_utf16().collect::<Vec<_>>());
    } else {
        let st = call_to_string(vm, o)?;
        let units = self_units(vm, st)?.to_vec();
        sb_buf(vm, this)?.extend_from_slice(&units);
    }
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_append_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_u16(ctx, 0);
    sb_buf(vm, this)?.push(c);
    vm.ret_ref(ctx, this);
    Ok(())
}

/// Append the decimal representation of `v` to a UTF-16 buffer without the
/// intermediate `String` allocation `v.to_string()` used to make.
#[inline]
fn append_i64_utf16(buf: &mut Vec<u16>, v: i64) {
    if v == 0 {
        buf.push(b'0' as u16);
        return;
    }
    let neg = v < 0;
    // `i64::MIN` has no positive counterpart; work on the wrapped magnitude.
    let mut u = if neg { (v as u64).wrapping_neg() } else { v as u64 };
    let mut tmp = [0u16; 20];
    let mut i = tmp.len();
    while u > 0 {
        i -= 1;
        tmp[i] = (b'0' as u64 + (u % 10)) as u16;
        u /= 10;
    }
    if neg {
        buf.push(b'-' as u16);
    }
    buf.extend_from_slice(&tmp[i..]);
}

fn sb_append_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i32(ctx, 0);
    append_i64_utf16(sb_buf(vm, this)?, v as i64);
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_append_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i64(ctx, 0);
    append_i64_utf16(sb_buf(vm, this)?, v);
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_append_bool(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_bool(ctx, 0);
    let s = if v { "true" } else { "false" };
    sb_buf(vm, this)?.extend(s.encode_utf16());
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_append_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_f32(ctx, 0);
    let s = format::float_to_string(v);
    sb_buf(vm, this)?.extend(s.encode_utf16());
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_append_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_f64(ctx, 0);
    let s = format::double_to_string(v);
    sb_buf(vm, this)?.extend(s.encode_utf16());
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let units = sb_buf(vm, this)?.clone();
    let h = make_string(vm, units)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sb_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let n = sb_buf(vm, this)?.len() as i32;
    vm.ret_i32(ctx, n);
    Ok(())
}

fn sb_capacity(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let n = sb_buf(vm, this)?.capacity(); // Vec capacity as a stand-in
    vm.ret_i32(ctx, n.max(16) as i32);
    Ok(())
}

fn sb_reverse(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    sb_buf(vm, this)?.reverse();
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_insert(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let idx = vm.arg_i32(ctx, 0);
    let s = vm.arg_ref(ctx, 1);
    let units = self_units(vm, s)?.to_vec();
    let buf = sb_buf(vm, this)?;
    let at = (idx.max(0) as usize).min(buf.len());
    for (i, u) in units.into_iter().enumerate() {
        buf.insert(at + i, u);
    }
    vm.ret_ref(ctx, this);
    Ok(())
}

fn sb_replace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let start = vm.arg_i32(ctx, 0).max(0) as usize;
    let end = vm.arg_i32(ctx, 1).max(0) as usize;
    let s = vm.arg_ref(ctx, 2);
    let units = self_units(vm, s)?.to_vec();
    let buf = sb_buf(vm, this)?;
    let start = start.min(buf.len());
    let end = end.min(buf.len()).max(start);
    buf.splice(start..end, units);
    vm.ret_ref(ctx, this);
    Ok(())
}

// ---------------------------------------------------------------------------
// PrintStream / System
// ---------------------------------------------------------------------------

fn install_print_stream(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/io/PrintStream;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    vm.c_print_stream = id;
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "()V", p, ps_init);
    vm.add_native(id, "println", "(Ljava/lang/String;)V", p, ps_println_str);
    vm.add_native(id, "println", "(Ljava/lang/Object;)V", p, ps_println_obj);
    vm.add_native(id, "println", "()V", p, ps_println);
    vm.add_native(id, "println", "(C)V", p, ps_println_char);
    vm.add_native(id, "println", "(I)V", p, ps_println_int);
    vm.add_native(id, "println", "(J)V", p, ps_println_long);
    vm.add_native(id, "println", "(Z)V", p, ps_println_bool);
    vm.add_native(id, "println", "(D)V", p, ps_println_double);
    vm.add_native(id, "print", "(Ljava/lang/String;)V", p, ps_print_str);
    vm.add_native(id, "print", "(Ljava/lang/Object;)V", p, ps_print_obj);
    vm.add_native(id, "print", "(C)V", p, ps_print_char);
    vm.add_native(id, "print", "(I)V", p, ps_print_int);
    vm.add_native(id, "flush", "()V", p, ps_flush);
    vm.link_class(id)?;
    Ok(())
}

fn ps_is_err(vm: &Vm, this: Handle) -> bool {
    this == vm.stderr && vm.stderr != NULL
}

fn ps_write(vm: &mut Vm, this: Handle, bytes: &[u8]) {
    if ps_is_err(vm, this) {
        vm.err_buf.extend_from_slice(bytes);
    } else {
        vm.out_buf.extend_from_slice(bytes);
    }
}

fn ps_write_units(vm: &mut Vm, this: Handle, units: &[u16]) {
    let s = String::from_utf16_lossy(units);
    ps_write(vm, this, s.as_bytes());
}

fn ps_init(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn ps_flush(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn ps_println(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    ps_write(vm, this, b"\n");
    Ok(())
}

fn ps_print_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let s = vm.arg_ref(ctx, 0);
    if s != NULL {
        let units = self_units(vm, s)?.to_vec();
        ps_write_units(vm, this, &units);
    }
    Ok(())
}

fn ps_println_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    ps_print_str(vm, ctx)?;
    ps_println(vm, ctx)
}

fn ps_print_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    if o == NULL {
        ps_write(vm, this, b"null");
    } else {
        let s = call_to_string(vm, o)?;
        let units = self_units(vm, s)?.to_vec();
        ps_write_units(vm, this, &units);
    }
    Ok(())
}

fn ps_println_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    ps_print_obj(vm, ctx)?;
    ps_println(vm, ctx)
}

fn ps_print_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_u16(ctx, 0);
    ps_write_units(vm, this, &[c]);
    Ok(())
}

fn ps_println_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    ps_print_char(vm, ctx)?;
    ps_println(vm, ctx)
}

fn ps_print_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i32(ctx, 0);
    ps_write(vm, this, v.to_string().as_bytes());
    Ok(())
}

fn ps_println_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    ps_print_int(vm, ctx)?;
    ps_println(vm, ctx)
}

fn ps_println_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i64(ctx, 0);
    ps_write(vm, this, v.to_string().as_bytes());
    ps_println(vm, ctx)
}

fn ps_println_bool(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_bool(ctx, 0);
    ps_write(vm, this, if v { b"true" } else { b"false" });
    ps_println(vm, ctx)
}

fn ps_println_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_f64(ctx, 0);
    ps_write(vm, this, format::double_to_string(v).as_bytes());
    ps_println(vm, ctx)
}

fn install_system(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/System;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    vm.c_system = id;
    vm.add_static_field(id, "out", "Ljava/io/PrintStream;", access::PUBLIC | access::STATIC | access::FINAL);
    vm.add_static_field(id, "err", "Ljava/io/PrintStream;", access::PUBLIC | access::STATIC | access::FINAL);
    let ps = vm.c_print_stream;
    let out = vm.alloc_instance(ps)?;
    vm.native_roots.push(out);
    let err = vm.alloc_instance(ps)?;
    vm.native_roots.push(err);
    vm.stdout = out;
    vm.stderr = err;
    // Set the static fields.
    let out_off = vm.classes[id as usize].declared_static_fields[0].offset as usize;
    let err_off = vm.classes[id as usize].declared_static_fields[1].offset as usize;
    vm.classes[id as usize].statics[out_off] = out;
    vm.classes[id as usize].statics[err_off] = err;
    vm.classes[id as usize].static_refs[out_off] = true;
    vm.classes[id as usize].static_refs[err_off] = true;

    let s = access::PUBLIC | access::STATIC;
    vm.add_native(id, "arraycopy", "(Ljava/lang/Object;ILjava/lang/Object;II)V", s, sys_arraycopy);
    vm.add_native(id, "currentTimeMillis", "()J", s, sys_current_time);
    vm.add_native(id, "nanoTime", "()J", s, sys_nano_time);
    vm.add_native(id, "identityHashCode", "(Ljava/lang/Object;)I", s, sys_identity_hash);
    vm.add_native(id, "exit", "(I)V", s, sys_exit);
    vm.add_native(id, "getProperty", "(Ljava/lang/String;)Ljava/lang/String;", s, sys_get_property);
    vm.link_class(id)?;
    vm.native_roots.pop();
    vm.native_roots.pop();
    Ok(())
}

fn sys_arraycopy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_ref(ctx, 0);
    let src_pos = vm.arg_i32(ctx, 1);
    let dst = vm.arg_ref(ctx, 2);
    let dst_pos = vm.arg_i32(ctx, 3);
    let len = vm.arg_i32(ctx, 4);
    if src == NULL || dst == NULL {
        return Err(vm.throw_npe("arraycopy"));
    }
    if len < 0 || src_pos < 0 || dst_pos < 0 {
        return Err(vm.throw_new("Ljava/lang/IndexOutOfBoundsException;", None));
    }
    // Snapshot source slice, then write to destination (handles overlap).
    let src_data = match vm.heap.get(src) {
        Some(Object::Array(a)) => a.data.clone(),
        _ => return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", None)),
    };
    let (slen, dlen) = match (vm.heap.get(src), vm.heap.get(dst)) {
        (Some(Object::Array(a)), Some(Object::Array(b))) => (a.len(), b.len()),
        _ => return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", None)),
    };
    if src_pos as usize + len as usize > slen || dst_pos as usize + len as usize > dlen {
        return Err(vm.throw_new("Ljava/lang/ArrayIndexOutOfBoundsException;", None));
    }
    if let Some(Object::Array(b)) = vm.heap.get_mut(dst) {
        let sp = src_pos as usize;
        let dp = dst_pos as usize;
        let n = len as usize;
        match (&src_data, &mut b.data) {
            (ArrayData::Ref(s), ArrayData::Ref(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Int(s), ArrayData::Int(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Byte(s), ArrayData::Byte(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Char(s), ArrayData::Char(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Short(s), ArrayData::Short(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Long(s), ArrayData::Long(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Float(s), ArrayData::Float(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Double(s), ArrayData::Double(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            (ArrayData::Bool(s), ArrayData::Bool(d)) => d[dp..dp + n].copy_from_slice(&s[sp..sp + n]),
            _ => return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", None)),
        }
    }
    Ok(())
}

fn sys_current_time(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    vm.ret_i64(ctx, t);
    Ok(())
}

fn sys_nano_time(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    vm.ret_i64(ctx, t);
    Ok(())
}

fn sys_identity_hash(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let o = vm.arg_ref(ctx, 0);
    vm.ret_i32(ctx, o as i32);
    Ok(())
}

fn sys_exit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let code = vm.arg_i32(ctx, 0);
    vm.exit_request = Some(code);
    vm.stopping = true;
    let t = vm.current_thread;
    vm.threads[t].frames.clear();
    Ok(())
}

fn sys_get_property(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let key = vm.arg_ref(ctx, 0);
    let k = vm.string_to_rust(key).unwrap_or_default();
    let val = match k.as_str() {
        "java.version" => "17",
        "line.separator" => "\n",
        _ => {
            vm.ret_ref(ctx, NULL);
            return Ok(());
        }
    };
    let h = vm.new_string(val)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// Throwable hierarchy
// ---------------------------------------------------------------------------

fn install_throwables(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/Throwable;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &["Ljava/io/Serializable;"],
    )?;
    vm.c_throwable = id;
    vm.add_instance_field(id, "detailMessage", "Ljava/lang/String;", access::PRIVATE);
    vm.add_instance_field(id, "cause", "Ljava/lang/Throwable;", access::PRIVATE);
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "()V", p, thr_init_empty);
    vm.add_native(id, "<init>", "(Ljava/lang/String;)V", p, thr_init_msg);
    vm.add_native(
        id,
        "getMessage",
        "()Ljava/lang/String;",
        p,
        thr_get_message,
    );
    vm.add_native(
        id,
        "getLocalizedMessage",
        "()Ljava/lang/String;",
        p,
        thr_get_message,
    );
    vm.add_native(
        id,
        "toString",
        "()Ljava/lang/String;",
        p,
        thr_to_string,
    );
    vm.add_native(
        id,
        "getCause",
        "()Ljava/lang/Throwable;",
        p,
        thr_get_cause,
    );
    vm.add_native(
        id,
        "printStackTrace",
        "()V",
        p,
        thr_print_stack_trace,
    );
    vm.link_class(id)?;

    let err = vm.def_class("Ljava/lang/Error;", p, Some("Ljava/lang/Throwable;"), &[])?;
    vm.link_class(err)?;
    let exc = vm.def_class(
        "Ljava/lang/Exception;",
        p,
        Some("Ljava/lang/Throwable;"),
        &[],
    )?;
    vm.link_class(exc)?;
    let rt = vm.def_class(
        "Ljava/lang/RuntimeException;",
        p,
        Some("Ljava/lang/Exception;"),
        &[],
    )?;
    vm.link_class(rt)?;

    // Linkage errors.
    let link = vm.def_class(
        "Ljava/lang/LinkageError;",
        p,
        Some("Ljava/lang/Error;"),
        &[],
    )?;
    vm.link_class(link)?;
    for (desc, super_desc) in [
        ("Ljava/lang/OutOfMemoryError;", "Ljava/lang/VirtualMachineError;"),
        ("Ljava/lang/StackOverflowError;", "Ljava/lang/VirtualMachineError;"),
        ("Ljava/lang/AbstractMethodError;", "Ljava/lang/IncompatibleClassChangeError;"),
        ("Ljava/lang/NoClassDefFoundError;", "Ljava/lang/LinkageError;"),
        ("Ljava/lang/ExceptionInInitializerError;", "Ljava/lang/LinkageError;"),
    ] {
        // define intermediate classes lazily
        let _ = super_desc;
        let c = vm.def_class(desc, p, Some("Ljava/lang/Error;"), &[])?;
        vm.link_class(c)?;
    }
    // Runtime exceptions.
    for desc in [
        "Ljava/lang/ArithmeticException;",
        "Ljava/lang/NullPointerException;",
        "Ljava/lang/ClassCastException;",
        "Ljava/lang/NegativeArraySizeException;",
        "Ljava/lang/ArrayStoreException;",
        "Ljava/lang/IllegalArgumentException;",
        "Ljava/lang/IllegalStateException;",
        "Ljava/lang/UnsupportedOperationException;",
        "Ljava/lang/NumberFormatException;",
        "Ljava/lang/IndexOutOfBoundsException;",
        "Ljava/lang/ArrayIndexOutOfBoundsException;",
        "Ljava/lang/StringIndexOutOfBoundsException;",
        "Ljava/util/NoSuchElementException;",
    ] {
        let c = vm.def_class(desc, p, Some("Ljava/lang/RuntimeException;"), &[])?;
        vm.link_class(c)?;
    }
    for desc in [
        "Ljava/lang/InterruptedException;",
        "Ljava/lang/CloneNotSupportedException;",
        "Ljava/lang/reflect/InvocationTargetException;",
    ] {
        let c = vm.def_class(desc, p, Some("Ljava/lang/Exception;"), &[])?;
        vm.link_class(c)?;
    }
    // Pre-create fatal exceptions so throwing them never allocates.
    vm.oom_object = vm.new_throwable("Ljava/lang/OutOfMemoryError;", Some("heap limit exceeded"))?;
    vm.stack_overflow_object =
        vm.new_throwable("Ljava/lang/StackOverflowError;", Some("stack overflow"))?;
    vm.native_roots.push(vm.oom_object);
    vm.native_roots.push(vm.stack_overflow_object);
    Ok(())
}

pub(crate) fn set_instance_field(vm: &mut Vm, this: Handle, name: &str, value: Handle) {
    thr_set_field(vm, this, name, value)
}

fn thr_set_field(vm: &mut Vm, this: Handle, name: &str, value: Handle) {
    let class = match vm.class_of_object(this) {
        Some(c) => c,
        None => return,
    };
    let mut c = Some(class);
    while let Some(cur) = c {
        let f = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == name)
            .cloned();
        if let Some(f) = f {
            if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(this) {
                fields[f.offset as usize] = value;
            }
            return;
        }
        c = vm.classes[cur as usize].super_class;
    }
}

fn thr_get_field(vm: &Vm, this: Handle, name: &str) -> Handle {
    let class = match vm.class_of_object(this) {
        Some(c) => c,
        None => return NULL,
    };
    let mut c = Some(class);
    while let Some(cur) = c {
        let f = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == name)
            .cloned();
        if let Some(f) = f {
            if let Some(Object::Instance { fields, .. }) = vm.heap.get(this) {
                return fields[f.offset as usize];
            }
        }
        c = vm.classes[cur as usize].super_class;
    }
    NULL
}

fn thr_init_empty(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn thr_init_msg(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let msg = vm.arg_ref(ctx, 0);
    thr_set_field(vm, this, "detailMessage", msg);
    Ok(())
}

fn thr_get_message(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let m = thr_get_field(vm, this, "detailMessage");
    vm.ret_ref(ctx, m);
    Ok(())
}

fn thr_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = vm.class_of_object(this).unwrap_or(vm.c_throwable);
    let name = vm.class_desc_of_id(cid);
    let msg = thr_get_field(vm, this, "detailMessage");
    let s = if msg == NULL {
        name
    } else {
        let m = vm.string_to_rust(msg).unwrap_or_default();
        format!("{name}: {m}")
    };
    let h = vm.new_string(&s)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn thr_get_cause(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = thr_get_field(vm, this, "cause");
    vm.ret_ref(ctx, c);
    Ok(())
}

fn thr_print_stack_trace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = vm.class_of_object(this).unwrap_or(vm.c_throwable);
    let name = vm.class_desc_of_id(cid);
    let msg = thr_get_field(vm, this, "detailMessage");
    let first = if msg == NULL {
        name
    } else {
        format!("{name}: {}", vm.string_to_rust(msg).unwrap_or_default())
    };
    let mut text = format!("{first}\n");
    if let Some(entries) = vm.throwable_stacks.get(&this) {
        for e in entries {
            text.push_str(&format!("\tat {}.{}({})\n", e.class, e.method, e.line));
        }
    }
    vm.err_buf.extend_from_slice(text.as_bytes());
    Ok(())
}

// ---------------------------------------------------------------------------
// Boxed primitives
// ---------------------------------------------------------------------------

fn install_boxes(vm: &mut Vm) -> VmResult<()> {
    let num = vm.def_class(
        "Ljava/lang/Number;",
        access::PUBLIC | access::ABSTRACT,
        Some("Ljava/lang/Object;"),
        &["Ljava/io/Serializable;"],
    )?;
    vm.link_class(num)?;

    // Integer
    let int = vm.def_class(
        "Ljava/lang/Integer;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Number;"),
        &["Ljava/lang/Comparable;"],
    )?;
    vm.c_integer = int;
    vm.add_instance_field(int, "value", "I", access::PRIVATE | access::FINAL);
    let p = access::PUBLIC;
    vm.add_native(int, "<init>", "(I)V", p, int_init);
    vm.add_native(int, "intValue", "()I", p, int_value);
    vm.add_native(int, "longValue", "()J", p, int_long_value);
    vm.add_native(int, "doubleValue", "()D", p, int_double_value);
    vm.add_native(int, "equals", "(Ljava/lang/Object;)Z", p, int_equals);
    vm.add_native(int, "hashCode", "()I", p, int_hash);
    vm.add_native(
        int,
        "compareTo",
        "(Ljava/lang/Integer;)I",
        p,
        int_compare_to,
    );
    vm.add_native(
        int,
        "compareTo",
        "(Ljava/lang/Object;)I",
        p,
        int_compare_to_obj,
    );
    vm.add_native(int, "toString", "()Ljava/lang/String;", p, int_to_string_inst);
    vm.add_native(int, "valueOf", "(I)Ljava/lang/Integer;", access::PUBLIC | access::STATIC, int_value_of);
    vm.add_native(int, "parseInt", "(Ljava/lang/String;)I", access::PUBLIC | access::STATIC, int_parse);
    vm.add_native(int, "parseInt", "(Ljava/lang/String;I)I", access::PUBLIC | access::STATIC, int_parse_radix);
    vm.add_native(int, "toString", "(I)Ljava/lang/String;", access::PUBLIC | access::STATIC, int_to_string_static);
    vm.add_native(int, "toString", "(II)Ljava/lang/String;", access::PUBLIC | access::STATIC, int_to_string_radix);
    vm.add_native(int, "toHexString", "(I)Ljava/lang/String;", access::PUBLIC | access::STATIC, int_to_hex);
    vm.add_native(int, "toOctalString", "(I)Ljava/lang/String;", access::PUBLIC | access::STATIC, int_to_octal);
    vm.add_native(int, "toBinaryString", "(I)Ljava/lang/String;", access::PUBLIC | access::STATIC, int_to_binary);
    vm.add_native(int, "bitCount", "(I)I", access::PUBLIC | access::STATIC, int_bit_count);
    vm.add_native(int, "numberOfLeadingZeros", "(I)I", access::PUBLIC | access::STATIC, int_nlz);
    vm.add_native(int, "numberOfTrailingZeros", "(I)I", access::PUBLIC | access::STATIC, int_ntz);
    vm.add_native(int, "highestOneBit", "(I)I", access::PUBLIC | access::STATIC, int_highest_one);
    vm.add_native(int, "lowestOneBit", "(I)I", access::PUBLIC | access::STATIC, int_lowest_one);
    vm.add_native(int, "signum", "(I)I", access::PUBLIC | access::STATIC, int_signum);
    vm.add_native(int, "rotateLeft", "(II)I", access::PUBLIC | access::STATIC, int_rotate_left);
    vm.add_native(int, "rotateRight", "(II)I", access::PUBLIC | access::STATIC, int_rotate_right);
    vm.add_native(int, "compare", "(II)I", access::PUBLIC | access::STATIC, int_compare_static);
    vm.link_class(int)?;

    // Long
    let long = vm.def_class(
        "Ljava/lang/Long;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Number;"),
        &["Ljava/lang/Comparable;"],
    )?;
    vm.c_long = long;
    vm.add_instance_field(long, "value", "J", access::PRIVATE | access::FINAL);
    vm.add_native(long, "<init>", "(J)V", p, long_init);
    vm.add_native(long, "longValue", "()J", p, long_value);
    vm.add_native(long, "intValue", "()I", p, long_int_value);
    vm.add_native(long, "doubleValue", "()D", p, long_double_value);
    vm.add_native(long, "equals", "(Ljava/lang/Object;)Z", p, long_equals);
    vm.add_native(long, "hashCode", "()I", p, long_hash);
    vm.add_native(
        long,
        "compareTo",
        "(Ljava/lang/Long;)I",
        p,
        long_compare_to,
    );
    vm.add_native(
        long,
        "compareTo",
        "(Ljava/lang/Object;)I",
        p,
        long_compare_to_obj,
    );
    vm.add_native(long, "toString", "()Ljava/lang/String;", p, long_to_string_inst);
    vm.add_native(long, "valueOf", "(J)Ljava/lang/Long;", access::PUBLIC | access::STATIC, long_value_of);
    vm.add_native(long, "parseLong", "(Ljava/lang/String;)J", access::PUBLIC | access::STATIC, long_parse);
    vm.add_native(long, "toString", "(J)Ljava/lang/String;", access::PUBLIC | access::STATIC, long_to_string_static);
    vm.add_native(long, "toString", "(JI)Ljava/lang/String;", access::PUBLIC | access::STATIC, long_to_string_radix);
    vm.add_native(long, "toHexString", "(J)Ljava/lang/String;", access::PUBLIC | access::STATIC, long_to_hex);
    vm.add_native(long, "toOctalString", "(J)Ljava/lang/String;", access::PUBLIC | access::STATIC, long_to_octal);
    vm.add_native(long, "toBinaryString", "(J)Ljava/lang/String;", access::PUBLIC | access::STATIC, long_to_binary);
    vm.add_native(long, "bitCount", "(J)I", access::PUBLIC | access::STATIC, long_bit_count);
    vm.add_native(long, "highestOneBit", "(J)J", access::PUBLIC | access::STATIC, long_highest_one);
    vm.add_native(long, "lowestOneBit", "(J)J", access::PUBLIC | access::STATIC, long_lowest_one);
    vm.add_native(long, "signum", "(J)I", access::PUBLIC | access::STATIC, long_signum);
    vm.add_native(long, "rotateLeft", "(JI)J", access::PUBLIC | access::STATIC, long_rotate_left);
    vm.add_native(long, "rotateRight", "(JI)J", access::PUBLIC | access::STATIC, long_rotate_right);
    vm.add_native(long, "compare", "(JJ)I", access::PUBLIC | access::STATIC, long_compare_static);
    vm.link_class(long)?;

    // Character
    let ch = vm.def_class(
        "Ljava/lang/Character;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &["Ljava/lang/Comparable;", "Ljava/io/Serializable;"],
    )?;
    vm.add_instance_field(ch, "value", "C", access::PRIVATE | access::FINAL);
    vm.add_native(ch, "<init>", "(C)V", p, char_init);
    vm.add_native(ch, "charValue", "()C", p, char_value);
    vm.add_native(ch, "isDigit", "(C)Z", access::PUBLIC | access::STATIC, char_is_digit);
    vm.add_native(ch, "isLetter", "(C)Z", access::PUBLIC | access::STATIC, char_is_letter);
    vm.add_native(ch, "isWhitespace", "(C)Z", access::PUBLIC | access::STATIC, char_is_ws);
    vm.add_native(ch, "isUpperCase", "(C)Z", access::PUBLIC | access::STATIC, char_is_upper);
    vm.add_native(ch, "isLowerCase", "(C)Z", access::PUBLIC | access::STATIC, char_is_lower);
    vm.add_native(ch, "toUpperCase", "(C)C", access::PUBLIC | access::STATIC, char_to_upper);
    vm.add_native(ch, "toLowerCase", "(C)C", access::PUBLIC | access::STATIC, char_to_lower);
    vm.add_native(ch, "getNumericValue", "(C)I", access::PUBLIC | access::STATIC, char_numeric);
    vm.add_native(ch, "digit", "(CI)I", access::PUBLIC | access::STATIC, char_digit);
    vm.add_native(ch, "forDigit", "(II)C", access::PUBLIC | access::STATIC, char_for_digit);
    vm.add_native(ch, "hashCode", "(C)I", access::PUBLIC | access::STATIC, char_hash_static);
    vm.add_native(ch, "compare", "(CC)I", access::PUBLIC | access::STATIC, char_compare);
    vm.add_native(ch, "valueOf", "(C)Ljava/lang/Character;", access::PUBLIC | access::STATIC, char_value_of);
    vm.link_class(ch)?;

    // Boolean
    let boolean = vm.def_class(
        "Ljava/lang/Boolean;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &["Ljava/io/Serializable;", "Ljava/lang/Comparable;"],
    )?;
    vm.add_instance_field(boolean, "value", "Z", access::PRIVATE | access::FINAL);
    vm.add_native(boolean, "<init>", "(Z)V", p, bool_init);
    vm.add_native(boolean, "booleanValue", "()Z", p, bool_value);
    vm.add_native(boolean, "valueOf", "(Z)Ljava/lang/Boolean;", access::PUBLIC | access::STATIC, bool_value_of);
    vm.add_native(boolean, "toString", "()Ljava/lang/String;", p, bool_to_string);
    vm.add_native(boolean, "equals", "(Ljava/lang/Object;)Z", p, bool_equals);
    vm.link_class(boolean)?;

    // Float / Double
    let float = vm.def_class(
        "Ljava/lang/Float;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Number;"),
        &["Ljava/lang/Comparable;"],
    )?;
    vm.add_instance_field(float, "value", "F", access::PRIVATE | access::FINAL);
    vm.add_native(float, "<init>", "(F)V", p, float_init);
    vm.add_native(float, "floatValue", "()F", p, float_value);
    vm.add_native(float, "valueOf", "(F)Ljava/lang/Float;", access::PUBLIC | access::STATIC, float_value_of);
    vm.add_native(float, "toString", "()Ljava/lang/String;", p, float_to_string_inst);
    vm.link_class(float)?;
    let double = vm.def_class(
        "Ljava/lang/Double;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Number;"),
        &["Ljava/lang/Comparable;"],
    )?;
    vm.add_instance_field(double, "value", "D", access::PRIVATE | access::FINAL);
    vm.add_native(double, "<init>", "(D)V", p, double_init);
    vm.add_native(double, "doubleValue", "()D", p, double_value);
    vm.add_native(double, "valueOf", "(D)Ljava/lang/Double;", access::PUBLIC | access::STATIC, double_value_of);
    vm.add_native(double, "toString", "()Ljava/lang/String;", p, double_to_string_inst);
    vm.link_class(double)?;
    set_type_field(vm, "Ljava/lang/Integer;", "I")?;
    set_type_field(vm, "Ljava/lang/Long;", "J")?;
    set_type_field(vm, "Ljava/lang/Character;", "C")?;
    set_type_field(vm, "Ljava/lang/Boolean;", "Z")?;
    set_type_field(vm, "Ljava/lang/Float;", "F")?;
    set_type_field(vm, "Ljava/lang/Double;", "D")?;
    Ok(())
}

// --- Integer ---
fn int_get(vm: &Vm, this: Handle) -> i32 {
    thr_get_field(vm, this, "value") as i32
}
fn int_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i32(ctx, 0);
    thr_set_field(vm, this, "value", v as u32);
    Ok(())
}
fn int_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, int_get(vm, this));
    Ok(())
}
fn int_long_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i64(ctx, int_get(vm, this) as i64);
    Ok(())
}
fn int_double_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_f64(ctx, int_get(vm, this) as f64);
    Ok(())
}
fn int_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let eq = vm.class_of_object(o) == Some(vm.c_integer) && int_get(vm, o) == int_get(vm, this);
    vm.ret_bool(ctx, eq);
    Ok(())
}
fn int_hash(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, int_get(vm, this));
    Ok(())
}
fn int_compare_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    vm.ret_i32(ctx, int_get(vm, this).cmp(&int_get(vm, o)) as i32);
    Ok(())
}
fn int_compare_to_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    int_compare_to(vm, ctx)
}
fn int_to_string_inst(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let h = vm.new_string(&int_get(vm, this).to_string())?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn boxed_int(vm: &mut Vm, v: i32) -> VmResult<Handle> {
    if (-128..=127).contains(&v) {
        if let Some(h) = vm.int_cache.get(&v) {
            return Ok(*h);
        }
    }
    let h = vm.builtin_instance(vm.c_integer)?;
    vm.native_roots.push(h);
    thr_set_field(vm, h, "value", v as u32);
    vm.native_roots.pop();
    if (-128..=127).contains(&v) {
        vm.int_cache.insert(v, h);
    }
    Ok(h)
}
fn int_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let h = boxed_int(vm, v)?;
    vm.native_roots.push(h);
    vm.ret_ref(ctx, h);
    vm.native_roots.pop();
    Ok(())
}
fn int_parse(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let text = vm.string_to_rust(s).unwrap_or_default();
    match format::parse_i32(&text, 10) {
        Ok(v) => vm.ret_i32(ctx, v),
        Err(_) => {
            let msg = format!("For input string: \"{text}\"");
            return Err(vm.throw_new("Ljava/lang/NumberFormatException;", Some(&msg)));
        }
    }
    Ok(())
}
fn int_parse_radix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let radix = vm.arg_i32(ctx, 1) as u32;
    let text = vm.string_to_rust(s).unwrap_or_default();
    match format::parse_i32(&text, radix) {
        Ok(v) => vm.ret_i32(ctx, v),
        Err(_) => return Err(vm.throw_new("Ljava/lang/NumberFormatException;", Some(&text))),
    }
    Ok(())
}
fn int_to_string_static(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let h = vm.new_string(&v.to_string())?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn int_to_string_radix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let r = vm.arg_i32(ctx, 1) as u32;
    let h = vm.new_string(&format::i32_to_radix(v, r))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn int_to_hex(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let h = vm.new_string(&format::i32_to_unsigned_string(v, 16))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn int_to_octal(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let h = vm.new_string(&format::i32_to_unsigned_string(v, 8))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn int_to_binary(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    let h = vm.new_string(&format::i32_to_unsigned_string(v, 2))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn int_bit_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_i32(ctx, 0).count_ones() as i32);
    Ok(())
}
fn int_nlz(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_i32(ctx, 0).leading_zeros() as i32);
    Ok(())
}
fn int_ntz(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_i32(ctx, 0).trailing_zeros() as i32);
    Ok(())
}
fn int_highest_one(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    vm.ret_i32(ctx, if v == 0 { 0 } else { 1i32 << (31 - v.leading_zeros()) });
    Ok(())
}
fn int_lowest_one(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    vm.ret_i32(ctx, v & v.wrapping_neg());
    Ok(())
}
fn int_signum(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_i32(ctx, 0).signum());
    Ok(())
}
fn int_rotate_left(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0) as u32;
    let d = vm.arg_i32(ctx, 1) as u32;
    vm.ret_i32(ctx, v.rotate_left(d & 31) as i32);
    Ok(())
}
fn int_rotate_right(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0) as u32;
    let d = vm.arg_i32(ctx, 1) as u32;
    vm.ret_i32(ctx, v.rotate_right(d & 31) as i32);
    Ok(())
}
fn int_compare_static(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i32(ctx, 0);
    let b = vm.arg_i32(ctx, 1);
    vm.ret_i32(ctx, a.cmp(&b) as i32);
    Ok(())
}

// --- Long ---
fn long_get(vm: &Vm, this: Handle) -> i64 {
    let off = thr_field_offset(vm, this, "value");
    match vm.heap.get(this) {
        Some(Object::Instance { fields, .. }) => {
            (fields[off] as u64 | ((fields[off + 1] as u64) << 32)) as i64
        }
        _ => 0,
    }
}
fn thr_field_offset(vm: &Vm, this: Handle, name: &str) -> usize {
    let class = vm.class_of_object(this).unwrap_or(0);
    let mut c = Some(class);
    while let Some(cur) = c {
        if let Some(f) = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == name)
        {
            return f.offset as usize;
        }
        c = vm.classes[cur as usize].super_class;
    }
    0
}
fn thr_set_field_long(vm: &mut Vm, this: Handle, name: &str, v: i64) {
    let off = thr_field_offset(vm, this, name);
    if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(this) {
        fields[off] = v as u32;
        fields[off + 1] = (v >> 32) as u32;
    }
}
fn long_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_i64(ctx, 0);
    thr_set_field_long(vm, this, "value", v);
    Ok(())
}
fn long_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i64(ctx, long_get(vm, this));
    Ok(())
}
fn long_int_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, long_get(vm, this) as i32);
    Ok(())
}
fn long_double_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_f64(ctx, long_get(vm, this) as f64);
    Ok(())
}
fn long_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let eq = vm.class_of_object(o) == Some(vm.c_long) && long_get(vm, o) == long_get(vm, this);
    vm.ret_bool(ctx, eq);
    Ok(())
}
fn long_hash(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = long_get(vm, this);
    vm.ret_i32(ctx, (v ^ (v >> 32)) as i32);
    Ok(())
}
fn long_compare_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    vm.ret_i32(ctx, long_get(vm, this).cmp(&long_get(vm, o)) as i32);
    Ok(())
}
fn long_compare_to_obj(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    long_compare_to(vm, ctx)
}
fn long_to_string_inst(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let h = vm.new_string(&long_get(vm, this).to_string())?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn boxed_long(vm: &mut Vm, v: i64) -> VmResult<Handle> {
    let h = vm.builtin_instance(vm.c_long)?;
    vm.native_roots.push(h);
    thr_set_field_long(vm, h, "value", v);
    vm.native_roots.pop();
    Ok(h)
}
fn long_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let h = boxed_long(vm, v)?;
    vm.native_roots.push(h);
    vm.ret_ref(ctx, h);
    vm.native_roots.pop();
    Ok(())
}
fn long_parse(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let text = vm.string_to_rust(s).unwrap_or_default();
    match format::parse_i64(&text, 10) {
        Ok(v) => vm.ret_i64(ctx, v),
        Err(_) => return Err(vm.throw_new("Ljava/lang/NumberFormatException;", Some(&text))),
    }
    Ok(())
}
fn long_to_string_static(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let h = vm.new_string(&v.to_string())?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn long_to_string_radix(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let r = vm.arg_i32(ctx, 1) as u32;
    let h = vm.new_string(&format::i64_to_radix(v, r))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn long_to_hex(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let h = vm.new_string(&format::i64_to_unsigned_string(v, 16))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn long_to_octal(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let h = vm.new_string(&format::i64_to_unsigned_string(v, 8))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn long_to_binary(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    let h = vm.new_string(&format::i64_to_unsigned_string(v, 2))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn long_bit_count(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_i64(ctx, 0).count_ones() as i32);
    Ok(())
}
fn long_highest_one(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    vm.ret_i64(ctx, if v == 0 { 0 } else { 1i64 << (63 - v.leading_zeros()) });
    Ok(())
}
fn long_lowest_one(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0);
    vm.ret_i64(ctx, v & v.wrapping_neg());
    Ok(())
}
fn long_signum(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_i64(ctx, 0).signum() as i32);
    Ok(())
}
fn long_rotate_left(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0) as u64;
    let d = vm.arg_i32(ctx, 1) as u32;
    vm.ret_i64(ctx, v.rotate_left(d & 63) as i64);
    Ok(())
}
fn long_rotate_right(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i64(ctx, 0) as u64;
    let d = vm.arg_i32(ctx, 1) as u32;
    vm.ret_i64(ctx, v.rotate_right(d & 63) as i64);
    Ok(())
}
fn long_compare_static(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_i64(ctx, 0);
    let b = vm.arg_i64(ctx, 1);
    vm.ret_i32(ctx, a.cmp(&b) as i32);
    Ok(())
}

// --- Character ---
fn char_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_u16(ctx, 0);
    thr_set_field(vm, this, "value", v as u32);
    Ok(())
}
fn char_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_u16(ctx, thr_get_field(vm, this, "value") as u16);
    Ok(())
}
fn char_is_digit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    vm.ret_bool(ctx, char::from_u32(c as u32).map(|c| c.is_ascii_digit()).unwrap_or(false));
    Ok(())
}
fn char_is_letter(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    vm.ret_bool(ctx, char::from_u32(c as u32).map(|c| c.is_alphabetic()).unwrap_or(false));
    Ok(())
}
fn char_is_ws(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    vm.ret_bool(ctx, char::from_u32(c as u32).map(|c| c.is_whitespace()).unwrap_or(false));
    Ok(())
}
fn char_is_upper(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    vm.ret_bool(ctx, char::from_u32(c as u32).map(|c| c.is_uppercase()).unwrap_or(false));
    Ok(())
}
fn char_is_lower(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    vm.ret_bool(ctx, char::from_u32(c as u32).map(|c| c.is_lowercase()).unwrap_or(false));
    Ok(())
}
fn char_to_upper(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    let mapped = char::from_u32(c as u32)
        .and_then(|ch| ch.to_uppercase().next())
        .map(|ch| ch as u32 as u16)
        .unwrap_or(c);
    vm.ret_u16(ctx, mapped);
    Ok(())
}
fn char_to_lower(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    let mapped = char::from_u32(c as u32)
        .and_then(|ch| ch.to_lowercase().next())
        .map(|ch| ch as u32 as u16)
        .unwrap_or(c);
    vm.ret_u16(ctx, mapped);
    Ok(())
}
fn char_numeric(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    let v = char::from_u32(c as u32)
        .and_then(|ch| ch.to_digit(36))
        .map(|d| d as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}
fn char_digit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    let radix = vm.arg_i32(ctx, 1) as u32;
    let v = char::from_u32(c as u32)
        .and_then(|ch| ch.to_digit(36))
        .filter(|d| *d < radix)
        .map(|d| d as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, v);
    Ok(())
}
fn char_for_digit(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let d = vm.arg_i32(ctx, 0);
    let radix = vm.arg_i32(ctx, 1) as u32;
    let c = if d < 0 || d >= radix as i32 || radix > 36 {
        0
    } else {
        crate::format::u32_to_radix(d as u32, radix).chars().next().unwrap_or('0') as u16
    };
    vm.ret_u16(ctx, c);
    Ok(())
}
fn char_hash_static(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, vm.arg_u16(ctx, 0) as i32);
    Ok(())
}
fn char_compare(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_u16(ctx, 0);
    let b = vm.arg_u16(ctx, 1);
    vm.ret_i32(ctx, a.cmp(&b) as i32);
    Ok(())
}
fn char_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let c = vm.arg_u16(ctx, 0);
    let class = vm.load_class("Ljava/lang/Character;")?;
    let h = vm.builtin_instance(class)?;
    vm.native_roots.push(h);
    thr_set_field(vm, h, "value", c as u32);
    vm.native_roots.pop();
    vm.ret_ref(ctx, h);
    Ok(())
}

// --- Boolean / Float / Double ---
fn bool_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_bool(ctx, 0);
    thr_set_field(vm, this, "value", v as u32);
    Ok(())
}
fn bool_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_bool(ctx, thr_get_field(vm, this, "value") != 0);
    Ok(())
}
fn bool_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_bool(ctx, 0);
    let class = vm.load_class("Ljava/lang/Boolean;")?;
    let h = vm.builtin_instance(class)?;
    vm.native_roots.push(h);
    thr_set_field(vm, h, "value", v as u32);
    vm.native_roots.pop();
    vm.ret_ref(ctx, h);
    Ok(())
}
fn bool_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = thr_get_field(vm, this, "value") != 0;
    let h = vm.intern_str(if v { "true" } else { "false" })?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn bool_equals(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let a = thr_get_field(vm, this, "value") != 0;
    let b = o != NULL && thr_get_field(vm, o, "value") != 0;
    vm.ret_bool(ctx, a == b && vm.class_of_object(o) == vm.class_of_object(this));
    Ok(())
}
fn float_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_f32(ctx, 0);
    thr_set_field(vm, this, "value", v.to_bits());
    Ok(())
}
fn float_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_f32(ctx, f32::from_bits(thr_get_field(vm, this, "value")));
    Ok(())
}
fn float_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f32(ctx, 0);
    let class = vm.load_class("Ljava/lang/Float;")?;
    let h = vm.builtin_instance(class)?;
    vm.native_roots.push(h);
    thr_set_field(vm, h, "value", v.to_bits());
    vm.native_roots.pop();
    vm.ret_ref(ctx, h);
    Ok(())
}
fn float_to_string_inst(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = f32::from_bits(thr_get_field(vm, this, "value"));
    let h = vm.new_string(&format::float_to_string(v))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
fn double_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = vm.arg_f64(ctx, 0);
    thr_set_field_long(vm, this, "value", v.to_bits() as i64);
    Ok(())
}
fn double_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let off = thr_field_offset(vm, this, "value");
    let bits = match vm.heap.get(this) {
        Some(Object::Instance { fields, .. }) => {
            (fields[off] as u64) | ((fields[off + 1] as u64) << 32)
        }
        _ => 0,
    };
    vm.ret_f64(ctx, f64::from_bits(bits));
    Ok(())
}
fn double_value_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_f64(ctx, 0);
    let class = vm.load_class("Ljava/lang/Double;")?;
    let h = vm.builtin_instance(class)?;
    vm.native_roots.push(h);
    thr_set_field_long(vm, h, "value", v.to_bits() as i64);
    vm.native_roots.pop();
    vm.ret_ref(ctx, h);
    Ok(())
}
fn double_to_string_inst(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let off = thr_field_offset(vm, this, "value");
    let bits = match vm.heap.get(this) {
        Some(Object::Instance { fields, .. }) => {
            (fields[off] as u64) | ((fields[off + 1] as u64) << 32)
        }
        _ => 0,
    };
    let h = vm.new_string(&format::double_to_string(f64::from_bits(bits)))?;
    vm.ret_ref(ctx, h);
    Ok(())
}
