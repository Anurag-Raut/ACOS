//! Decode AIDL transaction tables out of real boot-classpath DEX files.
//!
//! The scanner is intentionally mechanical: it reads `Foo$Stub.onTransact`'s
//! `packed-switch`/`sparse-switch` dispatch, resolves the `this.<method>` call
//! inside each case block to a `(code -> name, signature)` row, and then walks
//! the matching `Foo$Stub$Proxy.<method>` bytecode for the ordered sequence of
//! `Parcel.write*` (request) and `Parcel.read*` (reply) calls.
//!
//! Anything the scanner cannot decode becomes a [`WireOp::Unknown`] carrying
//! the method and program counter — it is never guessed.

use std::collections::{BTreeMap, HashMap};

use acos_dex::decode::{decode, decode_payload, payload_kind, payload_len};
use acos_dex::encoded::EncodedValue;
use acos_dex::{ClassDef, CodeItem, DexFile, Instruction, MethodRef};

use crate::ir::{InterfaceInfo, MethodInfo, WireOp};

/// Errors surfaced while decoding one interface.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("class not found: {0}")]
    NoClass(String),
    #[error("method not found: {0}.{1}{2}")]
    NoMethod(String, String, String),
    #[error("DEX error: {0}")]
    Dex(#[from] acos_dex::DexError),
}

/// Strip a type descriptor to a dotted name: `Landroid/view/IWindow;` ->
/// `android.view.IWindow`.
pub fn dotted(desc: &str) -> String {
    desc.trim_start_matches('L')
        .trim_end_matches(';')
        .replace('/', ".")
}

/// `proto` string for a resolved method reference, e.g. `(I)I`.
pub fn proto_string(r: &MethodRef<'_>) -> String {
    format!("({}){}", r.parameters.concat(), r.return_type)
}

/// An interface descriptor in `L...;` form for a dotted name.
pub fn descriptor_of(dotted_name: &str) -> String {
    format!("L{};", dotted_name.replace('.', "/"))
}

/// Hand-written (non-AIDL-generated) stub/proxy pairs that still speak the
/// standard Parcel transaction protocol. `IContentProvider` is the canonical
/// example: it has no `$Stub`/`$Stub$Proxy`, but `ContentProviderNative`
/// `onTransact` and `ContentProviderProxy` are byte-for-byte equivalent in
/// shape to generated AIDL.
pub const HANDWRITTEN_PAIRS: &[(&str, &str, &str)] = &[(
    "android.content.IContentProvider",
    "Landroid/content/ContentProviderNative;",
    "Landroid/content/ContentProviderProxy;",
)];

/// `(native, proxy)` descriptors for a hand-written interface.
pub fn handwritten_pair(interface_dotted: &str) -> Option<(&'static str, &'static str)> {
    HANDWRITTEN_PAIRS
        .iter()
        .find(|(i, _, _)| *i == interface_dotted)
        .map(|(_, n, p)| (*n, *p))
}

/// Dotted interface name for a stub/native class descriptor.
pub fn interface_for_stub(stub_desc: &str) -> Option<String> {
    if let Some(rest) = stub_desc.strip_suffix("$Stub;") {
        return Some(dotted(&format!("{rest};")));
    }
    HANDWRITTEN_PAIRS
        .iter()
        .find(|(_, n, _)| *n == stub_desc)
        .map(|(i, _, _)| (*i).to_string())
}

/// Proxy class descriptor for a stub/native class descriptor.
pub fn proxy_for_stub(stub_desc: &str) -> String {
    if let Some((_, _, proxy)) = HANDWRITTEN_PAIRS.iter().find(|(_, n, _)| *n == stub_desc) {
        return (*proxy).to_string();
    }
    format!("{}$Proxy;", stub_desc.trim_end_matches(';'))
}

/// One method declaration as found on the `$Stub`/`$Stub$Proxy` class.
#[derive(Clone, Debug)]
pub struct MethodDecl {
    pub method_idx: u32,
    pub name: String,
    pub proto: String,
    pub return_type: String,
    pub params: Vec<String>,
    pub access_flags: u32,
    pub code_off: u32,
}

impl MethodDecl {
    fn is_static(&self) -> bool {
        self.access_flags & 0x0008 != 0
    }
}

/// All methods (direct + virtual) declared on a class.
pub fn class_methods(dex: &DexFile, def: &ClassDef) -> Vec<MethodDecl> {
    let mut out = Vec::new();
    if let Ok(Some(cd)) = dex.class_data(def) {
        for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
            if let Ok(r) = dex.method_ref(m.method_idx) {
                out.push(MethodDecl {
                    method_idx: m.method_idx,
                    name: r.name.to_string(),
                    proto: proto_string(&r),
                    return_type: r.return_type.to_string(),
                    params: r.parameters.iter().map(|s| s.to_string()).collect(),
                    access_flags: m.access_flags,
                    code_off: m.code_off,
                });
            }
        }
    }
    out
}

/// Find a method by name and proto on a class.
pub fn find_method<'a>(
    methods: &'a [MethodDecl],
    name: &str,
    proto: &str,
) -> Option<&'a MethodDecl> {
    methods.iter().find(|m| m.name == name && m.proto == proto)
}

/// List every AIDL `$Stub` class (not `$Stub$Proxy`) declared in a DEX.
pub fn stub_classes(dex: &DexFile) -> Vec<String> {
    let mut out = Vec::new();
    for def in dex.class_defs() {
        let Ok(desc) = dex.class_descriptor(def) else {
            continue;
        };
        if desc.ends_with("$Stub;") {
            out.push(desc.to_string());
        }
    }
    out
}

/// Classification of one resolved invoke instruction.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Call {
    Write(WireOp),
    Read(WireOp),
    /// `readStrongBinder`; the interface is resolved by look-ahead.
    ReadBinder,
    /// Parcel plumbing that is not part of the AIDL wire layout.
    Header,
    /// Not a Parcel call we care about.
    Other,
    /// A Parcel call we do not know how to decode.
    Unsupported(String),
}

struct Code<'a> {
    insns: &'a [u16],
}

impl<'a> Code<'a> {
    fn new(item: &'a CodeItem) -> Self {
        Code { insns: &item.insns }
    }

    fn at(&self, pc: usize) -> Option<Instruction> {
        if payload_kind(self.insns, pc).is_some() {
            return None;
        }
        decode(self.insns, pc).ok()
    }

    fn step(&self, pc: usize) -> usize {
        if let Some(i) = self.at(pc) {
            pc + i.size as usize
        } else {
            payload_len(self.insns, pc)
                .map(|len| pc + len.max(1))
                .unwrap_or(pc + 1)
        }
    }

    fn len(&self) -> usize {
        self.insns.len()
    }
}

/// Register width in code units for a descriptor (`J`/`D` are wide).
fn width(desc: &str) -> i32 {
    match desc {
        "J" | "D" => 2,
        _ => 1,
    }
}

/// A scanner bound to one DEX file.
#[derive(Debug)]
pub struct Scanner<'a> {
    dex: &'a DexFile,
}

impl<'a> Scanner<'a> {
    pub fn new(dex: &'a DexFile) -> Self {
        Scanner { dex }
    }

    /// Decode every transaction of `interface_dotted` (e.g.
    /// `android.view.IWindowSession`).
    pub fn decode_interface(&self, interface_dotted: &str) -> Result<InterfaceInfo, DecodeError> {
        if let Some((native, proxy)) = handwritten_pair(interface_dotted) {
            let stub = self
                .dex
                .find_class(native)
                .ok_or_else(|| DecodeError::NoClass(native.to_string()))?;
            return self.decode_pair(stub, native, proxy, interface_dotted);
        }
        let iface_desc = descriptor_of(interface_dotted);
        let stub_desc = format!("{}$Stub;", iface_desc.trim_end_matches(';'));
        let stub = self
            .dex
            .find_class(&stub_desc)
            .ok_or_else(|| DecodeError::NoClass(stub_desc.clone()))?;
        self.decode_stub_class(stub, &stub_desc)
    }

    /// Decode an interface from its already-resolved `$Stub` class.
    pub fn decode_stub_class(
        &self,
        stub: &ClassDef,
        stub_desc: &str,
    ) -> Result<InterfaceInfo, DecodeError> {
        let interface_dotted =
            interface_for_stub(stub_desc).ok_or_else(|| DecodeError::NoClass(stub_desc.to_string()))?;
        let proxy_desc = proxy_for_stub(stub_desc);
        self.decode_pair(stub, stub_desc, &proxy_desc, &interface_dotted)
    }

    /// Decode an interface from explicitly named stub/native and proxy classes.
    pub fn decode_pair(
        &self,
        stub: &ClassDef,
        stub_desc: &str,
        proxy_desc: &str,
        interface_dotted: &str,
    ) -> Result<InterfaceInfo, DecodeError> {
        let stub_methods = class_methods(self.dex, stub);
        let on_transact = find_method(
            &stub_methods,
            "onTransact",
            "(ILandroid/os/Parcel;Landroid/os/Parcel;I)Z",
        )
        .ok_or_else(|| {
            DecodeError::NoMethod(stub_desc.to_string(), "onTransact".into(), String::new())
        })?;

        if on_transact.code_off == 0 {
            return Err(DecodeError::NoMethod(
                stub_desc.to_string(),
                "onTransact".into(),
                "no code".into(),
            ));
        }
        let on_code = self.dex.code_item(on_transact.code_off)?;
        let dispatch = self.decode_dispatch(stub_desc, &stub_methods, &on_code);

        // The proxy carries the canonical argument/reply layouts.
        let proxy = self.dex.find_class(proxy_desc);
        let proxy_methods = proxy.map(|p| class_methods(self.dex, p)).unwrap_or_default();

        let first_call_transaction = dispatch.keys().copied().min().unwrap_or(1);
        let mut methods = Vec::with_capacity(dispatch.len());
        for (code, entry) in &dispatch {
            let mut info = MethodInfo {
                name: entry.name.clone(),
                signature: entry.proto.clone(),
                return_type: entry.return_type.clone(),
                code: *code,
                oneway: false,
                args: Vec::new(),
                reply: Vec::new(),
                incomplete: None,
            };
            if let Some(pm) = find_method(&proxy_methods, &entry.name, &entry.proto) {
                if pm.code_off == 0 {
                    info.incomplete = Some("proxy method has no code".into());
                } else {
                    let pcode = self.dex.code_item(pm.code_off)?;
                    self.extract_proxy_layout(pm, &pcode, &mut info);
                }
            } else {
                info.incomplete = Some("no matching proxy method".into());
            }
            methods.push(info);
        }
        methods.sort_by_key(|m| m.code);

        Ok(InterfaceInfo {
            descriptor: interface_dotted.to_string(),
            first_call_transaction,
            methods,
        })
    }

    /// Map transaction code -> method entry by decoding the `onTransact` switch.
    fn decode_dispatch(
        &self,
        stub_desc: &str,
        stub_methods: &[MethodDecl],
        code: &CodeItem,
    ) -> BTreeMap<u32, DispatchEntry> {
        let c = Code::new(code);
        let mut out = BTreeMap::new();
        let mut pc = 0usize;
        while pc < c.len() {
            let Some(insn) = c.at(pc) else {
                pc = c.step(pc);
                continue;
            };
            if !insn.opcode.is_switch() {
                pc += insn.size as usize;
                continue;
            }
            let payload_pc = (pc as i64 + insn.target as i64) as usize;
            if let Ok(payload) = decode_payload(c.insns, payload_pc) {
                let (keys, targets) = match &payload {
                    acos_dex::Payload::PackedSwitch(s) | acos_dex::Payload::SparseSwitch(s) => {
                        (s.keys.clone(), s.targets.clone())
                    }
                    _ => (Vec::new(), Vec::new()),
                };
                for (key, target) in keys.iter().zip(targets.iter()) {
                    if *key < 1 {
                        continue;
                    }
                    let block = (pc as i64 + *target as i64) as usize;
                    if let Some(entry) =
                        self.stub_invoke_in_block(stub_desc, stub_methods, c.insns, block, 0)
                    {
                        out.insert(*key as u32, entry);
                    }
                }
            }
            pc += insn.size as usize;
        }
        out
    }

    /// Find the first `this.<aidlMethod>(...)` invocation in a case block.
    fn stub_invoke_in_block(
        &self,
        stub_desc: &str,
        stub_methods: &[MethodDecl],
        insns: &[u16],
        block: usize,
        depth: u32,
    ) -> Option<DispatchEntry> {
        // Bounded control-flow walk: a case block can contain loops and
        // nullable-array branches, so follow both the fallthrough and branch
        // targets, but never revisit an instruction. Dead blocks (e.g. a bare
        // `goto` to the common epilogue) therefore yield no method.
        let c = Code { insns };
        let mut visited = vec![false; c.len() + 2];
        let mut work = vec![block];
        let mut budget = 0usize;
        while let Some(start) = work.pop() {
            let mut pc = start;
            while pc < c.len() {
                if visited[pc] {
                    break;
                }
                visited[pc] = true;
                budget += 1;
                if budget > 50_000 {
                    return None;
                }
                let Some(insn) = c.at(pc) else {
                    pc = c.step(pc);
                    continue;
                };
                if insn.opcode.is_invoke() {
                    if let Ok(m) = self.dex.method_ref(insn.index) {
                        if m.class == stub_desc
                            && m.name != "asBinder"
                            && m.name != "asInterface"
                            && m.name != "onTransact"
                        {
                            // Large stubs split `onTransact` into
                            // `onTransact$<method>$(Parcel, Parcel)` trampolines;
                            // descend into the trampoline to find the real call.
                            if m.name.starts_with("onTransact$") && depth < 4 {
                                if let Some(syn) =
                                    stub_methods.iter().find(|sm| sm.name == m.name)
                                {
                                    if syn.code_off != 0 {
                                        if let Ok(syn_code) = self.dex.code_item(syn.code_off) {
                                            if let Some(entry) = self.stub_invoke_in_block(
                                                stub_desc,
                                                stub_methods,
                                                &syn_code.insns,
                                                0,
                                                depth + 1,
                                            ) {
                                                return Some(entry);
                                            }
                                        }
                                    }
                                }
                                // Fall through to a linear scan of the
                                // trampoline-less name if it cannot be resolved.
                            } else {
                                return Some(DispatchEntry {
                                    name: m.name.to_string(),
                                    proto: proto_string(&m),
                                    return_type: m.return_type.to_string(),
                                });
                            }
                        }
                    }
                }
                if insn.opcode.is_goto() {
                    let target = (pc as i64 + insn.target as i64) as usize;
                    if target < c.len() {
                        work.push(target);
                    }
                    break; // an unconditional jump does not fall through
                }
                if insn.opcode.is_branch() {
                    // Conditional branch: explore it, and keep the fallthrough.
                    let target = (pc as i64 + insn.target as i64) as usize;
                    if target < c.len() {
                        work.push(target);
                    }
                } else if matches!(insn.opcode.byte(), 0x0e..=0x11 | 0x27) {
                    break; // return / throw
                }
                pc += insn.size as usize;
            }
        }
        None
    }

    /// Extract `args`, `reply` and `oneway` from a `$Stub$Proxy` method.
    fn extract_proxy_layout(&self, pm: &MethodDecl, code: &CodeItem, info: &mut MethodInfo) {
        let c = Code::new(code);

        // Locate `IBinder.transact(int, Parcel, Parcel, int)`.
        let mut transact: Option<(usize, u16, u16, u16)> = None;
        let mut pc = 0usize;
        while pc < c.len() {
            let Some(insn) = c.at(pc) else {
                pc = c.step(pc);
                continue;
            };
            if insn.opcode.is_invoke() {
                if let Ok(m) = self.dex.method_ref(insn.index) {
                    if m.name == "transact"
                        && m.return_type == "Z"
                        && m.parameters.len() == 4
                        && m.class == "Landroid/os/IBinder;"
                    {
                        // F35c: regs = [receiver, code, data, reply, flags].
                        transact = Some((pc, insn.regs[2], insn.regs[3], insn.regs[4]));
                        break;
                    }
                }
            }
            pc += insn.size as usize;
        }
        let Some((transact_pc, data_reg, reply_reg, flags_reg)) = transact else {
            info.incomplete = Some("no IBinder.transact call in proxy".into());
            return;
        };

        info.oneway = self.const_reg_value(code, transact_pc, flags_reg) == Some(1);
        self.scan_method(code, transact_pc, data_reg, reply_reg, pm, info);
    }

    /// One forward pass over the proxy method, interleaving register-type
    /// tracking with the argument (pre-`transact`) and reply (post-
    /// `readException`) scans. Interleaving matters: AIDL reuses registers
    /// (`sget-object v3, X.CREATOR; readTypedObject(v3)` redefines `v3` right
    /// after), so a single precomputed map is wrong.
    fn scan_method(
        &self,
        code: &CodeItem,
        transact_pc: usize,
        data_reg: u16,
        reply_reg: u16,
        pm: &MethodDecl,
        info: &mut MethodInfo,
    ) {
        let c = Code::new(code);
        let insns = c.insns;
        let mut types = self.init_types(pm, code);
        let mut aliases: HashMap<u16, u16> = HashMap::new();
        aliases.insert(data_reg, data_reg);
        if reply_reg != data_reg {
            aliases.insert(reply_reg, reply_reg);
        }
        let mut seen_exception = false;
        let mut last_return: Option<String> = None;
        let mut pc = 0usize;
        let mut guard = 0;
        while pc < c.len() && guard < 300_000 {
            guard += 1;
            let Some(insn) = c.at(pc) else {
                pc = c.step(pc);
                continue;
            };

            // A nullable `CharSequence`/parcelable argument is written as
            // `if (value == null) writeInt(0) else { writeInt(1); <value> }`:
            // the *branch* comes before the marker write, so `try_cond` (which
            // needs a `move-result` between marker and branch) cannot see it.
            // This form has its own matcher.
            if matches!(insn.opcode.byte(), 0x38 | 0x39) {
                if let Some((cond, end)) = self.try_nullable_cond(insns, pc, c.len(), &types) {
                    info.args.push(cond);
                    pc = end;
                    continue;
                }
            }
            if insn.opcode.is_invoke() {
                if let Ok(m) = self.dex.method_ref(insn.index) {
                    // The Parcel operand is the receiver for `Parcel.*` and the
                    // first argument for `<Parcelable>.writeToParcel/readFromParcel`.
                    let parcel_reg = if m.class == "Landroid/os/Parcel;" {
                        Some(insn.regs[0])
                    } else if m.name == "writeToParcel" || m.name == "readFromParcel" {
                        Some(insn.regs[1])
                    } else {
                        None
                    };
                    let recv = parcel_reg.map(|r| aliases.get(&r).copied().unwrap_or(r));
                    if pc >= transact_pc && !seen_exception {
                        // Generated AIDL uses `Parcel.readException`; the
                        // hand-written provider proxy uses the static
                        // `DatabaseUtils.readExceptionFromParcel`.
                        if m.name == "readException" || m.name == "readExceptionFromParcel" {
                            seen_exception = true;
                        }
                    } else if insn.opcode.byte() == 0x6e && pc < transact_pc {
                        if recv == Some(data_reg) {
                            if let Some(end) =
                                self.emit_call(insns, pc, transact_pc, &types, &mut info.args)
                            {
                                pc = end;
                                continue;
                            }
                        }
                    } else if insn.opcode.byte() == 0x6e && recv == Some(reply_reg) {
                        if let Some(end) = self.emit_call(insns, pc, c.len(), &types, &mut info.reply) {
                            pc = end;
                            continue;
                        }
                    }
                }
            }

            // Register-type updates for this instruction.
            match insn.opcode.byte() {
                0x07..=0x09 => {
                    if let Some(ty) = types.get(&insn.regs[1]).cloned() {
                        types.insert(insn.regs[0], ty);
                    } else {
                        types.remove(&insn.regs[0]);
                    }
                }
                0x0c => {
                    if let Some(r) = last_return.clone() {
                        types.insert(insn.regs[0], r);
                    } else {
                        types.remove(&insn.regs[0]);
                    }
                }
                0x0a | 0x0b | 0x0d | 0x1c | 0x22 | 0x23 => {
                    types.remove(&insn.regs[0]);
                }
                0x1a | 0x1b => {
                    types.insert(insn.regs[0], "Ljava/lang/String;".into());
                }
                0x62 => {
                    if let Ok(fr) = self.dex.field_ref(insn.index) {
                        if fr.name == "CREATOR" {
                            types.insert(insn.regs[0], fr.class.to_string());
                        } else {
                            types.insert(insn.regs[0], fr.type_.to_string());
                        }
                    }
                }
                _ => {}
            }
            if insn.opcode.is_invoke() {
                last_return = self
                    .dex
                    .method_ref(insn.index)
                    .ok()
                    .map(|m| m.return_type.to_string());
            }
            pc += insn.size as usize;
        }
    }

    /// Classify one Parcel call and append it. Returns the next pc when a
    /// conditional block was folded (the caller must resume there).
    fn emit_call(
        &self,
        insns: &[u16],
        pc: usize,
        to: usize,
        types: &HashMap<u16, String>,
        ops: &mut Vec<WireOp>,
    ) -> Option<usize> {
        let insn = decode(insns, pc).ok()?;
        match self.classify_call(&insn, types) {
            Call::ReadBinder => {
                let iface = self
                    .result_reg(insns, pc)
                    .and_then(|rr| self.binder_iface_after(insns, pc, rr));
                ops.push(WireOp::Binder { iface });
                None
            }
            Call::Read(op) | Call::Write(op) => {
                if let Some((cond, end)) = self.try_cond(insns, pc, to, types) {
                    ops.push(cond);
                    Some(end)
                } else {
                    ops.push(op);
                    None
                }
            }
            Call::Header | Call::Other => None,
            Call::Unsupported(note) => {
                ops.push(WireOp::Unknown {
                    note: format!("{note} @pc {pc:#06x}"),
                });
                None
            }
        }
    }

    fn init_types(&self, pm: &MethodDecl, code: &CodeItem) -> HashMap<u16, String> {
        let mut t = HashMap::new();
        let mut reg = code.registers_size as i32 - code.ins_size as i32;
        if !pm.is_static() {
            reg += 1; // `this`
        }
        for p in &pm.params {
            t.insert(reg as u16, p.clone());
            reg += width(p);
        }
        t
    }

    fn result_reg(&self, insns: &[u16], invoke_pc: usize) -> Option<u16> {
        let insn = decode(insns, invoke_pc).ok()?;
        let next = decode(insns, invoke_pc + insn.size as usize).ok()?;
        if matches!(next.opcode.byte(), 0x0a..=0x0c) {
            Some(next.regs[0])
        } else {
            None
        }
    }

    /// If `readStrongBinder` is followed by `<Iface>$Stub.asInterface`, return
    /// the dotted interface name.
    fn binder_iface_after(&self, insns: &[u16], invoke_pc: usize, result_reg: u16) -> Option<String> {
        let mut pc = invoke_pc;
        for _ in 0..10 {
            let insn = decode(insns, pc).ok()?;
            if insn.opcode.is_invoke() {
                if let Ok(m) = self.dex.method_ref(insn.index) {
                    if m.name == "asInterface"
                        && m.class.ends_with("$Stub;")
                        && insn.regs[0] == result_reg
                    {
                        let trimmed = m.class.strip_suffix("$Stub;")?;
                        return Some(dotted(trimmed));
                    }
                }
            }
            if insn.opcode.is_terminator() {
                break;
            }
            pc += insn.size as usize;
        }
        None
    }

    /// Detect `marker; if (marker == 0) skip; <op>` and fold it into a `Cond`.
    /// Fold the AIDL nullable-value idiom whose branch precedes the marker
    /// write (so `try_cond` cannot see it):
    ///
    /// ```text
    /// if-eqz vA, else
    ///   const/4 vB, #1
    ///   invoke-virtual {parcel, vB}, Parcel.writeInt(I)V      // marker
    ///   invoke-static {vA, parcel, 0}, TextUtils.writeToParcel // value
    ///   goto end
    /// else:
    ///   invoke-virtual {parcel, vB}, Parcel.writeInt(I)V       // vB == 0 (hoisted)
    /// end:
    /// ```
    ///
    /// The wire form is `int32 present` + the value when present, which is
    /// exactly `WireOp::Cond { marker: I32, ops: [value] }`. Only the
    /// `if-eqz`/present-on-fallthrough shape is folded; every other shape keeps
    /// the previous literal decoding so no unknown pattern is guessed at.
    fn try_nullable_cond(
        &self,
        insns: &[u16],
        pc: usize,
        to: usize,
        types: &HashMap<u16, String>,
    ) -> Option<(WireOp, usize)> {
        let branch = decode(insns, pc).ok()?;
        if branch.opcode.byte() != 0x38 {
            return None;
        }
        let fallthrough = pc + branch.size as usize;
        // `const/4 vB, #1` on the present path (or the same constant hoisted
        // above the branch, in which case the marker invoke comes first).
        let mut marker_pc = fallthrough;
        let mut marker_reg = None;
        if let Ok(first) = decode(insns, fallthrough) {
            if first.opcode.byte() == 0x12 && first.literal == 1 {
                marker_pc = fallthrough + first.size as usize;
                marker_reg = Some(first.regs[0]);
            }
        }
        if marker_reg.is_none() {
            let reg = self.write_int_reg(insns, fallthrough)?;
            if self.const_before(insns, pc, reg) == Some(1) {
                marker_reg = Some(reg);
            }
        }
        let marker_reg = marker_reg?;
        if !self.is_write_int_of(insns, marker_pc, marker_reg) {
            return None;
        }
        let marker = decode(insns, marker_pc).ok()?;
        let value_pc = marker_pc + marker.size as usize;
        if value_pc >= to {
            return None;
        }
        let value_insn = decode(insns, value_pc).ok()?;
        if !value_insn.opcode.is_invoke() {
            return None;
        }
        let value = match self.classify_call(&value_insn, types) {
            Call::Write(op) | Call::Read(op) => op,
            _ => return None,
        };
        let goto_pc = value_pc + value_insn.size as usize;
        let goto_insn = decode(insns, goto_pc).ok()?;
        if !goto_insn.opcode.is_goto() {
            return None;
        }
        // The absent path writes the same marker with a constant `0`. A dex
        // branch encodes its destination as a signed code-unit delta from the
        // branch instruction itself (unlike a method-relative jump table).
        let else_pc = (pc as i64 + branch.target as i64) as usize;
        if else_pc >= to {
            return None;
        }
        // The `const/4 vB, #0` may lead the absent marker or be hoisted above
        // the branch (the common AIDL shape).
        let mut else_marker_pc = else_pc;
        let mut else_reg = None;
        if let Ok(first) = decode(insns, else_pc) {
            if first.opcode.byte() == 0x12 && first.literal == 0 {
                else_marker_pc = else_pc + first.size as usize;
                else_reg = Some(first.regs[0]);
            }
        }
        if else_reg.is_none() {
            let reg = self.write_int_reg(insns, else_pc)?;
            if self.const_before(insns, pc, reg) == Some(0) {
                else_reg = Some(reg);
            }
        }
        let else_reg = else_reg?;
        if !self.is_write_int_of(insns, else_marker_pc, else_reg) {
            return None;
        }
        let else_marker = decode(insns, else_marker_pc).ok()?;
        let resume = else_marker_pc + else_marker.size as usize;
        // Both arms converge on the instruction after the absent marker.
        let end = (goto_pc as i64 + goto_insn.target as i64) as usize;
        if resume != end {
            return None;
        }
        Some((
            WireOp::Cond {
                marker: Box::new(WireOp::I32),
                ops: vec![value],
            },
            resume,
        ))
    }

    /// The value register of a `Parcel.writeInt(reg)` invoke at `pc`.
    fn write_int_reg(&self, insns: &[u16], pc: usize) -> Option<u16> {
        if !self.is_write_int_of(insns, pc, decode(insns, pc).ok()?.regs[1]) {
            return None;
        }
        let insn = decode(insns, pc).ok()?;
        self.is_write_int_of(insns, pc, insn.regs[1]).then_some(insn.regs[1])
    }

    /// The last `const*` written to `reg` before `before_pc`, if any.
    fn const_before(&self, insns: &[u16], before_pc: usize, reg: u16) -> Option<i64> {
        let mut value = None;
        let mut pc = 0usize;
        while pc < before_pc {
            let Ok(insn) = decode(insns, pc) else {
                break;
            };
            if matches!(insn.opcode.byte(), 0x12..=0x15) && insn.regs[0] == reg {
                value = Some(insn.literal);
            }
            pc += insn.size as usize;
        }
        value
    }

    /// True when the instruction at `pc` is `Parcel.writeInt(reg)`.
    fn is_write_int_of(&self, insns: &[u16], pc: usize, reg: u16) -> bool {
        let Ok(insn) = decode(insns, pc) else {
            return false;
        };
        if !insn.opcode.is_invoke() {
            return false;
        }
        let Ok(m) = self.dex.method_ref(insn.index) else {
            return false;
        };
        m.class == "Landroid/os/Parcel;" && m.name == "writeInt" && insn.regs[1] == reg
    }

    fn try_cond(
        &self,
        insns: &[u16],
        pc: usize,
        to: usize,
        types: &HashMap<u16, String>,
    ) -> Option<(WireOp, usize)> {
        let marker_insn = decode(insns, pc).ok()?;
        let marker = match self.classify_call(&marker_insn, types) {
            Call::Read(op) | Call::Write(op) => op,
            _ => return None,
        };
        let mv_pc = pc + marker_insn.size as usize;
        let mv = decode(insns, mv_pc).ok()?;
        if !matches!(mv.opcode.byte(), 0x0a..=0x0c) {
            return None;
        }
        let r = mv.regs[0];
        let branch_pc = mv_pc + mv.size as usize;
        let branch = decode(insns, branch_pc).ok()?;
        if !(branch.opcode.byte() == 0x38 || branch.opcode.byte() == 0x39) || branch.regs[0] != r {
            return None;
        }
        let fallthrough = branch_pc + branch.size as usize;
        if fallthrough >= to {
            return None;
        }
        // Collect the conditional body on the fallthrough path until a goto.
        let mut body = Vec::new();
        let mut p = fallthrough;
        let mut end = fallthrough;
        let mut steps = 0;
        while p < to && steps < 40 {
            steps += 1;
            let Some(insn) = decode(insns, p).ok() else {
                break;
            };
            if insn.opcode.is_invoke() && insn.opcode.byte() == 0x6e {
                match self.classify_call(&insn, types) {
                    Call::Read(op) | Call::Write(op) => body.push(op),
                    Call::ReadBinder => body.push(WireOp::Binder { iface: None }),
                    Call::Header | Call::Other => {}
                    Call::Unsupported(note) => body.push(WireOp::Unknown {
                        note: format!("{note} @pc {p:#06x}"),
                    }),
                }
            }
            if insn.opcode.is_goto() {
                end = (p as i64 + insn.target as i64) as usize;
                break;
            }
            if insn.opcode.is_terminator() {
                end = p + insn.size as usize;
                break;
            }
            p += insn.size as usize;
        }
        if body.is_empty() {
            return None;
        }
        Some((
            WireOp::Cond {
                marker: Box::new(marker),
                ops: body,
            },
            end.max(pc + 1),
        ))
    }

    /// Constant value written to `reg` on the path to `before_pc`, if a simple
    /// `const*` was seen last.
    fn const_reg_value(&self, code: &CodeItem, before_pc: usize, reg: u16) -> Option<i64> {
        let c = Code::new(code);
        let mut value = None;
        let mut pc = 0usize;
        while pc < before_pc {
            let Some(insn) = c.at(pc) else {
                pc = c.step(pc);
                continue;
            };
            if matches!(insn.opcode.byte(), 0x12..=0x19)
                && insn.regs[0] == reg
                && insn.reg_count >= 1
            {
                value = Some(insn.literal);
            }
            pc += insn.size as usize;
        }
        value
    }

    /// Classify a resolved invoke instruction as an AIDL wire operation.
    fn classify_call(&self, insn: &Instruction, types: &HashMap<u16, String>) -> Call {
        let Ok(m) = self.dex.method_ref(insn.index) else {
            return Call::Other;
        };
        if m.class != "Landroid/os/Parcel;" {
            return match m.name {
                // `TextUtils.writeToParcel(CharSequence, Parcel, int)` is what an
                // AIDL proxy calls for a `CharSequence` field. Its wire form is
                // `int32 flag` + `string8` for a plain String, where the flag is
                // `1` for a non-Spanned value (`0` when span data follows).
                // Modelling it as a conditional `Str8` keeps the parcel aligned
                // for the (universal) plain-text case; a Spanned payload is
                // reported by the reader instead of being silently misparsed.
                "writeToParcel" | "readFromParcel" if m.class == "Landroid/text/TextUtils;" => {
                    let op = WireOp::Cond {
                        marker: Box::new(WireOp::I32),
                        ops: vec![WireOp::Str8],
                    };
                    if m.name == "writeToParcel" {
                        Call::Write(op)
                    } else {
                        Call::Read(op)
                    }
                }
                "readFromParcel" => Call::Read(WireOp::Parcelable {
                    class: dotted(m.class),
                }),
                "writeToParcel" => Call::Write(WireOp::Parcelable {
                    class: dotted(m.class),
                }),
                // `<X>.CREATOR.createFromParcel(parcel)`: the creator register
                // carries the declaring class (set by `sget-object`).
                "createFromParcel" => match types.get(&insn.regs[0]).map(|d| dotted(d)) {
                    Some(class) => Call::Read(WireOp::Parcelable { class }),
                    None => Call::Unsupported("createFromParcel creator class unresolved".into()),
                },
                _ => Call::Other,
            };
        }
        let write = m.name.starts_with("write");
        let op = |w: WireOp, r: WireOp| if write { Call::Write(w) } else { Call::Read(r) };
        match m.name {
            "writeInterfaceToken" | "readException" | "writeNoException"
            | "enforceNoDataAvail" | "recycle" | "setDataPosition" | "dataSize" | "dataAvail"
            | "dataPosition" | "appendFrom" | "marshall" | "unmarshall" | "setDataSize"
            | "hasFileDescriptors" | "markSensitive" | "setDataPositionToEnd" => Call::Header,
            "writeInt" | "readInt" => op(WireOp::I32, WireOp::I32),
            "writeLong" | "readLong" => op(WireOp::I64, WireOp::I64),
            "writeFloat" | "readFloat" => op(WireOp::F32, WireOp::F32),
            "writeDouble" | "readDouble" => op(WireOp::F64, WireOp::F64),
            "writeBoolean" | "readBoolean" => op(WireOp::Bool, WireOp::Bool),
            "writeByte" | "readByte" => op(WireOp::Byte, WireOp::Byte),
            "writeChar" | "readChar" => op(WireOp::Char, WireOp::Char),
            "writeString" | "readString" => op(WireOp::Str16, WireOp::Str16),
            "writeString8" | "readString8" => op(WireOp::Str8, WireOp::Str8),
            "writeStrongInterface" => Call::Write(WireOp::Binder {
                iface: types.get(&insn.regs[1]).map(|d| dotted(d)),
            }),
            "writeStrongBinder" => Call::Write(WireOp::Binder {
                iface: types
                    .get(&insn.regs[1])
                    .filter(|d| d.as_str() != "Landroid/os/IBinder;")
                    .map(|d| dotted(d)),
            }),
            "readStrongBinder" => Call::ReadBinder,
            "writeByteArray" | "createByteArray" | "readByteArray" => {
                op(WireOp::ByteArray, WireOp::ByteArray)
            }
            "writeIntArray" | "createIntArray" | "readIntArray" => {
                op(WireOp::IntArray, WireOp::IntArray)
            }
            "writeLongArray" | "createLongArray" | "readLongArray" => {
                op(WireOp::LongArray, WireOp::LongArray)
            }
            "writeFloatArray" | "createFloatArray" | "readFloatArray" => {
                op(WireOp::FloatArray, WireOp::FloatArray)
            }
            "writeDoubleArray" | "createDoubleArray" | "readDoubleArray" => {
                op(WireOp::DoubleArray, WireOp::DoubleArray)
            }
            "writeBooleanArray" | "createBooleanArray" | "readBooleanArray" => {
                op(WireOp::BooleanArray, WireOp::BooleanArray)
            }
            "writeCharArray" | "createCharArray" | "readCharArray" => {
                op(WireOp::CharArray, WireOp::CharArray)
            }
            "writeStringArray" | "createStringArray" | "readStringArray" => {
                op(WireOp::StringArray, WireOp::StringArray)
            }
            "writeStringList" | "readStringList" | "createStringArrayList"
            | "writeStringArrayList" => op(WireOp::StringList, WireOp::StringList),
            "writeBinderList" | "readBinderList" | "createBinderArrayList"
            | "writeBinderArrayList" => op(WireOp::BinderList, WireOp::BinderList),
            "writeRawFileDescriptor" | "readRawFileDescriptor" => op(WireOp::RawFd, WireOp::RawFd),
            "writeTypedArray" | "createTypedArray" | "readTypedArray" => {
                match types.get(&insn.regs[1]).map(|d| dotted(d)) {
                    Some(class) => op(
                        WireOp::TypedArray {
                            class: class.clone(),
                        },
                        WireOp::TypedArray { class },
                    ),
                    None => Call::Unsupported("typed array creator class unresolved".into()),
                }
            }
            "writeTypedList" | "createTypedArrayList" | "readTypedList" | "readTypedArrayList" => {
                match types.get(&insn.regs[1]).map(|d| dotted(d)) {
                    Some(class) => op(
                        WireOp::TypedList {
                            class: class.clone(),
                        },
                        WireOp::TypedList { class },
                    ),
                    None => Call::Unsupported("typed list element class unresolved".into()),
                }
            }
            "writeTypedObject" | "readTypedObject" => {
                match types.get(&insn.regs[1]).map(|d| dotted(d)) {
                    Some(class) => op(
                        WireOp::TypedObject {
                            class: class.clone(),
                            nullable: true,
                        },
                        WireOp::TypedObject {
                            class,
                            nullable: true,
                        },
                    ),
                    None => Call::Unsupported("typed object class unresolved".into()),
                }
            }
            "writeParcelable" | "readParcelable" => {
                match types.get(&insn.regs[1]).map(|d| dotted(d)) {
                    Some(class) => op(
                        WireOp::Parcelable {
                            class: class.clone(),
                        },
                        WireOp::Parcelable { class },
                    ),
                    None => Call::Unsupported("parcelable class unresolved".into()),
                }
            }
            "writeList" | "readArrayList" | "readList" => op(
                WireOp::List {
                    elem: Box::new(WireOp::Unknown {
                        note: "list element".into(),
                    }),
                },
                WireOp::List {
                    elem: Box::new(WireOp::Unknown {
                        note: "list element".into(),
                    }),
                },
            ),
            "writeArray" | "readArray" => op(
                WireOp::Array {
                    elem: Box::new(WireOp::Unknown {
                        note: "array element".into(),
                    }),
                },
                WireOp::Array {
                    elem: Box::new(WireOp::Unknown {
                        note: "array element".into(),
                    }),
                },
            ),
            "writeMap" | "readHashMap" | "readMap" => op(
                WireOp::Map {
                    key: Box::new(WireOp::Unknown {
                        note: "map key".into(),
                    }),
                    value: Box::new(WireOp::Unknown {
                        note: "map value".into(),
                    }),
                },
                WireOp::Map {
                    key: Box::new(WireOp::Unknown {
                        note: "map key".into(),
                    }),
                    value: Box::new(WireOp::Unknown {
                        note: "map value".into(),
                    }),
                },
            ),
            "writeBundle" | "readBundle" => op(WireOp::Bundle, WireOp::Bundle),
            "writeValue" | "readValue" => op(WireOp::Value, WireOp::Value),
            other => Call::Unsupported(format!("Parcel.{other}")),
        }
    }
}

#[derive(Clone, Debug)]
struct DispatchEntry {
    name: String,
    proto: String,
    return_type: String,
}

/// Decode one interface from a single DEX.
pub fn decode_interface(
    dex: &DexFile,
    interface_dotted: &str,
) -> Result<InterfaceInfo, DecodeError> {
    Scanner::new(dex).decode_interface(interface_dotted)
}

/// Convenience wrapper around [`decode_interface`] that returns `None` if the
/// interface is not present in the DEX.
pub fn try_decode_interface(dex: &DexFile, interface_dotted: &str) -> Option<InterfaceInfo> {
    decode_interface(dex, interface_dotted).ok()
}

/// Static string value of a named `static final` field, if present.
pub fn static_string_field(dex: &DexFile, def: &ClassDef, name: &str) -> Option<String> {
    let cd = dex.class_data(def).ok()??;
    let values = dex.static_values(def).ok()??;
    for (i, f) in cd.static_fields.iter().enumerate() {
        let Ok(fr) = dex.field_ref(f.field_idx) else {
            continue;
        };
        if fr.name == name {
            if let Some(EncodedValue::String(idx)) = values.get(i) {
                return dex.string(*idx).ok().map(str::to_string);
            }
        }
    }
    None
}

/// `TRANSACTION_<name>` code constants declared as `static final int` fields.
pub fn transaction_constants(dex: &DexFile, def: &ClassDef) -> BTreeMap<String, i64> {
    let mut out = BTreeMap::new();
    let Ok(Some(cd)) = dex.class_data(def) else {
        return out;
    };
    let Ok(Some(values)) = dex.static_values(def) else {
        return out;
    };
    for (i, f) in cd.static_fields.iter().enumerate() {
        let Ok(fr) = dex.field_ref(f.field_idx) else {
            continue;
        };
        let Some(rest) = fr.name.strip_prefix("TRANSACTION_") else {
            continue;
        };
        if let Some(EncodedValue::Int(v)) = values.get(i) {
            out.insert(rest.to_string(), *v as i64);
        }
    }
    out
}
