// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 Hidden Classes (AnvilShape), String Interning Pool, and Mark-Sweep GC Heap.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {

u32 format_u64_dec(u64 v, char* out) noexcept {
    if (v == 0ULL) {
        out[0] = '0';
        return 1u;
    }
    char rev[24];
    u32 n = 0u;
    while (v != 0ULL) {
        rev[n++] = static_cast<char>('0' + (v % 10ULL));
        v /= 10ULL;
    }
    for (u32 i = 0u; i < n; ++i) {
        out[i] = rev[n - 1u - i];
    }
    return n;
}

u32 format_sig_exp10(u64 sig, u32 prec, i32 exp10, char* out) noexcept {
    u64 p10 = 1ULL;
    for (u32 i = 0u; i < prec; ++i) {
        p10 *= 10ULL;
    }
    if (sig >= p10) {
        sig /= 10ULL;
        ++exp10;
    }
    char digits[20];
    for (i32 i = static_cast<i32>(prec) - 1; i >= 0; --i) {
        digits[i] = static_cast<char>('0' + (sig % 10ULL));
        sig /= 10ULL;
    }
    u32 ndig = prec;
    while (ndig > 1u && digits[ndig - 1u] == '0') {
        --ndig;
    }
    u32 pos = 0u;
    if (exp10 >= 0 && exp10 < 21) {
        const u32 int_digits = static_cast<u32>(exp10) + 1u;
        if (ndig <= int_digits) {
            for (u32 i = 0u; i < ndig; ++i) {
                out[pos++] = digits[i];
            }
            for (u32 i = ndig; i < int_digits; ++i) {
                out[pos++] = '0';
            }
        } else {
            for (u32 i = 0u; i < int_digits; ++i) {
                out[pos++] = digits[i];
            }
            out[pos++] = '.';
            for (u32 i = int_digits; i < ndig; ++i) {
                out[pos++] = digits[i];
            }
        }
        return pos;
    }
    if (exp10 >= -6 && exp10 < 0) {
        out[pos++] = '0';
        out[pos++] = '.';
        const u32 zeros = static_cast<u32>(-exp10 - 1);
        for (u32 i = 0u; i < zeros; ++i) {
            out[pos++] = '0';
        }
        for (u32 i = 0u; i < ndig; ++i) {
            out[pos++] = digits[i];
        }
        return pos;
    }
    out[pos++] = digits[0];
    if (ndig > 1u) {
        out[pos++] = '.';
        for (u32 i = 1u; i < ndig; ++i) {
            out[pos++] = digits[i];
        }
    }
    out[pos++] = 'e';
    if (exp10 >= 0) {
        out[pos++] = '+';
        pos += format_u64_dec(static_cast<u64>(exp10), out + pos);
    } else {
        out[pos++] = '-';
        pos += format_u64_dec(static_cast<u64>(-exp10), out + pos);
    }
    return pos;
}

u32 format_js_number(f64 x, char* out) noexcept {
    if (js_is_nan(x)) {
        out[0] = 'N'; out[1] = 'a'; out[2] = 'N';
        return 3u;
    }
    if (x == 0.0) {
        out[0] = '0';
        return 1u;
    }
    u32 pos = 0u;
    if (x < 0.0) {
        out[pos++] = '-';
        x = -x;
    }
    if (!js_is_finite(x)) {
        const char* inf = "Infinity";
        for (u32 i = 0u; i < 8u; ++i) {
            out[pos++] = inf[i];
        }
        return pos;
    }
    if (x < 18446744073709551616.0 && js_floor(x) == x) {
        pos += format_u64_dec(static_cast<u64>(x), out + pos);
        return pos;
    }
    i32 exp10 = 0;
    f64 m = x;
    while (m >= 1e16) { m /= 1e16; exp10 += 16; }
    while (m >= 10.0) { m /= 10.0; ++exp10; }
    while (m > 0.0 && m < 1e-15) { m *= 1e16; exp10 -= 16; }
    while (m > 0.0 && m < 1.0) { m *= 10.0; --exp10; }

    static const f64 pow10_exact[23] = {
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10,
        1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22
    };
    const i32 scale_exp = 16 - exp10;
    f64 scaled = x;
    if (scale_exp >= 0) {
        i32 rem = scale_exp;
        while (rem > 22) { scaled *= 1e16; rem -= 16; }
        scaled *= pow10_exact[rem];
    } else {
        i32 rem = -scale_exp;
        while (rem > 22) { scaled /= 1e16; rem -= 16; }
        scaled /= pow10_exact[rem];
    }

    u64 sig17 = static_cast<u64>(scaled + 0.5);
    if (sig17 >= 100000000000000000ULL) {
        sig17 /= 10ULL;
        ++exp10;
    }
    if (sig17 < 10000000000000000ULL) {
        sig17 = 10000000000000000ULL;
    }
    const u64 target_bits = f64_to_bits(x);
    for (u32 step = 0u; step < 8u; ++step) {
        const u32 n17 = format_sig_exp10(sig17, 17u, exp10, out + pos);
        const f64 parsed = parse_f64_slice(out + pos, n17);
        const u64 pbits = f64_to_bits(parsed);
        if (pbits == target_bits) {
            break;
        }
        if (pbits < target_bits) {
            ++sig17;
            if (sig17 >= 100000000000000000ULL) {
                sig17 = 10000000000000000ULL;
                ++exp10;
            }
        } else {
            if (sig17 == 10000000000000000ULL) {
                sig17 = 99999999999999999ULL;
                --exp10;
            } else {
                --sig17;
            }
        }
    }
    if (exp10 < -307) {
        u64 div = 10000000000000000ULL;
        for (u32 prec = 1u; prec <= 14u; ++prec) {
            const u64 sig_p = (sig17 + (div >> 1u)) / div;
            const u32 np = format_sig_exp10(sig_p, prec, exp10, out + pos);
            if (f64_to_bits(parse_f64_slice(out + pos, np)) == target_bits) {
                return pos + np;
            }
            div /= 10ULL;
        }
    }
    const u64 sig15 = (sig17 + 50ULL) / 100ULL;
    const u32 n15 = format_sig_exp10(sig15, 15u, exp10, out + pos);
    if (f64_to_bits(parse_f64_slice(out + pos, n15)) == target_bits) {
        return pos + n15;
    }
    const u64 sig16 = (sig17 + 5ULL) / 10ULL;
    const u32 n16 = format_sig_exp10(sig16, 16u, exp10, out + pos);
    if (f64_to_bits(parse_f64_slice(out + pos, n16)) == target_bits) {
        return pos + n16;
    }
    const u32 n17 = format_sig_exp10(sig17, 17u, exp10, out + pos);
    return pos + n17;
}

inline bool bytes_equal(const char* a, const char* b, u32 len) noexcept {
    for (u32 i = 0; i < len; ++i) {
        if (a[i] != b[i]) {
            return false;
        }
    }
    return true;
}

constexpr u32 INIT_STR_POOL_BYTES = 1024u;
constexpr u32 INIT_STR_TABLE = 64u;
constexpr u32 INIT_ARRAY_TABLE = 16u;
constexpr u32 INIT_SHAPE_TABLE = 16u;
constexpr u32 INIT_TRANSITION_TABLE = 16u;
constexpr u32 INIT_OBJECT_TABLE = 16u;
constexpr u32 INIT_OBJECT_SLOTS = 4u;
constexpr u32 INIT_FUNCTION_TABLE = 16u;

} // namespace

bool AnvilHeap::ensure_string_bytes(u32 need) noexcept {
    if (need <= str_pool_cap) {
        return true;
    }
    u32 new_cap = str_pool_cap == 0u ? INIT_STR_POOL_BYTES : str_pool_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena.alloc(new_cap, &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    char* dst = reinterpret_cast<char*>(arena.at(new_off));
    const char* src = string_bytes();
    for (u32 i = 0u; i < string_bytes_used; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = str_pool_off;
    const u32 old_cap = str_pool_cap;
    str_pool_off = new_off;
    str_pool_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap);
    }
    return true;
}

bool AnvilHeap::ensure_string_table(u32 need) noexcept {
    if (need <= strings_cap) {
        return true;
    }
    u32 new_cap = strings_cap == 0u ? INIT_STR_TABLE : strings_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 bytes = new_cap * static_cast<u32>(sizeof(AnvilInternedString));
    const u32 new_off = arena.alloc(bytes, &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilInternedString* dst = reinterpret_cast<AnvilInternedString*>(arena.at(new_off));
    const AnvilInternedString* src = strings();
    for (u32 i = 0u; i < string_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = strings_off;
    const u32 old_cap = strings_cap;
    strings_off = new_off;
    strings_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilInternedString)));
    }
    return true;
}

void AnvilHeap::init(u8* arena_base, u32 reserve, u32 budget) noexcept {
    arena.init(arena_base, reserve, budget);
    str_pool_off = 0u;
    str_pool_cap = 0u;
    string_bytes_used = 0u;
    strings_off = 0u;
    strings_cap = 0u;
    string_count = 0u;
    ensure_string_bytes(INIT_STR_POOL_BYTES);
    ensure_string_table(INIT_STR_TABLE);

    shapes_off = 0u;
    shapes_cap = 0u;
    shape_count = 0u;
    transitions_off = 0u;
    transitions_cap = 0u;
    transition_count = 0u;
    objects_off = 0u;
    objects_cap = 0u;
    object_count = 0u;
    ensure_shapes(INIT_SHAPE_TABLE);
    ensure_transitions(INIT_TRANSITION_TABLE);
    ensure_objects(INIT_OBJECT_TABLE);
    if (shapes_cap > 0u) {
        shape_count = 1u;
        AnvilShape& root = shapes()[0];
        root.id = 0u;
        root.parent_shape = 0u;
        root.prop_count = 0u;
    }

    arrays_off = 0u;
    arrays_cap = 0u;
    array_count = 0u;
    ensure_arrays(INIT_ARRAY_TABLE);

    functions_off = 0u;
    functions_cap = 0u;
    function_count = 0u;
    ensure_functions(INIT_FUNCTION_TABLE);

    closures_off = 0u;
    closures_cap = 0u;
    closure_count = 0u;
    live_closures = 0u;
    ensure_closures(INIT_FUNCTION_TABLE);

    err_proto_id = NO_SLOT;
    type_err_proto_id = NO_SLOT;
    range_err_proto_id = NO_SLOT;
    syntax_err_proto_id = NO_SLOT;
    ref_err_proto_id = NO_SLOT;

    live_objects = 0u;
    live_arrays = 0u;
    bytes_allocated = sizeof(AnvilShape);
    gc_collections = 0u;
    shape_transitions_count = 0u;
}

u32 AnvilHeap::intern_string(const char* str, u32 len) noexcept {
    if (!str && len > 0u) {
        len = 0u;
    }
    const u64 h = fnv1a_hash_bytes(str, len);
    for (u32 i = 0; i < string_count; ++i) {
        const AnvilInternedString& e = strings()[i];
        if (e.active && e.hash == h && e.length == len) {
            if (len == 0u || bytes_equal(&string_bytes()[e.offset], str, len)) {
                return i;
            }
        }
    }
    if (!ensure_string_table(string_count + 1u)) {
        return NO_SLOT;
    }
    if (!ensure_string_bytes(string_bytes_used + len)) {
        return NO_SLOT;
    }
    const u32 sym = string_count++;
    const u32 off = string_bytes_used;
    char* pool = string_bytes();
    for (u32 i = 0; i < len; ++i) {
        pool[off + i] = str[i];
    }
    string_bytes_used += len;
    AnvilInternedString& e = strings()[sym];
    e.hash = h;
    e.offset = off;
    e.length = len;
    e.marked = true;
    e.active = true;
    bytes_allocated += static_cast<u32>(sizeof(AnvilInternedString)) + len;
    return sym;
}

u32 AnvilHeap::intern_concat(u32 sym_a, u32 sym_b) noexcept {
    u32 len_a = 0u;
    u32 len_b = 0u;
    (void) get_string_ptr(sym_a, &len_a);
    (void) get_string_ptr(sym_b, &len_b);
    if (!ensure_string_bytes(string_bytes_used + len_a + len_b)) {
        return NO_SLOT;
    }
    const u32 off = string_bytes_used;
    char* pool = string_bytes();
    const char* ptr_a = get_string_ptr(sym_a, nullptr);
    const char* ptr_b = get_string_ptr(sym_b, nullptr);
    if (len_a >= 3u && len_b >= 3u) {
        const u8 a0 = static_cast<u8>(ptr_a[len_a - 3u]);
        const u8 a1 = static_cast<u8>(ptr_a[len_a - 2u]);
        const u8 a2 = static_cast<u8>(ptr_a[len_a - 1u]);
        const u8 b0 = static_cast<u8>(ptr_b[0u]);
        const u8 b1 = static_cast<u8>(ptr_b[1u]);
        const u8 b2 = static_cast<u8>(ptr_b[2u]);
        if (a0 == 0xEDu && a1 >= 0xA0u && a1 <= 0xAFu && (a2 & 0xC0u) == 0x80u &&
            b0 == 0xEDu && b1 >= 0xB0u && b1 <= 0xBFu && (b2 & 0xC0u) == 0x80u) {
            const u32 hi = ((static_cast<u32>(a0 & 0x0Fu) << 12u) |
                            (static_cast<u32>(a1 & 0x3Fu) << 6u) |
                            static_cast<u32>(a2 & 0x3Fu));
            const u32 lo = ((static_cast<u32>(b0 & 0x0Fu) << 12u) |
                            (static_cast<u32>(b1 & 0x3Fu) << 6u) |
                            static_cast<u32>(b2 & 0x3Fu));
            const u32 cp = 0x10000u + ((hi - 0xD800u) << 10u) + (lo - 0xDC00u);
            u32 w = 0u;
            for (u32 i = 0u; i < len_a - 3u; ++i) {
                pool[off + w++] = ptr_a[i];
            }
            pool[off + w++] = static_cast<char>(0xF0u | (cp >> 18u));
            pool[off + w++] = static_cast<char>(0x80u | ((cp >> 12u) & 0x3Fu));
            pool[off + w++] = static_cast<char>(0x80u | ((cp >> 6u) & 0x3Fu));
            pool[off + w++] = static_cast<char>(0x80u | (cp & 0x3Fu));
            for (u32 i = 3u; i < len_b; ++i) {
                pool[off + w++] = ptr_b[i];
            }
            return intern_string(&pool[off], w);
        }
    }
    for (u32 i = 0u; i < len_a; ++i) {
        pool[off + i] = ptr_a[i];
    }
    for (u32 i = 0u; i < len_b; ++i) {
        pool[off + len_a + i] = ptr_b[i];
    }
    return intern_string(&pool[off], len_a + len_b);
}

const char* AnvilHeap::get_string_ptr(u32 sym, u32* len_out) const noexcept {
    if (sym >= string_count || !strings()[sym].active) {
        if (len_out) {
            *len_out = 0u;
        }
        return "";
    }
    if (len_out) {
        *len_out = strings()[sym].length;
    }
    return &string_bytes()[strings()[sym].offset];
}

bool AnvilHeap::string_equals(u32 sym_a, u32 sym_b) const noexcept {
    if (sym_a == sym_b) {
        return true;
    }
    if (sym_a >= string_count || sym_b >= string_count) {
        return false;
    }
    const AnvilInternedString& a = strings()[sym_a];
    const AnvilInternedString& b = strings()[sym_b];
    if (a.hash != b.hash || a.length != b.length) {
        return false;
    }
    return bytes_equal(&string_bytes()[a.offset], &string_bytes()[b.offset], a.length);
}

bool AnvilHeap::ensure_shapes(u32 need) noexcept {
    if (need <= shapes_cap) {
        return true;
    }
    u32 new_cap = shapes_cap == 0u ? INIT_SHAPE_TABLE : shapes_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilShape)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilShape* dst = reinterpret_cast<AnvilShape*>(arena.at(new_off));
    const AnvilShape* src = shapes();
    for (u32 i = 0u; i < shape_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = shapes_off;
    const u32 old_cap = shapes_cap;
    shapes_off = new_off;
    shapes_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilShape)));
    }
    return true;
}

bool AnvilHeap::ensure_transitions(u32 need) noexcept {
    if (need <= transitions_cap) {
        return true;
    }
    u32 new_cap = transitions_cap == 0u ? INIT_TRANSITION_TABLE : transitions_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilShapeTransition)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilShapeTransition* dst = reinterpret_cast<AnvilShapeTransition*>(arena.at(new_off));
    const AnvilShapeTransition* src = transitions();
    for (u32 i = 0u; i < transition_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = transitions_off;
    const u32 old_cap = transitions_cap;
    transitions_off = new_off;
    transitions_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilShapeTransition)));
    }
    return true;
}

bool AnvilHeap::ensure_functions(u32 need) noexcept {
    if (need <= functions_cap) {
        return true;
    }
    u32 new_cap = functions_cap == 0u ? INIT_FUNCTION_TABLE : functions_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilHeapFunction)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilHeapFunction* dst = reinterpret_cast<AnvilHeapFunction*>(arena.at(new_off));
    const AnvilHeapFunction* src = functions();
    for (u32 i = 0u; i < function_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = functions_off;
    const u32 old_cap = functions_cap;
    functions_off = new_off;
    functions_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilHeapFunction)));
    }
    return true;
}

bool AnvilHeap::ensure_objects(u32 need) noexcept {
    if (need <= objects_cap) {
        return true;
    }
    u32 new_cap = objects_cap == 0u ? INIT_OBJECT_TABLE : objects_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilHeapObject)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilHeapObject* dst = reinterpret_cast<AnvilHeapObject*>(arena.at(new_off));
    const AnvilHeapObject* src = objects();
    for (u32 i = 0u; i < object_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = objects_off;
    const u32 old_cap = objects_cap;
    objects_off = new_off;
    objects_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilHeapObject)));
    }
    return true;
}

i32 AnvilHeap::lookup_shape_slot(u32 shape_id, u32 prop_sym) const noexcept {
    if (shape_id >= shape_count) {
        return -1;
    }
    return shapes()[shape_id].find_slot(prop_sym);
}

u32 AnvilHeap::transition_shape(u32 current_shape, u32 prop_sym) noexcept {
    if (current_shape >= shape_count) {
        return 0u;
    }
    if (shapes()[current_shape].find_slot(prop_sym) >= 0) {
        return current_shape;
    }
    for (u32 i = 0; i < transition_count; ++i) {
        if (transitions()[i].from_shape == current_shape && transitions()[i].prop_sym == prop_sym) {
            return transitions()[i].to_shape;
        }
    }
    if (shapes()[current_shape].prop_count >= DICT_THRESHOLD) {
        return current_shape;
    }
    u32 parent_count = shapes()[current_shape].prop_count;
    u32 parent_syms[DICT_THRESHOLD];
    for (u32 i = 0; i < parent_count; ++i) {
        parent_syms[i] = shapes()[current_shape].prop_syms[i];
    }
    if (!ensure_shapes(shape_count + 1u)) {
        return current_shape;
    }
    const u32 next_id = shape_count++;
    AnvilShape& child = shapes()[next_id];
    child.id = next_id;
    child.parent_shape = current_shape;
    child.prop_count = parent_count + 1u;
    for (u32 i = 0; i < parent_count; ++i) {
        child.prop_syms[i] = parent_syms[i];
    }
    child.prop_syms[parent_count] = prop_sym;

    if (ensure_transitions(transition_count + 1u)) {
        AnvilShapeTransition& t = transitions()[transition_count];
        t.from_shape = current_shape;
        t.prop_sym = prop_sym;
        t.to_shape = next_id;
        ++transition_count;
    }
    ++shape_transitions_count;
    bytes_allocated += static_cast<u32>(sizeof(AnvilShape));
    return next_id;
}

u32 AnvilHeap::alloc_object(u32 initial_shape) noexcept {
    bool over = false;
    const u32 slots_off = arena.alloc(INIT_OBJECT_SLOTS * static_cast<u32>(sizeof(AnvilValue)), &over);
    if (slots_off == NO_SLOT) {
        return NO_SLOT;
    }
    u32 id = NO_SLOT;
    for (u32 i = 0; i < object_count; ++i) {
        if (!objects()[i].active) {
            id = i;
            break;
        }
    }
    if (id == NO_SLOT) {
        if (!ensure_objects(object_count + 1u)) {
            arena.free(slots_off, INIT_OBJECT_SLOTS * static_cast<u32>(sizeof(AnvilValue)));
            return NO_SLOT;
        }
        id = object_count++;
    }
    AnvilValue* slots = object_slots(slots_off);
    for (u32 s = 0; s < INIT_OBJECT_SLOTS; ++s) {
        slots[s] = AnvilValue::make_undefined();
    }
    AnvilHeapObject& obj = objects()[id];
    obj.active = true;
    obj.marked = false;
    obj.dict = false;
    obj.builtin_kind = static_cast<u8>(BuiltinObjectKind::Plain);
    obj.shape_id = initial_shape < shape_count ? initial_shape : 0u;
    obj.proto_obj_id = 0xFFFFFFFFu;
    obj.prop_count = obj.shape_id < shape_count ? shapes()[obj.shape_id].prop_count : 0u;
    obj.slots_off = slots_off;
    obj.slots_cap = INIT_OBJECT_SLOTS;
    obj.dict_keys_off = NO_SLOT;
    obj.aux_id = NO_SLOT;
    obj.byte_offset = 0u;
    obj.byte_length = 0u;
    ++live_objects;
    bytes_allocated += static_cast<u32>(sizeof(AnvilHeapObject));
    return id;
}

u32 AnvilHeap::alloc_error_object(BuiltinCtorKind ctor_kind, u32 msg_sym) noexcept {
    const u32 id = alloc_object(0u);
    if (id == NO_SLOT) {
        return NO_SLOT;
    }
    objects()[id].builtin_kind = static_cast<u8>(BuiltinObjectKind::Error);
    objects()[id].aux_id = static_cast<u32>(ctor_kind);
    const char* err_name = "Error";
    u32 err_name_len = 5u;
    switch (ctor_kind) {
        case BuiltinCtorKind::TypeError:
            err_name = "TypeError";
            err_name_len = 9u;
            break;
        case BuiltinCtorKind::RangeError:
            err_name = "RangeError";
            err_name_len = 10u;
            break;
        case BuiltinCtorKind::SyntaxError:
            err_name = "SyntaxError";
            err_name_len = 11u;
            break;
        case BuiltinCtorKind::ReferenceError:
            err_name = "ReferenceError";
            err_name_len = 14u;
            break;
        default:
            break;
    }
    const u32 k_name = intern_string("name", 4u);
    const u32 v_name = intern_string(err_name, err_name_len);
    const u32 k_msg = intern_string("message", 7u);
    if (k_name == NO_SLOT || v_name == NO_SLOT || k_msg == NO_SLOT) {
        return NO_SLOT;
    }
    if (set_object_prop(id, k_name, AnvilValue::make_string(v_name)) != StoreResult::Ok) {
        return NO_SLOT;
    }
    if (set_object_prop(id, k_msg, AnvilValue::make_string(msg_sym)) != StoreResult::Ok) {
        return NO_SLOT;
    }
    return id;
}

bool AnvilHeap::ensure_arrays(u32 need) noexcept {
    if (need <= arrays_cap) {
        return true;
    }
    u32 new_cap = arrays_cap == 0u ? INIT_ARRAY_TABLE : arrays_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 bytes = new_cap * static_cast<u32>(sizeof(AnvilHeapArray));
    const u32 new_off = arena.alloc(bytes, &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilHeapArray* dst = reinterpret_cast<AnvilHeapArray*>(arena.at(new_off));
    const AnvilHeapArray* src = arrays();
    for (u32 i = 0u; i < array_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = arrays_off;
    const u32 old_cap = arrays_cap;
    arrays_off = new_off;
    arrays_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilHeapArray)));
    }
    return true;
}

u32 AnvilHeap::alloc_array(u32 initial_capacity) noexcept {
    const u32 cap = initial_capacity < 4u ? 4u : (initial_capacity > 128u ? 128u : initial_capacity);
    bool over = false;
    const u32 elems_off = arena.alloc(cap * static_cast<u32>(sizeof(AnvilValue)), &over);
    if (elems_off == NO_SLOT) {
        return NO_SLOT;
    }
    u32 id = NO_SLOT;
    for (u32 i = 0; i < array_count; ++i) {
        if (!arrays()[i].active) {
            id = i;
            break;
        }
    }
    if (id == NO_SLOT) {
        if (!ensure_arrays(array_count + 1u)) {
            arena.free(elems_off, cap * static_cast<u32>(sizeof(AnvilValue)));
            return NO_SLOT;
        }
        id = array_count++;
    }
    AnvilValue* store = array_elems(elems_off);
    for (u32 k = 0; k < cap; ++k) {
        store[k] = AnvilValue::make_undefined();
    }
    AnvilHeapArray& arr = arrays()[id];
    arr.active = true;
    arr.marked = false;
    arr.length = 0u;
    arr.storage_off = elems_off;
    arr.capacity = cap;
    ++live_arrays;
    bytes_allocated += static_cast<u32>(sizeof(AnvilHeapArray)) + cap * static_cast<u32>(sizeof(AnvilValue));
    return id;
}

u32 AnvilHeap::alloc_cell(AnvilValue v) noexcept {
    const u32 id = alloc_array(1u);
    if (id == NO_SLOT) {
        return NO_SLOT;
    }
    arrays()[id].length = 1u;
    array_elems(arrays()[id].storage_off)[0] = v;
    return id;
}

u32 AnvilHeap::alloc_function(
    u32 name_sym,
    u32 arity,
    u32 local_count,
    u32 code_start,
    u32 code_len
) noexcept {
    if (!ensure_functions(function_count + 1u)) {
        return NO_SLOT;
    }
    const u32 id = function_count++;
    AnvilHeapFunction& f = functions()[id];
    f.active = true;
    f.marked = true;
    f.name_sym = name_sym;
    f.arity = arity;
    f.local_count = local_count;
    f.code_start = code_start;
    f.code_len = code_len;
    f.canonical_closure = NO_SLOT;
    f.home_proto_id = NO_SLOT;
    bytes_allocated += static_cast<u32>(sizeof(AnvilHeapFunction));
    return id;
}

bool AnvilHeap::ensure_closures(u32 need) noexcept {
    if (need <= closures_cap) {
        return true;
    }
    u32 new_cap = closures_cap == 0u ? INIT_FUNCTION_TABLE : closures_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilHeapClosure)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilHeapClosure* dst = reinterpret_cast<AnvilHeapClosure*>(arena.at(new_off));
    const AnvilHeapClosure* src = closures();
    for (u32 i = 0u; i < closure_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = closures_off;
    const u32 old_cap = closures_cap;
    closures_off = new_off;
    closures_cap = new_cap;
    if (old_cap != 0u) {
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilHeapClosure)));
    }
    return true;
}

u32 AnvilHeap::alloc_closure(u32 fn_id, u32 upvals_off, u32 upval_count, bool pinned) noexcept {
    u32 id = NO_SLOT;
    for (u32 i = 0u; i < closure_count; ++i) {
        if (!closures()[i].active) {
            id = i;
            break;
        }
    }
    if (id == NO_SLOT) {
        if (!ensure_closures(closure_count + 1u)) {
            return NO_SLOT;
        }
        id = closure_count++;
    }
    AnvilHeapClosure& c = closures()[id];
    c.active = true;
    c.marked = false;
    c.pinned = pinned;
    c.fn_id = fn_id;
    c.upvals_off = upvals_off;
    c.upval_count = upval_count;
    c.proto_obj_id = NO_SLOT;
    c.static_obj_id = NO_SLOT;
    c.super_closure_id = NO_SLOT;
    ++live_closures;
    bytes_allocated += static_cast<u32>(sizeof(AnvilHeapClosure));
    return id;
}

static bool find_own_prop(const AnvilHeap& heap, const AnvilHeapObject& obj, u32 prop_sym,
                          AnvilValue* out) noexcept {
    if (obj.dict) {
        const u32* keys = heap.object_keys(obj.dict_keys_off);
        const AnvilValue* slots = heap.object_slots(obj.slots_off);
        for (u32 i = 0; i < obj.prop_count; ++i) {
            if (keys[i] == prop_sym) {
                *out = slots[i];
                return true;
            }
        }
        return false;
    }
    const i32 slot = heap.lookup_shape_slot(obj.shape_id, prop_sym);
    if (slot >= 0 && static_cast<u32>(slot) < obj.slots_cap) {
        *out = heap.object_slots(obj.slots_off)[slot];
        return true;
    }
    return false;
}

AnvilValue AnvilHeap::get_object_prop(
    u32 obj_id,
    u32 prop_sym,
    u32* out_shape,
    i32* out_slot
) const noexcept {
    if (obj_id >= object_count || !objects()[obj_id].active) {
        if (out_slot) {
            *out_slot = -1;
        }
        return AnvilValue::make_undefined();
    }
    const AnvilHeapObject& obj = objects()[obj_id];
    if (out_shape) {
        *out_shape = obj.shape_id;
    }
    if (obj.dict) {
        if (out_slot) {
            *out_slot = -1;
        }
    } else {
        const i32 slot = lookup_shape_slot(obj.shape_id, prop_sym);
        if (out_slot) {
            *out_slot = slot;
        }
    }
    AnvilValue found;
    if (find_own_prop(*this, obj, prop_sym, &found)) {
        return found;
    }
    // Check prototype chain (up to 16 hops to avoid cycles).
    u32 proto = obj.proto_obj_id;
    u32 hops = 0u;
    while (proto < object_count && objects()[proto].active && hops < 16u) {
        if (find_own_prop(*this, objects()[proto], prop_sym, &found)) {
            return found;
        }
        proto = objects()[proto].proto_obj_id;
        ++hops;
    }
    return AnvilValue::make_undefined();
}

bool AnvilHeap::has_object_prop(u32 obj_id, u32 prop_sym) const noexcept {
    if (obj_id >= object_count || !objects()[obj_id].active) {
        return false;
    }
    const AnvilHeapObject& obj = objects()[obj_id];
    AnvilValue found;
    if (find_own_prop(*this, obj, prop_sym, &found)) {
        return true;
    }
    u32 proto = obj.proto_obj_id;
    u32 hops = 0u;
    while (proto < object_count && objects()[proto].active && hops < 16u) {
        if (find_own_prop(*this, objects()[proto], prop_sym, &found)) {
            return true;
        }
        proto = objects()[proto].proto_obj_id;
        ++hops;
    }
    return false;
}

bool AnvilHeap::delete_object_prop(u32 obj_id, u32 prop_sym) noexcept {
    if (obj_id >= object_count || !objects()[obj_id].active) {
        return true;
    }
    AnvilValue dummy;
    if (!find_own_prop(*this, objects()[obj_id], prop_sym, &dummy)) {
        return true;
    }
    if (!objects()[obj_id].dict && !convert_object_to_dict(obj_id)) {
        return false;
    }
    AnvilHeapObject& obj = objects()[obj_id];
    u32* keys = object_keys(obj.dict_keys_off);
    AnvilValue* slots = object_slots(obj.slots_off);
    for (u32 i = 0; i < obj.prop_count; ++i) {
        if (keys[i] == prop_sym) {
            for (u32 j = i; j + 1u < obj.prop_count; ++j) {
                keys[j] = keys[j + 1u];
                slots[j] = slots[j + 1u];
            }
            --obj.prop_count;
            keys[obj.prop_count] = 0u;
            slots[obj.prop_count] = AnvilValue::make_undefined();
            break;
        }
    }
    return true;
}

u32 AnvilHeap::value_to_string_sym(AnvilValue val) noexcept {
    switch (val.tag()) {
        case ValueTag::String:
            return val.payload();
        case ValueTag::Undefined:
            return intern_string("undefined", 9u);
        case ValueTag::Null:
            return intern_string("null", 4u);
        case ValueTag::Bool:
            return (val.payload() != 0u) ? intern_string("true", 4u) : intern_string("false", 5u);
        case ValueTag::Smi:
        case ValueTag::Number: {
            char buf[64];
            const u32 n = format_js_number(val.as_number(), buf);
            return intern_string(buf, n);
        }
        case ValueTag::Array: {
            const u32 arr_id = val.payload();
            if (arr_id >= array_count || !arrays()[arr_id].active) {
                return intern_string("", 0u);
            }
            u32 acc = intern_string("", 0u);
            const u32 comma = intern_string(",", 1u);
            if (acc == NO_SLOT || comma == NO_SLOT) {
                return NO_SLOT;
            }
            const u32 len = arrays()[arr_id].length;
            for (u32 i = 0u; i < len; ++i) {
                if (i > 0u) {
                    acc = intern_concat(acc, comma);
                    if (acc == NO_SLOT) {
                        return NO_SLOT;
                    }
                }
                const AnvilValue elem = get_array_elem(arr_id, static_cast<i32>(i));
                if (!elem.is_undefined() && !elem.is_null()) {
                    const u32 part = value_to_string_sym(elem);
                    if (part == NO_SLOT) {
                        return NO_SLOT;
                    }
                    acc = intern_concat(acc, part);
                    if (acc == NO_SLOT) {
                        return NO_SLOT;
                    }
                }
            }
            return acc;
        }
        case ValueTag::Object: {
            const u32 obj_id = val.payload();
            if (obj_id < object_count && objects()[obj_id].active &&
                objects()[obj_id].builtin_kind == static_cast<u8>(BuiltinObjectKind::Error)) {
                const u32 k_name = intern_string("name", 4u);
                const u32 k_msg = intern_string("message", 7u);
                if (k_name == NO_SLOT || k_msg == NO_SLOT) {
                    return NO_SLOT;
                }
                const AnvilValue nv = get_object_prop(obj_id, k_name);
                const AnvilValue mv = get_object_prop(obj_id, k_msg);
                const u32 n_sym = nv.is_undefined() ? intern_string("Error", 5u) : value_to_string_sym(nv);
                const u32 m_sym = mv.is_undefined() ? intern_string("", 0u) : value_to_string_sym(mv);
                if (n_sym == NO_SLOT || m_sym == NO_SLOT) {
                    return NO_SLOT;
                }
                u32 m_len = 0u;
                (void) get_string_ptr(m_sym, &m_len);
                if (m_len == 0u) {
                    return n_sym;
                }
                const u32 sep = intern_string(": ", 2u);
                if (sep == NO_SLOT) {
                    return NO_SLOT;
                }
                const u32 a = intern_concat(n_sym, sep);
                return a == NO_SLOT ? NO_SLOT : intern_concat(a, m_sym);
            }
            return intern_string("[object Object]", 15u);
        }
        case ValueTag::Function:
            return intern_string("function () { [native code] }", 29u);
    }
    return intern_string("", 0u);
}

bool AnvilHeap::ensure_object_slots(u32 obj_id, u32 need) noexcept {
    if (need <= objects()[obj_id].slots_cap) {
        return true;
    }
    u32 new_cap = objects()[obj_id].slots_cap == 0u ? INIT_OBJECT_SLOTS : objects()[obj_id].slots_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    const bool dict = objects()[obj_id].dict;
    bool over = false;
    const u32 new_slots = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilValue)), &over);
    if (new_slots == NO_SLOT) {
        return false;
    }
    u32 new_keys = NO_SLOT;
    if (dict) {
        new_keys = arena.alloc(new_cap * static_cast<u32>(sizeof(u32)), &over);
        if (new_keys == NO_SLOT) {
            arena.free(new_slots, new_cap * static_cast<u32>(sizeof(AnvilValue)));
            return false;
        }
    }
    AnvilHeapObject& obj = objects()[obj_id];
    const u32 pc = obj.prop_count;
    AnvilValue* sdst = object_slots(new_slots);
    const AnvilValue* ssrc = object_slots(obj.slots_off);
    for (u32 i = 0; i < new_cap; ++i) {
        sdst[i] = (i < pc) ? ssrc[i] : AnvilValue::make_undefined();
    }
    const u32 old_slots = obj.slots_off;
    const u32 old_cap = obj.slots_cap;
    obj.slots_off = new_slots;
    obj.slots_cap = new_cap;
    arena.free(old_slots, old_cap * static_cast<u32>(sizeof(AnvilValue)));
    if (dict) {
        u32* kdst = object_keys(new_keys);
        const u32* ksrc = object_keys(obj.dict_keys_off);
        for (u32 i = 0; i < new_cap; ++i) {
            kdst[i] = (i < pc) ? ksrc[i] : 0u;
        }
        const u32 old_keys = obj.dict_keys_off;
        obj.dict_keys_off = new_keys;
        arena.free(old_keys, old_cap * static_cast<u32>(sizeof(u32)));
    }
    return true;
}

bool AnvilHeap::convert_object_to_dict(u32 obj_id) noexcept {
    if (objects()[obj_id].dict) {
        return true;
    }
    const u32 cap = objects()[obj_id].slots_cap;
    bool over = false;
    const u32 keys_off = arena.alloc(cap * static_cast<u32>(sizeof(u32)), &over);
    if (keys_off == NO_SLOT) {
        return false;
    }
    AnvilHeapObject& obj = objects()[obj_id];
    u32* keys = object_keys(keys_off);
    for (u32 i = 0; i < cap; ++i) {
        keys[i] = 0u;
    }
    if (obj.shape_id < shape_count) {
        const AnvilShape& sh = shapes()[obj.shape_id];
        const u32 n = sh.prop_count < obj.prop_count ? sh.prop_count : obj.prop_count;
        for (u32 i = 0; i < n; ++i) {
            keys[i] = sh.prop_syms[i];
        }
    }
    obj.dict = true;
    obj.shape_id = DICT_SHAPE;
    obj.dict_keys_off = keys_off;
    return true;
}

StoreResult AnvilHeap::set_object_prop(
    u32 obj_id,
    u32 prop_sym,
    AnvilValue val,
    u32* out_shape,
    i32* out_slot
) noexcept {
    if (obj_id >= object_count || !objects()[obj_id].active) {
        if (out_slot) {
            *out_slot = -1;
        }
        return StoreResult::NotStored;
    }
    if (objects()[obj_id].dict) {
        if (out_shape) {
            *out_shape = DICT_SHAPE;
        }
        if (out_slot) {
            *out_slot = -1;
        }
        const u32 pc = objects()[obj_id].prop_count;
        u32* keys = object_keys(objects()[obj_id].dict_keys_off);
        AnvilValue* slots = object_slots(objects()[obj_id].slots_off);
        for (u32 i = 0; i < pc; ++i) {
            if (keys[i] == prop_sym) {
                slots[i] = val;
                return StoreResult::Ok;
            }
        }
        if (!ensure_object_slots(obj_id, pc + 1u)) {
            return StoreResult::HeapFull;
        }
        AnvilHeapObject& obj = objects()[obj_id];
        object_keys(obj.dict_keys_off)[pc] = prop_sym;
        object_slots(obj.slots_off)[pc] = val;
        obj.prop_count = pc + 1u;
        return StoreResult::Ok;
    }
    {
        const u32 cur_shape = objects()[obj_id].shape_id;
        const i32 slot = lookup_shape_slot(cur_shape, prop_sym);
        if (slot >= 0 && static_cast<u32>(slot) < objects()[obj_id].slots_cap) {
            object_slots(objects()[obj_id].slots_off)[slot] = val;
            if (out_shape) {
                *out_shape = cur_shape;
            }
            if (out_slot) {
                *out_slot = slot;
            }
            return StoreResult::Ok;
        }
    }
    if (objects()[obj_id].shape_id < shape_count &&
        shapes()[objects()[obj_id].shape_id].prop_count >= DICT_THRESHOLD) {
        if (!convert_object_to_dict(obj_id)) {
            if (out_slot) {
                *out_slot = -1;
            }
            return StoreResult::HeapFull;
        }
        return set_object_prop(obj_id, prop_sym, val, out_shape, out_slot);
    }
    const u32 cur_shape = objects()[obj_id].shape_id;
    const u32 new_shape = transition_shape(cur_shape, prop_sym);
    const i32 slot = lookup_shape_slot(new_shape, prop_sym);
    if (slot < 0) {
        if (out_slot) {
            *out_slot = -1;
        }
        return StoreResult::HeapFull;
    }
    if (!ensure_object_slots(obj_id, static_cast<u32>(slot) + 1u)) {
        if (out_slot) {
            *out_slot = -1;
        }
        return StoreResult::HeapFull;
    }
    AnvilHeapObject& obj = objects()[obj_id];
    obj.shape_id = new_shape;
    obj.prop_count = shapes()[new_shape].prop_count;
    object_slots(obj.slots_off)[slot] = val;
    if (out_shape) {
        *out_shape = new_shape;
    }
    if (out_slot) {
        *out_slot = slot;
    }
    return StoreResult::Ok;
}

AnvilValue AnvilHeap::get_array_elem(u32 arr_id, i32 index) const noexcept {
    if (arr_id >= array_count || !arrays()[arr_id].active || index < 0) {
        return AnvilValue::make_undefined();
    }
    const AnvilHeapArray& arr = arrays()[arr_id];
    const u32 u_idx = static_cast<u32>(index);
    if (u_idx >= arr.length || u_idx >= arr.capacity) {
        return AnvilValue::make_undefined();
    }
    return array_elems(arr.storage_off)[u_idx];
}

StoreResult AnvilHeap::set_array_elem(u32 arr_id, i32 index, AnvilValue val) noexcept {
    if (arr_id >= array_count || !arrays()[arr_id].active || index < 0) {
        return StoreResult::NotStored;
    }
    const u32 u_idx = static_cast<u32>(index);
    if (u_idx >= arrays()[arr_id].capacity) {
        const u32 old_len = arrays()[arr_id].length;
        const u32 old_off = arrays()[arr_id].storage_off;
        const u32 old_cap = arrays()[arr_id].capacity;
        const u32 new_cap = (u_idx + 1u) + ((u_idx + 1u) >> 1u) + 8u;
        bool over = false;
        const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(AnvilValue)), &over);
        if (new_off == NO_SLOT) {
            return StoreResult::HeapFull;
        }
        AnvilValue* dst = array_elems(new_off);
        const AnvilValue* src = array_elems(old_off);
        for (u32 k = 0; k < new_cap; ++k) {
            dst[k] = (k < old_len) ? src[k] : AnvilValue::make_undefined();
        }
        AnvilHeapArray& arr = arrays()[arr_id];
        arr.storage_off = new_off;
        arr.capacity = new_cap;
        arena.free(old_off, old_cap * static_cast<u32>(sizeof(AnvilValue)));
        bytes_allocated += new_cap * static_cast<u32>(sizeof(AnvilValue));
    }
    AnvilHeapArray& arr = arrays()[arr_id];
    array_elems(arr.storage_off)[u_idx] = val;
    if (u_idx >= arr.length) {
        arr.length = u_idx + 1u;
    }
    return StoreResult::Ok;
}

StoreResult AnvilHeap::push_array_elem(u32 arr_id, AnvilValue val) noexcept {
    if (arr_id >= array_count || !arrays()[arr_id].active) {
        return StoreResult::NotStored;
    }
    const i32 idx = static_cast<i32>(arrays()[arr_id].length);
    return set_array_elem(arr_id, idx, val);
}

void AnvilHeap::mark_value(AnvilValue v) noexcept {
    switch (v.tag()) {
        case ValueTag::String: {
            const u32 sym = v.payload();
            if (sym < string_count) {
                strings()[sym].marked = true;
            }
            break;
        }
        case ValueTag::Object:
            mark_object(v.payload());
            break;
        case ValueTag::Array:
            mark_array(v.payload());
            break;
        case ValueTag::Function:
            mark_closure(v.payload());
            break;
        default:
            break;
    }
}

void AnvilHeap::mark_object(u32 obj_id) noexcept {
    if (obj_id >= object_count || !objects()[obj_id].active || objects()[obj_id].marked) {
        return;
    }
    objects()[obj_id].marked = true;
    const u32 count = objects()[obj_id].prop_count;
    const u32 slots_off = objects()[obj_id].slots_off;
    for (u32 i = 0; i < count; ++i) {
        mark_value(object_slots(slots_off)[i]);
    }
    if (objects()[obj_id].dict && objects()[obj_id].dict_keys_off != NO_SLOT) {
        const u32* keys = object_keys(objects()[obj_id].dict_keys_off);
        for (u32 i = 0; i < count; ++i) {
            if (keys[i] < string_count) {
                strings()[keys[i]].marked = true;
            }
        }
    }
    const u8 bkind = objects()[obj_id].builtin_kind;
    const u32 aux = objects()[obj_id].aux_id;
    if (aux != NO_SLOT) {
        if (bkind == static_cast<u8>(BuiltinObjectKind::Map) ||
            bkind == static_cast<u8>(BuiltinObjectKind::Set) ||
            bkind == static_cast<u8>(BuiltinObjectKind::ArrayBuffer)) {
            mark_array(aux);
        } else if (bkind >= static_cast<u8>(BuiltinObjectKind::Uint8Array) &&
                   bkind <= static_cast<u8>(BuiltinObjectKind::Float64Array)) {
            mark_object(aux);
        }
    }
    const u32 proto = objects()[obj_id].proto_obj_id;
    if (proto < object_count) {
        mark_object(proto);
    }
}

void AnvilHeap::mark_array(u32 arr_id) noexcept {
    if (arr_id >= array_count || !arrays()[arr_id].active || arrays()[arr_id].marked) {
        return;
    }
    arrays()[arr_id].marked = true;
    const u32 len = arrays()[arr_id].length;
    const u32 off = arrays()[arr_id].storage_off;
    const AnvilValue* elems = array_elems(off);
    for (u32 i = 0; i < len; ++i) {
        mark_value(elems[i]);
    }
}

void AnvilHeap::mark_closure(u32 closure_id) noexcept {
    if (closure_id >= closure_count || !closures()[closure_id].active ||
        closures()[closure_id].marked) {
        return;
    }
    closures()[closure_id].marked = true;
    const u32 n = closures()[closure_id].upval_count;
    const u32 off = closures()[closure_id].upvals_off;
    if (off != NO_SLOT) {
        const u32* cells = closure_upvals(off);
        for (u32 i = 0u; i < n; ++i) {
            mark_array(cells[i]);
        }
    }
    const u32 proto_id = closures()[closure_id].proto_obj_id;
    if (proto_id != NO_SLOT && proto_id < object_count) {
        mark_object(proto_id);
    }
    const u32 static_id = closures()[closure_id].static_obj_id;
    if (static_id != NO_SLOT && static_id < object_count) {
        mark_object(static_id);
    }
    const u32 super_id = closures()[closure_id].super_closure_id;
    if (super_id != NO_SLOT && super_id < closure_count) {
        mark_closure(super_id);
    }
    const u32 fn_id = closures()[closure_id].fn_id;
    if (fn_id < function_count) {
        functions()[fn_id].marked = true;
        const u32 home_id = functions()[fn_id].home_proto_id;
        if (home_id != NO_SLOT && home_id < object_count) {
            mark_object(home_id);
        }
    }
}

u32 AnvilHeap::sweep_unmarked() noexcept {
    u32 reclaimed = 0u;
    for (u32 i = 0; i < object_count; ++i) {
        AnvilHeapObject& obj = objects()[i];
        if (obj.active) {
            if (!obj.marked) {
                arena.free(obj.slots_off, obj.slots_cap * static_cast<u32>(sizeof(AnvilValue)));
                if (obj.dict && obj.dict_keys_off != NO_SLOT) {
                    arena.free(obj.dict_keys_off, obj.slots_cap * static_cast<u32>(sizeof(u32)));
                }
                obj.active = false;
                obj.dict = false;
                obj.builtin_kind = 0u;
                obj.shape_id = 0u;
                obj.prop_count = 0u;
                obj.slots_off = NO_SLOT;
                obj.slots_cap = 0u;
                obj.dict_keys_off = NO_SLOT;
                obj.aux_id = NO_SLOT;
                obj.byte_offset = 0u;
                obj.byte_length = 0u;
                if (live_objects > 0u) {
                    --live_objects;
                }
                if (bytes_allocated >= sizeof(AnvilHeapObject)) {
                    bytes_allocated -= static_cast<u32>(sizeof(AnvilHeapObject));
                }
                ++reclaimed;
            } else {
                obj.marked = false;
            }
        }
    }
    for (u32 i = 0; i < array_count; ++i) {
        AnvilHeapArray& arr = arrays()[i];
        if (arr.active) {
            if (!arr.marked) {
                arena.free(arr.storage_off, arr.capacity * static_cast<u32>(sizeof(AnvilValue)));
                arr.active = false;
                arr.length = 0u;
                arr.capacity = 0u;
                arr.storage_off = NO_SLOT;
                if (live_arrays > 0u) {
                    --live_arrays;
                }
                if (bytes_allocated >= sizeof(AnvilHeapArray)) {
                    bytes_allocated -= static_cast<u32>(sizeof(AnvilHeapArray));
                }
                ++reclaimed;
            } else {
                arr.marked = false;
            }
        }
    }
    for (u32 i = 0; i < closure_count; ++i) {
        AnvilHeapClosure& c = closures()[i];
        if (c.active && !c.marked && !c.pinned) {
            if (c.upvals_off != NO_SLOT) {
                arena.free(c.upvals_off, c.upval_count * static_cast<u32>(sizeof(u32)));
            }
            c.active = false;
            c.upvals_off = NO_SLOT;
            c.upval_count = 0u;
            c.proto_obj_id = NO_SLOT;
            c.static_obj_id = NO_SLOT;
            c.super_closure_id = NO_SLOT;
            if (live_closures > 0u) {
                --live_closures;
            }
            if (bytes_allocated >= sizeof(AnvilHeapClosure)) {
                bytes_allocated -= static_cast<u32>(sizeof(AnvilHeapClosure));
            }
            ++reclaimed;
        } else {
            c.marked = false;
        }
    }
    ++gc_collections;
    return reclaimed;
}

void AnvilHeap::clear_marks() noexcept {
    for (u32 i = 0; i < object_count; ++i) {
        objects()[i].marked = false;
    }
    for (u32 i = 0; i < array_count; ++i) {
        arrays()[i].marked = false;
    }
    for (u32 i = 0; i < closure_count; ++i) {
        closures()[i].marked = false;
    }
}

u32 AnvilHeap::collect_garbage(const AnvilValue* roots, u32 root_count) noexcept {
    clear_marks();
    if (roots) {
        for (u32 i = 0; i < root_count; ++i) {
            mark_value(roots[i]);
        }
    }
    return sweep_unmarked();
}

} // namespace anvil_vm
