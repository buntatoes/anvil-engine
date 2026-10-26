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
    KwDo       = 54u,
    KwBreak    = 55u,
    KwContinue = 56u,
    LooseEq    = 57u,
    LooseNe    = 58u,
};

struct Token {
    TokenKind kind;
    u32 offset;
    u32 length;
    f64 num_val;
};

// The lexer's tokens live in a compiler-side arena, not a fixed array, so a
// script is no longer capped at a token count. The arena is reserved after the
// compiler struct and, like the heap arena, only pages what a compile touches.
constexpr u32 ANVIL_COMPILER_ARENA_RESERVE_BYTES = 256u * 1024u * 1024u;
// Tokens the arena holds to start with; it doubles from here as needed.
constexpr u32 INIT_TOKEN_BUFFER = 1024u;

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

// Names served by the Rust event loop (promises, async functions, and timers).
// The numeric VM has no job queue, so a script that names one is rejected and
// runs on the Rust interpreter instead of having the call dropped.
inline bool is_event_loop_word(const char* s, u32 len) noexcept {
    return match_kw(s, len, "async", 5) || match_kw(s, len, "await", 5) ||
           match_kw(s, len, "Promise", 7) || match_kw(s, len, "setTimeout", 10) ||
           match_kw(s, len, "setInterval", 11) || match_kw(s, len, "clearTimeout", 12) ||
           match_kw(s, len, "clearInterval", 13) || match_kw(s, len, "queueMicrotask", 14) ||
           match_kw(s, len, "requestAnimationFrame", 21);
}

// Words of the language this compiler does not implement. Without this
// check they would read as plain names and the script would run with a
// different meaning, so a script that uses one is rejected instead.
inline bool is_unsupported_word(const char* s, u32 len) noexcept {
    static const char* const words[] = {
        "this", "typeof", "instanceof", "in", "delete", "void", "throw", "try",
        "catch", "finally", "switch", "case", "default", "class", "extends",
        "super", "with", "yield", "import", "export", "debugger", "enum",
    };
    for (const char* w : words) {
        u32 w_len = 0u;
        while (w[w_len] != '\0') {
            ++w_len;
        }
        if (match_kw(s, len, w, w_len)) {
            return true;
        }
    }
    return false;
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
    // The token buffer is a block in the compiler arena, grown as the source
    // needs. `arena` is set by the compiler before tokenizing.
    AnvilArena* arena;
    u32 tokens_off;
    u32 tokens_cap;
    u32 count;

    Token* tokens() noexcept {
        return reinterpret_cast<Token*>(arena->at(tokens_off));
    }
    const Token* tokens() const noexcept {
        return reinterpret_cast<const Token*>(arena->at(tokens_off));
    }

    // Grow the token block to hold at least `need` tokens, copying what is
    // there and returning the old block to the free list. False when the
    // compiler arena cannot grow any further.
    bool ensure_tokens(u32 need) noexcept {
        if (need <= tokens_cap) {
            return true;
        }
        u32 new_cap = tokens_cap == 0u ? INIT_TOKEN_BUFFER : tokens_cap;
        while (new_cap < need) {
            new_cap *= 2u;
        }
        bool over = false;
        const u32 new_off = arena->alloc(new_cap * static_cast<u32>(sizeof(Token)), &over);
        if (new_off == NO_SLOT) {
            return false;
        }
        Token* dst = reinterpret_cast<Token*>(arena->at(new_off));
        const Token* old = reinterpret_cast<const Token*>(arena->at(tokens_off));
        for (u32 i = 0u; i < count; ++i) {
            dst[i] = old[i];
        }
        const u32 old_off = tokens_off;
        const u32 old_cap = tokens_cap;
        tokens_off = new_off;
        tokens_cap = new_cap;
        if (old_cap != 0u) {
            arena->free(old_off, old_cap * static_cast<u32>(sizeof(Token)));
        }
        return true;
    }
    // Why tokenize returned false, and the source bytes it stopped at.
    u32 fail_code;
    const char* fail_text;
    u32 fail_offset;
    u32 fail_len;

    bool fail(u32 code, const char* text, u32 offset, u32 length) noexcept {
        fail_code = code;
        fail_text = text;
        fail_offset = offset;
        fail_len = length;
        return false;
    }

    bool tokenize(const char* src, u32 len) noexcept {
        count = 0u;
        tokens_off = 0u;
        tokens_cap = 0u;
        fail_code = VM_OK;
        fail_text = "";
        fail_offset = 0u;
        fail_len = 0u;
        u32 pos = 0u;
        while (pos < len) {
            if (!ensure_tokens(count + 2u)) {
                return fail(VM_REFUSE_TOKENS, "source", pos, 0u);
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
                u32 dots = 0u;
                while (pos < len && (is_digit(src[pos]) || src[pos] == '.')) {
                    if (src[pos] == '.') {
                        ++dots;
                    }
                    ++pos;
                }
                // Exponents, hex, separators and `1.2.3` are not read here.
                if (dots > 1u || (pos < len && is_ident_cont(src[pos]))) {
                    while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) {
                        ++pos;
                    }
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                }
                tokens()[count++] = Token{
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
                    // The pool keeps the source bytes, so an escape would
                    // keep its backslash. A line break ends the literal.
                    if (src[pos] == '\\' || src[pos] == '\n') {
                        return fail(
                            VM_REFUSE_UNSUPPORTED,
                            "unsupported string literal",
                            start - 1u,
                            pos - start + 2u
                        );
                    }
                    ++pos;
                }
                const u32 str_len = pos - start;
                if (pos >= len) {
                    return fail(VM_REFUSE_SYNTAX, "unterminated string", start - 1u, pos - start + 1u);
                }
                ++pos;
                tokens()[count++] = Token{TokenKind::String, start, str_len, 0.0};
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
                else if (match_kw(s, id_len, "do", 2)) kind = TokenKind::KwDo;
                else if (match_kw(s, id_len, "break", 5)) kind = TokenKind::KwBreak;
                else if (match_kw(s, id_len, "continue", 8)) kind = TokenKind::KwContinue;
                if (is_event_loop_word(s, id_len) || is_unsupported_word(s, id_len)) {
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported word", start, id_len);
                }
                tokens()[count++] = Token{kind, start, id_len, 0.0};
                continue;
            }

            // Multi-character operators
            const char n1 = (pos + 1u < len) ? src[pos + 1u] : '\0';
            const char n2 = (pos + 2u < len) ? src[pos + 2u] : '\0';

            // Arrows, optional chaining and `??` are not compiled. Reading
            // them as `=`, `>` or `?` would change what the script means.
            if ((c == '=' && n1 == '>') || (c == '?' && (n1 == '.' || n1 == '?'))) {
                return fail(VM_REFUSE_UNSUPPORTED, "unsupported operator", pos, 2u);
            }
            if (c == '=' && n1 == '=' && n2 == '=') {
                tokens()[count++] = Token{TokenKind::EqEq, pos, 3u, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '!' && n1 == '=' && n2 == '=') {
                tokens()[count++] = Token{TokenKind::BangEq, pos, 3u, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '>' && n1 == '>' && n2 == '>') {
                tokens()[count++] = Token{TokenKind::UShr, pos, 3u, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '=' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::LooseEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '!' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::LooseNe, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '<' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::Le, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '>' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::Ge, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '<' && n1 == '<') {
                tokens()[count++] = Token{TokenKind::Shl, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '>' && n1 == '>') {
                tokens()[count++] = Token{TokenKind::Shr, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '&' && n1 == '&') {
                tokens()[count++] = Token{TokenKind::AndAnd, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '|' && n1 == '|') {
                tokens()[count++] = Token{TokenKind::OrOr, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '+' && n1 == '+') {
                tokens()[count++] = Token{TokenKind::PlusPlus, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '-' && n1 == '-') {
                tokens()[count++] = Token{TokenKind::MinusMinus, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '+' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::PlusEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '-' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::MinusEq, pos, 2u, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '*' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::StarEq, pos, 2u, 0.0};
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
                default: return fail(VM_REFUSE_UNSUPPORTED, "unsupported character", pos, 1u);
            }
            tokens()[count++] = Token{single, pos, 1u, 0.0};
            ++pos;
        }
        tokens()[count++] = Token{TokenKind::Eof, len, 0u, 0.0};
        return true;
    }
};

constexpr u32 NO_DEPTH = 0xFFFFFFFFu;

// Loops nested inside each other, and `break` or `continue` jumps in one
// loop, that the compiler keeps track of.
constexpr u32 MAX_LOOP_NESTING = 16u;
constexpr u32 MAX_LOOP_JUMPS = 32u;

// Jumps out of one loop that wait for their target to be known.
struct LoopJumps {
    u32 breaks[MAX_LOOP_JUMPS];
    u32 break_count;
    u32 continues[MAX_LOOP_JUMPS];
    u32 continue_count;
};

// Counts one level of block scope for as long as it is in scope.
struct BlockScope {
    u32& depth;
    explicit BlockScope(u32& d) noexcept : depth(d) {
        ++depth;
    }
    ~BlockScope() {
        --depth;
    }
    BlockScope(const BlockScope&) = delete;
    BlockScope& operator=(const BlockScope&) = delete;
};

struct LocalBinding {
    u32 name_sym;
    u16 reg;
};

struct AnvilCompiler {
    AnvilVmContext* ctx;
    const char* src;
    // Compile-time storage (the token buffer, and later the syntax tree) lives
    // in this arena, which sits in the compiler memory after the struct. It is
    // separate from the runtime heap, so a large source does not eat the run's
    // heap budget.
    AnvilArena arena;
    AnvilLexer lexer;
    u32 cursor;
    // The locals table is a block in the compiler arena (like the token
    // buffer), grown as a function declares more. `local_count` is how many are
    // live; `local_floor` is the first index the function being compiled owns,
    // so an inner function sees only its own locals, not the enclosing ones.
    u32 locals_off;
    u32 locals_cap;
    u32 local_count;
    u32 local_floor;
    u16 next_reg;
    // Registers below this hold the current function's parameters and
    // locals. Registers from here up are temporaries, free again after
    // each statement.
    u16 reg_floor;
    // Highest register count the current function has used.
    u16 high_reg;
    u32 next_loop_id;
    // Loops around the statement being compiled.
    LoopJumps loops[MAX_LOOP_NESTING];
    u32 loop_depth;
    bool in_function;
    bool had_error;
    // Braces and for-loop headers around the statement being compiled.
    u32 block_depth;

    // Native-stack guard for the recursive descent. stack_base is the stack
    // address where the compile began; stack_budget is how far below it the
    // parse may grow before it is refused as too deeply nested. The stack
    // grows down, so the bytes used are stack_base minus the current frame's
    // address.
    u64 stack_base;
    u32 stack_budget;

    bool within_stack_budget() noexcept {
        char probe;
        const u64 here = reinterpret_cast<u64>(&probe);
        return here >= stack_base ? true : (stack_base - here) < stack_budget;
    }

    // Refuse a script whose nesting has grown the native stack past its
    // budget, so a pathologically deep input stops with a reason rather than
    // overflowing the stack. Returns true when it tripped.
    bool refuse_if_too_deep() noexcept {
        if (within_stack_budget()) {
            return false;
        }
        reject_limit(VM_REFUSE_NESTING, "nesting", stack_budget, "bytes of compiler stack");
        return true;
    }

    const Token& peek(u32 ahead = 0u) const noexcept {
        const u32 idx = (cursor + ahead < lexer.count) ? (cursor + ahead) : (lexer.count - 1u);
        return lexer.tokens()[idx];
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

    u16 compile_host_text() noexcept {
        advance();
        advance();
        advance();
        advance();
        const Token id_tok = advance();
        advance();
        advance();
        advance();
        advance();
        if (ctx->host_id_count >= 8u) {
            reject_limit(VM_REFUSE_HOST_SLOTS, "text target", 8u, "elements");
            return 0u;
        }
        if (id_tok.length == 0u || id_tok.length >= 64u) {
            reject_name(VM_REFUSE_UNSUPPORTED, "unsupported element id", id_tok);
            return 0u;
        }
        for (u32 i = 0u; i < id_tok.length; ++i) {
            if (src[id_tok.offset + i] == '\\') {
                reject_name(VM_REFUSE_UNSUPPORTED, "unsupported element id", id_tok);
                return 0u;
            }
        }
        const u32 slot = ctx->host_id_count++;
        for (u32 i = 0u; i < id_tok.length; ++i) {
            ctx->host_ids[slot][i] = src[id_tok.offset + i];
        }
        ctx->host_ids[slot][id_tok.length] = '\0';
        ctx->host_id_lens[slot] = id_tok.length;
        const u16 rhs = compile_expr();
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
        reject_syntax();
        return false;
    }

    // Refuse the script and record why, with the token the compiler is at.
    void reject_here(u32 code, const char* text) noexcept {
        had_error = true;
        if (!ctx->fail(code, text)) {
            return;
        }
        const Token& t = peek();
        if (t.kind == TokenKind::Eof) {
            ctx->error.add(" at end of script");
        } else {
            ctx->error.add(" near '");
            ctx->error.add_source(&src[t.offset], t.length);
            ctx->error.add("'");
        }
    }

    void reject_syntax() noexcept {
        reject_here(VM_REFUSE_SYNTAX, "syntax the VM does not compile");
    }

    void reject_name(u32 code, const char* text, const Token& t) noexcept {
        had_error = true;
        ctx->fail_name(code, text, &src[t.offset], t.length);
    }

    void reject_limit(u32 code, const char* what, u32 limit, const char* unit) noexcept {
        had_error = true;
        ctx->fail_limit(code, what, limit, unit);
    }

    bool begin_loop() noexcept {
        if (loop_depth >= MAX_LOOP_NESTING) {
            reject_limit(VM_REFUSE_LOOPS, "loop nesting", MAX_LOOP_NESTING, "loops");
            return false;
        }
        loops[loop_depth].break_count = 0u;
        loops[loop_depth].continue_count = 0u;
        ++loop_depth;
        return true;
    }

    // Point the loop's `continue` jumps at continue_pc and its `break`
    // jumps at the current end of the code.
    void end_loop(u32 continue_pc) noexcept {
        if (loop_depth == 0u) {
            return;
        }
        --loop_depth;
        const LoopJumps& jumps = loops[loop_depth];
        const u32 break_pc = ctx->program.code_len;
        for (u32 i = 0u; i < jumps.break_count && i < MAX_LOOP_JUMPS; ++i) {
            if (jumps.breaks[i] < ctx->program.code_len) {
                ctx->program.code()[jumps.breaks[i]] = AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, break_pc);
            }
        }
        for (u32 i = 0u; i < jumps.continue_count && i < MAX_LOOP_JUMPS; ++i) {
            if (jumps.continues[i] < ctx->program.code_len) {
                ctx->program.code()[jumps.continues[i]] =
                    AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, continue_pc);
            }
        }
    }

    // `break` or `continue`: a jump whose target end_loop fills in.
    void compile_loop_jump(bool is_break) noexcept {
        if (loop_depth == 0u) {
            reject_here(VM_REFUSE_SYNTAX, is_break ? "break outside a loop" : "continue outside a loop");
            return;
        }
        if (peek().kind == TokenKind::Ident) {
            reject_here(VM_REFUSE_UNSUPPORTED, "labels are not compiled");
            return;
        }
        LoopJumps& jumps = loops[loop_depth - 1u];
        u32& count = is_break ? jumps.break_count : jumps.continue_count;
        if (count >= MAX_LOOP_JUMPS) {
            reject_limit(VM_REFUSE_LOOPS, is_break ? "break" : "continue", MAX_LOOP_JUMPS, "jumps per loop");
            return;
        }
        const u32 pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
        (is_break ? jumps.breaks : jumps.continues)[count] = pc;
        ++count;
        match(TokenKind::Semicolon);
    }

    u32 find_function(u32 sym) const noexcept {
        for (u32 i = 0; i < ctx->heap.function_count; ++i) {
            if (ctx->heap.functions()[i].name_sym == sym) {
                return i;
            }
        }
        return NO_SLOT;
    }

    // Make every function the script declares before compiling any code,
    // so a call may come before the declaration, as in JavaScript. Only
    // top-level declarations are hoisted this way; one inside a block or a
    // function has different scope rules and refuses the script.
    void hoist_functions() noexcept {
        u32 depth = 0u;
        for (u32 i = 0u; i < lexer.count && !had_error; ++i) {
            const Token& t = lexer.tokens()[i];
            if (t.kind == TokenKind::LBrace) {
                ++depth;
                continue;
            }
            if (t.kind == TokenKind::RBrace) {
                if (depth > 0u) {
                    --depth;
                }
                continue;
            }
            if (t.kind != TokenKind::KwFunction) {
                continue;
            }
            const TokenKind before = i > 0u ? lexer.tokens()[i - 1u].kind : TokenKind::Semicolon;
            if (depth != 0u || (before != TokenKind::Semicolon && before != TokenKind::RBrace)) {
                const Token& shown = (i + 1u < lexer.count) ? lexer.tokens()[i + 1u] : t;
                reject_name(VM_REFUSE_UNSUPPORTED, "function declared inside a block or function", shown);
                return;
            }
            if (i + 2u >= lexer.count || lexer.tokens()[i + 1u].kind != TokenKind::Ident ||
                lexer.tokens()[i + 2u].kind != TokenKind::LParen) {
                reject_name(VM_REFUSE_UNSUPPORTED, "unsupported function expression", t);
                return;
            }
            const Token& name = lexer.tokens()[i + 1u];
            const u32 sym = token_sym(name);
            if (had_error) {
                return;
            }
            if (find_function(sym) != NO_SLOT) {
                reject_name(VM_REFUSE_UNSUPPORTED, "function declared twice", name);
                return;
            }
            u32 arity = 0u;
            for (u32 j = i + 3u; j < lexer.count && lexer.tokens()[j].kind != TokenKind::RParen; ++j) {
                if (lexer.tokens()[j].kind == TokenKind::Ident) {
                    ++arity;
                }
            }
            if (ctx->heap.alloc_function(sym, arity, 0u, 0u, 0u) == NO_SLOT) {
                reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
                return;
            }
        }
    }

    u32 new_site(u32 prop_sym) noexcept {
        const u32 site = ctx->ic.allocate_site(prop_sym);
        if (site == NO_SLOT) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return 0u;
        }
        // The site index rides in a 16-bit operand, so a script may address up to
        // 65,535 inline-cache sites. Past that, refuse with a reason rather than
        // let the index wrap onto another site.
        if (site > 0xFFFFu) {
            reject_limit(VM_REFUSE_SITES, "inline cache site", 0xFFFFu, "sites per script");
            return 0u;
        }
        return site;
    }

    u16 global_slot(u32 sym) noexcept {
        const u16 slot = ctx->resolve_global_slot(sym);
        if (slot == NO_GLOBAL) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return 0u;
        }
        return slot;
    }

    // After a whole expression, a `(`, `[` or `.` would make it a call or a
    // member access that this compiler did not read. Reject the script
    // rather than run the rest as a separate statement.
    void reject_if_expression_continues() noexcept {
        const TokenKind k = peek().kind;
        if (k == TokenKind::LParen || k == TokenKind::LBracket || k == TokenKind::Dot) {
            reject_here(VM_REFUSE_UNSUPPORTED, "unsupported expression");
        }
    }

    u16 alloc_reg() noexcept {
        if (next_reg >= MAX_REGISTERS_PER_FRAME) {
            reject_limit(VM_REFUSE_REGISTERS, "register", MAX_REGISTERS_PER_FRAME, "registers per function");
            return MAX_REGISTERS_PER_FRAME - 1u;
        }
        const u16 r = next_reg++;
        if (next_reg > high_reg) {
            high_reg = next_reg;
        }
        return r;
    }

    LocalBinding* locals() noexcept {
        return reinterpret_cast<LocalBinding*>(arena.at(locals_off));
    }
    const LocalBinding* locals() const noexcept {
        return reinterpret_cast<const LocalBinding*>(arena.at(locals_off));
    }

    // Grow the locals block to hold at least `need` entries, copying what is
    // there and returning the old block to the free list. False when the
    // compiler arena cannot grow any further.
    bool ensure_locals(u32 need) noexcept {
        if (need <= locals_cap) {
            return true;
        }
        u32 new_cap = locals_cap == 0u ? 16u : locals_cap;
        while (new_cap < need) {
            new_cap *= 2u;
        }
        bool over = false;
        const u32 new_off = arena.alloc(new_cap * static_cast<u32>(sizeof(LocalBinding)), &over);
        if (new_off == NO_SLOT) {
            return false;
        }
        LocalBinding* dst = reinterpret_cast<LocalBinding*>(arena.at(new_off));
        const LocalBinding* old = reinterpret_cast<const LocalBinding*>(arena.at(locals_off));
        for (u32 i = 0u; i < local_count; ++i) {
            dst[i] = old[i];
        }
        const u32 old_off = locals_off;
        const u32 old_cap = locals_cap;
        locals_off = new_off;
        locals_cap = new_cap;
        if (old_cap != 0u) {
            arena.free(old_off, old_cap * static_cast<u32>(sizeof(LocalBinding)));
        }
        return true;
    }

    i32 find_local(u32 sym) const noexcept {
        for (i32 i = static_cast<i32>(local_count) - 1; i >= static_cast<i32>(local_floor); --i) {
            if (locals()[i].name_sym == sym) {
                return static_cast<i32>(locals()[i].reg);
            }
        }
        return -1;
    }

    u16 declare_local(u32 sym) noexcept {
        const i32 existing = find_local(sym);
        if (existing >= 0) {
            return static_cast<u16>(existing);
        }
        const u16 r = alloc_reg();
        if (ensure_locals(local_count + 1u)) {
            locals()[local_count++] = LocalBinding{sym, r};
        } else {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
        }
        if (r + 1u > reg_floor) {
            reg_floor = static_cast<u16>(r + 1u);
        }
        return r;
    }

    u32 intern_name(const char* name, u32 len) noexcept {
        const u32 sym = ctx->heap.intern_string(name, len);
        if (sym == NO_SLOT) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return 0u;
        }
        return sym;
    }

    u32 token_sym(const Token& t) noexcept {
        const u32 sym = ctx->heap.intern_string(&src[t.offset], t.length);
        if (sym == NO_SLOT) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return 0u;
        }
        return sym;
    }

    void emit_load_undefined(u16 dst) noexcept {
        const u16 cidx = ctx->program.add_constant(AnvilValue::make_undefined());
        ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
    }

    void emit_load_number(u16 dst, f64 n) noexcept {
        const i32 as_int = (n >= -32768.0 && n <= 32767.0) ? static_cast<i32>(n) : 0x7FFFFFFF;
        if (static_cast<f64>(as_int) == n) {
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

    u16 compile_primary() noexcept {
        const Token& t = peek();
        if (t.kind == TokenKind::Number) {
            advance();
            const u16 dst = alloc_reg();
            emit_load_number(dst, t.num_val);
            return dst;
        }
        if (t.kind == TokenKind::KwTrue) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadTrue, dst, 0u, 0u));
            return dst;
        }
        if (t.kind == TokenKind::KwFalse) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadFalse, dst, 0u, 0u));
            return dst;
        }
        if (t.kind == TokenKind::KwNull) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, dst, 0u, 0u));
            return dst;
        }
        if (t.kind == TokenKind::String) {
            advance();
            const u32 sym = token_sym(t);
            const u16 dst = alloc_reg();
            const u16 cidx = ctx->program.add_constant(AnvilValue::make_string(sym));
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
            return dst;
        }
        if (t.kind == TokenKind::LParen) {
            advance();
            const u16 inner = compile_expr();
            expect(TokenKind::RParen);
            return inner;
        }
        if (t.kind == TokenKind::LBrace) {
            // Object literal `{ key: expr, ... }` lowered to `NewObject` + `SetPropIC`.
            advance();
            const u16 obj_reg = alloc_reg();
            // Each property value is a temporary once it is stored, so free the
            // registers above the object after every store. A literal with many
            // properties then needs only a couple of registers, not one per key.
            const u16 obj_mark = next_reg;
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewObject, obj_reg, 0u, 0u));
            while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
                const Token& key_tok = advance();
                if (key_tok.kind != TokenKind::Ident && key_tok.kind != TokenKind::String) {
                    reject_syntax();
                    break;
                }
                const u32 prop_sym = token_sym(key_tok);
                expect(TokenKind::Colon);
                const u16 val_reg = compile_expr();
                const u32 ic_site = new_site(prop_sym);
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::SetPropIC,
                    obj_reg,
                    val_reg,
                    static_cast<u16>(ic_site)
                ));
                if (next_reg > obj_mark) {
                    next_reg = obj_mark;
                }
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
            const u16 arr_reg = alloc_reg();
            // As with object literals, each element is a temporary once pushed,
            // so free the registers above the array after every push.
            const u16 arr_mark = next_reg;
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewArray, arr_reg, 8u, 0u));
            while (peek().kind != TokenKind::RBracket && peek().kind != TokenKind::Eof && !had_error) {
                const u16 elem_reg = compile_expr();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ArrayPush, arr_reg, elem_reg, 0u));
                if (next_reg > arr_mark) {
                    next_reg = arr_mark;
                }
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
                if (math_op == 0u) {
                    reject_name(VM_REFUSE_UNSUPPORTED, "unsupported Math function", m_tok);
                }
                expect(TokenKind::LParen);
                const u16 arg_reg = compile_expr();
                expect(TokenKind::RParen);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CallMath, dst, math_op, arg_reg));
                return dst;
            }

            // Function call `name(a, b, ...)`
            if (match(TokenKind::LParen)) {
                u32 target_fn = 0xFFFFFFFFu;
                for (u32 fi = 0; fi < ctx->heap.function_count; ++fi) {
                    if (ctx->heap.functions()[fi].name_sym == sym) {
                        target_fn = fi;
                        break;
                    }
                }
                // A call to a name that is not a function declared in this
                // script (console.log, a page function, a local holding a
                // value) cannot run here.
                if (target_fn == 0xFFFFFFFFu) {
                    reject_name(VM_REFUSE_UNKNOWN_NAME, "call to a function not declared in this script", id_tok);
                } else if (find_local(sym) >= 0) {
                    reject_name(VM_REFUSE_UNSUPPORTED, "call through a variable", id_tok);
                }
                // Every argument is compiled first. Their values are then
                // copied into a run of registers above all of them, so an
                // argument's temporaries cannot overwrite an earlier one.
                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_expr();
                    if (argc < MAX_CALL_ARGS) {
                        arg_regs[argc] = arg_r;
                    } else {
                        reject_limit(VM_REFUSE_REGISTERS, "call argument", MAX_CALL_ARGS, "arguments per call");
                    }
                    ++argc;
                    if (!match(TokenKind::Comma)) {
                        break;
                    }
                }
                expect(TokenKind::RParen);
                // Extra arguments are evaluated and dropped. Missing ones
                // are filled in.
                const u32 arity = (target_fn < ctx->heap.function_count) ? ctx->heap.functions()[target_fn].arity : 0u;
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < arity && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (i < argc) {
                        if (arg_regs[i] != slot_r) {
                            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                        }
                    } else {
                        emit_load_undefined(slot_r);
                    }
                }
                const u16 dst = alloc_reg();
                if (target_fn != 0xFFFFFFFFu) {
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::Call,
                        dst,
                        static_cast<u16>(target_fn),
                        call_base
                    ));
                }
                return dst;
            }

            // Variable read (local register or global binding)
            const i32 local_r = find_local(sym);
            if (local_r < 0 && match_kw(&src[id_tok.offset], id_tok.length, "undefined", 9)) {
                const u16 dst = alloc_reg();
                emit_load_undefined(dst);
                return compile_postfix(dst, sym, -1);
            }
            u16 base_reg = 0u;
            if (local_r >= 0) {
                base_reg = static_cast<u16>(local_r);
            } else {
                base_reg = alloc_reg();
                const u16 g_slot = global_slot(sym);
                ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::GetGlobal, base_reg, g_slot));
            }
            return compile_postfix(base_reg, sym, local_r);
        }

        reject_syntax();
        const u16 fallback = alloc_reg();
        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, fallback, 0u, 0u));
        return fallback;
    }

    u16 compile_postfix(u16 base_reg, u32 ident_sym, i32 local_r) noexcept {
        u16 cur = base_reg;
        while (!had_error) {
            if (match(TokenKind::Dot)) {
                const Token prop_tok = advance();
                if (prop_tok.kind != TokenKind::Ident) {
                    reject_syntax();
                    break;
                }
                const u32 prop_sym = token_sym(prop_tok);
                // Check for `arr.push(expr)` method call.
                if (match_kw(&src[prop_tok.offset], prop_tok.length, "push", 4) && match(TokenKind::LParen)) {
                    const u16 val_reg = compile_expr();
                    expect(TokenKind::RParen);
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ArrayPush, cur, val_reg, 0u));
                    // push gives the new length.
                    const u32 len_site = new_site(intern_name("length", 6u));
                    const u16 len_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::GetPropIC,
                        len_reg,
                        cur,
                        static_cast<u16>(len_site)
                    ));
                    cur = len_reg;
                    continue;
                }
                // Method calls other than push are not compiled.
                if (peek().kind == TokenKind::LParen) {
                    reject_name(VM_REFUSE_UNSUPPORTED, "unsupported method call", prop_tok);
                    break;
                }
                // Check for property assignment `obj.prop = expr`.
                if (match(TokenKind::Assign)) {
                    const u16 rhs = compile_expr();
                    const u32 ic_site = new_site(prop_sym);
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::SetPropIC,
                        cur,
                        rhs,
                        static_cast<u16>(ic_site)
                    ));
                    cur = rhs;
                    continue;
                }
                const u32 ic_site = new_site(prop_sym);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::GetPropIC,
                    dst,
                    cur,
                    static_cast<u16>(ic_site)
                ));
                cur = dst;
                continue;
            }
            if (match(TokenKind::LBracket)) {
                const u16 idx_reg = compile_expr();
                expect(TokenKind::RBracket);
                if (match(TokenKind::Assign)) {
                    const u16 rhs = compile_expr();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetElem, cur, idx_reg, rhs));
                    cur = rhs;
                    continue;
                }
                if (peek().kind == TokenKind::LParen) {
                    reject_here(VM_REFUSE_UNSUPPORTED, "unsupported call through an element");
                    break;
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, dst, cur, idx_reg));
                cur = dst;
                continue;
            }
            if (peek().kind == TokenKind::PlusPlus || peek().kind == TokenKind::MinusMinus) {
                // Only a plain variable is compiled. `o.n++` or `a[i]++`
                // would need the store to go back to the property.
                if (cur != base_reg) {
                    reject_here(VM_REFUSE_UNSUPPORTED, "++ or -- on something other than a variable");
                    break;
                }
                const bool is_inc = advance().kind == TokenKind::PlusPlus;
                // The value of `x++` is the old value as a number.
                const u16 zero_reg = alloc_reg();
                emit_load_number(zero_reg, 0.0);
                const u16 old_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Sub, old_reg, cur, zero_reg));
                const u16 one_reg = alloc_reg();
                emit_load_number(one_reg, 1.0);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub,
                    dst,
                    old_reg,
                    one_reg
                ));
                store_back_ident(ident_sym, local_r, dst);
                cur = old_reg;
                break;
            }
            break;
        }
        return cur;
    }

    void store_back_ident(u32 sym, i32 local_r, u16 src_reg) noexcept {
        if (local_r >= 0) {
            if (static_cast<u16>(local_r) != src_reg) {
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::Move,
                    static_cast<u16>(local_r),
                    src_reg,
                    0u
                ));
            }
        } else {
            const u16 g_slot = global_slot(sym);
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, src_reg, g_slot));
        }
    }

    u16 compile_unary() noexcept {
        if (refuse_if_too_deep()) {
            return 0u;
        }
        if (match(TokenKind::Minus)) {
            const u16 operand = compile_unary();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Neg, dst, operand, 0u));
            return dst;
        }
        if (match(TokenKind::Bang)) {
            const u16 operand = compile_unary();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Not, dst, operand, 0u));
            return dst;
        }
        return compile_primary();
    }

    u16 compile_mul() noexcept {
        u16 lhs = compile_unary();
        while (peek().kind == TokenKind::Star ||
               peek().kind == TokenKind::Slash ||
               peek().kind == TokenKind::Percent) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_unary();
            const u16 dst = alloc_reg();
            const AnvilOpcode opc = (op == TokenKind::Star)
                ? AnvilOpcode::Mul
                : (op == TokenKind::Slash ? AnvilOpcode::Div : AnvilOpcode::Mod);
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_add() noexcept {
        u16 lhs = compile_mul();
        while (peek().kind == TokenKind::Plus || peek().kind == TokenKind::Minus) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_mul();
            const u16 dst = alloc_reg();
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

    u16 compile_shift() noexcept {
        u16 lhs = compile_add();
        while (peek().kind == TokenKind::Shl ||
               peek().kind == TokenKind::Shr ||
               peek().kind == TokenKind::UShr) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_add();
            const u16 dst = alloc_reg();
            const AnvilOpcode opc = (op == TokenKind::Shl)
                ? AnvilOpcode::Shl
                : (op == TokenKind::Shr ? AnvilOpcode::Shr : AnvilOpcode::UShr);
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_relational() noexcept {
        u16 lhs = compile_shift();
        while (peek().kind == TokenKind::Lt ||
               peek().kind == TokenKind::Le ||
               peek().kind == TokenKind::Gt ||
               peek().kind == TokenKind::Ge) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_shift();
            const u16 dst = alloc_reg();
            AnvilOpcode opc = AnvilOpcode::CmpLt;
            if (op == TokenKind::Le) opc = AnvilOpcode::CmpLe;
            else if (op == TokenKind::Gt) opc = AnvilOpcode::CmpGt;
            else if (op == TokenKind::Ge) opc = AnvilOpcode::CmpGe;
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_equality() noexcept {
        u16 lhs = compile_relational();
        while (peek().kind == TokenKind::EqEq || peek().kind == TokenKind::BangEq ||
               peek().kind == TokenKind::LooseEq || peek().kind == TokenKind::LooseNe) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_relational();
            const u16 dst = alloc_reg();
            AnvilOpcode opc = AnvilOpcode::CmpEq;
            if (op == TokenKind::BangEq) {
                opc = AnvilOpcode::CmpNe;
            } else if (op == TokenKind::LooseEq) {
                opc = AnvilOpcode::CmpLooseEq;
            } else if (op == TokenKind::LooseNe) {
                opc = AnvilOpcode::CmpLooseNe;
            }
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_bitwise() noexcept {
        u16 lhs = compile_equality();
        while (peek().kind == TokenKind::Amp ||
               peek().kind == TokenKind::Caret ||
               peek().kind == TokenKind::Pipe) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_equality();
            const u16 dst = alloc_reg();
            const AnvilOpcode opc = (op == TokenKind::Amp)
                ? AnvilOpcode::BitAnd
                : (op == TokenKind::Caret ? AnvilOpcode::BitXor : AnvilOpcode::BitOr);
            ctx->program.emit(AnvilInstruction::make_abc(opc, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_logical() noexcept {
        u16 lhs = compile_bitwise();
        while (peek().kind == TokenKind::AndAnd || peek().kind == TokenKind::OrOr) {
            const bool is_and = advance().kind == TokenKind::AndAnd;
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, lhs, 0u));
            const u32 patch_pc = ctx->program.emit(AnvilInstruction::make_abx(
                is_and ? AnvilOpcode::JumpIfFalse : AnvilOpcode::JumpIfTrue,
                dst,
                0u
            ));
            const u16 rhs = compile_bitwise();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, rhs, 0u));
            ctx->program.code()[patch_pc] = AnvilInstruction::make_abx(
                is_and ? AnvilOpcode::JumpIfFalse : AnvilOpcode::JumpIfTrue,
                dst,
                ctx->program.code_len
            );
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_expr() noexcept {
        const u16 cond_or_val = compile_logical();
        if (match(TokenKind::Question)) {
            const u16 dst = alloc_reg();
            const u32 jfalse_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_or_val, 0u)
            );
            const u16 then_reg = compile_expr();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, then_reg, 0u));
            const u32 jend_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
            expect(TokenKind::Colon);
            ctx->program.code()[jfalse_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                cond_or_val,
                ctx->program.code_len
            );
            const u16 else_reg = compile_expr();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, else_reg, 0u));
            ctx->program.code()[jend_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                ctx->program.code_len
            );
            return dst;
        }
        return cond_or_val;
    }

    // No temporary outlives the statement that made it, so the registers
    // above the locals are free again once a statement is compiled. The
    // returned register is only read by the final Halt.
    u16 compile_statement() noexcept {
        const u16 result = compile_statement_body();
        if (next_reg > reg_floor) {
            next_reg = reg_floor;
        }
        return result;
    }

    u16 compile_statement_body() noexcept {
        if (refuse_if_too_deep()) {
            return 0u;
        }
        while (match(TokenKind::Semicolon)) {}
        const Token& t = peek();
        if (t.kind == TokenKind::Eof || t.kind == TokenKind::RBrace) {
            return 0u;
        }

        if (match(TokenKind::LBrace)) {
            const BlockScope scope(block_depth);
            u16 last_reg = 0u;
            while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
                last_reg = compile_statement();
            }
            expect(TokenKind::RBrace);
            return last_reg;
        }

        if (match(TokenKind::KwLet) || match(TokenKind::KwConst) || match(TokenKind::KwVar)) {
            const bool is_var = lexer.tokens()[cursor - 1u].kind == TokenKind::KwVar;
            const Token id_tok = advance();
            if (id_tok.kind != TokenKind::Ident) {
                reject_syntax();
                return 0u;
            }
            const u32 sym = token_sym(id_tok);
            if (find_function(sym) != NO_SLOT) {
                reject_name(VM_REFUSE_UNSUPPORTED, "name used for a variable and a function", id_tok);
                return 0u;
            }
            if (!in_function) {
                // A top-level name lives only in its global slot, so a
                // function that reads or writes it sees the same value as
                // the top-level code does.
                u16 val_reg = 0u;
                if (match(TokenKind::Assign)) {
                    val_reg = compile_expr();
                } else {
                    val_reg = alloc_reg();
                    emit_load_undefined(val_reg);
                }
                reject_if_expression_continues();
                const u16 g_slot = global_slot(sym);
                if (!had_error && g_slot < ctx->global_count) {
                    AnvilGlobalBinding& gb = ctx->globals()[g_slot];
                    if (gb.decl_depth != NO_DEPTH && gb.decl_depth != block_depth) {
                        reject_name(VM_REFUSE_UNSUPPORTED, "name declared again in an inner block", id_tok);
                        return 0u;
                    }
                    gb.decl_depth = block_depth;
                    gb.declared = true;
                    // Later scripts see top-level names and `var`s. A
                    // `let` or `const` in a block is gone after it.
                    if (block_depth == 0u || is_var) {
                        gb.exported = true;
                    }
                }
                ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, val_reg, g_slot));
                match(TokenKind::Semicolon);
                return val_reg;
            }
            const u16 dst = declare_local(sym);
            if (match(TokenKind::Assign)) {
                const u16 val_reg = compile_expr();
                reject_if_expression_continues();
                if (val_reg != dst) {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, val_reg, 0u));
                }
            } else {
                emit_load_undefined(dst);
            }
            match(TokenKind::Semicolon);
            return dst;
        }

        if (match(TokenKind::KwReturn)) {
            if (!in_function) {
                reject_here(VM_REFUSE_SYNTAX, "return outside a function");
                return 0u;
            }
            u16 ret_reg = 0u;
            if (peek().kind != TokenKind::Semicolon &&
                peek().kind != TokenKind::RBrace &&
                peek().kind != TokenKind::Eof) {
                ret_reg = compile_expr();
                reject_if_expression_continues();
            } else {
                ret_reg = alloc_reg();
                emit_load_undefined(ret_reg);
            }
            match(TokenKind::Semicolon);
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, ret_reg, 0u, 0u));
            return ret_reg;
        }

        if (match(TokenKind::KwIf)) {
            expect(TokenKind::LParen);
            // The condition is a temporary consumed by the branch below, so free
            // it before compiling the body. Nested ifs then reuse one slot
            // instead of holding one register per level of nesting.
            const u16 cond_mark = next_reg;
            const u16 cond_reg = compile_expr();
            expect(TokenKind::RParen);
            const u32 jfalse_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, 0u)
            );
            if (next_reg > cond_mark) {
                next_reg = cond_mark;
            }
            u16 last_reg = compile_statement();
            if (match(TokenKind::KwElse)) {
                const u32 jend_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
                ctx->program.code()[jfalse_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse,
                    cond_reg,
                    ctx->program.code_len
                );
                last_reg = compile_statement();
                ctx->program.code()[jend_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::Jump,
                    0u,
                    ctx->program.code_len
                );
            } else {
                ctx->program.code()[jfalse_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse,
                    cond_reg,
                    ctx->program.code_len
                );
            }
            return last_reg;
        }

        if (match(TokenKind::KwBreak)) {
            compile_loop_jump(true);
            return 0u;
        }

        if (match(TokenKind::KwContinue)) {
            compile_loop_jump(false);
            return 0u;
        }

        if (match(TokenKind::KwDo)) {
            const u32 loop_id = (next_loop_id < 63u) ? next_loop_id++ : 63u;
            const u32 loop_head_pc = ctx->program.code_len;
            ctx->program.emit(AnvilInstruction::make_abc(
                AnvilOpcode::LoopHeader,
                static_cast<u8>(loop_id),
                0u,
                0u
            ));
            if (!begin_loop()) {
                return 0u;
            }
            const u16 body_reg = compile_statement();
            expect(TokenKind::KwWhile);
            expect(TokenKind::LParen);
            const u32 cond_pc = ctx->program.code_len;
            const u16 cond_reg = compile_expr();
            expect(TokenKind::RParen);
            match(TokenKind::Semicolon);
            ctx->program.emit(AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfTrue,
                cond_reg,
                loop_head_pc
            ));
            end_loop(cond_pc);
            return body_reg;
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
            const u16 cond_reg = compile_expr();
            expect(TokenKind::RParen);
            const u32 jexit_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, 0u)
            );
            if (!begin_loop()) {
                return 0u;
            }
            const u16 body_reg = compile_statement();
            ctx->program.emit(AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                loop_head_pc
            ));
            ctx->program.code()[jexit_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                cond_reg,
                ctx->program.code_len
            );
            end_loop(loop_head_pc);
            return body_reg;
        }

        if (match(TokenKind::KwFor)) {
            // A `let` in the header belongs to the loop.
            const BlockScope scope(block_depth);
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
            u16 cond_reg = 0u;
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

            ctx->program.code()[jbody_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                ctx->program.code_len
            );
            if (!begin_loop()) {
                return 0u;
            }
            const u16 body_reg = compile_statement();
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, static_cast<u16>(step_pc)));

            if (has_cond) {
                ctx->program.code()[jexit_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse,
                    cond_reg,
                    ctx->program.code_len
                );
            }
            end_loop(step_pc);
            return body_reg;
        }

        if (match(TokenKind::KwFunction)) {
            const Token fn_tok = advance();
            if (fn_tok.kind != TokenKind::Ident) {
                reject_syntax();
                return 0u;
            }
            const u32 fn_sym = token_sym(fn_tok);
            expect(TokenKind::LParen);

            // Save enclosing compiler state. The enclosing locals stay in the
            // arena; the function's own locals are appended above local_floor
            // and dropped when its local_count is restored, so an inner
            // function never sees or disturbs them.
            const u32 saved_local_count = local_count;
            const u32 saved_local_floor = local_floor;
            const u16 saved_next_reg = next_reg;
            const u16 saved_reg_floor = reg_floor;
            const u16 saved_high_reg = high_reg;
            const bool saved_in_fn = in_function;

            local_floor = local_count;
            next_reg = 0u;
            reg_floor = 0u;
            high_reg = 0u;
            in_function = true;

            u32 arity = 0u;
            while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                const Token p_tok = advance();
                if (p_tok.kind != TokenKind::Ident) {
                    reject_syntax();
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
            // hoist_functions made the function already; its code starts here.
            const u32 fn_id = find_function(fn_sym);
            if (fn_id == NO_SLOT) {
                reject_name(VM_REFUSE_SYNTAX, "function not found by the first pass", fn_tok);
            } else {
                ctx->heap.functions()[fn_id].code_start = fn_start_pc;
            }

            compile_statement();
            const u16 fallthrough_reg = alloc_reg();
            emit_load_undefined(fallthrough_reg);
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, fallthrough_reg, 0u, 0u));

            if (fn_id < ctx->heap.function_count) {
                ctx->heap.functions()[fn_id].local_count = high_reg;
                ctx->heap.functions()[fn_id].code_len = ctx->program.code_len - fn_start_pc;
            }
            ctx->program.code()[jskip_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                ctx->program.code_len
            );

            // Restore enclosing compiler state; the function's locals above
            // local_floor fall away as local_count returns to its old value.
            local_count = saved_local_count;
            local_floor = saved_local_floor;
            next_reg = saved_next_reg;
            reg_floor = saved_reg_floor;
            high_reg = saved_high_reg;
            in_function = saved_in_fn;
            return 0u;
        }

        const u16 res = compile_statement_or_assign_no_semi();
        reject_if_expression_continues();
        match(TokenKind::Semicolon);
        return res;
    }

    u16 compile_statement_or_assign_no_semi() noexcept {
        if (looks_like_host_text()) {
            return compile_host_text();
        }
        if (peek().kind == TokenKind::Ident) {
            const TokenKind second = peek(1u).kind;
            const TokenKind third = peek(2u).kind;
            // `x++;` or a for-loop step `x++)` alone: nothing reads the
            // value, so the old value is not kept as a number.
            if ((second == TokenKind::PlusPlus || second == TokenKind::MinusMinus) &&
                (third == TokenKind::Semicolon || third == TokenKind::RParen ||
                 third == TokenKind::RBrace || third == TokenKind::Eof)) {
                const Token id_tok = advance();
                const bool is_inc = advance().kind == TokenKind::PlusPlus;
                const u32 sym = token_sym(id_tok);
                const i32 local_r = find_local(sym);
                u16 cur_reg = 0u;
                if (local_r >= 0) {
                    cur_reg = static_cast<u16>(local_r);
                } else {
                    cur_reg = alloc_reg();
                    const u16 g_slot = global_slot(sym);
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::GetGlobal, cur_reg, g_slot));
                }
                const u16 one_reg = alloc_reg();
                emit_load_number(one_reg, 1.0);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub,
                    dst,
                    cur_reg,
                    one_reg
                ));
                store_back_ident(sym, local_r, dst);
                // At the top level the old value is still in its temporary,
                // and the VM reads a script's value as a number.
                return local_r >= 0 ? dst : cur_reg;
            }
            if (second == TokenKind::Assign ||
                second == TokenKind::PlusEq ||
                second == TokenKind::MinusEq ||
                second == TokenKind::StarEq) {
                const Token id_tok = advance();
                const TokenKind assign_kind = advance().kind;
                const u32 sym = token_sym(id_tok);
                const i32 local_r = find_local(sym);
                const u16 rhs = compile_expr();

                if (assign_kind == TokenKind::Assign) {
                    store_back_ident(sym, local_r, rhs);
                    return rhs;
                }

                u16 cur_reg = 0u;
                if (local_r >= 0) {
                    cur_reg = static_cast<u16>(local_r);
                } else {
                    cur_reg = alloc_reg();
                    const u16 g_slot = global_slot(sym);
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::GetGlobal, cur_reg, g_slot));
                }
                const u16 dst = alloc_reg();
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
    return static_cast<u32>(sizeof(AnvilCompiler)) + ANVIL_COMPILER_ARENA_RESERVE_BYTES;
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
    // The arena is the reserved bytes right after the compiler struct.
    g_compiler.arena.init(
        static_cast<u8*>(compiler_memory) + sizeof(AnvilCompiler),
        ANVIL_COMPILER_ARENA_RESERVE_BYTES,
        ANVIL_COMPILER_ARENA_RESERVE_BYTES
    );
    g_compiler.lexer.arena = &g_compiler.arena;
    g_compiler.ctx = &ctx;
    g_compiler.src = src;
    g_compiler.cursor = 0u;
    g_compiler.locals_off = 0u;
    g_compiler.locals_cap = 0u;
    g_compiler.local_count = 0u;
    g_compiler.local_floor = 0u;
    g_compiler.next_reg = 0u;
    g_compiler.reg_floor = 0u;
    g_compiler.high_reg = 0u;
    g_compiler.next_loop_id = 0u;
    g_compiler.loop_depth = 0u;
    g_compiler.in_function = false;
    g_compiler.had_error = false;
    g_compiler.block_depth = 0u;
    // Anchor the native-stack guard at the compile entry: the recursive
    // descent grows the stack down from here, and a parse that grows past
    // the budget is refused instead of overflowing the stack.
    char stack_anchor;
    g_compiler.stack_base = reinterpret_cast<u64>(&stack_anchor);
    g_compiler.stack_budget = ctx.effective_parse_stack_budget();

    if (!g_compiler.lexer.tokenize(src, len)) {
        const AnvilLexer& lex = g_compiler.lexer;
        if (lex.fail_code == VM_REFUSE_TOKENS) {
            ctx.fail_limit(
                VM_REFUSE_TOKENS, "source", ANVIL_COMPILER_ARENA_RESERVE_BYTES, "bytes"
            );
        } else {
            ctx.fail_name(lex.fail_code, lex.fail_text, &src[lex.fail_offset], lex.fail_len);
        }
        return false;
    }
    g_compiler.hoist_functions();

    u16 last_reg = 0u;
    while (g_compiler.peek().kind != TokenKind::Eof && !g_compiler.had_error) {
        if (g_compiler.peek().kind == TokenKind::RBrace) {
            // A '}' with no open block. compile_statement returns without
            // consuming it, so this loop would never end. Reject the script;
            // the Rust interpreter runs it instead.
            g_compiler.reject_here(VM_REFUSE_SYNTAX, "unexpected");
            break;
        }
        last_reg = g_compiler.compile_statement();
    }
    // The top-level code's register high-water is the first frame's size.
    ctx.program.top_frame_regs = g_compiler.high_reg;
    ctx.program.emit(AnvilInstruction::make_abc(AnvilOpcode::Halt, last_reg, 0u, 0u));
    if (!ctx.program.compile_ok) {
        if (ctx.program.overflow == VM_REFUSE_CONSTANTS) {
            ctx.fail_limit(VM_REFUSE_HEAP, "heap", ctx.heap.arena.budget, "bytes");
        } else {
            ctx.fail_limit(VM_REFUSE_CODE_SIZE, "code size", ANVIL_MAX_PROGRAM_INSTRUCTIONS, "instructions");
        }
    }
    // A global no top-level declaration gave a value belongs to another
    // script or to the page (console, document, a library). Leave the whole
    // script to the Rust interpreter, which can see those names.
    for (u32 i = 0u; i < ctx.global_count; ++i) {
        if (!ctx.globals()[i].declared) {
            g_compiler.had_error = true;
            u32 name_len = 0u;
            const char* name = ctx.heap.get_string_ptr(ctx.globals()[i].name_sym, &name_len);
            ctx.fail_name(VM_REFUSE_UNKNOWN_NAME, "name not declared in this script", name, name_len);
        }
    }
    if (g_compiler.had_error && ctx.error.code == VM_OK) {
        ctx.fail(VM_REFUSE_SYNTAX, "syntax the VM does not compile");
    }
    return !g_compiler.had_error && ctx.program.compile_ok;
}

} // namespace

bool compile_js_to_bytecode(AnvilVmContext& ctx, void* compiler_memory, const char* src, u32 len) noexcept {
    return compile_owned(ctx, compiler_memory, src, len);
}

} // namespace anvil_vm
