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

inline f64 fast_sqrt(f64 x) noexcept {
    if (x <= 0.0) {
        return 0.0;
    }
    f64 g = x > 1.0 ? x * 0.5 : 1.0;
    for (int i = 0; i < 10; ++i) {
        g = 0.5 * (g + x / g);
    }
    return g;
}

inline f64 fast_floor(f64 x) noexcept {
    const i64 i = static_cast<i64>(x);
    const f64 d = static_cast<f64>(i);
    return (x < d) ? (d - 1.0) : d;
}

inline f64 fast_ceil(f64 x) noexcept {
    const i64 i = static_cast<i64>(x);
    const f64 d = static_cast<f64>(i);
    return (x > d) ? (d + 1.0) : d;
}

inline f64 fast_round(f64 x) noexcept {
    return x >= 0.0 ? fast_floor(x + 0.5) : fast_ceil(x - 0.5);
}

bool values_strictly_equal(const AnvilHeap& heap, AnvilValue a, AnvilValue b) noexcept {
    if (a.bits == b.bits) {
        return true;
    }
    if (a.is_number() && b.is_number()) {
        return a.as_number() == b.as_number();
    }
    if (a.is_string() && b.is_string()) {
        return heap.string_equals(a.payload(), b.payload());
    }
    return false;
}

} // namespace

void AnvilVmContext::init() noexcept {
    heap.init();
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
        case StoreResult::Ok:
        case StoreResult::NotStored:
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

    while (pc < program.code_len) {
        if (++steps_executed > MAX_VM_STEPS) {
            fail_limit(VM_STOP_STEPS, "step", MAX_VM_STEPS, "steps");
            return AnvilValue::make_undefined();
        }

        const AnvilInstruction inst = program.code[pc++];
        AnvilValue* R = &registers[reg_base];

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
                        fail_limit(VM_STOP_STRINGS, "string pool", MAX_STRING_BYTES, "bytes");
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
                    if (prod >= -1073741824LL && prod <= 1073741823LL) {
                        R[inst.a] = AnvilValue::make_smi(static_cast<i32>(prod));
                        break;
                    }
                }
                R[inst.a] = AnvilValue::make_number(lhs.as_number() * rhs.as_number());
                break;
            }

            case AnvilOpcode::Div: {
                const f64 denom = R[inst.c].as_number();
                R[inst.a] = AnvilValue::make_number(denom == 0.0 ? 0.0 : (R[inst.b].as_number() / denom));
                break;
            }

            case AnvilOpcode::Mod: {
                const i64 r_int = static_cast<i64>(R[inst.c].as_number());
                if (r_int == 0) {
                    R[inst.a] = AnvilValue::make_smi(0);
                } else {
                    const i64 l_int = static_cast<i64>(R[inst.b].as_number());
                    R[inst.a] = AnvilValue::make_number(static_cast<f64>(l_int % r_int));
                }
                break;
            }

            case AnvilOpcode::Neg:
                R[inst.a] = AnvilValue::make_number(-R[inst.b].as_number());
                break;

            case AnvilOpcode::Not:
                R[inst.a] = AnvilValue::make_bool(!R[inst.b].is_truthy());
                break;

            case AnvilOpcode::BitAnd:
                R[inst.a] = AnvilValue::make_smi(
                    static_cast<i32>(R[inst.b].as_number()) & static_cast<i32>(R[inst.c].as_number())
                );
                break;

            case AnvilOpcode::BitOr:
                R[inst.a] = AnvilValue::make_smi(
                    static_cast<i32>(R[inst.b].as_number()) | static_cast<i32>(R[inst.c].as_number())
                );
                break;

            case AnvilOpcode::BitXor:
                R[inst.a] = AnvilValue::make_smi(
                    static_cast<i32>(R[inst.b].as_number()) ^ static_cast<i32>(R[inst.c].as_number())
                );
                break;

            case AnvilOpcode::Shl: {
                const i32 s = static_cast<i32>(R[inst.c].as_number()) & 31;
                R[inst.a] = AnvilValue::make_smi(static_cast<i32>(R[inst.b].as_number()) << s);
                break;
            }

            case AnvilOpcode::Shr: {
                const i32 s = static_cast<i32>(R[inst.c].as_number()) & 31;
                R[inst.a] = AnvilValue::make_smi(static_cast<i32>(R[inst.b].as_number()) >> s);
                break;
            }

            case AnvilOpcode::UShr: {
                const u32 s = static_cast<u32>(R[inst.c].as_number()) & 31u;
                const u32 u = static_cast<u32>(static_cast<i32>(R[inst.b].as_number())) >> s;
                R[inst.a] = AnvilValue::make_number(static_cast<f64>(u));
                break;
            }

            case AnvilOpcode::CmpEq:
                R[inst.a] = AnvilValue::make_bool(values_strictly_equal(heap, R[inst.b], R[inst.c]));
                break;

            case AnvilOpcode::CmpNe:
                R[inst.a] = AnvilValue::make_bool(!values_strictly_equal(heap, R[inst.b], R[inst.c]));
                break;

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
                if (!R[inst.a].is_truthy()) {
                    pc = inst.imm16();
                }
                break;

            case AnvilOpcode::JumpIfTrue:
                if (R[inst.a].is_truthy()) {
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
                const i32 idx = static_cast<i32>(R[inst.c].as_number());
                if (recv.is_array()) {
                    R[inst.a] = heap.get_array_elem(recv.payload(), idx);
                } else {
                    R[inst.a] = AnvilValue::make_undefined();
                }
                break;
            }

            case AnvilOpcode::SetElem: {
                const AnvilValue recv = R[inst.a];
                const i32 idx = static_cast<i32>(R[inst.b].as_number());
                if (recv.is_array()) {
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
                if (recv.is_array()) {
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
                    case 1u: out = v < 0.0 ? -v : v; break;
                    case 2u: out = fast_floor(v); break;
                    case 3u: out = fast_ceil(v); break;
                    case 4u: out = fast_round(v); break;
                    case 5u: out = fast_sqrt(v); break;
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
        case 4u: {
            if (rhs == 0.0) {
                return 0.0;
            }
            const i64 l_int = static_cast<i64>(lhs);
            const i64 r_int = static_cast<i64>(rhs);
            return r_int == 0 ? 0.0 : static_cast<f64>(l_int % r_int);
        }
        case 5u:
            return static_cast<f64>(static_cast<i32>(lhs) & static_cast<i32>(rhs));
        case 6u:
            return static_cast<f64>(static_cast<i32>(lhs) | static_cast<i32>(rhs));
        case 7u:
            return static_cast<f64>(static_cast<i32>(lhs) ^ static_cast<i32>(rhs));
        case 8u:
            return static_cast<f64>(static_cast<i32>(lhs) << (static_cast<i32>(rhs) & 31));
        case 9u:
            return static_cast<f64>(static_cast<i32>(lhs) >> (static_cast<i32>(rhs) & 31));
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
    return static_cast<u32>(sizeof(anvil_vm::AnvilVmContext));
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

// Compile and execute a JavaScript snippet inside the caller-owned `AnvilVmContext`.
// Returns 1 when the numeric result is valid, 2 when the compiler accepted the
// script and execution aborted, and 0 when the compiler rejected it.
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
    // The telemetry also carries the reason when the script is refused.
    if (!compiler_memory || !source_ptr || source_len == 0u) {
        ctx.fail(anvil_vm::VM_REFUSE_SYNTAX, "empty script");
        ctx.fill_telemetry(telemetry_out);
        return 0u;
    }
    ctx.host_set_text = g_text_host_fn;
    ctx.host = g_text_host;
    if (!anvil_vm::compile_js_to_bytecode(ctx, compiler_memory, source_ptr, source_len)) {
        ctx.fill_telemetry(telemetry_out);
        return 0u;
    }
    const anvil_vm::AnvilValue res = ctx.execute(0u);
    if (telemetry_out) {
        ctx.fill_telemetry(telemetry_out);
    }
    if (numeric_result_out) {
        *numeric_result_out = res.as_number();
    }
    if (ctx.aborted) {
        return 2u;
    }
    return 1u;
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
