// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// x86-64 baseline JIT for numeric AnvilVM programs.
// Author: Buntato
//
// Compiles a bytecode program to native machine code and runs it.
// Programs that use objects, calls, or element access stay on the interpreter.
// A textContent write of a number is a call back into the page.
// Linux uses the System V argument order. Windows uses rcx, rdx, and xmm2,
// and leaves 32 bytes of shadow space. If the page cannot be made executable,
// the interpreter runs the program instead.

#include "anvil_vm.hpp"

using uint8_t = u8;
inline void copy_bytes(void* dst, const void* src, u32 n) noexcept { auto* d = static_cast<u8*>(dst); const auto* s = static_cast<const u8*>(src); for (u32 i = 0u; i < n; ++i) d[i] = s[i]; }

#if defined(_WIN32)
#define NOMINMAX
extern "C" {
__declspec(dllimport) void* __stdcall VirtualAlloc(void* lpAddress, u64 dwSize, u32 flAllocationType, u32 flProtect) noexcept;
__declspec(dllimport) i32 __stdcall VirtualFree(void* lpAddress, u64 dwSize, u32 dwFreeType) noexcept;
__declspec(dllimport) i32 __stdcall VirtualProtect(void* lpAddress, u64 dwSize, u32 flNewProtect, u32* lpflOldProtect) noexcept;
}
constexpr u32 MEM_COMMIT = 0x00001000u;
constexpr u32 MEM_RESERVE = 0x00002000u;
constexpr u32 MEM_RELEASE = 0x00008000u;
constexpr u32 PAGE_READWRITE = 0x04u;
constexpr u32 PAGE_EXECUTE_READ = 0x20u;
using DWORD = u32;
#else
#include <sys/mman.h>
#include <unistd.h>
#endif

namespace anvil_vm {

namespace {

constexpr u32 kRegSlots = 64u;
constexpr u32 kCodeBytes = 256u * 1024u;

struct CodePage {
    uint8_t* bytes;
    u32 capacity;
    u32 length;
};

bool map_page(CodePage* page) noexcept {
    page->bytes = nullptr;
    page->capacity = kCodeBytes;
    page->length = 0u;
#if defined(_WIN32)
    void* mem = VirtualAlloc(nullptr, kCodeBytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
    if (!mem) {
        return false;
    }
    page->bytes = static_cast<uint8_t*>(mem);
    return true;
#else
    void* mem = mmap(nullptr, kCodeBytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mem == MAP_FAILED) {
        return false;
    }
    page->bytes = static_cast<uint8_t*>(mem);
    return true;
#endif
}

void unmap_page(CodePage* page) noexcept {
    if (!page->bytes) {
        return;
    }
#if defined(_WIN32)
    VirtualFree(page->bytes, 0, MEM_RELEASE);
#else
    munmap(page->bytes, page->capacity);
#endif
    page->bytes = nullptr;
    page->length = 0u;
}

bool seal_page(CodePage* page) noexcept {
#if defined(_WIN32)
    DWORD old = 0;
    return VirtualProtect(page->bytes, page->capacity, PAGE_EXECUTE_READ, &old) != 0;
#else
    return mprotect(page->bytes, page->capacity, PROT_READ | PROT_EXEC) == 0;
#endif
}

struct Fixup {
    u32 site;
    u32 target_pc;
    bool abs_abort;
    bool to_epilogue;
};

struct Asm {
    CodePage* page;
    u32 pc_at[MAX_BYTECODE_INSTRUCTIONS];
    Fixup fixups[MAX_BYTECODE_INSTRUCTIONS * 2u];
    u32 fixup_count;
    u32 epilogue_at;
    u32 abort_at;

    bool emit(uint8_t b) noexcept {
        if (page->length + 1u >= page->capacity) {
            return false;
        }
        page->bytes[page->length++] = b;
        return true;
    }

    bool emit_bytes(const uint8_t* data, u32 n) noexcept {
        if (page->length + n >= page->capacity) {
            return false;
        }
        copy_bytes(page->bytes + page->length, data, n);
        page->length += n;
        return true;
    }

    bool emit_u32(u32 value) noexcept {
        uint8_t raw[4] = {
            static_cast<uint8_t>(value),
            static_cast<uint8_t>(value >> 8),
            static_cast<uint8_t>(value >> 16),
            static_cast<uint8_t>(value >> 24),
        };
        return emit_bytes(raw, 4u);
    }

    bool emit_u64(u64 value) noexcept {
        uint8_t raw[8];
        for (u32 i = 0u; i < 8u; ++i) {
            raw[i] = static_cast<uint8_t>(value >> (8u * i));
        }
        return emit_bytes(raw, 8u);
    }

    // mov rax, imm64
    bool mov_rax_imm(u64 value) noexcept {
        return emit(0x48) && emit(0xB8) && emit_u64(value);
    }

    // mov rdi, r13 on Linux. mov rcx, r13 on Windows.
    bool mov_arg0_r13() noexcept {
#if defined(_WIN32)
        return emit(0x4C) && emit(0x89) && emit(0xE9);
#else
        return emit(0x4C) && emit(0x89) && emit(0xEF);
#endif
    }

    // mov esi, imm32 on Linux. mov edx, imm32 on Windows.
    bool mov_arg1_imm(u32 value) noexcept {
#if defined(_WIN32)
        return emit(0xBA) && emit_u32(value);
#else
        return emit(0xBE) && emit_u32(value);
#endif
    }

    // A (pointer, u32, double) call. Linux passes the number in xmm0.
    // Windows passes it in xmm2.
    bool load_ptr_u32_f64(u8 value_slot, u32 imm) noexcept {
#if defined(_WIN32)
        return movsd_xmm_from_reg(2u, value_slot) && mov_arg0_r13() && mov_arg1_imm(imm);
#else
        return movsd_xmm_from_reg(0u, value_slot) && mov_arg0_r13() && mov_arg1_imm(imm);
#endif
    }

    bool call_rax() noexcept {
        return emit(0xFF) && emit(0xD0);
    }

    bool test_eax_eax() noexcept {
        return emit(0x85) && emit(0xC0);
    }

    bool jz_rel32_fixup_abort() noexcept {
        // 0F 84 rel32
        if (!(emit(0x0F) && emit(0x84))) {
            return false;
        }
        if (fixup_count >= (MAX_BYTECODE_INSTRUCTIONS * 2u)) {
            return false;
        }
        fixups[fixup_count++] = Fixup{page->length, 0u, true, false};
        return emit_u32(0u);
    }

    bool jmp_rel32_to_pc(u32 target_pc) noexcept {
        if (!emit(0xE9)) {
            return false;
        }
        if (fixup_count >= (MAX_BYTECODE_INSTRUCTIONS * 2u)) {
            return false;
        }
        fixups[fixup_count++] = Fixup{page->length, target_pc, false, false};
        return emit_u32(0u);
    }

    bool jcc_rel32_to_pc(uint8_t cc, u32 target_pc) noexcept {
        if (!(emit(0x0F) && emit(cc))) {
            return false;
        }
        if (fixup_count >= (MAX_BYTECODE_INSTRUCTIONS * 2u)) {
            return false;
        }
        fixups[fixup_count++] = Fixup{page->length, target_pc, false, false};
        return emit_u32(0u);
    }

    // movsd xmmN, [r12 + disp8]
    bool movsd_xmm_from_reg(uint8_t xmm, u8 slot) noexcept {
        const u32 disp = static_cast<u32>(slot) * 8u;
        // F2 41 0F 10 modrm disp32  (xmm from [r12+disp32])
        if (!(emit(0xF2) && emit(0x41) && emit(0x0F) && emit(0x10))) {
            return false;
        }
        const uint8_t modrm = static_cast<uint8_t>(0x80 | (xmm << 3) | 0x04);
        return emit(modrm) && emit(0x24) && emit_u32(disp);
    }

    // movsd [r12 + disp32], xmmN
    bool movsd_reg_from_xmm(u8 slot, uint8_t xmm) noexcept {
        const u32 disp = static_cast<u32>(slot) * 8u;
        if (!(emit(0xF2) && emit(0x41) && emit(0x0F) && emit(0x11))) {
            return false;
        }
        const uint8_t modrm = static_cast<uint8_t>(0x80 | (xmm << 3) | 0x04);
        return emit(modrm) && emit(0x24) && emit_u32(disp);
    }

    bool xorpd_xmm(uint8_t xmm) noexcept {
        // 66 0F 57 modrm  (xorpd xmm, xmm)
        return emit(0x66) && emit(0x0F) && emit(0x57) && emit(static_cast<uint8_t>(0xC0 | (xmm << 3) | xmm));
    }

    bool addsd() noexcept { return emit(0xF2) && emit(0x0F) && emit(0x58) && emit(0xC1); }
    bool subsd() noexcept { return emit(0xF2) && emit(0x0F) && emit(0x5C) && emit(0xC1); }
    bool mulsd() noexcept { return emit(0xF2) && emit(0x0F) && emit(0x59) && emit(0xC1); }
    bool divsd() noexcept { return emit(0xF2) && emit(0x0F) && emit(0x5E) && emit(0xC1); }

    bool ucomisd_xmm0_xmm1() noexcept {
        return emit(0x66) && emit(0x0F) && emit(0x2E) && emit(0xC1);
    }

    bool cvtsi2sd_xmm0_eax() noexcept {
        return emit(0xF2) && emit(0x0F) && emit(0x2A) && emit(0xC0);
    }

    bool mov_eax_imm(i32 value) noexcept {
        return emit(0xB8) && emit_u32(static_cast<u32>(value));
    }

    bool step_or_abort(u64 step_fn) noexcept {
        if (!mov_arg0_r13() || !mov_rax_imm(step_fn) || !call_rax() || !test_eax_eax()) {
            return false;
        }
        return jz_rel32_fixup_abort();
    }

    bool call_binary(u64 fn, u8 dst, u8 lhs, u8 rhs) noexcept {
        return movsd_xmm_from_reg(0u, lhs) && movsd_xmm_from_reg(1u, rhs) &&
               mov_rax_imm(fn) && call_rax() && movsd_reg_from_xmm(dst, 0u);
    }

    bool patch() noexcept {
        for (u32 i = 0u; i < fixup_count; ++i) {
            const Fixup fx = fixups[i];
            u32 target = abort_at;
            if (fx.to_epilogue) {
                target = epilogue_at;
            } else if (!fx.abs_abort) {
                if (fx.target_pc >= MAX_BYTECODE_INSTRUCTIONS) {
                    return false;
                }
                target = pc_at[fx.target_pc];
            }
            if (target == 0xFFFFFFFFu) {
                return false;
            }
            const i32 rel = static_cast<i32>(target) - static_cast<i32>(fx.site + 4u);
            copy_bytes(page->bytes + fx.site, &rel, 4u);
        }
        return true;
    }
};

bool opcode_supported(AnvilOpcode op) noexcept {
    switch (op) {
        case AnvilOpcode::Nop:
        case AnvilOpcode::LoadConst:
        case AnvilOpcode::LoadSmi:
        case AnvilOpcode::LoadTrue:
        case AnvilOpcode::LoadFalse:
        case AnvilOpcode::Move:
        case AnvilOpcode::Add:
        case AnvilOpcode::Sub:
        case AnvilOpcode::Mul:
        case AnvilOpcode::Div:
        case AnvilOpcode::Mod:
        case AnvilOpcode::Neg:
        case AnvilOpcode::Not:
        case AnvilOpcode::BitAnd:
        case AnvilOpcode::BitOr:
        case AnvilOpcode::BitXor:
        case AnvilOpcode::Shl:
        case AnvilOpcode::Shr:
        case AnvilOpcode::UShr:
        case AnvilOpcode::CmpEq:
        case AnvilOpcode::CmpNe:
        case AnvilOpcode::CmpLooseEq:
        case AnvilOpcode::CmpLooseNe:
        case AnvilOpcode::CmpLt:
        case AnvilOpcode::CmpLe:
        case AnvilOpcode::CmpGt:
        case AnvilOpcode::CmpGe:
        case AnvilOpcode::Jump:
        case AnvilOpcode::JumpIfFalse:
        case AnvilOpcode::JumpIfTrue:
        case AnvilOpcode::GetGlobal:
        case AnvilOpcode::SetGlobal:
        case AnvilOpcode::CallMath:
        case AnvilOpcode::LoopHeader:
        case AnvilOpcode::Return:
        case AnvilOpcode::Halt:
        case AnvilOpcode::HostSetText:
            return true;
        default:
            return false;
    }
}

bool program_is_numeric(const AnvilBytecodeProgram& program) noexcept {
    if (program.code_len == 0u || !program.compile_ok) {
        return false;
    }
    for (u32 i = 0u; i < program.code_len; ++i) {
        const AnvilInstruction inst = program.code[i];
        if (!opcode_supported(inst.op)) {
            return false;
        }
        if (inst.op == AnvilOpcode::LoadConst) {
            const u16 idx = inst.imm16();
            if (idx >= program.const_len || !program.constants[idx].is_number()) {
                return false;
            }
        }
        if (inst.op == AnvilOpcode::Jump || inst.op == AnvilOpcode::JumpIfFalse ||
            inst.op == AnvilOpcode::JumpIfTrue) {
            if (inst.imm16() >= program.code_len) {
                return false;
            }
        }
        if (inst.op == AnvilOpcode::HostSetText && inst.imm16() >= 8u) {
            return false;
        }
        if (inst.a >= kRegSlots) {
            return false;
        }
        // Native code keeps every value as a double, so true would read
        // back as 1. A boolean may only feed the jump right after it.
        const bool makes_bool = inst.op == AnvilOpcode::LoadTrue || inst.op == AnvilOpcode::LoadFalse ||
                                inst.op == AnvilOpcode::Not || inst.op == AnvilOpcode::CmpEq ||
                                inst.op == AnvilOpcode::CmpNe || inst.op == AnvilOpcode::CmpLt ||
                                inst.op == AnvilOpcode::CmpLe || inst.op == AnvilOpcode::CmpGt ||
                                inst.op == AnvilOpcode::CmpGe || inst.op == AnvilOpcode::CmpLooseEq ||
                                inst.op == AnvilOpcode::CmpLooseNe;
        if (makes_bool) {
            if (i + 1u >= program.code_len) {
                return false;
            }
            const AnvilInstruction next = program.code[i + 1u];
            const bool jump = next.op == AnvilOpcode::JumpIfFalse || next.op == AnvilOpcode::JumpIfTrue;
            if (!jump || next.a != inst.a) {
                return false;
            }
        }
    }
    return true;
}

int step_budget(AnvilVmContext* ctx) noexcept {
    const u32 limit = ctx->effective_step_limit();
    if (++ctx->steps_executed > limit) {
        ctx->fail_limit(VM_STOP_STEPS, "step", limit, "steps");
        return 0;
    }
    return 1;
}

double global_load(AnvilVmContext* ctx, u32 slot) noexcept {
    if (slot >= ctx->global_count) {
        return 0.0;
    }
    return ctx->globals[slot].value.as_number();
}

void global_store(AnvilVmContext* ctx, u32 slot, double value) noexcept {
    if (slot >= MAX_GLOBALS) {
        return;
    }
    ctx->globals[slot].value = AnvilValue::make_number(value);
    ctx->globals[slot].occupied = true;
    if (slot >= ctx->global_count) {
        ctx->global_count = slot + 1u;
    }
}

void loop_header(AnvilVmContext* ctx, u32 lid) noexcept {
    const u32 id = lid & 63u;
    ++ctx->program.loop_trip_counts[id];
    if (ctx->program.loop_trip_counts[id] == HOT_LOOP_THRESHOLD) {
        ++ctx->hot_loops_accelerated;
    }
}

double num_div(double lhs, double rhs) noexcept {
    return lhs / rhs;
}

double num_mod(double lhs, double rhs) noexcept {
    return js_remainder(lhs, rhs);
}

double num_neg(double v) noexcept { return -v; }

double num_not(double v) noexcept {
    const bool truthy = (v != 0.0) && (v == v);
    return truthy ? 0.0 : 1.0;
}

double bit_and(double lhs, double rhs) noexcept {
    return static_cast<double>(js_to_int32(lhs) & js_to_int32(rhs));
}

double bit_or(double lhs, double rhs) noexcept {
    return static_cast<double>(js_to_int32(lhs) | js_to_int32(rhs));
}

double bit_xor(double lhs, double rhs) noexcept {
    return static_cast<double>(js_to_int32(lhs) ^ js_to_int32(rhs));
}

double bit_shl(double lhs, double rhs) noexcept {
    const u32 s = js_to_uint32(rhs) & 31u;
    return static_cast<double>(static_cast<i32>(js_to_uint32(lhs) << s));
}

double bit_shr(double lhs, double rhs) noexcept {
    const u32 s = js_to_uint32(rhs) & 31u;
    return static_cast<double>(js_to_int32(lhs) >> s);
}

double bit_ushr(double lhs, double rhs) noexcept {
    const u32 s = js_to_uint32(rhs) & 31u;
    return static_cast<double>(js_to_uint32(lhs) >> s);
}

double cmp_eq(double lhs, double rhs) noexcept { return lhs == rhs ? 1.0 : 0.0; }
double cmp_ne(double lhs, double rhs) noexcept { return lhs != rhs ? 1.0 : 0.0; }
double cmp_lt(double lhs, double rhs) noexcept { return lhs < rhs ? 1.0 : 0.0; }
double cmp_le(double lhs, double rhs) noexcept { return lhs <= rhs ? 1.0 : 0.0; }
double cmp_gt(double lhs, double rhs) noexcept { return lhs > rhs ? 1.0 : 0.0; }
double cmp_ge(double lhs, double rhs) noexcept { return lhs >= rhs ? 1.0 : 0.0; }

void host_set_text_bridge(AnvilVmContext* ctx, u32 slot, double value) noexcept {
    if (!ctx || !ctx->host_set_text || slot >= ctx->host_id_count || slot >= 8u) {
        return;
    }
    ctx->host_set_text(ctx->host, ctx->host_ids[slot], ctx->host_id_lens[slot], value);
}

double math_call(u32 op, double v) noexcept {
    switch (op) {
        case 1u: return js_abs(v);
        case 2u: return js_floor(v);
        case 3u: return js_ceil(v);
        case 4u: return js_round(v);
        case 5u: return js_sqrt(v);
        default: return v;
    }
}

double math_dispatch(double op, double v) noexcept {
    return math_call(op >= 0.0 && op < 16.0 ? static_cast<u32>(op) : 0u, v);
}

bool emit_cmp_flags(Asm& as, u8 dst) noexcept {
    // After ucomisd xmm0, xmm1 the caller sets flags. This helper is not used.
    (void)as;
    (void)dst;
    return true;
}

bool emit_setcc_bool(Asm& as, u8 dst, uint8_t setcc) noexcept {
    // setcc al ; movzx eax, al ; cvtsi2sd xmm0, eax ; store
    if (!(as.emit(0x0F) && as.emit(setcc) && as.emit(0xC0))) {
        return false;
    }
    if (!(as.emit(0x0F) && as.emit(0xB6) && as.emit(0xC0))) {
        return false;
    }
    return as.cvtsi2sd_xmm0_eax() && as.movsd_reg_from_xmm(dst, 0u);
}

} // namespace

bool anvil_jit_try_run(AnvilVmContext* ctx, u32 entry_pc, f64* out_number) noexcept {
    if (!ctx || !out_number || entry_pc != 0u) {
        return false;
    }
    if (!program_is_numeric(ctx->program)) {
        return false;
    }

    CodePage page{};
    if (!map_page(&page)) {
        return false;
    }

    Asm* as_ptr = static_cast<Asm*>(
#if defined(_WIN32)
        VirtualAlloc(nullptr, sizeof(Asm), MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE)
#else
        mmap(nullptr, sizeof(Asm), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0)
#endif
    );
    if (!as_ptr
#if !defined(_WIN32)
        || as_ptr == MAP_FAILED
#endif
    ) {
        unmap_page(&page);
        return false;
    }
    Asm& as = *as_ptr;
    as.page = &page;
    as.fixup_count = 0u;
    as.epilogue_at = 0u;
    as.abort_at = 0u;
    for (u32 i = 0u; i < MAX_BYTECODE_INSTRUCTIONS; ++i) {
        as.pc_at[i] = 0xFFFFFFFFu;
    }

    bool ok = true;
    // prologue: push rbp; mov rbp, rsp; push rbx; push r12; push r13
    ok = ok && as.emit(0x55) && as.emit(0x48) && as.emit(0x89) && as.emit(0xE5);
    ok = ok && as.emit(0x53);
    ok = ok && as.emit(0x41) && as.emit(0x54);
    ok = ok && as.emit(0x41) && as.emit(0x55);
#if defined(_WIN32)
    // 32 bytes of shadow space, plus 8 so the stack stays 16-byte aligned.
    ok = ok && as.emit(0x48) && as.emit(0x83) && as.emit(0xEC) && as.emit(0x28);
    // mov r12, rcx ; mov r13, rdx
    ok = ok && as.emit(0x49) && as.emit(0x89) && as.emit(0xCC);
    ok = ok && as.emit(0x49) && as.emit(0x89) && as.emit(0xD5);
#else
    ok = ok && as.emit(0x48) && as.emit(0x83) && as.emit(0xEC) && as.emit(0x08);
    // mov r12, rdi ; mov r13, rsi
    ok = ok && as.emit(0x49) && as.emit(0x89) && as.emit(0xFC);
    ok = ok && as.emit(0x49) && as.emit(0x89) && as.emit(0xF5);
#endif

    const u64 step_fn = reinterpret_cast<u64>(&step_budget);
    const u64 load_fn = reinterpret_cast<u64>(&global_load);
    const u64 store_fn = reinterpret_cast<u64>(&global_store);
    const u64 loop_fn = reinterpret_cast<u64>(&loop_header);

    for (u32 pc = 0u; ok && pc < ctx->program.code_len; ++pc) {
        as.pc_at[pc] = page.length;
        const AnvilInstruction inst = ctx->program.code[pc];
        ok = ok && as.step_or_abort(step_fn);
        switch (inst.op) {
            case AnvilOpcode::Nop:
                break;
            case AnvilOpcode::LoadSmi:
                ok = ok && as.mov_eax_imm(inst.signed_imm16()) && as.cvtsi2sd_xmm0_eax() &&
                     as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::LoadTrue:
                ok = ok && as.mov_eax_imm(1) && as.cvtsi2sd_xmm0_eax() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::LoadFalse:
                ok = ok && as.xorpd_xmm(0u) && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::LoadConst: {
                const f64 n = ctx->program.constants[inst.imm16()].as_number();
                u64 bits = 0;
                copy_bytes(&bits, &n, 8u);
                // mov rax, bits ; movq xmm0, rax ; store
                ok = ok && as.mov_rax_imm(bits);
                ok = ok && as.emit(0x66) && as.emit(0x48) && as.emit(0x0F) && as.emit(0x6E) && as.emit(0xC0);
                ok = ok && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            }
            case AnvilOpcode::Move:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.b) && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::Add:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.b) && as.movsd_xmm_from_reg(1u, inst.c) &&
                     as.addsd() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::Sub:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.b) && as.movsd_xmm_from_reg(1u, inst.c) &&
                     as.subsd() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::Mul:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.b) && as.movsd_xmm_from_reg(1u, inst.c) &&
                     as.mulsd() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::Div:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&num_div), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::Mod:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&num_mod), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::Neg:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.b) && as.mov_rax_imm(reinterpret_cast<u64>(&num_neg)) &&
                     as.call_rax() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::Not:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.b) && as.mov_rax_imm(reinterpret_cast<u64>(&num_not)) &&
                     as.call_rax() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::BitAnd:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&bit_and), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::BitOr:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&bit_or), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::BitXor:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&bit_xor), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::Shl:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&bit_shl), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::Shr:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&bit_shr), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::UShr:
                ok = ok && as.call_binary(reinterpret_cast<u64>(&bit_ushr), inst.a, inst.b, inst.c);
                break;
            case AnvilOpcode::CmpEq:
            case AnvilOpcode::CmpNe:
            case AnvilOpcode::CmpLooseEq:
            case AnvilOpcode::CmpLooseNe:
            case AnvilOpcode::CmpLt:
            case AnvilOpcode::CmpLe:
            case AnvilOpcode::CmpGt:
            case AnvilOpcode::CmpGe: {
                // Native code only runs programs where every value is a
                // number, and there `==` is the same as `===`.
                double (*fn)(double, double) = &cmp_eq;
                if (inst.op == AnvilOpcode::CmpNe || inst.op == AnvilOpcode::CmpLooseNe) fn = &cmp_ne;
                if (inst.op == AnvilOpcode::CmpLt) fn = &cmp_lt;
                if (inst.op == AnvilOpcode::CmpLe) fn = &cmp_le;
                if (inst.op == AnvilOpcode::CmpGt) fn = &cmp_gt;
                if (inst.op == AnvilOpcode::CmpGe) fn = &cmp_ge;
                ok = ok && as.call_binary(reinterpret_cast<u64>(fn), inst.a, inst.b, inst.c);
                break;
            }
            case AnvilOpcode::Jump:
                ok = ok && as.jmp_rel32_to_pc(inst.imm16());
                break;
            case AnvilOpcode::JumpIfFalse:
                // xorpd xmm1,xmm1 ; movsd xmm0, reg ; ucomisd ; jp taken ; je taken
                ok = ok && as.xorpd_xmm(1u) && as.movsd_xmm_from_reg(0u, inst.a) && as.ucomisd_xmm0_xmm1();
                ok = ok && as.jcc_rel32_to_pc(0x8A, inst.imm16()); // jp
                ok = ok && as.jcc_rel32_to_pc(0x84, inst.imm16()); // je
                break;
            case AnvilOpcode::JumpIfTrue: {
                // Taken when not NaN and not zero. jp skip ; je skip ; jmp taken
                ok = ok && as.xorpd_xmm(1u) && as.movsd_xmm_from_reg(0u, inst.a) && as.ucomisd_xmm0_xmm1();
                const u32 skip_site_base = page.length;
                (void)skip_site_base;
                // jp +12 (over je and jmp), je +8 (over jmp), jmp target
                // short jumps are easier to size exactly.
                // jp rel8
                ok = ok && as.emit(0x7A) && as.emit(0x07);
                // je rel8
                ok = ok && as.emit(0x74) && as.emit(0x05);
                ok = ok && as.jmp_rel32_to_pc(inst.imm16());
                break;
            }
            case AnvilOpcode::GetGlobal:
                ok = ok && as.mov_arg0_r13() && as.mov_arg1_imm(inst.imm16()) &&
                     as.mov_rax_imm(load_fn) && as.call_rax() && as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            case AnvilOpcode::SetGlobal:
                ok = ok && as.load_ptr_u32_f64(inst.a, inst.imm16()) &&
                     as.mov_rax_imm(store_fn) && as.call_rax();
                break;
            case AnvilOpcode::CallMath: {
                // xmm0 = op as double, xmm1 = value
                ok = ok && as.mov_eax_imm(static_cast<i32>(inst.b)) && as.cvtsi2sd_xmm0_eax();
                // that put op in xmm0. Need value in xmm1 and op still in xmm0.
                ok = ok && as.movsd_xmm_from_reg(1u, inst.c);
                // cvtsi2sd wrote xmm0, then movsd xmm1 from reg. Good.
                ok = ok && as.mov_rax_imm(reinterpret_cast<u64>(&math_dispatch)) && as.call_rax() &&
                     as.movsd_reg_from_xmm(inst.a, 0u);
                break;
            }
            case AnvilOpcode::LoopHeader:
                ok = ok && as.mov_arg0_r13() && as.mov_arg1_imm(inst.a) && as.mov_rax_imm(loop_fn) && as.call_rax();
                break;
            case AnvilOpcode::HostSetText:
                ok = ok && as.load_ptr_u32_f64(inst.a, inst.imm16()) &&
                     as.mov_rax_imm(reinterpret_cast<u64>(&host_set_text_bridge)) && as.call_rax();
                break;
            case AnvilOpcode::Return:
            case AnvilOpcode::Halt:
                ok = ok && as.movsd_xmm_from_reg(0u, inst.a);
                if (!as.emit(0xE9)) {
                    ok = false;
                    break;
                }
                if (as.fixup_count >= (MAX_BYTECODE_INSTRUCTIONS * 2u)) {
                    ok = false;
                    break;
                }
                as.fixups[as.fixup_count++] = Fixup{page.length, 0u, false, true};
                ok = ok && as.emit_u32(0u);
                break;
            default:
                ok = false;
                break;
        }
    }

    if (ok) {
        as.abort_at = page.length;
        ok = ok && as.xorpd_xmm(0u);
        as.epilogue_at = page.length;
#if defined(_WIN32)
        ok = ok && as.emit(0x48) && as.emit(0x83) && as.emit(0xC4) && as.emit(0x28);
#else
        ok = ok && as.emit(0x48) && as.emit(0x83) && as.emit(0xC4) && as.emit(0x08);
#endif
        ok = ok && as.emit(0x41) && as.emit(0x5D);
        ok = ok && as.emit(0x41) && as.emit(0x5C);
        ok = ok && as.emit(0x5B);
        ok = ok && as.emit(0x5D);
        ok = ok && as.emit(0xC3);
    }

    const bool patched = ok && as.patch();
#if defined(_WIN32)
    VirtualFree(as_ptr, 0, MEM_RELEASE);
#else
    munmap(as_ptr, sizeof(Asm));
#endif
    if (!patched || !seal_page(&page)) {
        unmap_page(&page);
        return false;
    }

    using Entry = double (*)(double*, AnvilVmContext*);
    auto entry = reinterpret_cast<Entry>(page.bytes);
    double regs[kRegSlots];
    for (u32 i = 0u; i < kRegSlots; ++i) {
        regs[i] = 0.0;
    }
    const double result = entry(regs, ctx);
    *out_number = ctx->aborted ? 0.0 : result;
    unmap_page(&page);
    return true;
}

} // namespace anvil_vm
