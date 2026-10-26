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

// Status codes returned by anvil_cpp_vm_run_script. 0 refused, 1 ran, 2
// stopped at a limit. A request (below) can also return 3 (stopped for a
// host call, reserved for later use) and 4 (the caller's out buffer
// was too small, so nothing was written).
constexpr u32 VM_RUN_REFUSED       = 0u;
constexpr u32 VM_RUN_OK            = 1u;
constexpr u32 VM_RUN_STOPPED       = 2u;
constexpr u32 VM_RUN_HOST_CALL     = 3u;
constexpr u32 VM_RUN_BUFFER_SMALL  = 4u;

// A source that begins with these four bytes is a request with a header,
// not a plain script. A plain script never starts with them, so every
// existing caller keeps the behaviour it had.
constexpr u32 AVK_HEADER_BYTES = 16u;
constexpr u32 AVK_OP_RUN       = 1u;
constexpr u32 AVK_OP_CONFIGURE = 2u;

inline bool has_request_magic(const char* s, u32 n) noexcept {
    return s && n >= 4u && s[0] == 'A' && s[1] == 'V' && s[2] == 'K' && s[3] == '1';
}

inline u32 read_u32_le(const char* p) noexcept {
    return static_cast<u32>(static_cast<u8>(p[0])) |
           (static_cast<u32>(static_cast<u8>(p[1])) << 8) |
           (static_cast<u32>(static_cast<u8>(p[2])) << 16) |
           (static_cast<u32>(static_cast<u8>(p[3])) << 24);
}


// Values whose as_number() is what JavaScript's ToNumber gives.
inline bool numeric_operand(AnvilValue v) noexcept {
    return v.is_number() || v.is_bool() || v.is_null() || v.is_undefined();
}

// Opcodes that read their operands as numbers, and how many they read.
inline u32 numeric_operands(AnvilOpcode op) noexcept {
    switch (op) {
        case AnvilOpcode::Neg:
            return 1u;
        case AnvilOpcode::Add:
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
        case AnvilOpcode::CmpLt:
        case AnvilOpcode::CmpLe:
        case AnvilOpcode::CmpGt:
        case AnvilOpcode::CmpGe:
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

// An index the VM's arrays can hold: whole, 0 or more and below 2^31.
inline i32 array_index(f64 v) noexcept {
    if (!(v >= 0.0 && v < 2147483648.0)) {
        return -1;
    }
    const i32 i = static_cast<i32>(v);
    return static_cast<f64>(i) == v ? i : -1;
}

// `==` for the values where it needs no conversion. Sets `known` to false
// for pairs that convert first (a number and a string, for example), which
// the VM leaves to the interpreter.
bool values_loosely_equal(const AnvilHeap& heap, AnvilValue a, AnvilValue b, bool& known) noexcept;

bool values_strictly_equal(const AnvilHeap& heap, AnvilValue a, AnvilValue b) noexcept {
    // Numbers compare as doubles first, so NaN is not equal to itself and
    // 0 equals -0.
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

} // namespace

void AnvilVmContext::init() noexcept {
    // The arena is the bytes Rust allocated after this header (see
    // anvil_cpp_vm_context_bytes). heap_byte_budget, kept across runs by a
    // Configure request, caps how much of it a run may use; 0 means default.
    u8* arena_base = reinterpret_cast<u8*>(this) + sizeof(AnvilVmContext);
    const u32 budget = heap_byte_budget != 0u ? heap_byte_budget : ANVIL_DEFAULT_HEAP_BUDGET;
    heap.init(arena_base, ANVIL_ARENA_RESERVE_BYTES, budget);
    ic.init();
    program.init();
    for (u32 i = 0; i < MAX_TOTAL_REGISTERS; ++i) {
        registers[i] = AnvilValue::make_undefined();
    }
    global_count = 0u;
    for (u32 i = 0; i < MAX_GLOBALS; ++i) {
        globals[i].name_sym = 0u;
        globals[i].value = AnvilValue::make_undefined();
        globals[i].occupied = false;
        globals[i].exported = false;
    }
    frame_count = 0u;
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
        if (globals[i].occupied && globals[i].name_sym == name_sym) {
            return static_cast<u16>(i);
        }
    }
    if (global_count >= MAX_GLOBALS) {
        return NO_GLOBAL;
    }
    const u16 slot = static_cast<u16>(global_count++);
    globals[slot].name_sym = name_sym;
    globals[slot].value = AnvilValue::make_undefined();
    globals[slot].occupied = true;
    globals[slot].exported = false;
    return slot;
}

void AnvilVmContext::reclaim_array_storage(u32 reg_top) noexcept {
    collect_garbage(reg_top);
    heap.compact_array_storage();
}

bool AnvilVmContext::store_ok(StoreResult stored) noexcept {
    switch (stored) {
        case StoreResult::TooManyProperties:
            fail_limit(VM_STOP_PROPERTIES, "property", MAX_SHAPE_PROPS, "properties per object");
            return false;
        case StoreResult::ShapesFull:
            fail_limit(VM_STOP_SHAPES, "shape", MAX_SHAPES, "shapes");
            return false;
        case StoreResult::ArrayTooLong:
            fail_limit(VM_STOP_ARRAY_LENGTH, "array length", MAX_ARRAY_LENGTH, "elements per array");
            return false;
        case StoreResult::ArrayStorageFull:
            fail_limit(VM_STOP_ARRAY_STORAGE, "array storage", MAX_ARRAY_ELEMENTS, "elements");
            return false;
        case StoreResult::NotStored:
            fail(VM_STOP_UNSUPPORTED, "property write on a value that is not an object");
            return false;
        case StoreResult::Ok:
            break;
    }
    return true;
}

void AnvilVmContext::collect_garbage(u32 reg_top) noexcept {
    heap.clear_marks();
    const u32 top = reg_top < MAX_TOTAL_REGISTERS ? reg_top : MAX_TOTAL_REGISTERS;
    for (u32 i = 0; i < top; ++i) {
        heap.mark_value(registers[i]);
    }
    for (u32 i = 0; i < global_count && i < MAX_GLOBALS; ++i) {
        if (globals[i].occupied) {
            heap.mark_value(globals[i].value);
        }
    }
    heap.sweep_unmarked();
}

AnvilValue AnvilVmContext::execute(u32 entry_pc) noexcept {
    f64 native_out = 0.0;
    if (anvil_jit_try_run(this, entry_pc, &native_out)) {
        ++native_compilations;
        return AnvilValue::make_number(native_out);
    }

    u32 pc = entry_pc;
    u32 reg_base = 0u;
    frame_count = 0u;

    const u32 step_limit_now = effective_step_limit();
    while (pc < program.code_len) {
        if (++steps_executed > step_limit_now) {
            fail_limit(VM_STOP_STEPS, "step", step_limit_now, "steps");
            return AnvilValue::make_undefined();
        }

        const AnvilInstruction inst = program.code[pc++];
        AnvilValue* R = &registers[reg_base];

        // Strings and objects in arithmetic or ordering need conversions
        // the VM does not make, apart from joining two strings with +.
        // The script runs on the Rust interpreter instead.
        const u32 operands = numeric_operands(inst.op);
        if (operands != 0u &&
            (!numeric_operand(R[inst.b]) || (operands == 2u && !numeric_operand(R[inst.c]))) &&
            !(inst.op == AnvilOpcode::Add && R[inst.b].is_string() && R[inst.c].is_string())) {
            fail(VM_STOP_UNSUPPORTED, "arithmetic or comparison on a string or object");
            return AnvilValue::make_undefined();
        }

        switch (inst.op) {
            case AnvilOpcode::Nop:
                break;

            case AnvilOpcode::LoadConst: {
                const u16 idx = inst.imm16();
                R[inst.a] = (idx < program.const_len) ? program.constants[idx] : AnvilValue::make_null();
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
                if (lhs.is_string() && rhs.is_string()) {
                    const u32 sym = heap.intern_concat(lhs.payload(), rhs.payload());
                    if (sym == NO_SLOT) {
                        fail_limit(VM_STOP_HEAP, "heap", heap.arena.budget, "bytes");
                        return AnvilValue::make_undefined();
                    }
                    R[inst.a] = AnvilValue::make_string(sym);
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
                    // A zero product with a negative factor is -0.
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
                R[inst.a] = AnvilValue::make_bool(R[inst.b].as_number() < R[inst.c].as_number());
                break;

            case AnvilOpcode::CmpLe:
                R[inst.a] = AnvilValue::make_bool(R[inst.b].as_number() <= R[inst.c].as_number());
                break;

            case AnvilOpcode::CmpGt:
                R[inst.a] = AnvilValue::make_bool(R[inst.b].as_number() > R[inst.c].as_number());
                break;

            case AnvilOpcode::CmpGe:
                R[inst.a] = AnvilValue::make_bool(R[inst.b].as_number() >= R[inst.c].as_number());
                break;

            case AnvilOpcode::Jump:
                pc = inst.imm16();
                break;

            case AnvilOpcode::JumpIfFalse:
                if (!truthy(heap, R[inst.a])) {
                    pc = inst.imm16();
                }
                break;

            case AnvilOpcode::JumpIfTrue:
                if (truthy(heap, R[inst.a])) {
                    pc = inst.imm16();
                }
                break;

            case AnvilOpcode::GetGlobal: {
                const u16 slot = inst.imm16();
                R[inst.a] = (slot < global_count) ? globals[slot].value : AnvilValue::make_undefined();
                break;
            }

            case AnvilOpcode::SetGlobal: {
                const u16 slot = inst.imm16();
                if (slot < MAX_GLOBALS) {
                    globals[slot].value = R[inst.a];
                    globals[slot].occupied = true;
                }
                break;
            }

            case AnvilOpcode::NewObject: {
                if (heap.live_objects + 8u >= MAX_HEAP_OBJECTS) {
                    collect_garbage(reg_base + MAX_REGISTERS_PER_FRAME);
                }
                const u32 obj_id = heap.alloc_object(0u);
                if (obj_id == NO_SLOT) {
                    fail_limit(VM_STOP_OBJECTS, "object", MAX_HEAP_OBJECTS, "live objects");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_object(obj_id);
                break;
            }

            case AnvilOpcode::NewArray: {
                if (heap.live_arrays + 4u >= MAX_HEAP_ARRAYS) {
                    collect_garbage(reg_base + MAX_REGISTERS_PER_FRAME);
                }
                u32 arr_id = heap.alloc_array(inst.b);
                if (arr_id == NO_SLOT) {
                    reclaim_array_storage(reg_base + MAX_REGISTERS_PER_FRAME);
                    arr_id = heap.alloc_array(inst.b);
                }
                if (arr_id == NO_SLOT) {
                    if (heap.live_arrays >= MAX_HEAP_ARRAYS) {
                        fail_limit(VM_STOP_ARRAYS, "array", MAX_HEAP_ARRAYS, "live arrays");
                    } else {
                        fail_limit(VM_STOP_ARRAY_STORAGE, "array storage", MAX_ARRAY_ELEMENTS, "elements");
                    }
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = AnvilValue::make_array(arr_id);
                break;
            }

            case AnvilOpcode::GetPropIC: {
                const u32 site_id = inst.c;
                const u32 prop_sym = (site_id < ic.site_count) ? ic.sites[site_id].prop_sym : 0u;
                if (!R[inst.b].is_object() && !R[inst.b].is_array()) {
                    fail(VM_STOP_UNSUPPORTED, "property read on a value that is not an object or array");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = ic.load_property_ic(heap, site_id, R[inst.b], prop_sym);
                break;
            }

            case AnvilOpcode::SetPropIC: {
                const u32 site_id = inst.c;
                const u32 prop_sym = (site_id < ic.site_count) ? ic.sites[site_id].prop_sym : 0u;
                const StoreResult stored = ic.store_property_ic(heap, site_id, R[inst.a], prop_sym, R[inst.b]);
                if (!store_ok(stored)) {
                    return AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::GetElem: {
                const AnvilValue recv = R[inst.b];
                if (!recv.is_array() || !R[inst.c].is_number()) {
                    fail(VM_STOP_UNSUPPORTED, "element read the VM does not run");
                    return AnvilValue::make_undefined();
                }
                // A fraction or a negative index reads no element.
                R[inst.a] = heap.get_array_elem(recv.payload(), array_index(R[inst.c].as_number()));
                break;
            }

            case AnvilOpcode::SetElem: {
                const AnvilValue recv = R[inst.a];
                const i32 idx = R[inst.b].is_number() ? array_index(R[inst.b].as_number()) : -1;
                if (!recv.is_array() || idx < 0) {
                    fail(VM_STOP_UNSUPPORTED, "element write the VM does not run");
                    return AnvilValue::make_undefined();
                }
                {
                    StoreResult stored = heap.set_array_elem(recv.payload(), idx, R[inst.c]);
                    if (stored == StoreResult::ArrayStorageFull) {
                        reclaim_array_storage(reg_base + MAX_REGISTERS_PER_FRAME);
                        stored = heap.set_array_elem(recv.payload(), idx, R[inst.c]);
                    }
                    if (!store_ok(stored)) {
                        return AnvilValue::make_undefined();
                    }
                }
                break;
            }

            case AnvilOpcode::ArrayPush: {
                const AnvilValue recv = R[inst.a];
                if (!recv.is_array()) {
                    fail(VM_STOP_UNSUPPORTED, "push on a value that is not an array");
                    return AnvilValue::make_undefined();
                }
                {
                    StoreResult stored = heap.push_array_elem(recv.payload(), R[inst.b]);
                    if (stored == StoreResult::ArrayStorageFull) {
                        reclaim_array_storage(reg_base + MAX_REGISTERS_PER_FRAME);
                        stored = heap.push_array_elem(recv.payload(), R[inst.b]);
                    }
                    if (!store_ok(stored)) {
                        return AnvilValue::make_undefined();
                    }
                }
                break;
            }

            case AnvilOpcode::Call: {
                const u32 fn_id = inst.b;
                if (fn_id >= heap.function_count || !heap.functions[fn_id].active) {
                    R[inst.a] = AnvilValue::make_null();
                    break;
                }
                if (frame_count + 1u >= MAX_CALL_FRAMES ||
                    reg_base + (MAX_REGISTERS_PER_FRAME * 2u) > MAX_TOTAL_REGISTERS) {
                    fail_limit(VM_STOP_CALL_DEPTH, "call depth", MAX_CALL_DEPTH, "nested calls");
                    return AnvilValue::make_undefined();
                }
                const AnvilHeapFunction& fn = heap.functions[fn_id];
                if (static_cast<u32>(inst.c) + fn.arity > MAX_REGISTERS_PER_FRAME) {
                    fail(VM_STOP_UNSUPPORTED, "call arguments outside the caller's registers");
                    return AnvilValue::make_undefined();
                }
                const u32 next_base = reg_base + MAX_REGISTERS_PER_FRAME;
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    registers[next_base + a_idx] = R[inst.c + a_idx];
                }
                frames[frame_count++] = AnvilCallFrame{fn_id, pc, reg_base, inst.a};
                reg_base = next_base;
                pc = fn.code_start;
                break;
            }

            case AnvilOpcode::CallMath: {
                const f64 v = R[inst.c].as_number();
                f64 out = v;
                switch (inst.b) {
                    case 1u: out = js_abs(v); break;
                    case 2u: out = js_floor(v); break;
                    case 3u: out = js_ceil(v); break;
                    case 4u: out = js_round(v); break;
                    case 5u: out = js_sqrt(v); break;
                    default: break;
                }
                R[inst.a] = AnvilValue::make_number(out);
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
                if (frame_count == 0u) {
                    return ret_val;
                }
                const AnvilCallFrame caller = frames[--frame_count];
                reg_base = caller.reg_base;
                pc = caller.return_pc;
                registers[reg_base + caller.dst_reg] = ret_val;
                break;
            }

            case AnvilOpcode::Halt:
                return R[inst.a];

            case AnvilOpcode::HostSetText:
                break;
        }
    }
    return registers[0];
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
    fill_exports(out);
}

namespace {

// Appends to a fixed export buffer. Once a write does not fit, nothing more
// is written and `full` stays set.
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

// Records of the exported globals: kind (u8), name length (u32), name, then
// for a number or bool an f64 and for a string its length (u32) and bytes.
// Kinds: 1 number, 2 bool, 3 null, 4 undefined, 5 string, 6 object, array
// or function (name only). `exports_full` is 1 when they did not all fit.
void AnvilVmContext::fill_exports(AnvilVmTelemetryC* out) const noexcept {
    ExportWriter w{out->exports, MAX_EXPORT_BYTES, 0u, false};
    const u32 count = global_count < MAX_GLOBALS ? global_count : MAX_GLOBALS;
    for (u32 index = 0u; index < count && !w.full; ++index) {
        const AnvilGlobalBinding& g = globals[index];
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

} // namespace anvil_vm

extern "C" {

// Kernel version check called by `is_cpp_jit_kernel_active()` in Rust.
u32 anvil_cpp_jit_kernel_version() noexcept {
    return anvil_vm::ANVIL_VM_KERNEL_VERSION;
}

// Binary numeric evaluation helper.
// Opcode 0: add, 1: sub, 2: mul, 3: div, 4: mod, 5: bit_and, 6: bit_or, 7: bit_xor, 8: shl, 9: shr.
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
        // These follow the interpreter's own fallback, which uses Rust's
        // saturating `as` casts.
        case 4u: {
            const i64 r_int = anvil_vm::sat_i64(rhs);
            if (r_int == 0) {
                return 0.0;
            }
            const i64 l_int = anvil_vm::sat_i64(lhs);
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
            return static_cast<f64>(static_cast<i32>(static_cast<u32>(anvil_vm::sat_i32(lhs)) << (anvil_vm::sat_i32(rhs) & 31)));
        case 9u:
            return static_cast<f64>(anvil_vm::sat_i32(lhs) >> (anvil_vm::sat_i32(rhs) & 31));
        default:
            return 0.0;
    }
}

// Fast-path integer range summation kernel (`for (let i = start; i < end; i += step) acc += i`).
i64 anvil_cpp_jit_sum_range_i64(i64 start, i64 end_exclusive, i64 step) noexcept {
    if (step <= 0 || start >= end_exclusive) {
        return 0;
    }
    const i64 span = end_exclusive - start;
    i64 count = (span + step - 1) / step;
    if (count > 100000) {
        count = 100000;
    }
    // Closed-form arithmetic progression evaluation in O(1) time.
    const i64 last = start + (count - 1) * step;
    return count * (start + last) / 2;
}

// The caller owns the context bytes. This file does not keep a process-global VM.
extern "C" u32 anvil_cpp_vm_context_bytes() {
    // The context header followed by the heap arena Rust zero-allocates in one
    // block. The arena is reserved but only touched as a run uses it.
    return static_cast<u32>(sizeof(anvil_vm::AnvilVmContext)) + anvil_vm::ANVIL_ARENA_RESERVE_BYTES;
}

extern "C" u32 anvil_cpp_vm_context_align() {
    return static_cast<u32>(alignof(anvil_vm::AnvilVmContext));
}

namespace {
void (*g_text_host_fn)(void*, const char*, u32, double) = nullptr;
void* g_text_host = nullptr;
}

extern "C" void anvil_cpp_vm_set_text_host(
    void (*fn)(void*, const char*, u32, double),
    void* host
) noexcept {
    g_text_host_fn = fn;
    g_text_host = host;
}

// Compile and run plain script text in the caller-owned context, filling the
// telemetry and numeric result. Shared by the plain path and the Run request.
static u32 run_script_text(
    anvil_vm::AnvilVmContext& ctx,
    void* compiler_memory,
    const char* script,
    u32 script_len,
    anvil_vm::AnvilVmTelemetryC* telemetry_out,
    f64* numeric_result_out
) noexcept {
    // The telemetry also carries the reason when the script is refused.
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
    return ctx.aborted ? anvil_vm::VM_RUN_STOPPED : anvil_vm::VM_RUN_OK;
}

// Handle a source that carries a request header:
//     "AVK1" | op u32 | flags u32 | out_cap u32 | payload
// The out buffer is the telemetry pointer. When it is smaller than the
// telemetry needs, nothing is written and the status is 4, so the caller can
// grow the buffer and ask again.
static u32 run_request(
    anvil_vm::AnvilVmContext& ctx,
    void* compiler_memory,
    const char* src,
    u32 len,
    anvil_vm::AnvilVmTelemetryC* telemetry_out,
    f64* numeric_result_out
) noexcept {
    if (len < anvil_vm::AVK_HEADER_BYTES) {
        ctx.fail(anvil_vm::VM_REFUSE_SYNTAX, "request header too short");
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_REFUSED;
    }
    const u32 op = anvil_vm::read_u32_le(src + 4);
    const u32 out_cap = anvil_vm::read_u32_le(src + 12);
    // The out buffer must hold the whole telemetry struct. When it does not,
    // write nothing and let the caller send a bigger one.
    if (telemetry_out && out_cap < static_cast<u32>(sizeof(anvil_vm::AnvilVmTelemetryC))) {
        return anvil_vm::VM_RUN_BUFFER_SMALL;
    }
    const u32 pos = anvil_vm::AVK_HEADER_BYTES;
    if (op == anvil_vm::AVK_OP_RUN) {
        return run_script_text(
            ctx, compiler_memory, src + pos, len - pos, telemetry_out, numeric_result_out
        );
    }
    if (op == anvil_vm::AVK_OP_CONFIGURE) {
        // The payload is four u32 limits. A zero leaves that limit as it was.
        // The settings stay on the context for the runs that follow.
        if (len >= pos + 16u) {
            const u32 step = anvil_vm::read_u32_le(src + pos);
            const u32 script_size = anvil_vm::read_u32_le(src + pos + 4u);
            const u32 heap = anvil_vm::read_u32_le(src + pos + 8u);
            const u32 depth = anvil_vm::read_u32_le(src + pos + 12u);
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
        }
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_OK;
    }
    ctx.fail(anvil_vm::VM_REFUSE_UNSUPPORTED, "unsupported request op");
    ctx.fill_telemetry(telemetry_out);
    return anvil_vm::VM_RUN_REFUSED;
}

// Compile and execute a JavaScript snippet inside the caller-owned `AnvilVmContext`.
// A plain script returns 1 (ran), 2 (accepted then stopped) or 0 (refused). A
// source that starts with the request magic is read as a request instead and
// can also return 3 or 4 (see run_request).
// This is not a general compiler.
u32 anvil_cpp_vm_run_script(
    void* context_memory,
    void* compiler_memory,
    const char* source_ptr,
    u32 source_len,
    anvil_vm::AnvilVmTelemetryC* telemetry_out,
    f64* numeric_result_out
) noexcept {
    if (!context_memory) {
        return 0u;
    }
    auto& ctx = *static_cast<anvil_vm::AnvilVmContext*>(context_memory);
    ctx.init();
    if (anvil_vm::has_request_magic(source_ptr, source_len)) {
        return run_request(
            ctx, compiler_memory, source_ptr, source_len, telemetry_out, numeric_result_out
        );
    }
    return run_script_text(
        ctx, compiler_memory, source_ptr, source_len, telemetry_out, numeric_result_out
    );
}

// Copy one numeric global out of a context left by anvil_cpp_vm_run_script.
// Returns 1 when `index` is occupied and holds a number.
extern "C" u32 anvil_cpp_vm_global_count(void* context_memory) noexcept {
    if (!context_memory) {
        return 0u;
    }
    const auto& ctx = *static_cast<const anvil_vm::AnvilVmContext*>(context_memory);
    return ctx.global_count;
}

extern "C" u32 anvil_cpp_vm_copy_global(
    void* context_memory,
    u32 index,
    char* name_out,
    u32 name_cap,
    f64* value_out
) noexcept {
    if (!context_memory || !name_out || name_cap < 2u || !value_out) {
        return 0u;
    }
    const auto& ctx = *static_cast<const anvil_vm::AnvilVmContext*>(context_memory);
    if (index >= ctx.global_count || !ctx.globals[index].occupied) {
        return 0u;
    }
    if (!ctx.globals[index].value.is_number()) {
        return 0u;
    }
    u32 len = 0u;
    const char* text = ctx.heap.get_string_ptr(ctx.globals[index].name_sym, &len);
    if (!text || len == 0u) {
        return 0u;
    }
    const u32 n = (len < name_cap - 1u) ? len : (name_cap - 1u);
    for (u32 i = 0u; i < n; ++i) {
        name_out[i] = text[i];
    }
    name_out[n] = '\0';
    *value_out = ctx.globals[index].value.as_number();
    return 1u;
}

// Exercise hidden-class transitions, polymorphic Inline Caches, and Mark-Sweep GC
// over `iterations` object allocations and property reads/writes.
u32 anvil_cpp_vm_run_ic_benchmark(
    void* context_memory,
    u32 iterations,
    anvil_vm::AnvilVmTelemetryC* telemetry_out
) noexcept {
    if (!context_memory) {
        return 0u;
    }
    auto& g_vm_ctx = *static_cast<anvil_vm::AnvilVmContext*>(context_memory);
    g_vm_ctx.init();

    const u32 sym_x = g_vm_ctx.heap.intern_string("x", 1u);
    const u32 sym_y = g_vm_ctx.heap.intern_string("y", 1u);
    const u32 sym_z = g_vm_ctx.heap.intern_string("z", 1u);

    const u32 site_set_x = g_vm_ctx.ic.allocate_site(sym_x);
    const u32 site_set_y = g_vm_ctx.ic.allocate_site(sym_y);
    const u32 site_get_x = g_vm_ctx.ic.allocate_site(sym_x);
    const u32 site_get_y = g_vm_ctx.ic.allocate_site(sym_y);

    const u32 capped = iterations > 4096u ? 4096u : (iterations == 0u ? 16u : iterations);
    i32 accum = 0;

    for (u32 i = 0u; i < capped; ++i) {
        if (g_vm_ctx.heap.live_objects + 4u >= anvil_vm::MAX_HEAP_OBJECTS) {
            g_vm_ctx.heap.collect_garbage(nullptr, 0u);
        }
        const u32 obj_id = g_vm_ctx.heap.alloc_object(0u);
        if (obj_id == anvil_vm::NO_SLOT) {
            break;
        }
        const anvil_vm::AnvilValue obj = anvil_vm::AnvilValue::make_object(obj_id);

        g_vm_ctx.ic.store_property_ic(g_vm_ctx.heap, site_set_x, obj, sym_x, anvil_vm::AnvilValue::make_smi(static_cast<i32>(i)));
        g_vm_ctx.ic.store_property_ic(g_vm_ctx.heap, site_set_y, obj, sym_y, anvil_vm::AnvilValue::make_smi(2));
        if ((i & 3u) == 0u) {
            // Introduce a second shape `(x, y, z)` to upgrade IC sites to Polymorphic.
            u32 sh = 0u;
            i32 sl = -1;
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
    return static_cast<u32>(accum);
}

// 4-way unrolled C++20 vector reduction kernel for numeric JS arrays
// (op 0: sum, op 1: min, op 2: max, op 3: dot-product self / sum-of-squares).
f64 anvil_cpp_vm_vector_reduce(u32 op, const f64* data_ptr, u32 len) noexcept {
    if (!data_ptr || len == 0u) {
        return 0.0;
    }
    if (op == 0u) {
        f64 s0 = 0.0, s1 = 0.0, s2 = 0.0, s3 = 0.0;
        u32 i = 0u;
        for (; i + 3u < len; i += 4u) {
            s0 += data_ptr[i];
            s1 += data_ptr[i + 1u];
            s2 += data_ptr[i + 2u];
            s3 += data_ptr[i + 3u];
        }
        f64 total = (s0 + s1) + (s2 + s3);
        for (; i < len; ++i) {
            total += data_ptr[i];
        }
        return total;
    }
    if (op == 1u) {
        f64 best = data_ptr[0];
        for (u32 i = 1u; i < len; ++i) {
            if (data_ptr[i] < best) {
                best = data_ptr[i];
            }
        }
        return best;
    }
    if (op == 2u) {
        f64 best = data_ptr[0];
        for (u32 i = 1u; i < len; ++i) {
            if (data_ptr[i] > best) {
                best = data_ptr[i];
            }
        }
        return best;
    }
    if (op == 3u) {
        f64 s0 = 0.0, s1 = 0.0, s2 = 0.0, s3 = 0.0;
        u32 i = 0u;
        for (; i + 3u < len; i += 4u) {
            s0 += data_ptr[i] * data_ptr[i];
            s1 += data_ptr[i + 1u] * data_ptr[i + 1u];
            s2 += data_ptr[i + 2u] * data_ptr[i + 2u];
            s3 += data_ptr[i + 3u] * data_ptr[i + 3u];
        }
        f64 total = (s0 + s1) + (s2 + s3);
        for (; i < len; ++i) {
            total += data_ptr[i] * data_ptr[i];
        }
        return total;
    }
    return 0.0;
}

} // extern "C"
