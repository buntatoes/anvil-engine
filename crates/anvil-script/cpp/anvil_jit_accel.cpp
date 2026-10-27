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
// stopped at a limit. A request (below) can also return 3 (paused at a yield,
// so the caller can resume it, and reused later for host calls) and 4 (the
// caller's out buffer was too small, so nothing was written).
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
    // The arena is the bytes Rust allocated after this header (see
    // anvil_cpp_vm_context_bytes). heap_byte_budget, kept across runs by a
    // Configure request, caps how much of it a run may use; 0 means default.
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
    paused = false;
    resume_pc = 0u;
    resume_reg_base = 0u;
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
    g.decl_depth = 0xFFFFFFFFu; // NO_DEPTH
    return slot;
}

void AnvilVmContext::reclaim_array_storage(u32 reg_top) noexcept {
    // Each array owns its element block now, so a collection frees the blocks
    // of unreachable arrays straight to the arena free list; no compaction.
    collect_garbage(reg_top);
}

bool AnvilVmContext::store_ok(StoreResult stored) noexcept {
    switch (stored) {
        case StoreResult::TooManyProperties:
        case StoreResult::ShapesFull:
        case StoreResult::ArrayTooLong:
        case StoreResult::ArrayStorageFull:
        case StoreResult::HeapFull:
            // Objects, shapes and arrays all grow in the arena now, so every one
            // of these only arises when the arena hits the byte budget.
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
    heap.sweep_unmarked();
}

AnvilValue AnvilVmContext::execute(u32 entry_pc) noexcept {
    // The numeric JIT runs a whole program in one call and cannot pause, so it
    // is only used when no yield is asked for.
    if (yield_interval == 0u) {
        f64 native_out = 0.0;
        if (anvil_jit_try_run(this, entry_pc, &native_out)) {
            ++native_compilations;
            return AnvilValue::make_number(native_out);
        }
    }

    frame_count = 0u;
    const u32 top_regs = program.top_frame_regs != 0u ? program.top_frame_regs : 1u;
    return run_loop(entry_pc, 0u, top_regs, NO_SLOT);
}

AnvilValue AnvilVmContext::resume() noexcept {
    paused = false;
    // A host call returns undefined to the guest; a plain yield resumes with
    // the register file untouched.
    if (pending_host_call) {
        pending_host_call = false;
        const u32 dst = resume_reg_base + resume_host_dst;
        if (dst < registers_cap) {
            registers()[dst] = AnvilValue::make_undefined();
        }
    }
    return run_loop(resume_pc, resume_reg_base, resume_frame_regs, resume_closure);
}

AnvilValue AnvilVmContext::run_loop(u32 pc, u32 reg_base, u32 frame_regs, u32 cur_closure_in) noexcept {
    const u32 step_limit_now = effective_step_limit();
    // Registers the running function owns: the base advances by this on a call
    // and back on a return, so frames take only the space each function needs.
    u32 cur_frame_regs = frame_regs;
    u32 cur_closure = cur_closure_in;
    u32 slice = 0u;
    while (pc < program.code_len) {
        // Pause at a yield boundary: save where to continue and hand control
        // back. The frame stack, registers and step count all stay on the
        // context, so resume() picks up with no native stack in use.
        if (yield_interval != 0u && slice >= yield_interval) {
            paused = true;
            resume_pc = pc;
            resume_reg_base = reg_base;
            resume_frame_regs = cur_frame_regs;
            resume_closure = cur_closure;
            return AnvilValue::make_undefined();
        }
        ++slice;
        if (++steps_executed > step_limit_now) {
            fail_limit(VM_STOP_STEPS, "step", step_limit_now, "steps");
            return AnvilValue::make_undefined();
        }

        const AnvilInstruction inst = program.code()[pc++];
        AnvilValue* R = &registers()[reg_base];

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
                u32 obj_id = heap.alloc_object(0u);
                if (obj_id == NO_SLOT) {
                    // Reclaim dead objects, then try once more.
                    collect_garbage(reg_base + cur_frame_regs);
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
                u32 arr_id = heap.alloc_array(inst.b);
                if (arr_id == NO_SLOT) {
                    // Reclaim dead arrays and old slices, then try once more.
                    reclaim_array_storage(reg_base + cur_frame_regs);
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
                if (!R[inst.b].is_object() && !R[inst.b].is_array()) {
                    fail(VM_STOP_UNSUPPORTED, "property read on a value that is not an object or array");
                    return AnvilValue::make_undefined();
                }
                R[inst.a] = ic.load_property_ic(heap, site_id, R[inst.b], prop_sym);
                break;
            }

            case AnvilOpcode::SetPropIC: {
                const u32 site_id = inst.c;
                const u32 prop_sym = (site_id < ic.site_count) ? ic.sites()[site_id].prop_sym : 0u;
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
                    if (stored == StoreResult::HeapFull) {
                        reclaim_array_storage(reg_base + cur_frame_regs);
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
                    if (stored == StoreResult::HeapFull) {
                        reclaim_array_storage(reg_base + cur_frame_regs);
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
                // The callee's window is its own register high-water, at least
                // its parameter count and at least one slot.
                u32 callee_regs = fn.local_count;
                if (callee_regs < fn.arity) {
                    callee_regs = fn.arity;
                }
                if (callee_regs == 0u) {
                    callee_regs = 1u;
                }
                const u32 next_base = reg_base + cur_frame_regs;
                // Grow the register and frame stacks for the callee before use.
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
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, static_cast<u8>(inst.a)};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = NO_SLOT;
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
                // Copy the arguments off the register file, record the call,
                // and pause with the host-call status. The caller reads the
                // arguments from the out buffer and sends a Resume.
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
                resume_host_dst = inst.a;
                return AnvilValue::make_undefined();
            }

            case AnvilOpcode::CallValue: {
                const AnvilValue callee = R[inst.b];
                if (!callee.is_function()) {
                    fail(VM_STOP_NOT_FUNCTION, "called value is not a function");
                    return AnvilValue::make_undefined();
                }
                const u32 cid = callee.payload();
                if (cid >= heap.closure_count || !heap.closures()[cid].active) {
                    fail(VM_STOP_NOT_FUNCTION, "called value is not a function");
                    return AnvilValue::make_undefined();
                }
                const u32 fn_id = heap.closures()[cid].fn_id;
                if (fn_id >= heap.function_count || !heap.functions()[fn_id].active) {
                    fail(VM_STOP_NOT_FUNCTION, "called value is not a function");
                    return AnvilValue::make_undefined();
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
                // Copy the arguments the caller staged into the callee window,
                // filling any parameter the call did not pass with undefined.
                for (u32 a_idx = 0u; a_idx < fn.arity; ++a_idx) {
                    regs[next_base + a_idx] = (a_idx < argc)
                        ? regs[reg_base + inst.c + a_idx]
                        : AnvilValue::make_undefined();
                }
                frames()[frame_count++] =
                    AnvilCallFrame{fn_id, cur_closure, pc, reg_base, cur_frame_regs, static_cast<u8>(inst.a)};
                reg_base = next_base;
                cur_frame_regs = callee_regs;
                cur_closure = cid;
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
                    case 6u: out = js_trunc(v); break;
                    case 7u: out = js_sign(v); break;
                    default: break;
                }
                R[inst.a] = AnvilValue::make_number(out);
                break;
            }

            case AnvilOpcode::CallMath2: {
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
                const f64 v = av.as_number();
                bool out = false;
                switch (inst.b) {
                    // Number.isInteger: a Number that is finite and whole.
                    case 1u: out = av.is_number() && js_is_finite(v) && js_trunc(v) == v; break;
                    // Number.isNaN: a Number that is NaN (no coercion).
                    case 2u: out = av.is_number() && js_is_nan(v); break;
                    // Number.isFinite: a Number that is finite (no coercion).
                    case 3u: out = av.is_number() && js_is_finite(v); break;
                    // Global isNaN: coerce to number, then test for NaN.
                    case 4u: out = js_is_nan(v); break;
                    // Global isFinite: coerce to number, then test for finite.
                    case 5u: out = js_is_finite(v); break;
                    default: break;
                }
                R[inst.a] = AnvilValue::make_bool(out);
                break;
            }

            case AnvilOpcode::NewCell: {
                u32 cell = heap.alloc_cell(R[inst.b]);
                if (cell == NO_SLOT) {
                    reclaim_array_storage(reg_base + cur_frame_regs);
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
                const u32 cid = heap.alloc_closure(fn_id, upvals_off, n, false);
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
                if (frame_count == 0u) {
                    return ret_val;
                }
                const AnvilCallFrame caller = frames()[--frame_count];
                reg_base = caller.reg_base;
                cur_frame_regs = caller.caller_frame_regs;
                cur_closure = caller.closure_id;
                pc = caller.return_pc;
                registers()[reg_base + caller.dst_reg] = ret_val;
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
    // When the run paused on a host call, the out buffer carries that call's
    // arguments instead of the top-level variables.
    if (paused && pending_host_call) {
        fill_host_call(out);
    } else {
        fill_exports(out);
    }
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

// The pending host call the out buffer carries while a run is paused on a
// HostCall: fn id (u32), argument count (u32), overflow flag (u32), then one
// tagged record per argument -- kind (u8) and, for a number or bool, an f64,
// or for a string its length (u32) and bytes. Kinds match the export records.
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
        }
    }
    out->exports_full = w.full ? 1u : 0u;
    out->exports_len = w.len;
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
    if (ctx.paused) {
        return anvil_vm::VM_RUN_HOST_CALL;
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
        // The payload is the limits as u32s (step, script size, heap, call
        // depth, then the optional yield interval and compiler stack budget). A
        // zero leaves that limit as it was (except the yield interval).
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
            // The yield interval is set directly when present: a value of 0
            // turns pausing off, so it is not treated as "leave unchanged".
            if (len >= pos + 20u) {
                ctx.yield_interval = anvil_vm::read_u32_le(src + pos + 16u);
            }
            // The compiler's native-stack budget (bytes); 0 leaves it as it
            // was, like the other limits above.
            if (len >= pos + 24u) {
                const u32 parse_stack = anvil_vm::read_u32_le(src + pos + 20u);
                if (parse_stack != 0u) {
                    ctx.parse_stack_budget = parse_stack;
                }
            }
        }
        ctx.fill_telemetry(telemetry_out);
        return anvil_vm::VM_RUN_OK;
    }
    if (op == anvil_vm::AVK_OP_RESUME) {
        // Continue a run that paused at a yield. The context still holds the
        // program, frames and registers, so nothing is compiled or reset here.
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
    // A resume request continues a paused run, so init() must not run: it would
    // reset the arena and throw away the program, frames and registers. Every
    // other request starts from a clean context.
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
    if (index >= ctx.global_count || !ctx.globals()[index].occupied) {
        return 0u;
    }
    if (!ctx.globals()[index].value.is_number()) {
        return 0u;
    }
    u32 len = 0u;
    const char* text = ctx.heap.get_string_ptr(ctx.globals()[index].name_sym, &len);
    if (!text || len == 0u) {
        return 0u;
    }
    const u32 n = (len < name_cap - 1u) ? len : (name_cap - 1u);
    for (u32 i = 0u; i < n; ++i) {
        name_out[i] = text[i];
    }
    name_out[n] = '\0';
    *value_out = ctx.globals()[index].value.as_number();
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
        if (i != 0u && (i & 255u) == 0u) {
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
