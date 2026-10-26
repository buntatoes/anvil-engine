// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Syntax check for selector lists.
//!
//! The cascade only matches a small part of the selector grammar, but the
//! rule about invalid selectors is about the whole grammar: one selector that
//! does not parse drops the whole rule, while a selector that parses but that
//! this crate cannot match yet simply never matches. This module answers the
//! first question only. It knows combinators, attribute selectors, and the
//! pseudo-class and pseudo-element names from the Selectors and pseudo-element
//! specifications. It does not build anything.

/// Pseudo-classes written without arguments.
const PSEUDO_CLASSES: &[&str] = &[
    "active",
    "any-link",
    "autofill",
    "blank",
    "buffering",
    "checked",
    "closed",
    "current",
    "default",
    "defined",
    "disabled",
    "empty",
    "enabled",
    "first",
    "first-child",
    "first-of-type",
    "focus",
    "focus-visible",
    "focus-within",
    "fullscreen",
    "future",
    "host",
    "hover",
    "in-range",
    "indeterminate",
    "invalid",
    "last-child",
    "last-of-type",
    "left",
    "link",
    "local-link",
    "modal",
    "muted",
    "only-child",
    "only-of-type",
    "open",
    "optional",
    "out-of-range",
    "past",
    "paused",
    "picture-in-picture",
    "placeholder-shown",
    "playing",
    "popover-open",
    "read-only",
    "read-write",
    "required",
    "right",
    "root",
    "scope",
    "seeking",
    "stalled",
    "target",
    "target-within",
    "user-invalid",
    "user-valid",
    "valid",
    "visited",
    "volume-locked",
];

/// Pseudo-classes that take an argument in parentheses.
const PSEUDO_CLASS_FUNCTIONS: &[&str] = &[
    "current",
    "dir",
    "has",
    "host",
    "host-context",
    "is",
    "lang",
    "not",
    "nth-child",
    "nth-last-child",
    "nth-last-of-type",
    "nth-of-type",
    "state",
    "where",
];

/// Pseudo-elements written after `::` without arguments.
const PSEUDO_ELEMENTS: &[&str] = &[
    "after",
    "backdrop",
    "before",
    "cue",
    "cue-region",
    "details-content",
    "file-selector-button",
    "first-letter",
    "first-line",
    "grammar-error",
    "marker",
    "placeholder",
    "selection",
    "spelling-error",
    "target-text",
];

/// Pseudo-elements written after `::` with an argument.
const PSEUDO_ELEMENT_FUNCTIONS: &[&str] = &["cue", "cue-region", "highlight", "part", "slotted"];

/// Pseudo-elements that may also be written with a single colon.
const LEGACY_PSEUDO_ELEMENTS: &[&str] = &["after", "before", "first-letter", "first-line"];

/// True when `list` is a valid comma-separated selector list.
///
/// An empty list, an empty entry (`a, , b` or a trailing comma), a stray
/// character, an unknown pseudo-class or pseudo-element, or unbalanced
/// brackets make the whole list invalid.
pub(crate) fn is_valid_selector_list(list: &str) -> bool {
    split_selector_list(list)
        .iter()
        .all(|part| is_valid_complex(part, false))
}

/// Split `list` on commas that are outside parentheses, brackets, and quotes.
/// The pieces are borrowed and untrimmed. An empty `list` gives one empty piece.
pub(crate) fn split_selector_list(list: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut quote = None;
    let mut start = 0;
    let mut chars = list.char_indices();
    while let Some((idx, ch)) = chars.next() {
        if let Some(q) = quote {
            if ch == '\\' {
                chars.next();
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\\' => {
                chars.next();
            }
            '"' | '\'' => quote = Some(ch),
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&list[start..idx]);
                start = idx + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    parts
}

/// Character scanner over one selector.
struct Scanner {
    chars: Vec<char>,
    pos: usize,
}

impl Scanner {
    fn new(src: &str) -> Self {
        Scanner {
            chars: src.chars().collect(),
            pos: 0,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn at_end(&self) -> bool {
        self.pos >= self.chars.len()
    }

    fn skip_whitespace(&mut self) -> bool {
        let start = self.pos;
        while self.peek().is_some_and(is_css_whitespace) {
            self.pos += 1;
        }
        self.pos > start
    }

    /// True at `\` followed by something other than a newline.
    fn at_escape(&self, offset: usize) -> bool {
        self.peek_at(offset) == Some('\\') && self.peek_at(offset + 1).is_some_and(|c| c != '\n')
    }

    /// Consume one escape. The scanner is on the backslash.
    fn eat_escape(&mut self) {
        self.pos += 1;
        let mut hex = 0;
        while hex < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
            self.pos += 1;
            hex += 1;
        }
        // Without hex digits the escaped character itself is taken. After
        // hex digits, one whitespace character belongs to the escape.
        if hex == 0 || self.peek().is_some_and(is_css_whitespace) {
            self.pos += 1;
        }
    }

    /// True when an identifier starts here.
    fn at_ident_start(&self) -> bool {
        match self.peek() {
            Some('-') => match self.peek_at(1) {
                Some(c) if is_name_start(c) || c == '-' => true,
                _ => self.at_escape(1),
            },
            Some(c) if is_name_start(c) => true,
            _ => self.at_escape(0),
        }
    }

    /// Consume an identifier and return it lowercased, without escapes
    /// decoded. `None` when no identifier starts here.
    fn eat_ident(&mut self) -> Option<String> {
        if !self.at_ident_start() {
            return None;
        }
        let mut out = String::new();
        loop {
            match self.peek() {
                Some('\\') if self.at_escape(0) => {
                    out.push('\\');
                    self.eat_escape();
                }
                Some(c) if is_name_char(c) => {
                    out.push(c.to_ascii_lowercase());
                    self.pos += 1;
                }
                _ => break,
            }
        }
        Some(out)
    }

    /// Consume a quoted string. The scanner is on the quote. False when the
    /// string runs to the end or into a raw newline.
    fn eat_string(&mut self) -> bool {
        let Some(quote) = self.peek() else {
            return false;
        };
        self.pos += 1;
        while let Some(c) = self.peek() {
            self.pos += 1;
            if c == quote {
                return true;
            }
            if c == '\n' {
                return false;
            }
            if c == '\\' {
                self.pos += 1;
            }
        }
        false
    }

    /// Consume a parenthesized argument and return its text. The scanner is
    /// just past `(`. `None` when it never closes.
    fn eat_argument(&mut self) -> Option<String> {
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            match c {
                '"' | '\'' => {
                    if !self.eat_string() {
                        return None;
                    }
                    continue;
                }
                '\\' => self.pos += 1,
                '(' | '[' => depth += 1,
                ')' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        let text: String = self.chars[start..self.pos].iter().collect();
                        self.pos += 1;
                        return Some(text);
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        None
    }
}

fn is_css_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || !c.is_ascii()
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == '-'
}

/// One complex selector: compounds joined by whitespace, `>`, `+`, or `~`.
/// With `relative`, a leading combinator is allowed, as inside `:has()`.
fn is_valid_complex(src: &str, relative: bool) -> bool {
    let mut scan = Scanner::new(src);
    scan.skip_whitespace();
    if scan.at_end() {
        return false;
    }
    if relative && matches!(scan.peek(), Some('>' | '+' | '~')) {
        scan.pos += 1;
        scan.skip_whitespace();
    }
    loop {
        if !eat_compound(&mut scan) {
            return false;
        }
        let spaced = scan.skip_whitespace();
        if scan.at_end() {
            return true;
        }
        if matches!(scan.peek(), Some('>' | '+' | '~')) {
            scan.pos += 1;
            scan.skip_whitespace();
        } else if !spaced {
            return false;
        }
    }
}

/// Consume one compound selector. False when nothing valid was there.
fn eat_compound(scan: &mut Scanner) -> bool {
    let mut any = false;
    // Optional type or universal, with an optional namespace prefix.
    if scan.peek() == Some('*') || scan.at_ident_start() || scan.peek() == Some('|') {
        if scan.peek() == Some('*') {
            scan.pos += 1;
        } else if scan.peek() != Some('|') {
            scan.eat_ident();
        }
        if scan.peek() == Some('|') && scan.peek_at(1) != Some('=') {
            scan.pos += 1;
            if scan.peek() == Some('*') {
                scan.pos += 1;
            } else if scan.eat_ident().is_none() {
                return false;
            }
        }
        any = true;
    }
    let mut after_pseudo_element = false;
    loop {
        match scan.peek() {
            Some('#') => {
                scan.pos += 1;
                if after_pseudo_element || scan.eat_ident().is_none() {
                    return false;
                }
            }
            Some('.') => {
                scan.pos += 1;
                if after_pseudo_element || scan.eat_ident().is_none() {
                    return false;
                }
            }
            Some('[') => {
                scan.pos += 1;
                if after_pseudo_element || !eat_attribute(scan) {
                    return false;
                }
            }
            Some(':') => {
                scan.pos += 1;
                let element = scan.peek() == Some(':');
                if element {
                    scan.pos += 1;
                }
                let Some(name) = scan.eat_ident() else {
                    return false;
                };
                let function = scan.peek() == Some('(');
                if function {
                    scan.pos += 1;
                    let Some(arg) = scan.eat_argument() else {
                        return false;
                    };
                    if !is_valid_pseudo_function(&name, element, &arg) {
                        return false;
                    }
                } else if element || LEGACY_PSEUDO_ELEMENTS.contains(&name.as_str()) {
                    if !(PSEUDO_ELEMENTS.contains(&name.as_str())
                        || (!element && LEGACY_PSEUDO_ELEMENTS.contains(&name.as_str())))
                    {
                        return false;
                    }
                } else if !PSEUDO_CLASSES.contains(&name.as_str()) {
                    return false;
                }
                if element || LEGACY_PSEUDO_ELEMENTS.contains(&name.as_str()) {
                    after_pseudo_element = true;
                }
            }
            _ => break,
        }
        any = true;
    }
    any
}

/// Consume the inside of `[...]`. The scanner is just past `[`.
fn eat_attribute(scan: &mut Scanner) -> bool {
    scan.skip_whitespace();
    if scan.peek() == Some('*') && scan.peek_at(1) == Some('|') {
        scan.pos += 2;
    } else if scan.peek() == Some('|') {
        scan.pos += 1;
    }
    if scan.eat_ident().is_none() {
        return false;
    }
    if scan.peek() == Some('|') && scan.peek_at(1) != Some('=') {
        scan.pos += 1;
        if scan.eat_ident().is_none() {
            return false;
        }
    }
    scan.skip_whitespace();
    if scan.peek() == Some(']') {
        scan.pos += 1;
        return true;
    }
    match (scan.peek(), scan.peek_at(1)) {
        (Some('='), _) => scan.pos += 1,
        (Some('~' | '|' | '^' | '$' | '*'), Some('=')) => scan.pos += 2,
        _ => return false,
    }
    scan.skip_whitespace();
    match scan.peek() {
        Some('"' | '\'') => {
            if !scan.eat_string() {
                return false;
            }
        }
        _ => {
            if scan.eat_ident().is_none() {
                return false;
            }
        }
    }
    scan.skip_whitespace();
    if let Some(flag) = scan.peek() {
        if matches!(flag, 'i' | 'I' | 's' | 'S')
            && scan
                .peek_at(1)
                .is_some_and(|c| c == ']' || is_css_whitespace(c))
        {
            scan.pos += 1;
            scan.skip_whitespace();
        }
    }
    if scan.peek() == Some(']') {
        scan.pos += 1;
        true
    } else {
        false
    }
}

/// Check `name(arg)` after `:` or, with `element`, after `::`.
fn is_valid_pseudo_function(name: &str, element: bool, arg: &str) -> bool {
    if element {
        return PSEUDO_ELEMENT_FUNCTIONS.contains(&name) && !arg.trim().is_empty();
    }
    if !PSEUDO_CLASS_FUNCTIONS.contains(&name) {
        return false;
    }
    match name {
        // Forgiving lists: a bad entry is ignored rather than fatal.
        "is" | "where" => true,
        "not" => is_valid_selector_list(arg),
        "has" => split_selector_list(arg)
            .iter()
            .all(|part| is_valid_complex(part, true)),
        _ => !arg.trim().is_empty(),
    }
}
