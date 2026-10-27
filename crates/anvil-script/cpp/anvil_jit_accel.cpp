// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 Register Virtual Machine Execution Engine, Hot-Loop Accelerator, and Rust FFI Entry Points.
// Author: Buntato
//
// Compiled alongside anvil_vm_heap.cpp, anvil_vm_ic.cpp, and anvil_vm_compiler.cpp
// by crates/anvil-script/build.rs when a C++20 compiler is available.

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {

constexpr u32 VM_RUN_REFUSED       = 0u;
constexpr u32 VM_RUN_OK            = 1u;
constexpr u32 VM_RUN_STOPPED       = 2u;
constexpr u32 VM_RUN_HOST_CALL     = 3u;
constexpr u32 VM_RUN_BUFFER_SMALL  = 4u;

constexpr u32 AVK_HEADER_BYTES = 16u;
constexpr u32 AVK_OP_RUN       = 1u;
constexpr u32 AVK_OP_CONFIGURE = 2u;
constexpr u32 AVK_OP_RESUME    = 3u;

inline bool has_request_magic(const char* s, u32 n) noexcept {
    return s && n >= 4u && s[0] == 'A' && s[1] == 'V' && s[2] == 'K' && s[3] == '1';
}

inline u32 read_u32_le(const char* p) noexcept {
    return static_cast<u32>(static_cast<u8>(p[0])) |
           (static_cast<u32>(static_cast<u8>(p[1])) << 8) |
           (static_cast<u32>(static_cast<u8>(p[2])) << 16) |
           (static_cast<u32>(static_cast<u8>(p[3])) << 24);
}

inline bool cstr_eq(const char* a, u32 alen, const char* b, u32 blen) noexcept {
    if (alen != blen || (!a && alen != 0u)) return false;
    for (u32 i = 0u; i < alen; ++i) {
        if (a[i] != b[i]) return false;
    }
    return true;
}

inline bool numeric_operand(AnvilValue v) noexcept {
    return v.is_number() || v.is_bool() || v.is_null() || v.is_undefined();
}

inline u32 numeric_operands(AnvilOpcode op) noexcept {
    switch (op) {
        case AnvilOpcode::Neg:
        case AnvilOpcode::BitNot:
            return 1u;
        case AnvilOpcode::Sub:
        case AnvilOpcode::Mul:
        case AnvilOpcode::Div:
        case AnvilOpcode::Mod:
        case AnvilOpcode::BitAnd:
        case AnvilOpcode::BitOr:
        case AnvilOpcode::BitXor:
        case AnvilOpcode::Shl:
        case AnvilOpcode::Shr:
        case AnvilOpcode::UShr:
            return 2u;
        default:
            return 0u;
    }
}

inline bool truthy(const AnvilHeap& heap, AnvilValue v) noexcept {
    if (v.is_string()) {
        u32 len = 0u;
        heap.get_string_ptr(v.payload(), &len);
        return len > 0u;
    }
    return v.is_truthy();
}

inline i32 array_index(f64 v) noexcept {
    if (!(v >= 0.0 && v < 2147483648.0)) {
        return -1;
    }
    const i32 i = static_cast<i32>(v);
    return static_cast<f64>(i) == v ? i : -1;
}

inline u32 utf8_next_cp(const char* p, u32 len, u32& i) noexcept {
    const u8 b0 = static_cast<u8>(p[i++]);
    if (b0 < 0x80u) {
        return b0;
    }
    if ((b0 & 0xE0u) == 0xC0u && i < len) {
        const u8 b1 = static_cast<u8>(p[i++]);
        return ((static_cast<u32>(b0 & 0x1Fu) << 6u) | static_cast<u32>(b1 & 0x3Fu));
    }
    if ((b0 & 0xF0u) == 0xE0u && i + 1u < len) {
        const u8 b1 = static_cast<u8>(p[i++]);
        const u8 b2 = static_cast<u8>(p[i++]);
        return ((static_cast<u32>(b0 & 0x0Fu) << 12u) |
                (static_cast<u32>(b1 & 0x3Fu) << 6u) |
                static_cast<u32>(b2 & 0x3Fu));
    }
    if ((b0 & 0xF8u) == 0xF0u && i + 2u < len) {
        const u8 b1 = static_cast<u8>(p[i++]);
        const u8 b2 = static_cast<u8>(p[i++]);
        const u8 b3 = static_cast<u8>(p[i++]);
        return ((static_cast<u32>(b0 & 0x07u) << 18u) |
                (static_cast<u32>(b1 & 0x3Fu) << 12u) |
                (static_cast<u32>(b2 & 0x3Fu) << 6u) |
                static_cast<u32>(b3 & 0x3Fu));
    }
    return b0;
}

inline u32 utf16_len(const char* p, u32 len) noexcept {
    u32 i = 0u;
    u32 u16_count = 0u;
    while (i < len) {
        const u32 cp = utf8_next_cp(p, len, i);
        u16_count += (cp >= 0x10000u) ? 2u : 1u;
    }
    return u16_count;
}

inline bool utf16_at(const char* p, u32 len, u32 target_idx, u16* out_unit) noexcept {
    u32 i = 0u;
    u32 u16_pos = 0u;
    while (i < len) {
        const u32 cp = utf8_next_cp(p, len, i);
        if (cp >= 0x10000u) {
            const u32 v = cp - 0x10000u;
            if (u16_pos == target_idx) {
                *out_unit = static_cast<u16>(0xD800u + (v >> 10u));
                return true;
            }
            if (u16_pos + 1u == target_idx) {
                *out_unit = static_cast<u16>(0xDC00u + (v & 0x3FFu));
                return true;
            }
            u16_pos += 2u;
        } else {
            if (u16_pos == target_idx) {
                *out_unit = static_cast<u16>(cp);
                return true;
            }
            ++u16_pos;
        }
    }
    return false;
}

inline i32 utf16_compare(const char* a, u32 len_a, const char* b, u32 len_b) noexcept {
    u32 ia = 0u, ib = 0u;
    i32 pend_a = -1, pend_b = -1;
    for (;;) {
        i32 ua = -1;
        if (pend_a >= 0) {
            ua = pend_a;
            pend_a = -1;
        } else if (ia < len_a) {
            const u32 cp = utf8_next_cp(a, len_a, ia);
            if (cp >= 0x10000u) {
                const u32 v = cp - 0x10000u;
                ua = static_cast<i32>(0xD800u + (v >> 10u));
                pend_a = static_cast<i32>(0xDC00u + (v & 0x3FFu));
            } else {
                ua = static_cast<i32>(cp);
            }
        }
        i32 ub = -1;
        if (pend_b >= 0) {
            ub = pend_b;
            pend_b = -1;
        } else if (ib < len_b) {
            const u32 cp = utf8_next_cp(b, len_b, ib);
            if (cp >= 0x10000u) {
                const u32 v = cp - 0x10000u;
                ub = static_cast<i32>(0xD800u + (v >> 10u));
                pend_b = static_cast<i32>(0xDC00u + (v & 0x3FFu));
            } else {
                ub = static_cast<i32>(cp);
            }
        }
        if (ua < 0 && ub < 0) return 0;
        if (ua < 0) return -1;
        if (ub < 0) return 1;
        if (ua != ub) return ua < ub ? -1 : 1;
    }
}

inline u32 encode_unit_utf8(u16 u, char* out) noexcept {
    if (u <= 0x7Fu) {
        out[0] = static_cast<char>(u);
        return 1u;
    }
    if (u <= 0x7FFu) {
        out[0] = static_cast<char>(0xC0u | (u >> 6u));
        out[1] = static_cast<char>(0x80u | (u & 0x3Fu));
        return 2u;
    }
    out[0] = static_cast<char>(0xE0u | (u >> 12u));
    out[1] = static_cast<char>(0x80u | ((u >> 6u) & 0x3Fu));
    out[2] = static_cast<char>(0x80u | (u & 0x3Fu));
    return 3u;
}

u32 utf16_slice_sym(AnvilHeap& heap, u32 sym, i32 start_idx, i32 end_idx) noexcept {
    if (start_idx >= end_idx) {
        return heap.intern_string("", 0u);
    }
    u32 byte_len = 0u;
    const char* ptr = heap.get_string_ptr(sym, &byte_len);
    const u32 s_u16 = static_cast<u32>(start_idx);
    const u32 e_u16 = static_cast<u32>(end_idx);

    u32 i = 0u;
    u32 u16_pos = 0u;
    u32 byte_start = NO_SLOT;
    u32 byte_end = NO_SLOT;
    bool split_start = false;
    bool split_end = false;
    u16 lead_surrogate = 0u;
    u16 trail_surrogate = 0u;

    while (i < byte_len) {
        const u32 cp_byte = i;
        const u32 cp = utf8_next_cp(ptr, byte_len, i);
        if (cp >= 0x10000u) {
            const u32 v = cp - 0x10000u;
            if (u16_pos == s_u16) {
                byte_start = cp_byte;
            } else if (u16_pos + 1u == s_u16) {
                split_start = true;
                lead_surrogate = static_cast<u16>(0xDC00u + (v & 0x3FFu));
                byte_start = i;
            }
            if (u16_pos + 1u == e_u16) {
                split_end = true;
                trail_surrogate = static_cast<u16>(0xD800u + (v >> 10u));
                byte_end = cp_byte;
                break;
            }
            if (u16_pos + 2u == e_u16) {
                byte_end = i;
                break;
            }
            u16_pos += 2u;
        } else {
            if (u16_pos == s_u16) {
                byte_start = cp_byte;
            }
            if (u16_pos + 1u == e_u16) {
                byte_end = i;
                break;
            }
            ++u16_pos;
        }
    }
    if (byte_start == NO_SLOT) {
        return heap.intern_string("", 0u);
    }
    if (byte_end == NO_SLOT) {
        byte_end = byte_len;
    }
    if (!split_start && !split_end) {
        const char* p = heap.get_string_ptr(sym, nullptr);
        return heap.intern_string(p + byte_start, byte_end - byte_start);
    }
    u32 res = heap.intern_string("", 0u);
    if (split_start && split_end && byte_start > byte_end) {
        char b[3];
        const u32 n = encode_unit_utf8(lead_surrogate, b);
        return heap.intern_string(b, n);
    }
    if (split_start) {
        char b[3];
        const u32 n = encode_unit_utf8(lead_surrogate, b);
        res = heap.intern_string(b, n);
    }
    if (byte_end > byte_start) {
        const char* p = heap.get_string_ptr(sym, nullptr);
        const u32 mid = heap.intern_string(p + byte_start, byte_end - byte_start);
        res = heap.intern_concat(res, mid);
    }
    if (split_end) {
        char b[3];
        const u32 n = encode_unit_utf8(trail_surrogate, b);
        const u32 tail = heap.intern_string(b, n);
        res = heap.intern_concat(res, tail);
    }
    return res;
}

bool values_loosely_equal(const AnvilHeap& heap, AnvilValue a, AnvilValue b, bool& known) noexcept;

bool values_strictly_equal(const AnvilHeap& heap, AnvilValue a, AnvilValue b) noexcept {
    if (a.is_number() && b.is_number()) {
        return a.as_number() == b.as_number();
    }
    if (a.bits == b.bits) {
        return true;
    }
    if (a.is_string() && b.is_string()) {
        return heap.string_equals(a.payload(), b.payload());
    }
    return false;
}

bool same_value_zero(const AnvilHeap& heap, AnvilValue a, AnvilValue b) noexcept {
    if (a.is_number() && b.is_number()) {
        const f64 na = a.as_number();
        const f64 nb = b.as_number();
        if (js_is_nan(na) && js_is_nan(nb)) {
            return true;
        }
        return na == nb;
    }
    return values_strictly_equal(heap, a, b);
}

bool values_loosely_equal(const AnvilHeap& heap, AnvilValue a, AnvilValue b, bool& known) noexcept {
    known = true;
    if (a.is_number() && b.is_number()) {
        return a.as_number() == b.as_number();
    }
    const bool a_nullish = a.is_null() || a.is_undefined();
    const bool b_nullish = b.is_null() || b.is_undefined();
    if (a_nullish || b_nullish) {
        return a_nullish && b_nullish;
    }
    if (a.tag() == b.tag()) {
        return values_strictly_equal(heap, a, b);
    }
    known = false;
    return false;
}

inline u32 typed_array_shift(u8 bkind) noexcept {
    switch (static_cast<BuiltinObjectKind>(bkind)) {
        case BuiltinObjectKind::Uint8Array:   return 0u;
        case BuiltinObjectKind::Uint16Array:  return 1u;
        case BuiltinObjectKind::Int32Array:   return 2u;
        case BuiltinObjectKind::Uint32Array:  return 2u;
        case BuiltinObjectKind::Float64Array: return 3u;
        default:                              return 0u;
    }
}

u8* get_arraybuffer_bytes(const AnvilHeap& heap, u32 buf_obj_id, u32* out_byte_len) noexcept {
    if (buf_obj_id >= heap.object_count || !heap.objects()[buf_obj_id].active) {
        if (out_byte_len) *out_byte_len = 0u;
        return nullptr;
    }
    const AnvilHeapObject& bobj = heap.objects()[buf_obj_id];
    if (static_cast<BuiltinObjectKind>(bobj.builtin_kind) != BuiltinObjectKind::ArrayBuffer ||
        bobj.aux_id >= heap.array_count || !heap.arrays()[bobj.aux_id].active) {
        if (out_byte_len) *out_byte_len = 0u;
        return nullptr;
    }
    if (out_byte_len) *out_byte_len = bobj.byte_length;
    const u32 storage_off = heap.arrays()[bobj.aux_id].storage_off;
    if (storage_off == NO_SLOT) {
        return nullptr;
    }
    return reinterpret_cast<u8*>(const_cast<AnvilValue*>(heap.array_elems(storage_off)));
}

AnvilValue typed_array_get_elem(const AnvilHeap& heap, const AnvilHeapObject& obj, i32 idx) noexcept {
    const u32 shift = typed_array_shift(obj.builtin_kind);
    const u32 elem_len = obj.byte_length >> shift;
    if (idx < 0 || static_cast<u32>(idx) >= elem_len) {
        return AnvilValue::make_undefined();
    }
    u32 buf_len = 0u;
    const u8* base = get_arraybuffer_bytes(heap, obj.aux_id, &buf_len);
    const u32 byte_pos = obj.byte_offset + (static_cast<u32>(idx) << shift);
    if (!base || byte_pos + (1u << shift) > buf_len) {
        return AnvilValue::make_undefined();
    }
    const u8* ptr = base + byte_pos;
    switch (static_cast<BuiltinObjectKind>(obj.builtin_kind)) {
        case BuiltinObjectKind::Uint8Array:
            return AnvilValue::make_smi(static_cast<i32>(ptr[0]));
        case BuiltinObjectKind::Uint16Array: {
            const u16 v = static_cast<u16>(ptr[0]) | (static_cast<u16>(ptr[1]) << 8u);
            return AnvilValue::make_smi(static_cast<i32>(v));
        }
        case BuiltinObjectKind::Int32Array: {
            const u32 u = static_cast<u32>(ptr[0]) |
                          (static_cast<u32>(ptr[1]) << 8u) |
                          (static_cast<u32>(ptr[2]) << 16u) |
                          (static_cast<u32>(ptr[3]) << 24u);
            return AnvilValue::make_smi(static_cast<i32>(u));
        }
        case BuiltinObjectKind::Uint32Array: {
            const u32 u = static_cast<u32>(ptr[0]) |
                          (static_cast<u32>(ptr[1]) << 8u) |
                          (static_cast<u32>(ptr[2]) << 16u) |
                          (static_cast<u32>(ptr[3]) << 24u);
            return AnvilValue::make_number(static_cast<f64>(u));
        }
        case BuiltinObjectKind::Float64Array: {
            u64 bits = 0ULL;
            for (u32 k = 0u; k < 8u; ++k) {
                bits |= (static_cast<u64>(ptr[k]) << (k * 8u));
            }
            return AnvilValue::make_number(bits_to_f64(bits));
        }
        default:
            return AnvilValue::make_undefined();
    }
}

void typed_array_set_elem(AnvilHeap& heap, const AnvilHeapObject& obj, i32 idx, AnvilValue val) noexcept {
    const u32 shift = typed_array_shift(obj.builtin_kind);
    const u32 elem_len = obj.byte_length >> shift;
    if (idx < 0 || static_cast<u32>(idx) >= elem_len) {
        return;
    }
    u32 buf_len = 0u;
    u8* base = get_arraybuffer_bytes(heap, obj.aux_id, &buf_len);
    const u32 byte_pos = obj.byte_offset + (static_cast<u32>(idx) << shift);
    if (!base || byte_pos + (1u << shift) > buf_len) {
        return;
    }
    u8* ptr = base + byte_pos;
    const f64 n = val.as_number();
    switch (static_cast<BuiltinObjectKind>(obj.builtin_kind)) {
        case BuiltinObjectKind::Uint8Array:
            ptr[0] = static_cast<u8>(js_to_uint32(n) & 0xFFu);
            break;
        case BuiltinObjectKind::Uint16Array: {
            const u16 v = static_cast<u16>(js_to_uint32(n) & 0xFFFFu);
            ptr[0] = static_cast<u8>(v & 0xFFu);
            ptr[1] = static_cast<u8>((v >> 8u) & 0xFFu);
            break;
        }
        case BuiltinObjectKind::Int32Array:
        case BuiltinObjectKind::Uint32Array: {
            const u32 v = js_to_uint32(n);
            ptr[0] = static_cast<u8>(v & 0xFFu);
            ptr[1] = static_cast<u8>((v >> 8u) & 0xFFu);
            ptr[2] = static_cast<u8>((v >> 16u) & 0xFFu);
            ptr[3] = static_cast<u8>((v >> 24u) & 0xFFu);
            break;
        }
        case BuiltinObjectKind::Float64Array: {
            const u64 bits = f64_to_bits(n);
            for (u32 k = 0u; k < 8u; ++k) {
                ptr[k] = static_cast<u8>((bits >> (k * 8u)) & 0xFFu);
            }
            break;
        }
        default:
            break;
    }
}

constexpr u64 TOMBSTONE_BITS = 0x7FFFFFFFFFFFFFFFULL;

AnvilValue map_get(const AnvilHeap& heap, const AnvilHeapObject& obj, AnvilValue key) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return AnvilValue::make_undefined();
    }
    const AnvilHeapArray& arr = heap.arrays()[arr_id];
    const AnvilValue* elems = heap.array_elems(arr.storage_off);
    for (u32 i = 0u; i + 1u < arr.length; i += 2u) {
        if (elems[i].bits != TOMBSTONE_BITS && same_value_zero(heap, elems[i], key)) {
            return elems[i + 1u];
        }
    }
    return AnvilValue::make_undefined();
}

bool map_has(const AnvilHeap& heap, const AnvilHeapObject& obj, AnvilValue key) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return false;
    }
    const AnvilHeapArray& arr = heap.arrays()[arr_id];
    const AnvilValue* elems = heap.array_elems(arr.storage_off);
    for (u32 i = 0u; i + 1u < arr.length; i += 2u) {
        if (elems[i].bits != TOMBSTONE_BITS && same_value_zero(heap, elems[i], key)) {
            return true;
        }
    }
    return false;
}

bool map_delete(AnvilHeap& heap, AnvilHeapObject& obj, AnvilValue key) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return false;
    }
    const AnvilHeapArray& arr = heap.arrays()[arr_id];
    AnvilValue* elems = heap.array_elems(arr.storage_off);
    for (u32 i = 0u; i + 1u < arr.length; i += 2u) {
        if (elems[i].bits != TOMBSTONE_BITS && same_value_zero(heap, elems[i], key)) {
            elems[i] = AnvilValue(TOMBSTONE_BITS);
            elems[i + 1u] = AnvilValue::make_undefined();
            if (obj.byte_length > 0u) --obj.byte_length;
            return true;
        }
    }
    return false;
}

void map_clear(AnvilHeap& heap, AnvilHeapObject& obj) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id < heap.array_count && heap.arrays()[arr_id].active) {
        heap.arrays()[arr_id].length = 0u;
    }
    obj.byte_length = 0u;
}

bool map_set(AnvilHeap& heap, u32 obj_id, AnvilValue key, AnvilValue val) noexcept {
    const u32 arr_id = heap.objects()[obj_id].aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return false;
    }
    const u32 len = heap.arrays()[arr_id].length;
    AnvilValue* elems = heap.array_elems(heap.arrays()[arr_id].storage_off);
    for (u32 i = 0u; i + 1u < len; i += 2u) {
        if (elems[i].bits != TOMBSTONE_BITS && same_value_zero(heap, elems[i], key)) {
            elems[i + 1u] = val;
            return true;
        }
    }
    if (heap.push_array_elem(arr_id, key) != StoreResult::Ok) return false;
    if (heap.push_array_elem(arr_id, val) != StoreResult::Ok) return false;
    ++heap.objects()[obj_id].byte_length;
    return true;
}

bool set_has(const AnvilHeap& heap, const AnvilHeapObject& obj, AnvilValue key) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return false;
    }
    const AnvilHeapArray& arr = heap.arrays()[arr_id];
    const AnvilValue* elems = heap.array_elems(arr.storage_off);
    for (u32 i = 0u; i < arr.length; ++i) {
        if (elems[i].bits != TOMBSTONE_BITS && same_value_zero(heap, elems[i], key)) {
            return true;
        }
    }
    return false;
}

bool set_delete(AnvilHeap& heap, AnvilHeapObject& obj, AnvilValue key) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return false;
    }
    const AnvilHeapArray& arr = heap.arrays()[arr_id];
    AnvilValue* elems = heap.array_elems(arr.storage_off);
    for (u32 i = 0u; i < arr.length; ++i) {
        if (elems[i].bits != TOMBSTONE_BITS && same_value_zero(heap, elems[i], key)) {
            elems[i] = AnvilValue(TOMBSTONE_BITS);
            if (obj.byte_length > 0u) --obj.byte_length;
            return true;
        }
    }
    return false;
}

void set_clear(AnvilHeap& heap, AnvilHeapObject& obj) noexcept {
    const u32 arr_id = obj.aux_id;
    if (arr_id < heap.array_count && heap.arrays()[arr_id].active) {
        heap.arrays()[arr_id].length = 0u;
    }
    obj.byte_length = 0u;
}

bool set_add(AnvilHeap& heap, u32 obj_id, AnvilValue key) noexcept {
    const u32 arr_id = heap.objects()[obj_id].aux_id;
    if (arr_id >= heap.array_count || !heap.arrays()[arr_id].active) {
        return false;
    }
    if (set_has(heap, heap.objects()[obj_id], key)) {
        return true;
    }
    if (heap.push_array_elem(arr_id, key) != StoreResult::Ok) return false;
    ++heap.objects()[obj_id].byte_length;
    return true;
}

f64 string_to_js_number(const char* ptr, u32 len) noexcept {
    u32 s = 0u;
    u32 e = len;
    while (s < e && (ptr[s] == ' ' || ptr[s] == '\t' || ptr[s] == '\r' || ptr[s] == '\n')) ++s;
    while (e > s && (ptr[e - 1u] == ' ' || ptr[e - 1u] == '\t' || ptr[e - 1u] == '\r' || ptr[e - 1u] == '\n')) --e;
    if (s == e) return 0.0;
    const u32 slen = e - s;
    const char* p = ptr + s;
    if (slen == 8u && cstr_eq(p, 8u, "Infinity", 8u)) {
        return bits_to_f64(0x7FF0000000000000ULL);
    }
    if (slen == 9u && (p[0] == '+' || p[0] == '-') && cstr_eq(p + 1, 8u, "Infinity", 8u)) {
        return bits_to_f64(p[0] == '-' ? 0xFFF0000000000000ULL : 0x7FF0000000000000ULL);
    }
    f64 sign = 1.0;
    u32 i = 0u;
    if (p[0] == '+' || p[0] == '-') {
        if (p[0] == '-') sign = -1.0;
        ++i;
    }
    if (i >= slen) return bits_to_f64(0x7FF8000000000000ULL);
    f64 val = 0.0;
    u32 digits = 0u;
    while (i < slen && p[i] >= '0' && p[i] <= '9') {
        val = val * 10.0 + static_cast<f64>(p[i] - '0');
        ++digits;
        ++i;
    }
    if (i < slen && p[i] == '.') {
        ++i;
        f64 scale = 0.1;
        while (i < slen && p[i] >= '0' && p[i] <= '9') {
            val += static_cast<f64>(p[i] - '0') * scale;
            scale *= 0.1;
            ++digits;
            ++i;
        }
    }
    if (digits == 0u || i < slen) {
        return bits_to_f64(0x7FF8000000000000ULL);
    }
    return sign * val;
}

f64 coerce_to_number(const AnvilHeap& heap, AnvilValue v) noexcept {
    if (v.is_string()) {
        u32 len = 0u;
        const char* p = heap.get_string_ptr(v.payload(), &len);
        return string_to_js_number(p, len);
    }
    return v.as_number();
}

} // namespace

namespace {
constexpr u32 INIT_GLOBAL_TABLE = 16u;
constexpr u32 INIT_CONSTANT_TABLE = 64u;
constexpr u32 INIT_FRAME_STACK = 8u;
} // namespace

bool AnvilVmContext::ensure_globals(u32 need) noexcept {
    if (need <= globals_cap) {
        return true;
    }
    u32 new_cap = globals_cap == 0u ? INIT_GLOBAL_TABLE : globals_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = heap.arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilGlobalBinding)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilGlobalBinding* dst = reinterpret_cast<AnvilGlobalBinding*>(heap.arena.at(new_off));
    const AnvilGlobalBinding* src = globals();
    for (u32 i = 0u; i < global_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = globals_off;
    const u32 old_cap = globals_cap;
    globals_off = new_off;
    globals_cap = new_cap;
    if (old_cap != 0u) {
        heap.arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilGlobalBinding)));
    }
    return true;
}

bool AnvilBytecodeProgram::ensure_constants(u32 need) noexcept {
    if (need <= constants_cap) {
        return true;
    }
    u32 new_cap = constants_cap == 0u ? INIT_CONSTANT_TABLE : constants_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena->alloc(new_cap * static_cast<u32>(sizeof(AnvilValue)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilValue* dst = reinterpret_cast<AnvilValue*>(arena->at(new_off));
    const AnvilValue* src = constants();
    for (u32 i = 0u; i < const_len; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = constants_off;
    const u32 old_cap = constants_cap;
    constants_off = new_off;
    constants_cap = new_cap;
    if (old_cap != 0u) {
        arena->free(old_off, old_cap * static_cast<u32>(sizeof(AnvilValue)));
    }
    return true;
}

void AnvilVmContext::init() noexcept {
    u8* arena_base = reinterpret_cast<u8*>(this) + sizeof(AnvilVmContext);
    const u32 budget = heap_byte_budget != 0u ? heap_byte_budget : ANVIL_DEFAULT_HEAP_BUDGET;
    heap.init(arena_base, ANVIL_ARENA_RESERVE_BYTES, budget);
    ic.init(&heap.arena);
    program.init(&heap.arena);
    registers_off = 0u;
    registers_cap = 0u;
    frames_off = 0u;
    frames_cap = 0u;
    ensure_registers(INIT_FRAME_STACK * INIT_REGISTERS_PER_FRAME);
    ensure_frames(INIT_FRAME_STACK);
    globals_off = 0u;
    globals_cap = 0u;
    global_count = 0u;
    ensure_globals(INIT_GLOBAL_TABLE);
    frame_count = 0u;
    try_depth = 0u;
    paused = false;
    resume_pc = 0u;
    resume_reg_base = 0u;
    resume_closure = NO_SLOT;
    resume_this = AnvilValue::make_undefined();
    pending_host_call = false;
    pending_host_fn = 0u;
    pending_arg_count = 0u;
    pending_host_overflow = 0u;
    resume_host_dst = 0u;
    resume_frame_regs = 0u;
    steps_executed = 0u;
    hot_loops_accelerated = 0u;
    native_compilations = 0u;
    aborted = false;
    error.clear();
    host_id_count = 0u;
    host_set_text = nullptr;
    host = nullptr;
    for (u32 i = 0u; i < 8u; ++i) {
        host_id_lens[i] = 0u;
        host_ids[i][0] = '\0';
    }
}

u16 AnvilVmContext::resolve_global_slot(u32 name_sym) noexcept {
    for (u32 i = 0; i < global_count; ++i) {
        if (globals()[i].occupied && globals()[i].name_sym == name_sym) {
            return static_cast<u16>(i);
        }
    }
    if (!ensure_globals(global_count + 1u)) {
        return NO_GLOBAL;
    }
    const u16 slot = static_cast<u16>(global_count++);
    AnvilGlobalBinding& g = globals()[slot];
    g.name_sym = name_sym;
    g.value = AnvilValue::make_undefined();
    g.occupied = true;
    g.exported = false;
    g.declared = false;
    g.decl_depth = 0xFFFFFFFFu;
    return slot;
}

void AnvilVmContext::reclaim_array_storage(u32 reg_top) noexcept {
    collect_garbage(reg_top);
}

bool AnvilVmContext::store_ok(StoreResult stored) noexcept {
    switch (stored) {
        case StoreResult::TooManyProperties:
        case StoreResult::ShapesFull:
        case StoreResult::ArrayTooLong:
        case StoreResult::ArrayStorageFull:
        case StoreResult::HeapFull:
            fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
            return false;
        case StoreResult::NotStored:
            fail(VM_STOP_UNSUPPORTED, "property write on a value that is not an object");
            return false;
        case StoreResult::Ok:
            break;
    }
    return true;
}

bool AnvilVmContext::ensure_registers(u32 need) noexcept {
    if (need <= registers_cap) {
        return true;
    }
    u32 new_cap = registers_cap == 0u ? (INIT_FRAME_STACK * INIT_REGISTERS_PER_FRAME) : registers_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = heap.arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilValue)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilValue* dst = reinterpret_cast<AnvilValue*>(heap.arena.at(new_off));
    const AnvilValue* src = registers();
    for (u32 i = 0u; i < registers_cap; ++i) {
        dst[i] = src[i];
    }
    for (u32 i = registers_cap; i < new_cap; ++i) {
        dst[i] = AnvilValue::make_undefined();
    }
    const u32 old_off = registers_off;
    const u32 old_cap = registers_cap;
    registers_off = new_off;
    registers_cap = new_cap;
    if (old_cap != 0u) {
        heap.arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilValue)));
    }
    return true;
}

bool AnvilVmContext::ensure_frames(u32 need) noexcept {
    if (need <= frames_cap) {
        return true;
    }
    u32 new_cap = frames_cap == 0u ? INIT_FRAME_STACK : frames_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = heap.arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilCallFrame)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilCallFrame* dst = reinterpret_cast<AnvilCallFrame*>(heap.arena.at(new_off));
    const AnvilCallFrame* src = frames();
    for (u32 i = 0u; i < frame_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = frames_off;
    const u32 old_cap = frames_cap;
    frames_off = new_off;
    frames_cap = new_cap;
    if (old_cap != 0u) {
        heap.arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilCallFrame)));
    }
    return true;
}

void AnvilVmContext::collect_garbage(u32 reg_top) noexcept {
    heap.clear_marks();
    const u32 top = reg_top < registers_cap ? reg_top : registers_cap;
    for (u32 i = 0; i < top; ++i) {
        heap.mark_value(registers()[i]);
    }
    for (u32 i = 0; i < global_count; ++i) {
        if (globals()[i].occupied) {
            heap.mark_value(globals()[i].value);
        }
    }
    for (u32 i = 0; i < program.const_len; ++i) {
        heap.mark_value(program.constants()[i]);
    }
    for (u32 i = 0; i < frame_count; ++i) {
        if (frames()[i].closure_id != NO_SLOT) {
            heap.mark_closure(frames()[i].closure_id);
        }
        if (frames()[i].new_instance_obj != NO_SLOT) {
            heap.mark_object(frames()[i].new_instance_obj);
        }
        heap.mark_value(frames()[i].caller_this);
    }
    for (u32 i = 0; i < try_depth; ++i) {
        if (try_stack[i].cur_closure != NO_SLOT) {
            heap.mark_closure(try_stack[i].cur_closure);
        }
        heap.mark_value(try_stack[i].cur_this);
        heap.mark_value(try_stack[i].completion_val);
    }
    if (resume_closure != NO_SLOT) {
        heap.mark_closure(resume_closure);
    }
    heap.mark_value(resume_this);
    if (heap.err_proto_id != NO_SLOT) heap.mark_object(heap.err_proto_id);
    if (heap.type_err_proto_id != NO_SLOT) heap.mark_object(heap.type_err_proto_id);
    if (heap.range_err_proto_id != NO_SLOT) heap.mark_object(heap.range_err_proto_id);
    if (heap.syntax_err_proto_id != NO_SLOT) heap.mark_object(heap.syntax_err_proto_id);
    if (heap.ref_err_proto_id != NO_SLOT) heap.mark_object(heap.ref_err_proto_id);
    for (u32 i = 0; i < heap.function_count; ++i) {
        if (heap.functions()[i].active && heap.functions()[i].canonical_closure != NO_SLOT) {
            heap.mark_closure(heap.functions()[i].canonical_closure);
        }
        if (heap.functions()[i].active && heap.functions()[i].home_proto_id != NO_SLOT) {
            heap.mark_object(heap.functions()[i].home_proto_id);
        }
    }
    heap.sweep_unmarked();
}

AnvilValue AnvilVmContext::execute(u32 entry_pc) noexcept {
    if (yield_interval == 0u) {
        f64 native_out = 0.0;
        if (anvil_jit_try_run(this, entry_pc, &native_out)) {
            ++native_compilations;
            return AnvilValue::make_number(native_out);
        }
    }

    frame_count = 0u;
    try_depth = 0u;
    const u32 top_regs = program.top_frame_regs != 0u ? program.top_frame_regs : 1u;
    return run_loop(entry_pc, 0u, top_regs, NO_SLOT, AnvilValue::make_undefined());
}

AnvilValue AnvilVmContext::resume() noexcept {
    paused = false;
    if (pending_host_call) {
        pending_host_call = false;
        const u32 dst = resume_reg_base + resume_host_dst;
        if (dst < registers_cap) {
            registers()[dst] = AnvilValue::make_undefined();
        }
    }
    return run_loop(resume_pc, resume_reg_base, resume_frame_regs, resume_closure, resume_this);
}

AnvilValue AnvilVmContext::run_loop(
    u32 pc,
    u32 reg_base,
    u32 frame_regs,
    u32 cur_closure_in,
    AnvilValue cur_this_in,
    u32 stop_frame_depth
) noexcept {
    const u32 step_limit_now = effective_step_limit();
    const u32 base_frame_depth = stop_frame_depth;
    u32 cur_frame_regs = frame_regs;
    u32 cur_closure = cur_closure_in;
    AnvilValue cur_this = cur_this_in;
    u32 slice = 0u;

    auto sync_gc_roots = [&]() noexcept {
        resume_closure = cur_closure;
        resume_this = cur_this;
    };

    auto unwind_throw = [&](AnvilValue exc_val) noexcept -> bool {
        while (try_depth > 0u) {
            AnvilTryHandler& h = try_stack[try_depth - 1u];
            if (h.frame_depth < base_frame_depth) {
                break;
            }
            frame_count = h.frame_depth;
            reg_base = h.reg_base;
            cur_frame_regs = h.cur_frame_regs;
            cur_closure = h.cur_closure;
            cur_this = h.cur_this;
            if (h.catch_pc != 0u) {
                const u32 target_pc = h.catch_pc;
                const u16 exc_r = h.catch_reg;
                h.catch_pc = 0u;
                if (h.finally_pc == 0u) {
                    --try_depth;
                }
                registers()[reg_base + exc_r] = exc_val;
                pc = target_pc;
                return true;
            }
            if (h.finally_pc != 0u) {
                const u32 target_pc = h.finally_pc;
                h.finally_pc = 0u;
                h.completion_kind = TryCompletionKind::Throw;
                h.completion_val = exc_val;
                pc = target_pc;
                return true;
            }
            --try_depth;
        }
        sync_gc_roots();
        const u32 sym = heap.value_to_string_sym(exc_val);
        u32 len = 0u;
        const char* s = heap.get_string_ptr(sym, &len);
        aborted = true;
        error.clear();
        error.code = VM_STOP_UNCAUGHT;
        error.add("Uncaught ");
        error.add_source(s, len);
        return false;
    };

    auto raise_builtin_error = [&](BuiltinCtorKind kind, const char* msg, u32 stop_code) noexcept -> bool {
        if (try_depth > 0u) {
            u32 mlen = 0u;
            while (msg[mlen] != '\0') ++mlen;
            sync_gc_roots();
            u32 msg_sym = heap.intern_string(msg, mlen);
            if (msg_sym == NO_SLOT) {
                collect_garbage(reg_base + cur_frame_regs);
                msg_sym = heap.intern_string(msg, mlen);
            }
            u32 err_id = heap.alloc_error_object(kind, msg_sym);
            if (err_id == NO_SLOT) {
                collect_garbage(reg_base + cur_frame_regs);
                err_id = heap.alloc_error_object(kind, msg_sym);
            }
            if (err_id != NO_SLOT) {
                return unwind_throw(AnvilValue::make_object(err_id));
            }
        }
        fail(stop_code, msg);
        return false;
    };

    while (pc < program.code_len) {
        if (yield_interval != 0u && slice >= yield_interval) {
            paused = true;
            resume_pc = pc;
            resume_reg_base = reg_base;
            resume_frame_regs = cur_frame_regs;
            resume_closure = cur_closure;
            resume_this = cur_this;
            return AnvilValue::make_undefined();
        }
        ++slice;
        if (++steps_executed > step_limit_now) {
            fail_limit(VM_STOP_STEPS, "step", step_limit_now, "steps");
            return AnvilValue::make_undefined();
        }

        const AnvilInstruction inst = program.code()[pc++];
        AnvilValue* R = &registers()[reg_base];

        const u32 operands = numeric_operands(inst.op);
        if (operands != 0u &&
            (!numeric_operand(R[inst.b]) || (operands == 2u && !numeric_operand(R[inst.c])))) {
            if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                     "arithmetic or comparison on a string or object",
                                     VM_STOP_UNSUPPORTED)) {
                return AnvilValue::make_undefined();
            }
            continue;
        }

        switch (inst.op) {
            case AnvilOpcode::Nop:
                break;

            case AnvilOpcode::LoadConst: {
                const u32 idx = inst.imm32();
                R[inst.a] = (idx < program.const_len) ? program.constants()[idx] : AnvilValue::make_null();
                break;
            }

            case AnvilOpcode::LoadSmi:
                R[inst.a] = AnvilValue::make_smi(inst.signed_imm16());
                break;

            case AnvilOpcode::LoadNull:
                R[inst.a] = AnvilValue::make_null();
                break;

            case AnvilOpcode::LoadTrue:
                R[inst.a] = AnvilValue::make_bool(true);
                break;

            case AnvilOpcode::LoadFalse:
                R[inst.a] = AnvilValue::make_bool(false);
                break;

            case AnvilOpcode::LoadThis:
                R[inst.a] = cur_this;
                break;

            case AnvilOpcode::Move:
                R[inst.a] = R[inst.b];
                break;

            case AnvilOpcode::Add: {
                const AnvilValue lhs = R[inst.b];
                const AnvilValue rhs = R[inst.c];
                if (lhs.is_smi() && rhs.is_smi()) {
                    const i64 sum = static_cast<i64>(lhs.as_smi()) + static_cast<i64>(rhs.as_smi());
                    if (sum >= -1073741824LL && sum <= 1073741823LL) {
                        R[inst.a] = AnvilValue::make_smi(static_cast<i32>(sum));
                        break;
                    }
                }
                if (lhs.is_string() || rhs.is_string()) {
                    sync_gc_roots();
                    u32 sym_a = heap.value_to_string_sym(lhs);
                    u32 sym_b = heap.value_to_string_sym(rhs);
                    u32 sym = (sym_a != NO_SLOT && sym_b != NO_SLOT)
                        ? heap.intern_concat(sym_a, sym_b)
                        : NO_SLOT;
                    if (sym == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        sym_a = heap.value_to_string_sym(lhs);
                        sym_b = heap.value_to_string_sym(rhs);
                        sym = (sym_a != NO_SLOT && sym_b != NO_SLOT)
                            ? heap.intern_concat(sym_a, sym_b)
                            : NO_SLOT;
                    }
                    if (sym == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    R = &registers()[reg_base];
                    R[inst.a] = AnvilValue::make_string(sym);
                    break;
                }
                if (!numeric_operand(lhs) || !numeric_operand(rhs)) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "arithmetic or comparison on a string or object",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                R[inst.a] = AnvilValue::make_number(lhs.as_number() + rhs.as_number());
                break;
            }

            case AnvilOpcode::Sub: {
                const AnvilValue lhs = R[inst.b];
                const AnvilValue rhs = R[inst.c];
                if (lhs.is_smi() && rhs.is_smi()) {
                    const i64 diff = static_cast<i64>(lhs.as_smi()) - static_cast<i64>(rhs.as_smi());
                    if (diff >= -1073741824LL && diff <= 1073741823LL) {
                        R[inst.a] = AnvilValue::make_smi(static_cast<i32>(diff));
                        break;
                    }
                }
                R[inst.a] = AnvilValue::make_number(lhs.as_number() - rhs.as_number());
                break;
            }

            case AnvilOpcode::Mul: {
                const AnvilValue lhs = R[inst.b];
                const AnvilValue rhs = R[inst.c];
                if (lhs.is_smi() && rhs.is_smi()) {
                    const i64 prod = static_cast<i64>(lhs.as_smi()) * static_cast<i64>(rhs.as_smi());
                    const bool negative_zero = prod == 0 && (lhs.as_smi() < 0 || rhs.as_smi() < 0);
                    if (!negative_zero && prod >= -1073741824LL && prod <= 1073741823LL) {
                        R[inst.a] = AnvilValue::make_smi(static_cast<i32>(prod));
                        break;
                    }
                }
                R[inst.a] = AnvilValue::make_number(lhs.as_number() * rhs.as_number());
                break;
            }

            case AnvilOpcode::Div:
                R[inst.a] = AnvilValue::make_number(R[inst.b].as_number() / R[inst.c].as_number());
                break;

            case AnvilOpcode::Mod:
                R[inst.a] = AnvilValue::make_number(js_remainder(R[inst.b].as_number(), R[inst.c].as_number()));
                break;

            case AnvilOpcode::Neg:
                R[inst.a] = AnvilValue::make_number(-R[inst.b].as_number());
                break;

            case AnvilOpcode::Not:
                R[inst.a] = AnvilValue::make_bool(!truthy(heap, R[inst.b]));
                break;

            case AnvilOpcode::BitNot:
                R[inst.a] = AnvilValue::make_smi(~js_to_int32(R[inst.b].as_number()));
                break;

            case AnvilOpcode::ToNum:
                R[inst.a] = AnvilValue::make_number(coerce_to_number(heap, R[inst.b]));
                break;

            case AnvilOpcode::BitAnd:
                R[inst.a] = AnvilValue::make_smi(
                    js_to_int32(R[inst.b].as_number()) & js_to_int32(R[inst.c].as_number())
                );
                break;

            case AnvilOpcode::BitOr:
                R[inst.a] = AnvilValue::make_smi(
                    js_to_int32(R[inst.b].as_number()) | js_to_int32(R[inst.c].as_number())
                );
                break;

            case AnvilOpcode::BitXor:
                R[inst.a] = AnvilValue::make_smi(
                    js_to_int32(R[inst.b].as_number()) ^ js_to_int32(R[inst.c].as_number())
                );
                break;

            case AnvilOpcode::Shl: {
                const i32 s = js_to_int32(R[inst.c].as_number()) & 31;
                R[inst.a] = AnvilValue::make_smi(js_to_int32(R[inst.b].as_number()) << s);
                break;
            }

            case AnvilOpcode::Shr: {
                const i32 s = js_to_int32(R[inst.c].as_number()) & 31;
                R[inst.a] = AnvilValue::make_smi(js_to_int32(R[inst.b].as_number()) >> s);
                break;
            }

            case AnvilOpcode::UShr: {
                const u32 s = js_to_uint32(R[inst.c].as_number()) & 31u;
                const u32 u = js_to_uint32(R[inst.b].as_number()) >> s;
                R[inst.a] = AnvilValue::make_number(static_cast<f64>(u));
                break;
            }

            case AnvilOpcode::CmpEq:
                R[inst.a] = AnvilValue::make_bool(values_strictly_equal(heap, R[inst.b], R[inst.c]));
                break;

            case AnvilOpcode::CmpNe:
                R[inst.a] = AnvilValue::make_bool(!values_strictly_equal(heap, R[inst.b], R[inst.c]));
                break;

            case AnvilOpcode::CmpLooseEq:
            case AnvilOpcode::CmpLooseNe: {
                bool known = true;
                const bool equal = values_loosely_equal(heap, R[inst.b], R[inst.c], known);
                if (!known) {
                    fail(VM_STOP_UNSUPPORTED, "== between values of different types");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_bool(inst.op == AnvilOpcode::CmpLooseEq ? equal : !equal);
                break;
            }

            case AnvilOpcode::CmpLt:
            case AnvilOpcode::CmpLe:
            case AnvilOpcode::CmpGt:
            case AnvilOpcode::CmpGe: {
                const AnvilValue lhs = R[inst.b];
                const AnvilValue rhs = R[inst.c];
                if (lhs.is_string() && rhs.is_string()) {
                    u32 la = 0u, lb = 0u;
                    const char* pa = heap.get_string_ptr(lhs.payload(), &la);
                    const char* pb = heap.get_string_ptr(rhs.payload(), &lb);
                    const i32 cmp = utf16_compare(pa, la, pb, lb);
                    bool res = false;
                    if (inst.op == AnvilOpcode::CmpLt) res = (cmp < 0);
                    else if (inst.op == AnvilOpcode::CmpLe) res = (cmp <= 0);
                    else if (inst.op == AnvilOpcode::CmpGt) res = (cmp > 0);
                    else res = (cmp >= 0);
                    R[inst.a] = AnvilValue::make_bool(res);
                    break;
                }
                if (!numeric_operand(lhs) || !numeric_operand(rhs)) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "arithmetic or comparison on a string or object",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const f64 na = lhs.as_number();
                const f64 nb = rhs.as_number();
                bool res = false;
                if (inst.op == AnvilOpcode::CmpLt) res = (na < nb);
                else if (inst.op == AnvilOpcode::CmpLe) res = (na <= nb);
                else if (inst.op == AnvilOpcode::CmpGt) res = (na > nb);
                else res = (na >= nb);
                R[inst.a] = AnvilValue::make_bool(res);
                break;
            }

            case AnvilOpcode::TypeOf: {
                const AnvilValue v = R[inst.b];
                const char* ts = "undefined";
                u32 tlen = 9u;
                switch (v.tag()) {
                    case ValueTag::Undefined: ts = "undefined"; tlen = 9u; break;
                    case ValueTag::Null:      ts = "object";    tlen = 6u; break;
                    case ValueTag::Bool:      ts = "boolean";   tlen = 7u; break;
                    case ValueTag::Smi:
                    case ValueTag::Number:    ts = "number";    tlen = 6u; break;
                    case ValueTag::String:    ts = "string";    tlen = 6u; break;
                    case ValueTag::Object:
                    case ValueTag::Array:
                    case ValueTag::DomNode:   ts = "object";    tlen = 6u; break;
                    case ValueTag::Function:  ts = "function";  tlen = 8u; break;
                }
                sync_gc_roots();
                u32 sym = heap.intern_string(ts, tlen);
                if (sym == NO_SLOT) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    sym = heap.intern_string(ts, tlen);
                }
                if (sym == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_string(sym);
                break;
            }

            case AnvilOpcode::InOp: {
                const AnvilValue key_val = R[inst.b];
                const AnvilValue target = R[inst.c];
                if (target.is_array()) {
                    const i32 idx = key_val.is_number() ? array_index(key_val.as_number()) : -1;
                    const u32 arr_id = target.payload();
                    const bool ok = (idx >= 0 && arr_id < heap.array_count &&
                                     heap.arrays()[arr_id].active &&
                                     static_cast<u32>(idx) < heap.arrays()[arr_id].length);
                    R[inst.a] = AnvilValue::make_bool(ok);
                    break;
                }
                if (!target.is_object()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "in operator on non-object",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                sync_gc_roots();
                const u32 prop_sym = heap.value_to_string_sym(key_val);
                R = &registers()[reg_base];
                R[inst.a] = AnvilValue::make_bool(heap.has_object_prop(target.payload(), prop_sym));
                break;
            }

            case AnvilOpcode::InstanceOf: {
                const AnvilValue lhs = R[inst.b];
                const AnvilValue rhs = R[inst.c];
                if (!rhs.is_function()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "instanceof right-hand side is not callable",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                bool found = false;
                const u32 cid = rhs.payload();
                if ((cid & 0xFFFFFF00u) == 0xFFFFFF00u) {
                    const BuiltinCtorKind bkind = static_cast<BuiltinCtorKind>(cid & 0xFFu);
                    if (bkind == BuiltinCtorKind::Error) {
                        found = lhs.is_object() &&
                                static_cast<BuiltinObjectKind>(heap.objects()[lhs.payload()].builtin_kind) == BuiltinObjectKind::Error;
                    } else if (bkind >= BuiltinCtorKind::TypeError && bkind <= BuiltinCtorKind::ReferenceError) {
                        found = lhs.is_object() &&
                                static_cast<BuiltinObjectKind>(heap.objects()[lhs.payload()].builtin_kind) == BuiltinObjectKind::Error &&
                                heap.objects()[lhs.payload()].aux_id == static_cast<u32>(bkind);
                    } else if (bkind == BuiltinCtorKind::Map) {
                        found = lhs.is_object() &&
                                static_cast<BuiltinObjectKind>(heap.objects()[lhs.payload()].builtin_kind) == BuiltinObjectKind::Map;
                    } else if (bkind == BuiltinCtorKind::Set) {
                        found = lhs.is_object() &&
                                static_cast<BuiltinObjectKind>(heap.objects()[lhs.payload()].builtin_kind) == BuiltinObjectKind::Set;
                    } else if (bkind == BuiltinCtorKind::ArrayBuffer) {
                        found = lhs.is_object() &&
                                static_cast<BuiltinObjectKind>(heap.objects()[lhs.payload()].builtin_kind) == BuiltinObjectKind::ArrayBuffer;
                    } else if (bkind >= BuiltinCtorKind::Uint8Array && bkind <= BuiltinCtorKind::Float64Array) {
                        const u8 tkind = static_cast<u8>(BuiltinObjectKind::Uint8Array) +
                                         (static_cast<u8>(bkind) - static_cast<u8>(BuiltinCtorKind::Uint8Array));
                        found = lhs.is_object() && heap.objects()[lhs.payload()].builtin_kind == tkind;
                    } else if (bkind == BuiltinCtorKind::Array) {
                        found = lhs.is_array();
                    } else if (bkind == BuiltinCtorKind::Object) {
                        found = lhs.is_object() || lhs.is_array() || lhs.is_function();
                    }
                } else if (lhs.is_object() && cid < heap.closure_count && heap.closures()[cid].active) {
                    const u32 target_proto = heap.closures()[cid].proto_obj_id;
                    if (target_proto != NO_SLOT) {
                        u32 cur_obj = lhs.payload();
                        u32 hops = 0u;
                        while (cur_obj < heap.object_count && heap.objects()[cur_obj].active && hops < 16u) {
                            const u32 p = heap.objects()[cur_obj].proto_obj_id;
                            if (p == target_proto) {
                                found = true;
                                break;
                            }
                            if (p == NO_SLOT || p == cur_obj) {
                                break;
                            }
                            cur_obj = p;
                            ++hops;
                        }
                    }
                }
                R[inst.a] = AnvilValue::make_bool(found);
                break;
            }

            case AnvilOpcode::DeleteProp: {
                const AnvilValue recv = R[inst.b];
                const u32 prop_sym = inst.imm32();
                if (recv.is_object()) {
                    heap.delete_object_prop(recv.payload(), prop_sym);
                }
                R[inst.a] = AnvilValue::make_bool(true);
                break;
            }

            case AnvilOpcode::DeleteElem: {
                const AnvilValue recv = R[inst.b];
                const AnvilValue key = R[inst.c];
                if (recv.is_object()) {
                    sync_gc_roots();
                    const u32 prop_sym = heap.value_to_string_sym(key);
                    R = &registers()[reg_base];
                    heap.delete_object_prop(recv.payload(), prop_sym);
                }
                R[inst.a] = AnvilValue::make_bool(true);
                break;
            }

            case AnvilOpcode::StringFromCharCode: {
                const u32 code_unit = js_to_uint32(R[inst.b].as_number()) & 0xFFFFu;
                char utf8_buf[3];
                const u32 n = encode_unit_utf8(static_cast<u16>(code_unit), utf8_buf);
                sync_gc_roots();
                u32 sym = heap.intern_string(utf8_buf, n);
                if (sym == NO_SLOT) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    sym = heap.intern_string(utf8_buf, n);
                }
                if (sym == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_string(sym);
                break;
            }

            case AnvilOpcode::Jump:
                pc = inst.imm32();
                break;

            case AnvilOpcode::JumpIfFalse:
                if (!truthy(heap, R[inst.a])) {
                    pc = inst.imm32();
                }
                break;

            case AnvilOpcode::JumpIfTrue:
                if (truthy(heap, R[inst.a])) {
                    pc = inst.imm32();
                }
                break;

            case AnvilOpcode::GetGlobal: {
                const u32 slot = inst.imm32();
                R[inst.a] = (slot < global_count) ? globals()[slot].value : AnvilValue::make_undefined();
                break;
            }

            case AnvilOpcode::SetGlobal: {
                const u32 slot = inst.imm32();
                if (slot < global_count) {
                    globals()[slot].value = R[inst.a];
                    globals()[slot].occupied = true;
                }
                break;
            }

            case AnvilOpcode::NewObject: {
                sync_gc_roots();
                u32 obj_id = heap.alloc_object(0u);
                if (obj_id == NO_SLOT) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    obj_id = heap.alloc_object(0u);
                }
                if (obj_id == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_object(obj_id);
                break;
            }

            case AnvilOpcode::NewArray: {
                sync_gc_roots();
                u32 arr_id = heap.alloc_array(inst.b);
                if (arr_id == NO_SLOT) {
                    reclaim_array_storage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    arr_id = heap.alloc_array(inst.b);
                }
                if (arr_id == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_array(arr_id);
                break;
            }

            case AnvilOpcode::GetPropIC: {
                const u32 site_id = inst.c;
                const u32 prop_sym = (site_id < ic.site_count) ? ic.sites()[site_id].prop_sym : 0u;
                const AnvilValue recv = R[inst.b];
                if (!recv.is_object() && !recv.is_array() && !recv.is_string() && !recv.is_function()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "property read on a value that is not an object or array",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                sync_gc_roots();
                R[inst.a] = ic.load_property_ic(heap, site_id, recv, prop_sym);
                break;
            }

            case AnvilOpcode::SetPropIC: {
                const u32 site_id = inst.c;
                const u32 prop_sym = (site_id < ic.site_count) ? ic.sites()[site_id].prop_sym : 0u;
                sync_gc_roots();
                StoreResult stored = ic.store_property_ic(heap, site_id, R[inst.a], prop_sym, R[inst.b]);
                if (stored == StoreResult::HeapFull) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    stored = ic.store_property_ic(heap, site_id, R[inst.a], prop_sym, R[inst.b]);
                }
                if (!store_ok(stored)) {
                    return AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::GetElem: {
                const AnvilValue recv = R[inst.b];
                const AnvilValue key = R[inst.c];
                if (recv.is_array() && key.is_number()) {
                    R[inst.a] = heap.get_array_elem(recv.payload(), array_index(key.as_number()));
                    break;
                }
                if (recv.is_string() && key.is_number()) {
                    const i32 idx = array_index(key.as_number());
                    if (idx < 0) {
                        R[inst.a] = AnvilValue::make_undefined();
                        break;
                    }
                    u32 blen = 0u;
                    const char* ptr = heap.get_string_ptr(recv.payload(), &blen);
                    u16 unit = 0u;
                    if (!utf16_at(ptr, blen, static_cast<u32>(idx), &unit)) {
                        R[inst.a] = AnvilValue::make_undefined();
                        break;
                    }
                    char ubuf[3];
                    const u32 un = encode_unit_utf8(unit, ubuf);
                    sync_gc_roots();
                    u32 csym = heap.intern_string(ubuf, un);
                    if (csym == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        csym = heap.intern_string(ubuf, un);
                    }
                    R[inst.a] = (csym != NO_SLOT) ? AnvilValue::make_string(csym) : AnvilValue::make_undefined();
                    break;
                }
                if (recv.is_object()) {
                    const u32 obj_id = recv.payload();
                    if (obj_id < heap.object_count && heap.objects()[obj_id].active) {
                        const BuiltinObjectKind bkind = static_cast<BuiltinObjectKind>(heap.objects()[obj_id].builtin_kind);
                        if (bkind >= BuiltinObjectKind::Uint8Array &&
                            bkind <= BuiltinObjectKind::Float64Array) {
                            const i32 idx = key.is_number() ? array_index(key.as_number()) : -1;
                            R[inst.a] = typed_array_get_elem(heap, heap.objects()[obj_id], idx);
                            break;
                        }
                        sync_gc_roots();
                        const u32 prop_sym = heap.value_to_string_sym(key);
                        R = &registers()[reg_base];
                        R[inst.a] = heap.get_object_prop(obj_id, prop_sym, nullptr, nullptr);
                        break;
                    }
                }
                if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                         "element read the VM does not run",
                                         VM_STOP_UNSUPPORTED)) {
                    return AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::SetElem: {
                const AnvilValue recv = R[inst.a];
                const AnvilValue key = R[inst.b];
                const AnvilValue val = R[inst.c];
                if (recv.is_array()) {
                    const i32 idx = key.is_number() ? array_index(key.as_number()) : -1;
                    if (idx < 0) {
                        if (!raise_builtin_error(BuiltinCtorKind::RangeError,
                                                 "element write the VM does not run",
                                                 VM_STOP_UNSUPPORTED)) {
                            return AnvilValue::make_undefined();
                        }
                        break;
                    }
                    sync_gc_roots();
                    StoreResult stored = heap.set_array_elem(recv.payload(), idx, val);
                    if (stored == StoreResult::HeapFull) {
                        reclaim_array_storage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        stored = heap.set_array_elem(recv.payload(), idx, val);
                    }
                    if (!store_ok(stored)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                if (recv.is_object()) {
                    const u32 obj_id = recv.payload();
                    if (obj_id < heap.object_count && heap.objects()[obj_id].active) {
                        const BuiltinObjectKind bkind = static_cast<BuiltinObjectKind>(heap.objects()[obj_id].builtin_kind);
                        if (bkind >= BuiltinObjectKind::Uint8Array &&
                            bkind <= BuiltinObjectKind::Float64Array) {
                            const i32 idx = key.is_number() ? array_index(key.as_number()) : -1;
                            typed_array_set_elem(heap, heap.objects()[obj_id], idx, val);
                            break;
                        }
                        sync_gc_roots();
                        const u32 prop_sym = heap.value_to_string_sym(key);
                        R = &registers()[reg_base];
                        StoreResult stored = heap.set_object_prop(obj_id, prop_sym, val, nullptr, nullptr);
                        if (stored == StoreResult::HeapFull) {
                            collect_garbage(reg_base + cur_frame_regs);
                            R = &registers()[reg_base];
                            stored = heap.set_object_prop(obj_id, prop_sym, val, nullptr, nullptr);
                        }
                        if (!store_ok(stored)) {
                            return AnvilValue::make_undefined();
                        }
                        break;
                    }
                }
                if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                         "element write the VM does not run",
                                         VM_STOP_UNSUPPORTED)) {
                    return AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::ArrayPush: {
                const AnvilValue recv = R[inst.a];
                if (!recv.is_array()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "push on a value that is not an array",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                sync_gc_roots();
                StoreResult stored = heap.push_array_elem(recv.payload(), R[inst.b]);
                if (stored == StoreResult::HeapFull) {
                    reclaim_array_storage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    stored = heap.push_array_elem(recv.payload(), R[inst.b]);
                }
                if (!store_ok(stored)) {
                    return AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::Call: {
                const u32 fn_id = inst.b;
                if (fn_id >= heap.function_count || !heap.functions()[fn_id].active) {
                    R[inst.a] = AnvilValue::make_null();
                    break;
                }
                if (frame_count + 1u > effective_call_depth()) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", effective_call_depth(), "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                if (static_cast<u32>(inst.c) + fn.arity > cur_frame_regs) {
                    fail(VM_STOP_UNSUPPORTED, "call arguments outside the caller's registers");
                    return AnvilValue::make_undefined();
                }
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) {
                    callee_regs = fn.arity;
                }
                if (callee_regs == 0u) {
                    callee_regs = 1u;
                }
                const u32 next_base = reg_base + cur_frame_regs;
                if (!ensure_registers(next_base + callee_regs) ||
                    !ensure_frames(frame_count + 1u)) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                AnvilValue* regs = registers();
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = regs[reg_base + inst.c + a_idx];
                }
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, inst.a, NO_SLOT, cur_this};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = NO_SLOT;
                cur_this = AnvilValue::make_undefined();
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::LoadFunc: {
                const u32 fn_id = inst.imm32();
                u32 cid = NO_SLOT;
                if (fn_id < heap.function_count) {
                    cid = heap.functions()[fn_id].canonical_closure;
                    if (cid == NO_SLOT) {
                        cid = heap.alloc_closure(fn_id, NO_SLOT, 0u, true);
                        if (cid == NO_SLOT) {
                            fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                            return AnvilValue::make_undefined();
                        }
                        heap.functions()[fn_id].canonical_closure = cid;
                    }
                }
                R[inst.a] = AnvilValue::make_function(cid);
                break;
            }

            case AnvilOpcode::HostCall: {
                const u32 fn_id = inst.imm32();
                const u32 base = inst.b;
                const u32 argc = inst.c;
                const u32 kept = argc < MAX_HOST_ARGS ? argc : MAX_HOST_ARGS;
                for (u32 i = 0u; i < kept; ++i) {
                    pending_args[i] = R[base + i];
                }
                pending_host_fn = fn_id;
                pending_arg_count = kept;
                pending_host_overflow = argc > MAX_HOST_ARGS ? 1u : 0u;
                pending_host_call = true;
                paused = true;
                resume_pc = pc;
                resume_reg_base = reg_base;
                resume_frame_regs = cur_frame_regs;
                resume_closure = cur_closure;
                resume_this = cur_this;
                resume_host_dst = inst.a;
                return AnvilValue::make_undefined();
            }

            case AnvilOpcode::CallValue: {
                const AnvilValue callee = R[inst.b];
                if (!callee.is_function()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const u32 cid = callee.payload();
                if (cid >= heap.closure_count || !heap.closures()[cid].active) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const u32 fn_id = heap.closures()[cid].fn_id;
                if (fn_id >= heap.function_count || !heap.functions()[fn_id].active) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                if (frame_count + 1u > effective_call_depth()) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", effective_call_depth(), "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                const u32 argc = inst.imm32();
                if (static_cast<u32>(inst.c) + argc > cur_frame_regs) {
                    fail(VM_STOP_UNSUPPORTED, "call arguments outside the caller's registers");
                    return AnvilValue::make_undefined();
                }
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) {
                    callee_regs = fn.arity;
                }
                if (callee_regs == 0u) {
                    callee_regs = 1u;
                }
                const u32 next_base = reg_base + cur_frame_regs;
                if (!ensure_registers(next_base + callee_regs) ||
                    !ensure_frames(frame_count + 1u)) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                AnvilValue* regs = registers();
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = (a_idx < argc)
                        ? regs[reg_base + inst.c + a_idx]
                        : AnvilValue::make_undefined();
                }
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, inst.a, NO_SLOT, cur_this};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = cid;
                cur_this = AnvilValue::make_undefined();
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::CallMethod: {
                const u32 argc = inst.pad;
                const u16 dst_reg = inst.a;
                const AnvilValue recv = R[inst.b];
                const u32 call_base = inst.c;
                const u32 prop_sym = inst.imm32();
                u32 mlen = 0u;
                const char* mname = heap.get_string_ptr(prop_sym, &mlen);
                auto arg_at = [&](u32 idx) noexcept -> AnvilValue {
                    if (idx >= argc) return AnvilValue::make_undefined();
                    return registers()[reg_base + call_base + idx];
                };

                if (recv.is_array()) {
                    const u32 arr_id = recv.payload();
                    if (cstr_eq(mname, mlen, "push", 4u)) {
                        for (u32 i = 0; i < argc; ++i) {
                            const AnvilValue v = arg_at(i);
                            sync_gc_roots();
                            StoreResult stored = heap.push_array_elem(arr_id, v);
                            if (stored == StoreResult::HeapFull) {
                                reclaim_array_storage(reg_base + cur_frame_regs);
                                R = &registers()[reg_base];
                                stored = heap.push_array_elem(arr_id, v);
                            }
                            if (!store_ok(stored)) {
                                return AnvilValue::make_undefined();
                            }
                        }
                        R[dst_reg] = AnvilValue::make_smi(static_cast<i32>(heap.arrays()[arr_id].length));
                        break;
                    }
                    if (cstr_eq(mname, mlen, "pop", 3u)) {
                        AnvilHeapArray& arr = heap.arrays()[arr_id];
                        if (arr.length == 0u) {
                            R[dst_reg] = AnvilValue::make_undefined();
                        } else {
                            --arr.length;
                            R[dst_reg] = heap.array_elems(arr.storage_off)[arr.length];
                        }
                        break;
                    }
                    if (cstr_eq(mname, mlen, "slice", 5u)) {
                        const AnvilHeapArray& arr = heap.arrays()[arr_id];
                        const i32 len = static_cast<i32>(arr.length);
                        i32 start = 0;
                        i32 end = len;
                        if (argc >= 1u && !arg_at(0u).is_undefined()) {
                            start = js_to_int32(arg_at(0u).as_number());
                            if (start < 0) start = (len + start > 0) ? (len + start) : 0;
                            if (start > len) start = len;
                        }
                        if (argc >= 2u && !arg_at(1u).is_undefined()) {
                            end = js_to_int32(arg_at(1u).as_number());
                            if (end < 0) end = (len + end > 0) ? (len + end) : 0;
                            if (end > len) end = len;
                        }
                        const u32 count = (end > start) ? static_cast<u32>(end - start) : 0u;
                        sync_gc_roots();
                        u32 out_id = heap.alloc_array(count > 0u ? count : 1u);
                        if (out_id == NO_SLOT) {
                            reclaim_array_storage(reg_base + cur_frame_regs);
                            R = &registers()[reg_base];
                            out_id = heap.alloc_array(count > 0u ? count : 1u);
                        }
                        if (out_id == NO_SLOT) {
                            fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                            return AnvilValue::make_undefined();
                        }
                        for (u32 i = 0; i < count; ++i) {
                            const AnvilValue elem = heap.get_array_elem(arr_id, start + static_cast<i32>(i));
                            heap.push_array_elem(out_id, elem);
                        }
                        R = &registers()[reg_base];
                        R[dst_reg] = AnvilValue::make_array(out_id);
                        break;
                    }
                    if (cstr_eq(mname, mlen, "toString", 8u)) {
                        sync_gc_roots();
                        const u32 sym = heap.value_to_string_sym(recv);
                        R = &registers()[reg_base];
                        R[dst_reg] = AnvilValue::make_string(sym);
                        break;
                    }
                }

                if (recv.is_string()) {
                    u32 blen = 0u;
                    const char* ptr = heap.get_string_ptr(recv.payload(), &blen);
                    if (cstr_eq(mname, mlen, "charCodeAt", 10u)) {
                        const i32 idx = (argc >= 1u && arg_at(0u).is_number())
                            ? js_to_int32(arg_at(0u).as_number())
                            : 0;
                        u16 unit = 0u;
                        if (idx < 0 || !utf16_at(ptr, blen, static_cast<u32>(idx), &unit)) {
                            R[dst_reg] = AnvilValue::make_number(bits_to_f64(0x7FF8000000000000ULL));
                        } else {
                            R[dst_reg] = AnvilValue::make_smi(static_cast<i32>(unit));
                        }
                        break;
                    }
                    if (cstr_eq(mname, mlen, "slice", 5u) || cstr_eq(mname, mlen, "substring", 9u)) {
                        const bool is_sub = cstr_eq(mname, mlen, "substring", 9u);
                        const i32 ulen = static_cast<i32>(utf16_len(ptr, blen));
                        i32 start = 0;
                        i32 end = ulen;
                        if (argc >= 1u && !arg_at(0u).is_undefined()) {
                            start = js_to_int32(arg_at(0u).as_number());
                            if (is_sub) {
                                if (start < 0) start = 0;
                                if (start > ulen) start = ulen;
                            } else {
                                if (start < 0) start = (ulen + start > 0) ? (ulen + start) : 0;
                                if (start > ulen) start = ulen;
                            }
                        }
                        if (argc >= 2u && !arg_at(1u).is_undefined()) {
                            end = js_to_int32(arg_at(1u).as_number());
                            if (is_sub) {
                                if (end < 0) end = 0;
                                if (end > ulen) end = ulen;
                                if (start > end) {
                                    const i32 t = start;
                                    start = end;
                                    end = t;
                                }
                            } else {
                                if (end < 0) end = (ulen + end > 0) ? (ulen + end) : 0;
                                if (end > ulen) end = ulen;
                            }
                        }
                        sync_gc_roots();
                        u32 sym = utf16_slice_sym(heap, recv.payload(), start, end > start ? end : start);
                        if (sym == NO_SLOT) {
                            collect_garbage(reg_base + cur_frame_regs);
                            R = &registers()[reg_base];
                            sym = utf16_slice_sym(heap, recv.payload(), start, end > start ? end : start);
                        }
                        if (sym == NO_SLOT) {
                            fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                            return AnvilValue::make_undefined();
                        }
                        R = &registers()[reg_base];
                        R[dst_reg] = AnvilValue::make_string(sym);
                        break;
                    }
                    if (cstr_eq(mname, mlen, "indexOf", 7u)) {
                        if (argc == 0u || !arg_at(0u).is_string()) {
                            R[dst_reg] = AnvilValue::make_smi(-1);
                            break;
                        }
                        u32 nlen = 0u;
                        const char* nptr = heap.get_string_ptr(arg_at(0u).payload(), &nlen);
                        const i32 from = (argc >= 2u && arg_at(1u).is_number())
                            ? js_to_int32(arg_at(1u).as_number())
                            : 0;
                        const u32 h_ulen = utf16_len(ptr, blen);
                        const u32 n_ulen = utf16_len(nptr, nlen);
                        const u32 start_u = (from < 0) ? 0u : (static_cast<u32>(from) > h_ulen ? h_ulen : static_cast<u32>(from));
                        if (n_ulen == 0u) {
                            R[dst_reg] = AnvilValue::make_smi(static_cast<i32>(start_u));
                            break;
                        }
                        i32 found_idx = -1;
                        if (n_ulen <= h_ulen) {
                            for (u32 pos = start_u; pos + n_ulen <= h_ulen; ++pos) {
                                bool match = true;
                                for (u32 k = 0; k < n_ulen; ++k) {
                                    u16 hu = 0u, nu = 0u;
                                    utf16_at(ptr, blen, pos + k, &hu);
                                    utf16_at(nptr, nlen, k, &nu);
                                    if (hu != nu) {
                                        match = false;
                                        break;
                                    }
                                }
                                if (match) {
                                    found_idx = static_cast<i32>(pos);
                                    break;
                                }
                            }
                        }
                        R[dst_reg] = AnvilValue::make_smi(found_idx);
                        break;
                    }
                    if (cstr_eq(mname, mlen, "toString", 8u)) {
                        R[dst_reg] = recv;
                        break;
                    }
                }

                if (recv.is_object()) {
                    const u32 obj_id = recv.payload();
                    if (obj_id < heap.object_count && heap.objects()[obj_id].active) {
                        const BuiltinObjectKind bkind = static_cast<BuiltinObjectKind>(heap.objects()[obj_id].builtin_kind);
                        if (bkind == BuiltinObjectKind::Map) {
                            if (cstr_eq(mname, mlen, "get", 3u)) {
                                R[dst_reg] = map_get(heap, heap.objects()[obj_id], arg_at(0u));
                                break;
                            }
                            if (cstr_eq(mname, mlen, "has", 3u)) {
                                R[dst_reg] = AnvilValue::make_bool(map_has(heap, heap.objects()[obj_id], arg_at(0u)));
                                break;
                            }
                            if (cstr_eq(mname, mlen, "delete", 6u)) {
                                R[dst_reg] = AnvilValue::make_bool(map_delete(heap, heap.objects()[obj_id], arg_at(0u)));
                                break;
                            }
                            if (cstr_eq(mname, mlen, "clear", 5u)) {
                                map_clear(heap, heap.objects()[obj_id]);
                                R[dst_reg] = AnvilValue::make_undefined();
                                break;
                            }
                            if (cstr_eq(mname, mlen, "set", 3u)) {
                                const AnvilValue k = arg_at(0u);
                                const AnvilValue v = arg_at(1u);
                                sync_gc_roots();
                                if (!map_set(heap, obj_id, k, v)) {
                                    reclaim_array_storage(reg_base + cur_frame_regs);
                                    R = &registers()[reg_base];
                                    if (!map_set(heap, obj_id, k, v)) {
                                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                                        return AnvilValue::make_undefined();
                                    }
                                }
                                R = &registers()[reg_base];
                                R[dst_reg] = recv;
                                break;
                            }
                            if (cstr_eq(mname, mlen, "forEach", 7u)) {
                                const AnvilValue cb = arg_at(0u);
                                if (!cb.is_function()) {
                                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                                             "Map.forEach callback is not a function",
                                                             VM_STOP_NOT_FUNCTION)) {
                                        return AnvilValue::make_undefined();
                                    }
                                    break;
                                }
                                const u32 cid = cb.payload();
                                const u32 fn_id = heap.closures()[cid].fn_id;
                                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                                u32 callee_regs = fn.local_count;
                                if (callee_regs < fn.arity) callee_regs = fn.arity;
                                if (callee_regs == 0u) callee_regs = 1u;
                                const u32 arr_id = heap.objects()[obj_id].aux_id;
                                const u32 len = heap.arrays()[arr_id].length;
                                for (u32 idx = 0; idx + 1u < len; idx += 2u) {
                                    const AnvilValue k = heap.get_array_elem(arr_id, static_cast<i32>(idx));
                                    if (k.bits == TOMBSTONE_BITS) continue;
                                    const AnvilValue v = heap.get_array_elem(arr_id, static_cast<i32>(idx + 1u));
                                    const u32 next_base = reg_base + cur_frame_regs;
                                    if (!ensure_registers(next_base + callee_regs) ||
                                        !ensure_frames(frame_count + 1u)) {
                                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                                        return AnvilValue::make_undefined();
                                    }
                                    for (u32 r = 0; r < callee_regs; ++r) {
                                        registers()[next_base + r] = AnvilValue::make_undefined();
                                    }
                                    if (fn.arity >= 1u) registers()[next_base + 0u] = v;
                                    if (fn.arity >= 2u) registers()[next_base + 1u] = k;
                                    if (fn.arity >= 3u) registers()[next_base + 2u] = recv;
                                    run_loop(fn.code_start, next_base, callee_regs, cid, AnvilValue::make_undefined(), frame_count);
                                    if (aborted) return AnvilValue::make_undefined();
                                    R = &registers()[reg_base];
                                }
                                R[dst_reg] = AnvilValue::make_undefined();
                                break;
                            }
                        }
                        if (bkind == BuiltinObjectKind::Set) {
                            if (cstr_eq(mname, mlen, "has", 3u)) {
                                R[dst_reg] = AnvilValue::make_bool(set_has(heap, heap.objects()[obj_id], arg_at(0u)));
                                break;
                            }
                            if (cstr_eq(mname, mlen, "delete", 6u)) {
                                R[dst_reg] = AnvilValue::make_bool(set_delete(heap, heap.objects()[obj_id], arg_at(0u)));
                                break;
                            }
                            if (cstr_eq(mname, mlen, "clear", 5u)) {
                                set_clear(heap, heap.objects()[obj_id]);
                                R[dst_reg] = AnvilValue::make_undefined();
                                break;
                            }
                            if (cstr_eq(mname, mlen, "add", 3u)) {
                                const AnvilValue k = arg_at(0u);
                                sync_gc_roots();
                                if (!set_add(heap, obj_id, k)) {
                                    reclaim_array_storage(reg_base + cur_frame_regs);
                                    R = &registers()[reg_base];
                                    if (!set_add(heap, obj_id, k)) {
                                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                                        return AnvilValue::make_undefined();
                                    }
                                }
                                R = &registers()[reg_base];
                                R[dst_reg] = recv;
                                break;
                            }
                            if (cstr_eq(mname, mlen, "forEach", 7u)) {
                                const AnvilValue cb = arg_at(0u);
                                if (!cb.is_function()) {
                                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                                             "Set.forEach callback is not a function",
                                                             VM_STOP_NOT_FUNCTION)) {
                                        return AnvilValue::make_undefined();
                                    }
                                    break;
                                }
                                const u32 cid = cb.payload();
                                const u32 fn_id = heap.closures()[cid].fn_id;
                                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                                u32 callee_regs = fn.local_count;
                                if (callee_regs < fn.arity) callee_regs = fn.arity;
                                if (callee_regs == 0u) callee_regs = 1u;
                                const u32 arr_id = heap.objects()[obj_id].aux_id;
                                const u32 len = heap.arrays()[arr_id].length;
                                for (u32 idx = 0; idx < len; ++idx) {
                                    const AnvilValue k = heap.get_array_elem(arr_id, static_cast<i32>(idx));
                                    if (k.bits == TOMBSTONE_BITS) continue;
                                    const u32 next_base = reg_base + cur_frame_regs;
                                    if (!ensure_registers(next_base + callee_regs) ||
                                        !ensure_frames(frame_count + 1u)) {
                                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                                        return AnvilValue::make_undefined();
                                    }
                                    for (u32 r = 0; r < callee_regs; ++r) {
                                        registers()[next_base + r] = AnvilValue::make_undefined();
                                    }
                                    if (fn.arity >= 1u) registers()[next_base + 0u] = k;
                                    if (fn.arity >= 2u) registers()[next_base + 1u] = k;
                                    if (fn.arity >= 3u) registers()[next_base + 2u] = recv;
                                    run_loop(fn.code_start, next_base, callee_regs, cid, AnvilValue::make_undefined(), frame_count);
                                    if (aborted) return AnvilValue::make_undefined();
                                    R = &registers()[reg_base];
                                }
                                R[dst_reg] = AnvilValue::make_undefined();
                                break;
                            }
                        }
                    }
                }

                AnvilValue callee = ic.load_property_ic(heap, NO_SLOT, recv, prop_sym);
                if (!callee.is_function() && recv.is_object() && cstr_eq(mname, mlen, "toString", 8u)) {
                    sync_gc_roots();
                    const u32 sym = heap.value_to_string_sym(recv);
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_string(sym);
                    break;
                }
                if (!callee.is_function()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const u32 cid = callee.payload();
                const u32 fn_id = heap.closures()[cid].fn_id;
                if (frame_count + 1u > effective_call_depth()) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", effective_call_depth(), "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) callee_regs = fn.arity;
                if (callee_regs == 0u) callee_regs = 1u;
                const u32 next_base = reg_base + cur_frame_regs;
                if (!ensure_registers(next_base + callee_regs) ||
                    !ensure_frames(frame_count + 1u)) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                AnvilValue* regs = registers();
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = (a_idx < argc)
                        ? regs[reg_base + call_base + a_idx]
                        : AnvilValue::make_undefined();
                }
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, dst_reg, NO_SLOT, cur_this};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = cid;
                cur_this = recv;
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::NewInstance: {
                const u16 dst_reg = inst.a;
                const AnvilValue callee = R[inst.b];
                const u32 call_base = inst.c;
                const u32 argc = inst.imm32();
                if (!callee.is_function()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const u32 cid = callee.payload();
                if (cid >= heap.closure_count || !heap.closures()[cid].active) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                u32 proto_id = heap.closures()[cid].proto_obj_id;
                if (proto_id == NO_SLOT) {
                    sync_gc_roots();
                    proto_id = heap.alloc_object(0u);
                    if (proto_id == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        proto_id = heap.alloc_object(0u);
                    }
                    heap.closures()[cid].proto_obj_id = proto_id;
                }
                sync_gc_roots();
                u32 inst_obj_id = heap.alloc_object(0u);
                if (inst_obj_id == NO_SLOT) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    inst_obj_id = heap.alloc_object(0u);
                }
                if (inst_obj_id == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                if (proto_id != NO_SLOT) {
                    heap.objects()[inst_obj_id].proto_obj_id = proto_id;
                }
                const AnvilValue instance_val = AnvilValue::make_object(inst_obj_id);

                const u32 fn_id = heap.closures()[cid].fn_id;
                if (frame_count + 1u > effective_call_depth()) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", effective_call_depth(), "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) callee_regs = fn.arity;
                if (callee_regs == 0u) callee_regs = 1u;
                const u32 next_base = reg_base + cur_frame_regs;
                if (!ensure_registers(next_base + callee_regs) ||
                    !ensure_frames(frame_count + 1u)) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                AnvilValue* regs = registers();
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = (a_idx < argc)
                        ? regs[reg_base + call_base + a_idx]
                        : AnvilValue::make_undefined();
                }
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, dst_reg, inst_obj_id, cur_this};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = cid;
                cur_this = instance_val;
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::NewBuiltin: {
                const BuiltinCtorKind ckind = static_cast<BuiltinCtorKind>(inst.pad);
                const u16 dst_reg = inst.a;
                const u32 call_base = inst.b;
                const u32 argc = inst.c;
                auto arg_at = [&](u32 idx) noexcept -> AnvilValue {
                    if (idx >= argc) return AnvilValue::make_undefined();
                    return registers()[reg_base + call_base + idx];
                };
                if (ckind >= BuiltinCtorKind::Error && ckind <= BuiltinCtorKind::ReferenceError) {
                    sync_gc_roots();
                    u32 msg_sym = NO_SLOT;
                    if (argc >= 1u && !arg_at(0u).is_undefined()) {
                        msg_sym = heap.value_to_string_sym(arg_at(0u));
                    }
                    u32 err_id = heap.alloc_error_object(ckind, msg_sym);
                    if (err_id == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        err_id = heap.alloc_error_object(ckind, msg_sym);
                    }
                    if (err_id == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_object(err_id);
                    break;
                }
                if (ckind == BuiltinCtorKind::Map) {
                    sync_gc_roots();
                    u32 obj_id = heap.alloc_object(0u);
                    u32 arr_id = heap.alloc_array(8u);
                    if (obj_id == NO_SLOT || arr_id == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        if (obj_id == NO_SLOT) obj_id = heap.alloc_object(0u);
                        if (arr_id == NO_SLOT) arr_id = heap.alloc_array(8u);
                    }
                    if (obj_id == NO_SLOT || arr_id == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    AnvilHeapObject& obj = heap.objects()[obj_id];
                    obj.builtin_kind = static_cast<u8>(BuiltinObjectKind::Map);
                    obj.aux_id = arr_id;
                    obj.byte_offset = 0u;
                    obj.byte_length = 0u;
                    if (argc >= 1u && arg_at(0u).is_array()) {
                        const u32 init_arr = arg_at(0u).payload();
                        const u32 init_len = heap.arrays()[init_arr].length;
                        for (u32 i = 0; i < init_len; ++i) {
                            const AnvilValue pair = heap.get_array_elem(init_arr, static_cast<i32>(i));
                            if (pair.is_array() && heap.arrays()[pair.payload()].length >= 2u) {
                                const AnvilValue k = heap.get_array_elem(pair.payload(), 0);
                                const AnvilValue v = heap.get_array_elem(pair.payload(), 1);
                                map_set(heap, obj_id, k, v);
                            }
                        }
                    }
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_object(obj_id);
                    break;
                }
                if (ckind == BuiltinCtorKind::Set) {
                    sync_gc_roots();
                    u32 obj_id = heap.alloc_object(0u);
                    u32 arr_id = heap.alloc_array(8u);
                    if (obj_id == NO_SLOT || arr_id == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        if (obj_id == NO_SLOT) obj_id = heap.alloc_object(0u);
                        if (arr_id == NO_SLOT) arr_id = heap.alloc_array(8u);
                    }
                    if (obj_id == NO_SLOT || arr_id == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    AnvilHeapObject& obj = heap.objects()[obj_id];
                    obj.builtin_kind = static_cast<u8>(BuiltinObjectKind::Set);
                    obj.aux_id = arr_id;
                    obj.byte_offset = 0u;
                    obj.byte_length = 0u;
                    if (argc >= 1u && arg_at(0u).is_array()) {
                        const u32 init_arr = arg_at(0u).payload();
                        const u32 init_len = heap.arrays()[init_arr].length;
                        for (u32 i = 0; i < init_len; ++i) {
                            set_add(heap, obj_id, heap.get_array_elem(init_arr, static_cast<i32>(i)));
                        }
                    }
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_object(obj_id);
                    break;
                }
                if (ckind == BuiltinCtorKind::ArrayBuffer) {
                    const i32 blen_i = (argc >= 1u && arg_at(0u).is_number())
                        ? js_to_int32(arg_at(0u).as_number())
                        : 0;
                    if (blen_i < 0) {
                        if (!raise_builtin_error(BuiltinCtorKind::RangeError,
                                                 "Invalid ArrayBuffer length",
                                                 VM_STOP_UNSUPPORTED)) {
                            return AnvilValue::make_undefined();
                        }
                        break;
                    }
                    const u32 blen = static_cast<u32>(blen_i);
                    const u32 slots = (blen + 7u) / 8u;
                    sync_gc_roots();
                    u32 obj_id = heap.alloc_object(0u);
                    u32 arr_id = heap.alloc_array(slots > 0u ? slots : 1u);
                    if (obj_id == NO_SLOT || arr_id == NO_SLOT) {
                        reclaim_array_storage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        if (obj_id == NO_SLOT) obj_id = heap.alloc_object(0u);
                        if (arr_id == NO_SLOT) arr_id = heap.alloc_array(slots > 0u ? slots : 1u);
                    }
                    if (obj_id == NO_SLOT || arr_id == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    AnvilValue* store = heap.array_elems(heap.arrays()[arr_id].storage_off);
                    for (u32 i = 0; i < (slots > 0u ? slots : 1u); ++i) {
                        store[i] = AnvilValue(0ULL);
                    }
                    AnvilHeapObject& obj = heap.objects()[obj_id];
                    obj.builtin_kind = static_cast<u8>(BuiltinObjectKind::ArrayBuffer);
                    obj.aux_id = arr_id;
                    obj.byte_offset = 0u;
                    obj.byte_length = blen;
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_object(obj_id);
                    break;
                }
                if (ckind >= BuiltinCtorKind::Uint8Array && ckind <= BuiltinCtorKind::Float64Array) {
                    const u8 tkind = static_cast<u8>(BuiltinObjectKind::Uint8Array) +
                                     (static_cast<u8>(ckind) - static_cast<u8>(BuiltinCtorKind::Uint8Array));
                    const u32 shift = typed_array_shift(tkind);
                    const u32 bpe = 1u << shift;
                    const AnvilValue first = arg_at(0u);
                    sync_gc_roots();
                    if (first.is_object() &&
                        static_cast<BuiltinObjectKind>(heap.objects()[first.payload()].builtin_kind) == BuiltinObjectKind::ArrayBuffer) {
                        const u32 buf_obj_id = first.payload();
                        const u32 buf_bytes = heap.objects()[buf_obj_id].byte_length;
                        const u32 byte_off = (argc >= 2u && arg_at(1u).is_number())
                            ? static_cast<u32>(js_to_int32(arg_at(1u).as_number()))
                            : 0u;
                        const u32 view_bytes = (argc >= 3u && arg_at(2u).is_number())
                            ? (static_cast<u32>(js_to_int32(arg_at(2u).as_number())) << shift)
                            : ((buf_bytes >= byte_off) ? ((buf_bytes - byte_off) & ~(bpe - 1u)) : 0u);
                        u32 view_id = heap.alloc_object(0u);
                        if (view_id == NO_SLOT) {
                            collect_garbage(reg_base + cur_frame_regs);
                            R = &registers()[reg_base];
                            view_id = heap.alloc_object(0u);
                        }
                        if (view_id == NO_SLOT) {
                            fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                            return AnvilValue::make_undefined();
                        }
                        AnvilHeapObject& view = heap.objects()[view_id];
                        view.builtin_kind = tkind;
                        view.aux_id = buf_obj_id;
                        view.byte_offset = byte_off;
                        view.byte_length = view_bytes;
                        R = &registers()[reg_base];
                        R[dst_reg] = AnvilValue::make_object(view_id);
                        break;
                    }
                    u32 elem_len = 0u;
                    bool from_array = false;
                    u32 src_arr_id = NO_SLOT;
                    if (first.is_array()) {
                        from_array = true;
                        src_arr_id = first.payload();
                        elem_len = heap.arrays()[src_arr_id].length;
                    } else if (first.is_number()) {
                        const i32 n = js_to_int32(first.as_number());
                        elem_len = (n > 0) ? static_cast<u32>(n) : 0u;
                    }
                    const u32 blen = elem_len << shift;
                    const u32 slots = (blen + 7u) / 8u;
                    u32 buf_obj_id = heap.alloc_object(0u);
                    u32 byte_arr_id = heap.alloc_array(slots > 0u ? slots : 1u);
                    u32 view_id = heap.alloc_object(0u);
                    if (buf_obj_id == NO_SLOT || byte_arr_id == NO_SLOT || view_id == NO_SLOT) {
                        reclaim_array_storage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        if (buf_obj_id == NO_SLOT) buf_obj_id = heap.alloc_object(0u);
                        if (byte_arr_id == NO_SLOT) byte_arr_id = heap.alloc_array(slots > 0u ? slots : 1u);
                        if (view_id == NO_SLOT) view_id = heap.alloc_object(0u);
                    }
                    if (buf_obj_id == NO_SLOT || byte_arr_id == NO_SLOT || view_id == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    AnvilValue* store = heap.array_elems(heap.arrays()[byte_arr_id].storage_off);
                    for (u32 i = 0; i < (slots > 0u ? slots : 1u); ++i) {
                        store[i] = AnvilValue(0ULL);
                    }
                    AnvilHeapObject& bobj = heap.objects()[buf_obj_id];
                    bobj.builtin_kind = static_cast<u8>(BuiltinObjectKind::ArrayBuffer);
                    bobj.aux_id = byte_arr_id;
                    bobj.byte_offset = 0u;
                    bobj.byte_length = blen;
                    AnvilHeapObject& view = heap.objects()[view_id];
                    view.builtin_kind = tkind;
                    view.aux_id = buf_obj_id;
                    view.byte_offset = 0u;
                    view.byte_length = blen;
                    if (from_array) {
                        for (u32 i = 0; i < elem_len; ++i) {
                            typed_array_set_elem(heap, view, static_cast<i32>(i),
                                                 heap.get_array_elem(src_arr_id, static_cast<i32>(i)));
                        }
                    }
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_object(view_id);
                    break;
                }
                if (ckind == BuiltinCtorKind::Array) {
                    const u32 cap = (argc == 1u && arg_at(0u).is_number())
                        ? static_cast<u32>(js_to_int32(arg_at(0u).as_number()) > 0 ? js_to_int32(arg_at(0u).as_number()) : 0)
                        : argc;
                    sync_gc_roots();
                    u32 arr_id = heap.alloc_array(cap > 0u ? cap : 1u);
                    if (arr_id == NO_SLOT) {
                        reclaim_array_storage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        arr_id = heap.alloc_array(cap > 0u ? cap : 1u);
                    }
                    if (arr_id == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    if (argc == 1u && arg_at(0u).is_number()) {
                        for (u32 i = 0; i < cap; ++i) {
                            heap.push_array_elem(arr_id, AnvilValue::make_undefined());
                        }
                    } else {
                        for (u32 i = 0; i < argc; ++i) {
                            heap.push_array_elem(arr_id, arg_at(i));
                        }
                    }
                    R = &registers()[reg_base];
                    R[dst_reg] = AnvilValue::make_array(arr_id);
                    break;
                }
                sync_gc_roots();
                u32 obj_id = heap.alloc_object(0u);
                if (obj_id == NO_SLOT) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    obj_id = heap.alloc_object(0u);
                }
                if (obj_id == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R = &registers()[reg_base];
                R[dst_reg] = AnvilValue::make_object(obj_id);
                break;
            }

            case AnvilOpcode::SetupClass: {
                const AnvilValue ctor_val = R[inst.a];
                const AnvilValue super_val = R[inst.b];
                if (!ctor_val.is_function()) break;
                const u32 cid = ctor_val.payload();
                sync_gc_roots();
                u32 proto_id = heap.closures()[cid].proto_obj_id;
                if (proto_id == NO_SLOT) {
                    proto_id = heap.alloc_object(0u);
                    if (proto_id == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        proto_id = heap.alloc_object(0u);
                    }
                    heap.closures()[cid].proto_obj_id = proto_id;
                }
                if (super_val.is_function()) {
                    const u32 super_cid = super_val.payload();
                    heap.closures()[cid].super_closure_id = super_cid;
                    u32 super_proto = heap.closures()[super_cid].proto_obj_id;
                    if (super_proto == NO_SLOT) {
                        super_proto = heap.alloc_object(0u);
                        heap.closures()[super_cid].proto_obj_id = super_proto;
                    }
                    if (proto_id != NO_SLOT && super_proto != NO_SLOT) {
                        heap.objects()[proto_id].proto_obj_id = super_proto;
                    }
                }
                break;
            }

            case AnvilOpcode::DefineMethod: {
                const bool is_static = inst.pad != 0u;
                const AnvilValue ctor_val = R[inst.a];
                const AnvilValue fn_val = R[inst.b];
                const u32 prop_sym = inst.imm32();
                if (!ctor_val.is_function()) break;
                const u32 cid = ctor_val.payload();
                sync_gc_roots();
                u32 target_obj = NO_SLOT;
                if (is_static) {
                    target_obj = heap.closures()[cid].static_obj_id;
                    if (target_obj == NO_SLOT) {
                        target_obj = heap.alloc_object(0u);
                        if (target_obj == NO_SLOT) {
                            collect_garbage(reg_base + cur_frame_regs);
                            R = &registers()[reg_base];
                            target_obj = heap.alloc_object(0u);
                        }
                        heap.closures()[cid].static_obj_id = target_obj;
                    }
                } else {
                    target_obj = heap.closures()[cid].proto_obj_id;
                    if (target_obj == NO_SLOT) {
                        target_obj = heap.alloc_object(0u);
                        if (target_obj == NO_SLOT) {
                            collect_garbage(reg_base + cur_frame_regs);
                            R = &registers()[reg_base];
                            target_obj = heap.alloc_object(0u);
                        }
                        heap.closures()[cid].proto_obj_id = target_obj;
                    }
                    if (fn_val.is_function()) {
                        const u32 m_cid = fn_val.payload();
                        const u32 m_fn = heap.closures()[m_cid].fn_id;
                        if (m_fn < heap.function_count) {
                            heap.functions()[m_fn].home_proto_id = target_obj;
                        }
                    }
                }
                if (target_obj != NO_SLOT) {
                    StoreResult stored = heap.set_object_prop(target_obj, prop_sym, fn_val, nullptr, nullptr);
                    if (stored == StoreResult::HeapFull) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        heap.set_object_prop(target_obj, prop_sym, fn_val, nullptr, nullptr);
                    }
                }
                break;
            }

            case AnvilOpcode::CallSuperCtor: {
                const u16 dst_reg = inst.a;
                const u32 call_base = inst.b;
                const u32 argc = inst.c;
                if (cur_closure == NO_SLOT || cur_closure >= heap.closure_count) {
                    R[dst_reg] = cur_this;
                    break;
                }
                const u32 super_cid = heap.closures()[cur_closure].super_closure_id;
                if (super_cid == NO_SLOT || super_cid >= heap.closure_count || !heap.closures()[super_cid].active) {
                    R[dst_reg] = cur_this;
                    break;
                }
                const u32 fn_id = heap.closures()[super_cid].fn_id;
                if (frame_count + 1u > effective_call_depth()) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", effective_call_depth(), "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) callee_regs = fn.arity;
                if (callee_regs == 0u) callee_regs = 1u;
                const u32 next_base = reg_base + cur_frame_regs;
                if (!ensure_registers(next_base + callee_regs) ||
                    !ensure_frames(frame_count + 1u)) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                AnvilValue* regs = registers();
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = (a_idx < argc)
                        ? regs[reg_base + call_base + a_idx]
                        : AnvilValue::make_undefined();
                }
                const u32 inst_obj_id = cur_this.is_object() ? cur_this.payload() : NO_SLOT;
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, dst_reg, inst_obj_id, cur_this};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = super_cid;
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::CallSuperMethod: {
                const u32 argc = inst.pad;
                const u16 dst_reg = inst.a;
                const u32 call_base = inst.b;
                const u32 prop_sym = inst.imm32();
                AnvilValue callee = AnvilValue::make_undefined();
                u32 start_proto = NO_SLOT;
                if (cur_closure != NO_SLOT && cur_closure < heap.closure_count) {
                    const u32 cur_fn_id = heap.closures()[cur_closure].fn_id;
                    if (cur_fn_id < heap.function_count) {
                        const u32 home = heap.functions()[cur_fn_id].home_proto_id;
                        if (home != NO_SLOT && home < heap.object_count) {
                            start_proto = heap.objects()[home].proto_obj_id;
                        }
                    }
                }
                if (start_proto == NO_SLOT && cur_this.is_object()) {
                    const u32 inst_obj = cur_this.payload();
                    const u32 proto_obj = heap.objects()[inst_obj].proto_obj_id;
                    if (proto_obj != NO_SLOT && proto_obj < heap.object_count) {
                        start_proto = heap.objects()[proto_obj].proto_obj_id;
                    }
                }
                if (start_proto != NO_SLOT && start_proto < heap.object_count) {
                    callee = heap.get_object_prop(start_proto, prop_sym, nullptr, nullptr);
                }
                if (!callee.is_function()) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "called value is not a function",
                                             VM_STOP_NOT_FUNCTION)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const u32 cid = callee.payload();
                const u32 fn_id = heap.closures()[cid].fn_id;
                if (frame_count + 1u > effective_call_depth()) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", effective_call_depth(), "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions()[fn_id];
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) callee_regs = fn.arity;
                if (callee_regs == 0u) callee_regs = 1u;
                const u32 next_base = reg_base + cur_frame_regs;
                if (!ensure_registers(next_base + callee_regs) ||
                    !ensure_frames(frame_count + 1u)) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                AnvilValue* regs = registers();
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = (a_idx < argc)
                        ? regs[reg_base + call_base + a_idx]
                        : AnvilValue::make_undefined();
                }
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, dst_reg, NO_SLOT, cur_this};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = cid;
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::EnterTry: {
                if (try_depth >= MAX_TRY_DEPTH) {
                    fail(VM_STOP_UNSUPPORTED, "try nesting limit exceeded");
                    return AnvilValue::make_undefined();
                }
                AnvilTryHandler& h = try_stack[try_depth++];
                h.frame_depth = frame_count;
                h.reg_base = reg_base;
                h.cur_frame_regs = cur_frame_regs;
                h.cur_closure = cur_closure;
                h.cur_this = cur_this;
                h.catch_pc = inst.imm32();
                h.finally_pc = 0u;
                h.catch_reg = inst.a;
                h.completion_kind = TryCompletionKind::Normal;
                h.completion_val = AnvilValue::make_undefined();
                h.completion_pc = 0u;
                break;
            }

            case AnvilOpcode::SetTryFinally: {
                if (try_depth > 0u) {
                    try_stack[try_depth - 1u].finally_pc = inst.imm32();
                }
                break;
            }

            case AnvilOpcode::LeaveTry: {
                if (try_depth > 0u) {
                    if (try_stack[try_depth - 1u].finally_pc == 0u) {
                        --try_depth;
                    } else {
                        try_stack[try_depth - 1u].catch_pc = 0u;
                    }
                }
                break;
            }

            case AnvilOpcode::EndFinally: {
                if (try_depth > 0u && try_stack[try_depth - 1u].completion_kind != TryCompletionKind::Normal) {
                    const AnvilTryHandler pending = try_stack[--try_depth];
                    if (pending.completion_kind == TryCompletionKind::Throw) {
                        if (!unwind_throw(pending.completion_val)) {
                            return AnvilValue::make_undefined();
                        }
                        break;
                    }
                    if (pending.completion_kind == TryCompletionKind::Return) {
                        const AnvilValue ret_val = pending.completion_val;
                        if (frame_count <= base_frame_depth) {
                            return ret_val;
                        }
                        const AnvilCallFrame caller = frames()[--frame_count];
                        reg_base = caller.reg_base;
                        cur_frame_regs = caller.caller_frame_regs;
                        cur_closure = caller.closure_id;
                        cur_this = caller.caller_this;
                        AnvilValue out_val = ret_val;
                        if (caller.new_instance_obj != NO_SLOT &&
                            !ret_val.is_object() && !ret_val.is_array() && !ret_val.is_function()) {
                            out_val = AnvilValue::make_object(caller.new_instance_obj);
                        }
                        registers()[reg_base + caller.dst_reg] = out_val;
                        pc = caller.return_pc;
                        break;
                    }
                } else if (try_depth > 0u) {
                    --try_depth;
                }
                break;
            }

            case AnvilOpcode::Throw: {
                const AnvilValue exc = R[inst.a];
                if (!unwind_throw(exc)) {
                    return AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::IterLen: {
                const AnvilValue iter = R[inst.b];
                u32 len = 0u;
                if (iter.is_array()) {
                    len = heap.arrays()[iter.payload()].length;
                } else if (iter.is_string()) {
                    u32 blen = 0u;
                    const char* ptr = heap.get_string_ptr(iter.payload(), &blen);
                    len = utf16_len(ptr, blen);
                } else if (iter.is_object()) {
                    const AnvilHeapObject& obj = heap.objects()[iter.payload()];
                    const BuiltinObjectKind bkind = static_cast<BuiltinObjectKind>(obj.builtin_kind);
                    if (bkind == BuiltinObjectKind::Set) {
                        len = obj.byte_length;
                    } else if (bkind >= BuiltinObjectKind::Uint8Array &&
                               bkind <= BuiltinObjectKind::Float64Array) {
                        len = obj.byte_length >> typed_array_shift(obj.builtin_kind);
                    }
                }
                R[inst.a] = AnvilValue::make_smi(static_cast<i32>(len));
                break;
            }

            case AnvilOpcode::IterGet: {
                const AnvilValue iter = R[inst.b];
                const i32 idx = R[inst.c].is_number() ? js_to_int32(R[inst.c].as_number()) : 0;
                if (iter.is_array()) {
                    R[inst.a] = heap.get_array_elem(iter.payload(), idx);
                    break;
                }
                if (iter.is_string()) {
                    u32 blen = 0u;
                    const char* ptr = heap.get_string_ptr(iter.payload(), &blen);
                    u16 unit = 0u;
                    if (idx < 0 || !utf16_at(ptr, blen, static_cast<u32>(idx), &unit)) {
                        R[inst.a] = AnvilValue::make_undefined();
                        break;
                    }
                    char ubuf[3];
                    const u32 un = encode_unit_utf8(unit, ubuf);
                    sync_gc_roots();
                    u32 csym = heap.intern_string(ubuf, un);
                    if (csym == NO_SLOT) {
                        collect_garbage(reg_base + cur_frame_regs);
                        R = &registers()[reg_base];
                        csym = heap.intern_string(ubuf, un);
                    }
                    R = &registers()[reg_base];
                    R[inst.a] = (csym != NO_SLOT) ? AnvilValue::make_string(csym) : AnvilValue::make_undefined();
                    break;
                }
                if (iter.is_object()) {
                    const AnvilHeapObject& obj = heap.objects()[iter.payload()];
                    const BuiltinObjectKind bkind = static_cast<BuiltinObjectKind>(obj.builtin_kind);
                    if (bkind == BuiltinObjectKind::Set) {
                        const u32 arr_id = obj.aux_id;
                        const u32 alen = heap.arrays()[arr_id].length;
                        i32 live = 0;
                        AnvilValue found = AnvilValue::make_undefined();
                        for (u32 k = 0; k < alen; ++k) {
                            const AnvilValue v = heap.get_array_elem(arr_id, static_cast<i32>(k));
                            if (v.bits == TOMBSTONE_BITS) continue;
                            if (live == idx) {
                                found = v;
                                break;
                            }
                            ++live;
                        }
                        R[inst.a] = found;
                        break;
                    }
                    if (bkind >= BuiltinObjectKind::Uint8Array &&
                        bkind <= BuiltinObjectKind::Float64Array) {
                        R[inst.a] = typed_array_get_elem(heap, obj, idx);
                        break;
                    }
                }
                R[inst.a] = AnvilValue::make_undefined();
                break;
            }

            case AnvilOpcode::CallMath: {
                if (!numeric_operand(R[inst.c])) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "arithmetic or comparison on a string or object",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const f64 v = R[inst.c].as_number();
                f64 out = v;
                switch (inst.b) {
                    case 1u: out = js_abs(v); break;
                    case 2u: out = js_floor(v); break;
                    case 3u: out = js_ceil(v); break;
                    case 4u: out = js_round(v); break;
                    case 5u: out = js_sqrt(v); break;
                    case 6u: out = js_trunc(v); break;
                    case 7u: out = js_sign(v); break;
                    default: break;
                }
                R[inst.a] = AnvilValue::make_number(out);
                break;
            }

            case AnvilOpcode::CallMath2: {
                if (!numeric_operand(R[inst.b]) || !numeric_operand(R[inst.c])) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "arithmetic or comparison on a string or object",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const f64 a = R[inst.b].as_number();
                const f64 b = R[inst.c].as_number();
                switch (inst.imm32()) {
                    case 1u: R[inst.a] = AnvilValue::make_number(js_pow(a, b)); break;
                    case 2u: R[inst.a] = AnvilValue::make_number(static_cast<f64>(js_imul(a, b))); break;
                    case 3u: R[inst.a] = AnvilValue::make_number(js_min2(a, b)); break;
                    case 4u: R[inst.a] = AnvilValue::make_number(js_max2(a, b)); break;
                    default: R[inst.a] = AnvilValue::make_undefined(); break;
                }
                break;
            }

            case AnvilOpcode::CallPred: {
                const AnvilValue av = R[inst.c];
                if ((inst.b == 4u || inst.b == 5u) && !numeric_operand(av)) {
                    if (!raise_builtin_error(BuiltinCtorKind::TypeError,
                                             "global isNaN/isFinite on a non-numeric value",
                                             VM_STOP_UNSUPPORTED)) {
                        return AnvilValue::make_undefined();
                    }
                    break;
                }
                const f64 v = av.as_number();
                bool out = false;
                switch (inst.b) {
                    case 1u: out = av.is_number() && js_is_finite(v) && js_trunc(v) == v; break;
                    case 2u: out = av.is_number() && js_is_nan(v); break;
                    case 3u: out = av.is_number() && js_is_finite(v); break;
                    case 4u: out = js_is_nan(v); break;
                    case 5u: out = js_is_finite(v); break;
                    case 6u: out = av.is_array(); break;
                    default: break;
                }
                R[inst.a] = AnvilValue::make_bool(out);
                break;
            }

            case AnvilOpcode::NewCell: {
                sync_gc_roots();
                u32 cell = heap.alloc_cell(R[inst.b]);
                if (cell == NO_SLOT) {
                    reclaim_array_storage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    cell = heap.alloc_cell(R[inst.b]);
                }
                if (cell == NO_SLOT) {
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_array(cell);
                break;
            }

            case AnvilOpcode::CellGet: {
                R[inst.a] = R[inst.b].is_array()
                    ? heap.get_array_elem(R[inst.b].payload(), 0)
                    : AnvilValue::make_undefined();
                break;
            }

            case AnvilOpcode::CellSet: {
                if (R[inst.a].is_array()) {
                    heap.set_array_elem(R[inst.a].payload(), 0, R[inst.b]);
                }
                break;
            }

            case AnvilOpcode::GetUpval: {
                const u32 idx = inst.b;
                if (cur_closure < heap.closure_count &&
                    idx < heap.closures()[cur_closure].upval_count) {
                    const u32 cell =
                        heap.closure_upvals(heap.closures()[cur_closure].upvals_off)[idx];
                    R[inst.a] = heap.get_array_elem(cell, 0);
                } else {
                    R[inst.a] = AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::GetUpvalCell: {
                const u32 idx = inst.b;
                if (cur_closure < heap.closure_count &&
                    idx < heap.closures()[cur_closure].upval_count) {
                    const u32 cell =
                        heap.closure_upvals(heap.closures()[cur_closure].upvals_off)[idx];
                    R[inst.a] = AnvilValue::make_array(cell);
                } else {
                    R[inst.a] = AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::SetUpval: {
                const u32 idx = inst.a;
                if (cur_closure < heap.closure_count &&
                    idx < heap.closures()[cur_closure].upval_count) {
                    const u32 cell =
                        heap.closure_upvals(heap.closures()[cur_closure].upvals_off)[idx];
                    heap.set_array_elem(cell, 0, R[inst.b]);
                }
                break;
            }

            case AnvilOpcode::MakeClosure: {
                const u32 fn_id = inst.imm32();
                const u32 base = inst.b;
                const u32 n = inst.c;
                u32 upvals_off = NO_SLOT;
                if (n > 0u) {
                    bool over = false;
                    upvals_off = heap.arena.alloc(n * static_cast<u32>(sizeof(u32)), &over);
                    if (upvals_off == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    u32* up = heap.closure_upvals(upvals_off);
                    for (u32 i = 0u; i < n; ++i) {
                        up[i] = R[base + i].payload();
                    }
                }
                sync_gc_roots();
                u32 cid = heap.alloc_closure(fn_id, upvals_off, n, false);
                if (cid == NO_SLOT) {
                    collect_garbage(reg_base + cur_frame_regs);
                    R = &registers()[reg_base];
                    cid = heap.alloc_closure(fn_id, upvals_off, n, false);
                }
                if (cid == NO_SLOT) {
                    if (upvals_off != NO_SLOT) {
                        heap.arena.free(upvals_off, n * static_cast<u32>(sizeof(u32)));
                    }
                    fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_function(cid);
                break;
            }

            case AnvilOpcode::LoopHeader: {
                const u32 lid = inst.a & 63u;
                ++program.loop_trip_counts[lid];
                if (program.loop_trip_counts[lid] == HOT_LOOP_THRESHOLD) {
                    ++hot_loops_accelerated;
                }
                break;
            }

            case AnvilOpcode::Return: {
                const AnvilValue ret_val = R[inst.a];
                while (try_depth > 0u && try_stack[try_depth - 1u].frame_depth == frame_count) {
                    AnvilTryHandler& h = try_stack[try_depth - 1u];
                    if (h.finally_pc != 0u) {
                        const u32 target_pc = h.finally_pc;
                        h.finally_pc = 0u;
                        h.catch_pc = 0u;
                        h.completion_kind = TryCompletionKind::Return;
                        h.completion_val = ret_val;
                        pc = target_pc;
                        goto next_instruction;
                    }
                    --try_depth;
                }
                if (frame_count <= base_frame_depth) {
                    return ret_val;
                }
                {
                    const AnvilCallFrame caller = frames()[--frame_count];
                    reg_base = caller.reg_base;
                    cur_frame_regs = caller.caller_frame_regs;
                    cur_closure = caller.closure_id;
                    cur_this = caller.caller_this;
                    pc = caller.return_pc;
                    AnvilValue out_val = ret_val;
                    if (caller.new_instance_obj != NO_SLOT &&
                        !ret_val.is_object() && !ret_val.is_array() && !ret_val.is_function()) {
                        out_val = AnvilValue::make_object(caller.new_instance_obj);
                    }
                    registers()[reg_base + caller.dst_reg] = out_val;
                }
                next_instruction:
                break;
            }

            case AnvilOpcode::Halt:
                return R[inst.a];

            case AnvilOpcode::HostSetText:
                break;
        }
    }
    return registers()[0];
}

void AnvilVmContext::fill_telemetry(AnvilVmTelemetryC* out) const noexcept {
    if (!out) {
        return;
    }
    out->kernel_version = ANVIL_VM_KERNEL_VERSION;
    out->bytecode_ops_executed = steps_executed;
    out->heap_objects_live = heap.live_objects + heap.live_arrays;
    out->heap_bytes_allocated = heap.bytes_allocated;
    out->gc_collections = heap.gc_collections;
    out->shapes_allocated = heap.shape_count;
    out->ic_hits = ic.ic_hits + ic.dom_ic_hits;
    out->ic_misses = ic.ic_misses + ic.dom_ic_misses;
    out->polymorphic_ic_sites = ic.polymorphic_upgrades;
    out->hot_loops_accelerated = hot_loops_accelerated;
    out->native_compilations = native_compilations;
    out->stop_code = error.code;
    const u32 n = error.len < MAX_ERROR_TEXT ? error.len : MAX_ERROR_TEXT;
    out->stop_len = n;
    for (u32 i = 0u; i < MAX_ERROR_TEXT; ++i) {
        out->stop_text[i] = i < n ? error.text[i] : '\0';
    }
    if (paused && pending_host_call) {
        fill_host_call(out);
    } else {
        fill_exports(out);
    }
}

namespace {

struct ExportWriter {
    u8* bytes;
    u32 cap;
    u32 len;
    bool full;

    bool room(u32 n) noexcept {
        if (full || n > cap - len) {
            full = true;
            return false;
        }
        return true;
    }
    void put_u8(u8 v) noexcept {
        if (room(1u)) {
            bytes[len++] = v;
        }
    }
    void put_u32(u32 v) noexcept {
        if (room(4u)) {
            for (u32 i = 0u; i < 4u; ++i) {
                bytes[len++] = static_cast<u8>(v >> (8u * i));
            }
        }
    }
    void put_f64(f64 v) noexcept {
        const u64 bits = __builtin_bit_cast(u64, v);
        if (room(8u)) {
            for (u32 i = 0u; i < 8u; ++i) {
                bytes[len++] = static_cast<u8>(bits >> (8u * i));
            }
        }
    }
    void put_text(const char* text, u32 n) noexcept {
        if (!text) {
            n = 0u;
        }
        if (room(n)) {
            for (u32 i = 0u; i < n; ++i) {
                bytes[len++] = static_cast<u8>(text[i]);
            }
        }
    }
};

} // namespace

void AnvilVmContext::fill_exports(AnvilVmTelemetryC* out) const noexcept {
    ExportWriter w{out->exports, MAX_EXPORT_BYTES, 0u, false};
    const u32 count = global_count;
    for (u32 index = 0u; index < count && !w.full; ++index) {
        const AnvilGlobalBinding& g = globals()[index];
        if (!g.occupied || !g.exported) {
            continue;
        }
        const AnvilValue v = g.value;
        u8 kind = 6u;
        if (v.is_bool()) {
            kind = 2u;
        } else if (v.is_number()) {
            kind = 1u;
        } else if (v.is_null()) {
            kind = 3u;
        } else if (v.is_undefined()) {
            kind = 4u;
        } else if (v.is_string()) {
            kind = 5u;
        }
        u32 name_len = 0u;
        const char* name = heap.get_string_ptr(g.name_sym, &name_len);
        w.put_u8(kind);
        w.put_u32(name ? name_len : 0u);
        w.put_text(name, name_len);
        if (kind == 1u || kind == 2u) {
            w.put_f64(v.as_number());
        } else if (kind == 5u) {
            u32 text_len = 0u;
            const char* text = heap.get_string_ptr(v.payload(), &text_len);
            w.put_u32(text ? text_len : 0u);
            w.put_text(text, text_len);
        }
    }
    out->exports_full = w.full ? 1u : 0u;
    out->exports_len = w.full ? 0u : w.len;
}

void AnvilVmContext::fill_host_call(AnvilVmTelemetryC* out) const noexcept {
    ExportWriter w{out->exports, MAX_EXPORT_BYTES, 0u, false};
    w.put_u32(pending_host_fn);
    w.put_u32(pending_arg_count);
    w.put_u32(pending_host_overflow);
    for (u32 i = 0u; i < pending_arg_count && !w.full; ++i) {
        const AnvilValue v = pending_args[i];
        u8 kind = 6u;
        if (v.is_bool()) {
            kind = 2u;
        } else if (v.is_number()) {
            kind = 1u;
        } else if (v.is_null()) {
            kind = 3u;
        } else if (v.is_undefined()) {
            kind = 4u;
        } else if (v.is_string()) {
            kind = 5u;
        }
        w.put_u8(kind);
        if (kind == 1u || kind == 2u) {
            w.put_f64(v.as_number());
        } else if (kind == 5u) {
            u32 text_len = 0u;
            const char* text = heap.get_string_ptr(v.payload(), &text_len);
            w.put_u32(text ? text_len : 0u);
            w.put_text(text, text_len);
        } else if (kind == 6u) {
            const u32 sym = const_cast<AnvilHeap&>(heap).value_to_string_sym(v);
            if (sym != NO_SLOT) {
                w.bytes[w.len - 1u] = 5u;
                u32 text_len = 0u;
                const char* text = heap.get_string_ptr(sym, &text_len);
                w.put_u32(text ? text_len : 0u);
                w.put_text(text, text_len);
            }
        }
    }
    out->exports_full = w.full ? 1u : 0u;
    out->exports_len = w.len;
}

} // namespace anvil_vm

extern "C" {

u32 anvil_cpp_jit_kernel_version() noexcept {
    return anvil_vm::ANVIL_VM_KERNEL_VERSION;
}

f64 anvil_cpp_jit_eval_binary(u32 opcode, f64 lhs, f64 rhs) noexcept {
    switch (opcode) {
        case 0u:
            return lhs + rhs;
        case 1u:
            return lhs - rhs;
        case 2u:
            return lhs * rhs;
        case 3u:
            return rhs == 0.0 ? 0.0 : (lhs / rhs);
        case 4u: {
            const anvil_vm::i64 r_int = anvil_vm::sat_i64(rhs);
            if (r_int == 0) {
                return 0.0;
            }
            const anvil_vm::i64 l_int = anvil_vm::sat_i64(lhs);
            if (r_int == -1) {
                return 0.0;
            }
            return static_cast<f64>(l_int % r_int);
        }
        case 5u:
            return static_cast<f64>(anvil_vm::sat_i32(lhs) & anvil_vm::sat_i32(rhs));
        case 6u:
            return static_cast<f64>(anvil_vm::sat_i32(lhs) | anvil_vm::sat_i32(rhs));
        case 7u:
            return static_cast<f64>(anvil_vm::sat_i32(lhs) ^ anvil_vm::sat_i32(rhs));
        case 8u:
            return static_cast<f64>(static_cast<anvil_vm::i32>(static_cast<anvil_vm::u32>(anvil_vm::sat_i32(lhs)) << (anvil_vm::sat_i32(rhs) & 31)));
        case 9u:
            return static_cast<f64>(anvil_vm::sat_i32(lhs) >> (anvil_vm::sat_i32(rhs) & 31));
        default:
            return 0.0;
    }
}

anvil_vm::i64 anvil_cpp_jit_sum_range_i64(anvil_vm::i64 start, anvil_vm::i64 end_exclusive, anvil_vm::i64 step) noexcept {
    if (step <= 0 || start >= end_exclusive) {
        return 0;
    }
    const anvil_vm::i64 span = end_exclusive - start;
    anvil_vm::i64 count = (span + step - 1) / step;
    if (count > 100000) {
        count = 100000;
    }
    const anvil_vm::i64 last = start + (count - 1) * step;
    return count * (start + last) / 2;
}

extern "C" anvil_vm::u32 anvil_cpp_vm_context_bytes() {
    return static_cast<anvil_vm::u32>(sizeof(anvil_vm::AnvilVmContext)) + anvil_vm::ANVIL_ARENA_RESERVE_BYTES;
}

extern "C" anvil_vm::u32 anvil_cpp_vm_context_align() {
    return static_cast<anvil_vm::u32>(alignof(anvil_vm::AnvilVmContext));
}

namespace {
void (*g_text_host_fn)(void*, const char*, anvil_vm::u32, double) = nullptr;
void* g_text_host = nullptr;
}

extern "C" void anvil_cpp_vm_set_text_host(
    void (*fn)(void*, const char*, anvil_vm::u32, double),
    void* host
) noexcept {
    g_text_host_fn = fn;
    g_text_host = host;
}

static anvil_vm::u32 run_script_text(
    anvil_vm::AnvilVmContext& ctx,
    void* compiler_memory,
    const char* script,
    anvil_vm::u32 script_len,
    anvil_vm::AnvilVmTelemetryC* telemetry_out,
    anvil_vm::f64* numeric_result_out
) noexcept {
    if (!compiler_memory || !script || script_len == 0u) {
        ctx.fail(anvil_vm::VM_REFUSE_SYNTAX, "empty script");
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_REFUSED;
    }
    if (ctx.script_size_limit != 0u && script_len > ctx.script_size_limit) {
        ctx.fail_limit(anvil_vm::VM_REFUSE_CODE_SIZE, "script size", ctx.script_size_limit, "bytes");
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_REFUSED;
    }
    ctx.host_set_text = g_text_host_fn;
    ctx.host = g_text_host;
    if (!anvil_vm::compile_js_to_bytecode(ctx, compiler_memory, script, script_len)) {
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_REFUSED;
    }
    const anvil_vm::AnvilValue res = ctx.execute(0u);
    if (telemetry_out) {
        ctx.fill_telemetry(telemetry_out);
    }
    if (numeric_result_out) {
        *numeric_result_out = res.as_number();
    }
    if (ctx.paused) {
        return anvil_vm::VM_RUN_HOST_CALL;
    }
    return ctx.aborted ? anvil_vm::VM_RUN_STOPPED : anvil_vm::VM_RUN_OK;
}

static anvil_vm::u32 run_request(
    anvil_vm::AnvilVmContext& ctx,
    void* compiler_memory,
    const char* src,
    anvil_vm::u32 len,
    anvil_vm::AnvilVmTelemetryC* telemetry_out,
    anvil_vm::f64* numeric_result_out
) noexcept {
    if (len < anvil_vm::AVK_HEADER_BYTES) {
        ctx.fail(anvil_vm::VM_REFUSE_SYNTAX, "request header too short");
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_REFUSED;
    }
    const anvil_vm::u32 op = anvil_vm::read_u32_le(src + 4);
    const anvil_vm::u32 out_cap = anvil_vm::read_u32_le(src + 12);
    if (telemetry_out && out_cap < static_cast<anvil_vm::u32>(sizeof(anvil_vm::AnvilVmTelemetryC))) {
        return anvil_vm::VM_RUN_BUFFER_SMALL;
    }
    const anvil_vm::u32 pos = anvil_vm::AVK_HEADER_BYTES;
    if (op == anvil_vm::AVK_OP_RUN) {
        return run_script_text(
            ctx, compiler_memory, src + pos, len - pos, telemetry_out, numeric_result_out
        );
    }
    if (op == anvil_vm::AVK_OP_CONFIGURE) {
        if (len >= pos + 16u) {
            const anvil_vm::u32 step = anvil_vm::read_u32_le(src + pos);
            const anvil_vm::u32 script_size = anvil_vm::read_u32_le(src + pos + 4u);
            const anvil_vm::u32 heap = anvil_vm::read_u32_le(src + pos + 8u);
            const anvil_vm::u32 depth = anvil_vm::read_u32_le(src + pos + 12u);
            if (step != 0u) {
                ctx.step_limit = step;
            }
            if (script_size != 0u) {
                ctx.script_size_limit = script_size;
            }
            if (heap != 0u) {
                ctx.heap_byte_budget = heap;
            }
            if (depth != 0u) {
                ctx.call_depth_limit = depth;
            }
            if (len >= pos + 20u) {
                ctx.yield_interval = anvil_vm::read_u32_le(src + pos + 16u);
            }
            if (len >= pos + 24u) {
                const anvil_vm::u32 parse_stack = anvil_vm::read_u32_le(src + pos + 20u);
                if (parse_stack != 0u) {
                    ctx.parse_stack_budget = parse_stack;
                }
            }
        }
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_OK;
    }
    if (op == anvil_vm::AVK_OP_RESUME) {
        if (!ctx.paused) {
            ctx.fail(anvil_vm::VM_REFUSE_UNSUPPORTED, "resume without a paused run");
            ctx.fill_telemetry(telemetry_out);
            return anvil_vm::VM_RUN_REFUSED;
        }
        const anvil_vm::AnvilValue res = ctx.resume();
        if (telemetry_out) {
            ctx.fill_telemetry(telemetry_out);
        }
        if (numeric_result_out) {
            *numeric_result_out = res.as_number();
        }
        if (ctx.paused) {
            return anvil_vm::VM_RUN_HOST_CALL;
        }
        return ctx.aborted ? anvil_vm::VM_RUN_STOPPED : anvil_vm::VM_RUN_OK;
    }
    ctx.fail(anvil_vm::VM_REFUSE_UNSUPPORTED, "unsupported request op");
    ctx.fill_telemetry(telemetry_out);
    return anvil_vm::VM_RUN_REFUSED;
}

anvil_vm::u32 anvil_cpp_vm_run_script(
    void* context_memory,
    void* compiler_memory,
    const char* source_ptr,
    anvil_vm::u32 source_len,
    anvil_vm::AnvilVmTelemetryC* telemetry_out,
    anvil_vm::f64* numeric_result_out
) noexcept {
    if (!context_memory) {
        return 0u;
    }
    auto& ctx = *static_cast<anvil_vm::AnvilVmContext*>(context_memory);
    const bool is_resume = anvil_vm::has_request_magic(source_ptr, source_len) &&
                           source_len >= anvil_vm::AVK_HEADER_BYTES &&
                           anvil_vm::read_u32_le(source_ptr + 4) == anvil_vm::AVK_OP_RESUME;
    if (!is_resume) {
        ctx.init();
    }
    if (anvil_vm::has_request_magic(source_ptr, source_len)) {
        return run_request(
            ctx, compiler_memory, source_ptr, source_len, telemetry_out, numeric_result_out
        );
    }
    return run_script_text(
        ctx, compiler_memory, source_ptr, source_len, telemetry_out, numeric_result_out
    );
}

extern "C" anvil_vm::u32 anvil_cpp_vm_global_count(void* context_memory) noexcept {
    if (!context_memory) {
        return 0u;
    }
    const auto& ctx = *static_cast<const anvil_vm::AnvilVmContext*>(context_memory);
    return ctx.global_count;
}

extern "C" anvil_vm::u32 anvil_cpp_vm_copy_global(
    void* context_memory,
    anvil_vm::u32 index,
    char* name_out,
    anvil_vm::u32 name_cap,
    anvil_vm::f64* value_out
) noexcept {
    if (!context_memory || !name_out || name_cap < 2u || !value_out) {
        return 0u;
    }
    const auto& ctx = *static_cast<const anvil_vm::AnvilVmContext*>(context_memory);
    if (index >= ctx.global_count || !ctx.globals()[index].occupied) {
        return 0u;
    }
    if (!ctx.globals()[index].value.is_number()) {
        return 0u;
    }
    anvil_vm::u32 len = 0u;
    const char* text = ctx.heap.get_string_ptr(ctx.globals()[index].name_sym, &len);
    if (!text || len == 0u) {
        return 0u;
    }
    const anvil_vm::u32 n = (len < name_cap - 1u) ? len : (name_cap - 1u);
    for (anvil_vm::u32 i = 0u; i < n; ++i) {
        name_out[i] = text[i];
    }
    name_out[n] = '\0';
    *value_out = ctx.globals()[index].value.as_number();
    return 1u;
}

anvil_vm::u32 anvil_cpp_vm_run_ic_benchmark(
    void* context_memory,
    anvil_vm::u32 iterations,
    anvil_vm::AnvilVmTelemetryC* telemetry_out
) noexcept {
    if (!context_memory) {
        return 0u;
    }
    auto& g_vm_ctx = *static_cast<anvil_vm::AnvilVmContext*>(context_memory);
    g_vm_ctx.init();

    const anvil_vm::u32 sym_x = g_vm_ctx.heap.intern_string("x", 1u);
    const anvil_vm::u32 sym_y = g_vm_ctx.heap.intern_string("y", 1u);
    const anvil_vm::u32 sym_z = g_vm_ctx.heap.intern_string("z", 1u);

    const anvil_vm::u32 site_set_x = g_vm_ctx.ic.allocate_site(sym_x);
    const anvil_vm::u32 site_set_y = g_vm_ctx.ic.allocate_site(sym_y);
    const anvil_vm::u32 site_get_x = g_vm_ctx.ic.allocate_site(sym_x);
    const anvil_vm::u32 site_get_y = g_vm_ctx.ic.allocate_site(sym_y);

    const anvil_vm::u32 capped = iterations > 4096u ? 4096u : (iterations == 0u ? 16u : iterations);
    anvil_vm::i32 accum = 0;

    for (anvil_vm::u32 i = 0u; i < capped; ++i) {
        if (i != 0u && (i & 255u) == 0u) {
            g_vm_ctx.heap.collect_garbage(nullptr, 0u);
        }
        const anvil_vm::u32 obj_id = g_vm_ctx.heap.alloc_object(0u);
        if (obj_id == anvil_vm::NO_SLOT) {
            break;
        }
        const anvil_vm::AnvilValue obj = anvil_vm::AnvilValue::make_object(obj_id);

        g_vm_ctx.ic.store_property_ic(g_vm_ctx.heap, site_set_x, obj, sym_x, anvil_vm::AnvilValue::make_smi(static_cast<anvil_vm::i32>(i)));
        g_vm_ctx.ic.store_property_ic(g_vm_ctx.heap, site_set_y, obj, sym_y, anvil_vm::AnvilValue::make_smi(2));
        if ((i & 3u) == 0u) {
            anvil_vm::u32 sh = 0u;
            anvil_vm::i32 sl = -1;
            g_vm_ctx.heap.set_object_prop(obj_id, sym_z, anvil_vm::AnvilValue::make_smi(1), &sh, &sl);
        }

        const anvil_vm::AnvilValue vx = g_vm_ctx.ic.load_property_ic(g_vm_ctx.heap, site_get_x, obj, sym_x);
        const anvil_vm::AnvilValue vy = g_vm_ctx.ic.load_property_ic(g_vm_ctx.heap, site_get_y, obj, sym_y);
        accum += vx.as_smi() * vy.as_smi();
        g_vm_ctx.steps_executed += 4u;
    }

    if (telemetry_out) {
        g_vm_ctx.fill_telemetry(telemetry_out);
    }
    return static_cast<anvil_vm::u32>(accum);
}

anvil_vm::f64 anvil_cpp_vm_vector_reduce(anvil_vm::u32 op, const anvil_vm::f64* data_ptr, anvil_vm::u32 len) noexcept {
    if (!data_ptr || len == 0u) {
        return 0.0;
    }
    if (op == 0u) {
        anvil_vm::f64 s0 = 0.0, s1 = 0.0, s2 = 0.0, s3 = 0.0;
        anvil_vm::u32 i = 0u;
        for (; i + 3u < len; i += 4u) {
            s0 += data_ptr[i];
            s1 += data_ptr[i + 1u];
            s2 += data_ptr[i + 2u];
            s3 += data_ptr[i + 3u];
        }
        anvil_vm::f64 total = (s0 + s1) + (s2 + s3);
        for (; i < len; ++i) {
            total += data_ptr[i];
        }
        return total;
    }
    if (op == 1u) {
        anvil_vm::f64 best = data_ptr[0];
        for (anvil_vm::u32 i = 1u; i < len; ++i) {
            if (data_ptr[i] < best) {
                best = data_ptr[i];
            }
        }
        return best;
    }
    if (op == 2u) {
        anvil_vm::f64 best = data_ptr[0];
        for (anvil_vm::u32 i = 1u; i < len; ++i) {
            if (data_ptr[i] > best) {
                best = data_ptr[i];
            }
        }
        return best;
    }
    if (op == 3u) {
        anvil_vm::f64 s0 = 0.0, s1 = 0.0, s2 = 0.0, s3 = 0.0;
        anvil_vm::u32 i = 0u;
        for (; i + 3u < len; i += 4u) {
            s0 += data_ptr[i] * data_ptr[i];
            s1 += data_ptr[i + 1u] * data_ptr[i + 1u];
            s2 += data_ptr[i + 2u] * data_ptr[i + 2u];
            s3 += data_ptr[i + 3u] * data_ptr[i + 3u];
        }
        anvil_vm::f64 total = (s0 + s1) + (s2 + s3);
        for (; i < len; ++i) {
            total += data_ptr[i] * data_ptr[i];
        }
        return total;
    }
    return 0.0;
}

} // extern "C"
