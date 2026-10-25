// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 Lexer, Recursive-Descent / Pratt Expression Parser, and Register Bytecode Compiler.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {

enum class TokenKind : u8 {
    Eof        = 0u,
    Number     = 1u,
    String     = 2u,
    Ident      = 3u,
    KwLet      = 4u,
    KwConst    = 5u,
    KwVar      = 6u,
    KwIf       = 7u,
    KwElse     = 8u,
    KwWhile    = 9u,
    KwFor      = 10u,
    KwFunction = 11u,
    KwReturn   = 12u,
    KwTrue     = 13u,
    KwFalse    = 14u,
    KwNull     = 15u,
    KwNew      = 16u,
    LParen     = 17u,
    RParen     = 18u,
    LBrace     = 19u,
    RBrace     = 20u,
    LBracket   = 21u,
    RBracket   = 22u,
    Comma      = 23u,
    Dot        = 24u,
    Colon      = 25u,
    Semicolon  = 26u,
    Question   = 27u,
    Plus       = 28u,
    Minus      = 29u,
    Star       = 30u,
    Slash      = 31u,
    Percent    = 32u,
    Bang       = 33u,
    Amp        = 34u,
    Pipe       = 35u,
    Caret      = 36u,
    Shl        = 37u,
    Shr        = 38u,
    UShr       = 39u,
    Assign     = 40u,
    PlusEq     = 41u,
    MinusEq    = 42u,
    StarEq     = 43u,
    PlusPlus   = 44u,
    MinusMinus = 45u,
    EqEq       = 46u,
    BangEq     = 47u,
    Lt         = 48u,
    Le         = 49u,
    Gt         = 50u,
    Ge         = 51u,
    AndAnd     = 52u,
    OrOr       = 53u,
};

struct Token {
    TokenKind kind;
    u32 offset;
    u32 length;
    f64 num_val;
};

constexpr u32 MAX_TOKENS = 2048u;

inline bool is_space(char c) noexcept {
    return c == ' ' || c == '\t' || c == '\r' || c == '\n';
}

inline bool is_digit(char c) noexcept {
    return c >= '0' && c <= '9';
}

inline bool is_ident_start(char c) noexcept {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_' || c == '$';
}

inline bool is_ident_cont(char c) noexcept {
    return is_ident_start(c) || is_digit(c);
}

inline bool match_kw(const char* s, u32 len, const char* kw, u32 kw_len) noexcept {
    if (len != kw_len) {
        return false;
    }
    for (u32 i = 0; i < len; ++i) {
        if (s[i] != kw[i]) {
            return false;
        }
    }
    return true;
}

f64 parse_f64_slice(const char* s, u32 len) noexcept {
    f64 int_part = 0.0;
    u32 i = 0u;
    while (i < len && is_digit(s[i])) {
        int_part = int_part * 10.0 + static_cast<f64>(s[i] - '0');
        ++i;
    }
    if (i < len && s[i] == '.') {
        ++i;
        f64 frac = 0.0;
        f64 scale = 0.1;
        while (i < len && is_digit(s[i])) {
            frac += static_cast<f64>(s[i] - '0') * scale;
            scale *= 0.1;
            ++i;
        }
        int_part += frac;
    }
    return int_part;
}

struct AnvilLexer {
    Token tokens[MAX_TOKENS];
    u32 count;

    bool tokenize(const char* src, u32 len) noexcept {
        count = 0u;
        u32 pos = 0u;
        while (pos < len) {
            if (count + 1u >= MAX_TOKENS) {
                return false;
            }
            const char c = src[pos];
            if (is_space(c)) {
                ++pos;
                continue;
            }
            // Skip single-line `//` and multi-line `/* ... */` comments.
            if (c == '/' && pos + 1u < len && src[pos + 1u] == '/') {
                pos += 2u;
                while (pos < len && src[pos] != '\n') {
                    ++pos;
                }
                continue;
            }
            if (c == '/' && pos + 1u < len && src[pos + 1u] == '*') {
                pos += 2u;
                while (pos + 1u < len && !(src[pos] == '*' && src[pos + 1u] == '/')) {
                    ++pos;
                }
                pos = (pos + 1u < len) ? (pos + 2u) : len;
                continue;
            }

            if (is_digit(c)) {
                const u32 start = pos;
                while (pos < len && (is_digit(src[pos]) || src[pos] == '.')) {
                    ++pos;
                }
                tokens[count++] = Token{
                    TokenKind::Number,
                    start,
                    pos - start,
                    parse_f64_slice(&src[start], pos - start),
                };
                continue;
            }

            if (c == '\'' || c == '"') {
                const char quote = c;
                ++pos;
                const u32 start = pos;
                while (pos < len && src[pos] != quote) {
                    if (src[pos] == '\\' && pos + 1u < len) {
                        pos += 2u;
                    } else {
                        ++pos;
                    }
                }
                const u32 str_len = pos - start;
                if (pos < len && src[pos] == quote) {
                    ++pos;
                }
                tokens[count++] = Token{TokenKind::String, start, str_len, 0.0};
                continue;
            }

            if (is_ident_start(c)) {
                const u32 start = pos;
                while (pos < len && is_ident_cont(src[pos])) {
                    ++pos;
                }
                const u32 id_len = pos - start;
                const char* s = &src[start];
                TokenKind kind = TokenKind::Ident;
                if (match_kw(s, id_len, "let", 3)) kind = TokenKind::KwLet;
                else if (match_kw(s, id_len, "const", 5)) kind = TokenKind::KwConst;
                else if (match_kw(s, id_len, "var", 3)) kind = TokenKind::KwVar;
                else if (match_kw(s, id_len, "if", 2)) kind = TokenKind::KwIf;
                else if (match_kw(s, id_len, "else", 4)) kind = TokenKind::KwElse;
                else if (match_kw(s, id_len, "while", 5)) kind = TokenKind::KwWhile;
                else if (match_kw(s, id_len, "for", 3)) kind = TokenKind::KwFor;
                else if (match_kw(s, id_len, "function", 8)) kind = TokenKind::KwFunction;
                else if (match_kw(s, id_len, "return", 6)) kind = TokenKind::KwReturn;
                else if (match_kw(s, id_len, "true", 4)) kind = TokenKind::KwTrue;
                else if (match_kw(s, id_len, "false", 5)) kind = TokenKind::KwFalse;
                else if (match_kw(s, id_len, "null", 4)) kind = TokenKind::KwNull;
                else if (match_kw(s, id_len, "new", 3)) kind = TokenKind::KwNew;
                tokens[count++] = Token{kind, start, id_len, 0.0};
                continue;
            }

            // Multi-character operators
            const char n1 = (pos + 1u < len) ? src[pos + 1u] : '\0';
            const char n2 = (pos + 2u < len) ? src[pos + 2u] : '\0';

            if (c == '=' && n1 == '=' && n2 == '=') {
                tokens[count++] = Token{TokenKind::EqEq, pos, 3u, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '!' && n1 == '=' && n2 == '=') {
                tokens[count++] = Token{TokenKind::BangEq, pos, 3u, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '>' && n1 == '>' && n2 == '>') {
                tokens[count++] = Token{TokenKind::UShr, pos, 3u, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '=' && n1 == '=') {
                tokens[count++] = Token{TokenKind::EqEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '!' && n1 == '=') {
                tokens[count++] = Token{TokenKind::BangEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '<' && n1 == '=') {
                tokens[count++] = Token{TokenKind::Le, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '>' && n1 == '=') {
                tokens[count++] = Token{TokenKind::Ge, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '<' && n1 == '<') {
                tokens[count++] = Token{TokenKind::Shl, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '>' && n1 == '>') {
                tokens[count++] = Token{TokenKind::Shr, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '&' && n1 == '&') {
                tokens[count++] = Token{TokenKind::AndAnd, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '|' && n1 == '|') {
                tokens[count++] = Token{TokenKind::OrOr, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '+' && n1 == '+') {
                tokens[count++] = Token{TokenKind::PlusPlus, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '-' && n1 == '-') {
                tokens[count++] = Token{TokenKind::MinusMinus, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '+' && n1 == '=') {
                tokens[count++] = Token{TokenKind::PlusEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '-' && n1 == '=') {
                tokens[count++] = Token{TokenKind::MinusEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '*' && n1 == '=') {
                tokens[count++] = Token{TokenKind::StarEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }

            TokenKind single = TokenKind::Eof;
            switch (c) {
                case '(': single = TokenKind::LParen; break;
                case ')': single = TokenKind::RParen; break;
                case '{': single = TokenKind::LBrace; break;
                case '}': single = TokenKind::RBrace; break;
                case '[': single = TokenKind::LBracket; break;
                case ']': single = TokenKind::RBracket; break;
                case ',': single = TokenKind::Comma; break;
                case '.': single = TokenKind::Dot; break;
                case ':': single = TokenKind::Colon; break;
                case ';': single = TokenKind::Semicolon; break;
                case '?': single = TokenKind::Question; break;
                case '+': single = TokenKind::Plus; break;
                case '-': single = TokenKind::Minus; break;
                case '*': single = TokenKind::Star; break;
                case '/': single = TokenKind::Slash; break;
                case '%': single = TokenKind::Percent; break;
                case '!': single = TokenKind::Bang; break;
                case '&': single = TokenKind::Amp; break;
                case '|': single = TokenKind::Pipe; break;
                case '^': single = TokenKind::Caret; break;
                case '=': single = TokenKind::Assign; break;
                case '<': single = TokenKind::Lt; break;
                case '>': single = TokenKind::Gt; break;
                default: return false;
            }
            tokens[count++] = Token{single, pos, 1u, 0.0};
            ++pos;
        }
        tokens[count++] = Token{TokenKind::Eof, len, 0u, 0.0};
        return true;
    }
};

struct LocalBinding {
    u32 name_sym;
    u8 reg;
};

struct AnvilCompiler {
    AnvilVmContext* ctx;
    const char* src;
    AnvilLexer lexer;
    u32 cursor;
    LocalBinding locals[MAX_REGISTERS_PER_FRAME];
    u32 local_count;
    u8 next_reg;
    u32 next_loop_id;
    bool in_function;
    bool had_error;

    const Token& peek(u32 ahead = 0u) const noexcept {
        const u32 idx = (cursor + ahead < lexer.count) ? (cursor + ahead) : (lexer.count - 1u);
        return lexer.tokens[idx];
    }

    const Token& advance() noexcept {
        const Token& t = peek(0u);
        if (cursor + 1u < lexer.count) {
            ++cursor;
        }
        return t;
    }

    bool match(TokenKind k) noexcept {
        if (peek().kind == k) {
            advance();
            return true;
        }
        return false;
    }

    bool looks_like_host_text() const noexcept {
        if (peek(0u).kind != TokenKind::Ident || peek(1u).kind != TokenKind::Dot) {
            return false;
        }
        if (!match_kw(&src[peek(0u).offset], peek(0u).length, "document", 8)) {
            return false;
        }
        if (peek(2u).kind != TokenKind::Ident ||
            !match_kw(&src[peek(2u).offset], peek(2u).length, "getElementById", 14)) {
            return false;
        }
        if (peek(3u).kind != TokenKind::LParen || peek(4u).kind != TokenKind::String ||
            peek(5u).kind != TokenKind::RParen || peek(6u).kind != TokenKind::Dot) {
            return false;
        }
        if (peek(7u).kind != TokenKind::Ident ||
            !match_kw(&src[peek(7u).offset], peek(7u).length, "textContent", 11)) {
            return false;
        }
        return peek(8u).kind == TokenKind::Assign;
    }

    u8 compile_host_text() noexcept {
        advance();
        advance();
        advance();
        advance();
        const Token id_tok = advance();
        advance();
        advance();
        advance();
        advance();
        if (ctx->host_id_count >= 8u || id_tok.length == 0u || id_tok.length >= 64u) {
            had_error = true;
            return 0u;
        }
        for (u32 i = 0u; i < id_tok.length; ++i) {
            if (src[id_tok.offset + i] == '\\') {
                had_error = true;
                return 0u;
            }
        }
        const u32 slot = ctx->host_id_count++;
        for (u32 i = 0u; i < id_tok.length; ++i) {
            ctx->host_ids[slot][i] = src[id_tok.offset + i];
        }
        ctx->host_ids[slot][id_tok.length] = '\0';
        ctx->host_id_lens[slot] = id_tok.length;
        const u8 rhs = compile_expr();
        ctx->program.emit(AnvilInstruction::make_abx(
            AnvilOpcode::HostSetText,
            rhs,
            static_cast<u16>(slot)
        ));
        return rhs;
    }

    bool expect(TokenKind k) noexcept {
        if (match(k)) {
            return true;
        }
        had_error = true;
        return false;
    }

    u8 alloc_reg() noexcept {
        if (next_reg >= MAX_REGISTERS_PER_FRAME) {
            had_error = true;
            return MAX_REGISTERS_PER_FRAME - 1u;
        }
        return next_reg++;
    }

    i32 find_local(u32 sym) const noexcept {
        for (i32 i = static_cast<i32>(local_count) - 1; i >= 0; --i) {
            if (locals[i].name_sym == sym) {
                return static_cast<i32>(locals[i].reg);
            }
        }
        return -1;
    }

    u8 declare_local(u32 sym) noexcept {
        const i32 existing = find_local(sym);
        if (existing >= 0) {
            return static_cast<u8>(existing);
        }
        const u8 r = alloc_reg();
        if (local_count < MAX_REGISTERS_PER_FRAME) {
            locals[local_count++] = LocalBinding{sym, r};
        }
        return r;
    }

    u32 token_sym(const Token& t) noexcept {
        return ctx->heap.intern_string(&src[t.offset], t.length);
    }

    void emit_load_number(u8 dst, f64 n) noexcept {
        const i32 as_int = static_cast<i32>(n);
        if (static_cast<f64>(as_int) == n && as_int >= -32768 && as_int <= 32767) {
            ctx->program.emit(AnvilInstruction::make_abx(
                AnvilOpcode::LoadSmi,
                dst,
                static_cast<u16>(static_cast<short>(as_int))
            ));
        } else {
            const u16 cidx = ctx->program.add_constant(AnvilValue::make_number(n));
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
        }
    }

    u8 compile_primary() noexcept {
        const Token& t = peek();
        if (t.kind == TokenKind::Number) {
            advance();
            const u8 dst = alloc_reg();
            emit_load_number(dst, t.num_val);
            return dst;
        }
        if (t.kind == TokenKind::KwTrue) {
            advance();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadTrue, dst, 0u, 0u));
            return dst;
        }
        if (t.kind == TokenKind::KwFalse) {
            advance();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadFalse, dst, 0u, 0u));
            return dst;
        }
        if (t.kind == TokenKind::KwNull) {
            advance();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, dst, 0u, 0u));
            return dst;
        }
        if (t.kind == TokenKind::String) {
            advance();
            const u32 sym = token_sym(t);
            const u8 dst = alloc_reg();
            const u16 cidx = ctx->program.add_constant(AnvilValue::make_string(sym));
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
            return dst;
        }
        if (t.kind == TokenKind::LParen) {
            advance();
            const u8 inner = compile_expr();
            expect(TokenKind::RParen);
            return inner;
        }
        if (t.kind == TokenKind::LBrace) {
            // Object literal `{ key: expr, ... }` lowered to `NewObject` + `SetPropIC`.
            advance();
            const u8 obj_reg = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewObject, obj_reg, 0u, 0u));
            while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
                const Token& key_tok = advance();
                if (key_tok.kind != TokenKind::Ident && key_tok.kind != TokenKind::String) {
                    had_error = true;
                    break;
                }
                const u32 prop_sym = token_sym(key_tok);
                expect(TokenKind::Colon);
                const u8 val_reg = compile_expr();
                const u32 ic_site = ctx->ic.allocate_site(prop_sym);
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::SetPropIC,
                    obj_reg,
                    val_reg,
                    static_cast<u8>(ic_site & 0xFFu)
                ));
                if (!match(TokenKind::Comma)) {
                    break;
                }
            }
            expect(TokenKind::RBrace);
            return obj_reg;
        }
        if (t.kind == TokenKind::LBracket) {
            // Array literal `[e0, e1, ...]` lowered to `NewArray` + `ArrayPush`.
            advance();
            const u8 arr_reg = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewArray, arr_reg, 8u, 0u));
            while (peek().kind != TokenKind::RBracket && peek().kind != TokenKind::Eof && !had_error) {
                const u8 elem_reg = compile_expr();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ArrayPush, arr_reg, elem_reg, 0u));
                if (!match(TokenKind::Comma)) {
                    break;
                }
            }
            expect(TokenKind::RBracket);
            return arr_reg;
        }
        if (t.kind == TokenKind::Ident) {
            const Token id_tok = advance();
            const u32 sym = token_sym(id_tok);

            // Built-in `Math.<op>(arg)` intrinsic call lowering.
            if (match_kw(&src[id_tok.offset], id_tok.length, "Math", 4) && match(TokenKind::Dot)) {
                const Token m_tok = advance();
                u8 math_op = 0u;
                if (match_kw(&src[m_tok.offset], m_tok.length, "abs", 3)) math_op = 1u;
                else if (match_kw(&src[m_tok.offset], m_tok.length, "floor", 5)) math_op = 2u;
                else if (match_kw(&src[m_tok.offset], m_tok.length, "ceil", 4)) math_op = 3u;
                else if (match_kw(&src[m_tok.offset], m_tok.length, "round", 5)) math_op = 4u;
                else if (match_kw(&src[m_tok.offset], m_tok.length, "sqrt", 4)) math_op = 5u;
                expect(TokenKind::LParen);
                const u8 arg_reg = compile_expr();
                expect(TokenKind::RParen);
                const u8 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CallMath, dst, math_op, arg_reg));
                return dst;
            }

            // Function call `name(a, b, ...)`
            if (match(TokenKind::LParen)) {
                u32 target_fn = 0xFFFFFFFFu;
                for (u32 fi = 0; fi < ctx->heap.function_count; ++fi) {
                    if (ctx->heap.functions[fi].name_sym == sym) {
                        target_fn = fi;
                        break;
                    }
                }
                const u8 call_base = next_reg;
                u8 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u8 arg_r = compile_expr();
                    const u8 slot_r = alloc_reg();
                    if (arg_r != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_r, 0u));
                    }
                    ++argc;
                    if (!match(TokenKind::Comma)) {
                        break;
                    }
                }
                expect(TokenKind::RParen);
                const u8 dst = alloc_reg();
                if (target_fn != 0xFFFFFFFFu) {
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::Call,
                        dst,
                        static_cast<u8>(target_fn & 0xFFu),
                        call_base
                    ));
                } else {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, dst, 0u, 0u));
                }
                return dst;
            }

            // Variable read (local register or global binding)
            const i32 local_r = find_local(sym);
            u8 base_reg = 0u;
            if (local_r >= 0) {
                base_reg = static_cast<u8>(local_r);
            } else {
                base_reg = alloc_reg();
                const u16 g_slot = ctx->resolve_global_slot(sym);
                ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::GetGlobal, base_reg, g_slot));
            }
            return compile_postfix(base_reg, sym, local_r);
        }

        had_error = true;
        const u8 fallback = alloc_reg();
        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, fallback, 0u, 0u));
        return fallback;
    }

    u8 compile_postfix(u8 base_reg, u32 ident_sym, i32 local_r) noexcept {
        u8 cur = base_reg;
        while (!had_error) {
            if (match(TokenKind::Dot)) {
                const Token prop_tok = advance();
                if (prop_tok.kind != TokenKind::Ident) {
                    had_error = true;
                    break;
                }
                const u32 prop_sym = token_sym(prop_tok);
                // Check for `arr.push(expr)` method call.
                if (match_kw(&src[prop_tok.offset], prop_tok.length, "push", 4) && match(TokenKind::LParen)) {
                    const u8 val_reg = compile_expr();
                    expect(TokenKind::RParen);
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ArrayPush, cur, val_reg, 0u));
                    continue;
                }
                // Check for property assignment `obj.prop = expr`.
                if (match(TokenKind::Assign)) {
                    const u8 rhs = compile_expr();
                    const u32 ic_site = ctx->ic.allocate_site(prop_sym);
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::SetPropIC,
                        cur,
                        rhs,
                        static_cast<u8>(ic_site & 0xFFu)
                    ));
                    cur = rhs;
                    continue;
                }
                const u32 ic_site = ctx->ic.allocate_site(prop_sym);
                const u8 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::GetPropIC,
                    dst,
                    cur,
                    static_cast<u8>(ic_site & 0xFFu)
                ));
                cur = dst;
                continue;
            }
            if (match(TokenKind::LBracket)) {
                const u8 idx_reg = compile_expr();
                expect(TokenKind::RBracket);
                if (match(TokenKind::Assign)) {
                    const u8 rhs = compile_expr();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetElem, cur, idx_reg, rhs));
                    cur = rhs;
                    continue;
                }
                const u8 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, dst, cur, idx_reg));
                cur = dst;
                continue;
            }
            if (match(TokenKind::PlusPlus) || match(TokenKind::MinusMinus)) {
                const bool is_inc = lexer.tokens[cursor - 1u].kind == TokenKind::PlusPlus;
                const u8 one_reg = alloc_reg();
                emit_load_number(one_reg, 1.0);
                const u8 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub,
                    dst,
                    cur,
                    one_reg
                ));
                store_back_ident(ident_sym, local_r, dst);
                cur = dst;
                continue;
            }
            break;
        }
        return cur;
    }

    void store_back_ident(u32 sym, i32 local_r, u8 src_reg) noexcept {
        if (local_r >= 0) {
            if (static_cast<u8>(local_r) != src_reg) {
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::Move,
                    static_cast<u8>(local_r),
                    src_reg,
                    0u
                ));
            }
        } else {
            const u16 g_slot = ctx->resolve_global_slot(sym);
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, src_reg, g_slot));
        }
    }

    u8 compile_unary() noexcept {
        if (match(TokenKind::Minus)) {
            const u8 operand = compile_unary();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Neg, dst, operand, 0u));
            return dst;
        }
        if (match(TokenKind::Bang)) {
            const u8 operand = compile_unary();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Not, dst, operand, 0u));
            return dst;
        }
        return compile_primary();
    }

    u8 compile_mul() noexcept {
        u8 lhs = compile_unary();
        while (peek().kind == TokenKind::Star ||
               peek().kind == TokenKind::Slash ||
               peek().kind == TokenKind::Percent) {
            const TokenKind op = advance().kind;
            const u8 rhs = compile_unary();
            const u8 dst = alloc_reg();
            const AnvilOpcode opc = (op == TokenKind::Star)
                ? AnvilOpcode::Mul
                : (op == TokenKind::Slash ? AnvilOpcode::Div : AnvilOpcode::Mod);
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_add() noexcept {
        u8 lhs = compile_mul();
        while (peek().kind == TokenKind::Plus || peek().kind == TokenKind::Minus) {
            const TokenKind op = advance().kind;
            const u8 rhs = compile_mul();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(
                op == TokenKind::Plus ? AnvilOpcode::Add : AnvilOpcode::Sub,
                dst,
                lhs,
                rhs
            ));
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_shift() noexcept {
        u8 lhs = compile_add();
        while (peek().kind == TokenKind::Shl ||
               peek().kind == TokenKind::Shr ||
               peek().kind == TokenKind::UShr) {
            const TokenKind op = advance().kind;
            const u8 rhs = compile_add();
            const u8 dst = alloc_reg();
            const AnvilOpcode opc = (op == TokenKind::Shl)
                ? AnvilOpcode::Shl
                : (op == TokenKind::Shr ? AnvilOpcode::Shr : AnvilOpcode::UShr);
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_relational() noexcept {
        u8 lhs = compile_shift();
        while (peek().kind == TokenKind::Lt ||
               peek().kind == TokenKind::Le ||
               peek().kind == TokenKind::Gt ||
               peek().kind == TokenKind::Ge) {
            const TokenKind op = advance().kind;
            const u8 rhs = compile_shift();
            const u8 dst = alloc_reg();
            AnvilOpcode opc = AnvilOpcode::CmpLt;
            if (op == TokenKind::Le) opc = AnvilOpcode::CmpLe;
            else if (op == TokenKind::Gt) opc = AnvilOpcode::CmpGt;
            else if (op == TokenKind::Ge) opc = AnvilOpcode::CmpGe;
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_equality() noexcept {
        u8 lhs = compile_relational();
        while (peek().kind == TokenKind::EqEq || peek().kind == TokenKind::BangEq) {
            const TokenKind op = advance().kind;
            const u8 rhs = compile_relational();
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(
                op == TokenKind::EqEq ? AnvilOpcode::CmpEq : AnvilOpcode::CmpNe,
                dst,
                lhs,
                rhs
            ));
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_bitwise() noexcept {
        u8 lhs = compile_equality();
        while (peek().kind == TokenKind::Amp ||
               peek().kind == TokenKind::Caret ||
               peek().kind == TokenKind::Pipe) {
            const TokenKind op = advance().kind;
            const u8 rhs = compile_equality();
            const u8 dst = alloc_reg();
            const AnvilOpcode opc = (op == TokenKind::Amp)
                ? AnvilOpcode::BitAnd
                : (op == TokenKind::Caret ? AnvilOpcode::BitXor : AnvilOpcode::BitOr);
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_logical() noexcept {
        u8 lhs = compile_bitwise();
        while (peek().kind == TokenKind::AndAnd || peek().kind == TokenKind::OrOr) {
            const bool is_and = advance().kind == TokenKind::AndAnd;
            const u8 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, lhs, 0u));
            const u32 patch_pc = ctx->program.emit(AnvilInstruction::make_abx(
                is_and ? AnvilOpcode::JumpIfFalse : AnvilOpcode::JumpIfTrue,
                dst,
                0u
            ));
            const u8 rhs = compile_bitwise();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, rhs, 0u));
            ctx->program.code[patch_pc] = AnvilInstruction::make_abx(
                is_and ? AnvilOpcode::JumpIfFalse : AnvilOpcode::JumpIfTrue,
                dst,
                static_cast<u16>(ctx->program.code_len)
            );
            lhs = dst;
        }
        return lhs;
    }

    u8 compile_expr() noexcept {
        const u8 cond_or_val = compile_logical();
        if (match(TokenKind::Question)) {
            const u8 dst = alloc_reg();
            const u32 jfalse_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_or_val, 0u)
            );
            const u8 then_reg = compile_expr();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, then_reg, 0u));
            const u32 jend_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
            expect(TokenKind::Colon);
            ctx->program.code[jfalse_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                cond_or_val,
                static_cast<u16>(ctx->program.code_len)
            );
            const u8 else_reg = compile_expr();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, else_reg, 0u));
            ctx->program.code[jend_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                static_cast<u16>(ctx->program.code_len)
            );
            return dst;
        }
        return cond_or_val;
    }

    u8 compile_statement() noexcept {
        while (match(TokenKind::Semicolon)) {}
        const Token& t = peek();
        if (t.kind == TokenKind::Eof || t.kind == TokenKind::RBrace) {
            return 0u;
        }

        if (match(TokenKind::LBrace)) {
            u8 last_reg = 0u;
            while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
                last_reg = compile_statement();
            }
            expect(TokenKind::RBrace);
            return last_reg;
        }

        if (match(TokenKind::KwLet) || match(TokenKind::KwConst) || match(TokenKind::KwVar)) {
            const Token id_tok = advance();
            if (id_tok.kind != TokenKind::Ident) {
                had_error = true;
                return 0u;
            }
            const u32 sym = token_sym(id_tok);
            const u8 dst = declare_local(sym);
            if (match(TokenKind::Assign)) {
                const u8 val_reg = compile_expr();
                if (val_reg != dst) {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, val_reg, 0u));
                }
            } else {
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, dst, 0u, 0u));
            }
            if (!in_function) {
                const u16 g_slot = ctx->resolve_global_slot(sym);
                ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, dst, g_slot));
            }
            match(TokenKind::Semicolon);
            return dst;
        }

        if (match(TokenKind::KwReturn)) {
            u8 ret_reg = 0u;
            if (peek().kind != TokenKind::Semicolon &&
                peek().kind != TokenKind::RBrace &&
                peek().kind != TokenKind::Eof) {
                ret_reg = compile_expr();
            } else {
                ret_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, ret_reg, 0u, 0u));
            }
            match(TokenKind::Semicolon);
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, ret_reg, 0u, 0u));
            return ret_reg;
        }

        if (match(TokenKind::KwIf)) {
            expect(TokenKind::LParen);
            const u8 cond_reg = compile_expr();
            expect(TokenKind::RParen);
            const u32 jfalse_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, 0u)
            );
            u8 last_reg = compile_statement();
            if (match(TokenKind::KwElse)) {
                const u32 jend_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
                ctx->program.code[jfalse_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse,
                    cond_reg,
                    static_cast<u16>(ctx->program.code_len)
                );
                last_reg = compile_statement();
                ctx->program.code[jend_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::Jump,
                    0u,
                    static_cast<u16>(ctx->program.code_len)
                );
            } else {
                ctx->program.code[jfalse_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse,
                    cond_reg,
                    static_cast<u16>(ctx->program.code_len)
                );
            }
            return last_reg;
        }

        if (match(TokenKind::KwWhile)) {
            const u32 loop_id = (next_loop_id < 63u) ? next_loop_id++ : 63u;
            const u32 loop_head_pc = ctx->program.code_len;
            ctx->program.emit(AnvilInstruction::make_abc(
                AnvilOpcode::LoopHeader,
                static_cast<u8>(loop_id),
                0u,
                0u
            ));
            expect(TokenKind::LParen);
            const u8 cond_reg = compile_expr();
            expect(TokenKind::RParen);
            const u32 jexit_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, 0u)
            );
            const u8 body_reg = compile_statement();
            ctx->program.emit(AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                static_cast<u16>(loop_head_pc)
            ));
            ctx->program.code[jexit_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                cond_reg,
                static_cast<u16>(ctx->program.code_len)
            );
            return body_reg;
        }

        if (match(TokenKind::KwFor)) {
            expect(TokenKind::LParen);
            if (!match(TokenKind::Semicolon)) {
                compile_statement();
            }
            const u32 loop_id = (next_loop_id < 63u) ? next_loop_id++ : 63u;
            const u32 cond_pc = ctx->program.code_len;
            ctx->program.emit(AnvilInstruction::make_abc(
                AnvilOpcode::LoopHeader,
                static_cast<u8>(loop_id),
                0u,
                0u
            ));
            u8 cond_reg = 0u;
            bool has_cond = false;
            if (!match(TokenKind::Semicolon)) {
                cond_reg = compile_expr();
                has_cond = true;
                expect(TokenKind::Semicolon);
            }
            const u32 jexit_pc = has_cond
                ? ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, 0u))
                : 0u;
            const u32 jbody_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));

            const u32 step_pc = ctx->program.code_len;
            if (peek().kind != TokenKind::RParen) {
                compile_statement_or_assign_no_semi();
            }
            expect(TokenKind::RParen);
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, static_cast<u16>(cond_pc)));

            ctx->program.code[jbody_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                static_cast<u16>(ctx->program.code_len)
            );
            const u8 body_reg = compile_statement();
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, static_cast<u16>(step_pc)));

            if (has_cond) {
                ctx->program.code[jexit_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse,
                    cond_reg,
                    static_cast<u16>(ctx->program.code_len)
                );
            }
            return body_reg;
        }

        if (match(TokenKind::KwFunction)) {
            const Token fn_tok = advance();
            if (fn_tok.kind != TokenKind::Ident) {
                had_error = true;
                return 0u;
            }
            const u32 fn_sym = token_sym(fn_tok);
            expect(TokenKind::LParen);

            // Save enclosing compiler state.
            LocalBinding saved_locals[MAX_REGISTERS_PER_FRAME];
            for (u32 i = 0; i < local_count; ++i) {
                saved_locals[i] = locals[i];
            }
            const u32 saved_local_count = local_count;
            const u8 saved_next_reg = next_reg;
            const bool saved_in_fn = in_function;

            local_count = 0u;
            next_reg = 0u;
            in_function = true;

            u32 arity = 0u;
            while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                const Token p_tok = advance();
                if (p_tok.kind != TokenKind::Ident) {
                    had_error = true;
                    break;
                }
                declare_local(token_sym(p_tok));
                ++arity;
                if (!match(TokenKind::Comma)) {
                    break;
                }
            }
            expect(TokenKind::RParen);

            const u32 jskip_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
            const u32 fn_start_pc = ctx->program.code_len;
            const u32 fn_id = ctx->heap.alloc_function(fn_sym, arity, 0u, fn_start_pc, 0u);

            compile_statement();
            const u8 fallthrough_reg = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, fallthrough_reg, 0u, 0u));
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, fallthrough_reg, 0u, 0u));

            ctx->heap.functions[fn_id].local_count = next_reg;
            ctx->heap.functions[fn_id].code_len = ctx->program.code_len - fn_start_pc;
            ctx->program.code[jskip_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                static_cast<u16>(ctx->program.code_len)
            );

            // Restore enclosing compiler state.
            for (u32 i = 0; i < saved_local_count; ++i) {
                locals[i] = saved_locals[i];
            }
            local_count = saved_local_count;
            next_reg = saved_next_reg;
            in_function = saved_in_fn;
            return 0u;
        }

        const u8 res = compile_statement_or_assign_no_semi();
        match(TokenKind::Semicolon);
        return res;
    }

    u8 compile_statement_or_assign_no_semi() noexcept {
        if (looks_like_host_text()) {
            return compile_host_text();
        }
        if (peek().kind == TokenKind::Ident) {
            const TokenKind second = peek(1u).kind;
            if (second == TokenKind::Assign ||
                second == TokenKind::PlusEq ||
                second == TokenKind::MinusEq ||
                second == TokenKind::StarEq) {
                const Token id_tok = advance();
                const TokenKind assign_kind = advance().kind;
                const u32 sym = token_sym(id_tok);
                const i32 local_r = find_local(sym);
                const u8 rhs = compile_expr();

                if (assign_kind == TokenKind::Assign) {
                    store_back_ident(sym, local_r, rhs);
                    return rhs;
                }

                u8 cur_reg = 0u;
                if (local_r >= 0) {
                    cur_reg = static_cast<u8>(local_r);
                } else {
                    cur_reg = alloc_reg();
                    const u16 g_slot = ctx->resolve_global_slot(sym);
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::GetGlobal, cur_reg, g_slot));
                }
                const u8 dst = alloc_reg();
                const AnvilOpcode opc = (assign_kind == TokenKind::PlusEq)
                    ? AnvilOpcode::Add
                    : (assign_kind == TokenKind::MinusEq ? AnvilOpcode::Sub : AnvilOpcode::Mul);
                ctx->program.emit(AnvilInstruction::make_abc(opc, dst, cur_reg, rhs));
                store_back_ident(sym, local_r, dst);
                return dst;
            }
        }
        return compile_expr();
    }
};

extern "C" u32 anvil_cpp_vm_compiler_bytes() {
    return static_cast<u32>(sizeof(AnvilCompiler));
}

extern "C" u32 anvil_cpp_vm_compiler_align() {
    return static_cast<u32>(alignof(AnvilCompiler));
}

bool compile_owned(AnvilVmContext& ctx, void* compiler_memory, const char* src, u32 len) noexcept {
    if (!src || !compiler_memory) {
        return false;
    }
    // The caller owns this workspace. It is not a process-global compiler.
    auto& g_compiler = *static_cast<AnvilCompiler*>(compiler_memory);
    g_compiler.ctx = &ctx;
    g_compiler.src = src;
    g_compiler.cursor = 0u;
    g_compiler.local_count = 0u;
    g_compiler.next_reg = 0u;
    g_compiler.next_loop_id = 0u;
    g_compiler.in_function = false;
    g_compiler.had_error = false;

    if (!g_compiler.lexer.tokenize(src, len)) {
        return false;
    }

    u8 last_reg = 0u;
    while (g_compiler.peek().kind != TokenKind::Eof && !g_compiler.had_error) {
        if (g_compiler.peek().kind == TokenKind::RBrace) {
            // A '}' with no open block. compile_statement returns without
            // consuming it, so this loop would never end. Reject the script;
            // the Rust interpreter runs it instead.
            g_compiler.had_error = true;
            break;
        }
        last_reg = g_compiler.compile_statement();
    }
    ctx.program.emit(AnvilInstruction::make_abc(AnvilOpcode::Halt, last_reg, 0u, 0u));
    return !g_compiler.had_error && ctx.program.compile_ok;
}

} // namespace

bool compile_js_to_bytecode(AnvilVmContext& ctx, void* compiler_memory, const char* src, u32 len) noexcept {
    return compile_owned(ctx, compiler_memory, src, len);
}

} // namespace anvil_vm
