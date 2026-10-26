// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 4-Way Polymorphic Inline Cache (IC) Engine & DOM Selector IC Table.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {
constexpr u32 INIT_IC_SITE_TABLE = 16u;
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
            if (str_len == 6u &&
                name[0] == 'l' && name[1] == 'e' && name[2] == 'n' &&
                name[3] == 'g' && name[4] == 't' && name[5] == 'h') {
                ++ic_hits;
                return AnvilValue::make_smi(static_cast<i32>(heap.arrays()[arr_id].length));
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

    // Store IC miss: transition shape if property is new and record resulting `(shape_id, slot)`.
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
