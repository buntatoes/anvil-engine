// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 4-Way Polymorphic Inline Cache (IC) Engine & DOM Selector IC Table.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {
constexpr u32 INIT_IC_SITE_TABLE = 16u;

inline bool prop_is(const char* name, u32 len, const char* lit, u32 lit_len) noexcept {
    if (len != lit_len) {
        return false;
    }
    for (u32 i = 0u; i < len; ++i) {
        if (name[i] != lit[i]) {
            return false;
        }
    }
    return true;
}

inline u32 utf16_unit_length(const char* s, u32 byte_len) noexcept {
    u32 units = 0u;
    u32 i = 0u;
    while (i < byte_len) {
        const u8 b = static_cast<u8>(s[i]);
        if ((b & 0x80u) == 0u) {
            i += 1u;
            units += 1u;
        } else if ((b & 0xE0u) == 0xC0u) {
            i += 2u;
            units += 1u;
        } else if ((b & 0xF0u) == 0xE0u) {
            i += 3u;
            units += 1u;
        } else if ((b & 0xF8u) == 0xF0u) {
            i += 4u;
            units += 2u;
        } else {
            i += 1u;
            units += 1u;
        }
    }
    return units;
}

} // namespace

bool AnvilIcTable::ensure_sites(u32 need) noexcept {
    if (need <= sites_cap) {
        return true;
    }
    u32 new_cap = sites_cap == 0u ? INIT_IC_SITE_TABLE : sites_cap;
    while (new_cap < need) {
        new_cap *= 2u;
    }
    bool over = false;
    const u32 new_off = arena->alloc(new_cap * static_cast<u32>(sizeof(AnvilIcSite)), &over);
    if (new_off == NO_SLOT) {
        return false;
    }
    AnvilIcSite* dst = reinterpret_cast<AnvilIcSite*>(arena->at(new_off));
    const AnvilIcSite* src = sites();
    for (u32 i = 0u; i < site_count; ++i) {
        dst[i] = src[i];
    }
    const u32 old_off = sites_off;
    const u32 old_cap = sites_cap;
    sites_off = new_off;
    sites_cap = new_cap;
    if (old_cap != 0u) {
        arena->free(old_off, old_cap * static_cast<u32>(sizeof(AnvilIcSite)));
    }
    return true;
}

void AnvilIcTable::init(AnvilArena* a) noexcept {
    arena = a;
    sites_off = 0u;
    sites_cap = 0u;
    site_count = 0u;
    ic_hits = 0u;
    ic_misses = 0u;
    polymorphic_upgrades = 0u;
    megamorphic_fallbacks = 0u;
    ensure_sites(INIT_IC_SITE_TABLE);

    dom_ic_hits = 0u;
    dom_ic_misses = 0u;
    for (u32 i = 0; i < MAX_DOM_IC_ENTRIES; ++i) {
        dom_entries[i].selector_hash = 0ULL;
        dom_entries[i].node_id = 0u;
        dom_entries[i].hit_count = 0u;
        dom_entries[i].occupied = false;
    }
}

u32 AnvilIcTable::allocate_site(u32 prop_sym) noexcept {
    if (!ensure_sites(site_count + 1u)) {
        return NO_SLOT;
    }
    const u32 id = site_count++;
    AnvilIcSite& st = sites()[id];
    st.state = IcState::Uninitialized;
    st.count = 0u;
    st.prop_sym = prop_sym;
    for (u32 s = 0; s < 4u; ++s) {
        st.stubs[s].shape_id = 0xFFFFFFFFu;
        st.stubs[s].slot_offset = -1;
    }
    return id;
}

AnvilValue AnvilIcTable::load_property_ic(
    AnvilHeap& heap,
    u32 site_id,
    AnvilValue receiver,
    u32 prop_sym
) noexcept {
    // Array `.length` fast-path check.
    if (receiver.is_array()) {
        const u32 arr_id = receiver.payload();
        if (arr_id < heap.array_count && heap.arrays()[arr_id].active) {
            u32 str_len = 0u;
            const char* name = heap.get_string_ptr(prop_sym, &str_len);
            if (prop_is(name, str_len, "length", 6u)) {
                ++ic_hits;
                return AnvilValue::make_smi(static_cast<i32>(heap.arrays()[arr_id].length));
            }
        }
        return AnvilValue::make_undefined();
    }

    // String `.length` (in UTF-16 code units).
    if (receiver.is_string()) {
        u32 str_len = 0u;
        const char* name = heap.get_string_ptr(prop_sym, &str_len);
        if (prop_is(name, str_len, "length", 6u)) {
            u32 s_bytes = 0u;
            const char* s_ptr = heap.get_string_ptr(receiver.payload(), &s_bytes);
            ++ic_hits;
            return AnvilValue::make_smi(static_cast<i32>(utf16_unit_length(s_ptr, s_bytes)));
        }
        return AnvilValue::make_undefined();
    }

    // Function / Class static properties and `.prototype` / `.name` / `.length`.
    if (receiver.is_function()) {
        const u32 cid = receiver.payload();
        if (cid < heap.closure_count && heap.closures()[cid].active) {
            u32 str_len = 0u;
            const char* name = heap.get_string_ptr(prop_sym, &str_len);
            if (prop_is(name, str_len, "prototype", 9u) &&
                heap.closures()[cid].proto_obj_id != NO_SLOT) {
                return AnvilValue::make_object(heap.closures()[cid].proto_obj_id);
            }
            u32 cur_cid = cid;
            u32 hops = 0u;
            while (cur_cid != NO_SLOT && cur_cid < heap.closure_count &&
                   heap.closures()[cur_cid].active && hops < 16u) {
                const u32 st_id = heap.closures()[cur_cid].static_obj_id;
                if (st_id != NO_SLOT && st_id < heap.object_count &&
                    heap.objects()[st_id].active && heap.has_object_prop(st_id, prop_sym)) {
                    return heap.get_object_prop(st_id, prop_sym);
                }
                cur_cid = heap.closures()[cur_cid].super_closure_id;
                ++hops;
            }
            const u32 fn_id = heap.closures()[cid].fn_id;
            if (fn_id < heap.function_count && heap.functions()[fn_id].active) {
                if (prop_is(name, str_len, "length", 6u)) {
                    return AnvilValue::make_smi(static_cast<i32>(heap.functions()[fn_id].arity));
                }
                if (prop_is(name, str_len, "name", 4u)) {
                    return AnvilValue::make_string(heap.functions()[fn_id].name_sym);
                }
            }
        }
        return AnvilValue::make_undefined();
    }

    if (!receiver.is_object()) {
        return AnvilValue::make_undefined();
    }
    const u32 obj_id = receiver.payload();
    if (obj_id >= heap.object_count || !heap.objects()[obj_id].active) {
        return AnvilValue::make_undefined();
    }

    const AnvilHeapObject& obj = heap.objects()[obj_id];

    // Builtin object virtual properties (`size`, `byteLength`, `byteOffset`, `buffer`, `length`).
    if (obj.builtin_kind != static_cast<u8>(BuiltinObjectKind::Plain) &&
        obj.builtin_kind != static_cast<u8>(BuiltinObjectKind::Error)) {
        u32 str_len = 0u;
        const char* name = heap.get_string_ptr(prop_sym, &str_len);
        const auto bkind = static_cast<BuiltinObjectKind>(obj.builtin_kind);
        if ((bkind == BuiltinObjectKind::Map || bkind == BuiltinObjectKind::Set) &&
            prop_is(name, str_len, "size", 4u)) {
            return AnvilValue::make_smi(static_cast<i32>(obj.byte_length));
        }
        if (bkind == BuiltinObjectKind::ArrayBuffer &&
            prop_is(name, str_len, "byteLength", 10u)) {
            return AnvilValue::make_number(static_cast<f64>(obj.byte_length));
        }
        if (bkind >= BuiltinObjectKind::Uint8Array && bkind <= BuiltinObjectKind::Float64Array) {
            if (prop_is(name, str_len, "byteLength", 10u)) {
                return AnvilValue::make_number(static_cast<f64>(obj.byte_length));
            }
            if (prop_is(name, str_len, "byteOffset", 10u)) {
                return AnvilValue::make_number(static_cast<f64>(obj.byte_offset));
            }
            if (prop_is(name, str_len, "buffer", 6u)) {
                return AnvilValue::make_object(obj.aux_id);
            }
            if (prop_is(name, str_len, "length", 6u)) {
                u32 shift = 0u;
                if (bkind == BuiltinObjectKind::Uint16Array) {
                    shift = 1u;
                } else if (bkind == BuiltinObjectKind::Int32Array ||
                           bkind == BuiltinObjectKind::Uint32Array) {
                    shift = 2u;
                } else if (bkind == BuiltinObjectKind::Float64Array) {
                    shift = 3u;
                }
                return AnvilValue::make_number(static_cast<f64>(obj.byte_length >> shift));
            }
        }
    }

    const u32 shape_id = obj.shape_id;

    if (site_id < site_count) {
        AnvilIcSite& site = sites()[site_id];
        if (site.state == IcState::Monomorphic) {
            if (site.stubs[0].shape_id == shape_id && site.stubs[0].slot_offset >= 0) {
                ++ic_hits;
                return heap.object_slots(obj.slots_off)[site.stubs[0].slot_offset];
            }
        } else if (site.state == IcState::Polymorphic) {
            for (u8 i = 0u; i < site.count; ++i) {
                if (site.stubs[i].shape_id == shape_id && site.stubs[i].slot_offset >= 0) {
                    ++ic_hits;
                    return heap.object_slots(obj.slots_off)[site.stubs[i].slot_offset];
                }
            }
        }
    }

    // Inline Cache miss: consult hidden class (`AnvilShape`) and update stub table.
    ++ic_misses;
    u32 resolved_shape = 0u;
    i32 resolved_slot = -1;
    const AnvilValue val = heap.get_object_prop(obj_id, prop_sym, &resolved_shape, &resolved_slot);

    if (site_id < site_count && resolved_slot >= 0) {
        AnvilIcSite& site = sites()[site_id];
        if (site.state == IcState::Uninitialized) {
            site.state = IcState::Monomorphic;
            site.count = 1u;
            site.stubs[0].shape_id = resolved_shape;
            site.stubs[0].slot_offset = resolved_slot;
        } else if (site.state == IcState::Monomorphic) {
            site.state = IcState::Polymorphic;
            site.stubs[1].shape_id = resolved_shape;
            site.stubs[1].slot_offset = resolved_slot;
            site.count = 2u;
            ++polymorphic_upgrades;
        } else if (site.state == IcState::Polymorphic) {
            if (site.count < 4u) {
                site.stubs[site.count].shape_id = resolved_shape;
                site.stubs[site.count].slot_offset = resolved_slot;
                ++site.count;
            } else {
                site.state = IcState::Megamorphic;
                ++megamorphic_fallbacks;
            }
        } else {
            ++megamorphic_fallbacks;
        }
    }
    return val;
}

StoreResult AnvilIcTable::store_property_ic(
    AnvilHeap& heap,
    u32 site_id,
    AnvilValue receiver,
    u32 prop_sym,
    AnvilValue val
) noexcept {
    if (receiver.is_function()) {
        const u32 cid = receiver.payload();
        if (cid >= heap.closure_count || !heap.closures()[cid].active) {
            return StoreResult::NotStored;
        }
        u32 st_id = heap.closures()[cid].static_obj_id;
        if (st_id == NO_SLOT) {
            st_id = heap.alloc_object(0u);
            if (st_id == NO_SLOT) {
                return StoreResult::HeapFull;
            }
            heap.closures()[cid].static_obj_id = st_id;
        }
        return heap.set_object_prop(st_id, prop_sym, val);
    }

    if (!receiver.is_object()) {
        return StoreResult::NotStored;
    }
    const u32 obj_id = receiver.payload();
    if (obj_id >= heap.object_count || !heap.objects()[obj_id].active) {
        return StoreResult::NotStored;
    }

    AnvilHeapObject& obj = heap.objects()[obj_id];
    const u32 shape_id = obj.shape_id;

    if (site_id < site_count) {
        AnvilIcSite& site = sites()[site_id];
        if (site.state == IcState::Monomorphic) {
            if (site.stubs[0].shape_id == shape_id && site.stubs[0].slot_offset >= 0) {
                ++ic_hits;
                heap.object_slots(obj.slots_off)[site.stubs[0].slot_offset] = val;
                return StoreResult::Ok;
            }
        } else if (site.state == IcState::Polymorphic) {
            for (u8 i = 0u; i < site.count; ++i) {
                if (site.stubs[i].shape_id == shape_id && site.stubs[i].slot_offset >= 0) {
                    ++ic_hits;
                    heap.object_slots(obj.slots_off)[site.stubs[i].slot_offset] = val;
                    return StoreResult::Ok;
                }
            }
        }
    }

    ++ic_misses;
    u32 new_shape = 0u;
    i32 slot = -1;
    const StoreResult stored = heap.set_object_prop(obj_id, prop_sym, val, &new_shape, &slot);
    if (stored == StoreResult::Ok && site_id < site_count && slot >= 0) {
        AnvilIcSite& site = sites()[site_id];
        if (site.state == IcState::Uninitialized) {
            site.state = IcState::Monomorphic;
            site.count = 1u;
            site.stubs[0].shape_id = new_shape;
            site.stubs[0].slot_offset = slot;
        } else if (site.state == IcState::Monomorphic) {
            site.state = IcState::Polymorphic;
            site.stubs[1].shape_id = new_shape;
            site.stubs[1].slot_offset = slot;
            site.count = 2u;
            ++polymorphic_upgrades;
        } else if (site.state == IcState::Polymorphic) {
            if (site.count < 4u) {
                site.stubs[site.count].shape_id = new_shape;
                site.stubs[site.count].slot_offset = slot;
                ++site.count;
            } else {
                site.state = IcState::Megamorphic;
                ++megamorphic_fallbacks;
            }
        } else {
            ++megamorphic_fallbacks;
        }
    }
    return stored;
}

i32 AnvilIcTable::lookup_dom_selector(const char* selector, u32 len) noexcept {
    if (!selector || len == 0u) {
        return -1;
    }
    const u64 h = fnv1a_hash_bytes(selector, len);
    const u32 start = static_cast<u32>(h % MAX_DOM_IC_ENTRIES);
    for (u32 probe = 0u; probe < 8u; ++probe) {
        const u32 idx = (start + probe) % MAX_DOM_IC_ENTRIES;
        if (!dom_entries[idx].occupied) {
            ++dom_ic_misses;
            return -1;
        }
        if (dom_entries[idx].selector_hash == h) {
            ++dom_entries[idx].hit_count;
            ++dom_ic_hits;
            return static_cast<i32>(dom_entries[idx].node_id);
        }
    }
    ++dom_ic_misses;
    return -1;
}

void AnvilIcTable::record_dom_selector(const char* selector, u32 len, u32 node_id) noexcept {
    if (!selector || len == 0u) {
        return;
    }
    const u64 h = fnv1a_hash_bytes(selector, len);
    const u32 start = static_cast<u32>(h % MAX_DOM_IC_ENTRIES);
    for (u32 probe = 0u; probe < 8u; ++probe) {
        const u32 idx = (start + probe) % MAX_DOM_IC_ENTRIES;
        if (!dom_entries[idx].occupied || dom_entries[idx].selector_hash == h) {
            dom_entries[idx].selector_hash = h;
            dom_entries[idx].node_id = node_id;
            dom_entries[idx].hit_count = 1u;
            dom_entries[idx].occupied = true;
            return;
        }
    }
    dom_entries[start].selector_hash = h;
    dom_entries[start].node_id = node_id;
    dom_entries[start].hit_count = 1u;
    dom_entries[start].occupied = true;
}

void AnvilIcTable::invalidate_dom_cache() noexcept {
    for (u32 i = 0; i < MAX_DOM_IC_ENTRIES; ++i) {
        dom_entries[i].occupied = false;
    }
}

} // namespace anvil_vm
