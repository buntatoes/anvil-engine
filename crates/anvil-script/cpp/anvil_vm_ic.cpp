// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 4-Way Polymorphic Inline Cache (IC) Engine & DOM Selector IC Table.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

void AnvilIcTable::init() noexcept {
    site_count = 0u;
    ic_hits = 0u;
    ic_misses = 0u;
    polymorphic_upgrades = 0u;
    megamorphic_fallbacks = 0u;
    for (u32 i = 0; i < MAX_IC_SITES; ++i) {
        sites[i].state = IcState::Uninitialized;
        sites[i].count = 0u;
        sites[i].prop_sym = 0u;
        for (u32 s = 0; s < 4u; ++s) {
            sites[i].stubs[s].shape_id = 0xFFFFFFFFu;
            sites[i].stubs[s].slot_offset = -1;
        }
    }

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
    if (site_count >= MAX_IC_SITES) {
        return MAX_IC_SITES - 1u;
    }
    const u32 id = site_count++;
    sites[id].state = IcState::Uninitialized;
    sites[id].count = 0u;
    sites[id].prop_sym = prop_sym;
    for (u32 s = 0; s < 4u; ++s) {
        sites[id].stubs[s].shape_id = 0xFFFFFFFFu;
        sites[id].stubs[s].slot_offset = -1;
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
        if (arr_id < MAX_HEAP_ARRAYS && heap.arrays[arr_id].active) {
            u32 str_len = 0u;
            const char* name = heap.get_string_ptr(prop_sym, &str_len);
            if (str_len == 6u &&
                name[0] == 'l' && name[1] == 'e' && name[2] == 'n' &&
                name[3] == 'g' && name[4] == 't' && name[5] == 'h') {
                ++ic_hits;
                return AnvilValue::make_smi(static_cast<i32>(heap.arrays[arr_id].length));
            }
        }
        return AnvilValue::make_undefined();
    }

    if (!receiver.is_object()) {
        return AnvilValue::make_undefined();
    }
    const u32 obj_id = receiver.payload();
    if (obj_id >= MAX_HEAP_OBJECTS || !heap.objects[obj_id].active) {
        return AnvilValue::make_undefined();
    }

    const AnvilHeapObject& obj = heap.objects[obj_id];
    const u32 shape_id = obj.shape_id;

    if (site_id < site_count) {
        AnvilIcSite& site = sites[site_id];
        if (site.state == IcState::Monomorphic) {
            if (site.stubs[0].shape_id == shape_id && site.stubs[0].slot_offset >= 0) {
                ++ic_hits;
                return obj.slots[site.stubs[0].slot_offset];
            }
        } else if (site.state == IcState::Polymorphic) {
            for (u8 i = 0u; i < site.count; ++i) {
                if (site.stubs[i].shape_id == shape_id && site.stubs[i].slot_offset >= 0) {
                    ++ic_hits;
                    return obj.slots[site.stubs[i].slot_offset];
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
        AnvilIcSite& site = sites[site_id];
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

bool AnvilIcTable::store_property_ic(
    AnvilHeap& heap,
    u32 site_id,
    AnvilValue receiver,
    u32 prop_sym,
    AnvilValue val
) noexcept {
    if (!receiver.is_object()) {
        return false;
    }
    const u32 obj_id = receiver.payload();
    if (obj_id >= MAX_HEAP_OBJECTS || !heap.objects[obj_id].active) {
        return false;
    }

    AnvilHeapObject& obj = heap.objects[obj_id];
    const u32 shape_id = obj.shape_id;

    if (site_id < site_count) {
        AnvilIcSite& site = sites[site_id];
        if (site.state == IcState::Monomorphic) {
            if (site.stubs[0].shape_id == shape_id && site.stubs[0].slot_offset >= 0) {
                ++ic_hits;
                obj.slots[site.stubs[0].slot_offset] = val;
                return true;
            }
        } else if (site.state == IcState::Polymorphic) {
            for (u8 i = 0u; i < site.count; ++i) {
                if (site.stubs[i].shape_id == shape_id && site.stubs[i].slot_offset >= 0) {
                    ++ic_hits;
                    obj.slots[site.stubs[i].slot_offset] = val;
                    return true;
                }
            }
        }
    }

    // Store IC miss: transition shape if property is new and record resulting `(shape_id, slot)`.
    ++ic_misses;
    u32 new_shape = 0u;
    i32 slot = -1;
    const bool ok = heap.set_object_prop(obj_id, prop_sym, val, &new_shape, &slot);
    if (ok && site_id < site_count && slot >= 0) {
        AnvilIcSite& site = sites[site_id];
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
    return ok;
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
