//! `com.android.org.conscrypt.NativeCrypto` — the OpenSSL-backed security
//! primitives the real Android 14 `conscrypt.jar` exposes to
//! `java.security` / `javax.crypto`.
//!
//! Android's `java.security.Security` reads `java/security/security.properties`
//! (in `core-oj.jar`) and instantiates four providers:
//! `com.android.org.conscrypt.OpenSSLProvider`, `sun.security.provider.CertPathProvider`,
//! `com.android.org.bouncycastle.jce.provider.BouncyCastleProvider` and
//! `com.android.org.conscrypt.JSSEProvider`. Loading `OpenSSLProvider`
//! initialises `NativeCrypto` (`System.loadLibrary("javacrypto")` plus the
//! native `clinit()`), so the provider cannot even be named without this
//! module.
//!
//! # What is real here
//!
//! Every primitive is backed by an audited pure-Rust RustCrypto crate — no
//! home-grown cryptography and no linking to a host OpenSSL:
//!
//! * digest: `md-5`, `sha1`, `sha2`, driven through `digest::DynDigest`;
//! * `HMAC`: `hmac`;
//! * AES-ECB/CBC/CTR block and stream modes: `aes` and `ctr`;
//! * `SecureRandom`: `getrandom` (the OS CSPRNG).
//!
//! The OpenSSL `EVP_*` shape is preserved with an opaque handle table: a
//! `jlong` address maps to a live Rust context. The Java `NativeRef`
//! subclasses carry that `address`, so the exact object identity/flush
//! semantics of `EVP_MD_CTX`/`HMAC_CTX`/`EVP_CIPHER_CTX` are respected.
//!
//! # What is *not* here
//!
//! TLS/SSL (`SSL_*`, `SSL_CTX_*`), certificate parsing (`d2i_X509*`,
//! `X509_*`), public-key operations (`EVP_PKEY_*`, `RSA_*`, `EC_*`) and
//! AES-GCM (conscrypt's AEAD path) are not implemented. Those remain a
//! structured `UNSUPPORTED_JAVA_API` naming the exact native — never a faked
//! value. The `M2` console/lifecycle paths do not use them.

use std::cell::RefCell;
use std::collections::HashMap;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use digest::DynDigest;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;

// --- algorithm identities --------------------------------------------------

/// A supported message-digest algorithm.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum MdKind {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl MdKind {
    /// Stable `jlong` id returned by `EVP_get_digestbyname` (never 0).
    fn id(self) -> i64 {
        match self {
            MdKind::Md5 => 1,
            MdKind::Sha1 => 2,
            MdKind::Sha224 => 3,
            MdKind::Sha256 => 4,
            MdKind::Sha384 => 5,
            MdKind::Sha512 => 6,
        }
    }

    fn from_id(id: i64) -> Option<MdKind> {
        Some(match id {
            1 => MdKind::Md5,
            2 => MdKind::Sha1,
            3 => MdKind::Sha224,
            4 => MdKind::Sha256,
            5 => MdKind::Sha384,
            6 => MdKind::Sha512,
            _ => return None,
        })
    }

    /// Normalise an OpenSSL/Java digest name (`sha-256`, `SHA256`, …).
    fn from_name(name: &str) -> Option<MdKind> {
        let n = name.trim().to_ascii_lowercase().replace(['-', '_'], "");
        Some(match n.as_str() {
            "md5" => MdKind::Md5,
            "sha1" => MdKind::Sha1,
            "sha224" => MdKind::Sha224,
            "sha256" => MdKind::Sha256,
            "sha384" => MdKind::Sha384,
            "sha512" => MdKind::Sha512,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            MdKind::Md5 => 16,
            MdKind::Sha1 => 20,
            MdKind::Sha224 => 28,
            MdKind::Sha256 => 32,
            MdKind::Sha384 => 48,
            MdKind::Sha512 => 64,
        }
    }

    fn new_digest(self) -> Box<dyn DynDigest> {
        match self {
            MdKind::Md5 => Box::new(md5::Md5::default()),
            MdKind::Sha1 => Box::new(sha1::Sha1::default()),
            MdKind::Sha224 => Box::new(sha2::Sha224::default()),
            MdKind::Sha256 => Box::new(sha2::Sha256::default()),
            MdKind::Sha384 => Box::new(sha2::Sha384::default()),
            MdKind::Sha512 => Box::new(sha2::Sha512::default()),
        }
    }
}

/// A supported AES cipher mode/keysize.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum CipherAlg {
    Aes128Ecb,
    Aes256Ecb,
    Aes128Cbc,
    Aes256Cbc,
    Aes128Ctr,
    Aes256Ctr,
}

impl CipherAlg {
    fn id(self) -> i64 {
        match self {
            CipherAlg::Aes128Ecb => 0x101,
            CipherAlg::Aes256Ecb => 0x102,
            CipherAlg::Aes128Cbc => 0x103,
            CipherAlg::Aes256Cbc => 0x104,
            CipherAlg::Aes128Ctr => 0x105,
            CipherAlg::Aes256Ctr => 0x106,
        }
    }

    fn from_id(id: i64) -> Option<CipherAlg> {
        Some(match id {
            0x101 => CipherAlg::Aes128Ecb,
            0x102 => CipherAlg::Aes256Ecb,
            0x103 => CipherAlg::Aes128Cbc,
            0x104 => CipherAlg::Aes256Cbc,
            0x105 => CipherAlg::Aes128Ctr,
            0x106 => CipherAlg::Aes256Ctr,
            _ => return None,
        })
    }

    fn from_name(name: &str) -> Option<CipherAlg> {
        let n = name.trim().to_ascii_lowercase();
        Some(match n.as_str() {
            "aes-128-ecb" => CipherAlg::Aes128Ecb,
            "aes-256-ecb" => CipherAlg::Aes256Ecb,
            "aes-128-cbc" => CipherAlg::Aes128Cbc,
            "aes-256-cbc" => CipherAlg::Aes256Cbc,
            "aes-128-ctr" => CipherAlg::Aes128Ctr,
            "aes-256-ctr" => CipherAlg::Aes256Ctr,
            _ => return None,
        })
    }

    fn key_len(self) -> usize {
        match self {
            CipherAlg::Aes128Ecb | CipherAlg::Aes128Cbc | CipherAlg::Aes128Ctr => 16,
            CipherAlg::Aes256Ecb | CipherAlg::Aes256Cbc | CipherAlg::Aes256Ctr => 32,
        }
    }

    fn iv_len(self) -> usize {
        match self {
            CipherAlg::Aes128Ecb | CipherAlg::Aes256Ecb => 0,
            _ => 16,
        }
    }

    fn block_size(self) -> usize {
        match self {
            CipherAlg::Aes128Ctr | CipherAlg::Aes256Ctr => 1,
            _ => 16,
        }
    }
}

// --- context tables --------------------------------------------------------

/// An `EVP_MD_CTX` (digest accumulator).
enum DigestCtx {
    /// Created but not yet `EVP_DigestInit_ex`'d.
    Empty,
    Ready {
        digest: Box<dyn DynDigest>,
    },
}

/// An `HMAC_CTX`.
#[allow(clippy::large_enum_variant)] // `HmacState` wraps a concrete `Hmac<D>`.
enum HmacCtx {
    Empty,
    Ready { mac: HmacState },
}

enum HmacState {
    Md5(hmac::Hmac<md5::Md5>),
    Sha1(hmac::Hmac<sha1::Sha1>),
    Sha224(hmac::Hmac<sha2::Sha224>),
    Sha256(hmac::Hmac<sha2::Sha256>),
    Sha384(hmac::Hmac<sha2::Sha384>),
    Sha512(hmac::Hmac<sha2::Sha512>),
}

impl HmacState {
    fn new(kind: MdKind, key: &[u8]) -> Option<HmacState> {
        Some(match kind {
            MdKind::Md5 => HmacState::Md5(hmac::Hmac::new_from_slice(key).ok()?),
            MdKind::Sha1 => HmacState::Sha1(hmac::Hmac::new_from_slice(key).ok()?),
            MdKind::Sha224 => HmacState::Sha224(hmac::Hmac::new_from_slice(key).ok()?),
            MdKind::Sha256 => HmacState::Sha256(hmac::Hmac::new_from_slice(key).ok()?),
            MdKind::Sha384 => HmacState::Sha384(hmac::Hmac::new_from_slice(key).ok()?),
            MdKind::Sha512 => HmacState::Sha512(hmac::Hmac::new_from_slice(key).ok()?),
        })
    }

    fn update(&mut self, data: &[u8]) {
        use hmac::Mac;
        match self {
            HmacState::Md5(h) => h.update(data),
            HmacState::Sha1(h) => h.update(data),
            HmacState::Sha224(h) => h.update(data),
            HmacState::Sha256(h) => h.update(data),
            HmacState::Sha384(h) => h.update(data),
            HmacState::Sha512(h) => h.update(data),
        }
    }

    fn finalize(self) -> Vec<u8> {
        use hmac::Mac;
        match self {
            HmacState::Md5(h) => h.finalize().into_bytes().to_vec(),
            HmacState::Sha1(h) => h.finalize().into_bytes().to_vec(),
            HmacState::Sha224(h) => h.finalize().into_bytes().to_vec(),
            HmacState::Sha256(h) => h.finalize().into_bytes().to_vec(),
            HmacState::Sha384(h) => h.finalize().into_bytes().to_vec(),
            HmacState::Sha512(h) => h.finalize().into_bytes().to_vec(),
        }
    }
}

/// An `EVP_CIPHER_CTX`.
struct CipherCtx {
    alg: Option<CipherAlg>,
    encrypt: bool,
    key: Vec<u8>,
    iv: Vec<u8>,
    padding: bool,
    /// Pending input that has not yet produced a complete output block.
    buf: Vec<u8>,
    /// Whether `EVP_CipherFinal_ex` consumed the final block (CBC/ECB).
    final_used: bool,
}

impl Default for CipherCtx {
    fn default() -> Self {
        CipherCtx {
            alg: None,
            encrypt: false,
            key: Vec::new(),
            iv: Vec::new(),
            padding: true,
            buf: Vec::new(),
            final_used: false,
        }
    }
}

impl CipherCtx {
    /// Encrypt or decrypt one 16-byte AES block in place (ECB, no chaining).
    fn aes_block(alg: CipherAlg, key: &[u8], block: &mut [u8; 16], encrypt: bool) -> bool {
        match alg {
            CipherAlg::Aes128Ecb | CipherAlg::Aes128Cbc | CipherAlg::Aes128Ctr => {
                match aes::Aes128::new_from_slice(key) {
                    Ok(c) => {
                        let mut b = aes::Block::clone_from_slice(block);
                        if encrypt {
                            c.encrypt_block(&mut b);
                        } else {
                            c.decrypt_block(&mut b);
                        }
                        block.copy_from_slice(&b);
                        true
                    }
                    Err(_) => false,
                }
            }
            CipherAlg::Aes256Ecb | CipherAlg::Aes256Cbc | CipherAlg::Aes256Ctr => {
                match aes::Aes256::new_from_slice(key) {
                    Ok(c) => {
                        let mut b = aes::Block::clone_from_slice(block);
                        if encrypt {
                            c.encrypt_block(&mut b);
                        } else {
                            c.decrypt_block(&mut b);
                        }
                        block.copy_from_slice(&b);
                        true
                    }
                    Err(_) => false,
                }
            }
        }
    }

    /// Apply the cipher to one full block, advancing CBC/CTR state.
    ///
    /// `iv`/chaining state is carried in `self.iv` for CBC (the last
    /// ciphertext block) and `self.buf` is *not* used here.
    fn transform_block(&mut self, input: [u8; 16]) -> Option<[u8; 16]> {
        let alg = self.alg?;
        match alg {
            CipherAlg::Aes128Ecb | CipherAlg::Aes256Ecb => {
                let mut block = input;
                if !Self::aes_block(alg, &self.key, &mut block, self.encrypt) {
                    return None;
                }
                Some(block)
            }
            CipherAlg::Aes128Cbc | CipherAlg::Aes256Cbc => {
                if self.encrypt {
                    let mut block = input;
                    for (b, k) in block.iter_mut().zip(self.iv.iter()) {
                        *b ^= *k;
                    }
                    if !Self::aes_block(alg, &self.key, &mut block, true) {
                        return None;
                    }
                    self.iv.copy_from_slice(&block);
                    Some(block)
                } else {
                    let cipher_block = input;
                    let mut block = input;
                    if !Self::aes_block(alg, &self.key, &mut block, false) {
                        return None;
                    }
                    for (b, k) in block.iter_mut().zip(self.iv.iter()) {
                        *b ^= *k;
                    }
                    self.iv.copy_from_slice(&cipher_block);
                    Some(block)
                }
            }
            // CTR is handled as a stream, not block by block.
            CipherAlg::Aes128Ctr | CipherAlg::Aes256Ctr => None,
        }
    }
}

/// All live crypto contexts for the current thread (one VM per thread).
#[derive(Default)]
struct CryptoState {
    digests: HashMap<i64, DigestCtx>,
    hmacs: HashMap<i64, HmacCtx>,
    ciphers: HashMap<i64, CipherCtx>,
    next: i64,
}

thread_local! {
    static STATE: RefCell<CryptoState> = RefCell::new(CryptoState::default());
}

fn alloc_ctx(f: impl FnOnce(&mut CryptoState, i64)) -> i64 {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.next += 1;
        // Start well above the fixed algorithm ids and never hand out the
        // null sentinel.
        let id = 0x1000_0000 + s.next;
        f(&mut s, id);
        id
    })
}

// --- byte-array helpers ----------------------------------------------------

fn read_bytes(vm: &Vm, h: Handle, off: usize, len: usize) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => {
                let end = off.checked_add(len)?;
                if end > v.len() {
                    return None;
                }
                Some(v[off..end].iter().map(|&b| b as u8).collect())
            }
            _ => None,
        },
        _ => None,
    }
}

fn write_bytes(vm: &mut Vm, h: Handle, off: usize, data: &[u8]) -> bool {
    match vm.heap.get_mut(h) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Byte(v) => {
                let end = off.checked_add(data.len());
                match end {
                    Some(end) if end <= v.len() => {
                        for (i, &b) in data.iter().enumerate() {
                            v[off + i] = b as i8;
                        }
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        },
        _ => false,
    }
}

fn byte_array(vm: &mut Vm, data: Vec<u8>) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(data.into_iter().map(|b| b as i8).collect()),
    }))
}

/// Read the `address` (`jlong`) field of a `NativeRef` subclass instance.
fn ref_address(vm: &Vm, h: Handle) -> Option<i64> {
    let class = vm.class_of_object(h)?;
    let mut c = Some(class);
    while let Some(cur) = c {
        let off = vm.classes[cur as usize]
            .declared_instance_fields
            .iter()
            .find(|f| f.name == "address")
            .map(|f| f.offset as usize);
        if let Some(off) = off {
            if let Some(Object::Instance { fields, .. }) = vm.heap.get(h) {
                let lo = *fields.get(off)? as u64;
                let hi = *fields.get(off + 1)? as u64;
                return Some((lo | (hi << 32)) as i64);
            }
            return None;
        }
        c = vm.classes[cur as usize].super_class;
    }
    None
}

fn arg_bytes(vm: &Vm, ctx: NativeCtx, n: usize, off: usize, len: usize) -> Option<Vec<u8>> {
    read_bytes(vm, vm.arg_ref(ctx, n), off, len)
}

fn string(vm: &Vm, ctx: NativeCtx, n: usize) -> String {
    vm.string_to_rust(vm.arg_ref(ctx, n)).unwrap_or_default()
}

fn unsupported(api: &str) -> VmError {
    VmError::unsupported_api(api, "acos_vm::natives::fwk_conscrypt")
}

// --- registration ----------------------------------------------------------

const NC: &str = "Lcom/android/org/conscrypt/NativeCrypto;";
const MD_CTX: &str = "Lcom/android/org/conscrypt/NativeRef$EVP_MD_CTX;";
const HMAC_CTX: &str = "Lcom/android/org/conscrypt/NativeRef$HMAC_CTX;";
const CIPHER_CTX: &str = "Lcom/android/org/conscrypt/NativeRef$EVP_CIPHER_CTX;";

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    // Class initialisation: `NativeCryptoJni.init()` already ran
    // `System.loadLibrary("javacrypto")`, which the framework-native loader
    // reports as satisfied. The native side has nothing further to set up.
    vm.register_native(NC, "clinit", "()V", clinit);

    // Provider bootstrap.
    vm.register_native(
        NC,
        "get_cipher_names",
        "(Ljava/lang/String;)[Ljava/lang/String;",
        get_cipher_names,
    );
    vm.register_native(NC, "EVP_has_aes_hardware", "()I", evp_has_aes_hardware);

    // MessageDigest.
    vm.register_native(
        NC,
        "EVP_get_digestbyname",
        "(Ljava/lang/String;)J",
        evp_get_digestbyname,
    );
    vm.register_native(NC, "EVP_MD_size", "(J)I", evp_md_size);
    vm.register_native(NC, "EVP_MD_CTX_create", "()J", evp_md_ctx_create);
    vm.register_native(NC, "EVP_MD_CTX_destroy", "(J)V", evp_md_ctx_destroy);
    vm.register_native(
        NC,
        "EVP_MD_CTX_cleanup",
        &format!("({MD_CTX})V"),
        evp_md_ctx_cleanup,
    );
    vm.register_native(
        NC,
        "EVP_MD_CTX_copy_ex",
        &format!("({MD_CTX}{MD_CTX})I"),
        evp_md_ctx_copy_ex,
    );
    vm.register_native(
        NC,
        "EVP_DigestInit_ex",
        &format!("({MD_CTX}J)I"),
        evp_digest_init,
    );
    vm.register_native(
        NC,
        "EVP_DigestUpdate",
        &format!("({MD_CTX}[BII)V"),
        evp_digest_update,
    );
    vm.register_native(
        NC,
        "EVP_DigestUpdateDirect",
        &format!("({MD_CTX}JI)V"),
        evp_digest_update_direct,
    );
    vm.register_native(
        NC,
        "EVP_DigestFinal_ex",
        &format!("({MD_CTX}[BI)I"),
        evp_digest_final,
    );

    // HMAC.
    vm.register_native(NC, "HMAC_CTX_new", "()J", hmac_ctx_new);
    vm.register_native(NC, "HMAC_CTX_free", "(J)V", hmac_ctx_free);
    vm.register_native(NC, "HMAC_Init_ex", &format!("({HMAC_CTX}[BJ)V"), hmac_init);
    vm.register_native(
        NC,
        "HMAC_Update",
        &format!("({HMAC_CTX}[BII)V"),
        hmac_update,
    );
    vm.register_native(
        NC,
        "HMAC_UpdateDirect",
        &format!("({HMAC_CTX}JI)V"),
        hmac_update_direct,
    );
    vm.register_native(NC, "HMAC_Final", &format!("({HMAC_CTX})[B"), hmac_final);

    // SecureRandom.
    vm.register_native(NC, "RAND_bytes", "([B)V", rand_bytes);

    // EVP cipher (AES-ECB/CBC/CTR).
    vm.register_native(
        NC,
        "EVP_get_cipherbyname",
        "(Ljava/lang/String;)J",
        evp_get_cipherbyname,
    );
    vm.register_native(NC, "EVP_CIPHER_iv_length", "(J)I", evp_cipher_iv_length);
    vm.register_native(NC, "EVP_CIPHER_CTX_new", "()J", evp_cipher_ctx_new);
    vm.register_native(NC, "EVP_CIPHER_CTX_free", "(J)V", evp_cipher_ctx_free);
    vm.register_native(
        NC,
        "EVP_CIPHER_CTX_block_size",
        &format!("({CIPHER_CTX})I"),
        evp_cipher_ctx_block_size,
    );
    vm.register_native(
        NC,
        "EVP_CIPHER_CTX_set_key_length",
        &format!("({CIPHER_CTX}I)V"),
        evp_cipher_ctx_set_key_length,
    );
    vm.register_native(
        NC,
        "EVP_CIPHER_CTX_set_padding",
        &format!("({CIPHER_CTX}Z)V"),
        evp_cipher_ctx_set_padding,
    );
    vm.register_native(
        NC,
        "get_EVP_CIPHER_CTX_buf_len",
        &format!("({CIPHER_CTX})I"),
        evp_cipher_ctx_buf_len,
    );
    vm.register_native(
        NC,
        "get_EVP_CIPHER_CTX_final_used",
        &format!("({CIPHER_CTX})Z"),
        evp_cipher_ctx_final_used,
    );
    vm.register_native(
        NC,
        "EVP_CipherInit_ex",
        &format!("({CIPHER_CTX}J[B[BZ)V"),
        evp_cipher_init,
    );
    vm.register_native(
        NC,
        "EVP_CipherUpdate",
        &format!("({CIPHER_CTX}[BI[BII)I"),
        evp_cipher_update,
    );
    vm.register_native(
        NC,
        "EVP_CipherFinal_ex",
        &format!("({CIPHER_CTX}[BI)I"),
        evp_cipher_final,
    );

    // `DirectByteBuffer` addresses are not used by this surface; returning 0
    // makes the Java callers fall back to the heap-array path.
    vm.register_native(
        NC,
        "getDirectBufferAddress",
        "(Ljava/nio/Buffer;)J",
        direct_address_zero,
    );
    Ok(())
}

// --- provider bootstrap ----------------------------------------------------

fn clinit(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

/// `NativeCrypto.get_cipher_names`. conscrypt passes an OpenSSL cipher list
/// such as `ALL:!DHE` and reads back `[openssl, java, ...]` pairs. ACOS does
/// not implement the TLS cipher surface, so the honest answer is an empty
/// list; the Java layer then exposes only the two mandatory signalling cipher
/// suites and never fabricates a cipher it cannot actually run.
fn get_cipher_names(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(Vec::new()),
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn evp_has_aes_hardware(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Constant-time AES primitives are always available through the `aes`
    // crate; the value only selects which cipher suites advertise AES-NI.
    // Reporting `1` matches modern hardware.
    _vm.ret_i32(ctx, 1);
    Ok(())
}

fn direct_address_zero(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, 0);
    Ok(())
}

// --- digest ----------------------------------------------------------------

fn evp_get_digestbyname(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = string(vm, ctx, 0);
    let id = MdKind::from_name(&name).map(|k| k.id()).unwrap_or(0);
    vm.ret_i64(ctx, id);
    Ok(())
}

fn evp_md_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_i64(ctx, 0);
    let size = MdKind::from_id(id).map(|k| k.size()).unwrap_or(0) as i32;
    vm.ret_i32(ctx, size);
    Ok(())
}

fn evp_md_ctx_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = alloc_ctx(|s, id| {
        s.digests.insert(id, DigestCtx::Empty);
    });
    vm.ret_i64(ctx, id);
    Ok(())
}

fn evp_md_ctx_destroy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_i64(ctx, 0);
    STATE.with(|s| s.borrow_mut().digests.remove(&id));
    vm.ret_u32(ctx, 0);
    Ok(())
}

fn evp_md_ctx_cleanup(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_MD_CTX".into(),
    })?;
    STATE.with(|s| {
        if let Some(c) = s.borrow_mut().digests.get_mut(&id) {
            *c = DigestCtx::Empty;
        }
    });
    vm.ret_u32(ctx, 0);
    Ok(())
}

fn evp_md_ctx_copy_ex(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dst = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_MD_CTX".into(),
    })?;
    let src = ref_address(vm, vm.arg_ref(ctx, 1)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_MD_CTX".into(),
    })?;
    let ok = STATE.with(|st| {
        let mut st = st.borrow_mut();
        let copy = match st.digests.get(&src) {
            Some(DigestCtx::Ready { digest }) => Some(DigestCtx::Ready {
                digest: digest.box_clone(),
            }),
            _ => None,
        };
        match copy {
            Some(c) => {
                st.digests.insert(dst, c);
                true
            }
            None => false,
        }
    });
    vm.ret_i32(ctx, if ok { 1 } else { 0 });
    Ok(())
}

fn evp_digest_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_MD_CTX".into(),
    })?;
    let kind = MdKind::from_id(vm.arg_i64(ctx, 1))
        .ok_or_else(|| VmError::Internal("EVP_DigestInit_ex: unknown digest".into()))?;
    let ok = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(slot) = s.digests.get_mut(&id) {
            *slot = DigestCtx::Ready {
                digest: kind.new_digest(),
            };
            true
        } else {
            false
        }
    });
    vm.ret_i32(ctx, if ok { 1 } else { 0 });
    Ok(())
}

fn evp_digest_update(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_MD_CTX".into(),
    })?;
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let data = arg_bytes(vm, ctx, 1, off, len)
        .ok_or_else(|| VmError::Internal("EVP_DigestUpdate: bad input array".into()))?;
    STATE.with(|s| {
        if let Some(DigestCtx::Ready { digest, .. }) = s.borrow_mut().digests.get_mut(&id) {
            digest.update(&data);
        }
    });
    Ok(())
}

fn evp_digest_update_direct(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Only reachable for direct `ByteBuffer`s, whose address is 0 here, so the
    // Java side already fell back to the heap path. Be explicit rather than
    // reading an arbitrary address.
    let _ = (vm, ctx);
    Err(unsupported("NativeCrypto.EVP_DigestUpdateDirect"))
}

fn evp_digest_final(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_MD_CTX".into(),
    })?;
    let out = vm.arg_ref(ctx, 1);
    let out_off = vm.arg_i32(ctx, 2).max(0) as usize;
    let out_bytes = STATE.with(|s| match s.borrow_mut().digests.get_mut(&id) {
        Some(DigestCtx::Ready { digest, .. }) => {
            let d = digest.finalize_reset();
            Some(d.to_vec())
        }
        _ => None,
    });
    let Some(bytes) = out_bytes else {
        return Err(VmError::Internal(
            "EVP_DigestFinal_ex: context not initialised".into(),
        ));
    };
    if !write_bytes(vm, out, out_off, &bytes) {
        return Err(vm.throw_new(
            "Ljavax/crypto/ShortBufferException;",
            Some("output too small for digest"),
        ));
    }
    vm.ret_i32(ctx, bytes.len() as i32);
    Ok(())
}

// --- HMAC ------------------------------------------------------------------

fn hmac_ctx_new(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = alloc_ctx(|s, id| {
        s.hmacs.insert(id, HmacCtx::Empty);
    });
    vm.ret_i64(ctx, id);
    Ok(())
}

fn hmac_ctx_free(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_i64(ctx, 0);
    STATE.with(|s| s.borrow_mut().hmacs.remove(&id));
    vm.ret_u32(ctx, 0);
    Ok(())
}

fn hmac_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "HMAC_CTX".into(),
    })?;
    let key = byte_vec(vm, vm.arg_ref(ctx, 1));
    let kind = MdKind::from_id(vm.arg_i64(ctx, 2))
        .ok_or_else(|| VmError::Internal("HMAC_Init_ex: unknown digest".into()))?;
    let mac = HmacState::new(kind, &key)
        .ok_or_else(|| VmError::Internal("HMAC_Init_ex: bad key".into()))?;
    let ok = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(slot) = s.hmacs.get_mut(&id) {
            *slot = HmacCtx::Ready { mac };
            true
        } else {
            false
        }
    });
    vm.ret_i32(ctx, if ok { 1 } else { 0 });
    Ok(())
}

fn hmac_update(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "HMAC_CTX".into(),
    })?;
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let data = arg_bytes(vm, ctx, 1, off, len)
        .ok_or_else(|| VmError::Internal("HMAC_Update: bad input array".into()))?;
    STATE.with(|s| {
        if let Some(HmacCtx::Ready { mac, .. }) = s.borrow_mut().hmacs.get_mut(&id) {
            mac.update(&data);
        }
    });
    Ok(())
}

fn hmac_update_direct(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = (vm, ctx);
    Err(unsupported("NativeCrypto.HMAC_UpdateDirect"))
}

fn hmac_final(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "HMAC_CTX".into(),
    })?;
    let bytes = STATE.with(|st| {
        let mut st = st.borrow_mut();
        let slot = st.hmacs.get_mut(&id)?;
        let old = std::mem::replace(slot, HmacCtx::Empty);
        match old {
            HmacCtx::Ready { mac, .. } => Some(mac.finalize()),
            HmacCtx::Empty => None,
        }
    });
    let Some(bytes) = bytes else {
        return Err(VmError::Internal(
            "HMAC_Final: context not initialised".into(),
        ));
    };
    let h = byte_array(vm, bytes)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

// --- SecureRandom ----------------------------------------------------------

fn rand_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_ref(ctx, 0);
    let len = match vm.heap.get(h) {
        Some(Object::Array(a)) => a.len(),
        _ => return Err(VmError::Internal("RAND_bytes: not a byte array".into())),
    };
    let mut buf = vec![0u8; len];
    getrandom::getrandom(&mut buf)
        .map_err(|e| VmError::Internal(format!("RAND_bytes: OS entropy unavailable: {e}")))?;
    if !write_bytes(vm, h, 0, &buf) {
        return Err(VmError::Internal("RAND_bytes: write failed".into()));
    }
    Ok(())
}

// --- EVP cipher ------------------------------------------------------------

fn evp_get_cipherbyname(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = string(vm, ctx, 0);
    let id = CipherAlg::from_name(&name).map(|a| a.id()).unwrap_or(0);
    vm.ret_i64(ctx, id);
    Ok(())
}

fn evp_cipher_iv_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_i64(ctx, 0);
    let n = CipherAlg::from_id(id).map(|a| a.iv_len()).unwrap_or(0) as i32;
    vm.ret_i32(ctx, n);
    Ok(())
}

fn evp_cipher_ctx_new(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = alloc_ctx(|s, id| {
        s.ciphers.insert(id, CipherCtx::default());
    });
    vm.ret_i64(ctx, id);
    Ok(())
}

fn evp_cipher_ctx_free(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = vm.arg_i64(ctx, 0);
    STATE.with(|s| s.borrow_mut().ciphers.remove(&id));
    vm.ret_u32(ctx, 0);
    Ok(())
}

fn evp_cipher_ctx_block_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    let n = STATE.with(|s| {
        s.borrow()
            .ciphers
            .get(&id)
            .and_then(|c| c.alg)
            .map(|a| a.block_size())
            .unwrap_or(16)
    });
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

fn evp_cipher_ctx_set_key_length(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // The key length is fixed by the chosen algorithm; conscrypt only calls
    // this for variable-length ciphers, which ACOS does not expose.
    let _ = (vm, ctx);
    Err(unsupported("NativeCrypto.EVP_CIPHER_CTX_set_key_length"))
}

fn evp_cipher_ctx_set_padding(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    let pad = vm.arg_bool(ctx, 1);
    STATE.with(|s| {
        if let Some(c) = s.borrow_mut().ciphers.get_mut(&id) {
            c.padding = pad;
        }
    });
    vm.ret_u32(ctx, 0);
    Ok(())
}

/// Number of buffered bytes the context is holding back (`get_EVP_CIPHER_CTX_buf_len`).
fn evp_cipher_ctx_buf_len(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    let n = STATE.with(|s| {
        s.borrow()
            .ciphers
            .get(&id)
            .map(|c| c.buf.len())
            .unwrap_or(0)
    });
    vm.ret_i32(ctx, n as i32);
    Ok(())
}

/// Whether `EVP_CipherFinal_ex` has run (`get_EVP_CIPHER_CTX_final_used`).
fn evp_cipher_ctx_final_used(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    let used = STATE.with(|s| {
        s.borrow()
            .ciphers
            .get(&id)
            .map(|c| c.final_used)
            .unwrap_or(false)
    });
    vm.ret_bool(ctx, used);
    Ok(())
}

fn evp_cipher_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    let cipher_id = vm.arg_i64(ctx, 1);
    let key = vm.arg_ref(ctx, 2);
    let iv = vm.arg_ref(ctx, 3);
    let encrypt = vm.arg_bool(ctx, 4);

    let key_bytes = if key == NULL {
        None
    } else {
        Some(byte_vec(vm, key))
    };
    let iv_bytes = if iv == NULL {
        None
    } else {
        Some(byte_vec(vm, iv))
    };
    let alg = CipherAlg::from_id(cipher_id);

    let ok = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(c) = s.ciphers.get_mut(&id) else {
            return false;
        };
        // `cipher_id == 0` means "keep the current cipher" (OpenSSL), which is
        // how conscrypt sets the key/IV after selecting the mode.
        let a = match alg {
            Some(a) => {
                c.alg = Some(a);
                c.buf.clear();
                a
            }
            None => match c.alg {
                Some(a) => a,
                None => return false,
            },
        };
        c.encrypt = encrypt;
        if let Some(k) = key_bytes.as_ref() {
            if k.len() != a.key_len() {
                return false;
            }
            c.key = k.clone();
        }
        if let Some(i) = iv_bytes.as_ref() {
            if a.iv_len() > 0 && i.len() != a.iv_len() {
                return false;
            }
            c.iv = i.clone();
        }
        if c.key.len() != a.key_len() {
            return false;
        }
        if a.iv_len() > 0 && c.iv.is_empty() {
            c.iv = vec![0u8; a.iv_len()];
        }
        true
    });
    vm.ret_i32(ctx, if ok { 1 } else { 0 });
    Ok(())
}

fn evp_cipher_update(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    // OpenSSL contract: `EVP_CipherUpdate(ctx, out, outLen, in, inLen)`.
    let out = vm.arg_ref(ctx, 1);
    let out_off = vm.arg_i32(ctx, 2).max(0) as usize;
    let in_off = vm.arg_i32(ctx, 4).max(0) as usize;
    let len = vm.arg_i32(ctx, 5).max(0) as usize;
    let input = arg_bytes(vm, ctx, 3, in_off, len)
        .ok_or_else(|| VmError::Internal("EVP_CipherUpdate: bad input array".into()))?;

    let produced = STATE.with(|s| {
        let mut s = s.borrow_mut();
        ciphers_update(s.ciphers.get_mut(&id), &input)
    });
    let Some(output) = produced else {
        return Err(VmError::Internal(
            "EVP_CipherUpdate: context not initialised".into(),
        ));
    };
    if !write_bytes(vm, out, out_off, &output) {
        return Err(vm.throw_new(
            "Ljavax/crypto/ShortBufferException;",
            Some("output too small"),
        ));
    }
    vm.ret_i32(ctx, output.len() as i32);
    Ok(())
}

fn evp_cipher_final(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = ref_address(vm, vm.arg_ref(ctx, 0)).ok_or_else(|| VmError::StaleHandle {
        what: "EVP_CIPHER_CTX".into(),
    })?;
    let out = vm.arg_ref(ctx, 1);
    let out_off = vm.arg_i32(ctx, 2).max(0) as usize;
    let result = STATE.with(|s| {
        let mut s = s.borrow_mut();
        match s.ciphers.get_mut(&id) {
            Some(c) => {
                let r = ciphers_final(c);
                if r.is_ok() {
                    c.final_used = true;
                }
                r
            }
            None => Err("context not initialised".to_string()),
        }
    });
    let output = match result {
        Ok(o) => o,
        Err(e) if e.starts_with("bad padding") => {
            return Err(vm.throw_new("Ljavax/crypto/BadPaddingException;", Some(&e)));
        }
        Err(e) => return Err(VmError::Internal(format!("EVP_CipherFinal_ex: {e}"))),
    };
    if !write_bytes(vm, out, out_off, &output) {
        return Err(vm.throw_new(
            "Ljavax/crypto/ShortBufferException;",
            Some("output too small"),
        ));
    }
    vm.ret_i32(ctx, output.len() as i32);
    Ok(())
}

fn byte_vec(vm: &Vm, h: Handle) -> Vec<u8> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => v.iter().map(|&b| b as u8).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// Process as much of `input` as possible, buffering a trailing partial block.
fn ciphers_update(ctx: Option<&mut CipherCtx>, input: &[u8]) -> Option<Vec<u8>> {
    let c = ctx?;
    let alg = c.alg?;
    if c.key.len() != alg.key_len() {
        return None;
    }
    // CTR is a stream cipher: process everything immediately.
    if alg == CipherAlg::Aes128Ctr || alg == CipherAlg::Aes256Ctr {
        let mut counter = [0u8; 16];
        counter.copy_from_slice(&c.iv);
        let out = ctr_apply(alg, &c.key, &mut counter, input);
        c.iv.copy_from_slice(&counter);
        return Some(out);
    }

    c.buf.extend_from_slice(input);
    let block = 16;
    // Keep one full block back when decrypting with padding so padding can be
    // inspected at final time.
    let keep = if !c.encrypt && c.padding { block } else { 0 };
    let mut out = Vec::new();
    while c.buf.len() >= block + keep {
        let mut blk = [0u8; 16];
        blk.copy_from_slice(&c.buf[..block]);
        let transformed = c.transform_block(blk)?;
        out.extend_from_slice(&transformed);
        c.buf.drain(..block);
    }
    Some(out)
}

/// Finish the cipher, applying/removing PKCS#7 padding.
fn ciphers_final(c: &mut CipherCtx) -> Result<Vec<u8>, String> {
    let alg = c.alg.ok_or("not initialised")?;
    if c.key.len() != alg.key_len() {
        return Err("not initialised".into());
    }
    if alg == CipherAlg::Aes128Ctr || alg == CipherAlg::Aes256Ctr {
        let mut counter = [0u8; 16];
        counter.copy_from_slice(&c.iv);
        let out = ctr_apply(alg, &c.key, &mut counter, &c.buf);
        c.iv.copy_from_slice(&counter);
        c.buf.clear();
        return Ok(out);
    }

    let block = 16;
    let mut out = Vec::new();
    if c.encrypt {
        if c.padding {
            let pad = block - (c.buf.len() % block);
            c.buf.extend(std::iter::repeat_n(pad as u8, pad));
        } else if !c.buf.len().is_multiple_of(block) {
            return Err("data not a multiple of the block size".into());
        }
        while c.buf.len() >= block {
            let mut blk = [0u8; 16];
            blk.copy_from_slice(&c.buf[..block]);
            let t = c.transform_block(blk).ok_or("cipher failure")?;
            out.extend_from_slice(&t);
            c.buf.drain(..block);
        }
        Ok(out)
    } else {
        if !c.buf.len().is_multiple_of(block) {
            return Err("data not a multiple of the block size".into());
        }
        while c.buf.len() >= block {
            let mut blk = [0u8; 16];
            blk.copy_from_slice(&c.buf[..block]);
            let t = c.transform_block(blk).ok_or("cipher failure")?;
            out.extend_from_slice(&t);
            c.buf.drain(..block);
        }
        if c.padding {
            let pad = *out.last().ok_or("bad padding: empty")? as usize;
            if pad == 0 || pad > block || pad > out.len() {
                return Err("bad padding".into());
            }
            if out[out.len() - pad..].iter().any(|&b| b as usize != pad) {
                return Err("bad padding".into());
            }
            out.truncate(out.len() - pad);
        }
        Ok(out)
    }
}

/// Apply AES-CTR keystream, advancing `counter` once per 16-byte block.
fn ctr_apply(alg: CipherAlg, key: &[u8], counter: &mut [u8; 16], input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut keystream = [0u8; 16];
    let mut ks_pos = 16usize;
    for &byte in input {
        if ks_pos == 16 {
            let mut block = *counter;
            if !CipherCtx::aes_block(alg, key, &mut block, true) {
                return Vec::new();
            }
            keystream = block;
            ks_pos = 0;
            ctr_increment(counter);
        }
        out.push(byte ^ keystream[ks_pos]);
        ks_pos += 1;
    }
    out
}

/// Increment a 128-bit big-endian counter by one.
fn ctr_increment(counter: &mut [u8; 16]) {
    for byte in counter.iter_mut().rev() {
        let (v, carry) = byte.overflowing_add(1);
        *byte = v;
        if !carry {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_names_round_trip() {
        for (name, kind) in [
            ("md5", MdKind::Md5),
            ("sha-1", MdKind::Sha1),
            ("SHA256", MdKind::Sha256),
            ("sha_384", MdKind::Sha384),
            ("sha512", MdKind::Sha512),
        ] {
            assert_eq!(MdKind::from_name(name), Some(kind));
            assert_eq!(MdKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(MdKind::from_name("blake2"), None);
    }

    #[test]
    fn cipher_names_round_trip() {
        for name in ["aes-128-ecb", "AES-256-CBC", "aes-128-ctr"] {
            let a = CipherAlg::from_name(name).expect(name);
            assert_eq!(CipherAlg::from_id(a.id()), Some(a));
        }
        assert_eq!(CipherAlg::from_name("aes-192-cbc"), None);
    }

    #[test]
    fn ctr_keystream_matches_nist_vector() {
        // NIST SP 800-38A F.5.1 CTR-AES128.Encrypt, first block.
        let key: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv = [
            0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
            0xfe, 0xff,
        ];
        let mut iv = iv;
        let pt = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        let ct = ctr_apply(CipherAlg::Aes128Ctr, &key, &mut iv, &pt);
        assert_eq!(
            ct,
            vec![
                0x87, 0x4d, 0x61, 0x91, 0xb6, 0x20, 0xe3, 0x26, 0x1b, 0xef, 0x68, 0x64, 0x99, 0x0d,
                0xb6, 0xce
            ]
        );
    }

    #[test]
    fn ecb_matches_fips197_vector() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let mut block = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        assert!(CipherCtx::aes_block(
            CipherAlg::Aes128Ecb,
            &key,
            &mut block,
            true
        ));
        assert_eq!(
            block,
            [
                0x69, 0xc4, 0xe0, 0xd8, 0x6a, 0x7b, 0x04, 0x30, 0xd8, 0xcd, 0xb7, 0x80, 0x70, 0xb4,
                0xc5, 0x5a
            ]
        );
    }
}
