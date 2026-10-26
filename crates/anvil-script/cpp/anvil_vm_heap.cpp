// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 Hidden Classes (AnvilShape), String Interning Pool, and Mark-Sweep GC Heap.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {

inline bool bytes_equal(const char* a, const char* b, u32 len) noexcept {
    for (u32 i = 0; i < len; ++i) {
        if (a[i] != b[i]) {
            return false;
        }
    }
    return true;
}

} // namespace

namespace {
constexpr u32 INIT_STR_POOL_BYTES = 1024u;
constexpr u32 INIT_STR_TABLE = 64u;
constexpr u32 INIT_ARRAY_TABLE = 16u;
constexpr u32 INIT_SHAPE_TABLE = 16u;
constexpr u32 INIT_TRANSITION_TABLE = 16u;
constexpr u32 INIT_OBJECT_TABLE = 16u;
constexpr u32 INIT_OBJECT_SLOTS = 4u;
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
    // Reserve the first blocks for the string pool and table. On a budget too
    // small for even these, the regions stay empty and interning fails with a
    // heap-limit stop, which is what a run on such a budget should get.
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
    // Shape 0 is the empty root hidden class.
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

    function_count = 0u;
    for (u32 i = 0; i < MAX_FUNCTIONS; ++i) {
        functions[i].active = false;
        functions[i].marked = false;
        functions[i].name_sym = 0u;
        functions[i].arity = 0u;
        functions[i].local_count = 0u;
        functions[i].code_start = 0u;
        functions[i].code_len = 0u;
    }

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
    // Grow the table and the byte pool first. When `str` points into the pool
    // (the intern_concat path) the caller has already reserved the bytes, so
    // neither grow fires here and `str` stays valid across the copy below.
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
    // Reserve room for the joined bytes first, so the pool does not move again
    // while the parts are copied in. Both parts live below string_bytes_used,
    // so building the result above it does not overwrite them. intern_string
    // keeps the bytes in place when the result is new and leaves them unused
    // when it is not.
    if (!ensure_string_bytes(string_bytes_used + len_a + len_b)) {
        return NO_SLOT;
    }
    const u32 off = string_bytes_used;
    char* pool = string_bytes();
    const char* ptr_a = get_string_ptr(sym_a, nullptr);
    for (u32 i = 0u; i < len_a; ++i) {
        pool[off + i] = ptr_a[i];
    }
    const char* ptr_b = get_string_ptr(sym_b, nullptr);
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
    // A shape never holds more than the dictionary threshold; past that the
    // caller puts the object into dictionary mode instead of transitioning.
    if (shapes()[current_shape].prop_count >= DICT_THRESHOLD) {
        return current_shape;
    }
    // Copy the parent's keys out before growing, since a grow moves the table.
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
    obj.shape_id = initial_shape < shape_count ? initial_shape : 0u;
    obj.proto_obj_id = 0xFFFFFFFFu;
    obj.prop_count = obj.shape_id < shape_count ? shapes()[obj.shape_id].prop_count : 0u;
    obj.slots_off = slots_off;
    obj.slots_cap = INIT_OBJECT_SLOTS;
    obj.dict_keys_off = NO_SLOT;
    ++live_objects;
    bytes_allocated += static_cast<u32>(sizeof(AnvilHeapObject));
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
    // Reuse a swept table slot, or grow the table for a new one.
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
u32 AnvilHeap::alloc_function(
    u32 name_sym,
    u32 arity,
    u32 local_count,
    u32 code_start,
    u32 code_len
) noexcept {
    if (function_count >= MAX_FUNCTIONS) {
        return NO_SLOT;
    }
    const u32 id = function_count++;
    functions[id].active = true;
    functions[id].marked = true;
    functions[id].name_sym = name_sym;
    functions[id].arity = arity;
    functions[id].local_count = local_count;
    functions[id].code_start = code_start;
    functions[id].code_len = code_len;
    bytes_allocated += sizeof(AnvilHeapFunction);
    return id;
}

// Find a property on one object, in whichever mode it is in. Returns the value
// and true when present. A dictionary object scans its own key map; a shape
// object consults its hidden class.
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
    // A dictionary object is never cached: its slot layout is its own, so a
    // stub keyed on its (sentinel) shape would mislead other objects.
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
    // Check prototype chain (up to 4 hops to avoid cycles).
    u32 proto = obj.proto_obj_id;
    u32 hops = 0u;
    while (proto < object_count && objects()[proto].active && hops < 4u) {
        if (find_own_prop(*this, objects()[proto], prop_sym, &found)) {
            return found;
        }
        proto = objects()[proto].proto_obj_id;
        ++hops;
    }
    return AnvilValue::make_undefined();
}

// Grow an object's slot block (and, in dictionary mode, its key block) to hold
// at least `need` entries. Both blocks grow together so key i always lines up
// with slot i. False on heap limit.
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

// Switch an object to dictionary mode: give it its own key map, seeded from the
// shared shape's property order, and drop the shared shape. False on heap limit.
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
    // Dictionary mode: find in, or append to, the object's own key map. Never
    // cached, so the slot is reported as -1.
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
    // Shape mode: an existing property writes in place.
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
    // A new property. At the threshold, switch to dictionary mode and retry;
    // otherwise transition to a child shape and write the new slot.
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
        // Grow this array into a fresh, larger block and free the old one.
        // HeapFull when the arena cannot carry the growth within the budget.
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
        case ValueTag::Function: {
            const u32 fn_id = v.payload();
            if (fn_id < function_count) {
                functions[fn_id].marked = true;
            }
            break;
        }
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
                obj.shape_id = 0u;
                obj.prop_count = 0u;
                obj.slots_off = NO_SLOT;
                obj.slots_cap = 0u;
                obj.dict_keys_off = NO_SLOT;
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
