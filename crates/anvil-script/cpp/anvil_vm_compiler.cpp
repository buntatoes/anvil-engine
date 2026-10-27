// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
// C++20 Lexer, Recursive-Descent / Pratt Expression Parser, and Register Bytecode Compiler.
// Author: Buntato

#include "anvil_vm.hpp"

namespace anvil_vm {

namespace {

enum class TokenKind : u8 {
    Eof            = 0u,
    Number         = 1u,
    String         = 2u,
    Ident          = 3u,
    KwLet          = 4u,
    KwConst        = 5u,
    KwVar          = 6u,
    KwIf           = 7u,
    KwElse         = 8u,
    KwWhile        = 9u,
    KwFor          = 10u,
    KwFunction     = 11u,
    KwReturn       = 12u,
    KwTrue         = 13u,
    KwFalse        = 14u,
    KwNull         = 15u,
    KwNew          = 16u,
    LParen         = 17u,
    RParen         = 18u,
    LBrace         = 19u,
    RBrace         = 20u,
    LBracket       = 21u,
    RBracket       = 22u,
    Comma          = 23u,
    Dot            = 24u,
    Colon          = 25u,
    Semicolon      = 26u,
    Question       = 27u,
    Plus           = 28u,
    Minus          = 29u,
    Star           = 30u,
    Slash          = 31u,
    Percent        = 32u,
    Bang           = 33u,
    Amp            = 34u,
    Pipe           = 35u,
    Caret          = 36u,
    Shl            = 37u,
    Shr            = 38u,
    UShr           = 39u,
    Assign         = 40u,
    PlusEq         = 41u,
    MinusEq        = 42u,
    StarEq         = 43u,
    PlusPlus       = 44u,
    MinusMinus     = 45u,
    EqEq           = 46u,
    BangEq         = 47u,
    Lt             = 48u,
    Le             = 49u,
    Gt             = 50u,
    Ge             = 51u,
    AndAnd         = 52u,
    OrOr           = 53u,
    KwDo           = 54u,
    KwBreak        = 55u,
    KwContinue     = 56u,
    LooseEq        = 57u,
    LooseNe        = 58u,
    FatArrow       = 59u,
    Tilde          = 60u,
    StarStar       = 61u,
    Nullish        = 62u,
    SlashEq        = 63u,
    PercentEq      = 64u,
    ShlEq          = 65u,
    ShrEq          = 66u,
    UShrEq         = 67u,
    AmpEq          = 68u,
    CaretEq        = 69u,
    PipeEq         = 70u,
    KwTypeof       = 71u,
    KwVoid         = 72u,
    KwDelete       = 73u,
    KwIn           = 74u,
    KwInstanceof   = 75u,
    KwSwitch       = 76u,
    KwCase         = 77u,
    KwDefault      = 78u,
    KwTry          = 79u,
    KwCatch        = 80u,
    KwFinally      = 81u,
    KwThrow        = 82u,
    KwClass        = 83u,
    KwExtends      = 84u,
    KwSuper        = 85u,
    KwThis         = 86u,
    KwExport       = 87u,
    TemplateFull   = 88u,
    TemplateHead   = 89u,
    TemplateMiddle = 90u,
    TemplateTail   = 91u,
};

struct Token {
    TokenKind kind;
    u32 offset;
    u32 length;
    u32 decoded_off;
    f64 num_val;
};

constexpr u32 ANVIL_COMPILER_ARENA_RESERVE_BYTES = 256u * 1024u * 1024u;
constexpr u32 INIT_TOKEN_BUFFER = 1024u;

inline bool is_space(char c) noexcept {
    return c == ' ' || c == '\t' || c == '\r' || c == '\n' || c == '\v' || c == '\f';
}

inline bool is_digit(char c) noexcept {
    return c >= '0' && c <= '9';
}

inline bool is_hex_digit(char c) noexcept {
    return is_digit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F');
}

inline u32 hex_val(char c) noexcept {
    if (c >= '0' && c <= '9') return static_cast<u32>(c - '0');
    if (c >= 'a' && c <= 'f') return static_cast<u32>(c - 'a' + 10);
    if (c >= 'A' && c <= 'F') return static_cast<u32>(c - 'A' + 10);
    return 0xFFFFFFFFu;
}

inline bool is_ident_start(char c) noexcept {
    return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_' || c == '$' ||
           static_cast<u8>(c) >= 0x80u;
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

inline bool is_event_loop_word(const char* s, u32 len) noexcept {
    return match_kw(s, len, "async", 5) || match_kw(s, len, "await", 5);
}

inline bool is_unsupported_word(const char* s, u32 len) noexcept {
    static const char* const words[] = {
        "with", "yield", "import", "debugger", "enum",
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

// Exact IEEE-754 number parser using 24-bit limbs (port of anvil-js numeric.js).
struct BigLimbs {
    u32 d[128];
    u32 len;

    void set_u32(u32 v) noexcept {
        if (v == 0u) {
            len = 0u;
        } else {
            d[0] = v & 0xFFFFFFu;
            const u32 hi = v >> 24u;
            if (hi != 0u) {
                d[1] = hi;
                len = 2u;
            } else {
                len = 1u;
            }
        }
    }

    void mul_add(u32 m, u32 a) noexcept {
        u64 carry = a;
        for (u32 i = 0u; i < len; ++i) {
            const u64 v = static_cast<u64>(d[i]) * m + carry;
            d[i] = static_cast<u32>(v) & 0xFFFFFFu;
            carry = v >> 24u;
        }
        while (carry > 0ULL && len < 128u) {
            d[len++] = static_cast<u32>(carry) & 0xFFFFFFu;
            carry >>= 24u;
        }
    }

    u32 bit_len() const noexcept {
        if (len == 0u) return 0u;
        u32 top = d[len - 1u];
        u32 bits = (len - 1u) * 24u;
        while (top > 0u) {
            ++bits;
            top >>= 1u;
        }
        return bits;
    }

    u32 get_bit(i32 idx) const noexcept {
        if (idx < 0) return 0u;
        const u32 limb = static_cast<u32>(idx) / 24u;
        if (limb >= len) return 0u;
        const u32 off = static_cast<u32>(idx) % 24u;
        return (d[limb] >> off) & 1u;
    }

    bool any_below(i32 top_excl) const noexcept {
        if (top_excl <= 0) return false;
        const u32 full = static_cast<u32>(top_excl) / 24u;
        const u32 rem = static_cast<u32>(top_excl) % 24u;
        for (u32 i = 0u; i < full && i < len; ++i) {
            if (d[i] != 0u) return true;
        }
        if (rem > 0u && full < len) {
            if ((d[full] & ((1u << rem) - 1u)) != 0u) return true;
        }
        return false;
    }

    void shl(u32 bits) noexcept {
        if (len == 0u || bits == 0u) return;
        const u32 limb_shift = bits / 24u;
        const u32 bit_shift = bits % 24u;
        if (limb_shift + len + 1u >= 128u) return;
        if (bit_shift == 0u) {
            for (i32 i = static_cast<i32>(len) - 1; i >= 0; --i) {
                d[static_cast<u32>(i) + limb_shift] = d[i];
            }
            for (u32 i = 0u; i < limb_shift; ++i) d[i] = 0u;
            len += limb_shift;
            return;
        }
        u32 carry = 0u;
        for (u32 i = 0u; i < len; ++i) {
            const u32 next_carry = d[i] >> (24u - bit_shift);
            d[i] = ((d[i] << bit_shift) | carry) & 0xFFFFFFu;
            carry = next_carry;
        }
        if (carry != 0u) {
            d[len++] = carry;
        }
        if (limb_shift > 0u) {
            for (i32 i = static_cast<i32>(len) - 1; i >= 0; --i) {
                d[static_cast<u32>(i) + limb_shift] = d[i];
            }
            for (u32 i = 0u; i < limb_shift; ++i) d[i] = 0u;
            len += limb_shift;
        }
    }
};

inline i32 big_cmp(const BigLimbs& a, const BigLimbs& b) noexcept {
    if (a.len != b.len) return a.len < b.len ? -1 : 1;
    for (i32 i = static_cast<i32>(a.len) - 1; i >= 0; --i) {
        if (a.d[i] != b.d[i]) return a.d[i] < b.d[i] ? -1 : 1;
    }
    return 0;
}

inline void big_sub_in_place(BigLimbs& a, const BigLimbs& b) noexcept {
    i64 borrow = 0;
    for (u32 i = 0u; i < a.len; ++i) {
        const i64 bv = (i < b.len) ? static_cast<i64>(b.d[i]) : 0;
        i64 diff = static_cast<i64>(a.d[i]) - bv - borrow;
        if (diff < 0) {
            diff += 0x1000000LL;
            borrow = 1;
        } else {
            borrow = 0;
        }
        a.d[i] = static_cast<u32>(diff);
    }
    while (a.len > 0u && a.d[a.len - 1u] == 0u) {
        --a.len;
    }
}

f64 pack_ieee754(i32 e2, u64 mant53) noexcept {
    const u64 biased = static_cast<u64>(e2 + 1023);
    const u64 frac = mant53 & 0x000FFFFFFFFFFFFFULL;
    return bits_to_f64((biased << 52u) | frac);
}

f64 parse_non_decimal(const char* s, u32 len, u32 radix) noexcept {
    BigLimbs b;
    b.set_u32(0u);
    for (u32 i = 0u; i < len; ++i) {
        if (s[i] == '_') continue;
        const u32 dv = hex_val(s[i]);
        if (dv < radix) {
            b.mul_add(radix, dv);
        }
    }
    const u32 nbits = b.bit_len();
    if (nbits == 0u) return 0.0;
    if (nbits <= 53u) {
        f64 v = 0.0;
        for (i32 i = static_cast<i32>(b.len) - 1; i >= 0; --i) {
            v = v * 16777216.0 + static_cast<f64>(b.d[i]);
        }
        return v;
    }
    if (nbits > 1024u) {
        return bits_to_f64(0x7FF0000000000000ULL);
    }
    const i32 drop = static_cast<i32>(nbits) - 53;
    u64 m = 0ULL;
    for (i32 i = static_cast<i32>(nbits) - 1; i >= drop; --i) {
        m = (m << 1u) | static_cast<u64>(b.get_bit(i));
    }
    const u32 half = b.get_bit(drop - 1);
    const bool sticky = b.any_below(drop - 1);
    if (half != 0u && (sticky || (m & 1ULL) != 0ULL)) {
        ++m;
        if (m == (1ULL << 53u)) {
            if (nbits == 1024u) {
                return bits_to_f64(0x7FF0000000000000ULL);
            }
            return pack_ieee754(static_cast<i32>(nbits), m >> 1u);
        }
    }
    return pack_ieee754(static_cast<i32>(nbits) - 1, m);
}

} // namespace

f64 parse_f64_slice(const char* s, u32 len) noexcept {
    if (len >= 2u && s[0] == '0') {
        if (s[1] == 'x' || s[1] == 'X') return parse_non_decimal(s + 2u, len - 2u, 16u);
        if (s[1] == 'o' || s[1] == 'O') return parse_non_decimal(s + 2u, len - 2u, 8u);
        if (s[1] == 'b' || s[1] == 'B') return parse_non_decimal(s + 2u, len - 2u, 2u);
    }

    u8 digits[768];
    u32 ndig = 0u;
    i32 exp10 = 0;
    bool seen_dot = false;
    u32 i = 0u;
    while (i < len && s[i] != 'e' && s[i] != 'E') {
        const char c = s[i++];
        if (c == '_') continue;
        if (c == '.') {
            seen_dot = true;
            continue;
        }
        if (is_digit(c)) {
            const u8 d = static_cast<u8>(c - '0');
            if (ndig == 0u && d == 0u) {
                if (seen_dot) --exp10;
            } else {
                if (ndig < 768u) {
                    digits[ndig++] = d;
                    if (seen_dot) --exp10;
                } else if (!seen_dot) {
                    ++exp10;
                }
            }
        }
    }
    if (i < len && (s[i] == 'e' || s[i] == 'E')) {
        ++i;
        i32 sign = 1;
        if (i < len && (s[i] == '+' || s[i] == '-')) {
            if (s[i] == '-') sign = -1;
            ++i;
        }
        i32 e = 0;
        while (i < len) {
            if (s[i] != '_' && is_digit(s[i])) {
                if (e < 10000) e = e * 10 + static_cast<i32>(s[i] - '0');
            }
            ++i;
        }
        exp10 += sign * e;
    }

    while (ndig > 0u && digits[ndig - 1u] == 0u) {
        --ndig;
        ++exp10;
    }
    if (ndig == 0u) return 0.0;

    const i32 sci_exp = exp10 + static_cast<i32>(ndig) - 1;
    if (sci_exp > 308) return bits_to_f64(0x7FF0000000000000ULL);
    if (sci_exp < -324) return 0.0;

    // Fast path for exact f64 integers and powers of 10 <= 10^22.
    if (ndig <= 15u && exp10 >= -22 && exp10 <= 22) {
        static const f64 pow10_tbl[23] = {
            1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10,
            1e11, 1e12, 1e13, 1e14, 1e15, 1e16, 1e17, 1e18, 1e19, 1e20, 1e21, 1e22
        };
        u64 int_v = 0ULL;
        for (u32 k = 0u; k < ndig; ++k) {
            int_v = int_v * 10ULL + digits[k];
        }
        const f64 fv = static_cast<f64>(int_v);
        if (exp10 >= 0) {
            return fv * pow10_tbl[exp10];
        }
        return fv / pow10_tbl[-exp10];
    }

    BigLimbs num;
    num.set_u32(0u);
    for (u32 k = 0u; k < ndig; ++k) {
        num.mul_add(10u, digits[k]);
    }
    BigLimbs den;
    den.set_u32(1u);
    if (exp10 >= 0) {
        u32 rem_e = static_cast<u32>(exp10);
        while (rem_e >= 9u) { num.mul_add(1000000000u, 0u); rem_e -= 9u; }
        while (rem_e > 0u) { num.mul_add(10u, 0u); --rem_e; }
    } else {
        u32 rem_e = static_cast<u32>(-exp10);
        while (rem_e >= 9u) { den.mul_add(1000000000u, 0u); rem_e -= 9u; }
        while (rem_e > 0u) { den.mul_add(10u, 0u); --rem_e; }
    }

    i32 e2 = static_cast<i32>(num.bit_len()) - static_cast<i32>(den.bit_len());
    auto cmp_shifted = [&](i32 shift) noexcept -> i32 {
        if (shift >= 0) {
            BigLimbs d_sh = den;
            d_sh.shl(static_cast<u32>(shift));
            return big_cmp(num, d_sh);
        } else {
            BigLimbs n_sh = num;
            n_sh.shl(static_cast<u32>(-shift));
            return big_cmp(n_sh, den);
        }
    };
    if (cmp_shifted(e2) < 0) {
        --e2;
    }
    if (e2 > 1023) return bits_to_f64(0x7FF0000000000000ULL);
    if (e2 < -1075) return 0.0;

    const i32 p = (e2 >= -1022) ? 53 : (e2 + 1075);
    const i32 shift = (p - 1) - e2;
    if (shift > 0) {
        num.shl(static_cast<u32>(shift));
    } else if (shift < 0) {
        den.shl(static_cast<u32>(-shift));
    }

    // Extract p bits of floor(num / den) + remainder.
    u64 q = 0ULL;
    for (i32 b = p - 1; b >= 0; --b) {
        BigLimbs d_sh = den;
        d_sh.shl(static_cast<u32>(b));
        if (big_cmp(num, d_sh) >= 0) {
            big_sub_in_place(num, d_sh);
            q |= (1ULL << static_cast<u32>(b));
        }
    }
    // Check 2 * rem vs den for ties-to-even.
    num.shl(1u);
    const i32 rcmp = big_cmp(num, den);
    if (rcmp > 0 || (rcmp == 0 && (q & 1ULL) != 0ULL)) {
        ++q;
    }
    if (p < 53) {
        if (q == (1ULL << 52u)) {
            return pack_ieee754(-1022, q << 1u);
        }
        return bits_to_f64(q);
    }
    if (q == (1ULL << 53u)) {
        q >>= 1u;
        ++e2;
        if (e2 > 1023) return bits_to_f64(0x7FF0000000000000ULL);
    }
    return pack_ieee754(e2, q);
}

namespace {

u32 encode_utf8_cp(u32 cp, char* out) noexcept {
    if (cp <= 0x7Fu) {
        out[0] = static_cast<char>(cp);
        return 1u;
    }
    if (cp <= 0x7FFu) {
        out[0] = static_cast<char>(0xC0u | (cp >> 6u));
        out[1] = static_cast<char>(0x80u | (cp & 0x3Fu));
        return 2u;
    }
    if (cp <= 0xFFFFu) {
        out[0] = static_cast<char>(0xE0u | (cp >> 12u));
        out[1] = static_cast<char>(0x80u | ((cp >> 6u) & 0x3Fu));
        out[2] = static_cast<char>(0x80u | (cp & 0x3Fu));
        return 3u;
    }
    out[0] = static_cast<char>(0xF0u | (cp >> 18u));
    out[1] = static_cast<char>(0x80u | ((cp >> 12u) & 0x3Fu));
    out[2] = static_cast<char>(0x80u | ((cp >> 6u) & 0x3Fu));
    out[3] = static_cast<char>(0x80u | (cp & 0x3Fu));
    return 4u;
}

struct AnvilLexer {
    AnvilArena* arena;
    u32 tokens_off;
    u32 tokens_cap;
    u32 count;
    u32 fail_code;
    const char* fail_text;
    u32 fail_offset;
    u32 fail_len;

    Token* tokens() noexcept {
        return reinterpret_cast<Token*>(arena->at(tokens_off));
    }
    const Token* tokens() const noexcept {
        return reinterpret_cast<const Token*>(arena->at(tokens_off));
    }

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

    bool fail(u32 code, const char* text, u32 offset, u32 length) noexcept {
        fail_code = code;
        fail_text = text;
        fail_offset = offset;
        fail_len = length;
        return false;
    }

    bool decode_escape(const char* src, u32 len, u32& pos, char* dst, u32& out_len, u32 tok_start) noexcept {
        if (pos >= len) {
            return fail(VM_REFUSE_SYNTAX, "unterminated string", tok_start, pos - tok_start);
        }
        const char esc = src[pos++];
        switch (esc) {
            case 'n': dst[out_len++] = '\n'; return true;
            case 'r': dst[out_len++] = '\r'; return true;
            case 't': dst[out_len++] = '\t'; return true;
            case 'b': dst[out_len++] = '\b'; return true;
            case 'f': dst[out_len++] = '\f'; return true;
            case 'v': dst[out_len++] = '\v'; return true;
            case '\\': dst[out_len++] = '\\'; return true;
            case '\'': dst[out_len++] = '\''; return true;
            case '"': dst[out_len++] = '"'; return true;
            case '`': dst[out_len++] = '`'; return true;
            case '$': dst[out_len++] = '$'; return true;
            case '\n': return true;
            case '\r':
                if (pos < len && src[pos] == '\n') ++pos;
                return true;
            case '0':
                if (pos < len && is_digit(src[pos])) {
                    return fail(VM_REFUSE_SYNTAX, "unsupported octal escape", tok_start, pos - tok_start);
                }
                dst[out_len++] = '\0';
                return true;
            case 'x': {
                if (pos + 2u > len || !is_hex_digit(src[pos]) || !is_hex_digit(src[pos + 1u])) {
                    return fail(VM_REFUSE_SYNTAX, "bad hex escape", tok_start, pos - tok_start);
                }
                const u32 cp = (hex_val(src[pos]) << 4u) | hex_val(src[pos + 1u]);
                pos += 2u;
                out_len += encode_utf8_cp(cp, dst + out_len);
                return true;
            }
            case 'u': {
                u32 cp = 0u;
                if (pos < len && src[pos] == '{') {
                    ++pos;
                    u32 nd = 0u;
                    while (pos < len && src[pos] != '}') {
                        if (!is_hex_digit(src[pos])) {
                            return fail(VM_REFUSE_SYNTAX, "bad unicode escape", tok_start, pos - tok_start);
                        }
                        cp = (cp << 4u) | hex_val(src[pos++]);
                        ++nd;
                        if (cp > 0x10FFFFu) {
                            return fail(VM_REFUSE_SYNTAX, "bad unicode escape", tok_start, pos - tok_start);
                        }
                    }
                    if (nd == 0u || pos >= len || src[pos] != '}') {
                        return fail(VM_REFUSE_SYNTAX, "bad unicode escape", tok_start, pos - tok_start);
                    }
                    ++pos;
                } else {
                    if (pos + 4u > len) {
                        return fail(VM_REFUSE_SYNTAX, "bad unicode escape", tok_start, pos - tok_start);
                    }
                    for (u32 k = 0u; k < 4u; ++k) {
                        if (!is_hex_digit(src[pos + k])) {
                            return fail(VM_REFUSE_SYNTAX, "bad unicode escape", tok_start, pos - tok_start);
                        }
                        cp = (cp << 4u) | hex_val(src[pos + k]);
                    }
                    pos += 4u;
                    // Combine surrogate pair \uD800..\uDBFF \uDC00..\uDFFF into 4-byte UTF-8.
                    if (cp >= 0xD800u && cp <= 0xDBFFu && pos + 6u <= len &&
                        src[pos] == '\\' && src[pos + 1u] == 'u' &&
                        is_hex_digit(src[pos + 2u]) && is_hex_digit(src[pos + 3u]) &&
                        is_hex_digit(src[pos + 4u]) && is_hex_digit(src[pos + 5u])) {
                        const u32 lo = (hex_val(src[pos + 2u]) << 12u) |
                                       (hex_val(src[pos + 3u]) << 8u) |
                                       (hex_val(src[pos + 4u]) << 4u) |
                                       hex_val(src[pos + 5u]);
                        if (lo >= 0xDC00u && lo <= 0xDFFFu) {
                            cp = 0x10000u + ((cp - 0xD800u) << 10u) + (lo - 0xDC00u);
                            pos += 6u;
                        }
                    }
                }
                out_len += encode_utf8_cp(cp, dst + out_len);
                return true;
            }
            default:
                if (esc >= '1' && esc <= '9') {
                    return fail(VM_REFUSE_SYNTAX, "unsupported octal escape", tok_start, pos - tok_start);
                }
                dst[out_len++] = esc;
                return true;
        }
    }

    bool scan_template_segment(
        const char* src,
        u32 len,
        u32& pos,
        bool is_head,
        u32* tpl_brace_depth,
        u32& tpl_stack_len,
        u32 brace_depth
    ) noexcept {
        const u32 start = pos;
        bool needs_decode = false;
        u32 scan = pos;
        while (scan < len) {
            const char ch = src[scan];
            if (ch == '`') {
                break;
            }
            if (ch == '$' && scan + 1u < len && src[scan + 1u] == '{') {
                break;
            }
            if (ch == '\\' || ch == '\r') {
                needs_decode = true;
                if (ch == '\\' && scan + 1u < len) {
                    ++scan;
                }
            }
            ++scan;
        }
        if (scan >= len) {
            return fail(VM_REFUSE_SYNTAX, "unterminated template", start > 0u ? start - 1u : 0u, len - start);
        }
        u32 decoded_off = NO_SLOT;
        u32 out_len = scan - start;
        if (needs_decode) {
            const u32 alloc_cap = (scan - start) * 3u + 8u;
            bool over = false;
            decoded_off = arena->alloc(alloc_cap, &over);
            if (decoded_off == NO_SLOT) {
                return fail(VM_REFUSE_TOKENS, "source", pos, 0u);
            }
            char* dst = reinterpret_cast<char*>(arena->at(decoded_off));
            out_len = 0u;
            while (pos < scan) {
                const char ch = src[pos++];
                if (ch == '\\') {
                    if (!decode_escape(src, len, pos, dst, out_len, start)) {
                        return false;
                    }
                } else if (ch == '\r') {
                    if (pos < scan && src[pos] == '\n') ++pos;
                    dst[out_len++] = '\n';
                } else {
                    dst[out_len++] = ch;
                }
            }
        } else {
            pos = scan;
        }

        if (src[pos] == '`') {
            ++pos;
            const TokenKind kind = is_head ? TokenKind::TemplateFull : TokenKind::TemplateTail;
            tokens()[count++] = Token{kind, start, out_len, decoded_off, 0.0};
            return true;
        }
        // Ended at `${`
        pos += 2u;
        if (is_head) {
            if (tpl_stack_len >= 16u) {
                return fail(VM_REFUSE_NESTING, "template nesting", start, 2u);
            }
            tpl_brace_depth[tpl_stack_len++] = brace_depth;
        }
        const TokenKind kind = is_head ? TokenKind::TemplateHead : TokenKind::TemplateMiddle;
        tokens()[count++] = Token{kind, start, out_len, decoded_off, 0.0};
        return true;
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
        u32 brace_depth = 0u;
        u32 tpl_brace_depth[16];
        u32 tpl_stack_len = 0u;

        if (len >= 2u && src[0] == '#' && src[1] == '!') {
            pos = 2u;
            while (pos < len && src[pos] != '\n' && src[pos] != '\r') {
                ++pos;
            }
        }

        while (pos < len) {
            if (!ensure_tokens(count + 2u)) {
                return fail(VM_REFUSE_TOKENS, "source", pos, 0u);
            }
            const char c = src[pos];
            if (is_space(c)) {
                ++pos;
                continue;
            }
            if (c == '/' && pos + 1u < len && src[pos + 1u] == '/') {
                pos += 2u;
                while (pos < len && src[pos] != '\n' && src[pos] != '\r') {
                    ++pos;
                }
                continue;
            }
            if (c == '/' && pos + 1u < len && src[pos + 1u] == '*') {
                const u32 cstart = pos;
                pos += 2u;
                while (pos + 1u < len && !(src[pos] == '*' && src[pos + 1u] == '/')) {
                    ++pos;
                }
                if (pos + 1u >= len) {
                    return fail(VM_REFUSE_SYNTAX, "unterminated comment", cstart, len - cstart);
                }
                pos += 2u;
                continue;
            }

            if (is_digit(c) || (c == '.' && pos + 1u < len && is_digit(src[pos + 1u]))) {
                const u32 start = pos;
                if (c == '0' && pos + 1u < len &&
                    (src[pos + 1u] == 'x' || src[pos + 1u] == 'X' ||
                     src[pos + 1u] == 'o' || src[pos + 1u] == 'O' ||
                     src[pos + 1u] == 'b' || src[pos + 1u] == 'B')) {
                    const char pfx = src[pos + 1u];
                    const u32 radix = (pfx == 'x' || pfx == 'X') ? 16u : ((pfx == 'o' || pfx == 'O') ? 8u : 2u);
                    pos += 2u;
                    u32 ndig = 0u;
                    bool last_sep = true;
                    while (pos < len) {
                        const char d = src[pos];
                        if (d == '_') {
                            if (last_sep) {
                                ++pos;
                                return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                            }
                            last_sep = true;
                            ++pos;
                            continue;
                        }
                        const u32 hv = hex_val(d);
                        if (hv >= radix) break;
                        last_sep = false;
                        ++ndig;
                        ++pos;
                    }
                    if (ndig == 0u || last_sep || (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.'))) {
                        while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                        return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                    }
                    tokens()[count++] = Token{
                        TokenKind::Number,
                        start,
                        pos - start,
                        NO_SLOT,
                        parse_f64_slice(&src[start], pos - start),
                    };
                    continue;
                }

                // Legacy octal or leading zero `00`, `07`, `0_1`.
                if (c == '0' && pos + 1u < len && (is_digit(src[pos + 1u]) || src[pos + 1u] == '_')) {
                    while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                }

                bool last_sep = true;
                while (pos < len && (is_digit(src[pos]) || src[pos] == '_')) {
                    if (src[pos] == '_') {
                        if (last_sep) {
                            while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                            return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                        }
                        last_sep = true;
                    } else {
                        last_sep = false;
                    }
                    ++pos;
                }
                if (pos > start && last_sep) {
                    while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                }
                if (pos < len && src[pos] == '.') {
                    ++pos;
                    if (pos < len && src[pos] == '_') {
                        while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                        return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                    }
                    last_sep = true;
                    u32 fdig = 0u;
                    while (pos < len && (is_digit(src[pos]) || src[pos] == '_')) {
                        if (src[pos] == '_') {
                            if (last_sep) {
                                while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                                return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                            }
                            last_sep = true;
                        } else {
                            last_sep = false;
                            ++fdig;
                        }
                        ++pos;
                    }
                    if (fdig > 0u && last_sep) {
                        while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                        return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                    }
                }
                if (pos < len && (src[pos] == 'e' || src[pos] == 'E')) {
                    ++pos;
                    if (pos < len && (src[pos] == '+' || src[pos] == '-')) {
                        ++pos;
                    }
                    u32 edig = 0u;
                    last_sep = true;
                    while (pos < len && (is_digit(src[pos]) || src[pos] == '_')) {
                        if (src[pos] == '_') {
                            if (last_sep) {
                                while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                                return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                            }
                            last_sep = true;
                        } else {
                            last_sep = false;
                            ++edig;
                        }
                        ++pos;
                    }
                    if (edig == 0u || last_sep) {
                        while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) ++pos;
                        return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                    }
                }
                if (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) {
                    while (pos < len && (is_ident_cont(src[pos]) || src[pos] == '.')) {
                        ++pos;
                    }
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported number", start, pos - start);
                }
                tokens()[count++] = Token{
                    TokenKind::Number,
                    start,
                    pos - start,
                    NO_SLOT,
                    parse_f64_slice(&src[start], pos - start),
                };
                continue;
            }

            if (c == '\'' || c == '"') {
                const char quote = c;
                ++pos;
                const u32 start = pos;
                bool has_escape = false;
                u32 scan = pos;
                while (scan < len && src[scan] != quote) {
                    if (src[scan] == '\n' || src[scan] == '\r') {
                        return fail(VM_REFUSE_SYNTAX, "unterminated string", start - 1u, scan - start + 1u);
                    }
                    if (src[scan] == '\\') {
                        has_escape = true;
                        ++scan;
                        if (scan < len && src[scan] == '\r' && scan + 1u < len && src[scan + 1u] == '\n') {
                            ++scan;
                        }
                    }
                    if (scan < len) ++scan;
                }
                if (scan >= len) {
                    return fail(VM_REFUSE_SYNTAX, "unterminated string", start - 1u, scan - start + 1u);
                }
                u32 decoded_off = NO_SLOT;
                u32 str_len = scan - start;
                if (has_escape) {
                    const u32 alloc_cap = (scan - start) * 3u + 8u;
                    bool over = false;
                    decoded_off = arena->alloc(alloc_cap, &over);
                    if (decoded_off == NO_SLOT) {
                        return fail(VM_REFUSE_TOKENS, "source", pos, 0u);
                    }
                    char* dst = reinterpret_cast<char*>(arena->at(decoded_off));
                    str_len = 0u;
                    while (pos < scan) {
                        const char ch = src[pos++];
                        if (ch == '\\') {
                            if (!decode_escape(src, len, pos, dst, str_len, start - 1u)) {
                                return false;
                            }
                        } else {
                            dst[str_len++] = ch;
                        }
                    }
                } else {
                    pos = scan;
                }
                ++pos; // closing quote
                tokens()[count++] = Token{TokenKind::String, start, str_len, decoded_off, 0.0};
                continue;
            }

            if (c == '`') {
                ++pos;
                if (!scan_template_segment(src, len, pos, true, tpl_brace_depth, tpl_stack_len, brace_depth)) {
                    return false;
                }
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
                else if (match_kw(s, id_len, "typeof", 6)) kind = TokenKind::KwTypeof;
                else if (match_kw(s, id_len, "void", 4)) kind = TokenKind::KwVoid;
                else if (match_kw(s, id_len, "delete", 6)) kind = TokenKind::KwDelete;
                else if (match_kw(s, id_len, "in", 2)) kind = TokenKind::KwIn;
                else if (match_kw(s, id_len, "instanceof", 10)) kind = TokenKind::KwInstanceof;
                else if (match_kw(s, id_len, "switch", 6)) kind = TokenKind::KwSwitch;
                else if (match_kw(s, id_len, "case", 4)) kind = TokenKind::KwCase;
                else if (match_kw(s, id_len, "default", 7)) kind = TokenKind::KwDefault;
                else if (match_kw(s, id_len, "try", 3)) kind = TokenKind::KwTry;
                else if (match_kw(s, id_len, "catch", 5)) kind = TokenKind::KwCatch;
                else if (match_kw(s, id_len, "finally", 7)) kind = TokenKind::KwFinally;
                else if (match_kw(s, id_len, "throw", 5)) kind = TokenKind::KwThrow;
                else if (match_kw(s, id_len, "class", 5)) kind = TokenKind::KwClass;
                else if (match_kw(s, id_len, "extends", 7)) kind = TokenKind::KwExtends;
                else if (match_kw(s, id_len, "super", 5)) kind = TokenKind::KwSuper;
                else if (match_kw(s, id_len, "this", 4)) kind = TokenKind::KwThis;
                else if (match_kw(s, id_len, "export", 6)) kind = TokenKind::KwExport;
                if (is_event_loop_word(s, id_len) || is_unsupported_word(s, id_len)) {
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported word", start, id_len);
                }
                tokens()[count++] = Token{kind, start, id_len, NO_SLOT, 0.0};
                continue;
            }

            if (c == '}') {
                if (tpl_stack_len > 0u && brace_depth == tpl_brace_depth[tpl_stack_len - 1u]) {
                    ++pos;
                    if (!scan_template_segment(src, len, pos, false, tpl_brace_depth, tpl_stack_len, brace_depth)) {
                        return false;
                    }
                    if (tokens()[count - 1u].kind == TokenKind::TemplateTail) {
                        --tpl_stack_len;
                    }
                    continue;
                }
                if (brace_depth > 0u) {
                    --brace_depth;
                }
                tokens()[count++] = Token{TokenKind::RBrace, pos, 1u, NO_SLOT, 0.0};
                ++pos;
                continue;
            }
            if (c == '{') {
                ++brace_depth;
                tokens()[count++] = Token{TokenKind::LBrace, pos, 1u, NO_SLOT, 0.0};
                ++pos;
                continue;
            }

            const char n1 = (pos + 1u < len) ? src[pos + 1u] : '\0';
            const char n2 = (pos + 2u < len) ? src[pos + 2u] : '\0';
            const char n3 = (pos + 3u < len) ? src[pos + 3u] : '\0';

            if (c == '>' && n1 == '>' && n2 == '>' && n3 == '=') {
                tokens()[count++] = Token{TokenKind::UShrEq, pos, 4u, NO_SLOT, 0.0};
                pos += 4u;
                continue;
            }
            if (c == '<' && n1 == '<' && n2 == '=') {
                tokens()[count++] = Token{TokenKind::ShlEq, pos, 3u, NO_SLOT, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '>' && n1 == '>' && n2 == '=') {
                tokens()[count++] = Token{TokenKind::ShrEq, pos, 3u, NO_SLOT, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '=' && n1 == '>') {
                tokens()[count++] = Token{TokenKind::FatArrow, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '?' && n1 == '.') {
                return fail(VM_REFUSE_UNSUPPORTED, "unsupported operator", pos, 2u);
            }
            if (c == '?' && n1 == '?') {
                if (n2 == '=') {
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported operator", pos, 3u);
                }
                tokens()[count++] = Token{TokenKind::Nullish, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '=' && n1 == '=' && n2 == '=') {
                tokens()[count++] = Token{TokenKind::EqEq, pos, 3u, NO_SLOT, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '!' && n1 == '=' && n2 == '=') {
                tokens()[count++] = Token{TokenKind::BangEq, pos, 3u, NO_SLOT, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '>' && n1 == '>' && n2 == '>') {
                tokens()[count++] = Token{TokenKind::UShr, pos, 3u, NO_SLOT, 0.0};
                pos += 3u;
                continue;
            }
            if (c == '*' && n1 == '*') {
                if (n2 == '=') {
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported operator", pos, 3u);
                }
                tokens()[count++] = Token{TokenKind::StarStar, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '=' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::LooseEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '!' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::LooseNe, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '<' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::Le, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '>' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::Ge, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '<' && n1 == '<') {
                tokens()[count++] = Token{TokenKind::Shl, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '>' && n1 == '>') {
                tokens()[count++] = Token{TokenKind::Shr, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '&' && n1 == '&') {
                if (n2 == '=') {
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported operator", pos, 3u);
                }
                tokens()[count++] = Token{TokenKind::AndAnd, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '|' && n1 == '|') {
                if (n2 == '=') {
                    return fail(VM_REFUSE_UNSUPPORTED, "unsupported operator", pos, 3u);
                }
                tokens()[count++] = Token{TokenKind::OrOr, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '+' && n1 == '+') {
                tokens()[count++] = Token{TokenKind::PlusPlus, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '-' && n1 == '-') {
                tokens()[count++] = Token{TokenKind::MinusMinus, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '+' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::PlusEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '-' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::MinusEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '*' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::StarEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '/' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::SlashEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '%' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::PercentEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '&' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::AmpEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '^' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::CaretEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }
            if (c == '|' && n1 == '=') {
                tokens()[count++] = Token{TokenKind::PipeEq, pos, 2u, NO_SLOT, 0.0};
                pos += 2u;
                continue;
            }

            TokenKind single = TokenKind::Eof;
            switch (c) {
                case '(': single = TokenKind::LParen; break;
                case ')': single = TokenKind::RParen; break;
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
                case '~': single = TokenKind::Tilde; break;
                case '&': single = TokenKind::Amp; break;
                case '|': single = TokenKind::Pipe; break;
                case '^': single = TokenKind::Caret; break;
                case '=': single = TokenKind::Assign; break;
                case '<': single = TokenKind::Lt; break;
                case '>': single = TokenKind::Gt; break;
                default: return fail(VM_REFUSE_UNSUPPORTED, "unsupported character", pos, 1u);
            }
            tokens()[count++] = Token{single, pos, 1u, NO_SLOT, 0.0};
            ++pos;
        }
        tokens()[count++] = Token{TokenKind::Eof, len, 0u, NO_SLOT, 0.0};
        return true;
    }
};

constexpr u32 NO_DEPTH = 0xFFFFFFFFu;
constexpr u32 MAX_LOOP_NESTING = 16u;
constexpr u32 MAX_LOOP_JUMPS = 32u;

struct LoopJumps {
    u32 label_sym;
    bool is_switch;
    u32 breaks[MAX_LOOP_JUMPS];
    u32 break_count;
    u32 continues[MAX_LOOP_JUMPS];
    u32 continue_count;
};

struct LocalBinding {
    u32 name_sym;
    u16 reg;
    bool is_cell;
    u32 decl_depth;
};

enum class VarKind { Local, Cell, Upval, Global };
struct VarRef {
    VarKind kind;
    u16 reg;
    u32 idx;
    u32 sym;
};

constexpr u32 ANVIL_MAX_UPVALUES = 64u;

struct FnScope {
    FnScope* parent;
    u32 local_floor;
    u32 upval_count;
    bool upval_is_local[ANVIL_MAX_UPVALUES];
    u16 upval_index[ANVIL_MAX_UPVALUES];
    u32 upval_sym[ANVIL_MAX_UPVALUES];
};

inline bool is_assign_token(TokenKind k) noexcept {
    switch (k) {
        case TokenKind::Assign:
        case TokenKind::PlusEq:
        case TokenKind::MinusEq:
        case TokenKind::StarEq:
        case TokenKind::SlashEq:
        case TokenKind::PercentEq:
        case TokenKind::ShlEq:
        case TokenKind::ShrEq:
        case TokenKind::UShrEq:
        case TokenKind::AmpEq:
        case TokenKind::CaretEq:
        case TokenKind::PipeEq:
            return true;
        default:
            return false;
    }
}

inline AnvilOpcode assign_token_opcode(TokenKind k) noexcept {
    switch (k) {
        case TokenKind::PlusEq:    return AnvilOpcode::Add;
        case TokenKind::MinusEq:   return AnvilOpcode::Sub;
        case TokenKind::StarEq:    return AnvilOpcode::Mul;
        case TokenKind::SlashEq:   return AnvilOpcode::Div;
        case TokenKind::PercentEq: return AnvilOpcode::Mod;
        case TokenKind::ShlEq:     return AnvilOpcode::Shl;
        case TokenKind::ShrEq:     return AnvilOpcode::Shr;
        case TokenKind::UShrEq:    return AnvilOpcode::UShr;
        case TokenKind::AmpEq:     return AnvilOpcode::BitAnd;
        case TokenKind::CaretEq:   return AnvilOpcode::BitXor;
        case TokenKind::PipeEq:    return AnvilOpcode::BitOr;
        default:                   return AnvilOpcode::Nop;
    }
}

struct AnvilCompiler {
    AnvilVmContext* ctx;
    const char* src;
    AnvilArena arena;
    AnvilLexer lexer;
    u32 cursor;
    u32 locals_off;
    u32 locals_cap;
    u32 local_count;
    u32 local_floor;
    bool fn_uses_cells;
    static constexpr u32 MAX_UPVALUES = ANVIL_MAX_UPVALUES;
    FnScope root_fn;
    FnScope* cur_fn;
    u16 next_reg;
    u16 reg_floor;
    u16 high_reg;
    u32 next_loop_id;
    LoopJumps loops[MAX_LOOP_NESTING];
    u32 loop_depth;
    u32 pending_label_sym;
    bool in_function;
    bool in_derived_ctor;
    bool had_error;
    u32 block_depth;
    u64 stack_base;
    u32 stack_budget;

    struct BlockScope {
        AnvilCompiler& c;
        explicit BlockScope(AnvilCompiler& comp) noexcept : c(comp) {
            ++c.block_depth;
        }
        ~BlockScope() {
            while (c.local_count > c.local_floor &&
                   c.locals()[c.local_count - 1u].decl_depth == c.block_depth) {
                --c.local_count;
            }
            --c.block_depth;
        }
        BlockScope(const BlockScope&) = delete;
        BlockScope& operator=(const BlockScope&) = delete;
    };

    bool within_stack_budget() noexcept {
        char probe;
        const u64 here = reinterpret_cast<u64>(&probe);
        return here >= stack_base ? true : (stack_base - here) < stack_budget;
    }

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
        if (id_tok.length == 0u || id_tok.length >= 64u || id_tok.decoded_off != NO_SLOT) {
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
        const u16 rhs = compile_assignment();
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

    bool begin_loop(bool is_switch = false) noexcept {
        if (loop_depth >= MAX_LOOP_NESTING) {
            reject_limit(VM_REFUSE_LOOPS, "loop nesting", MAX_LOOP_NESTING, "loops");
            return false;
        }
        loops[loop_depth].label_sym = pending_label_sym;
        loops[loop_depth].is_switch = is_switch;
        loops[loop_depth].break_count = 0u;
        loops[loop_depth].continue_count = 0u;
        pending_label_sym = NO_SLOT;
        ++loop_depth;
        return true;
    }

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

    void compile_loop_jump(bool is_break) noexcept {
        if (loop_depth == 0u) {
            reject_here(VM_REFUSE_SYNTAX, is_break ? "break outside a loop" : "continue outside a loop");
            return;
        }
        i32 target_idx = static_cast<i32>(loop_depth) - 1;
        if (peek().kind == TokenKind::Ident) {
            const Token lbl_tok = advance();
            const u32 lbl_sym = token_sym(lbl_tok);
            target_idx = -1;
            for (i32 i = static_cast<i32>(loop_depth) - 1; i >= 0; --i) {
                if (loops[i].label_sym == lbl_sym && (is_break || !loops[i].is_switch)) {
                    target_idx = i;
                    break;
                }
            }
            if (target_idx < 0) {
                reject_here(VM_REFUSE_UNSUPPORTED, "labels are not compiled");
                return;
            }
        } else if (!is_break) {
            while (target_idx >= 0 && loops[target_idx].is_switch) {
                --target_idx;
            }
            if (target_idx < 0) {
                reject_here(VM_REFUSE_SYNTAX, "continue outside a loop");
                return;
            }
        }
        LoopJumps& jumps = loops[target_idx];
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
            const bool stmt_pos =
                (before == TokenKind::Semicolon || before == TokenKind::RBrace ||
                 before == TokenKind::LBrace || before == TokenKind::KwExport);
            if (!stmt_pos) {
                continue;
            }
            if (depth != 0u) {
                continue;
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

    u16 declare_local(u32 sym, bool is_var = false) noexcept {
        const u32 d = is_var ? 0u : block_depth;
        for (i32 i = static_cast<i32>(local_count) - 1; i >= static_cast<i32>(local_floor); --i) {
            if (locals()[i].name_sym == sym && (is_var || locals()[i].decl_depth == d)) {
                return locals()[i].reg;
            }
        }
        const u16 r = alloc_reg();
        if (ensure_locals(local_count + 1u)) {
            locals()[local_count++] = LocalBinding{sym, r, fn_uses_cells, d};
        } else {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
        }
        if (r + 1u > reg_floor) {
            reg_floor = static_cast<u16>(r + 1u);
        }
        return r;
    }

    i32 add_upvalue(FnScope* fs, bool is_local, u16 index, u32 sym) noexcept {
        for (u32 i = 0u; i < fs->upval_count; ++i) {
            if (fs->upval_sym[i] == sym) {
                return static_cast<i32>(i);
            }
        }
        if (fs->upval_count >= MAX_UPVALUES) {
            reject_limit(VM_REFUSE_REGISTERS, "captured variable", MAX_UPVALUES,
                         "captured variables per function");
            return 0;
        }
        fs->upval_is_local[fs->upval_count] = is_local;
        fs->upval_index[fs->upval_count] = index;
        fs->upval_sym[fs->upval_count] = sym;
        return static_cast<i32>(fs->upval_count++);
    }

    i32 resolve_upvalue(FnScope* fs, u32 sym) noexcept {
        FnScope* parent = fs->parent;
        if (parent == nullptr) {
            return -1;
        }
        for (i32 i = static_cast<i32>(fs->local_floor) - 1;
             i >= static_cast<i32>(parent->local_floor); --i) {
            if (locals()[i].name_sym == sym) {
                locals()[i].is_cell = true;
                return add_upvalue(fs, true, locals()[i].reg, sym);
            }
        }
        const i32 pu = resolve_upvalue(parent, sym);
        if (pu >= 0) {
            return add_upvalue(fs, false, static_cast<u16>(pu), sym);
        }
        return -1;
    }

    VarRef resolve_var(u32 sym) noexcept {
        for (i32 i = static_cast<i32>(local_count) - 1; i >= static_cast<i32>(local_floor); --i) {
            if (locals()[i].name_sym == sym) {
                return VarRef{locals()[i].is_cell ? VarKind::Cell : VarKind::Local,
                              locals()[i].reg, 0u, sym};
            }
        }
        if (cur_fn != nullptr && cur_fn->parent != nullptr) {
            const i32 u = resolve_upvalue(cur_fn, sym);
            if (u >= 0) {
                return VarRef{VarKind::Upval, 0u, static_cast<u32>(u), sym};
            }
        }
        return VarRef{VarKind::Global, 0u, 0u, sym};
    }

    u16 load_var(const VarRef& vr) noexcept {
        switch (vr.kind) {
            case VarKind::Local:
                return vr.reg;
            case VarKind::Cell: {
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CellGet, dst, vr.reg, 0u));
                return dst;
            }
            case VarKind::Upval: {
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::GetUpval, dst, static_cast<u16>(vr.idx), 0u));
                return dst;
            }
            case VarKind::Global:
            default: {
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abx(
                    AnvilOpcode::GetGlobal, dst, global_slot(vr.sym)));
                return dst;
            }
        }
    }

    void store_var(const VarRef& vr, u16 src_reg) noexcept {
        switch (vr.kind) {
            case VarKind::Local:
                if (vr.reg != src_reg) {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, vr.reg, src_reg, 0u));
                }
                break;
            case VarKind::Cell:
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CellSet, vr.reg, src_reg, 0u));
                break;
            case VarKind::Upval:
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::SetUpval, static_cast<u16>(vr.idx), src_reg, 0u));
                break;
            case VarKind::Global:
            default:
                if (find_function(vr.sym) != NO_SLOT) {
                    reject_here(VM_REFUSE_UNSUPPORTED, "cannot reassign a declared function");
                    break;
                }
                ctx->program.emit(AnvilInstruction::make_abx(
                    AnvilOpcode::SetGlobal, src_reg, global_slot(vr.sym)));
                break;
        }
    }

    void init_local(u16 reg, bool is_cell, u16 value_reg) noexcept {
        if (is_cell) {
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewCell, reg, value_reg, 0u));
        } else if (reg != value_reg) {
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, reg, value_reg, 0u));
        }
    }

    bool body_has_nested_function() const noexcept {
        if (cursor >= lexer.count) {
            return false;
        }
        if (lexer.tokens()[cursor].kind == TokenKind::LBrace) {
            u32 d = 0u;
            for (u32 k = 0u; cursor + k < lexer.count; ++k) {
                const TokenKind kk = lexer.tokens()[cursor + k].kind;
                if (kk == TokenKind::LBrace) {
                    ++d;
                } else if (kk == TokenKind::RBrace) {
                    --d;
                    if (d == 0u) { return false; }
                } else if (kk == TokenKind::KwFunction || kk == TokenKind::FatArrow ||
                           kk == TokenKind::KwClass) {
                    return true;
                }
            }
            return false;
        }
        i32 d = 0;
        for (u32 k = 0u; cursor + k < lexer.count; ++k) {
            const TokenKind kk = lexer.tokens()[cursor + k].kind;
            if (kk == TokenKind::LParen || kk == TokenKind::LBracket || kk == TokenKind::LBrace) {
                ++d;
            } else if (kk == TokenKind::RParen || kk == TokenKind::RBracket ||
                       kk == TokenKind::RBrace) {
                if (d == 0) { return false; }
                --d;
            } else if (d == 0 && (kk == TokenKind::Comma || kk == TokenKind::Semicolon)) {
                return false;
            } else if (kk == TokenKind::KwFunction || kk == TokenKind::FatArrow ||
                       kk == TokenKind::KwClass) {
                return true;
            }
        }
        return false;
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
        const char* ptr = (t.decoded_off != NO_SLOT)
            ? reinterpret_cast<const char*>(arena.at(t.decoded_off))
            : &src[t.offset];
        const u32 sym = ctx->heap.intern_string(ptr, t.length);
        if (sym == NO_SLOT) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return 0u;
        }
        return sym;
    }

    void emit_load_undefined(u16 dst) noexcept {
        const u32 cidx = ctx->program.add_constant(AnvilValue::make_undefined());
        ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
    }

    void emit_load_number(u16 dst, f64 n) noexcept {
        const bool neg_zero = (n == 0.0 && (f64_to_bits(n) >> 63u) != 0u);
        if (!neg_zero && n >= -32768.0 && n <= 32767.0) {
            const i32 as_int = static_cast<i32>(n);
            if (static_cast<f64>(as_int) == n) {
                ctx->program.emit(AnvilInstruction::make_abx(
                    AnvilOpcode::LoadSmi,
                    dst,
                    static_cast<u16>(static_cast<short>(as_int))
                ));
                return;
            }
        }
        const u32 cidx = ctx->program.add_constant(AnvilValue::make_number(n));
        ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
    }

    bool is_arrow_ahead() const noexcept {
        u32 depth = 0u;
        for (u32 k = 0u; cursor + k < lexer.count; ++k) {
            const TokenKind kk = lexer.tokens()[cursor + k].kind;
            if (kk == TokenKind::LParen) {
                ++depth;
            } else if (kk == TokenKind::RParen) {
                if (depth > 0u) {
                    --depth;
                }
                if (depth == 0u) {
                    const u32 after = cursor + k + 1u;
                    return after < lexer.count &&
                           lexer.tokens()[after].kind == TokenKind::FatArrow;
                }
            } else if (kk == TokenKind::Eof) {
                break;
            }
        }
        return false;
    }

    u16 compile_function_literal(
        bool is_arrow,
        bool paren_params,
        u32 single_param_sym,
        u32 fn_name_sym = 0xFFFFFFFFu,
        bool is_derived_ctor = false
    ) noexcept {
        const u32 fn_id = ctx->heap.alloc_function(fn_name_sym, 0u, 0u, 0u, 0u);
        if (fn_id == NO_SLOT) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return alloc_reg();
        }

        const u32 saved_local_count = local_count;
        const u32 saved_local_floor = local_floor;
        const u16 saved_next_reg = next_reg;
        const u16 saved_reg_floor = reg_floor;
        const u16 saved_high_reg = high_reg;
        const bool saved_in_fn = in_function;
        const bool saved_in_derived = in_derived_ctor;
        const bool saved_uses_cells = fn_uses_cells;
        const u32 saved_loop_depth = loop_depth;
        const u32 saved_block_depth = block_depth;
        FnScope fs;
        fs.parent = cur_fn;
        fs.local_floor = local_count;
        fs.upval_count = 0u;
        FnScope* saved_cur_fn = cur_fn;
        cur_fn = &fs;

        local_floor = local_count;
        next_reg = 0u;
        reg_floor = 0u;
        high_reg = 0u;
        loop_depth = 0u;
        block_depth = 0u;
        in_function = true;
        in_derived_ctor = is_derived_ctor;
        fn_uses_cells = false;

        u32 arity = 0u;
        if (single_param_sym != NO_SLOT) {
            declare_local(single_param_sym, true);
            arity = 1u;
        } else if (paren_params) {
            expect(TokenKind::LParen);
            while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                const Token p_tok = advance();
                if (p_tok.kind != TokenKind::Ident) {
                    reject_syntax();
                    break;
                }
                declare_local(token_sym(p_tok), true);
                ++arity;
                if (!match(TokenKind::Comma)) {
                    break;
                }
            }
            expect(TokenKind::RParen);
        }
        if (is_arrow) {
            expect(TokenKind::FatArrow);
        }

        fn_uses_cells = body_has_nested_function();

        const u32 jskip_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
        const u32 fn_start_pc = ctx->program.code_len;
        if (fn_id < ctx->heap.function_count) {
            ctx->heap.functions()[fn_id].code_start = fn_start_pc;
            ctx->heap.functions()[fn_id].arity = arity;
        }

        if (fn_uses_cells) {
            for (u32 i = local_floor; i < local_count; ++i) {
                locals()[i].is_cell = true;
                const u16 preg = locals()[i].reg;
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewCell, preg, preg, 0u));
            }
        }

        if (is_arrow && peek().kind != TokenKind::LBrace) {
            const u16 r = compile_assignment();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, r, 0u, 0u));
        } else {
            compile_statement();
            const u16 ft = alloc_reg();
            emit_load_undefined(ft);
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, ft, 0u, 0u));
        }

        if (fn_id < ctx->heap.function_count) {
            ctx->heap.functions()[fn_id].local_count = high_reg;
            ctx->heap.functions()[fn_id].code_len = ctx->program.code_len - fn_start_pc;
        }
        ctx->program.code()[jskip_pc] =
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, ctx->program.code_len);

        const u32 n_up = fs.upval_count;
        bool captured_is_local[MAX_UPVALUES];
        u16 captured_index[MAX_UPVALUES];
        for (u32 i = 0u; i < n_up; ++i) {
            captured_is_local[i] = fs.upval_is_local[i];
            captured_index[i] = fs.upval_index[i];
        }

        local_count = saved_local_count;
        local_floor = saved_local_floor;
        next_reg = saved_next_reg;
        reg_floor = saved_reg_floor;
        high_reg = saved_high_reg;
        loop_depth = saved_loop_depth;
        block_depth = saved_block_depth;
        in_function = saved_in_fn;
        in_derived_ctor = saved_in_derived;
        fn_uses_cells = saved_uses_cells;
        cur_fn = saved_cur_fn;

        const u16 dst = alloc_reg();
        if (n_up == 0u) {
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadFunc, dst, fn_id));
        } else {
            const u16 base = next_reg;
            for (u32 i = 0u; i < n_up; ++i) {
                const u16 slot = alloc_reg();
                if (captured_is_local[i]) {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot, captured_index[i], 0u));
                } else {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetUpvalCell, slot, captured_index[i], 0u));
                }
            }
            ctx->program.emit(AnvilInstruction{AnvilOpcode::MakeClosure, 0u, dst, base,
                                               static_cast<u16>(n_up), fn_id});
        }
        return dst;
    }

    bool is_property_name_token(TokenKind k) const noexcept {
        return k == TokenKind::Ident ||
               (static_cast<u8>(k) >= static_cast<u8>(TokenKind::KwLet) &&
                static_cast<u8>(k) <= static_cast<u8>(TokenKind::KwNull)) ||
               k == TokenKind::KwNew || k == TokenKind::KwDo ||
               k == TokenKind::KwBreak || k == TokenKind::KwContinue ||
               (static_cast<u8>(k) >= static_cast<u8>(TokenKind::KwTypeof) &&
                static_cast<u8>(k) <= static_cast<u8>(TokenKind::KwExport));
    }

    u8 builtin_ctor_kind(const char* s, u32 len) const noexcept {
        if (match_kw(s, len, "Error", 5)) return static_cast<u8>(BuiltinCtorKind::Error);
        if (match_kw(s, len, "TypeError", 9)) return static_cast<u8>(BuiltinCtorKind::TypeError);
        if (match_kw(s, len, "RangeError", 10)) return static_cast<u8>(BuiltinCtorKind::RangeError);
        if (match_kw(s, len, "SyntaxError", 11)) return static_cast<u8>(BuiltinCtorKind::SyntaxError);
        if (match_kw(s, len, "ReferenceError", 14)) return static_cast<u8>(BuiltinCtorKind::ReferenceError);
        if (match_kw(s, len, "Map", 3)) return static_cast<u8>(BuiltinCtorKind::Map);
        if (match_kw(s, len, "Set", 3)) return static_cast<u8>(BuiltinCtorKind::Set);
        if (match_kw(s, len, "ArrayBuffer", 11)) return static_cast<u8>(BuiltinCtorKind::ArrayBuffer);
        if (match_kw(s, len, "Uint8Array", 10)) return static_cast<u8>(BuiltinCtorKind::Uint8Array);
        if (match_kw(s, len, "Uint16Array", 11)) return static_cast<u8>(BuiltinCtorKind::Uint16Array);
        if (match_kw(s, len, "Int32Array", 10)) return static_cast<u8>(BuiltinCtorKind::Int32Array);
        if (match_kw(s, len, "Uint32Array", 11)) return static_cast<u8>(BuiltinCtorKind::Uint32Array);
        if (match_kw(s, len, "Float64Array", 12)) return static_cast<u8>(BuiltinCtorKind::Float64Array);
        if (match_kw(s, len, "Array", 5)) return static_cast<u8>(BuiltinCtorKind::Array);
        if (match_kw(s, len, "Object", 6)) return static_cast<u8>(BuiltinCtorKind::Object);
        return 0u;
    }

    bool is_shadowed_name(u32 sym) const noexcept {
        if (find_local(sym) >= 0 || find_function(sym) != NO_SLOT) {
            return true;
        }
        if (cur_fn != nullptr && cur_fn->parent != nullptr) {
            for (i32 i = static_cast<i32>(local_floor) - 1; i >= 0; --i) {
                if (locals()[i].name_sym == sym) {
                    return true;
                }
            }
        }
        for (u32 gi = 0u; gi < ctx->global_count; ++gi) {
            if (ctx->globals()[gi].occupied && ctx->globals()[gi].name_sym == sym &&
                ctx->globals()[gi].declared) {
                return true;
            }
        }
        return false;
    }

    u16 synthesize_default_ctor(u32 class_name_sym, bool has_extends) noexcept {
        const u32 fn_id = ctx->heap.alloc_function(class_name_sym, has_extends ? 8u : 0u, 0u, 0u, 0u);
        if (fn_id == NO_SLOT) {
            reject_limit(VM_REFUSE_HEAP, "heap", ctx->heap.arena.budget, "bytes");
            return alloc_reg();
        }
        const u32 jskip_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
        const u32 fn_start_pc = ctx->program.code_len;
        if (fn_id < ctx->heap.function_count) {
            ctx->heap.functions()[fn_id].code_start = fn_start_pc;
        }
        if (has_extends) {
            // Parameters 0..7 are in registers 0..7; CallSuperCtor forwards them.
            ctx->program.emit(AnvilInstruction{AnvilOpcode::CallSuperCtor, 0u, 8u, 0u, 8u, 0u});
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadConst, 9u,
                static_cast<u16>(ctx->program.add_constant(AnvilValue::make_undefined())), 0u));
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, 9u, 0u, 0u));
            if (fn_id < ctx->heap.function_count) {
                ctx->heap.functions()[fn_id].local_count = 10u;
                ctx->heap.functions()[fn_id].code_len = ctx->program.code_len - fn_start_pc;
            }
        } else {
            const u32 cidx = ctx->program.add_constant(AnvilValue::make_undefined());
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, 0u, cidx));
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Return, 0u, 0u, 0u));
            if (fn_id < ctx->heap.function_count) {
                ctx->heap.functions()[fn_id].local_count = 1u;
                ctx->heap.functions()[fn_id].code_len = ctx->program.code_len - fn_start_pc;
            }
        }
        ctx->program.code()[jskip_pc] =
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, ctx->program.code_len);
        const u16 dst = alloc_reg();
        ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadFunc, dst, fn_id));
        return dst;
    }

    u16 compile_class_literal(u32 class_name_sym) noexcept {
        bool has_extends = false;
        u16 super_reg = 0u;
        if (match(TokenKind::KwExtends)) {
            has_extends = true;
            super_reg = compile_unary();
        } else {
            super_reg = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, super_reg, 0u, 0u));
        }

        expect(TokenKind::LBrace);

        struct MethodEntry {
            u32 name_sym;
            u16 fn_reg;
            bool is_static;
        };
        MethodEntry methods[64];
        u32 method_count = 0u;
        bool has_ctor = false;
        u16 ctor_reg = 0u;

        while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
            if (match(TokenKind::Semicolon)) {
                continue;
            }
            bool is_static = false;
            if (peek().kind == TokenKind::Ident &&
                match_kw(&src[peek().offset], peek().length, "static", 6) &&
                peek(1u).kind != TokenKind::LParen) {
                advance();
                is_static = true;
            }
            const Token m_tok = advance();
            if (!is_property_name_token(m_tok.kind)) {
                reject_syntax();
                break;
            }
            const u32 m_sym = token_sym(m_tok);
            const bool is_ctor = (!is_static &&
                m_tok.kind == TokenKind::Ident &&
                match_kw(&src[m_tok.offset], m_tok.length, "constructor", 11));
            if (is_ctor) {
                has_ctor = true;
                ctor_reg = compile_function_literal(false, true, NO_SLOT, class_name_sym, has_extends);
            } else {
                const u16 m_reg = compile_function_literal(false, true, NO_SLOT, m_sym, false);
                if (method_count < 64u) {
                    methods[method_count++] = MethodEntry{m_sym, m_reg, is_static};
                }
            }
        }
        expect(TokenKind::RBrace);

        if (!has_ctor) {
            ctor_reg = synthesize_default_ctor(class_name_sym, has_extends);
        }

        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetupClass, ctor_reg, super_reg, 0u));
        for (u32 i = 0u; i < method_count; ++i) {
            ctx->program.emit(AnvilInstruction{
                AnvilOpcode::DefineMethod,
                methods[i].is_static ? 1u : 0u,
                ctor_reg,
                methods[i].fn_reg,
                0u,
                methods[i].name_sym
            });
        }
        return ctor_reg;
    }

    u16 compile_new_expr() noexcept {
        if (peek().kind == TokenKind::Ident) {
            const Token& id_tok = peek();
            const u32 sym = token_sym(id_tok);
            const u8 bkind = builtin_ctor_kind(&src[id_tok.offset], id_tok.length);
            if (bkind != 0u && !is_shadowed_name(sym) && peek(1u).kind == TokenKind::LParen) {
                advance(); // constructor name
                expect(TokenKind::LParen);
                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_assignment();
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
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < argc && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (arg_regs[i] != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                    }
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction{
                    AnvilOpcode::NewBuiltin, bkind, dst, call_base, static_cast<u16>(argc), 0u
                });
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }
        }

        // General `new Callee(args...)`: compile callee primary + member accesses before `(`.
        u16 callee_reg = 0u;
        if (peek().kind == TokenKind::Ident) {
            const Token id_tok = advance();
            const u32 sym = token_sym(id_tok);
            const VarRef vr = resolve_var(sym);
            if (vr.kind == VarKind::Global) {
                const u32 fn_as_val = find_function(sym);
                if (fn_as_val != NO_SLOT) {
                    callee_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadFunc, callee_reg, fn_as_val));
                } else {
                    bool found_global = false;
                    for (u32 gi = 0u; gi < ctx->global_count; ++gi) {
                        if (ctx->globals()[gi].occupied && ctx->globals()[gi].name_sym == sym &&
                            ctx->globals()[gi].declared) {
                            found_global = true;
                            break;
                        }
                    }
                    if (!found_global) {
                        reject_name(VM_REFUSE_UNKNOWN_NAME, "call to a function not declared in this script", id_tok);
                    }
                    callee_reg = load_var(vr);
                }
            } else {
                callee_reg = load_var(vr);
            }
        } else {
            callee_reg = compile_primary();
        }
        while (peek().kind == TokenKind::Dot || peek().kind == TokenKind::LBracket) {
            if (match(TokenKind::Dot)) {
                const Token prop_tok = advance();
                if (!is_property_name_token(prop_tok.kind)) {
                    reject_syntax();
                    break;
                }
                const u32 prop_sym = token_sym(prop_tok);
                const u32 ic_site = new_site(prop_sym);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::GetPropIC, dst, callee_reg, static_cast<u16>(ic_site)));
                callee_reg = dst;
            } else if (match(TokenKind::LBracket)) {
                const u16 idx_reg = compile_expr();
                expect(TokenKind::RBracket);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, dst, callee_reg, idx_reg));
                callee_reg = dst;
            }
        }

        u16 arg_regs[MAX_CALL_ARGS];
        u32 argc = 0u;
        if (match(TokenKind::LParen)) {
            while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                const u16 arg_r = compile_assignment();
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
        }
        const u16 call_base = next_reg;
        for (u32 i = 0u; i < argc && !had_error; ++i) {
            const u16 slot_r = alloc_reg();
            if (arg_regs[i] != slot_r) {
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
            }
        }
        const u16 dst = alloc_reg();
        ctx->program.emit(AnvilInstruction{
            AnvilOpcode::NewInstance, 0u, dst, callee_reg, call_base, argc
        });
        return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
    }

    u16 compile_primary() noexcept {
        const Token& t = peek();
        if (t.kind == TokenKind::Number) {
            advance();
            const u16 dst = alloc_reg();
            emit_load_number(dst, t.num_val);
            return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwTrue) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadTrue, dst, 0u, 0u));
            return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwFalse) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadFalse, dst, 0u, 0u));
            return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwNull) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, dst, 0u, 0u));
            return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwThis) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadThis, dst, 0u, 0u));
            return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwNew) {
            advance();
            return compile_new_expr();
        }
        if (t.kind == TokenKind::KwSuper) {
            advance();
            if (match(TokenKind::LParen)) {
                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_assignment();
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
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < argc && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (arg_regs[i] != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                    }
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction{
                    AnvilOpcode::CallSuperCtor, 0u, dst, call_base, static_cast<u16>(argc), 0u
                });
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
            }
            if (match(TokenKind::Dot)) {
                const Token m_tok = advance();
                if (!is_property_name_token(m_tok.kind)) {
                    reject_syntax();
                    return alloc_reg();
                }
                const u32 m_sym = token_sym(m_tok);
                expect(TokenKind::LParen);
                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_assignment();
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
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < argc && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (arg_regs[i] != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                    }
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction{
                    AnvilOpcode::CallSuperMethod,
                    static_cast<u8>(argc < 255u ? argc : 255u),
                    dst,
                    call_base,
                    0u,
                    m_sym
                });
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
            }
            reject_syntax();
            return alloc_reg();
        }
        if (t.kind == TokenKind::String || t.kind == TokenKind::TemplateFull) {
            advance();
            const u32 sym = token_sym(t);
            const u16 dst = alloc_reg();
            const u32 cidx = ctx->program.add_constant(AnvilValue::make_string(sym));
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
            return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::TemplateHead) {
            const Token head_tok = advance();
            const u32 head_sym = token_sym(head_tok);
            u16 acc_reg = alloc_reg();
            const u32 cidx = ctx->program.add_constant(AnvilValue::make_string(head_sym));
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, acc_reg, cidx));
            while (!had_error && peek().kind != TokenKind::Eof) {
                const u16 expr_reg = compile_expr();
                const u16 next_acc = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Add, next_acc, acc_reg, expr_reg));
                acc_reg = next_acc;
                if (peek().kind == TokenKind::TemplateMiddle) {
                    const Token mid_tok = advance();
                    if (mid_tok.length > 0u) {
                        const u32 mid_sym = token_sym(mid_tok);
                        const u16 mid_reg = alloc_reg();
                        const u32 mcidx = ctx->program.add_constant(AnvilValue::make_string(mid_sym));
                        ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, mid_reg, mcidx));
                        const u16 joined = alloc_reg();
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Add, joined, acc_reg, mid_reg));
                        acc_reg = joined;
                    }
                } else if (peek().kind == TokenKind::TemplateTail) {
                    const Token tail_tok = advance();
                    if (tail_tok.length > 0u) {
                        const u32 tail_sym = token_sym(tail_tok);
                        const u16 tail_reg = alloc_reg();
                        const u32 tcidx = ctx->program.add_constant(AnvilValue::make_string(tail_sym));
                        ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, tail_reg, tcidx));
                        const u16 joined = alloc_reg();
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Add, joined, acc_reg, tail_reg));
                        acc_reg = joined;
                    }
                    break;
                } else {
                    reject_syntax();
                    break;
                }
            }
            return compile_postfix(acc_reg, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwFunction) {
            advance();
            u32 fn_name_sym = 0xFFFFFFFFu;
            if (peek().kind == TokenKind::Ident) {
                fn_name_sym = token_sym(advance());
            }
            const u16 fn_reg = compile_function_literal(false, true, NO_SLOT, fn_name_sym, false);
            return compile_postfix(fn_reg, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::KwClass) {
            advance();
            u32 cls_sym = 0xFFFFFFFFu;
            if (peek().kind == TokenKind::Ident) {
                cls_sym = token_sym(advance());
            }
            const u16 cls_reg = compile_class_literal(cls_sym);
            return compile_postfix(cls_reg, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::LParen) {
            if (is_arrow_ahead()) {
                return compile_function_literal(true, true, NO_SLOT);
            }
            advance();
            const u16 inner = compile_expr();
            expect(TokenKind::RParen);
            return compile_postfix(inner, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::LBrace) {
            advance();
            const u16 obj_reg = alloc_reg();
            const u16 obj_mark = next_reg;
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewObject, obj_reg, 0u, 0u));
            while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
                const Token key_tok = advance();
                if (!is_property_name_token(key_tok.kind) && key_tok.kind != TokenKind::String) {
                    reject_syntax();
                    break;
                }
                const u32 prop_sym = token_sym(key_tok);
                u16 val_reg = 0u;
                if (match(TokenKind::Colon)) {
                    val_reg = compile_assignment();
                } else if (key_tok.kind == TokenKind::Ident &&
                           (peek().kind == TokenKind::Comma || peek().kind == TokenKind::RBrace)) {
                    // Shorthand property `{ a, b }`.
                    const VarRef vr = resolve_var(prop_sym);
                    val_reg = load_var(vr);
                } else {
                    reject_syntax();
                    break;
                }
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
            return compile_postfix(obj_reg, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::LBracket) {
            advance();
            const u16 arr_reg = alloc_reg();
            const u16 arr_mark = next_reg;
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewArray, arr_reg, 8u, 0u));
            while (peek().kind != TokenKind::RBracket && peek().kind != TokenKind::Eof && !had_error) {
                const u16 elem_reg = compile_assignment();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ArrayPush, arr_reg, elem_reg, 0u));
                if (next_reg > arr_mark) {
                    next_reg = arr_mark;
                }
                if (!match(TokenKind::Comma)) {
                    break;
                }
            }
            expect(TokenKind::RBracket);
            return compile_postfix(arr_reg, VarRef{VarKind::Global, 0u, 0u, 0u}, false);
        }
        if (t.kind == TokenKind::Ident) {
            const Token id_tok = advance();
            const u32 sym = token_sym(id_tok);

            if (peek().kind == TokenKind::FatArrow) {
                return compile_function_literal(true, false, sym);
            }

            // Built-in `console.log(args...)`.
            if (match_kw(&src[id_tok.offset], id_tok.length, "console", 7) &&
                !is_shadowed_name(sym) &&
                peek(0u).kind == TokenKind::Dot && peek(1u).kind == TokenKind::Ident &&
                match_kw(&src[peek(1u).offset], peek(1u).length, "log", 3) &&
                peek(2u).kind == TokenKind::LParen) {
                advance(); // dot
                advance(); // log
                expect(TokenKind::LParen);
                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_assignment();
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
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < argc && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (i < argc && arg_regs[i] != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                    }
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction{AnvilOpcode::HostCall, 0u, dst, call_base,
                                                   static_cast<u16>(argc), 0u});
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }

            // Global isNaN / isFinite.
            if ((match_kw(&src[id_tok.offset], id_tok.length, "isNaN", 5) ||
                 match_kw(&src[id_tok.offset], id_tok.length, "isFinite", 8)) &&
                !is_shadowed_name(sym) &&
                peek().kind == TokenKind::LParen) {
                const u8 pred_op =
                    match_kw(&src[id_tok.offset], id_tok.length, "isNaN", 5) ? 4u : 5u;
                expect(TokenKind::LParen);
                const u16 arg_reg = compile_assignment();
                expect(TokenKind::RParen);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CallPred, dst, pred_op, arg_reg));
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }

            // Built-in `String.fromCharCode(...)`.
            if (match_kw(&src[id_tok.offset], id_tok.length, "String", 6) &&
                !is_shadowed_name(sym) &&
                peek(0u).kind == TokenKind::Dot && peek(1u).kind == TokenKind::Ident &&
                match_kw(&src[peek(1u).offset], peek(1u).length, "fromCharCode", 12) &&
                peek(2u).kind == TokenKind::LParen) {
                advance(); // dot
                advance(); // fromCharCode
                expect(TokenKind::LParen);
                if (peek().kind == TokenKind::RParen) {
                    advance();
                    const u16 dst = alloc_reg();
                    const u32 empty_sym = intern_name("", 0u);
                    const u32 cidx = ctx->program.add_constant(AnvilValue::make_string(empty_sym));
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }
                const u16 first_arg = compile_assignment();
                u16 acc = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::StringFromCharCode, acc, first_arg, 0u));
                while (match(TokenKind::Comma)) {
                    if (peek().kind == TokenKind::RParen) break;
                    const u16 next_arg = compile_assignment();
                    const u16 part = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::StringFromCharCode, part, next_arg, 0u));
                    const u16 joined = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Add, joined, acc, part));
                    acc = joined;
                }
                expect(TokenKind::RParen);
                return compile_postfix(acc, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }

            // Built-in `Array.isArray(arg)`.
            if (match_kw(&src[id_tok.offset], id_tok.length, "Array", 5) &&
                !is_shadowed_name(sym) &&
                peek(0u).kind == TokenKind::Dot && peek(1u).kind == TokenKind::Ident &&
                match_kw(&src[peek(1u).offset], peek(1u).length, "isArray", 7) &&
                peek(2u).kind == TokenKind::LParen) {
                advance();
                advance();
                expect(TokenKind::LParen);
                const u16 arg_reg = compile_assignment();
                expect(TokenKind::RParen);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CallPred, dst, 6u, arg_reg));
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }

            // Built-in `Math.<op>(...)` and `Math.<CONST>`.
            if (match_kw(&src[id_tok.offset], id_tok.length, "Math", 4) &&
                !is_shadowed_name(sym) && match(TokenKind::Dot)) {
                const Token m_tok = advance();
                const char* mp = &src[m_tok.offset];
                const u32 ml = m_tok.length;

                if (peek().kind != TokenKind::LParen) {
                    f64 cval = 0.0;
                    bool is_const = true;
                    if (match_kw(mp, ml, "PI", 2)) cval = 3.141592653589793;
                    else if (match_kw(mp, ml, "E", 1)) cval = 2.718281828459045;
                    else if (match_kw(mp, ml, "LN2", 3)) cval = 0.6931471805599453;
                    else if (match_kw(mp, ml, "LN10", 4)) cval = 2.302585092994046;
                    else if (match_kw(mp, ml, "LOG2E", 5)) cval = 1.4426950408889634;
                    else if (match_kw(mp, ml, "LOG10E", 6)) cval = 0.4342944819032518;
                    else if (match_kw(mp, ml, "SQRT2", 5)) cval = 1.4142135623730951;
                    else if (match_kw(mp, ml, "SQRT1_2", 7)) cval = 0.7071067811865476;
                    else is_const = false;
                    if (!is_const) {
                        reject_name(VM_REFUSE_UNSUPPORTED, "unsupported Math member", m_tok);
                    }
                    const u16 dst = alloc_reg();
                    emit_load_number(dst, cval);
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }

                u8 math_op = 0u;
                if (match_kw(mp, ml, "abs", 3)) math_op = 1u;
                else if (match_kw(mp, ml, "floor", 5)) math_op = 2u;
                else if (match_kw(mp, ml, "ceil", 4)) math_op = 3u;
                else if (match_kw(mp, ml, "round", 5)) math_op = 4u;
                else if (match_kw(mp, ml, "sqrt", 4)) math_op = 5u;
                else if (match_kw(mp, ml, "trunc", 5)) math_op = 6u;
                else if (match_kw(mp, ml, "sign", 4)) math_op = 7u;
                if (math_op != 0u) {
                    expect(TokenKind::LParen);
                    const u16 arg_reg = compile_assignment();
                    expect(TokenKind::RParen);
                    const u16 dst = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CallMath, dst, math_op, arg_reg));
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }

                u32 op2 = 0u;
                if (match_kw(mp, ml, "pow", 3)) op2 = 1u;
                else if (match_kw(mp, ml, "imul", 4)) op2 = 2u;
                if (op2 != 0u) {
                    expect(TokenKind::LParen);
                    const u16 a_reg = compile_assignment();
                    expect(TokenKind::Comma);
                    const u16 b_reg = compile_assignment();
                    expect(TokenKind::RParen);
                    const u16 dst = alloc_reg();
                    ctx->program.emit(AnvilInstruction{AnvilOpcode::CallMath2, 0u, dst, a_reg, b_reg, op2});
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }

                u32 mm = 0u;
                if (match_kw(mp, ml, "min", 3)) mm = 3u;
                else if (match_kw(mp, ml, "max", 3)) mm = 4u;
                if (mm != 0u) {
                    expect(TokenKind::LParen);
                    if (peek().kind == TokenKind::RParen) {
                        advance();
                        const u16 dst = alloc_reg();
                        emit_load_number(dst, bits_to_f64(mm == 3u ? 0x7FF0000000000000ULL
                                                                   : 0xFFF0000000000000ULL));
                        return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                    }
                    u16 acc = compile_assignment();
                    while (match(TokenKind::Comma)) {
                        if (peek().kind == TokenKind::RParen) {
                            break;
                        }
                        const u16 nxt = compile_assignment();
                        const u16 res = alloc_reg();
                        ctx->program.emit(AnvilInstruction{AnvilOpcode::CallMath2, 0u, res, acc, nxt, mm});
                        acc = res;
                    }
                    expect(TokenKind::RParen);
                    return compile_postfix(acc, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }

                reject_name(VM_REFUSE_UNSUPPORTED, "unsupported Math function", m_tok);
                const u16 dst = alloc_reg();
                emit_load_undefined(dst);
                return dst;
            }

            // Built-in `Number.<pred>(arg)`.
            if (match_kw(&src[id_tok.offset], id_tok.length, "Number", 6) &&
                !is_shadowed_name(sym) && match(TokenKind::Dot)) {
                const Token n_tok = advance();
                u8 pred_op = 0u;
                if (match_kw(&src[n_tok.offset], n_tok.length, "isInteger", 9)) pred_op = 1u;
                else if (match_kw(&src[n_tok.offset], n_tok.length, "isNaN", 5)) pred_op = 2u;
                else if (match_kw(&src[n_tok.offset], n_tok.length, "isFinite", 8)) pred_op = 3u;
                if (pred_op == 0u) {
                    reject_name(VM_REFUSE_UNSUPPORTED, "unsupported Number function", n_tok);
                    const u16 dst = alloc_reg();
                    emit_load_undefined(dst);
                    return dst;
                }
                expect(TokenKind::LParen);
                const u16 arg_reg = compile_assignment();
                expect(TokenKind::RParen);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CallPred, dst, pred_op, arg_reg));
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }

            // Error constructors called without `new`, or `String(x)` / `Number(x)` / `Boolean(x)`.
            if (!is_shadowed_name(sym) && peek().kind == TokenKind::LParen) {
                const u8 bkind = builtin_ctor_kind(&src[id_tok.offset], id_tok.length);
                if (bkind >= static_cast<u8>(BuiltinCtorKind::Error) &&
                    bkind <= static_cast<u8>(BuiltinCtorKind::ReferenceError)) {
                    expect(TokenKind::LParen);
                    u16 arg_regs[MAX_CALL_ARGS];
                    u32 argc = 0u;
                    while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                        const u16 arg_r = compile_assignment();
                        if (argc < MAX_CALL_ARGS) arg_regs[argc] = arg_r;
                        ++argc;
                        if (!match(TokenKind::Comma)) break;
                    }
                    expect(TokenKind::RParen);
                    const u16 call_base = next_reg;
                    for (u32 i = 0u; i < argc && !had_error; ++i) {
                        const u16 slot_r = alloc_reg();
                        if (arg_regs[i] != slot_r) {
                            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                        }
                    }
                    const u16 dst = alloc_reg();
                    ctx->program.emit(AnvilInstruction{
                        AnvilOpcode::NewBuiltin, bkind, dst, call_base, static_cast<u16>(argc), 0u
                    });
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }
                if (match_kw(&src[id_tok.offset], id_tok.length, "Number", 6)) {
                    expect(TokenKind::LParen);
                    const u16 dst = alloc_reg();
                    if (match(TokenKind::RParen)) {
                        emit_load_number(dst, 0.0);
                    } else {
                        const u16 arg = compile_assignment();
                        expect(TokenKind::RParen);
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, dst, arg, 0u));
                    }
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }
                if (match_kw(&src[id_tok.offset], id_tok.length, "String", 6)) {
                    expect(TokenKind::LParen);
                    const u32 empty_sym = intern_name("", 0u);
                    const u32 cidx = ctx->program.add_constant(AnvilValue::make_string(empty_sym));
                    const u16 empty_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, empty_reg, cidx));
                    if (match(TokenKind::RParen)) {
                        return compile_postfix(empty_reg, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                    }
                    const u16 arg = compile_assignment();
                    expect(TokenKind::RParen);
                    const u16 dst = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Add, dst, empty_reg, arg));
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }
                if (match_kw(&src[id_tok.offset], id_tok.length, "Boolean", 7)) {
                    expect(TokenKind::LParen);
                    const u16 dst = alloc_reg();
                    if (match(TokenKind::RParen)) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadFalse, dst, 0u, 0u));
                    } else {
                        const u16 arg = compile_assignment();
                        expect(TokenKind::RParen);
                        const u16 tmp = alloc_reg();
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Not, tmp, arg, 0u));
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Not, dst, tmp, 0u));
                    }
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }
            }

            // Function call `name(args)`.
            if (match(TokenKind::LParen)) {
                const VarRef callee_var = resolve_var(sym);
                const u32 target_fn = find_function(sym);
                const bool direct = (target_fn != NO_SLOT && callee_var.kind == VarKind::Global);

                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_assignment();
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

                if (direct) {
                    const u32 arity = ctx->heap.functions()[target_fn].arity;
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
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::Call, dst, static_cast<u16>(target_fn), call_base));
                    return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
                }

                u16 callee_reg = 0u;
                bool have_callee = false;
                if (callee_var.kind == VarKind::Local || callee_var.kind == VarKind::Cell ||
                    callee_var.kind == VarKind::Upval) {
                    callee_reg = load_var(callee_var);
                    have_callee = true;
                } else {
                    for (u32 gi = 0u; gi < ctx->global_count; ++gi) {
                        if (ctx->globals()[gi].occupied && ctx->globals()[gi].name_sym == sym &&
                            ctx->globals()[gi].declared) {
                            callee_reg = alloc_reg();
                            ctx->program.emit(AnvilInstruction::make_abx(
                                AnvilOpcode::GetGlobal, callee_reg, static_cast<u16>(gi)));
                            have_callee = true;
                            break;
                        }
                    }
                }
                if (!have_callee) {
                    reject_name(VM_REFUSE_UNKNOWN_NAME, "call to a function not declared in this script", id_tok);
                    const u16 dst = alloc_reg();
                    emit_load_undefined(dst);
                    return dst;
                }
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < argc && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (arg_regs[i] != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                    }
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction{
                    AnvilOpcode::CallValue, 0u, dst, callee_reg, call_base, argc});
                return compile_postfix(dst, VarRef{VarKind::Global, 0u, 0u, sym}, false);
            }

            const VarRef vr = resolve_var(sym);
            if (vr.kind == VarKind::Global) {
                if (match_kw(&src[id_tok.offset], id_tok.length, "undefined", 9)) {
                    const u16 dst = alloc_reg();
                    emit_load_undefined(dst);
                    return compile_postfix(dst, vr, false);
                }
                if (match_kw(&src[id_tok.offset], id_tok.length, "NaN", 3) && !is_shadowed_name(sym)) {
                    const u16 dst = alloc_reg();
                    emit_load_number(dst, bits_to_f64(0x7FF8000000000000ULL));
                    return compile_postfix(dst, vr, false);
                }
                if (match_kw(&src[id_tok.offset], id_tok.length, "Infinity", 8) && !is_shadowed_name(sym)) {
                    const u16 dst = alloc_reg();
                    emit_load_number(dst, bits_to_f64(0x7FF0000000000000ULL));
                    return compile_postfix(dst, vr, false);
                }
                const u8 bkind = builtin_ctor_kind(&src[id_tok.offset], id_tok.length);
                if (bkind != 0u && !is_shadowed_name(sym) && !is_assign_token(peek().kind)) {
                    const u16 dst = alloc_reg();
                    const u32 cidx = ctx->program.add_constant(
                        AnvilValue::make_function(0xFFFFFF00u | static_cast<u32>(bkind)));
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
                    return compile_postfix(dst, vr, false);
                }
                const u32 fn_as_value = find_function(sym);
                if (fn_as_value != NO_SLOT) {
                    if (is_assign_token(peek().kind)) {
                        reject_name(VM_REFUSE_UNSUPPORTED, "cannot reassign a declared function", id_tok);
                        return alloc_reg();
                    }
                    const u16 dst = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadFunc, dst, fn_as_value));
                    return compile_postfix(dst, vr, false);
                }
            }
            if (peek().kind == TokenKind::Assign) {
                advance();
                const u16 rhs = compile_assignment();
                store_var(vr, rhs);
                return rhs;
            }
            if (is_assign_token(peek().kind)) {
                const TokenKind ak = advance().kind;
                const u16 cur_reg = load_var(vr);
                const u16 rhs = compile_assignment();
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(assign_token_opcode(ak), dst, cur_reg, rhs));
                store_var(vr, dst);
                return dst;
            }
            const u16 base_reg = load_var(vr);
            return compile_postfix(base_reg, vr, true);
        }

        reject_syntax();
        const u16 fallback = alloc_reg();
        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, fallback, 0u, 0u));
        return fallback;
    }

    u16 compile_postfix(u16 base_reg, VarRef vr, bool is_var_lvalue = true) noexcept {
        u16 cur = base_reg;
        bool can_assign_var = is_var_lvalue;
        while (!had_error) {
            if (match(TokenKind::Dot)) {
                can_assign_var = false;
                const Token prop_tok = advance();
                if (!is_property_name_token(prop_tok.kind)) {
                    reject_syntax();
                    break;
                }
                const u32 prop_sym = token_sym(prop_tok);
                if (match(TokenKind::LParen)) {
                    u16 arg_regs[MAX_CALL_ARGS];
                    u32 argc = 0u;
                    while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                        const u16 arg_r = compile_assignment();
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
                    const u16 call_base = next_reg;
                    for (u32 i = 0u; i < argc && !had_error; ++i) {
                        const u16 slot_r = alloc_reg();
                        if (arg_regs[i] != slot_r) {
                            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                        }
                    }
                    const u16 dst = alloc_reg();
                    ctx->program.emit(AnvilInstruction{
                        AnvilOpcode::CallMethod,
                        static_cast<u8>(argc < 255u ? argc : 255u),
                        dst,
                        cur,
                        call_base,
                        prop_sym
                    });
                    cur = dst;
                    continue;
                }
                if (match(TokenKind::Assign)) {
                    const u16 rhs = compile_assignment();
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
                if (is_assign_token(peek().kind)) {
                    const TokenKind ak = advance().kind;
                    const u32 get_site = new_site(prop_sym);
                    const u16 lhs_val = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::GetPropIC, lhs_val, cur, static_cast<u16>(get_site)));
                    const u16 rhs = compile_assignment();
                    const u16 res = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(assign_token_opcode(ak), res, lhs_val, rhs));
                    const u32 set_site = new_site(prop_sym);
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::SetPropIC, cur, res, static_cast<u16>(set_site)));
                    cur = res;
                    continue;
                }
                if (peek().kind == TokenKind::PlusPlus || peek().kind == TokenKind::MinusMinus) {
                    const bool is_inc = advance().kind == TokenKind::PlusPlus;
                    const u32 get_site = new_site(prop_sym);
                    const u16 raw_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::GetPropIC, raw_reg, cur, static_cast<u16>(get_site)));
                    const u16 old_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, old_reg, raw_reg, 0u));
                    const u16 one_reg = alloc_reg();
                    emit_load_number(one_reg, 1.0);
                    const u16 new_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub, new_reg, old_reg, one_reg));
                    const u32 set_site = new_site(prop_sym);
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::SetPropIC, cur, new_reg, static_cast<u16>(set_site)));
                    cur = old_reg;
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
                can_assign_var = false;
                const u16 idx_reg = compile_expr();
                expect(TokenKind::RBracket);
                if (match(TokenKind::Assign)) {
                    const u16 rhs = compile_assignment();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetElem, cur, idx_reg, rhs));
                    cur = rhs;
                    continue;
                }
                if (is_assign_token(peek().kind)) {
                    const TokenKind ak = advance().kind;
                    const u16 lhs_val = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, lhs_val, cur, idx_reg));
                    const u16 rhs = compile_assignment();
                    const u16 res = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(assign_token_opcode(ak), res, lhs_val, rhs));
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetElem, cur, idx_reg, res));
                    cur = res;
                    continue;
                }
                if (peek().kind == TokenKind::PlusPlus || peek().kind == TokenKind::MinusMinus) {
                    const bool is_inc = advance().kind == TokenKind::PlusPlus;
                    const u16 raw_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, raw_reg, cur, idx_reg));
                    const u16 old_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, old_reg, raw_reg, 0u));
                    const u16 one_reg = alloc_reg();
                    emit_load_number(one_reg, 1.0);
                    const u16 new_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub, new_reg, old_reg, one_reg));
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetElem, cur, idx_reg, new_reg));
                    cur = old_reg;
                    continue;
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, dst, cur, idx_reg));
                cur = dst;
                continue;
            }
            if (match(TokenKind::LParen)) {
                can_assign_var = false;
                u16 arg_regs[MAX_CALL_ARGS];
                u32 argc = 0u;
                while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                    const u16 arg_r = compile_assignment();
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
                const u16 call_base = next_reg;
                for (u32 i = 0u; i < argc && !had_error; ++i) {
                    const u16 slot_r = alloc_reg();
                    if (arg_regs[i] != slot_r) {
                        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, slot_r, arg_regs[i], 0u));
                    }
                }
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction{
                    AnvilOpcode::CallValue, 0u, dst, cur, call_base, argc});
                cur = dst;
                continue;
            }
            if (peek().kind == TokenKind::PlusPlus || peek().kind == TokenKind::MinusMinus) {
                if (!can_assign_var) {
                    reject_here(VM_REFUSE_UNSUPPORTED, "++ or -- on something other than a variable");
                    break;
                }
                const bool is_inc = advance().kind == TokenKind::PlusPlus;
                const u16 old_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, old_reg, cur, 0u));
                const u16 one_reg = alloc_reg();
                emit_load_number(one_reg, 1.0);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub,
                    dst,
                    old_reg,
                    one_reg
                ));
                store_var(vr, dst);
                cur = old_reg;
                break;
            }
            break;
        }
        return cur;
    }

    u16 compile_prefix_inc_dec(bool is_inc) noexcept {
        if (peek().kind != TokenKind::Ident && peek().kind != TokenKind::KwThis) {
            reject_here(VM_REFUSE_UNSUPPORTED, "++ or -- on something other than a variable");
            return alloc_reg();
        }
        bool is_var = false;
        VarRef vr{VarKind::Global, 0u, 0u, 0u};
        u16 cur = 0u;
        if (peek().kind == TokenKind::KwThis) {
            advance();
            cur = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadThis, cur, 0u, 0u));
        } else {
            const Token id_tok = advance();
            const u32 sym = token_sym(id_tok);
            vr = resolve_var(sym);
            is_var = true;
            if (peek().kind == TokenKind::Dot || peek().kind == TokenKind::LBracket) {
                cur = load_var(vr);
                is_var = false;
            }
        }
        while (peek().kind == TokenKind::Dot || peek().kind == TokenKind::LBracket) {
            if (match(TokenKind::Dot)) {
                const Token prop_tok = advance();
                if (!is_property_name_token(prop_tok.kind)) {
                    reject_syntax();
                    return alloc_reg();
                }
                const u32 prop_sym = token_sym(prop_tok);
                if (peek().kind != TokenKind::Dot && peek().kind != TokenKind::LBracket) {
                    const u32 get_site = new_site(prop_sym);
                    const u16 raw_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::GetPropIC, raw_reg, cur, static_cast<u16>(get_site)));
                    const u16 num_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, num_reg, raw_reg, 0u));
                    const u16 one_reg = alloc_reg();
                    emit_load_number(one_reg, 1.0);
                    const u16 new_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub, new_reg, num_reg, one_reg));
                    const u32 set_site = new_site(prop_sym);
                    ctx->program.emit(AnvilInstruction::make_abc(
                        AnvilOpcode::SetPropIC, cur, new_reg, static_cast<u16>(set_site)));
                    return new_reg;
                }
                const u32 get_site = new_site(prop_sym);
                const u16 nxt = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::GetPropIC, nxt, cur, static_cast<u16>(get_site)));
                cur = nxt;
            } else if (match(TokenKind::LBracket)) {
                const u16 idx_reg = compile_expr();
                expect(TokenKind::RBracket);
                if (peek().kind != TokenKind::Dot && peek().kind != TokenKind::LBracket) {
                    const u16 raw_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, raw_reg, cur, idx_reg));
                    const u16 num_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, num_reg, raw_reg, 0u));
                    const u16 one_reg = alloc_reg();
                    emit_load_number(one_reg, 1.0);
                    const u16 new_reg = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(
                        is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub, new_reg, num_reg, one_reg));
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::SetElem, cur, idx_reg, new_reg));
                    return new_reg;
                }
                const u16 nxt = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::GetElem, nxt, cur, idx_reg));
                cur = nxt;
            }
        }
        if (!is_var) {
            reject_here(VM_REFUSE_UNSUPPORTED, "++ or -- on something other than a variable");
            return alloc_reg();
        }
        const u16 raw_reg = load_var(vr);
        const u16 num_reg = alloc_reg();
        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, num_reg, raw_reg, 0u));
        const u16 one_reg = alloc_reg();
        emit_load_number(one_reg, 1.0);
        const u16 new_reg = alloc_reg();
        ctx->program.emit(AnvilInstruction::make_abc(
            is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub, new_reg, num_reg, one_reg));
        store_var(vr, new_reg);
        return new_reg;
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
        if (match(TokenKind::Plus)) {
            const u16 operand = compile_unary();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::ToNum, dst, operand, 0u));
            return dst;
        }
        if (match(TokenKind::Bang)) {
            const u16 operand = compile_unary();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Not, dst, operand, 0u));
            return dst;
        }
        if (match(TokenKind::Tilde)) {
            const u16 operand = compile_unary();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::BitNot, dst, operand, 0u));
            return dst;
        }
        if (match(TokenKind::KwTypeof)) {
            // If operand is a bare identifier not followed by member/call, allow even if undeclared.
            if (peek().kind == TokenKind::Ident &&
                peek(1u).kind != TokenKind::Dot &&
                peek(1u).kind != TokenKind::LBracket &&
                peek(1u).kind != TokenKind::LParen) {
                const Token next_tok = peek();
                const u32 sym = token_sym(next_tok);
                if (!is_shadowed_name(sym) &&
                    !match_kw(&src[next_tok.offset], next_tok.length, "undefined", 9) &&
                    !match_kw(&src[next_tok.offset], next_tok.length, "NaN", 3) &&
                    !match_kw(&src[next_tok.offset], next_tok.length, "Infinity", 8) &&
                    builtin_ctor_kind(&src[next_tok.offset], next_tok.length) == 0u) {
                    advance();
                    const u16 dst = alloc_reg();
                    const u32 usym = intern_name("undefined", 9u);
                    const u32 cidx = ctx->program.add_constant(AnvilValue::make_string(usym));
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::LoadConst, dst, cidx));
                    return dst;
                }
            }
            const u16 operand = compile_unary();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::TypeOf, dst, operand, 0u));
            return dst;
        }
        if (match(TokenKind::KwVoid)) {
            (void) compile_unary();
            const u16 dst = alloc_reg();
            emit_load_undefined(dst);
            return dst;
        }
        if (match(TokenKind::KwDelete)) {
            const u16 operand = compile_unary();
            // If the last emitted instruction was GetPropIC or GetElem into `operand`, rewrite it to DeleteProp / DeleteElem.
            if (ctx->program.code_len > 0u) {
                AnvilInstruction& last = ctx->program.code()[ctx->program.code_len - 1u];
                if (last.op == AnvilOpcode::GetPropIC && last.a == operand) {
                    const u32 site_id = last.c;
                    const u32 prop_sym = (site_id < ctx->ic.site_count) ? ctx->ic.sites()[site_id].prop_sym : 0u;
                    last = AnvilInstruction{AnvilOpcode::DeleteProp, 0u, operand, last.b, 0u, prop_sym};
                    return operand;
                }
                if (last.op == AnvilOpcode::GetElem && last.a == operand) {
                    last = AnvilInstruction::make_abc(AnvilOpcode::DeleteElem, operand, last.b, last.c);
                    return operand;
                }
            }
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadTrue, dst, 0u, 0u));
            return dst;
        }
        if (match(TokenKind::PlusPlus)) {
            return compile_prefix_inc_dec(true);
        }
        if (match(TokenKind::MinusMinus)) {
            return compile_prefix_inc_dec(false);
        }
        return compile_primary();
    }

    u16 compile_exp() noexcept {
        const u16 lhs = compile_unary();
        if (match(TokenKind::StarStar)) {
            const u16 rhs = compile_exp(); // right-associative
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction{AnvilOpcode::CallMath2, 0u, dst, lhs, rhs, 1u});
            return dst;
        }
        return lhs;
    }

    u16 compile_mul() noexcept {
        u16 lhs = compile_exp();
        while (peek().kind == TokenKind::Star ||
               peek().kind == TokenKind::Slash ||
               peek().kind == TokenKind::Percent) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_exp();
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
               peek().kind == TokenKind::Ge ||
               peek().kind == TokenKind::KwIn ||
               peek().kind == TokenKind::KwInstanceof) {
            const TokenKind op = advance().kind;
            const u16 rhs = compile_shift();
            const u16 dst = alloc_reg();
            AnvilOpcode opc = AnvilOpcode::CmpLt;
            if (op == TokenKind::Le) opc = AnvilOpcode::CmpLe;
            else if (op == TokenKind::Gt) opc = AnvilOpcode::CmpGt;
            else if (op == TokenKind::Ge) opc = AnvilOpcode::CmpGe;
            else if (op == TokenKind::KwIn) opc = AnvilOpcode::InOp;
            else if (op == TokenKind::KwInstanceof) opc = AnvilOpcode::InstanceOf;
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

    u16 compile_bit_and() noexcept {
        u16 lhs = compile_equality();
        while (peek().kind == TokenKind::Amp) {
            advance();
            const u16 rhs = compile_equality();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::BitAnd, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_bit_xor() noexcept {
        u16 lhs = compile_bit_and();
        while (peek().kind == TokenKind::Caret) {
            advance();
            const u16 rhs = compile_bit_and();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::BitXor, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_bitwise() noexcept {
        u16 lhs = compile_bit_xor();
        while (peek().kind == TokenKind::Pipe) {
            advance();
            const u16 rhs = compile_bit_xor();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::BitOr, dst, lhs, rhs));
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_logical_and() noexcept {
        u16 lhs = compile_bitwise();
        while (peek().kind == TokenKind::AndAnd) {
            advance();
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, lhs, 0u));
            const u32 patch_pc = ctx->program.emit(AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                dst,
                0u
            ));
            const u16 rhs = compile_bitwise();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, rhs, 0u));
            ctx->program.code()[patch_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                dst,
                ctx->program.code_len
            );
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_logical() noexcept {
        u16 lhs = compile_logical_and();
        while (peek().kind == TokenKind::OrOr || peek().kind == TokenKind::Nullish) {
            const TokenKind op = advance().kind;
            if (op == TokenKind::Nullish) {
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, lhs, 0u));
                const u16 null_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LoadNull, null_reg, 0u, 0u));
                const u16 is_nullish = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CmpLooseEq, is_nullish, dst, null_reg));
                const u32 patch_pc = ctx->program.emit(AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse, is_nullish, 0u));
                const u16 rhs = compile_logical_and();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, rhs, 0u));
                ctx->program.code()[patch_pc] = AnvilInstruction::make_abx(
                    AnvilOpcode::JumpIfFalse, is_nullish, ctx->program.code_len);
                lhs = dst;
                continue;
            }
            const u16 dst = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, lhs, 0u));
            const u32 patch_pc = ctx->program.emit(AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfTrue,
                dst,
                0u
            ));
            const u16 rhs = compile_logical_and();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, rhs, 0u));
            ctx->program.code()[patch_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfTrue,
                dst,
                ctx->program.code_len
            );
            lhs = dst;
        }
        return lhs;
    }

    u16 compile_assignment() noexcept {
        const u16 cond_or_val = compile_logical();
        if (match(TokenKind::Question)) {
            const u16 dst = alloc_reg();
            const u32 jfalse_pc = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_or_val, 0u)
            );
            const u16 then_reg = compile_assignment();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, dst, then_reg, 0u));
            const u32 jend_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
            expect(TokenKind::Colon);
            ctx->program.code()[jfalse_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::JumpIfFalse,
                cond_or_val,
                ctx->program.code_len
            );
            const u16 else_reg = compile_assignment();
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

    u16 compile_expr() noexcept {
        u16 res = compile_assignment();
        while (match(TokenKind::Comma)) {
            res = compile_assignment();
        }
        return res;
    }

    u16 compile_statement() noexcept {
        const u16 result = compile_statement_body();
        if (next_reg > reg_floor) {
            next_reg = reg_floor;
        }
        return result;
    }

    u16 compile_var_decl_list(bool is_var, bool no_semi = false) noexcept {
        u16 last_reg = 0u;
        while (!had_error) {
            const Token id_tok = advance();
            if (id_tok.kind != TokenKind::Ident) {
                reject_syntax();
                return 0u;
            }
            const u32 sym = token_sym(id_tok);
            if (!in_function && find_function(sym) != NO_SLOT) {
                reject_name(VM_REFUSE_UNSUPPORTED, "name used for a variable and a function", id_tok);
                return 0u;
            }
            if (!in_function) {
                if (fn_uses_cells && block_depth > 0u && !is_var) {
                    const u16 dst = declare_local(sym, false);
                    if (match(TokenKind::Assign)) {
                        const u16 v = compile_assignment();
                        reject_if_expression_continues();
                        init_local(dst, true, v);
                    } else {
                        const u16 u = alloc_reg();
                        emit_load_undefined(u);
                        init_local(dst, true, u);
                    }
                    last_reg = dst;
                } else {
                    u16 val_reg = 0u;
                    if (match(TokenKind::Assign)) {
                        val_reg = compile_assignment();
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
                        if (block_depth == 0u || is_var) {
                            gb.exported = true;
                        }
                    }
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, val_reg, g_slot));
                    last_reg = val_reg;
                }
            } else {
                const u16 dst = declare_local(sym, is_var);
                const bool is_cell = fn_uses_cells;
                if (match(TokenKind::Assign)) {
                    const u16 val_reg = compile_assignment();
                    reject_if_expression_continues();
                    init_local(dst, is_cell, val_reg);
                } else {
                    const u16 u = alloc_reg();
                    emit_load_undefined(u);
                    init_local(dst, is_cell, u);
                }
                last_reg = dst;
            }
            if (!match(TokenKind::Comma)) {
                break;
            }
        }
        if (!no_semi) {
            match(TokenKind::Semicolon);
        }
        return last_reg;
    }

    u16 compile_try_stmt() noexcept {
        const u16 exc_reg = alloc_reg();
        const u16 saved_floor = reg_floor;
        if (exc_reg + 1u > reg_floor) {
            reg_floor = static_cast<u16>(exc_reg + 1u);
        }
        const u32 enter_pc = ctx->program.emit(
            AnvilInstruction::make_abx(AnvilOpcode::EnterTry, exc_reg, 0u));
        const u32 set_fin_pc = ctx->program.emit(
            AnvilInstruction::make_abx(AnvilOpcode::SetTryFinally, 0u, 0u));

        if (peek().kind != TokenKind::LBrace) {
            reject_syntax();
            reg_floor = saved_floor;
            return 0u;
        }
        compile_statement(); // try block
        ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LeaveTry, 0u, 0u, 0u));
        const u32 j_after_try = ctx->program.emit(
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));

        u32 catch_pc = 0u;
        u32 j_after_catch = NO_SLOT;
        if (match(TokenKind::KwCatch)) {
            catch_pc = ctx->program.code_len;
            const BlockScope catch_scope(*this);
            if (match(TokenKind::LParen)) {
                const Token p_tok = advance();
                if (p_tok.kind != TokenKind::Ident) {
                    reject_syntax();
                    reg_floor = saved_floor;
                    return 0u;
                }
                const u32 p_sym = token_sym(p_tok);
                expect(TokenKind::RParen);
                if (!in_function && !(fn_uses_cells && block_depth > 0u)) {
                    const u16 g_slot = global_slot(p_sym);
                    if (g_slot < ctx->global_count) {
                        ctx->globals()[g_slot].declared = true;
                    }
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, exc_reg, g_slot));
                } else {
                    const u16 p_reg = declare_local(p_sym, false);
                    init_local(p_reg, fn_uses_cells, exc_reg);
                }
            }
            if (peek().kind != TokenKind::LBrace) {
                reject_syntax();
                reg_floor = saved_floor;
                return 0u;
            }
            compile_statement(); // catch block
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::LeaveTry, 0u, 0u, 0u));
            j_after_catch = ctx->program.emit(
                AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
        }

        u32 finally_pc = 0u;
        if (match(TokenKind::KwFinally)) {
            finally_pc = ctx->program.code_len;
            if (peek().kind != TokenKind::LBrace) {
                reject_syntax();
                reg_floor = saved_floor;
                return 0u;
            }
            compile_statement(); // finally block
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::EndFinally, 0u, 0u, 0u));
        }

        if (catch_pc == 0u && finally_pc == 0u) {
            reject_syntax();
            reg_floor = saved_floor;
            return 0u;
        }

        ctx->program.code()[enter_pc] =
            AnvilInstruction::make_abx(AnvilOpcode::EnterTry, exc_reg, catch_pc);
        if (finally_pc != 0u) {
            ctx->program.code()[set_fin_pc] =
                AnvilInstruction::make_abx(AnvilOpcode::SetTryFinally, 0u, finally_pc);
        } else {
            ctx->program.code()[set_fin_pc] = AnvilInstruction::make_abc(AnvilOpcode::Nop, 0u, 0u, 0u);
        }

        const u32 normal_target = (finally_pc != 0u) ? finally_pc : ctx->program.code_len;
        ctx->program.code()[j_after_try] =
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, normal_target);
        if (j_after_catch != NO_SLOT) {
            ctx->program.code()[j_after_catch] =
                AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, normal_target);
        }

        reg_floor = saved_floor;
        return 0u;
    }

    u16 compile_switch_stmt() noexcept {
        expect(TokenKind::LParen);
        const u16 disc_reg = compile_expr();
        expect(TokenKind::RParen);
        expect(TokenKind::LBrace);

        const u16 saved_floor = reg_floor;
        if (disc_reg + 1u > reg_floor) {
            reg_floor = static_cast<u16>(disc_reg + 1u);
        }

        const BlockScope switch_scope(*this);
        if (!begin_loop(true)) {
            reg_floor = saved_floor;
            return 0u;
        }

        const u32 j_dispatch = ctx->program.emit(
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));

        u32 case_cursors[128];
        u32 case_body_pcs[128];
        u32 case_count = 0u;
        u32 default_body_pc = NO_SLOT;

        while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
            if (match(TokenKind::KwCase)) {
                if (case_count >= 128u) {
                    reject_limit(VM_REFUSE_LOOPS, "switch case", 128u, "cases");
                    break;
                }
                case_cursors[case_count] = cursor;
                u32 paren_d = 0u;
                u32 q_d = 0u;
                while (peek().kind != TokenKind::Eof) {
                    const TokenKind ck = peek().kind;
                    if (ck == TokenKind::LParen || ck == TokenKind::LBracket || ck == TokenKind::LBrace) {
                        ++paren_d;
                    } else if (ck == TokenKind::RParen || ck == TokenKind::RBracket || ck == TokenKind::RBrace) {
                        if (paren_d > 0u) --paren_d;
                    } else if (paren_d == 0u && ck == TokenKind::Question) {
                        ++q_d;
                    } else if (paren_d == 0u && ck == TokenKind::Colon) {
                        if (q_d > 0u) {
                            --q_d;
                        } else {
                            advance();
                            break;
                        }
                    }
                    advance();
                }
                case_body_pcs[case_count++] = ctx->program.code_len;
            } else if (match(TokenKind::KwDefault)) {
                expect(TokenKind::Colon);
                default_body_pc = ctx->program.code_len;
            } else {
                compile_statement();
            }
        }
        expect(TokenKind::RBrace);

        const u32 j_after_bodies = ctx->program.emit(
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
        ctx->program.code()[j_dispatch] =
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, ctx->program.code_len);

        const u32 saved_cursor = cursor;
        for (u32 i = 0u; i < case_count && !had_error; ++i) {
            cursor = case_cursors[i];
            const u16 cval_reg = compile_expr();
            expect(TokenKind::Colon);
            const u16 cmp_reg = alloc_reg();
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CmpEq, cmp_reg, disc_reg, cval_reg));
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::JumpIfTrue, cmp_reg, case_body_pcs[i]));
            if (next_reg > reg_floor) {
                next_reg = reg_floor;
            }
        }
        cursor = saved_cursor;

        if (default_body_pc != NO_SLOT) {
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, default_body_pc));
        }
        ctx->program.code()[j_after_bodies] =
            AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, ctx->program.code_len);

        end_loop(0u);
        reg_floor = saved_floor;
        return 0u;
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

        if (match(TokenKind::KwExport)) {
            if (in_function || block_depth > 0u) {
                reject_here(VM_REFUSE_SYNTAX, "export inside a block or function");
                return 0u;
            }
            const TokenKind nk = peek().kind;
            if (nk != TokenKind::KwLet && nk != TokenKind::KwConst && nk != TokenKind::KwVar &&
                nk != TokenKind::KwFunction && nk != TokenKind::KwClass) {
                reject_here(VM_REFUSE_UNSUPPORTED, "unsupported export");
                return 0u;
            }
        }

        if (match(TokenKind::LBrace)) {
            const BlockScope scope(*this);
            u16 last_reg = 0u;
            while (peek().kind != TokenKind::RBrace && peek().kind != TokenKind::Eof && !had_error) {
                last_reg = compile_statement();
            }
            expect(TokenKind::RBrace);
            return last_reg;
        }

        if (match(TokenKind::KwLet) || match(TokenKind::KwConst) || match(TokenKind::KwVar)) {
            const bool is_var = lexer.tokens()[cursor - 1u].kind == TokenKind::KwVar;
            return compile_var_decl_list(is_var, false);
        }

        if (match(TokenKind::KwClass)) {
            const Token cls_tok = advance();
            if (cls_tok.kind != TokenKind::Ident) {
                reject_syntax();
                return 0u;
            }
            const u32 cls_sym = token_sym(cls_tok);
            if (!in_function) {
                const u16 g_slot = global_slot(cls_sym);
                if (!had_error && g_slot < ctx->global_count) {
                    AnvilGlobalBinding& gb = ctx->globals()[g_slot];
                    gb.decl_depth = block_depth;
                    gb.declared = true;
                    if (block_depth == 0u) {
                        gb.exported = true;
                    }
                }
                const u16 ctor_reg = compile_class_literal(cls_sym);
                ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, ctor_reg, g_slot));
                return ctor_reg;
            } else {
                const u16 dst = declare_local(cls_sym, false);
                const VarRef self = resolve_var(cls_sym);
                const u16 u = alloc_reg();
                emit_load_undefined(u);
                init_local(dst, self.kind == VarKind::Cell, u);
                const u16 ctor_reg = compile_class_literal(cls_sym);
                store_var(self, ctor_reg);
                return dst;
            }
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

        if (match(TokenKind::KwThrow)) {
            const u16 val_reg = compile_expr();
            reject_if_expression_continues();
            match(TokenKind::Semicolon);
            ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Throw, val_reg, 0u, 0u));
            return val_reg;
        }

        if (match(TokenKind::KwTry)) {
            return compile_try_stmt();
        }

        if (match(TokenKind::KwSwitch)) {
            return compile_switch_stmt();
        }

        if (match(TokenKind::KwIf)) {
            expect(TokenKind::LParen);
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

        // Labeled loop or block `label: stmt`.
        if (peek(0u).kind == TokenKind::Ident && peek(1u).kind == TokenKind::Colon) {
            const Token lbl_tok = advance();
            advance(); // colon
            pending_label_sym = token_sym(lbl_tok);
            return compile_statement();
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
            const BlockScope scope(*this);
            expect(TokenKind::LParen);

            // Check for `for (const/let/var x of iterable)`.
            if ((peek(0u).kind == TokenKind::KwLet || peek(0u).kind == TokenKind::KwConst ||
                 peek(0u).kind == TokenKind::KwVar) &&
                peek(1u).kind == TokenKind::Ident &&
                peek(2u).kind == TokenKind::Ident &&
                match_kw(&src[peek(2u).offset], peek(2u).length, "of", 2)) {
                const bool is_var = advance().kind == TokenKind::KwVar;
                const Token id_tok = advance();
                advance(); // `of`
                const u32 var_sym = token_sym(id_tok);
                const u16 iter_expr_reg = compile_expr();
                expect(TokenKind::RParen);

                const u16 saved_floor = reg_floor;
                const u16 iter_reg = alloc_reg();
                if (iter_reg != iter_expr_reg) {
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Move, iter_reg, iter_expr_reg, 0u));
                }
                const u16 idx_reg = alloc_reg();
                emit_load_number(idx_reg, 0.0);
                const u16 len_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::IterLen, len_reg, iter_reg, 0u));
                if (len_reg + 1u > reg_floor) {
                    reg_floor = static_cast<u16>(len_reg + 1u);
                }

                u16 local_dst = 0u;
                u16 g_slot = 0u;
                bool use_global = false;
                if (!in_function && !(fn_uses_cells && block_depth > 0u && !is_var)) {
                    use_global = true;
                    g_slot = global_slot(var_sym);
                    if (g_slot < ctx->global_count) {
                        ctx->globals()[g_slot].declared = true;
                        ctx->globals()[g_slot].decl_depth = block_depth;
                    }
                } else {
                    local_dst = declare_local(var_sym, is_var);
                }

                const u32 loop_id = (next_loop_id < 63u) ? next_loop_id++ : 63u;
                const u32 cond_pc = ctx->program.code_len;
                ctx->program.emit(AnvilInstruction::make_abc(
                    AnvilOpcode::LoopHeader, static_cast<u8>(loop_id), 0u, 0u));
                const u16 cond_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CmpLt, cond_reg, idx_reg, len_reg));
                const u32 jexit_pc = ctx->program.emit(
                    AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, 0u));

                const u16 elem_reg = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::IterGet, elem_reg, iter_reg, idx_reg));
                if (use_global) {
                    ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::SetGlobal, elem_reg, g_slot));
                } else {
                    init_local(local_dst, fn_uses_cells, elem_reg);
                }

                if (!begin_loop()) {
                    reg_floor = saved_floor;
                    return 0u;
                }
                const u16 body_reg = compile_statement();
                const u32 step_pc = ctx->program.code_len;
                const u16 one_reg = alloc_reg();
                emit_load_number(one_reg, 1.0);
                ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::Add, idx_reg, idx_reg, one_reg));
                ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, cond_pc));

                ctx->program.code()[jexit_pc] =
                    AnvilInstruction::make_abx(AnvilOpcode::JumpIfFalse, cond_reg, ctx->program.code_len);
                end_loop(step_pc);
                reg_floor = saved_floor;
                return body_reg;
            }

            const u32 loop_locals_start = local_count;
            if (!match(TokenKind::Semicolon)) {
                compile_statement();
            }
            const u32 loop_locals_end = local_count;
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
            for (u32 i = loop_locals_start; i < loop_locals_end; ++i) {
                if (locals()[i].is_cell) {
                    const u16 reg = locals()[i].reg;
                    const u16 tmp = alloc_reg();
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::CellGet, tmp, reg, 0u));
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewCell, reg, tmp, 0u));
                }
            }
            if (peek().kind != TokenKind::RParen) {
                compile_statement_or_assign_no_semi();
            }
            expect(TokenKind::RParen);
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, cond_pc));

            ctx->program.code()[jbody_pc] = AnvilInstruction::make_abx(
                AnvilOpcode::Jump,
                0u,
                ctx->program.code_len
            );
            if (!begin_loop()) {
                return 0u;
            }
            const u16 body_reg = compile_statement();
            ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, step_pc));

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

            if (in_function) {
                const u16 dst = declare_local(fn_sym, true);
                const VarRef self = resolve_var(fn_sym);
                const u16 u = alloc_reg();
                emit_load_undefined(u);
                init_local(dst, self.kind == VarKind::Cell, u);
                const u16 val = compile_function_literal(false, true, NO_SLOT, fn_sym, false);
                store_var(self, val);
                return 0u;
            }

            const u32 fn_id = find_function(fn_sym);
            if (fn_id == NO_SLOT) {
                reject_name(VM_REFUSE_UNSUPPORTED,
                            "function declared inside a block", fn_tok);
                return 0u;
            }
            expect(TokenKind::LParen);

            const u32 saved_local_count = local_count;
            const u32 saved_local_floor = local_floor;
            const u16 saved_next_reg = next_reg;
            const u16 saved_reg_floor = reg_floor;
            const u16 saved_high_reg = high_reg;
            const bool saved_in_fn = in_function;
            const bool saved_in_derived = in_derived_ctor;
            const bool saved_uses_cells = fn_uses_cells;
            const u32 saved_loop_depth = loop_depth;
            const u32 saved_block_depth = block_depth;
            FnScope fs;
            fs.parent = cur_fn;
            fs.local_floor = local_count;
            fs.upval_count = 0u;
            FnScope* saved_cur_fn = cur_fn;
            cur_fn = &fs;

            local_floor = local_count;
            next_reg = 0u;
            reg_floor = 0u;
            high_reg = 0u;
            loop_depth = 0u;
            block_depth = 0u;
            in_function = true;
            in_derived_ctor = false;
            fn_uses_cells = false;

            u32 arity = 0u;
            while (peek().kind != TokenKind::RParen && peek().kind != TokenKind::Eof && !had_error) {
                const Token p_tok = advance();
                if (p_tok.kind != TokenKind::Ident) {
                    reject_syntax();
                    break;
                }
                declare_local(token_sym(p_tok), true);
                ++arity;
                if (!match(TokenKind::Comma)) {
                    break;
                }
            }
            expect(TokenKind::RParen);

            fn_uses_cells = body_has_nested_function();

            const u32 jskip_pc = ctx->program.emit(AnvilInstruction::make_abx(AnvilOpcode::Jump, 0u, 0u));
            const u32 fn_start_pc = ctx->program.code_len;
            if (fn_id < ctx->heap.function_count) {
                ctx->heap.functions()[fn_id].code_start = fn_start_pc;
            }

            if (fn_uses_cells) {
                for (u32 i = local_floor; i < local_count; ++i) {
                    locals()[i].is_cell = true;
                    const u16 preg = locals()[i].reg;
                    ctx->program.emit(AnvilInstruction::make_abc(AnvilOpcode::NewCell, preg, preg, 0u));
                }
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

            local_count = saved_local_count;
            local_floor = saved_local_floor;
            next_reg = saved_next_reg;
            reg_floor = saved_reg_floor;
            high_reg = saved_high_reg;
            loop_depth = saved_loop_depth;
            block_depth = saved_block_depth;
            in_function = saved_in_fn;
            in_derived_ctor = saved_in_derived;
            fn_uses_cells = saved_uses_cells;
            cur_fn = saved_cur_fn;
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
            if ((second == TokenKind::PlusPlus || second == TokenKind::MinusMinus) &&
                (third == TokenKind::Semicolon || third == TokenKind::RParen ||
                 third == TokenKind::RBrace || third == TokenKind::Eof)) {
                const Token id_tok = advance();
                const bool is_inc = advance().kind == TokenKind::PlusPlus;
                const u32 sym = token_sym(id_tok);
                const VarRef vr = resolve_var(sym);
                const u16 cur_reg = load_var(vr);
                const u16 one_reg = alloc_reg();
                emit_load_number(one_reg, 1.0);
                const u16 dst = alloc_reg();
                ctx->program.emit(AnvilInstruction::make_abc(
                    is_inc ? AnvilOpcode::Add : AnvilOpcode::Sub,
                    dst,
                    cur_reg,
                    one_reg
                ));
                store_var(vr, dst);
                return vr.kind == VarKind::Global ? cur_reg : dst;
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
    auto& g_compiler = *static_cast<AnvilCompiler*>(compiler_memory);
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
    g_compiler.pending_label_sym = NO_SLOT;
    g_compiler.in_function = false;
    g_compiler.in_derived_ctor = false;
    g_compiler.fn_uses_cells = false;
    g_compiler.root_fn.parent = nullptr;
    g_compiler.root_fn.local_floor = 0u;
    g_compiler.root_fn.upval_count = 0u;
    g_compiler.cur_fn = &g_compiler.root_fn;
    g_compiler.had_error = false;
    g_compiler.block_depth = 0u;
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

    for (u32 ti = 0u; ti < g_compiler.lexer.count; ++ti) {
        const TokenKind tk = g_compiler.lexer.tokens()[ti].kind;
        if (tk == TokenKind::KwFunction || tk == TokenKind::FatArrow || tk == TokenKind::KwClass) {
            g_compiler.fn_uses_cells = true;
            break;
        }
    }

    u16 last_reg = 0u;
    while (g_compiler.peek().kind != TokenKind::Eof && !g_compiler.had_error) {
        if (g_compiler.peek().kind == TokenKind::RBrace) {
            g_compiler.reject_here(VM_REFUSE_SYNTAX, "unexpected");
            break;
        }
        last_reg = g_compiler.compile_statement();
    }
    ctx.program.top_frame_regs = g_compiler.high_reg;
    ctx.program.emit(AnvilInstruction::make_abc(AnvilOpcode::Halt, last_reg, 0u, 0u));
    if (!ctx.program.compile_ok) {
        if (ctx.program.overflow == VM_REFUSE_CONSTANTS) {
            ctx.fail_limit(VM_REFUSE_HEAP, "heap", ctx.heap.arena.budget, "bytes");
        } else {
            ctx.fail_limit(VM_REFUSE_CODE_SIZE, "code size", ANVIL_MAX_PROGRAM_INSTRUCTIONS, "instructions");
        }
    }
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
