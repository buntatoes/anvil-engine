// SPDX-License-Identifier: LicenseRef-Proprietary
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
//   - Windows MSVC: cl.exe /c /O2 /fp:fast /std:c++20 /Zl
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

// ============================================================================
// 1. Deterministic Engine Limits & Budgets
// ============================================================================

constexpr u32 ANVIL_VM_KERNEL_VERSION    = 18u;
constexpr u32 MAX_VM_STEPS              = 100000u;
constexpr u32 MAX_CALL_FRAMES           = 64u;
constexpr u32 MAX_REGISTERS_PER_FRAME   = 64u;
// One 64-register window per call frame, so calls can nest until the
// frame stack is full.
constexpr u32 MAX_TOTAL_REGISTERS       = MAX_CALL_FRAMES * MAX_REGISTERS_PER_FRAME;
// The managed heap lives in an arena that Rust allocates after the context
// header (see anvil_cpp_vm_context_bytes). The arena is reserved up front but
// only touched as it is used; one byte budget, set by Configure, caps how
// much of it a run may hand out. The string pool, interned-string table and
// growable heap tables all draw from it, so these counts are no longer fixed.
constexpr u32 ANVIL_ARENA_RESERVE_BYTES = 256u * 1024u * 1024u;
constexpr u32 ANVIL_DEFAULT_HEAP_BUDGET = 64u * 1024u * 1024u;
constexpr u32 MAX_SHAPES                = 256u;
constexpr u32 MAX_SHAPE_PROPS           = 32u;
constexpr u32 MAX_SHAPE_TRANSITIONS     = 512u;
constexpr u32 MAX_HEAP_OBJECTS          = 512u;
constexpr u32 MAX_HEAP_ARRAYS           = 256u;
constexpr u32 MAX_ARRAY_ELEMENTS        = 2048u;
constexpr u32 MAX_ARRAY_LENGTH          = 256u;
// Calls nest until the frame stack or the register file runs out.
constexpr u32 MAX_CALL_DEPTH =
    (MAX_CALL_FRAMES - 1u) < (MAX_TOTAL_REGISTERS / MAX_REGISTERS_PER_FRAME - 1u)
        ? (MAX_CALL_FRAMES - 1u)
        : (MAX_TOTAL_REGISTERS / MAX_REGISTERS_PER_FRAME - 1u);
constexpr u32 MAX_FUNCTIONS             = 128u;
constexpr u32 MAX_BYTECODE_INSTRUCTIONS = 4096u;
constexpr u32 MAX_CONSTANTS             = 512u;
constexpr u32 MAX_GLOBALS               = 256u;
constexpr u32 MAX_IC_SITES              = 256u;
constexpr u32 MAX_DOM_IC_ENTRIES        = 128u;
constexpr u32 HOT_LOOP_THRESHOLD        = 16u;

// Returned by the allocators and tables when they are full.
constexpr u32 NO_SLOT = 0xFFFFFFFFu;
constexpr u16 NO_GLOBAL = 0xFFFFu;

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
};

constexpr u32 MAX_ERROR_TEXT = 160u;
constexpr u32 MAX_EXPORT_BYTES = 4096u;

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
};

// ============================================================================
// 2. 64-Bit NaN-Boxed Value Representation (AnvilValue)
// ============================================================================
//
// IEEE-754 double values that are not quiet tagged NaNs are stored raw via bit-cast.
// Tagged non-double values use the upper 16 bits:
//   0x7FF8 | (tag << 48) | 32-bit payload
// Because standard hardware NaNs generated by arithmetic have tag bits 0, any
// tag in [1..9] uniquely identifies a non-double boxed value.

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

// Number helpers shared by the interpreter and the native loops. Casting a
// double that is NaN or out of range to an integer is undefined in C++, so
// every conversion goes through these.

constexpr f64 TWO_POW_52 = 4503599627370496.0;
constexpr f64 TWO_POW_32 = 4294967296.0;

inline bool js_is_nan(f64 v) noexcept {
    return v != v;
}

inline bool js_is_finite(f64 v) noexcept {
    return v == v && v - v == 0.0;
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
    // Scale into [0.5, 2) by powers of four, run Newton, scale back.
    f64 x = v;
    f64 scale = 1.0;
    while (x > 2.0) {
        x *= 0.25;
        scale *= 2.0;
    }
    while (x < 0.5) {
        x *= 4.0;
        scale *= 0.5;
    }
    f64 g = 1.0;
    for (int i = 0; i < 8; ++i) {
        g = 0.5 * (g + x / g);
    }
    return g * scale;
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
    // q is whole from 2^52 up, so t is then a multiple of 2^32.
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

// The % operator: the sign follows the dividend, as in JavaScript. Each
// subtraction of a power-of-two multiple of the divisor is exact.
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

// Saturating casts with the results Rust's `as` gives, NaN to 0.
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
        // The range check comes first so the cast is defined, and -0 stays
        // a double so its sign is kept.
        if (n >= -1073741824.0 && n <= 1073741823.0 && (n != 0.0 || (f64_to_bits(n) >> 63) == 0u)) {
            const i32 as_int = static_cast<i32>(n);
            if (static_cast<f64>(as_int) == n) {
                return make_smi(as_int);
            }
        }
        // Every NaN becomes the one plain NaN, so no NaN the hardware makes
        // (0 / 0 sets the sign bit) can read back as a tagged value.
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
                return d != 0.0 && d == d;
            }
            default:
                return true;
        }
    }
};

// ============================================================================
// 3. Hidden Classes (AnvilShape) & Managed Mark-Sweep Heap (AnvilHeap)
// ============================================================================

struct AnvilArena {
    // Recomputed from the context pointer at every init(), so nothing inside
    // the arena is a raw pointer and the whole region stays relocatable.
    u8* base;
    u32 used;      // bytes handed out so far
    u32 reserve;   // total bytes the arena may ever use
    u32 budget;    // the byte budget: handing out past it fails the run
    u32 high_water;

    void init(u8* b, u32 reserve_bytes, u32 budget_bytes) noexcept {
        base = b;
        used = 0u;
        reserve = reserve_bytes;
        budget = (budget_bytes == 0u || budget_bytes > reserve_bytes) ? reserve_bytes
                                                                      : budget_bytes;
        high_water = 0u;
    }

    // Hand out `bytes` rounded up to 8, or NO_SLOT when that would pass the
    // budget. `*over` (when given) is set on failure so the caller can report
    // the heap limit.
    u32 alloc(u32 bytes, bool* over) noexcept {
        const u32 need = (bytes + 7u) & ~7u;
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
    u32 prop_syms[MAX_SHAPE_PROPS];

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
    u32 shape_id;
    u32 proto_obj_id;
    AnvilValue slots[MAX_SHAPE_PROPS];
};

struct AnvilHeapArray {
    bool active;
    bool marked;
    u32 length;
    u32 start_index;
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
};

struct AnvilHeap {
    // The arena every growable region is carved from.
    AnvilArena arena;

    // Interned string pool: a byte region and a table, both in the arena and
    // grown as needed. Offsets are relative to the arena base; byte offsets in
    // the table entries are relative to str_pool_off.
    u32 str_pool_off;
    u32 str_pool_cap;
    u32 string_bytes_used;
    u32 strings_off;
    u32 strings_cap;
    u32 string_count;

    // Typed views on the two string regions.
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
    // Grow a region to at least `need` bytes/entries; false on heap limit.
    bool ensure_string_bytes(u32 need) noexcept;
    bool ensure_string_table(u32 need) noexcept;

    // Hidden classes (Shapes) & transition table
    AnvilShape shapes[MAX_SHAPES];
    u32 shape_count;
    AnvilShapeTransition transitions[MAX_SHAPE_TRANSITIONS];
    u32 transition_count;

    // GC-managed objects, arrays, and functions
    AnvilHeapObject objects[MAX_HEAP_OBJECTS];
    AnvilHeapArray arrays[MAX_HEAP_ARRAYS];
    AnvilValue array_storage[MAX_ARRAY_ELEMENTS];
    u32 array_storage_used;
    AnvilHeapFunction functions[MAX_FUNCTIONS];
    u32 function_count;

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

    // These return NO_SLOT when the table or storage is full.
    u32 alloc_object(u32 initial_shape = 0u) noexcept;
    u32 alloc_array(u32 initial_capacity = 8u) noexcept;
    u32 alloc_function(u32 name_sym, u32 arity, u32 local_count, u32 code_start, u32 code_len) noexcept;

    AnvilValue get_object_prop(u32 obj_id, u32 prop_sym, u32* out_shape, i32* out_slot) const noexcept;
    StoreResult set_object_prop(u32 obj_id, u32 prop_sym, AnvilValue val, u32* out_shape, i32* out_slot) noexcept;

    AnvilValue get_array_elem(u32 arr_id, i32 index) const noexcept;
    StoreResult set_array_elem(u32 arr_id, i32 index, AnvilValue val) noexcept;
    StoreResult push_array_elem(u32 arr_id, AnvilValue val) noexcept;
    // Intern the concatenation of two pooled strings, built in place at the
    // end of the pool. NO_SLOT when the pool has no room for it.
    u32 intern_concat(u32 sym_a, u32 sym_b) noexcept;
    // Move the live arrays down to the start of array storage so the space
    // of freed arrays and of grown arrays' old slices can be used again.
    void compact_array_storage() noexcept;

    void mark_value(AnvilValue v) noexcept;
    void mark_object(u32 obj_id) noexcept;
    void mark_array(u32 arr_id) noexcept;
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
    AnvilIcSite sites[MAX_IC_SITES];
    u32 site_count;
    u32 ic_hits;
    u32 ic_misses;
    u32 polymorphic_upgrades;
    u32 megamorphic_fallbacks;

    AnvilDomIcEntry dom_entries[MAX_DOM_IC_ENTRIES];
    u32 dom_ic_hits;
    u32 dom_ic_misses;

    void init() noexcept;
    // NO_SLOT when all sites are taken.
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
    LoadConst    = 1u,   // R[a] = Const[imm16]
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
    Jump         = 26u,  // pc = imm16
    JumpIfFalse  = 27u,  // if (!R[a]) pc = imm16
    JumpIfTrue   = 28u,  // if (R[a]) pc = imm16
    GetGlobal    = 29u,  // R[a] = Globals[imm16]
    SetGlobal    = 30u,  // Globals[imm16] = R[a]
    NewObject    = 31u,  // R[a] = alloc_object()
    NewArray     = 32u,  // R[a] = alloc_array(b)
    GetPropIC    = 33u,  // R[a] = IC[c](R[b], prop_sym_from_ic)
    SetPropIC    = 34u,  // IC[c](R[a], prop_sym_from_ic) = R[b]
    GetElem      = 35u,  // R[a] = R[b][R[c]]
    SetElem      = 36u,  // R[a][R[b]] = R[c]
    ArrayPush    = 37u,  // push(R[a], R[b])
    Call         = 38u,  // R[a] = call(fn_id = b, arg_base = c, argc = imm8)
    CallMath     = 39u,  // R[a] = math_op(op = b, arg = R[c])
    LoopHeader   = 40u,  // hot-loop profiling counter at loop head
    Return       = 41u,  // return R[a]
    Halt         = 42u,  // stop chunk execution, result in R[a]
    // R[a] is a number. imm16 is a slot in AnvilVmContext::host_ids.
    // The native compiler calls the page host. The interpreter ignores it.
    HostSetText  = 43u,
    CmpLooseEq   = 44u,  // R[a] = (R[b] == R[c])
    CmpLooseNe   = 45u,  // R[a] = (R[b] != R[c])
};

struct AnvilInstruction {
    AnvilOpcode op;
    u8 a;
    u8 b;
    u8 c;

    static constexpr AnvilInstruction make_abc(AnvilOpcode op, u8 a, u8 b, u8 c) noexcept {
        return AnvilInstruction{op, a, b, c};
    }

    static constexpr AnvilInstruction make_abx(AnvilOpcode op, u8 a, u16 imm16) noexcept {
        return AnvilInstruction{
            op,
            a,
            static_cast<u8>((imm16 >> 8) & 0xFFu),
            static_cast<u8>(imm16 & 0xFFu),
        };
    }

    constexpr u16 imm16() const noexcept {
        return static_cast<u16>((static_cast<u16>(b) << 8) | static_cast<u16>(c));
    }

    constexpr i32 signed_imm16() const noexcept {
        return static_cast<i32>(static_cast<short>(imm16()));
    }
};

struct AnvilGlobalBinding {
    u32 name_sym;
    AnvilValue value;
    bool occupied;
    // Declared at the top level or with `var`, so later scripts see it.
    bool exported;
};

struct AnvilCallFrame {
    u32 fn_id;
    u32 return_pc;
    u32 reg_base;
    u8 dst_reg;
};

struct AnvilBytecodeProgram {
    AnvilInstruction code[MAX_BYTECODE_INSTRUCTIONS];
    u32 code_len;
    AnvilValue constants[MAX_CONSTANTS];
    u32 const_len;
    u32 loop_trip_counts[64];
    bool compile_ok;
    // VM_REFUSE_CODE_SIZE or VM_REFUSE_CONSTANTS once compile_ok is false.
    u32 overflow;

    void init() noexcept {
        code_len = 0u;
        const_len = 0u;
        for (u32 i = 0; i < 64u; ++i) {
            loop_trip_counts[i] = 0u;
        }
        compile_ok = true;
        overflow = VM_OK;
    }

    u32 emit(AnvilInstruction inst) noexcept {
        if (code_len >= MAX_BYTECODE_INSTRUCTIONS) {
            if (compile_ok) {
                overflow = VM_REFUSE_CODE_SIZE;
            }
            compile_ok = false;
            return 0u;
        }
        const u32 idx = code_len++;
        code[idx] = inst;
        return idx;
    }

    u16 add_constant(AnvilValue val) noexcept {
        for (u32 i = 0; i < const_len; ++i) {
            if (constants[i].bits == val.bits) {
                return static_cast<u16>(i);
            }
        }
        if (const_len >= MAX_CONSTANTS) {
            if (compile_ok) {
                overflow = VM_REFUSE_CONSTANTS;
            }
            compile_ok = false;
            return 0u;
        }
        const u16 idx = static_cast<u16>(const_len++);
        constants[idx] = val;
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
    // Why the run stopped or the script was refused: a VmErrorCode (0 when
    // it ran to the end) and the reason, `stop_len` bytes with no NUL.
    u32 stop_code;
    u32 stop_len;
    char stop_text[MAX_ERROR_TEXT];
    // Top-level variables later scripts can see, written by fill_exports.
    // `exports_len` bytes are used; `exports_full` is 1 when they did not fit.
    u32 exports_len;
    u32 exports_full;
    u8 exports[MAX_EXPORT_BYTES];
};

// The Rust side (`AnvilVmTelemetry` in src/lib.rs) has the same layout.
static_assert(sizeof(AnvilVmTelemetryC) == 11u * 4u + 8u + MAX_ERROR_TEXT + 8u + MAX_EXPORT_BYTES,
              "telemetry layout");

struct AnvilVmContext {
    AnvilHeap heap;
    AnvilIcTable ic;
    AnvilBytecodeProgram program;
    AnvilValue registers[MAX_TOTAL_REGISTERS];
    AnvilGlobalBinding globals[MAX_GLOBALS];
    u32 global_count;
    AnvilCallFrame frames[MAX_CALL_FRAMES];
    u32 frame_count;
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
    // Limits a Configure request set for this context. 0 means "use the
    // built-in default". They are kept across runs (init() does not clear
    // them), so a Configure call applies to the Run calls that follow on the
    // same context. step_limit and script_size_limit take effect now; the
    // heap and call-depth budgets are stored here for the heap and the frame
    // stack to use as they grow.
    u32 step_limit;
    u32 script_size_limit;
    u32 heap_byte_budget;
    u32 call_depth_limit;

    // Steps this run may take before it stops: the configured budget, or the
    // built-in default when none was set.
    u32 effective_step_limit() const noexcept {
        return step_limit != 0u ? step_limit : MAX_VM_STEPS;
    }

    void init() noexcept;
    // NO_GLOBAL when all global slots are taken.
    u16 resolve_global_slot(u32 name_sym) noexcept;

    // Record why the run stopped or the script was refused. Only the first
    // reason is kept. A stop code also ends the run.
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

    // "<what> limit reached: <limit> <unit>"
    void fail_limit(u32 code, const char* what, u32 limit, const char* unit) noexcept {
        if (fail(code, what)) {
            error.add(" limit reached: ");
            error.add_u32(limit);
            error.add(" ");
            error.add(unit);
        }
    }

    // "<text> '<name>'"
    void fail_name(u32 code, const char* text, const char* name, u32 name_len) noexcept {
        if (fail(code, text)) {
            error.add(" '");
            error.add_source(name, name_len);
            error.add("'");
        }
    }
    // Collect with the registers below `reg_top` and the globals as roots.
    void collect_garbage(u32 reg_top) noexcept;
    // Collect, then compact array storage.
    void reclaim_array_storage(u32 reg_top) noexcept;
    // Record a stop for a store that hit a limit. False when the run stops.
    bool store_ok(StoreResult stored) noexcept;
    AnvilValue execute(u32 entry_pc = 0u) noexcept;
    void fill_telemetry(AnvilVmTelemetryC* out) const noexcept;
    void fill_exports(AnvilVmTelemetryC* out) const noexcept;
};

// Compile a numeric program to x86-64 and run it. Returns false when the
// program is not numeric or the code page cannot be executed. On success,
// `out_number` is the Halt/Return value.
bool anvil_jit_try_run(AnvilVmContext* ctx, u32 entry_pc, f64* out_number) noexcept;

// Compile source text into `ctx.program`. `compiler_memory` is caller-owned
// storage of `anvil_cpp_vm_compiler_bytes` bytes. This is not a general compiler.
bool compile_js_to_bytecode(AnvilVmContext& ctx, void* compiler_memory, const char* src, u32 len) noexcept;

// Hash helper used across string pool and DOM selector IC.
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
