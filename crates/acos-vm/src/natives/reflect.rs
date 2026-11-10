//! `java.lang.Class`, `java.lang.reflect.*` and `VMClassLoader` natives.
//!
//! These supply the reflection surface the console tier touches (class names,
//! primitive classes, interfaces, `newInstance`). Annotation queries return the
//! honest empty result (ACOS does not parse annotation metadata yet).

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::class::ClassId;
use crate::heap::{new_array_data, ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::{descriptor_to_dotted, InvokeKind, Vm};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let c = "Ljava/lang/Class;";
    vm.register_native(c, "getNameNative", "()Ljava/lang/String;", cl_get_name);
    vm.register_native(c, "isAssignableFrom", "(Ljava/lang/Class;)Z", cl_is_assignable_from);
    vm.register_native(c, "getPrimitiveClass", "(Ljava/lang/String;)Ljava/lang/Class;", cl_get_primitive);
    vm.register_native(c, "classForName", "(Ljava/lang/String;ZLjava/lang/ClassLoader;)Ljava/lang/Class;", cl_for_name);
    vm.register_native(c, "newInstance", "()Ljava/lang/Object;", cl_new_instance);
    vm.register_native(c, "isAnonymousClass", "()Z", cl_false);
    vm.register_native(c, "isRecord0", "()Z", cl_false);
    vm.register_native(c, "getDeclaringClass", "()Ljava/lang/Class;", cl_get_declaring_class);
    vm.register_native(c, "getEnclosingClass", "()Ljava/lang/Class;", cl_get_declaring_class);
    vm.register_native(c, "getEnclosingConstructorNative", "()Ljava/lang/reflect/Constructor;", cl_null);
    vm.register_native(c, "getEnclosingMethodNative", "()Ljava/lang/reflect/Method;", cl_null);
    vm.register_native(c, "getInterfacesInternal", "()[Ljava/lang/Class;", cl_interfaces);
    vm.register_native(c, "getDeclaredClasses", "()[Ljava/lang/Class;", cl_empty_class_array);
    vm.register_native(c, "getDeclaredFields", "()[Ljava/lang/reflect/Field;", cl_empty_field_array);
    vm.register_native(c, "getDeclaredFieldsUnchecked", "(Z)[Ljava/lang/reflect/Field;", cl_empty_field_array);
    vm.register_native(c, "getPublicDeclaredFields", "()[Ljava/lang/reflect/Field;", cl_empty_field_array);
    vm.register_native(c, "getDeclaredMethodsUnchecked", "(Z)[Ljava/lang/reflect/Method;", cl_empty_method_array);
    vm.register_native(c, "getDeclaredConstructorsInternal", "(Z)[Ljava/lang/reflect/Constructor;", cl_empty_ctor_array);
    vm.register_native(c, "getDeclaredMethodInternal", "(Ljava/lang/String;[Ljava/lang/Class;)Ljava/lang/reflect/Method;", cl_null);
    vm.register_native(c, "getDeclaredConstructorInternal", "([Ljava/lang/Class;)Ljava/lang/reflect/Constructor;", cl_null);
    vm.register_native(c, "getDeclaredField", "(Ljava/lang/String;)Ljava/lang/reflect/Field;", cl_get_declared_field);
    vm.register_native(c, "getPublicFieldRecursive", "(Ljava/lang/String;)Ljava/lang/reflect/Field;", cl_get_public_field_recursive);

    let f = "Ljava/lang/reflect/Field;";
    vm.register_native(f, "getNameInternal", "()Ljava/lang/String;", field_get_name);
    vm.register_native(f, "getArtField", "()J", field_get_art);
    vm.register_native(f, "get", "(Ljava/lang/Object;)Ljava/lang/Object;", field_get);
    vm.register_native(f, "getInt", "(Ljava/lang/Object;)I", field_get_int);
    vm.register_native(f, "getLong", "(Ljava/lang/Object;)J", field_get_long);
    vm.register_native(f, "getBoolean", "(Ljava/lang/Object;)Z", field_get_bool);
    vm.register_native(f, "getByte", "(Ljava/lang/Object;)B", field_get_byte);
    vm.register_native(f, "getChar", "(Ljava/lang/Object;)C", field_get_char);
    vm.register_native(f, "getShort", "(Ljava/lang/Object;)S", field_get_short);
    vm.register_native(f, "getFloat", "(Ljava/lang/Object;)F", field_get_float);
    vm.register_native(f, "getDouble", "(Ljava/lang/Object;)D", field_get_double);
    vm.register_native(f, "set", "(Ljava/lang/Object;Ljava/lang/Object;)V", field_set);
    vm.register_native(f, "setInt", "(Ljava/lang/Object;I)V", field_set_int);
    vm.register_native(f, "setLong", "(Ljava/lang/Object;J)V", field_set_long);
    vm.register_native(f, "setBoolean", "(Ljava/lang/Object;Z)V", field_set_int);
    vm.register_native(f, "setByte", "(Ljava/lang/Object;B)V", field_set_int);
    vm.register_native(f, "setChar", "(Ljava/lang/Object;C)V", field_set_int);
    vm.register_native(f, "setShort", "(Ljava/lang/Object;S)V", field_set_int);
    vm.register_native(f, "setFloat", "(Ljava/lang/Object;F)V", field_set_float);
    vm.register_native(f, "setDouble", "(Ljava/lang/Object;D)V", field_set_double);
    vm.register_native(c, "isDeclaredAnnotationPresent", "(Ljava/lang/Class;)Z", cl_false);
    vm.register_native(c, "getDeclaredAnnotation", "(Ljava/lang/Class;)Ljava/lang/annotation/Annotation;", cl_null);
    vm.register_native(c, "getDeclaredAnnotations", "()[Ljava/lang/annotation/Annotation;", cl_empty_annotation_array);
    vm.register_native(c, "getSignatureAnnotation", "()[Ljava/lang/String;", cl_null);
    vm.register_native(c, "getNestHostFromAnnotation", "()Ljava/lang/Class;", cl_null);
    vm.register_native(c, "getNestMembersFromAnnotation", "()[Ljava/lang/Class;", cl_null);
    vm.register_native(c, "getPermittedSubclassesFromAnnotation", "()[Ljava/lang/Class;", cl_null);
    vm.register_native(c, "getInnerClassName", "()Ljava/lang/String;", cl_get_inner_class_name);
    vm.register_native(c, "getInnerClassFlags", "(I)I", cl_inner_flags);
    vm.register_native(c, "ensureExtDataPresent", "()Ldalvik/system/ClassExt;", cl_null);
    vm.register_native(c, "getRecordAnnotationElement", "(Ljava/lang/String;Ljava/lang/Class;)[Ljava/lang/Object;", cl_null);

    let a = "Ljava/lang/reflect/Array;";
    vm.register_native(a, "createObjectArray", "(Ljava/lang/Class;I)Ljava/lang/Object;", arr_create_object);
    vm.register_native(a, "createMultiArray", "(Ljava/lang/Class;[I)Ljava/lang/Object;", arr_create_multi);

    let v = "Ljava/lang/VMClassLoader;";
    vm.register_native(v, "findLoadedClass", "(Ljava/lang/ClassLoader;Ljava/lang/String;)Ljava/lang/Class;", cl_for_name_loader);
    vm.register_native(
        v,
        "getBootClassPathEntries",
        "()[Ljava/lang/String;",
        vm_boot_classpath_entries,
    );

    vm.register_native(
        "Ljava/lang/Enum;",
        "enumValues",
        "(Ljava/lang/Class;)[Ljava/lang/Object;",
        enum_values,
    );
    Ok(())
}

/// `Enum.enumValues(Class)` — call the enum type's synthetic `values()` method.
fn enum_values(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let cid = match vm.class_id_of_class_object(class_obj) {
        Some(c) => c,
        None => {
            vm.ret_ref(ctx, NULL);
            return Ok(());
        }
    };
    let candidates = vm.classes[cid as usize].declared_methods.clone();
    let mid = candidates.into_iter().find(|&m| {
        let mm = &vm.methods[m as usize];
        mm.name == "values"
            && mm.params.is_empty()
            && vm.type_desc(mm.ret).starts_with('[')
    });
    match mid {
        Some(m) => {
            let r = crate::interp::call_method(vm, m, NULL, &[])?;
            vm.ret_ref(ctx, r as u32);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn represented(vm: &Vm, h: Handle) -> Option<ClassId> {
    vm.class_id_of_class_object(h)
}

fn cl_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = match represented(vm, this) {
        Some(id) => {
            let desc = vm.class_desc(id).to_string();
            class_binary_name(&desc)
        }
        None => "java.lang.Object".to_string(),
    };
    let h = vm.new_string(&name)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `Class.getName` binary name for a descriptor (arrays/pimitives included).
fn class_binary_name(desc: &str) -> String {
    if desc.starts_with('[') {
        return desc.replace('/', ".");
    }
    match desc {
        "Z" => "boolean".to_string(),
        "B" => "byte".to_string(),
        "C" => "char".to_string(),
        "S" => "short".to_string(),
        "I" => "int".to_string(),
        "J" => "long".to_string(),
        "F" => "float".to_string(),
        "D" => "double".to_string(),
        "V" => "void".to_string(),
        d => descriptor_to_dotted(d),
    }
}

fn cl_get_primitive(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name_h = vm.arg_ref(ctx, 0);
    let name = vm.string_to_rust(name_h).unwrap_or_default();
    let desc = match name.as_str() {
        "boolean" => "Z",
        "byte" => "B",
        "char" => "C",
        "short" => "S",
        "int" => "I",
        "long" => "J",
        "float" => "F",
        "double" => "D",
        "void" => "V",
        _ => "",
    };
    if desc.is_empty() {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    }
    let cid = vm.load_class(desc)?;
    let obj = vm.class_object(cid)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn cl_for_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name_h = vm.arg_ref(ctx, 0);
    let name = vm.string_to_rust(name_h).unwrap_or_default();
    let initialize = vm.arg_bool(ctx, 1);
    if let Some(cid) = resolve_class_name(vm, &name)? {
        // `Class.forName(name, initialize, loader)`: the boolean is meaningful,
        // so run `<clinit>` (ART does this before `readParcelableCreator`
        // reads a hidden class's `CREATOR`).
        if initialize {
            vm.initialize_class(cid)?;
        }
        let obj = vm.class_object(cid)?;
        vm.ret_ref(ctx, obj);
        Ok(())
    } else {
        Err(vm.throw_new(
            "Ljava/lang/ClassNotFoundException;",
            Some(&name),
        ))
    }
}

/// `Class.isAssignableFrom(Class)`.
///
/// Needed by `Parcel.readParcelableCreator`, which rejects a `Parcelable`
/// whose class does not report itself as implementing `Parcelable` (the real
/// method is native in ART).
fn cl_is_assignable_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    let to = vm.class_id_of_class_object(this);
    let from = vm.class_id_of_class_object(other);
    let ok = match (from, to) {
        (Some(f), Some(t)) => vm.is_assignable(f, t)?,
        _ => false,
    };
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn cl_for_name_loader(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name_h = vm.arg_ref(ctx, 1);
    let name = vm.string_to_rust(name_h).unwrap_or_default();
    match resolve_class_name(vm, &name)? {
        Some(cid) => {
            let obj = vm.class_object(cid)?;
            vm.ret_ref(ctx, obj);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

/// `VMClassLoader.getBootClassPathEntries()`.
///
/// `java.lang.BootClassLoader.loadClass` iterates the returned entries looking
/// for the class file. ACOS resolves boot classes through the VM's own loader,
/// so an empty list is correct: `loadClass` then falls back to the
/// `VMClassLoader.findLoadedClass` native, which searches the boot images.
fn vm_boot_classpath_entries(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm
        .new_array_empty("java.lang.String", 0)
        .map_err(|e| crate::error::VmError::Internal(format!("boot classpath entries: {e}")))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn resolve_class_name(vm: &mut Vm, name: &str) -> VmResult<Option<ClassId>> {
    let desc = if name.starts_with('[') {
        name.replace('.', "/")
    } else {
        format!("L{};", name.replace('.', "/"))
    };
    match vm.load_class(&desc) {
        Ok(c) => Ok(Some(c)),
        Err(VmError::UnsupportedApi { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

fn cl_new_instance(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cid = match represented(vm, this) {
        Some(c) => c,
        None => {
            return Err(vm.throw_new("Ljava/lang/InstantiationException;", None));
        }
    };
    let obj = vm.alloc_instance(cid)?;
    vm.native_roots.push(obj);
    let ctor = vm.find_method(cid, "<init>", "()V", InvokeKind::Direct);
    let result = match ctor {
        Ok(m) => crate::interp::call_method(vm, m, obj, &[]).map(|_| ()),
        Err(_) => Err(vm.throw_new("Ljava/lang/InstantiationException;", None)),
    };
    vm.native_roots.pop();
    result?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// The dotted name of the outer class of a nested class (`Main$A` -> `Main`).
fn outer_dotted(desc: &str) -> Option<String> {
    let dotted = descriptor_to_dotted(desc);
    let i = dotted.rfind('$')?;
    Some(dotted[..i].to_string())
}

fn cl_get_declaring_class(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let outer = represented(vm, this)
        .map(|id| vm.class_desc(id).to_string())
        .and_then(|d| outer_dotted(&d));
    if let Some(outer) = outer {
        let desc = format!("L{};", outer.replace('.', "/"));
        if let Ok(cid) = vm.load_class(&desc) {
            let obj = vm.class_object(cid)?;
            vm.ret_ref(ctx, obj);
            return Ok(());
        }
    }
    vm.ret_ref(ctx, NULL);
    Ok(())
}

fn cl_get_inner_class_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = represented(vm, this)
        .map(|id| descriptor_to_dotted(vm.class_desc(id)))
        .and_then(|d| d.rfind('$').map(|i| d[i + 1..].to_string()));
    match name {
        Some(n) => {
            let h = vm.new_string(&n)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn cl_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn cl_null(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, NULL);
    Ok(())
}

fn cl_interfaces(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let mut elems = Vec::new();
    if let Some(id) = represented(vm, this) {
        let ifaces = vm.classes[id as usize].interfaces.clone();
        for i in ifaces {
            let obj = vm.class_object(i)?;
            elems.push(obj);
        }
    }
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/Class;".to_string(),
        data: ArrayData::Ref(elems),
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn cl_inner_flags(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = vm.arg_i32(ctx, 0);
    vm.ret_i32(ctx, v);
    Ok(())
}

fn empty_ref_array(vm: &mut Vm, desc: &str) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: desc.to_string(),
        data: ArrayData::Ref(Vec::new()),
    }))
}

fn cl_empty_class_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = empty_ref_array(vm, "Ljava/lang/Class;")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn cl_empty_field_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = empty_ref_array(vm, "Ljava/lang/reflect/Field;")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn cl_empty_method_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = empty_ref_array(vm, "Ljava/lang/reflect/Method;")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn cl_empty_ctor_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = empty_ref_array(vm, "Ljava/lang/reflect/Constructor;")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn cl_empty_annotation_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = empty_ref_array(vm, "Ljava/lang/annotation/Annotation;")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn arr_create_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let len = vm.arg_i32(ctx, 1);
    if len < 0 {
        return Err(vm.throw_new(
            "Ljava/lang/NegativeArraySizeException;",
            Some(&len.to_string()),
        ));
    }
    let len = len as usize;
    let desc = vm
        .class_id_of_class_object(class_obj)
        .map(|c| vm.class_desc(c).to_string())
        .unwrap_or_else(|| "Ljava/lang/Object;".to_string());
    // Bound the guest length before the `vec![..; len]` backing store: the
    // reflection path must throw Java OutOfMemoryError, never abort the host
    // allocator (HONESTY_AUDIT.md #2).
    vm.check_array_len(&desc, len)?;
    let data = new_array_data(&desc, len)?;
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: desc,
        data,
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn arr_create_multi(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let dims = vm.arg_ref(ctx, 1);
    let desc = vm
        .class_id_of_class_object(class_obj)
        .map(|c| vm.class_desc(c).to_string())
        .unwrap_or_else(|| "Ljava/lang/Object;".to_string());
    let lens: Vec<i32> = match vm.heap.get(dims) {
        Some(Object::Array(ArrayObj { data: ArrayData::Int(v), .. })) => v.clone(),
        _ => Vec::new(),
    };
    // The JVM caps array dimensions at 255; a guest `int[] dims` must not drive
    // unbounded recursion in `build_multi`.
    if lens.len() > 255 {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("array dimensions exceed 255"),
        ));
    }
    let obj = build_multi(vm, &desc, &lens)?;
    vm.ret_ref(ctx, obj);
    Ok(())
}

/// Build a multi-dimensional array. `elem_desc` is the *leaf* component type
/// (`I`, `Ljava/lang/String;`, ...) and `dims` the dimension sizes from the
/// outermost inward.
fn build_multi(vm: &mut Vm, elem_desc: &str, dims: &[i32]) -> VmResult<Handle> {
    let raw = dims.first().copied().unwrap_or(0);
    if raw < 0 {
        return Err(vm.throw_new(
            "Ljava/lang/NegativeArraySizeException;",
            Some(&raw.to_string()),
        ));
    }
    let n = raw as usize;
    // Bound every dimension before allocating/recursing (HONESTY_AUDIT.md #2).
    vm.check_array_len(elem_desc, n)?;
    if dims.len() <= 1 {
        let data = new_array_data(elem_desc, n)?;
        return vm.alloc(Object::Array(ArrayObj {
            elem_desc: elem_desc.to_string(),
            data,
        }));
    }
    let mut elems = Vec::with_capacity(n);
    // Root the children across further allocations (GC-stress collects
    // anything reachable only from the host stack).
    let root_base = vm.native_roots.len();
    let result = (|| -> VmResult<Handle> {
        for _ in 0..n {
            let child = build_multi(vm, elem_desc, &dims[1..])?;
            vm.native_roots.push(child);
            elems.push(child);
        }
        let component = format!("{}{}", "[".repeat(dims.len() - 1), elem_desc);
        vm.alloc(Object::Array(ArrayObj {
            elem_desc: component,
            data: ArrayData::Ref(elems.clone()),
        }))
    })();
    vm.native_roots.truncate(root_base);
    result
}

// ---------------------------------------------------------------------------
// Reflection: Field construction and access
// ---------------------------------------------------------------------------

const FIELD_BASE: i32 = 8;

fn cl_get_declared_field(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let cid = represented(vm, this);
    if let Some(cid) = cid {
        if let Some((def, is_static, idx)) = find_field(vm, cid, &name) {
            let h = make_field(vm, cid, &def, is_static, idx)?;
            vm.ret_ref(ctx, h);
            return Ok(());
        }
    }
    Err(vm.throw_new("Ljava/lang/NoSuchFieldException;", Some(&name)))
}

fn cl_get_public_field_recursive(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let mut c = represented(vm, this);
    while let Some(cid) = c {
        if let Some((def, is_static, idx)) = find_field(vm, cid, &name) {
            let h = make_field(vm, cid, &def, is_static, idx)?;
            vm.ret_ref(ctx, h);
            return Ok(());
        }
        c = vm.classes[cid as usize].super_class;
    }
    Err(vm.throw_new("Ljava/lang/NoSuchFieldException;", Some(&name)))
}

fn find_field(
    vm: &Vm,
    cid: ClassId,
    name: &str,
) -> Option<(crate::class::FieldDef, bool, usize)> {
    let c = &vm.classes[cid as usize];
    if let Some((i, f)) = c
        .declared_instance_fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == name)
    {
        return Some((f.clone(), false, i));
    }
    if let Some((i, f)) = c
        .declared_static_fields
        .iter()
        .enumerate()
        .find(|(_, f)| f.name == name)
    {
        return Some((f.clone(), true, i));
    }
    None
}

fn make_field(
    vm: &mut Vm,
    cid: ClassId,
    def: &crate::class::FieldDef,
    is_static: bool,
    idx: usize,
) -> VmResult<Handle> {
    let field_class = vm.load_class("Ljava/lang/reflect/Field;")?;
    let obj = vm.alloc_instance(field_class)?;
    vm.native_roots.push(obj);
    let result = (|| -> VmResult<()> {
        let dc = vm.class_object(cid)?;
        vm.native_roots.push(dc);
        let type_cid = vm.load_class(&def.desc)?;
        let ty = vm.class_object(type_cid)?;
        vm.native_roots.push(ty);
        let offset = FIELD_BASE + def.offset as i32 * 4;
        vm.set_instance_field_by_name(obj, "declaringClass", dc);
        vm.set_instance_field_by_name(obj, "type", ty);
        vm.set_instance_field_by_name(obj, "accessFlags", def.access);
        vm.set_instance_field_by_name(obj, "artFieldIndex", idx as u32);
        vm.set_instance_field_by_name(obj, "offset", offset as u32);
        let _ = is_static;
        vm.native_roots.pop();
        vm.native_roots.pop();
        Ok(())
    })();
    vm.native_roots.pop();
    result?;
    Ok(obj)
}

fn field_name_of(vm: &Vm, this: Handle) -> Option<String> {
    let dc = vm.get_instance_field_by_name(this, "declaringClass")?;
    let cid = vm.class_id_of_class_object(dc)?;
    let idx = vm.get_instance_field_by_name(this, "artFieldIndex").unwrap_or(0) as usize;
    let access = vm.get_instance_field_by_name(this, "accessFlags").unwrap_or(0);
    let c = &vm.classes[cid as usize];
    if access & acos_dex::access::STATIC != 0 {
        c.declared_static_fields.get(idx).map(|f| f.name.clone())
    } else {
        c.declared_instance_fields.get(idx).map(|f| f.name.clone())
    }
}

fn field_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    match field_name_of(vm, this) {
        Some(n) => {
            let h = vm.new_string(&n)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn field_get_art(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let off = vm.get_instance_field_by_name(this, "offset").unwrap_or(0);
    vm.ret_i64(ctx, off as i64);
    Ok(())
}

/// Resolve a `Field` mirror to `(is_static, slot)`.
///
/// `artFieldIndex` is the **declaration index** (used by `getNameInternal` and
/// `getDeclaredFields`), while the storage slot is `FieldDef::offset`: a
/// `long`/`double` occupies two slots, so indexing `Vm::statics` (or an
/// object's field array) by the declaration index reads the wrong slot as soon
/// as a wide field precedes the target. That is the `Settings.Global` NPE in
/// the M2 chain: `Field.get` returned a value from the wrong static slot.
fn field_slot(vm: &Vm, this: Handle) -> (bool, usize) {
    let access = vm.get_instance_field_by_name(this, "accessFlags").unwrap_or(0);
    let is_static = access & acos_dex::access::STATIC != 0;
    let idx = vm.get_instance_field_by_name(this, "artFieldIndex").unwrap_or(0) as usize;
    if let Some(cid) = vm
        .get_instance_field_by_name(this, "declaringClass")
        .and_then(|dc| vm.class_id_of_class_object(dc))
    {
        let c = &vm.classes[cid as usize];
        let def = if is_static {
            c.declared_static_fields.get(idx)
        } else {
            c.declared_instance_fields.get(idx)
        };
        if let Some(d) = def {
            return (is_static, d.offset as usize);
        }
    }
    // Fall back to the encoded byte offset (instance-field encoding).
    let off = vm.get_instance_field_by_name(this, "offset").unwrap_or(0) as i32;
    (is_static, ((off - FIELD_BASE) / 4).max(0) as usize)
}

fn read_field(vm: &mut Vm, this: Handle, target: Handle) -> VmResult<u32> {
    let (is_static, slot) = field_slot(vm, this);
    if is_static {
        if let Some(cid) = vm
            .get_instance_field_by_name(this, "declaringClass")
            .and_then(|dc| vm.class_id_of_class_object(dc))
        {
            // JLS 12.4.1 / ART: reading a static field initialises the
            // declaring class first. Without this, a later compiled
            // `sget-object` runs `<clinit>` and overwrites a value written
            // through `Field.set` (`SoLoader.sSoSources`, W88-A blocker 3).
            vm.initialize_class(cid)?;
            return Ok(vm.classes[cid as usize].statics.get(slot).copied().unwrap_or(0));
        }
        return Ok(0);
    }
    Ok(match vm.heap.get(target) {
        Some(Object::Instance { fields, .. }) => fields.get(slot).copied().unwrap_or(0),
        _ => 0,
    })
}

/// Read the high 32 bits of a wide (`long`/`double`) field: the adjacent slot.
fn read_wide_hi(vm: &Vm, this: Handle, target: Handle) -> u64 {
    let (is_static, slot) = field_slot(vm, this);
    if is_static {
        vm.get_instance_field_by_name(this, "declaringClass")
            .and_then(|dc| vm.class_id_of_class_object(dc))
            .and_then(|cid| vm.classes[cid as usize].statics.get(slot + 1).copied())
            .unwrap_or(0) as u64
    } else {
        match vm.heap.get(target) {
            Some(Object::Instance { fields, .. }) => {
                fields.get(slot + 1).copied().unwrap_or(0) as u64
            }
            _ => 0,
        }
    }
}

fn write_field(vm: &mut Vm, this: Handle, target: Handle, v: u32) -> VmResult<()> {
    let (is_static, slot) = field_slot(vm, this);
    if is_static {
        if let Some(cid) = vm
            .get_instance_field_by_name(this, "declaringClass")
            .and_then(|dc| vm.class_id_of_class_object(dc))
        {
            // JLS 12.4.1 / ART: writing a static field initialises the class
            // first, so `<clinit>` cannot clobber the write afterwards.
            vm.initialize_class(cid)?;
            if slot < vm.classes[cid as usize].statics.len() {
                vm.classes[cid as usize].statics[slot] = v;
            }
        }
        return Ok(());
    }
    if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(target) {
        if slot < fields.len() {
            fields[slot] = v;
        }
    }
    Ok(())
}

/// Write the high 32 bits of a wide (`long`/`double`) field.
fn write_wide_hi(vm: &mut Vm, this: Handle, target: Handle, hi: u32) {
    let (is_static, slot) = field_slot(vm, this);
    if is_static {
        if let Some(cid) = vm
            .get_instance_field_by_name(this, "declaringClass")
            .and_then(|dc| vm.class_id_of_class_object(dc))
        {
            if slot + 1 < vm.classes[cid as usize].statics.len() {
                vm.classes[cid as usize].statics[slot + 1] = hi;
            }
        }
        return;
    }
    if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(target) {
        if slot + 1 < fields.len() {
            fields[slot + 1] = hi;
        }
    }
}

/// Descriptor of a field's declared type (`I`, `[B`, `Ljava/lang/String;`).
fn field_type_desc(vm: &Vm, this: Handle) -> Option<String> {
    let ty = vm.get_instance_field_by_name(this, "type")?;
    let cid = vm.class_id_of_class_object(ty)?;
    // The raw descriptor, not the dotted Java name.
    Some(vm.class_desc(cid).to_string())
}

fn read_wide(vm: &mut Vm, this: Handle, target: Handle) -> VmResult<u64> {
    Ok((read_field(vm, this, target)? as u64) | (read_wide_hi(vm, this, target) << 32))
}

/// Box a primitive field value exactly as ART's native `Field.get` does.
fn box_primitive(vm: &mut Vm, kind: u8, raw: u64) -> VmResult<Handle> {
    let (class, sig, arg) = match kind {
        b'Z' => ("Ljava/lang/Boolean;", "(Z)Ljava/lang/Boolean;", raw & 1),
        b'B' => (
            "Ljava/lang/Byte;",
            "(B)Ljava/lang/Byte;",
            (raw as u8 as i8 as i64) as u64,
        ),
        b'C' => (
            "Ljava/lang/Character;",
            "(C)Ljava/lang/Character;",
            raw & 0xffff,
        ),
        b'S' => (
            "Ljava/lang/Short;",
            "(S)Ljava/lang/Short;",
            (raw as u16 as i16 as i64) as u64,
        ),
        b'I' => (
            "Ljava/lang/Integer;",
            "(I)Ljava/lang/Integer;",
            (raw as u32 as i32 as i64) as u64,
        ),
        b'J' => ("Ljava/lang/Long;", "(J)Ljava/lang/Long;", raw),
        b'F' => ("Ljava/lang/Float;", "(F)Ljava/lang/Float;", raw),
        b'D' => ("Ljava/lang/Double;", "(D)Ljava/lang/Double;", raw),
        _ => {
            return Err(vm.throw_new(
                "Ljava/lang/InternalError;",
                Some("Field.get on unknown primitive"),
            ))
        }
    };
    let cid = vm.load_class(class)?;
    let mid = vm.find_method(cid, "valueOf", sig, InvokeKind::Static)?;
    Ok(crate::interp::call_method(vm, mid, NULL, &[arg])? as u32)
}

/// Unbox the argument of `Field.set` for a primitive field.
fn unbox_primitive(vm: &mut Vm, kind: u8, obj: Handle) -> VmResult<u64> {
    if obj == NULL {
        return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
    }
    let (method, sig) = match kind {
        b'Z' => ("booleanValue", "()Z"),
        b'B' => ("byteValue", "()B"),
        b'C' => ("charValue", "()C"),
        b'S' => ("shortValue", "()S"),
        b'I' => ("intValue", "()I"),
        b'J' => ("longValue", "()J"),
        b'F' => ("floatValue", "()F"),
        b'D' => ("doubleValue", "()D"),
        _ => {
            return Err(vm.throw_new(
                "Ljava/lang/InternalError;",
                Some("Field.set on unknown primitive"),
            ))
        }
    };
    let class = vm
        .class_of_object(obj)
        .ok_or_else(|| vm.throw_new("Ljava/lang/IllegalArgumentException;", None))?;
    let mid = vm.find_method(class, method, sig, InvokeKind::Virtual)?;
    crate::interp::call_method(vm, mid, obj, &[])
}

fn field_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let desc = field_type_desc(vm, this).unwrap_or_default();
    let h = match desc.as_bytes().first().copied() {
        Some(b'L') | Some(b'[') | None => read_field(vm, this, target)?,
        Some(b'J') | Some(b'D') => {
            let raw = read_wide(vm, this, target)?;
            box_primitive(vm, desc.as_bytes()[0], raw)?
        }
        Some(c) => {
            let raw = read_field(vm, this, target)? as u64;
            box_primitive(vm, c, raw)?
        }
    };
    vm.ret_ref(ctx, h);
    Ok(())
}

fn field_get_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = read_field(vm, this, target)?;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn field_get_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let lo = read_field(vm, this, target)? as u64;
    // Wide fields are stored in two adjacent slots.
    let hi = read_wide_hi(vm, this, target);
    vm.ret_i64(ctx, (lo | (hi << 32)) as i64);
    Ok(())
}

fn field_get_bool(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = read_field(vm, this, target)?;
    vm.ret_bool(ctx, v != 0);
    Ok(())
}

fn field_get_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = read_field(vm, this, target)?;
    vm.ret_i32(ctx, v as i8 as i32);
    Ok(())
}

fn field_get_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = read_field(vm, this, target)?;
    vm.ret_i32(ctx, v as u16 as i32);
    Ok(())
}

fn field_get_short(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = read_field(vm, this, target)?;
    vm.ret_i32(ctx, v as i16 as i32);
    Ok(())
}

fn field_get_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = read_field(vm, this, target)?;
    vm.ret_f32(ctx, f32::from_bits(v));
    Ok(())
}

fn field_get_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let lo = read_field(vm, this, target)? as u64;
    let hi = read_wide_hi(vm, this, target);
    vm.ret_f64(ctx, f64::from_bits(lo | (hi << 32)));
    Ok(())
}

fn field_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let value = vm.arg_ref(ctx, 1);
    let desc = field_type_desc(vm, this).unwrap_or_default();
    match desc.as_bytes().first().copied() {
        Some(b'L') | Some(b'[') | None => write_field(vm, this, target, value)?,
        Some(c) => {
            let raw = unbox_primitive(vm, c, value)?;
            if c == b'J' || c == b'D' {
                write_field(vm, this, target, raw as u32)?;
                write_wide_hi(vm, this, target, (raw >> 32) as u32);
            } else {
                write_field(vm, this, target, raw as u32)?;
            }
        }
    }
    Ok(())
}

fn field_set_int(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = vm.arg_i32(ctx, 1);
    write_field(vm, this, target, v as u32)?;
    Ok(())
}

fn field_set_long(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = vm.arg_i64(ctx, 1) as u64;
    write_field(vm, this, target, v as u32)?;
    write_wide_hi(vm, this, target, (v >> 32) as u32);
    Ok(())
}

fn field_set_float(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = vm.arg_f32(ctx, 1);
    write_field(vm, this, target, v.to_bits())?;
    Ok(())
}

fn field_set_double(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    let v = vm.arg_f64(ctx, 1).to_bits();
    write_field(vm, this, target, v as u32)?;
    write_wide_hi(vm, this, target, (v >> 32) as u32);
    Ok(())
}
