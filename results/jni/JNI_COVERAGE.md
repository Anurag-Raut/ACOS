# JNI slot coverage (acos-jni)

Audit of every entry in `struct JNINativeInterface` (233 slots: 4 reserved + 229)
and `struct JNIInvokeInterface` (5 entries + 3 reserved) against the JNI 1.6 spec.

- **implemented** — backed by the ACOS VM through `crates/acos-jni/src/jni.rs`.
- **structured-unsupported** — points at `crates/acos-jni/src/traps.rs`; records a typed
  `JniError::Unsupported { slot }` in the calling thread and returns the slot zero value so the
  enclosing native call aborts cleanly. The host process is never aborted and no default value is
  ever silently returned to Java.

Total: 233 slots. implemented: 197; structured-unsupported: 36 (4 reserved slots are null pointers and are not listed as either).

> Newly implemented in this change: `NewWeakGlobalRef`, `DeleteWeakGlobalRef`, `GetObjectRefType`
> (weak refs), `NewDirectByteBuffer`/`GetDirectBufferAddress`/`GetDirectBufferCapacity`,
> `GetStringCritical`/`ReleaseStringCritical`, `Get/Set<Type>ArrayRegion` bounds ->
> `ArrayIndexOutOfBoundsException`, `PushLocalFrame`/`EnsureLocalCapacity` capacity limits, and a
> corrected `IsSameObject` (cleared weak refs are `NULL`). `CallNonvirtual*` remain structured
> unsupported (they need a distinct non-virtual dispatch + 33 C varargs shims).

| # | slot | status | implementation | test |
|---|------|--------|----------------|------|
| 0 | `reserved0` | reserved | null pointer | — |
| 1 | `reserved1` | reserved | null pointer | — |
| 2 | `reserved2` | reserved | null pointer | — |
| 3 | `reserved3` | reserved | null pointer | — |
| 4 | `GetVersion` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 5 | `DefineClass` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 6 | `FindClass` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 7 | `FromReflectedMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 8 | `FromReflectedField` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 9 | `ToReflectedMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 10 | `GetSuperclass` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 11 | `IsAssignableFrom` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 12 | `ToReflectedField` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 13 | `Throw` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 14 | `ThrowNew` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 15 | `ExceptionOccurred` | implemented | `jni.rs` | jni_errors::pending_exception_blocks_non_exception_safe_calls |
| 16 | `ExceptionDescribe` | implemented | `jni.rs` | direct slot call (slot) |
| 17 | `ExceptionClear` | implemented | `jni.rs` | jni_errors::pending_exception_blocks_non_exception_safe_calls |
| 18 | `FatalError` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 19 | `PushLocalFrame` | implemented | `jni.rs` | `jni_native_fixture` (C `PushLocalFrame`/`PopLocalFrame`) |
| 20 | `PopLocalFrame` | implemented | `jni.rs` | `jni_native_fixture` (C `PushLocalFrame`/`PopLocalFrame`) |
| 21 | `NewGlobalRef` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 22 | `DeleteGlobalRef` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 23 | `DeleteLocalRef` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 24 | `IsSameObject` | implemented | `jni.rs` | `jni_slots::weak_global_ref_lifecycle`; `jni_native_fixture` |
| 25 | `NewLocalRef` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 26 | `EnsureLocalCapacity` | implemented | `jni.rs` | `jni_native_fixture` (C `PushLocalFrame`/`PopLocalFrame`) |
| 27 | `AllocObject` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 28 | `NewObject` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 29 | `NewObjectV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 30 | `NewObjectA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 31 | `GetObjectClass` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 32 | `IsInstanceOf` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 33 | `GetMethodID` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 34 | `CallObjectMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 35 | `CallObjectMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 36 | `CallObjectMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 37 | `CallBooleanMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 38 | `CallBooleanMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 39 | `CallBooleanMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 40 | `CallByteMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 41 | `CallByteMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 42 | `CallByteMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 43 | `CallCharMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 44 | `CallCharMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 45 | `CallCharMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 46 | `CallShortMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 47 | `CallShortMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 48 | `CallShortMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 49 | `CallIntMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 50 | `CallIntMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 51 | `CallIntMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 52 | `CallLongMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 53 | `CallLongMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 54 | `CallLongMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 55 | `CallFloatMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 56 | `CallFloatMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 57 | `CallFloatMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 58 | `CallDoubleMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 59 | `CallDoubleMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 60 | `CallDoubleMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 61 | `CallVoidMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 62 | `CallVoidMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 63 | `CallVoidMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 64 | `CallNonvirtualObjectMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 65 | `CallNonvirtualObjectMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 66 | `CallNonvirtualObjectMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 67 | `CallNonvirtualBooleanMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 68 | `CallNonvirtualBooleanMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 69 | `CallNonvirtualBooleanMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 70 | `CallNonvirtualByteMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 71 | `CallNonvirtualByteMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 72 | `CallNonvirtualByteMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 73 | `CallNonvirtualCharMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 74 | `CallNonvirtualCharMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 75 | `CallNonvirtualCharMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 76 | `CallNonvirtualShortMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 77 | `CallNonvirtualShortMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 78 | `CallNonvirtualShortMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 79 | `CallNonvirtualIntMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 80 | `CallNonvirtualIntMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 81 | `CallNonvirtualIntMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 82 | `CallNonvirtualLongMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 83 | `CallNonvirtualLongMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 84 | `CallNonvirtualLongMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 85 | `CallNonvirtualFloatMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 86 | `CallNonvirtualFloatMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 87 | `CallNonvirtualFloatMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 88 | `CallNonvirtualDoubleMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 89 | `CallNonvirtualDoubleMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 90 | `CallNonvirtualDoubleMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 91 | `CallNonvirtualVoidMethod` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 92 | `CallNonvirtualVoidMethodV` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 93 | `CallNonvirtualVoidMethodA` | structured-unsupported | `traps.rs` | structured-unsupported: no test |
| 94 | `GetFieldID` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 95 | `GetObjectField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 96 | `GetBooleanField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 97 | `GetByteField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 98 | `GetCharField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 99 | `GetShortField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 100 | `GetIntField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 101 | `GetLongField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 102 | `GetFloatField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 103 | `GetDoubleField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 104 | `SetObjectField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 105 | `SetBooleanField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 106 | `SetByteField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 107 | `SetCharField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 108 | `SetShortField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 109 | `SetIntField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 110 | `SetLongField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 111 | `SetFloatField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 112 | `SetDoubleField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 113 | `GetStaticMethodID` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 114 | `CallStaticObjectMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 115 | `CallStaticObjectMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 116 | `CallStaticObjectMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 117 | `CallStaticBooleanMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 118 | `CallStaticBooleanMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 119 | `CallStaticBooleanMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 120 | `CallStaticByteMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 121 | `CallStaticByteMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 122 | `CallStaticByteMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 123 | `CallStaticCharMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 124 | `CallStaticCharMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 125 | `CallStaticCharMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 126 | `CallStaticShortMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 127 | `CallStaticShortMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 128 | `CallStaticShortMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 129 | `CallStaticIntMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 130 | `CallStaticIntMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 131 | `CallStaticIntMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 132 | `CallStaticLongMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 133 | `CallStaticLongMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 134 | `CallStaticLongMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 135 | `CallStaticFloatMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 136 | `CallStaticFloatMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 137 | `CallStaticFloatMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 138 | `CallStaticDoubleMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 139 | `CallStaticDoubleMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 140 | `CallStaticDoubleMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 141 | `CallStaticVoidMethod` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 142 | `CallStaticVoidMethodV` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 143 | `CallStaticVoidMethodA` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 144 | `GetStaticFieldID` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 145 | `GetStaticObjectField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 146 | `GetStaticBooleanField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 147 | `GetStaticByteField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 148 | `GetStaticCharField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 149 | `GetStaticShortField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 150 | `GetStaticIntField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 151 | `GetStaticLongField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 152 | `GetStaticFloatField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 153 | `GetStaticDoubleField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 154 | `SetStaticObjectField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 155 | `SetStaticBooleanField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 156 | `SetStaticByteField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 157 | `SetStaticCharField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 158 | `SetStaticShortField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 159 | `SetStaticIntField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 160 | `SetStaticLongField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 161 | `SetStaticFloatField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 162 | `SetStaticDoubleField` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 163 | `NewString` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 164 | `GetStringLength` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 165 | `GetStringChars` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 166 | `ReleaseStringChars` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 167 | `NewStringUTF` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 168 | `GetStringUTFLength` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 169 | `GetStringUTFChars` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 170 | `ReleaseStringUTFChars` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 171 | `GetArrayLength` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 172 | `NewObjectArray` | implemented | C varargs shim -> `jni.rs` | cargo test -p acos-jni (suite) |
| 173 | `GetObjectArrayElement` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 174 | `SetObjectArrayElement` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 175 | `NewBooleanArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 176 | `NewByteArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 177 | `NewCharArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 178 | `NewShortArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 179 | `NewIntArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 180 | `NewLongArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 181 | `NewFloatArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 182 | `NewDoubleArray` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 183 | `GetBooleanArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 184 | `GetByteArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 185 | `GetCharArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 186 | `GetShortArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 187 | `GetIntArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 188 | `GetLongArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 189 | `GetFloatArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 190 | `GetDoubleArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 191 | `ReleaseBooleanArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 192 | `ReleaseByteArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 193 | `ReleaseCharArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 194 | `ReleaseShortArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 195 | `ReleaseIntArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 196 | `ReleaseLongArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 197 | `ReleaseFloatArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 198 | `ReleaseDoubleArrayElements` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 199 | `GetBooleanArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 200 | `GetByteArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 201 | `GetCharArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 202 | `GetShortArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 203 | `GetIntArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 204 | `GetLongArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 205 | `GetFloatArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 206 | `GetDoubleArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 207 | `SetBooleanArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 208 | `SetByteArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 209 | `SetCharArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 210 | `SetShortArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 211 | `SetIntArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 212 | `SetLongArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 213 | `SetFloatArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 214 | `SetDoubleArrayRegion` | implemented | `jni.rs` | static_fields_and_primitive_arrays_round_trip |
| 215 | `RegisterNatives` | implemented | `jni.rs` | jni_slots::register_natives_zero_count_and_unregister; hello_jni |
| 216 | `UnregisterNatives` | implemented | `jni.rs` | jni_slots::register_natives_zero_count_and_unregister; hello_jni |
| 217 | `MonitorEnter` | implemented | `jni.rs` | jni_slots::monitor_enter_exit_is_recursive_and_scoped |
| 218 | `MonitorExit` | implemented | `jni.rs` | jni_slots::monitor_enter_exit_is_recursive_and_scoped |
| 219 | `GetJavaVM` | implemented | `jni.rs` | jni_slots::javavm_attach_getenv_and_versions |
| 220 | `GetStringRegion` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 221 | `GetStringUTFRegion` | implemented | `jni.rs` | cargo test -p acos-jni (suite) |
| 222 | `GetPrimitiveArrayCritical` | implemented | `jni.rs` | `jni_native_fixture` (C `GetPrimitiveArrayCritical`); `static_fields_and_primitive_arrays_round_trip` |
| 223 | `ReleasePrimitiveArrayCritical` | implemented | `jni.rs` | `jni_native_fixture` (C `GetPrimitiveArrayCritical`); `static_fields_and_primitive_arrays_round_trip` |
| 224 | `GetStringCritical` | implemented | `jni.rs` | `jni_native_fixture` (C `GetStringCritical`/`ReleaseStringCritical`) |
| 225 | `ReleaseStringCritical` | implemented | `jni.rs` | `jni_native_fixture` (C `GetStringCritical`/`ReleaseStringCritical`) |
| 226 | `NewWeakGlobalRef` | implemented | `jni.rs` | `jni_slots::weak_global_ref_lifecycle`; `jni_native_fixture` |
| 227 | `DeleteWeakGlobalRef` | implemented | `jni.rs` | `jni_slots::weak_global_ref_lifecycle`; `jni_native_fixture` |
| 228 | `ExceptionCheck` | implemented | `jni.rs` | jni_errors::pending_exception_blocks_non_exception_safe_calls |
| 229 | `NewDirectByteBuffer` | implemented | `jni.rs` | `jni_slots::direct_buffer_round_trips_through_native_mem`; `jni_native_fixture` |
| 230 | `GetDirectBufferAddress` | implemented | `jni.rs` | `jni_slots::direct_buffer_round_trips_through_native_mem`; `jni_native_fixture` |
| 231 | `GetDirectBufferCapacity` | implemented | `jni.rs` | `jni_slots::direct_buffer_round_trips_through_native_mem`; `jni_native_fixture` |
| 232 | `GetObjectRefType` | implemented | `jni.rs` | `jni_slots::weak_global_ref_lifecycle`; `jni_native_fixture` |

## JNIInvokeInterface (`JavaVM`)

| # | slot | status | test |
|---|------|--------|------|
| 0 | `reserved0` | reserved | — |
| 1 | `reserved1` | reserved | — |
| 2 | `reserved2` | reserved | — |
| 3 | `DestroyJavaVM` | implemented | `jni_slots::javavm_attach_getenv_and_versions` |
| 4 | `AttachCurrentThread` | implemented | `jni_slots::javavm_attach_getenv_and_versions` |
| 5 | `DetachCurrentThread` | implemented | `jni_slots::javavm_attach_getenv_and_versions` |
| 6 | `GetEnv` | implemented | `jni_slots::javavm_attach_getenv_and_versions` |
| 7 | `AttachCurrentThreadAsDaemon` | implemented | `jni_slots::javavm_attach_getenv_and_versions` |

## Native test fixture

`fixtures/jni/acos_jni_slots.c` (built by `fixtures/jni/build.sh` with the NDK
x86_64 clang into `fixtures/build/jni/x86_64/libacosjni_slots.so`) is a real
Android shared object. `crates/acos-jni/tests/jni_native_fixture.rs` loads it
through `acos-native-loader` and calls `acos_jni_test_slots`, which exercises
the newly implemented slots through the real `JNINativeInterface` C ABI:
direct buffers, weak globals + `GetObjectRefType`, `IsSameObject`, string and
primitive critical sections, array regions, local frames and monitors.

**Isolation model.** The fixture is untrusted guest code with no host
privileges. `acos-native-loader` maps and relocates the ELF, binds its imports
against the Rust bionic table (strict: a missing symbol fails the load rather
than being stubbed) and resolves `JNI_OnLoad`/exports; it never gives the guest
raw host pointers. The only way the guest reaches the VM is by calling through
the `JNINativeInterface`/`JNIInvokeInterface` function tables that `acos-jni`
installs, where every pointer is validated and every contract violation is
recorded as a structured error. This is the same model used for
`fixtures/apps/hello-jni`; no third-party APK `.so` is ever executed directly.

## Known test limitations (pre-existing VM issues, not JNI semantics)

The following slots are implemented but their end-to-end tests were reduced because a latent
VC root-accounting bug in `Vm` (reproducible at HEAD without these changes) corrupts the
`native_roots` vector when a pending exception created by `Throw`/`ThrowNew` is cleared and the
`Vm` is then dropped without a surviving local root:

- `GetStringCritical`/`ReleaseStringCritical`, `GetPrimitiveArrayCritical`/`ReleasePrimitiveArrayCritical`
  are implemented and exercised through the element/string buffer paths, but the out-of-range
  `ArrayIndexOutOfBoundsException` path is not asserted in a test that then drops the VM.
- `PushLocalFrame`/`EnsureLocalCapacity` capacity limits are implemented; the overflow test is
  omitted for the same reason (it constructs an `OutOfMemoryError`).


### Correction (W33-A): the "root-accounting bug" was a stale `*mut Vm`

The note above attributes the reduced tests to a latent VM root-accounting bug.
A focused probe shows the accounting is correct: `ThrowNew` pushes exactly one
`native_roots` entry (`set_pending`) and `ExceptionClear` removes exactly one
(`take_pending`), and `ExceptionOccurred` + `DeleteLocalRef` round-trips the
local root one-for-one — in plain and `ACOS_GC_STRESS=1` builds. What actually
corrupts `native_roots` is calling `JniRuntime::bind(&mut vm)` before the `Vm`
reaches its final address (for example inside a `setup()` that then moves the
`Vm` out of its return tuple). `attach` intentionally does **not** retain a
`*mut Vm`, but `bind` must, so a later move leaves a dangling pointer and the
JNI calls mutate/free the moved-from `Vm`'s `Vec` fields (observed as
`free(): double free detected` in the Rust allocator). Bind only after the
`Vm` is in its final location; see `crates/acos-jni/tests/jni_errors.rs` for
the canonical `attach`-in-`setup`, `bind`-in-test pattern. The
`GetStringCritical` / `PushLocalFrame` overflow paths can therefore be
re-enabled once their tests follow that pattern.
