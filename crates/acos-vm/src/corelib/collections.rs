//! `java.util` collection builtins (the small subset the corpus needs).

use acos_dex::access;

use crate::class::NativeCtx;
use crate::corelib::{lang, Vm};
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, BuiltinData, Handle, Object, NULL};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    install_iterator(vm)?;
    install_array_list(vm)?;
    install_arrays(vm)?;
    install_collections(vm)?;
    install_maps(vm)?;
    install_enum_set(vm)?;
    install_objects(vm)?;
    install_reflect_array(vm)?;
    Ok(())
}

fn install_iterator(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/util/ArrayList$Itr;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &["Ljava/util/Iterator;"],
    )?;
    vm.add_native(id, "<init>", "()V", access::PUBLIC, itr_init);
    vm.add_native(id, "hasNext", "()Z", access::PUBLIC, itr_has_next);
    vm.add_native(id, "next", "()Ljava/lang/Object;", access::PUBLIC, itr_next);
    vm.link_class(id)?;
    Ok(())
}

fn itr_init(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}
fn itr_has_next(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::Iterator { items, index },
            ..
        }) => *index < items.len(),
        _ => false,
    };
    vm.ret_bool(ctx, v);
    Ok(())
}
fn itr_next(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let mut out = NULL;
    if let Some(Object::Builtin {
        data: BuiltinData::Iterator { items, index },
        ..
    }) = vm.heap.get_mut(this)
    {
        if *index < items.len() {
            out = items[*index];
            *index += 1;
        }
    }
    if out == NULL {
        return Err(vm.throw_new("Ljava/util/NoSuchElementException;", None));
    }
    vm.ret_ref(ctx, out);
    Ok(())
}

fn make_iterator(vm: &mut Vm, items: Vec<Handle>) -> VmResult<Handle> {
    let class = vm.load_class("Ljava/util/ArrayList$Itr;")?;
    vm.alloc(Object::Builtin {
        class,
        data: BuiltinData::Iterator { items, index: 0 },
    })
}

fn install_array_list(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/util/ArrayList;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &["Ljava/util/List;"],
    )?;
    vm.builtin_alloc.insert(id, crate::corelib::BuiltinAlloc::List);
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "()V", p, al_init);
    vm.add_native(id, "<init>", "(Ljava/util/Collection;)V", p, al_init_collection);
    vm.add_native(id, "add", "(Ljava/lang/Object;)Z", p, al_add);
    vm.add_native(id, "get", "(I)Ljava/lang/Object;", p, al_get);
    vm.add_native(id, "size", "()I", p, al_size);
    vm.add_native(id, "isEmpty", "()Z", p, al_is_empty);
    vm.add_native(id, "contains", "(Ljava/lang/Object;)Z", p, al_contains);
    vm.add_native(id, "indexOf", "(Ljava/lang/Object;)I", p, al_index_of);
    vm.add_native(id, "lastIndexOf", "(Ljava/lang/Object;)I", p, al_last_index_of);
    vm.add_native(id, "remove", "(Ljava/lang/Object;)Z", p, al_remove);
    vm.add_native(id, "iterator", "()Ljava/util/Iterator;", p, al_iterator);
    vm.add_native(id, "toArray", "()[Ljava/lang/Object;", p, al_to_array);
    vm.add_native(id, "toString", "()Ljava/lang/String;", p, al_to_string);
    vm.link_class(id)?;
    Ok(())
}

fn list_items(vm: &Vm, this: Handle) -> Vec<Handle> {
    match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::List(v),
            ..
        }) => v.clone(),
        _ => Vec::new(),
    }
}

fn al_init(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn al_init_collection(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let src = vm.arg_ref(ctx, 0);
    let items = list_items(vm, src);
    if let Some(Object::Builtin {
        data: BuiltinData::List(v),
        ..
    }) = vm.heap.get_mut(this)
    {
        *v = items;
    }
    Ok(())
}

fn al_add(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    if let Some(Object::Builtin {
        data: BuiltinData::List(v),
        ..
    }) = vm.heap.get_mut(this)
    {
        v.push(o);
    }
    vm.ret_bool(ctx, true);
    Ok(())
}

fn al_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let i = vm.arg_i32(ctx, 0);
    let items = list_items(vm, this);
    if i < 0 || i as usize >= items.len() {
        return Err(vm.throw_new("Ljava/lang/IndexOutOfBoundsException;", Some(&format!("Index: {i}, Size: {}", items.len()))));
    }
    vm.ret_ref(ctx, items[i as usize]);
    Ok(())
}

fn al_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, list_items(vm, this).len() as i32);
    Ok(())
}

fn al_is_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_bool(ctx, list_items(vm, this).is_empty());
    Ok(())
}

fn al_contains(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let items = list_items(vm, this);
    for it in items {
        if vm.value_equals(it, o)? {
            vm.ret_bool(ctx, true);
            return Ok(());
        }
    }
    vm.ret_bool(ctx, false);
    Ok(())
}

fn al_index_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let items = list_items(vm, this);
    for (i, it) in items.iter().enumerate() {
        if vm.value_equals(*it, o)? {
            vm.ret_i32(ctx, i as i32);
            return Ok(());
        }
    }
    vm.ret_i32(ctx, -1);
    Ok(())
}

fn al_last_index_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let items = list_items(vm, this);
    let mut found = -1;
    for (i, it) in items.iter().enumerate() {
        if vm.value_equals(*it, o)? {
            found = i as i32;
        }
    }
    vm.ret_i32(ctx, found);
    Ok(())
}

fn al_remove(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let items = list_items(vm, this);
    let mut idx = None;
    for (i, it) in items.iter().enumerate() {
        if vm.value_equals(*it, o)? {
            idx = Some(i);
            break;
        }
    }
    if let Some(i) = idx {
        if let Some(Object::Builtin {
            data: BuiltinData::List(v),
            ..
        }) = vm.heap.get_mut(this)
        {
            v.remove(i);
        }
        vm.ret_bool(ctx, true);
    } else {
        vm.ret_bool(ctx, false);
    }
    Ok(())
}

fn al_iterator(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let items = list_items(vm, this);
    let it = make_iterator(vm, items)?;
    vm.native_roots.push(it);
    vm.ret_ref(ctx, it);
    vm.native_roots.pop();
    Ok(())
}

fn al_to_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let items = list_items(vm, this);
    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/Object;".to_string(),
        data: ArrayData::Ref(items),
    }))?;
    vm.native_roots.push(arr);
    vm.ret_ref(ctx, arr);
    vm.native_roots.pop();
    Ok(())
}

fn al_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let items = list_items(vm, this);
    let s = render_collection(vm, &items)?;
    vm.ret_ref(ctx, s);
    Ok(())
}

fn render_collection(vm: &mut Vm, items: &[Handle]) -> VmResult<Handle> {
    vm.native_roots.extend(items.iter().copied());
    let mut out = String::from("[");
    for (i, it) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        if *it == NULL {
            out.push_str("null");
        } else {
            let s = lang::call_to_string(vm, *it)?;
            out.push_str(&vm.string_to_rust(s).unwrap_or_default());
        }
    }
    out.push(']');
    let h = vm.new_string(&out)?;
    for _ in items {
        vm.native_roots.pop();
    }
    Ok(h)
}

fn install_arrays(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/util/Arrays;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    let s = access::PUBLIC | access::STATIC;
    vm.add_native(id, "asList", "([Ljava/lang/Object;)Ljava/util/List;", s, arrays_as_list);
    vm.add_native(id, "toString", "([Ljava/lang/Object;)Ljava/lang/String;", s, arrays_to_string);
    vm.link_class(id)?;
    Ok(())
}

fn arrays_as_list(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 0);
    let items = match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Ref(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let class = vm.load_class("Ljava/util/ArrayList;")?;
    let h = vm.alloc(Object::Builtin {
        class,
        data: BuiltinData::List(items),
    })?;
    vm.native_roots.push(h);
    vm.ret_ref(ctx, h);
    vm.native_roots.pop();
    Ok(())
}

fn arrays_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let arr = vm.arg_ref(ctx, 0);
    if arr == NULL {
        let h = vm.intern_str("null")?;
        vm.ret_ref(ctx, h);
        return Ok(());
    }
    let items = match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Ref(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let s = render_collection(vm, &items)?;
    vm.ret_ref(ctx, s);
    Ok(())
}

fn install_collections(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/util/Collections;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    let s = access::PUBLIC | access::STATIC;
    vm.add_native(id, "sort", "(Ljava/util/List;)V", s, collections_sort);
    vm.link_class(id)?;
    Ok(())
}

fn collections_sort(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let list = vm.arg_ref(ctx, 0);
    let items = list_items(vm, list);
    vm.native_roots.push(list);
    vm.native_roots.extend(items.iter().copied());
    let mut sorted = items.clone();
    // Insertion sort using `compareTo` (stable and easy to propagate errors).
    let mut i = 1;
    while i < sorted.len() {
        let key = sorted[i];
        let mut j = i;
        while j > 0 && vm.compare_objects(sorted[j - 1], key)? > 0 {
            sorted[j] = sorted[j - 1];
            j -= 1;
        }
        sorted[j] = key;
        i += 1;
    }
    if let Some(Object::Builtin {
        data: BuiltinData::List(v),
        ..
    }) = vm.heap.get_mut(list)
    {
        *v = sorted;
    }
    for _ in 0..items.len() + 1 {
        vm.native_roots.pop();
    }
    Ok(())
}

fn install_maps(vm: &mut Vm) -> VmResult<()> {
    // Map$Entry implementation.
    let node = vm.def_class(
        "Ljava/util/HashMap$Node;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &["Ljava/util/Map$Entry;"],
    )?;
    vm.add_native(node, "getKey", "()Ljava/lang/Object;", access::PUBLIC, entry_get_key);
    vm.add_native(node, "getValue", "()Ljava/lang/Object;", access::PUBLIC, entry_get_value);
    vm.add_native(node, "toString", "()Ljava/lang/String;", access::PUBLIC, entry_to_string);
    vm.link_class(node)?;

    let hash = vm.def_class(
        "Ljava/util/HashMap;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &["Ljava/util/Map;"],
    )?;
    vm.builtin_alloc.insert(hash, crate::corelib::BuiltinAlloc::Map);
    let tree = vm.def_class(
        "Ljava/util/TreeMap;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &["Ljava/util/Map;"],
    )?;
    vm.builtin_alloc.insert(tree, crate::corelib::BuiltinAlloc::SortedMap);

    for id in [hash, tree] {
        let p = access::PUBLIC;
        vm.add_native(id, "<init>", "()V", p, map_init);
        vm.add_native(id, "put", "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;", p, map_put);
        vm.add_native(id, "get", "(Ljava/lang/Object;)Ljava/lang/Object;", p, map_get);
        vm.add_native(id, "containsKey", "(Ljava/lang/Object;)Z", p, map_contains_key);
        vm.add_native(id, "size", "()I", p, map_size);
        vm.add_native(id, "toString", "()Ljava/lang/String;", p, map_to_string);
        vm.add_native(id, "keySet", "()Ljava/util/Set;", p, map_key_set);
        vm.add_native(id, "entrySet", "()Ljava/util/Set;", p, map_entry_set);
        vm.link_class(id)?;
    }
    Ok(())
}

fn map_entries(vm: &Vm, this: Handle) -> Vec<(Handle, Handle)> {
    match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::HashMap(v) | BuiltinData::TreeMap(v),
            ..
        }) => v.clone(),
        _ => Vec::new(),
    }
}

fn map_set_entries(vm: &mut Vm, this: Handle, entries: Vec<(Handle, Handle)>) {
    if let Some(Object::Builtin {
        data: BuiltinData::HashMap(v) | BuiltinData::TreeMap(v),
        ..
    }) = vm.heap.get_mut(this)
    {
        *v = entries;
    }
}

fn map_init(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn map_put(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let k = vm.arg_ref(ctx, 0);
    let val = vm.arg_ref(ctx, 1);
    let mut entries = map_entries(vm, this);
    let sorted = matches!(
        vm.heap.get(this),
        Some(Object::Builtin {
            data: BuiltinData::TreeMap(_),
            ..
        })
    );
    let mut prev = NULL;
    let mut found = false;
    for (ek, ev) in entries.iter_mut() {
        if vm.value_equals(*ek, k)? {
            prev = *ev;
            *ev = val;
            found = true;
            break;
        }
    }
    if !found {
        entries.push((k, val));
    }
    if sorted {
        // Keep keys ordered by natural order.
        let mut i = 1;
        while i < entries.len() {
            let key = entries[i];
            let mut j = i;
            while j > 0 && vm.compare_objects(entries[j - 1].0, key.0)? > 0 {
                entries[j] = entries[j - 1];
                j -= 1;
            }
            entries[j] = key;
            i += 1;
        }
    }
    map_set_entries(vm, this, entries);
    vm.ret_ref(ctx, prev);
    Ok(())
}

fn map_get(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let k = vm.arg_ref(ctx, 0);
    let entries = map_entries(vm, this);
    for (ek, ev) in entries {
        if vm.value_equals(ek, k)? {
            vm.ret_ref(ctx, ev);
            return Ok(());
        }
    }
    vm.ret_ref(ctx, NULL);
    Ok(())
}

fn map_contains_key(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let k = vm.arg_ref(ctx, 0);
    let entries = map_entries(vm, this);
    for (ek, _) in entries {
        if vm.value_equals(ek, k)? {
            vm.ret_bool(ctx, true);
            return Ok(());
        }
    }
    vm.ret_bool(ctx, false);
    Ok(())
}

fn map_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.ret_i32(ctx, map_entries(vm, this).len() as i32);
    Ok(())
}

fn map_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let entries = map_entries(vm, this);
    vm.native_roots.push(this);
    let mut out = String::from("{");
    for (i, (k, v)) in entries.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&string_of(vm, *k)?);
        out.push('=');
        out.push_str(&string_of(vm, *v)?);
    }
    out.push('}');
    let h = vm.new_string(&out)?;
    vm.native_roots.pop();
    vm.ret_ref(ctx, h);
    Ok(())
}

fn string_of(vm: &mut Vm, h: Handle) -> VmResult<String> {
    if h == NULL {
        return Ok("null".to_string());
    }
    if let Some(s) = vm.string_to_rust(h) {
        return Ok(s);
    }
    let s = lang::call_to_string(vm, h)?;
    Ok(vm.string_to_rust(s).unwrap_or_default())
}

fn map_key_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let entries = map_entries(vm, this);
    let keys: Vec<Handle> = entries.into_iter().map(|(k, _)| k).collect();
    let class = vm.load_class("Ljava/util/ArrayList;")?;
    let h = vm.alloc(Object::Builtin {
        class,
        data: BuiltinData::List(keys),
    })?;
    vm.native_roots.push(h);
    vm.ret_ref(ctx, h);
    vm.native_roots.pop();
    Ok(())
}

fn map_entry_set(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let entries = map_entries(vm, this);
    let node = vm.load_class("Ljava/util/HashMap$Node;")?;
    vm.native_roots.push(this);
    let mut nodes = Vec::new();
    for (k, v) in entries {
        let n = vm.alloc(Object::Builtin {
            class: node,
            data: BuiltinData::Entry { key: k, value: v },
        })?;
        vm.native_roots.push(n);
        nodes.push(n);
    }
    let class = vm.load_class("Ljava/util/ArrayList;")?;
    let h = vm.alloc(Object::Builtin {
        class,
        data: BuiltinData::List(nodes),
    })?;
    vm.native_roots.push(h);
    while vm.native_roots.last() != Some(&this) {
        vm.native_roots.pop();
    }
    vm.native_roots.pop();
    vm.ret_ref(ctx, h);
    Ok(())
}

fn entry_get_key(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let k = match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::Entry { key, .. },
            ..
        }) => *key,
        _ => NULL,
    };
    vm.ret_ref(ctx, k);
    Ok(())
}

fn entry_get_value(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let v = match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::Entry { value, .. },
            ..
        }) => *value,
        _ => NULL,
    };
    vm.ret_ref(ctx, v);
    Ok(())
}

fn entry_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let (k, v) = match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::Entry { key, value },
            ..
        }) => (*key, *value),
        _ => (NULL, NULL),
    };
    let s = format!("{}={}", string_of(vm, k)?, string_of(vm, v)?);
    let h = vm.new_string(&s)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn install_enum_set(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/util/EnumSet;",
        access::PUBLIC | access::ABSTRACT,
        Some("Ljava/lang/Object;"),
        &["Ljava/util/Set;"],
    )?;
    vm.builtin_alloc.insert(id, crate::corelib::BuiltinAlloc::EnumSet);
    // Concrete implementation class.
    let impl_id = vm.def_class(
        "Ljava/util/RegularEnumSet;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/util/EnumSet;"),
        &[],
    )?;
    vm.builtin_alloc.insert(impl_id, crate::corelib::BuiltinAlloc::EnumSet);
    let s = access::PUBLIC | access::STATIC;
    vm.add_native(id, "of", "(Ljava/lang/Enum;Ljava/lang/Enum;)Ljava/util/EnumSet;", s, enumset_of);
    let p = access::PUBLIC;
    vm.add_native(impl_id, "contains", "(Ljava/lang/Object;)Z", p, enumset_contains);
    vm.add_native(impl_id, "size", "()I", p, enumset_size);
    vm.add_native(impl_id, "toString", "()Ljava/lang/String;", p, enumset_to_string);
    vm.add_native(impl_id, "iterator", "()Ljava/util/Iterator;", p, enumset_iterator);
    vm.link_class(id)?;
    vm.link_class(impl_id)?;
    Ok(())
}

fn enumset_parts(vm: &Vm, this: Handle) -> (u32, Vec<Handle>) {
    match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::EnumSet { enum_class, elements },
            ..
        }) => (*enum_class, elements.clone()),
        _ => (0, Vec::new()),
    }
}

fn enumset_of(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let a = vm.arg_ref(ctx, 0);
    let b = vm.arg_ref(ctx, 1);
    let cls = if a != NULL {
        vm.class_of_object(a).unwrap_or(0)
    } else {
        vm.class_of_object(b).unwrap_or(0)
    };
    let impl_id = vm.load_class("Ljava/util/RegularEnumSet;")?;
    let h = vm.alloc(Object::Builtin {
        class: impl_id,
        data: BuiltinData::EnumSet {
            enum_class: cls,
            elements: vec![a, b],
        },
    })?;
    vm.native_roots.push(h);
    vm.ret_ref(ctx, h);
    vm.native_roots.pop();
    Ok(())
}

fn enumset_contains(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let o = vm.arg_ref(ctx, 0);
    let (_, elems) = enumset_parts(vm, this);
    let r = elems.iter().any(|e| {
        vm.value_equals(*e, o).unwrap_or(false) || (o != NULL && *e == o)
    });
    vm.ret_bool(ctx, r);
    Ok(())
}

fn enumset_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let (_, elems) = enumset_parts(vm, this);
    vm.ret_i32(ctx, elems.len() as i32);
    Ok(())
}

fn enumset_to_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let (_, elems) = enumset_parts(vm, this);
    vm.native_roots.push(this);
    let s = render_collection(vm, &elems)?;
    vm.native_roots.pop();
    vm.ret_ref(ctx, s);
    Ok(())
}

fn enumset_iterator(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let (_, elems) = enumset_parts(vm, this);
    let it = make_iterator(vm, elems)?;
    vm.native_roots.push(it);
    vm.ret_ref(ctx, it);
    vm.native_roots.pop();
    Ok(())
}

fn install_objects(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/util/Objects;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    let s = access::PUBLIC | access::STATIC;
    vm.add_native(
        id,
        "requireNonNull",
        "(Ljava/lang/Object;)Ljava/lang/Object;",
        s,
        objects_require_non_null,
    );
    vm.link_class(id)?;
    Ok(())
}

fn objects_require_non_null(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let o = vm.arg_ref(ctx, 0);
    if o == NULL {
        return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
    }
    vm.ret_ref(ctx, o);
    Ok(())
}

fn install_reflect_array(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/reflect/Array;",
        access::PUBLIC | access::FINAL,
        Some("Ljava/lang/Object;"),
        &[],
    )?;
    let s = access::PUBLIC | access::STATIC;
    vm.add_native(
        id,
        "newInstance",
        "(Ljava/lang/Class;[I)Ljava/lang/Object;",
        s,
        reflect_array_new_instance,
    );
    vm.link_class(id)?;
    Ok(())
}

fn reflect_array_new_instance(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let class_obj = vm.arg_ref(ctx, 0);
    let dims = vm.arg_ref(ctx, 1);
    let cid = match vm.heap.get(class_obj) {
        Some(Object::Class(c)) => *c,
        _ => return Err(vm.throw_new("Ljava/lang/IllegalArgumentException;", None)),
    };
    let comp = vm.class_desc(cid).to_string();
    let dim_list: Vec<i32> = match vm.heap.get(dims) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Int(v) => v.clone(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    let h = build_multi_array(vm, &comp, &dim_list)?;
    vm.native_roots.push(h);
    vm.ret_ref(ctx, h);
    vm.native_roots.pop();
    Ok(())
}

fn build_multi_array(vm: &mut Vm, elem_desc: &str, dims: &[i32]) -> VmResult<Handle> {
    if dims.is_empty() {
        return Err(VmError::Internal("Array.newInstance with no dims".into()));
    }
    if dims[0] < 0 {
        return Err(vm.throw_new("Ljava/lang/NegativeArraySizeException;", None));
    }
    if dims.len() == 1 {
        let obj = crate::vm::simple_array_object(elem_desc, dims[0] as usize)?;
        return vm.alloc(obj);
    }
    let inner_desc = format!("[{elem_desc}");
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: inner_desc,
        data: ArrayData::Ref(vec![NULL; dims[0] as usize]),
    }))?;
    vm.native_roots.push(h);
    let mut children = Vec::new();
    for _ in 0..dims[0] {
        children.push(build_multi_array(vm, elem_desc, &dims[1..])?);
    }
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Ref(v),
        ..
    })) = vm.heap.get_mut(h)
    {
        *v = children;
    }
    vm.native_roots.pop();
    Ok(h)
}
