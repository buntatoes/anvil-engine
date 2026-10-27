// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
// C++20 Register Virtual Machine, NaN-Boxed Heap, Hidden Classes, and Inline Cache Engine.
// Author: Buntato
//
// Freestanding C++20 header used by:
//   - anvil_vm_heap.cpp     (String interning, AnvilShape transition trees, Mark-Sweep GC)
//   - anvil_vm_ic.cpp       (4-way Polymorphic Inline Cache & DOM selector IC table)
//   - anvil_vm_compiler.cpp (C++20 Lexer, Recursive-Descent/Pratt Parser, Register Bytecode Compiler)
//   - anvil_jit_accel.cpp   (Register VM dispatch loop, hot-loop OSR kernels, extern "C" ABI)
//
// Designed to compile cleanly with zero external runtime dependencies under both:
//   - Windows MSVC: cl.exe /c /O2 /fp:precise /std:c++20 /Zl
//   - Linux GCC/Clang: g++ -c -O3 -std=c++20 -fno-exceptions -fno-rtti

#ifndef ANVIL_VM_HPP
#define ANVIL_VM_HPP

using u8  = unsigned char;
using u16 = unsigned short;
using u32 = unsigned int;
using u64 = unsigned long long;
using i32 = int;
using i64 = long long;
using f64 = double;

namespace anvil_vm {

using ::u8;
using ::u16;
using ::u32;
using ::u64;
using ::i32;
using ::i64;
using ::f64;

// ============================================================================
// 1. Deterministic Engine Limits & Budgets
// ============================================================================

constexpr u32 ANVIL_VM_KERNEL_VERSION    = 18u;
constexpr u32 MAX_VM_STEPS              = 100000u;
constexpr u32 MAX_REGISTERS_PER_FRAME   = 65535u;
constexpr u32 INIT_REGISTERS_PER_FRAME  = 64u;
constexpr u32 MAX_CALL_ARGS             = 256u;
constexpr u32 ANVIL_ARENA_RESERVE_BYTES = 256u * 1024u * 1024u;
constexpr u32 ANVIL_DEFAULT_HEAP_BUDGET = 64u * 1024u * 1024u;
constexpr u32 ANVIL_DEFAULT_CALL_DEPTH  = 10000u;
constexpr u32 ANVIL_DEFAULT_PARSE_STACK_BUDGET = 1024u * 1024u;
constexpr u32 DICT_THRESHOLD            = 64u;
constexpr u32 MAX_BYTECODE_INSTRUCTIONS = 4096u;
constexpr u32 ANVIL_MAX_PROGRAM_INSTRUCTIONS = 16777215u;
constexpr u32 INIT_CODE_BUFFER = 256u;
constexpr u32 MAX_DOM_IC_ENTRIES        = 128u;
constexpr u32 HOT_LOOP_THRESHOLD        = 16u;
constexpr u32 MAX_TRY_DEPTH             = 64u;

// Returned by the allocators and tables when they are full.
constexpr u32 NO_SLOT = 0xFFFFFFFFu;
// A dictionary-mode object carries this in place of a shape id.
constexpr u32 DICT_SHAPE = 0xFFFFFFFEu;
constexpr u16 NO_GLOBAL = 0xFFFFu;

// Built-in object kinds stored in AnvilHeapObject::builtin_kind.
enum class BuiltinObjectKind : u8 {
    Plain       = 0u,
    Map         = 1u,
    Set         = 2u,
    ArrayBuffer = 3u,
    Uint8Array  = 4u,
    Uint16Array = 5u,
    Int32Array  = 6u,
    Uint32Array = 7u,
    Float64Array = 8u,
    Error       = 9u,
};

// Error subtype IDs used by NewBuiltin.
enum class BuiltinCtorKind : u8 {
    Error        = 1u,
    TypeError    = 2u,
    RangeError   = 3u,
    SyntaxError  = 4u,
    ReferenceError = 5u,
    Map          = 6u,
    Set          = 7u,
    ArrayBuffer  = 8u,
    Uint8Array   = 9u,
    Uint16Array  = 10u,
    Int32Array   = 11u,
    Uint32Array  = 12u,
    Float64Array = 13u,
    Array        = 14u,
    Object       = 15u,
};

// Why a run stopped or the compiler refused a script. Codes below 32 stop
// a run that has started; codes from 32 up refuse the script before it
// runs. The Rust side reads these from the run telemetry.
enum VmErrorCode : u32 {
    VM_OK                   = 0u,
    VM_STOP_STEPS           = 1u,
    VM_STOP_CALL_DEPTH      = 2u,
    VM_STOP_OBJECTS         = 3u,
    VM_STOP_ARRAYS          = 4u,
    VM_STOP_ARRAY_STORAGE   = 5u,
    VM_STOP_ARRAY_LENGTH    = 6u,
    VM_STOP_STRINGS         = 7u,
    VM_STOP_PROPERTIES      = 8u,
    VM_STOP_SHAPES          = 9u,
    VM_STOP_UNSUPPORTED     = 10u,
    VM_STOP_HEAP            = 11u,
    VM_STOP_NOT_FUNCTION    = 12u,
    VM_STOP_UNCAUGHT        = 13u,
    VM_REFUSE_SYNTAX        = 32u,
    VM_REFUSE_UNSUPPORTED   = 33u,
    VM_REFUSE_UNKNOWN_NAME  = 34u,
    VM_REFUSE_REGISTERS     = 40u,
    VM_REFUSE_TOKENS        = 41u,
    VM_REFUSE_CODE_SIZE     = 42u,
    VM_REFUSE_CONSTANTS     = 43u,
    VM_REFUSE_FUNCTIONS     = 44u,
    VM_REFUSE_GLOBALS       = 45u,
    VM_REFUSE_SITES         = 46u,
    VM_REFUSE_STRINGS       = 47u,
    VM_REFUSE_HOST_SLOTS    = 48u,
    VM_REFUSE_LOOPS         = 49u,
    VM_REFUSE_HEAP          = 50u,
    VM_REFUSE_NESTING       = 51u,
};

constexpr u32 MAX_ERROR_TEXT = 160u;
constexpr u32 MAX_EXPORT_BYTES = 4096u;
constexpr u32 MAX_HOST_ARGS = 32u;

// The first reason a run stopped or a script was refused, as plain text.
struct AnvilVmError {
    u32 code;
    u32 len;
    char text[MAX_ERROR_TEXT];

    void clear() noexcept {
        code = VM_OK;
        len = 0u;
        text[0] = '\0';
    }

    void add(const char* s) noexcept {
        while (s && *s && len + 1u < MAX_ERROR_TEXT) {
            text[len++] = *s++;
        }
        text[len] = '\0';
    }

    // Script text is copied as printable ASCII only and cut at 40 bytes.
    void add_source(const char* s, u32 n) noexcept {
        const u32 shown = n < 40u ? n : 40u;
        for (u32 i = 0u; s && i < shown && len + 1u < MAX_ERROR_TEXT; ++i) {
            const char c = s[i];
            text[len++] = (c >= 0x20 && c < 0x7F) ? c : '?';
        }
        text[len] = '\0';
        if (shown < n) {
            add("...");
        }
    }

    void add_u32(u32 v) noexcept {
        char digits[10];
        u32 n = 0u;
        do {
            digits[n++] = static_cast<char>('0' + v % 10u);
            v /= 10u;
        } while (v != 0u && n < 10u);
        while (n > 0u && len + 1u < MAX_ERROR_TEXT) {
            text[len++] = digits[--n];
        }
        text[len] = '\0';
    }
};

// Result of a property or element store.
enum class StoreResult : u32 {
    Ok = 0u,
    // Not an object or array, an inactive one, or a negative index.
    NotStored = 1u,
    TooManyProperties = 2u,
    ShapesFull = 3u,
    ArrayTooLong = 4u,
    ArrayStorageFull = 5u,
    // The arena ran out within the byte budget while growing storage.
    HeapFull = 6u,
};

// ============================================================================
// 2. 64-Bit NaN-Boxed Value Representation (AnvilValue)
// ============================================================================

enum class ValueTag : u32 {
    Number    = 0u,
    Smi       = 1u,
    Bool      = 2u,
    Null      = 3u,
    Undefined = 4u,
    String    = 5u,
    Object    = 6u,
    Array     = 7u,
    Function  = 8u,
    DomNode   = 9u,
};

constexpr u64 NANBOX_BASE_MASK = 0x7FF8000000000000ULL;
constexpr u64 NANBOX_TAG_Check = 0xFFF8000000000000ULL;
constexpr u64 NANBOX_TAG_SHIFT = 48u;
constexpr u64 NANBOX_TAG_MASK  = 0x0007000000000000ULL;
constexpr u64 NANBOX_EXT_BIT   = 0x8000000000000000ULL;

inline u64 f64_to_bits(f64 v) noexcept {
    union {
        f64 d;
        u64 u;
    } conv{v};
    return conv.u;
}

inline f64 bits_to_f64(u64 b) noexcept {
    union {
        u64 u;
        f64 d;
    } conv{b};
    return conv.d;
}

constexpr f64 TWO_POW_52 = 4503599627370496.0;
constexpr f64 TWO_POW_32 = 4294967296.0;

constexpr u64 F64_EXPONENT_MASK = 0x7FF0000000000000ULL;
constexpr u64 F64_FRACTION_MASK = 0x000FFFFFFFFFFFFFULL;

inline bool js_is_nan(f64 v) noexcept {
    const u64 b = f64_to_bits(v);
    return (b & F64_EXPONENT_MASK) == F64_EXPONENT_MASK && (b & F64_FRACTION_MASK) != 0u;
}

inline bool js_is_finite(f64 v) noexcept {
    return (f64_to_bits(v) & F64_EXPONENT_MASK) != F64_EXPONENT_MASK;
}

inline f64 js_abs(f64 v) noexcept {
    return v < 0.0 ? -v : (v == 0.0 ? 0.0 : v);
}

// Math.floor. Doubles of magnitude 2^52 and up are already whole.
inline f64 js_floor(f64 v) noexcept {
    if (!(js_abs(v) < TWO_POW_52)) {
        return v;
    }
    const f64 t = static_cast<f64>(static_cast<i64>(v));
    const f64 r = (v < t) ? t - 1.0 : t;
    return (r == 0.0 && (v < 0.0 || (f64_to_bits(v) >> 63) != 0u)) ? -0.0 : r;
}

inline f64 js_ceil(f64 v) noexcept {
    if (!(js_abs(v) < TWO_POW_52)) {
        return v;
    }
    const f64 t = static_cast<f64>(static_cast<i64>(v));
    const f64 r = (v > t) ? t + 1.0 : t;
    return (r == 0.0 && (v < 0.0 || (f64_to_bits(v) >> 63) != 0u)) ? -0.0 : r;
}

// Math.round: halves round up, and -0.5 up to -0 gives -0.
inline f64 js_round(f64 v) noexcept {
    if (!(js_abs(v) < TWO_POW_52)) {
        return v;
    }
    if (v >= 0.0 && v < 0.5) {
        return 0.0;
    }
    if (v >= -0.5 && v <= 0.0) {
        return -0.0;
    }
    f64 r = js_floor(v);
    if (v - r >= 0.5) {
        r += 1.0;
    }
    return (r == 0.0 && (v < 0.0 || (f64_to_bits(v) >> 63) != 0u)) ? -0.0 : r;
}

inline f64 js_sqrt(f64 v) noexcept {
    if (js_is_nan(v) || v < 0.0) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    if (v == 0.0 || !js_is_finite(v)) {
        return v;
    }
#if defined(__GNUC__) || defined(__clang__)
    return __builtin_sqrt(v);
#else
    const u64 b = f64_to_bits(v);
    i32 exp = static_cast<i32>((b >> 52) & 0x7FFu) - 1023;
    u64 mant = b & F64_FRACTION_MASK;
    if (exp == -1023) {
        exp = -1022;
        while ((mant & (1ULL << 52)) == 0ULL) {
            mant <<= 1;
            --exp;
        }
    } else {
        mant |= (1ULL << 52);
    }
    if ((exp & 1) != 0) {
        mant <<= 1;
        --exp;
    }
    i32 half_exp = exp >> 1;
    u64 q = 0ULL;
    u64 rem = 0ULL;
    for (i32 i = 0; i < 54; ++i) {
        const i32 shift = 52 - (i * 2);
        const u64 pair = (shift >= 0) ? ((mant >> shift) & 3ULL) : 0ULL;
        rem = (rem << 2) | pair;
        const u64 cand = (q << 2) | 1ULL;
        if (rem >= cand) {
            rem -= cand;
            q = (q << 1) | 1ULL;
        } else {
            q <<= 1;
        }
    }
    const u64 round_bit = q & 1ULL;
    u64 sig = q >> 1;
    if (round_bit != 0ULL && (rem != 0ULL || (sig & 1ULL) != 0ULL)) {
        ++sig;
        if (sig == (1ULL << 53)) {
            sig >>= 1;
            ++half_exp;
        }
    }
    const u64 out_exp = static_cast<u64>(half_exp + 1023);
    return bits_to_f64((out_exp << 52) | (sig & F64_FRACTION_MASK));
#endif
}

// ToInt32: whole part modulo 2^32, as a signed value. NaN and infinities
// give 0.
inline i32 js_to_int32(f64 v) noexcept {
    if (!js_is_finite(v)) {
        return 0;
    }
    const f64 t = v < 0.0 ? -js_floor(-v) : js_floor(v);
    const f64 q = t / TWO_POW_32;
    if (!(js_abs(q) < TWO_POW_52)) {
        return 0;
    }
    f64 r = t - js_floor(q) * TWO_POW_32;
    if (r < 0.0) {
        r += TWO_POW_32;
    }
    const u32 u = r < TWO_POW_32 ? static_cast<u32>(r) : 0u;
    return static_cast<i32>(u);
}

inline u32 js_to_uint32(f64 v) noexcept {
    return static_cast<u32>(js_to_int32(v));
}

// Math.trunc: drop the fraction toward zero, keeping -0 for a small negative.
inline f64 js_trunc(f64 v) noexcept {
    if (!js_is_finite(v)) {
        return v;
    }
    return v < 0.0 ? -js_floor(-v) : js_floor(v);
}

// Math.sign: -1, -0, +0, 1 or NaN, with the zero sign kept.
inline f64 js_sign(f64 v) noexcept {
    if (js_is_nan(v)) {
        return v;
    }
    if (v > 0.0) {
        return 1.0;
    }
    if (v < 0.0) {
        return -1.0;
    }
    return v;
}

// Math.pow with the ECMAScript edge cases that differ from C's pow.
inline f64 js_pow(f64 base, f64 exp) noexcept {
    if (exp == 0.0) {
        return 1.0;
    }
    if (js_is_nan(exp) || js_is_nan(base)) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    if ((base == 1.0 || base == -1.0) && !js_is_finite(exp)) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    return __builtin_pow(base, exp);
}

// Math.imul: the 32-bit integer product, as a signed 32-bit result.
inline i32 js_imul(f64 a, f64 b) noexcept {
    return static_cast<i32>(js_to_uint32(a) * js_to_uint32(b));
}

// Math.min of two values: NaN propagates and -0 is less than +0.
inline f64 js_min2(f64 a, f64 b) noexcept {
    if (js_is_nan(a) || js_is_nan(b)) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    if (a == 0.0 && b == 0.0) {
        const bool neg = (f64_to_bits(a) >> 63) != 0u || (f64_to_bits(b) >> 63) != 0u;
        return neg ? -0.0 : 0.0;
    }
    return a < b ? a : b;
}

// Math.max of two values: NaN propagates and +0 is greater than -0.
inline f64 js_max2(f64 a, f64 b) noexcept {
    if (js_is_nan(a) || js_is_nan(b)) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    if (a == 0.0 && b == 0.0) {
        const bool both_neg = (f64_to_bits(a) >> 63) != 0u && (f64_to_bits(b) >> 63) != 0u;
        return both_neg ? -0.0 : 0.0;
    }
    return a > b ? a : b;
}

// The % operator: the sign follows the dividend, as in JavaScript.
inline f64 js_remainder(f64 x, f64 y) noexcept {
    if (js_is_nan(x) || js_is_nan(y) || !js_is_finite(x) || y == 0.0) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    if (!js_is_finite(y) || x == 0.0) {
        return x;
    }
    const bool negative = x < 0.0;
    f64 r = negative ? -x : x;
    const f64 d = y < 0.0 ? -y : y;
    while (r >= d) {
        f64 t = d;
        while (t <= r * 0.5) {
            t *= 2.0;
        }
        r -= t;
    }
    if (r == 0.0) {
        return negative ? -0.0 : 0.0;
    }
    return negative ? -r : r;
}

inline i64 sat_i64(f64 v) noexcept {
    if (js_is_nan(v)) {
        return 0;
    }
    if (v >= 9223372036854775807.0) {
        return 9223372036854775807LL;
    }
    if (v <= -9223372036854775808.0) {
        return -9223372036854775807LL - 1LL;
    }
    return static_cast<i64>(v);
}

inline i32 sat_i32(f64 v) noexcept {
    if (js_is_nan(v)) {
        return 0;
    }
    if (v >= 2147483647.0) {
        return 2147483647;
    }
    if (v <= -2147483648.0) {
        return -2147483647 - 1;
    }
    return static_cast<i32>(v);
}

struct AnvilValue {
    u64 bits;

    constexpr AnvilValue() noexcept : bits(NANBOX_BASE_MASK | (4ULL << 48)) {}
    constexpr explicit AnvilValue(u64 raw) noexcept : bits(raw) {}

    static constexpr AnvilValue make_undefined() noexcept {
        return AnvilValue(NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::Undefined) << 48));
    }

    static constexpr AnvilValue make_null() noexcept {
        return AnvilValue(NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::Null) << 48));
    }

    static constexpr AnvilValue make_bool(bool b) noexcept {
        return AnvilValue(
            NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::Bool) << 48) | (b ? 1ULL : 0ULL)
        );
    }

    static constexpr AnvilValue make_smi(i32 v) noexcept {
        return AnvilValue(
            NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::Smi) << 48) |
            static_cast<u64>(static_cast<u32>(v))
        );
    }

    static AnvilValue make_number(f64 n) noexcept {
        if (n >= -1073741824.0 && n <= 1073741823.0 && (n != 0.0 || (f64_to_bits(n) >> 63) == 0u)) {
            const i32 as_int = static_cast<i32>(n);
            if (static_cast<f64>(as_int) == n) {
                return make_smi(as_int);
            }
        }
        if (js_is_nan(n)) {
            return AnvilValue(NANBOX_BASE_MASK);
        }
        return AnvilValue(f64_to_bits(n));
    }

    static constexpr AnvilValue make_string(u32 sym_id) noexcept {
        return AnvilValue(
            NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::String) << 48) | static_cast<u64>(sym_id)
        );
    }

    static constexpr AnvilValue make_object(u32 obj_id) noexcept {
        return AnvilValue(
            NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::Object) << 48) | static_cast<u64>(obj_id)
        );
    }

    static constexpr AnvilValue make_array(u32 arr_id) noexcept {
        return AnvilValue(
            NANBOX_BASE_MASK | (static_cast<u64>(ValueTag::Array) << 48) | static_cast<u64>(arr_id)
        );
    }

    static constexpr AnvilValue make_function(u32 fn_id) noexcept {
        return AnvilValue(
            NANBOX_EST_TAG(ValueTag::Function, fn_id)
        );
    }

    static constexpr AnvilValue make_dom_node(u32 node_id) noexcept {
        return AnvilValue(
            NANBOX_EST_TAG(ValueTag::DomNode, node_id)
        );
    }

    static constexpr u64 NANBOX_EST_TAG(ValueTag t, u32 payload) noexcept {
        return NANBOX_EXT_BIT | NANBOX_BASE_MASK |
               ((static_cast<u64>(t) & 0x7ULL) << 48) | static_cast<u64>(payload);
    }

    constexpr ValueTag tag() const noexcept {
        if ((bits & 0x7FF8000000000000ULL) != NANBOX_BASE_MASK) {
            return ValueTag::Number;
        }
        const u32 low_tag = static_cast<u32>((bits >> 48) & 0x7ULL);
        if ((bits & NANBOX_EXT_BIT) != 0ULL) {
            if (low_tag == (static_cast<u32>(ValueTag::Function) & 0x7u)) {
                return ValueTag::Function;
            }
            if (low_tag == (static_cast<u32>(ValueTag::DomNode) & 0x7u)) {
                return ValueTag::DomNode;
            }
            return ValueTag::Number;
        }
        if (low_tag == 0u) {
            return ValueTag::Number;
        }
        return static_cast<ValueTag>(low_tag);
    }

    constexpr bool is_smi() const noexcept { return tag() == ValueTag::Smi; }
    constexpr bool is_number() const noexcept {
        const ValueTag t = tag();
        return t == ValueTag::Smi || t == ValueTag::Number;
    }
    constexpr bool is_bool() const noexcept { return tag() == ValueTag::Bool; }
    constexpr bool is_null() const noexcept { return tag() == ValueTag::Null; }
    constexpr bool is_undefined() const noexcept { return tag() == ValueTag::Undefined; }
    constexpr bool is_string() const noexcept { return tag() == ValueTag::String; }
    constexpr bool is_object() const noexcept { return tag() == ValueTag::Object; }
    constexpr bool is_array() const noexcept { return tag() == ValueTag::Array; }
    constexpr bool is_function() const noexcept { return tag() == ValueTag::Function; }
    constexpr bool is_dom_node() const noexcept { return tag() == ValueTag::DomNode; }

    constexpr u32 payload() const noexcept {
        return static_cast<u32>(bits & 0xFFFFFFFFULL);
    }

    constexpr i32 as_smi() const noexcept {
        return static_cast<i32>(payload());
    }

    f64 as_number() const noexcept {
        const ValueTag t = tag();
        if (t == ValueTag::Smi) {
            return static_cast<f64>(as_smi());
        }
        if (t == ValueTag::Number) {
            return bits_to_f64(bits);
        }
        if (t == ValueTag::Bool) {
            return payload() != 0u ? 1.0 : 0.0;
        }
        if (t == ValueTag::Undefined) {
            return bits_to_f64(0x7FF8000000000000ULL);
        }
        return 0.0;
    }

    bool is_truthy() const noexcept {
        const ValueTag t = tag();
        switch (t) {
            case ValueTag::Null:
            case ValueTag::Undefined:
                return false;
            case ValueTag::Bool:
                return payload() != 0u;
            case ValueTag::Smi:
                return as_smi() != 0;
            case ValueTag::Number: {
                const f64 d = bits_to_f64(bits);
                return d != 0.0 && !js_is_nan(d);
            }
            default:
                return true;
        }
    }
};

// ============================================================================
// 3. Hidden Classes (AnvilShape) & Managed Mark-Sweep Heap (AnvilHeap)
// ============================================================================

constexpr u32 ARENA_SIZE_CLASSES = 32u;

struct AnvilArena {
    u8* base;
    u32 used;      // bump frontier
    u32 reserve;   // total bytes the arena may ever use
    u32 budget;    // the byte budget: bumping past it fails the run
    u32 high_water;
    u32 free_list[ARENA_SIZE_CLASSES]; // head block offset per class, or NO_SLOT

    static u32 size_class(u32 bytes) noexcept {
        u32 need = bytes < 8u ? 8u : bytes;
        u32 c = 3u;
        u32 sz = 8u;
        while (sz < need) {
            sz <<= 1u;
            ++c;
        }
        return c;
    }
    static u32 class_bytes(u32 c) noexcept { return 1u << c; }

    void init(u8* b, u32 reserve_bytes, u32 budget_bytes) noexcept {
        base = b;
        used = 0u;
        reserve = reserve_bytes;
        budget = (budget_bytes == 0u || budget_bytes > reserve_bytes) ? reserve_bytes
                                                                      : budget_bytes;
        high_water = 0u;
        for (u32 i = 0u; i < ARENA_SIZE_CLASSES; ++i) {
            free_list[i] = NO_SLOT;
        }
    }

    u32 alloc(u32 bytes, bool* over) noexcept {
        const u32 c = size_class(bytes);
        if (free_list[c] != NO_SLOT) {
            const u32 off = free_list[c];
            u32 next = 0u;
            const u8* link = base + off;
            next = static_cast<u32>(link[0]) | (static_cast<u32>(link[1]) << 8u) |
                   (static_cast<u32>(link[2]) << 16u) | (static_cast<u32>(link[3]) << 24u);
            free_list[c] = next;
            return off;
        }
        const u32 need = class_bytes(c);
        if (need > budget - used) {
            if (over) {
                *over = true;
            }
            return NO_SLOT;
        }
        const u32 off = used;
        used += need;
        if (used > high_water) {
            high_water = used;
        }
        return off;
    }

    void free(u32 off, u32 bytes) noexcept {
        if (off == NO_SLOT) {
            return;
        }
        const u32 c = size_class(bytes);
        u8* link = base + off;
        const u32 next = free_list[c];
        link[0] = static_cast<u8>(next & 0xFFu);
        link[1] = static_cast<u8>((next >> 8u) & 0xFFu);
        link[2] = static_cast<u8>((next >> 16u) & 0xFFu);
        link[3] = static_cast<u8>((next >> 24u) & 0xFFu);
        free_list[c] = off;
    }

    u8* at(u32 off) noexcept { return base + off; }
    const u8* at(u32 off) const noexcept { return base + off; }
};

struct AnvilInternedString {
    u64 hash;
    u32 offset;
    u32 length;
    bool marked;
    bool active;
};

struct AnvilShapeTransition {
    u32 from_shape;
    u32 prop_sym;
    u32 to_shape;
};

struct AnvilShape {
    u32 id;
    u32 parent_shape;
    u32 prop_count;
    u32 prop_syms[DICT_THRESHOLD];

    i32 find_slot(u32 sym) const noexcept {
        for (u32 i = 0; i < prop_count; ++i) {
            if (prop_syms[i] == sym) {
                return static_cast<i32>(i);
            }
        }
        return -1;
    }
};

struct AnvilHeapObject {
    bool active;
    bool marked;
    bool dict;            // dictionary mode: own key map, no shared shape
    u8 builtin_kind;
    u32 shape_id;         // a shape id, or DICT_SHAPE when dict
    u32 proto_obj_id;
    u32 prop_count;       // live properties, kept in both modes
    u32 slots_off;        // arena block: AnvilValue[slots_cap]
    u32 slots_cap;
    u32 dict_keys_off;    // arena block: u32[slots_cap] of keys (dict only), else NO_SLOT
    u32 aux_id;           // Map/Set backing array id, ArrayBuffer arena offset, or TypedArray buffer obj_id
    u32 byte_offset;      // TypedArray byte offset into buffer
    u32 byte_length;      // ArrayBuffer or TypedArray byte length
};

struct AnvilHeapArray {
    bool active;
    bool marked;
    u32 length;
    u32 storage_off; // arena block holding AnvilValue[capacity], or NO_SLOT
    u32 capacity;
};

struct AnvilHeapFunction {
    bool active;
    bool marked;
    u32 name_sym;
    u32 arity;
    u32 local_count;
    u32 code_start;
    u32 code_len;
    u32 canonical_closure;
    u32 home_proto_id;    // prototype object where this method lives (for super.m), or NO_SLOT
};

struct AnvilHeapClosure {
    bool active;
    bool marked;
    bool pinned;        // a canonical closure, never collected
    u32 fn_id;
    u32 upvals_off;     // arena block: u32[upval_count] of cell array ids, or NO_SLOT
    u32 upval_count;
    u32 proto_obj_id;   // constructor .prototype object, or NO_SLOT
    u32 static_obj_id;  // constructor static properties object, or NO_SLOT
    u32 super_closure_id; // superclass constructor closure id, or NO_SLOT
};

struct AnvilHeap {
    AnvilArena arena;

    u32 str_pool_off;
    u32 str_pool_cap;
    u32 string_bytes_used;
    u32 strings_off;
    u32 strings_cap;
    u32 string_count;

    char* string_bytes() noexcept { return reinterpret_cast<char*>(arena.at(str_pool_off)); }
    const char* string_bytes() const noexcept {
        return reinterpret_cast<const char*>(arena.at(str_pool_off));
    }
    AnvilInternedString* strings() noexcept {
        return reinterpret_cast<AnvilInternedString*>(arena.at(strings_off));
    }
    const AnvilInternedString* strings() const noexcept {
        return reinterpret_cast<const AnvilInternedString*>(arena.at(strings_off));
    }
    bool ensure_string_bytes(u32 need) noexcept;
    bool ensure_string_table(u32 need) noexcept;

    u32 shapes_off;
    u32 shapes_cap;
    u32 shape_count;
    u32 transitions_off;
    u32 transitions_cap;
    u32 transition_count;

    AnvilShape* shapes() noexcept { return reinterpret_cast<AnvilShape*>(arena.at(shapes_off)); }
    const AnvilShape* shapes() const noexcept {
        return reinterpret_cast<const AnvilShape*>(arena.at(shapes_off));
    }
    AnvilShapeTransition* transitions() noexcept {
        return reinterpret_cast<AnvilShapeTransition*>(arena.at(transitions_off));
    }
    const AnvilShapeTransition* transitions() const noexcept {
        return reinterpret_cast<const AnvilShapeTransition*>(arena.at(transitions_off));
    }
    bool ensure_shapes(u32 need) noexcept;
    bool ensure_transitions(u32 need) noexcept;

    u32 objects_off;
    u32 objects_cap;
    u32 object_count;
    AnvilHeapObject* objects() noexcept {
        return reinterpret_cast<AnvilHeapObject*>(arena.at(objects_off));
    }
    const AnvilHeapObject* objects() const noexcept {
        return reinterpret_cast<const AnvilHeapObject*>(arena.at(objects_off));
    }
    AnvilValue* object_slots(u32 off) noexcept {
        return reinterpret_cast<AnvilValue*>(arena.at(off));
    }
    const AnvilValue* object_slots(u32 off) const noexcept {
        return reinterpret_cast<const AnvilValue*>(arena.at(off));
    }
    u32* object_keys(u32 off) noexcept { return reinterpret_cast<u32*>(arena.at(off)); }
    const u32* object_keys(u32 off) const noexcept {
        return reinterpret_cast<const u32*>(arena.at(off));
    }
    bool ensure_objects(u32 need) noexcept;
    bool ensure_object_slots(u32 obj_id, u32 need) noexcept;
    bool convert_object_to_dict(u32 obj_id) noexcept;

    u32 functions_off;
    u32 functions_cap;
    u32 function_count;
    AnvilHeapFunction* functions() noexcept {
        return reinterpret_cast<AnvilHeapFunction*>(arena.at(functions_off));
    }
    const AnvilHeapFunction* functions() const noexcept {
        return reinterpret_cast<const AnvilHeapFunction*>(arena.at(functions_off));
    }
    bool ensure_functions(u32 need) noexcept;

    u32 closures_off;
    u32 closures_cap;
    u32 closure_count;
    u32 live_closures;
    AnvilHeapClosure* closures() noexcept {
        return reinterpret_cast<AnvilHeapClosure*>(arena.at(closures_off));
    }
    const AnvilHeapClosure* closures() const noexcept {
        return reinterpret_cast<const AnvilHeapClosure*>(arena.at(closures_off));
    }
    u32* closure_upvals(u32 off) noexcept { return reinterpret_cast<u32*>(arena.at(off)); }
    const u32* closure_upvals(u32 off) const noexcept {
        return reinterpret_cast<const u32*>(arena.at(off));
    }
    bool ensure_closures(u32 need) noexcept;
    u32 alloc_closure(u32 fn_id, u32 upvals_off, u32 upval_count, bool pinned) noexcept;

    u32 arrays_off;
    u32 arrays_cap;
    u32 array_count;

    AnvilHeapArray* arrays() noexcept {
        return reinterpret_cast<AnvilHeapArray*>(arena.at(arrays_off));
    }
    const AnvilHeapArray* arrays() const noexcept {
        return reinterpret_cast<const AnvilHeapArray*>(arena.at(arrays_off));
    }
    AnvilValue* array_elems(u32 off) noexcept {
        return reinterpret_cast<AnvilValue*>(arena.at(off));
    }
    const AnvilValue* array_elems(u32 off) const noexcept {
        return reinterpret_cast<const AnvilValue*>(arena.at(off));
    }
    bool ensure_arrays(u32 need) noexcept;

    // Built-in Error prototype hierarchy (pinned objects)
    u32 err_proto_id;
    u32 type_err_proto_id;
    u32 range_err_proto_id;
    u32 syntax_err_proto_id;
    u32 ref_err_proto_id;

    // Heap & GC telemetry
    u32 live_objects;
    u32 live_arrays;
    u32 bytes_allocated;
    u32 gc_collections;
    u32 shape_transitions_count;

    void init(u8* arena_base, u32 reserve, u32 budget) noexcept;
    u32 intern_string(const char* str, u32 len) noexcept;
    const char* get_string_ptr(u32 sym, u32* len_out) const noexcept;
    bool string_equals(u32 sym_a, u32 sym_b) const noexcept;

    u32 transition_shape(u32 current_shape, u32 prop_sym) noexcept;
    i32 lookup_shape_slot(u32 shape_id, u32 prop_sym) const noexcept;

    u32 alloc_object(u32 initial_shape = 0u) noexcept;
    u32 alloc_array(u32 initial_capacity = 8u) noexcept;
    u32 alloc_cell(AnvilValue v) noexcept;
    u32 alloc_function(u32 name_sym, u32 arity, u32 local_count, u32 code_start, u32 code_len) noexcept;
    u32 alloc_error_object(BuiltinCtorKind kind, u32 msg_sym) noexcept;

    AnvilValue get_object_prop(u32 obj_id, u32 prop_sym, u32* out_shape = nullptr, i32* out_slot = nullptr) const noexcept;
    StoreResult set_object_prop(u32 obj_id, u32 prop_sym, AnvilValue val, u32* out_shape = nullptr, i32* out_slot = nullptr) noexcept;
    bool has_object_prop(u32 obj_id, u32 prop_sym) const noexcept;
    bool delete_object_prop(u32 obj_id, u32 prop_sym) noexcept;

    AnvilValue get_array_elem(u32 arr_id, i32 index) const noexcept;
    StoreResult set_array_elem(u32 arr_id, i32 index, AnvilValue val) noexcept;
    StoreResult push_array_elem(u32 arr_id, AnvilValue val) noexcept;
    u32 intern_concat(u32 sym_a, u32 sym_b) noexcept;
    u32 value_to_string_sym(AnvilValue v) noexcept;

    void mark_value(AnvilValue v) noexcept;
    void mark_object(u32 obj_id) noexcept;
    void mark_array(u32 arr_id) noexcept;
    void mark_closure(u32 closure_id) noexcept;
    void clear_marks() noexcept;
    u32 sweep_unmarked() noexcept;
    u32 collect_garbage(const AnvilValue* roots, u32 root_count) noexcept;
};

// ============================================================================
// 4. 4-Way Polymorphic Inline Cache (IC) Engine & DOM Selector IC
// ============================================================================

enum class IcState : u8 {
    Uninitialized = 0u,
    Monomorphic   = 1u,
    Polymorphic   = 2u,
    Megamorphic   = 3u,
};

struct AnvilIcStub {
    u32 shape_id;
    i32 slot_offset;
};

struct AnvilIcSite {
    IcState state;
    u8 count;
    u32 prop_sym;
    AnvilIcStub stubs[4];
};

struct AnvilDomIcEntry {
    u64 selector_hash;
    u32 node_id;
    u32 hit_count;
    bool occupied;
};

struct AnvilIcTable {
    AnvilArena* arena;
    u32 sites_off;
    u32 sites_cap;
    u32 site_count;
    AnvilIcSite* sites() noexcept { return reinterpret_cast<AnvilIcSite*>(arena->at(sites_off)); }
    const AnvilIcSite* sites() const noexcept {
        return reinterpret_cast<const AnvilIcSite*>(arena->at(sites_off));
    }
    bool ensure_sites(u32 need) noexcept;
    u32 ic_hits;
    u32 ic_misses;
    u32 polymorphic_upgrades;
    u32 megamorphic_fallbacks;

    AnvilDomIcEntry dom_entries[MAX_DOM_IC_ENTRIES];
    u32 dom_ic_hits;
    u32 dom_ic_misses;

    void init(AnvilArena* a) noexcept;
    u32 allocate_site(u32 prop_sym) noexcept;
    AnvilValue load_property_ic(AnvilHeap& heap, u32 site_id, AnvilValue receiver, u32 prop_sym) noexcept;
    StoreResult store_property_ic(AnvilHeap& heap, u32 site_id, AnvilValue receiver, u32 prop_sym, AnvilValue val) noexcept;

    i32 lookup_dom_selector(const char* selector, u32 len) noexcept;
    void record_dom_selector(const char* selector, u32 len, u32 node_id) noexcept;
    void invalidate_dom_cache() noexcept;
};

// ============================================================================
// 5. Register-Based Bytecode ISA (AnvilOpcode & AnvilInstruction)
// ============================================================================

enum class AnvilOpcode : u8 {
    Nop          = 0u,
    LoadConst    = 1u,   // R[a] = Const[imm32]
    LoadSmi      = 2u,   // R[a] = Smi(imm16 as i16)
    LoadNull     = 3u,   // R[a] = null
    LoadTrue     = 4u,   // R[a] = true
    LoadFalse    = 5u,   // R[a] = false
    Move         = 6u,   // R[a] = R[b]
    Add          = 7u,   // R[a] = R[b] + R[c]
    Sub          = 8u,   // R[a] = R[b] - R[c]
    Mul          = 9u,   // R[a] = R[b] * R[c]
    Div          = 10u,  // R[a] = R[b] / R[c]
    Mod          = 11u,  // R[a] = R[b] % R[c]
    Neg          = 12u,  // R[a] = -R[b]
    Not          = 13u,  // R[a] = !R[b]
    BitAnd       = 14u,  // R[a] = R[b] & R[c]
    BitOr        = 15u,  // R[a] = R[b] | R[c]
    BitXor       = 16u,  // R[a] = R[b] ^ R[c]
    Shl          = 17u,  // R[a] = R[b] << R[c]
    Shr          = 18u,  // R[a] = R[b] >> R[c]
    UShr         = 19u,  // R[a] = R[b] >>> R[c]
    CmpEq        = 20u,  // R[a] = (R[b] === R[c])
    CmpNe        = 21u,  // R[a] = (R[b] !== R[c])
    CmpLt        = 22u,  // R[a] = (R[b] < R[c])
    CmpLe        = 23u,  // R[a] = (R[b] <= R[c])
    CmpGt        = 24u,  // R[a] = (R[b] > R[c])
    CmpGe        = 25u,  // R[a] = (R[b] >= R[c])
    Jump         = 26u,  // pc = imm32
    JumpIfFalse  = 27u,  // if (!R[a]) pc = imm32
    JumpIfTrue   = 28u,  // if (R[a]) pc = imm32
    GetGlobal    = 29u,  // R[a] = Globals[imm32]
    SetGlobal    = 30u,  // Globals[imm32] = R[a]
    NewObject    = 31u,  // R[a] = alloc_object()
    NewArray     = 32u,  // R[a] = alloc_array(b)
    GetPropIC    = 33u,  // R[a] = IC[c](R[b], prop_sym_from_ic)
    SetPropIC    = 34u,  // IC[c](R[a], prop_sym_from_ic) = R[b]
    GetElem      = 35u,  // R[a] = R[b][R[c]]
    SetElem      = 36u,  // R[a][R[b]] = R[c]
    ArrayPush    = 37u,  // push(R[a], R[b])
    Call         = 38u,  // R[a] = call(fn_id = b, arg_base = c)
    CallMath     = 39u,  // R[a] = math_op(op = b, arg = R[c])
    LoopHeader   = 40u,  // hot-loop profiling counter at loop head
    Return       = 41u,  // return R[a]
    Halt         = 42u,  // stop chunk execution, result in R[a]
    HostSetText  = 43u,
    CmpLooseEq   = 44u,  // R[a] = (R[b] == R[c])
    CmpLooseNe   = 45u,  // R[a] = (R[b] != R[c])
    LoadFunc     = 46u,  // R[a] = function value for fn_id = imm32
    CallValue    = 47u,  // R[a] = call(R[b], arg_base = c, argc = imm32)
    CallMath2    = 48u,  // R[a] = math2(op = imm32, R[b], R[c])
    CallPred     = 49u,  // R[a] = predicate(op = b, R[c]) -> bool
    NewCell      = 50u,  // R[a] = new cell holding R[b]
    CellGet      = 51u,  // R[a] = contents of cell R[b]
    CellSet      = 52u,  // cell R[a] <- R[b]
    GetUpval     = 53u,  // R[a] = contents of the running closure's upvalue b
    SetUpval     = 54u,  // the running closure's upvalue a <- R[b]
    MakeClosure  = 55u,  // R[a] = closure(fn_id = imm32, upvalue cells R[b..b+c])
    HostCall     = 56u,  // R[a] = host_call(fn_id = imm32, arg_base = b, argc = c)
    GetUpvalCell = 57u,  // R[a] = cell held in running closure's upvalue b
    BitNot       = 58u,  // R[a] = ~ToInt32(R[b])
    TypeOf       = 59u,  // R[a] = typeof R[b]
    InOp         = 60u,  // R[a] = (R[b] in R[c])
    InstanceOf   = 61u,  // R[a] = (R[b] instanceof R[c])
    DeleteProp   = 62u,  // R[a] = delete R[b].prop(imm32)
    DeleteElem   = 63u,  // R[a] = delete R[b][R[c]]
    LoadThis     = 64u,  // R[a] = cur_this
    CallMethod   = 65u,  // R[a] = R[b].method(args = R[b+1..b+1+c], prop_sym = imm32)
    NewInstance  = 66u,  // R[a] = new R[b](arg_base = c, argc = imm32)
    NewBuiltin   = 67u,  // R[a] = new_builtin(kind = b, arg_base = c, argc = imm32)
    StringFromCharCode = 68u, // R[a] = String.fromCharCode(R[b..b+c])
    EnterTry     = 69u,  // push try handler (catch_reg = a, catch_pc = imm32)
    SetTryFinally = 70u, // set top try handler's finally_pc = imm32
    LeaveTry     = 71u,  // normal exit from try/catch -> target_pc = imm32
    EndFinally   = 72u,  // finish finally block
    Throw        = 73u,  // throw R[a]
    IterLen      = 74u,  // R[a] = iterable_len(R[b])
    IterGet      = 75u,  // R[a] = iterable_get(R[b], idx = R[c])
    SetupClass   = 76u,  // configure class closure R[a], super = R[b], proto_out = R[c]
    DefineMethod = 77u,  // define method on class R[a], closure = R[b], is_static = c, name_sym = imm32
    CallSuperCtor = 78u, // R[a] = super(arg_base = b, argc = c)
    CallSuperMethod = 79u, // R[a] = super.method(arg_base = b, argc = c, name_sym = imm32)
    ToNum        = 80u,  // R[a] = +R[b]
};

struct AnvilInstruction {
    AnvilOpcode op;
    u8 pad;
    u16 a;
    u16 b;
    u16 c;
    u32 imm;

    static constexpr AnvilInstruction make_abc(AnvilOpcode op, u16 a, u16 b, u16 c) noexcept {
        return AnvilInstruction{op, 0u, a, b, c, 0u};
    }

    static constexpr AnvilInstruction make_abx(AnvilOpcode op, u16 a, u32 imm) noexcept {
        return AnvilInstruction{op, 0u, a, 0u, 0u, imm};
    }

    constexpr u32 imm32() const noexcept {
        return imm;
    }

    constexpr i32 signed_imm32() const noexcept {
        return static_cast<i32>(imm);
    }

    constexpr u16 imm16() const noexcept {
        return static_cast<u16>(imm);
    }

    constexpr i32 signed_imm16() const noexcept {
        return static_cast<i32>(static_cast<short>(static_cast<u16>(imm)));
    }
};

struct AnvilGlobalBinding {
    u32 name_sym;
    AnvilValue value;
    bool occupied;
    bool exported;
    bool declared;
    u32 decl_depth;
};

struct AnvilCallFrame {
    u32 fn_id;
    u32 closure_id;
    u32 return_pc;
    u32 reg_base;
    u32 caller_frame_regs;
    u16 dst_reg;
    u32 new_instance_obj; // NO_SLOT for normal calls; object id for `new Ctor(...)`
    AnvilValue caller_this;
};

// Completion state for an active try/catch/finally handler.
enum class TryCompletionKind : u8 {
    Normal = 0u,
    Throw  = 1u,
    Return = 2u,
};

struct AnvilTryHandler {
    u32 frame_depth;
    u32 reg_base;
    u32 cur_frame_regs;
    u32 cur_closure;
    AnvilValue cur_this;
    u32 catch_pc;
    u32 finally_pc;
    u16 catch_reg;
    TryCompletionKind completion_kind;
    AnvilValue completion_val;
    u32 completion_pc;
};

struct AnvilBytecodeProgram {
    u32 code_off;
    u32 code_cap;
    u32 code_len;
    u32 top_frame_regs;
    AnvilArena* arena;
    u32 constants_off;
    u32 constants_cap;
    u32 const_len;
    u32 loop_trip_counts[64];
    bool compile_ok;
    u32 overflow;

    AnvilValue* constants() noexcept {
        return reinterpret_cast<AnvilValue*>(arena->at(constants_off));
    }
    const AnvilValue* constants() const noexcept {
        return reinterpret_cast<const AnvilValue*>(arena->at(constants_off));
    }
    bool ensure_constants(u32 need) noexcept;

    AnvilInstruction* code() noexcept {
        return reinterpret_cast<AnvilInstruction*>(arena->at(code_off));
    }
    const AnvilInstruction* code() const noexcept {
        return reinterpret_cast<const AnvilInstruction*>(arena->at(code_off));
    }
    bool ensure_code(u32 need) noexcept {
        if (need <= code_cap) {
            return true;
        }
        u32 new_cap = code_cap == 0u ? INIT_CODE_BUFFER : code_cap;
        while (new_cap < need) {
            new_cap *= 2u;
        }
        bool over = false;
        const u32 new_off = arena->alloc(new_cap * static_cast<u32>(sizeof(AnvilInstruction)), &over);
        if (new_off == NO_SLOT) {
            return false;
        }
        AnvilInstruction* dst = reinterpret_cast<AnvilInstruction*>(arena->at(new_off));
        const AnvilInstruction* old = code();
        for (u32 i = 0u; i < code_len; ++i) {
            dst[i] = old[i];
        }
        const u32 old_off = code_off;
        const u32 old_cap = code_cap;
        code_off = new_off;
        code_cap = new_cap;
        if (old_cap != 0u) {
            arena->free(old_off, old_cap * static_cast<u32>(sizeof(AnvilInstruction)));
        }
        return true;
    }

    void init(AnvilArena* a) noexcept {
        arena = a;
        code_off = 0u;
        code_cap = 0u;
        code_len = 0u;
        top_frame_regs = 0u;
        constants_off = 0u;
        constants_cap = 0u;
        const_len = 0u;
        for (u32 i = 0; i < 64u; ++i) {
            loop_trip_counts[i] = 0u;
        }
        compile_ok = true;
        overflow = VM_OK;
    }

    u32 emit(AnvilInstruction inst) noexcept {
        if (code_len >= ANVIL_MAX_PROGRAM_INSTRUCTIONS || !ensure_code(code_len + 1u)) {
            if (compile_ok) {
                overflow = VM_REFUSE_CODE_SIZE;
            }
            compile_ok = false;
            return 0u;
        }
        const u32 idx = code_len++;
        code()[idx] = inst;
        return idx;
    }

    u32 add_constant(AnvilValue val) noexcept {
        for (u32 i = 0; i < const_len; ++i) {
            if (constants()[i].bits == val.bits) {
                return i;
            }
        }
        if (!ensure_constants(const_len + 1u)) {
            if (compile_ok) {
                overflow = VM_REFUSE_CONSTANTS;
            }
            compile_ok = false;
            return 0u;
        }
        const u32 idx = const_len++;
        constants()[idx] = val;
        return idx;
    }
};

// ============================================================================
// 6. Complete C++20 Virtual Machine Instance & Telemetry Struct
// ============================================================================

struct AnvilVmTelemetryC {
    u32 kernel_version;
    u32 bytecode_ops_executed;
    u32 heap_objects_live;
    u32 heap_bytes_allocated;
    u32 gc_collections;
    u32 shapes_allocated;
    u32 ic_hits;
    u32 ic_misses;
    u32 polymorphic_ic_sites;
    u32 hot_loops_accelerated;
    u32 native_compilations;
    u32 stop_code;
    u32 stop_len;
    char stop_text[MAX_ERROR_TEXT];
    u32 exports_len;
    u32 exports_full;
    u8 exports[MAX_EXPORT_BYTES];
};

static_assert(sizeof(AnvilVmTelemetryC) == 11u * 4u + 8u + MAX_ERROR_TEXT + 8u + MAX_EXPORT_BYTES,
              "telemetry layout");

struct AnvilVmContext {
    AnvilHeap heap;
    AnvilIcTable ic;
    AnvilBytecodeProgram program;
    u32 registers_off;
    u32 registers_cap;
    AnvilValue* registers() noexcept {
        return reinterpret_cast<AnvilValue*>(heap.arena.at(registers_off));
    }
    const AnvilValue* registers() const noexcept {
        return reinterpret_cast<const AnvilValue*>(heap.arena.at(registers_off));
    }
    bool ensure_registers(u32 need) noexcept;
    u32 globals_off;
    u32 globals_cap;
    u32 global_count;
    AnvilGlobalBinding* globals() noexcept {
        return reinterpret_cast<AnvilGlobalBinding*>(heap.arena.at(globals_off));
    }
    const AnvilGlobalBinding* globals() const noexcept {
        return reinterpret_cast<const AnvilGlobalBinding*>(heap.arena.at(globals_off));
    }
    bool ensure_globals(u32 need) noexcept;
    u32 frames_off;
    u32 frames_cap;
    u32 frame_count;
    AnvilCallFrame* frames() noexcept {
        return reinterpret_cast<AnvilCallFrame*>(heap.arena.at(frames_off));
    }
    const AnvilCallFrame* frames() const noexcept {
        return reinterpret_cast<const AnvilCallFrame*>(heap.arena.at(frames_off));
    }
    bool ensure_frames(u32 need) noexcept;

    AnvilTryHandler try_stack[MAX_TRY_DEPTH];
    u32 try_depth;

    u32 steps_executed;
    u32 hot_loops_accelerated;
    u32 native_compilations;
    bool aborted;
    AnvilVmError error;
    char host_ids[8][64];
    u32 host_id_lens[8];
    u32 host_id_count;
    void (*host_set_text)(void* host, const char* id, u32 len, double value);
    void* host;
    u32 step_limit;
    u32 script_size_limit;
    u32 heap_byte_budget;
    u32 call_depth_limit;

    u32 yield_interval;
    bool paused;
    u32 resume_pc;
    u32 resume_reg_base;
    u32 resume_frame_regs;
    u32 resume_closure;
    AnvilValue resume_this;
    bool pending_host_call;
    u32 pending_host_fn;
    u32 pending_arg_count;
    u32 pending_host_overflow;
    u16 resume_host_dst;
    AnvilValue pending_args[MAX_HOST_ARGS];

    u32 parse_stack_budget;

    u32 effective_step_limit() const noexcept {
        return step_limit != 0u ? step_limit : MAX_VM_STEPS;
    }

    u32 effective_call_depth() const noexcept {
        return call_depth_limit != 0u ? call_depth_limit : ANVIL_DEFAULT_CALL_DEPTH;
    }

    u32 effective_parse_stack_budget() const noexcept {
        return parse_stack_budget != 0u ? parse_stack_budget : ANVIL_DEFAULT_PARSE_STACK_BUDGET;
    }

    void init() noexcept;
    u16 resolve_global_slot(u32 name_sym) noexcept;

    bool fail(u32 code, const char* text) noexcept {
        if (code < VM_REFUSE_SYNTAX) {
            aborted = true;
        }
        if (error.code != VM_OK) {
            return false;
        }
        error.clear();
        error.code = code;
        error.add(text);
        return true;
    }

    void fail_limit(u32 code, const char* what, u32 limit, const char* unit) noexcept {
        if (fail(code, what)) {
            error.add(" limit reached: ");
            error.add_u32(limit);
            error.add(" ");
            error.add(unit);
        }
    }

    void fail_name(u32 code, const char* text, const char* name, u32 name_len) noexcept {
        if (fail(code, text)) {
            error.add(" '");
            error.add_source(name, name_len);
            error.add("'");
        }
    }
    void collect_garbage(u32 reg_top) noexcept;
    void reclaim_array_storage(u32 reg_top) noexcept;
    bool store_ok(StoreResult stored) noexcept;
    AnvilValue execute(u32 entry_pc = 0u) noexcept;
    AnvilValue resume() noexcept;
    AnvilValue run_loop(u32 pc, u32 reg_base, u32 frame_regs, u32 cur_closure_in, AnvilValue cur_this_in = AnvilValue::make_undefined(), u32 stop_frame_depth = 0u) noexcept;
    void fill_telemetry(AnvilVmTelemetryC* out) const noexcept;
    void fill_exports(AnvilVmTelemetryC* out) const noexcept;
    void fill_host_call(AnvilVmTelemetryC* out) const noexcept;
};

bool anvil_jit_try_run(AnvilVmContext* ctx, u32 entry_pc, f64* out_number) noexcept;
bool compile_js_to_bytecode(AnvilVmContext& ctx, void* compiler_memory, const char* src, u32 len) noexcept;

// Exact IEEE-754 decimal/radix number literal parser used by the compiler and number formatter.
f64 parse_f64_slice(const char* s, u32 len) noexcept;

inline u64 fnv1a_hash_bytes(const char* data, u32 len) noexcept {
    u64 h = 14695981039346656037ULL;
    for (u32 i = 0; i < len; ++i) {
        h ^= static_cast<u64>(static_cast<u8>(data[i]));
        h *= 1099511628211ULL;
    }
    return h;
}

} // namespace anvil_vm

#endif // ANVIL_VM_HPP
