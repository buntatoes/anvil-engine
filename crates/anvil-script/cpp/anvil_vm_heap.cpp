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

void AnvilHeap::init() noexcept {
    string_bytes_used = 0u;
    string_count = 0u;
    for (u32 i = 0; i < MAX_INTERNED_STRINGS; ++i) {
        strings[i].hash = 0ULL;
        strings[i].offset = 0u;
        strings[i].length = 0u;
        strings[i].marked = false;
        strings[i].active = false;
    }

    // Shape 0 is the empty root hidden class.
    shape_count = 1u;
    shapes[0].id = 0u;
    shapes[0].parent_shape = 0u;
    shapes[0].prop_count = 0u;
    for (u32 j = 0; j < MAX_SHAPE_PROPS; ++j) {
        shapes[0].prop_syms[j] = 0u;
    }
    for (u32 i = 1; i < MAX_SHAPES; ++i) {
        shapes[i].id = i;
        shapes[i].parent_shape = 0u;
        shapes[i].prop_count = 0u;
    }

    transition_count = 0u;
    for (u32 i = 0; i < MAX_SHAPE_TRANSITIONS; ++i) {
        transitions[i].from_shape = 0u;
        transitions[i].prop_sym = 0u;
        transitions[i].to_shape = 0u;
    }

    for (u32 i = 0; i < MAX_HEAP_OBJECTS; ++i) {
        objects[i].active = false;
        objects[i].marked = false;
        objects[i].shape_id = 0u;
        objects[i].proto_obj_id = 0xFFFFFFFFu;
        for (u32 s = 0; s < MAX_SHAPE_PROPS; ++s) {
            objects[i].slots[s] = AnvilValue::make_undefined();
        }
    }

    array_storage_used = 0u;
    for (u32 i = 0; i < MAX_ARRAY_ELEMENTS; ++i) {
        array_storage[i] = AnvilValue::make_undefined();
    }
    for (u32 i = 0; i < MAX_HEAP_ARRAYS; ++i) {
        arrays[i].active = false;
        arrays[i].marked = false;
        arrays[i].length = 0u;
        arrays[i].start_index = 0u;
        arrays[i].capacity = 0u;
    }

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
        if (strings[i].active && strings[i].hash == h && strings[i].length == len) {
            if (len == 0u || bytes_equal(&string_bytes[strings[i].offset], str, len)) {
                return i;
            }
        }
    }
    if (string_count >= MAX_INTERNED_STRINGS || string_bytes_used + len > MAX_STRING_BYTES) {
        return 0u;
    }
    const u32 sym = string_count++;
    const u32 off = string_bytes_used;
    for (u32 i = 0; i < len; ++i) {
        string_bytes[off + i] = str[i];
    }
    string_bytes_used += len;
    strings[sym].hash = h;
    strings[sym].offset = off;
    strings[sym].length = len;
    strings[sym].marked = true;
    strings[sym].active = true;
    bytes_allocated += sizeof(AnvilInternedString) + len;
    return sym;
}

const char* AnvilHeap::get_string_ptr(u32 sym, u32* len_out) const noexcept {
    if (sym >= string_count || !strings[sym].active) {
        if (len_out) {
            *len_out = 0u;
        }
        return "";
    }
    if (len_out) {
        *len_out = strings[sym].length;
    }
    return &string_bytes[strings[sym].offset];
}

bool AnvilHeap::string_equals(u32 sym_a, u32 sym_b) const noexcept {
    if (sym_a == sym_b) {
        return true;
    }
    if (sym_a >= string_count || sym_b >= string_count) {
        return false;
    }
    const AnvilInternedString& a = strings[sym_a];
    const AnvilInternedString& b = strings[sym_b];
    if (a.hash != b.hash || a.length != b.length) {
        return false;
    }
    return bytes_equal(&string_bytes[a.offset], &string_bytes[b.offset], a.length);
}

i32 AnvilHeap::lookup_shape_slot(u32 shape_id, u32 prop_sym) const noexcept {
    if (shape_id >= shape_count) {
        return -1;
    }
    return shapes[shape_id].find_slot(prop_sym);
}

u32 AnvilHeap::transition_shape(u32 current_shape, u32 prop_sym) noexcept {
    if (current_shape >= shape_count) {
        return 0u;
    }
    // Check if property already exists in current shape.
    if (shapes[current_shape].find_slot(prop_sym) >= 0) {
        return current_shape;
    }
    // Check existing transition edge (current_shape, prop_sym) -> to_shape.
    for (u32 i = 0; i < transition_count; ++i) {
        if (transitions[i].from_shape == current_shape && transitions[i].prop_sym == prop_sym) {
            return transitions[i].to_shape;
        }
    }
    // Allocate a new hidden class with all properties of `current_shape` + `prop_sym`.
    const AnvilShape& parent = shapes[current_shape];
    if (parent.prop_count >= MAX_SHAPE_PROPS || shape_count >= MAX_SHAPES) {
        return current_shape;
    }
    const u32 next_id = shape_count++;
    AnvilShape& child = shapes[next_id];
    child.id = next_id;
    child.parent_shape = current_shape;
    child.prop_count = parent.prop_count + 1u;
    for (u32 i = 0; i < parent.prop_count; ++i) {
        child.prop_syms[i] = parent.prop_syms[i];
    }
    child.prop_syms[parent.prop_count] = prop_sym;

    if (transition_count < MAX_SHAPE_TRANSITIONS) {
        transitions[transition_count].from_shape = current_shape;
        transitions[transition_count].prop_sym = prop_sym;
        transitions[transition_count].to_shape = next_id;
        ++transition_count;
    }
    ++shape_transitions_count;
    bytes_allocated += sizeof(AnvilShape);
    return next_id;
}

u32 AnvilHeap::alloc_object(u32 initial_shape) noexcept {
    for (u32 i = 0; i < MAX_HEAP_OBJECTS; ++i) {
        if (!objects[i].active) {
            objects[i].active = true;
            objects[i].marked = false;
            objects[i].shape_id = initial_shape < shape_count ? initial_shape : 0u;
            objects[i].proto_obj_id = 0xFFFFFFFFu;
            for (u32 s = 0; s < MAX_SHAPE_PROPS; ++s) {
                objects[i].slots[s] = AnvilValue::make_undefined();
            }
            ++live_objects;
            bytes_allocated += sizeof(AnvilHeapObject);
            return i;
        }
    }
    return 0u;
}

u32 AnvilHeap::alloc_array(u32 initial_capacity) noexcept {
    const u32 cap = initial_capacity < 4u ? 4u : (initial_capacity > 128u ? 128u : initial_capacity);
    if (array_storage_used + cap > MAX_ARRAY_ELEMENTS) {
        return 0u;
    }
    for (u32 i = 0; i < MAX_HEAP_ARRAYS; ++i) {
        if (!arrays[i].active) {
            arrays[i].active = true;
            arrays[i].marked = false;
            arrays[i].length = 0u;
            arrays[i].start_index = array_storage_used;
            arrays[i].capacity = cap;
            for (u32 k = 0; k < cap; ++k) {
                array_storage[array_storage_used + k] = AnvilValue::make_undefined();
            }
            array_storage_used += cap;
            ++live_arrays;
            bytes_allocated += sizeof(AnvilHeapArray) + cap * sizeof(AnvilValue);
            return i;
        }
    }
    return 0u;
}

u32 AnvilHeap::alloc_function(
    u32 name_sym,
    u32 arity,
    u32 local_count,
    u32 code_start,
    u32 code_len
) noexcept {
    if (function_count >= MAX_FUNCTIONS) {
        return 0u;
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

AnvilValue AnvilHeap::get_object_prop(
    u32 obj_id,
    u32 prop_sym,
    u32* out_shape,
    i32* out_slot
) const noexcept {
    if (obj_id >= MAX_HEAP_OBJECTS || !objects[obj_id].active) {
        if (out_slot) {
            *out_slot = -1;
        }
        return AnvilValue::make_undefined();
    }
    const AnvilHeapObject& obj = objects[obj_id];
    if (out_shape) {
        *out_shape = obj.shape_id;
    }
    const i32 slot = lookup_shape_slot(obj.shape_id, prop_sym);
    if (out_slot) {
        *out_slot = slot;
    }
    if (slot >= 0 && static_cast<u32>(slot) < MAX_SHAPE_PROPS) {
        return obj.slots[slot];
    }
    // Check prototype chain (up to 4 hops to avoid cycles).
    u32 proto = obj.proto_obj_id;
    u32 hops = 0u;
    while (proto < MAX_HEAP_OBJECTS && objects[proto].active && hops < 4u) {
        const i32 pslot = lookup_shape_slot(objects[proto].shape_id, prop_sym);
        if (pslot >= 0 && static_cast<u32>(pslot) < MAX_SHAPE_PROPS) {
            return objects[proto].slots[pslot];
        }
        proto = objects[proto].proto_obj_id;
        ++hops;
    }
    return AnvilValue::make_undefined();
}

bool AnvilHeap::set_object_prop(
    u32 obj_id,
    u32 prop_sym,
    AnvilValue val,
    u32* out_shape,
    i32* out_slot
) noexcept {
    if (obj_id >= MAX_HEAP_OBJECTS || !objects[obj_id].active) {
        if (out_slot) {
            *out_slot = -1;
        }
        return false;
    }
    AnvilHeapObject& obj = objects[obj_id];
    i32 slot = lookup_shape_slot(obj.shape_id, prop_sym);
    if (slot < 0) {
        const u32 new_shape = transition_shape(obj.shape_id, prop_sym);
        obj.shape_id = new_shape;
        slot = lookup_shape_slot(new_shape, prop_sym);
    }
    if (out_shape) {
        *out_shape = obj.shape_id;
    }
    if (out_slot) {
        *out_slot = slot;
    }
    if (slot >= 0 && static_cast<u32>(slot) < MAX_SHAPE_PROPS) {
        obj.slots[slot] = val;
        return true;
    }
    return false;
}

AnvilValue AnvilHeap::get_array_elem(u32 arr_id, i32 index) const noexcept {
    if (arr_id >= MAX_HEAP_ARRAYS || !arrays[arr_id].active || index < 0) {
        return AnvilValue::make_undefined();
    }
    const AnvilHeapArray& arr = arrays[arr_id];
    const u32 u_idx = static_cast<u32>(index);
    if (u_idx >= arr.length || arr.start_index + u_idx >= MAX_ARRAY_ELEMENTS) {
        return AnvilValue::make_undefined();
    }
    return array_storage[arr.start_index + u_idx];
}

bool AnvilHeap::set_array_elem(u32 arr_id, i32 index, AnvilValue val) noexcept {
    if (arr_id >= MAX_HEAP_ARRAYS || !arrays[arr_id].active || index < 0) {
        return false;
    }
    AnvilHeapArray& arr = arrays[arr_id];
    const u32 u_idx = static_cast<u32>(index);
    if (u_idx >= arr.capacity) {
        // Reallocate contiguous slice if capacity exceeded and storage permits.
        const u32 new_cap = (u_idx + 8u) < 256u ? (u_idx + 8u) : 256u;
        if (u_idx >= new_cap || array_storage_used + new_cap > MAX_ARRAY_ELEMENTS) {
            return false;
        }
        const u32 new_start = array_storage_used;
        for (u32 k = 0; k < new_cap; ++k) {
            array_storage[new_start + k] =
                (k < arr.length) ? array_storage[arr.start_index + k] : AnvilValue::make_undefined();
        }
        arr.start_index = new_start;
        arr.capacity = new_cap;
        array_storage_used += new_cap;
    }
    array_storage[arr.start_index + u_idx] = val;
    if (u_idx >= arr.length) {
        arr.length = u_idx + 1u;
    }
    return true;
}

bool AnvilHeap::push_array_elem(u32 arr_id, AnvilValue val) noexcept {
    if (arr_id >= MAX_HEAP_ARRAYS || !arrays[arr_id].active) {
        return false;
    }
    const i32 idx = static_cast<i32>(arrays[arr_id].length);
    return set_array_elem(arr_id, idx, val);
}

void AnvilHeap::mark_value(AnvilValue v) noexcept {
    switch (v.tag()) {
        case ValueTag::String: {
            const u32 sym = v.payload();
            if (sym < string_count) {
                strings[sym].marked = true;
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
    if (obj_id >= MAX_HEAP_OBJECTS || !objects[obj_id].active || objects[obj_id].marked) {
        return;
    }
    AnvilHeapObject& obj = objects[obj_id];
    obj.marked = true;
    const u32 count = obj.shape_id < shape_count ? shapes[obj.shape_id].prop_count : 0u;
    for (u32 i = 0; i < count; ++i) {
        mark_value(obj.slots[i]);
    }
    if (obj.proto_obj_id < MAX_HEAP_OBJECTS) {
        mark_object(obj.proto_obj_id);
    }
}

void AnvilHeap::mark_array(u32 arr_id) noexcept {
    if (arr_id >= MAX_HEAP_ARRAYS || !arrays[arr_id].active || arrays[arr_id].marked) {
        return;
    }
    AnvilHeapArray& arr = arrays[arr_id];
    arr.marked = true;
    for (u32 i = 0; i < arr.length; ++i) {
        if (arr.start_index + i < MAX_ARRAY_ELEMENTS) {
            mark_value(array_storage[arr.start_index + i]);
        }
    }
}

u32 AnvilHeap::sweep_unmarked() noexcept {
    u32 reclaimed = 0u;
    for (u32 i = 0; i < MAX_HEAP_OBJECTS; ++i) {
        if (objects[i].active) {
            if (!objects[i].marked) {
                objects[i].active = false;
                objects[i].shape_id = 0u;
                if (live_objects > 0u) {
                    --live_objects;
                }
                if (bytes_allocated >= sizeof(AnvilHeapObject)) {
                    bytes_allocated -= sizeof(AnvilHeapObject);
                }
                ++reclaimed;
            } else {
                objects[i].marked = false;
            }
        }
    }
    for (u32 i = 0; i < MAX_HEAP_ARRAYS; ++i) {
        if (arrays[i].active) {
            if (!arrays[i].marked) {
                arrays[i].active = false;
                arrays[i].length = 0u;
                if (live_arrays > 0u) {
                    --live_arrays;
                }
                if (bytes_allocated >= sizeof(AnvilHeapArray)) {
                    bytes_allocated -= sizeof(AnvilHeapArray);
                }
                ++reclaimed;
            } else {
                arrays[i].marked = false;
            }
        }
    }
    ++gc_collections;
    return reclaimed;
}

u32 AnvilHeap::collect_garbage(const AnvilValue* roots, u32 root_count) noexcept {
    for (u32 i = 0; i < MAX_HEAP_OBJECTS; ++i) {
        objects[i].marked = false;
    }
    for (u32 i = 0; i < MAX_HEAP_ARRAYS; ++i) {
        arrays[i].marked = false;
    }
    if (roots) {
        for (u32 i = 0; i < root_count; ++i) {
            mark_value(roots[i]);
        }
    }
    return sweep_unmarked();
}

} // namespace anvil_vm
