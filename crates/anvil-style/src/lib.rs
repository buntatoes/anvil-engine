// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Style sheets, selector matching, and the cascade.
//!
//! Second stage of the pipeline. anvil-dom has already built the node list.
//! This crate borrows that [`anvil_dom::Document`], parses CSS into an owned [`Stylesheet`],
//! and writes one [`ComputedStyle`] per reachable node. anvil-layout reads the
//! [`StyleMap`] and places boxes. This crate does not lay out or paint.
//!
//! `width`, `height`, `max-width`, and the offset properties keep `px`, `%`,
//! `em`, `rem`, `vw`, `vh`, `vmin`, `vmax`, and `ch`. Any other unit becomes
//! [`Size::Auto`]. `font-size` accepts `px`, `%`, `em`, and `rem` and resolves
//! them here: `%` and `em` use the parent font size, and `rem` uses the
//! `html` font size (16px until that element sets one). Margin, padding,
//! border, and `gap` still accept only `px` and `0`. `calc()` does not mix
//! viewport units or `ch`. `display` accepts `block`, `inline`, `inline-block`,
//! `list-item`, `flow-root`, `flex`, `inline-flex`, `grid`, `inline-grid`,
//! and `none`. A descendant selector is whitespace between compounds. `>`,
//! `+`, `~`, attribute selectors, and pseudo-classes are checked for syntax
//! but never match yet. A rule whose selector list holds an invalid selector
//! is dropped whole.
//!
//! `transform` keeps up to [`MAX_TRANSFORM_FNS`] 2D functions:
//! `translate()`, `translateX()`, `translateY()`, `scale()`, `scaleX()`,
//! `scaleY()`, `rotate()`, `skew()`, `skewX()`, `skewY()`, and `matrix()`.
//! Angles take `deg`, `rad`, `grad`, `turn`, or `0`. 3D functions,
//! `calc()` inside a function, and the standalone `translate`, `scale`,
//! and `rotate` properties are not parsed; a list holding any of them is
//! ignored. `transform-origin` takes one or two keywords, lengths, or
//! percentages; a third (z) length is accepted and ignored.
//!
//! `opacity` takes a number or a percentage, clamped to `0..=1`.
//! `transition` and its `-property`, `-duration`, `-delay`, and
//! `-timing-function` longhands keep up to [`MAX_TRANSITIONS`] entries.
//! `animation` and its `-name`, `-duration`, `-delay`, `-iteration-count`,
//! `-direction`, `-fill-mode`, and `-timing-function` longhands keep up to
//! [`MAX_ANIMATIONS`] entries and play `@keyframes` blocks;
//! `animation-play-state` is accepted and ignored.
//! [`compute_styles_animated`] runs both through an [`Animator`]; the plain
//! `compute_styles` functions ignore them. See [`Animator`] for what moves.
//!
//! `@media` reads `min-width`, `max-width`, and `prefers-color-scheme`.
//! Only `dark` matches a color scheme; `light` and any other value do not.
//! An inner `@media` replaces the outer query instead of combining with it.
//! `@keyframes` blocks are kept on the [`Stylesheet`], whether or not they sit
//! inside `@media`. Other at-rules are skipped. The cascade is user-agent defaults, then author
//! rules by specificity `(ids, classes, types)` and source order, then the
//! `style` attribute, then `!important` author rules in the same order, then
//! `!important` declarations in the `style` attribute.

use anvil_dom::{Document, NodeData, NodeId};
use std::collections::HashMap;

mod animation;
mod selector_check;
pub use animation::{
    ANIMATABLE, AnimProperty, AnimValue, AnimationDirection, AnimationSpec, Animations, Animator,
    FillMode, FrameClock, Keyframe, KeyframesRule, MAX_ANIMATIONS, MAX_TRANSITIONS, ManualClock,
    MonotonicClock, StepPosition, TimingFunction, TransitionSpec, Transitions, animation_name_hash,
    interpolate, read_value, write_value,
};

/// A parsed style sheet. Rules are owned, in source order, including rules
/// that came from inside `@media`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// `@keyframes` blocks in source order, including any inside `@media`.
    pub keyframes: Vec<KeyframesRule>,
}

/// Viewport tests attached to a rule. Absent fields are unconstrained.
///
/// `color_scheme` is owned lowercase text from the query. Widths are CSS
/// pixels rounded from the parsed number. A query with no recognized tests
/// matches every viewport.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaQuery {
    pub min_width_px: Option<u32>,
    pub max_width_px: Option<u32>,
    pub color_scheme: Option<String>,
}

impl MediaQuery {
    /// Parse parenthesized features out of an `@media` condition.
    ///
    /// `cond` is borrowed. The result owns any color-scheme string. Unknown
    /// features are ignored. A condition with no closing `)` stops at the
    /// broken parenthesis. Width values may include a `px` suffix.
    pub fn parse(cond: &str) -> Self {
        let mut mq = Self::default();
        let lower = cond.to_ascii_lowercase();
        let mut rest = lower.as_str();
        while let Some(open) = rest.find('(') {
            let after_open = &rest[open + 1..];
            let Some(close) = after_open.find(')') else {
                break;
            };
            let expr = after_open[..close].trim();
            if let Some((k, v)) = expr.split_once(':') {
                let key = k.trim();
                let val = v.trim().trim_end_matches("px").trim();
                match key {
                    "min-width" => {
                        mq.min_width_px = val.parse::<f32>().ok().map(|f| f.round() as u32);
                    }
                    "max-width" => {
                        mq.max_width_px = val.parse::<f32>().ok().map(|f| f.round() as u32);
                    }
                    "prefers-color-scheme" => {
                        mq.color_scheme = Some(val.to_string());
                    }
                    _ => {}
                }
            }
            rest = &after_open[close + 1..];
        }
        mq
    }

    /// True when `viewport_width` is inside the min and max, and the color
    /// scheme is unset or `dark`.
    ///
    /// Width is rounded to a `u32` after clamping at 0. Any other color scheme,
    /// including `light`, returns false. The query is borrowed.
    pub fn matches(&self, viewport_width: f32) -> bool {
        let vw = viewport_width.max(0.0).round() as u32;
        if let Some(min_w) = self.min_width_px {
            if vw < min_w {
                return false;
            }
        }
        if let Some(max_w) = self.max_width_px {
            if vw > max_w {
                return false;
            }
        }
        if let Some(scheme) = &self.color_scheme {
            if !scheme.eq_ignore_ascii_case("dark") {
                return false;
            }
        }
        true
    }
}

/// One style rule. Selectors and declarations are owned.
/// `media` is `None` when the rule was not inside `@media`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
    pub media: Option<MediaQuery>,
}

/// One compound in a selector chain: optional tag, id, and classes.
///
/// `universal` is `*` with no tag, id, or class. Identifiers are owned.
/// An empty compound matches nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompoundSelector {
    pub universal: bool,
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
}

impl CompoundSelector {
    /// True when the compound has no universal flag, tag, id, or class.
    fn is_empty(&self) -> bool {
        !self.universal && self.tag.is_none() && self.id.is_none() && self.classes.is_empty()
    }

    /// True when `node_id` is an element and every tag, id, and class matches.
    ///
    /// `doc` is borrowed. Text and the document node never match. A universal
    /// compound with no other tests matches every element.
    fn matches(&self, doc: &Document, node_id: NodeId) -> bool {
        let data = doc.get(node_id).data();
        let Some(actual_tag) = data.tag() else {
            return false;
        };
        if let Some(tag) = &self.tag {
            if actual_tag != tag {
                return false;
            }
        }
        if let Some(id) = &self.id {
            if data.id() != Some(id.as_str()) {
                return false;
            }
        }
        for class_name in &self.classes {
            if !data.has_class(class_name) {
                return false;
            }
        }
        true
    }

    /// Specificity as `(ids, classes, types)`. A tag contributes one type.
    /// The universal flag contributes nothing. The class count is cast to `u16`.
    fn specificity(&self) -> (u16, u16, u16) {
        let ids = u16::from(self.id.is_some());
        let classes = self.classes.len() as u16;
        let tags = u16::from(self.tag.is_some());
        (ids, classes, tags)
    }
}

/// A selector chain. The last compound is the subject. Earlier compounds
/// must match some ancestor, in order, not necessarily the parent.
/// `chain` is owned. An empty chain matches nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selector {
    pub chain: Vec<CompoundSelector>,
}

impl Selector {
    /// True when `node_id` matches the subject and each earlier compound
    /// matches an ancestor. `doc` is borrowed. An empty chain is false.
    pub fn matches(&self, doc: &Document, node_id: NodeId) -> bool {
        let Some((last, ancestors)) = self.chain.split_last() else {
            return false;
        };
        if !last.matches(doc, node_id) {
            return false;
        }
        let mut current = doc.get(node_id).parent();
        for required in ancestors.iter().rev() {
            let mut found = None;
            while let Some(parent_id) = current {
                if required.matches(doc, parent_id) {
                    found = doc.get(parent_id).parent();
                    break;
                }
                current = doc.get(parent_id).parent();
            }
            if found.is_none() && current.is_none() {
                return false;
            }
            current = found;
        }
        true
    }

    /// Sum of each compound's `(ids, classes, types)`.
    pub fn specificity(&self) -> (u16, u16, u16) {
        self.chain.iter().fold((0, 0, 0), |(i, c, t), part| {
            let (pi, pc, pt) = part.specificity();
            (i + pi, c + pc, t + pt)
        })
    }
}

/// One CSS declaration. `name` is owned and lowercase. `value` is owned
/// and still unparsed, with any trailing `!important` taken off.
///
/// `important` is true when the value ended in `!important`. Important
/// declarations win over normal ones in the cascade, whatever their
/// specificity or order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// sRGB color. Channels are 0–255. `a` is 255 when the source omitted alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// Opaque black.
    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    /// Opaque white.
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    /// Parse `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`, `rgb()`, `rgba()`, or a
    /// named color: black, white, red, green, blue, gray, grey, silver, yellow,
    /// orange, transparent.
    ///
    /// `value` is borrowed. `None` if the text is not one of those forms.
    /// `rgba` alpha with a decimal point and a value of at most 1 is a fraction
    /// of 255. Any other alpha is a 0–255 channel.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if let Some(hex) = value.strip_prefix('#') {
            return parse_hex(hex);
        }
        let lower = value.to_ascii_lowercase();
        if let Some(inner) = lower.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
            return parse_rgb(inner, false);
        }
        if let Some(inner) = lower
            .strip_prefix("rgba(")
            .and_then(|s| s.strip_suffix(')'))
        {
            return parse_rgb(inner, true);
        }
        Some(match lower.as_str() {
            "black" => Self::BLACK,
            "white" => Self {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            "red" => Self {
                r: 255,
                g: 0,
                b: 0,
                a: 255,
            },
            "green" => Self {
                r: 0,
                g: 128,
                b: 0,
                a: 255,
            },
            "blue" => Self {
                r: 0,
                g: 0,
                b: 255,
                a: 255,
            },
            "gray" | "grey" => Self {
                r: 128,
                g: 128,
                b: 128,
                a: 255,
            },
            "silver" => Self {
                r: 192,
                g: 192,
                b: 192,
                a: 255,
            },
            "yellow" => Self {
                r: 255,
                g: 255,
                b: 0,
                a: 255,
            },
            "orange" => Self {
                r: 255,
                g: 165,
                b: 0,
                a: 255,
            },
            "transparent" => Self {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            _ => return None,
        })
    }

    /// Owned `#rrggbb`. Alpha is omitted.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// `display` as stored after the cascade. Unknown values are left unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// Participate in a line. This is the initial value.
    Inline,
    /// A block box.
    Block,
    /// A grid container (`display: grid`).
    Grid,
    /// A flex container. The items are the element's children.
    Flex,
    /// The element and its descendants generate no box.
    None,
}

/// Main axis of a flex container. The initial value is [`Self::Row`].
/// `row-reverse` and `column-reverse` are not recognized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexDirection {
    /// Lay items out on horizontal lines. [`FlexWrap`] decides whether they wrap.
    Row,
    /// Lay items out in a single vertical line. Wrap and `flex-grow` are ignored.
    Column,
}

/// Whether a row flex container breaks items onto more than one line.
/// The initial value is [`Self::NoWrap`]. `wrap-reverse` is not recognized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlexWrap {
    /// Keep every item on one line, even when the line is wider than the container.
    NoWrap,
    /// Start a new line when the next item does not fit. A single item that is
    /// wider than the container stays on its own line.
    Wrap,
}

/// Main-axis packing for a flex line. Only one line is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JustifyContent {
    /// Pack against the start. `flex-start` and `start` both map here.
    Start,
    /// Center the free space before and after the items.
    Center,
    /// Pack against the end. `flex-end` and `end` both map here.
    End,
    /// First item at the start, last at the end, equal gaps between.
    /// A single item is placed at the start.
    SpaceBetween,
}

/// Cross-axis alignment for flex items. Column flex ignores this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignItems {
    /// Align to the cross start. `flex-start` and `start` both map here.
    Start,
    /// Center on the cross axis.
    Center,
    /// Align to the cross end. `flex-end` and `end` both map here.
    End,
    /// Stretch items whose height is `auto` to the line's cross size.
    /// This is the initial value. Width is not stretched.
    Stretch,
}

/// `position` as stored for anvil-layout. Unknown values are left unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    /// In flow. Offsets are ignored. This is the initial value.
    Static,
    /// In flow, then shifted by `top`/`left` (or `bottom`/`right`).
    Relative,
    /// Out of flow. The containing block is the parent's border box.
    Absolute,
    /// Out of flow. Laid out against the origin, not a separate viewport.
    Fixed,
    /// In flow, then shifted while scrolling so that the box keeps the
    /// `top` / `right` / `bottom` / `left` distance from the edge of its
    /// scrollport, without leaving its containing block.
    Sticky,
}

/// `overflow` as stored for layout and paint. `scroll` and `auto` become [`Self::Hidden`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    /// Descendants may paint outside the box. This is the initial value.
    Visible,
    /// Clip descendants to the padding edge. Also used for `scroll` and `auto`.
    Hidden,
}

/// A length stored after parsing. [`Size::used_px`] turns `px`, `%`, `em`,
/// and `rem` into CSS pixels. `vw`, `vh`, `vmin`, and `vmax` stay here until
/// layout, which uses the viewport stored on [`StyleMap`].
/// Units other than those are not represented; they parse as [`Self::Auto`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    /// The layout stage chooses the size.
    Auto,
    /// A CSS pixel length. Zero is allowed. Negative values are not rejected here.
    Px(f32),
    /// A percentage of the containing block on the axis layout is resolving.
    /// `50.0` means `50%`.
    Percent(f32),
    /// A multiple of the element's own font size. `2.0` means `2em`.
    Em(f32),
    /// A multiple of the root font size. `2.0` means `2rem`.
    Rem(f32),
    /// A percentage of the viewport width. `50.0` means `50vw`.
    Vw(f32),
    /// A percentage of the viewport height. `50.0` means `50vh`.
    Vh(f32),
    /// A percentage of the smaller viewport side. `50.0` means `50vmin`.
    Vmin(f32),
    /// A percentage of the larger viewport side. `50.0` means `50vmax`.
    Vmax(f32),
    /// A multiple of the width of the `0` glyph in the element's font.
    /// `20.0` means `20ch`. Layout resolves it; see `anvil_layout::ch_width`.
    Ch(f32),
    /// A combined `calc(...)` expression across `px`, `%`, `em`, and `rem`.
    Calc {
        px: f32,
        percent: f32,
        em: f32,
        rem: f32,
    },
}

impl Size {
    /// Used CSS pixels, or `None` when this length is [`Size::Auto`].
    ///
    /// Viewport units and `ch` also return `None` here, because they need
    /// the viewport or font metrics that only layout has.
    ///
    /// `percent_basis` is the containing-block size on this axis, in CSS pixels.
    /// A percentage needs a finite basis. `em_px` is this element's font size.
    /// `rem_px` is the root font size from [`StyleMap::root_font_px`].
    pub fn used_px(self, percent_basis: f32, em_px: f32, rem_px: f32) -> Option<f32> {
        match self {
            Size::Auto => None,
            Size::Px(value) => Some(value),
            Size::Percent(value) => {
                if percent_basis.is_finite() {
                    Some(percent_basis * value / 100.0)
                } else {
                    None
                }
            }
            Size::Em(value) => Some(em_px * value),
            Size::Rem(value) => Some(rem_px * value),
            Size::Vw(_) | Size::Vh(_) | Size::Vmin(_) | Size::Vmax(_) | Size::Ch(_) => None,
            Size::Calc {
                px,
                percent,
                em,
                rem,
            } => {
                if percent != 0.0 && !percent_basis.is_finite() {
                    return None;
                }
                let pct_px = if percent != 0.0 {
                    percent_basis * percent / 100.0
                } else {
                    0.0
                };
                Some(px + pct_px + em * em_px + rem * rem_px)
            }
        }
    }
}

/// Four CSS pixel edges, clockwise from the top. Not a percentage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edge {
    /// All four edges zero.
    pub const fn zero() -> Self {
        Self {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        }
    }

    /// The same pixel length on every edge.
    pub const fn all(value: f32) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }
}

/// One `box-shadow` layer. Offsets are CSS pixels, y downward (positive `offset_y`
/// moves the shadow down). Blur is floored at 0 when parsed. The default color,
/// if the declaration omits one, is black at alpha 128. Inset shadows are not parsed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: Color,
}

/// A two-stop `linear-gradient`. `angle_deg` follows CSS: 0 is upward, 90 is
/// to the right. Extra color stops are dropped; only the first and last are kept.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinearGradient {
    pub angle_deg: f32,
    pub start_color: Color,
    pub end_color: Color,
}

/// One track in `grid-template-columns`, `grid-template-rows`, or `grid-auto-rows`.
///
/// Columns size `auto` as `1fr`, because this engine does not measure the
/// intrinsic width of grid items. Rows size `auto` to the tallest item that
/// sits only in that row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GridTrack {
    /// Fixed pixel size (`120px`).
    Px(f32),
    /// Percentage of the grid container content size on that axis (`25%`).
    /// A row percentage with no definite container height acts as `auto`.
    Percent(f32),
    /// Share of remaining free space (`1fr`, `2fr`).
    Fr(f32),
    /// `auto`. See the type docs for how each axis sizes it.
    Auto,
    /// `minmax(min, max)`. The minimum is never a flexible size.
    MinMax(TrackBreadth, TrackBreadth),
}

/// Largest grid line number or span kept by the parser. Bigger values are
/// clamped, so a typo such as `span 99999` does not build a huge grid.
pub const MAX_GRID_LINE: i16 = 100;

/// One end of a grid item's placement on an axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GridLine {
    /// Auto-placed. This is the initial value.
    #[default]
    Auto,
    /// A numbered line. Positive counts from the start (1 is the first
    /// line), negative from the end of the explicit grid (-1 is the last).
    /// Never 0.
    Line(i16),
    /// `span N`. Always at least 1.
    Span(i16),
}

/// `grid-column` or `grid-row` after parsing: a start and an end line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GridPlacement {
    pub start: GridLine,
    pub end: GridLine,
}

/// One side of a `minmax()` track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackBreadth {
    /// Fixed pixel size.
    Px(f32),
    /// Percentage of the grid container content size on that axis.
    Percent(f32),
    /// Flexible share. Only valid as the maximum.
    Fr(f32),
    /// Content size on rows, `1fr` behavior on columns when it is the maximum.
    Auto,
}

/// Most functions one `transform` list keeps. A longer list is ignored.
pub const MAX_TRANSFORM_FNS: usize = 8;

/// One 2D transform function. Angles are radians. Translations keep their
/// unit; layout resolves percentages against the border box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TransformFn {
    /// `translate()`, `translateX()`, `translateY()`.
    Translate(Size, Size),
    /// `scale()`, `scaleX()`, `scaleY()`.
    Scale(f32, f32),
    /// `rotate()`. Positive turns clockwise on screen.
    Rotate(f32),
    /// `skew()`, `skewX()`, `skewY()`.
    Skew(f32, f32),
    /// `matrix(a, b, c, d, e, f)`, with `e` and `f` in CSS pixels.
    Matrix([f32; 6]),
}

/// A parsed `transform` list in source order. Empty is `none`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformList {
    fns: [TransformFn; MAX_TRANSFORM_FNS],
    len: u8,
}

impl TransformList {
    /// The empty list, `transform: none`.
    pub const NONE: Self = Self {
        fns: [TransformFn::Scale(1.0, 1.0); MAX_TRANSFORM_FNS],
        len: 0,
    };

    /// A list holding `fns`, or `None` when there are more than
    /// [`MAX_TRANSFORM_FNS`].
    pub fn new(fns: &[TransformFn]) -> Option<Self> {
        if fns.len() > MAX_TRANSFORM_FNS {
            return None;
        }
        let mut list = Self::NONE;
        list.fns[..fns.len()].copy_from_slice(fns);
        list.len = fns.len() as u8;
        Some(list)
    }

    /// The functions in source order.
    pub fn functions(&self) -> &[TransformFn] {
        &self.fns[..usize::from(self.len).min(MAX_TRANSFORM_FNS)]
    }

    /// True for `none`.
    pub fn is_none(&self) -> bool {
        self.len == 0
    }
}

/// `transform-origin` as a horizontal and a vertical length. Percentages
/// are of the border box. The initial value is `50% 50%`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformOrigin {
    pub x: Size,
    pub y: Size,
}

impl TransformOrigin {
    /// `50% 50%`, the center of the border box.
    pub const CENTER: Self = Self {
        x: Size::Percent(50.0),
        y: Size::Percent(50.0),
    };
}

/// Used values for one element after the user-agent defaults, the cascade,
/// and the inline `style` attribute.
///
/// `width`, `height`, `max-width`, and offsets may still be `%`, `em`, or `rem`.
/// `font-size` is already in CSS pixels. `background` is `None` when no solid color was set.
/// `background_gradient` is independent and, when set, also stores its first
/// stop in `background`. `box_shadow` is `None` when unset or `none`.
/// Inherited on elements: `color`, `font_size`, and `underline`. A child's
/// `border_color` starts as the inherited `color`, not the parent's border color.
/// Text nodes keep the parent style unchanged. This value does not include
/// a used position; anvil-layout resolves `auto` sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub top: Size,
    pub right: Size,
    pub bottom: Size,
    pub left: Size,
    pub z_index: i32,
    pub overflow: Overflow,
    pub color: Color,
    pub background: Option<Color>,
    pub background_gradient: Option<LinearGradient>,
    pub box_shadow: Option<BoxShadow>,
    pub font_size: f32,
    pub margin: Edge,
    pub padding: Edge,
    pub border: Edge,
    pub border_color: Color,
    pub border_radius: f32,
    pub width: Size,
    pub height: Size,
    pub max_width: Size,
    pub underline: bool,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    /// Main-size growth factor for a flex item. The initial value is 0.
    /// This is not inherited. Negative values are stored as 0. Column flex ignores it.
    pub flex_grow: f32,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    pub grid_template_columns: [GridTrack; 12],
    pub grid_column_count: u8,
    /// Explicit row tracks from `grid-template-rows`. See [`Self::grid_row_tracks`].
    pub grid_template_rows: [GridTrack; 12],
    pub grid_row_count: u8,
    /// Size of rows past the explicit grid (`grid-auto-rows`). The initial value is `auto`.
    pub grid_auto_rows: GridTrack,
    /// Placement from `grid-column` and its longhands. Not inherited.
    pub grid_column: GridPlacement,
    /// Placement from `grid-row` and its longhands. Not inherited.
    pub grid_row: GridPlacement,
    pub gap: f32,
    /// `transform`. Not inherited. See [`TransformList`].
    pub transform: TransformList,
    /// `transform-origin`. Not inherited.
    pub transform_origin: TransformOrigin,
    /// `opacity` in `0..=1`. Not inherited; it applies to the box and
    /// everything in it as one group.
    pub opacity: f32,
    /// `transition` and its longhands. Not inherited.
    pub transitions: Transitions,
    /// `animation` and its longhands. Not inherited.
    pub animations: Animations,
}

impl ComputedStyle {
    /// Initial values: inline, static, visible, black text, 16px, no margin,
    /// padding, border, or background, row flex, start justification, stretch
    /// alignment, and zero gap.
    pub const fn initial() -> Self {
        Self {
            display: Display::Inline,
            position: Position::Static,
            top: Size::Auto,
            right: Size::Auto,
            bottom: Size::Auto,
            left: Size::Auto,
            z_index: 0,
            overflow: Overflow::Visible,
            color: Color::BLACK,
            background: None,
            background_gradient: None,
            box_shadow: None,
            font_size: 16.0,
            margin: Edge::zero(),
            padding: Edge::zero(),
            border: Edge::zero(),
            border_color: Color::BLACK,
            border_radius: 0.0,
            width: Size::Auto,
            height: Size::Auto,
            max_width: Size::Auto,
            underline: false,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            flex_grow: 0.0,
            justify_content: JustifyContent::Start,
            align_items: AlignItems::Stretch,
            grid_template_columns: [GridTrack::Fr(1.0); 12],
            grid_column_count: 0,
            grid_template_rows: [GridTrack::Auto; 12],
            grid_row_count: 0,
            grid_auto_rows: GridTrack::Auto,
            grid_column: GridPlacement {
                start: GridLine::Auto,
                end: GridLine::Auto,
            },
            grid_row: GridPlacement {
                start: GridLine::Auto,
                end: GridLine::Auto,
            },
            gap: 0.0,
            transform: TransformList::NONE,
            transform_origin: TransformOrigin::CENTER,
            opacity: 1.0,
            transitions: Transitions::INITIAL,
            animations: Animations::INITIAL,
        }
    }

    /// Active column tracks parsed from `grid-template-columns`.
    pub fn grid_tracks(&self) -> &[GridTrack] {
        &self.grid_template_columns[..(self.grid_column_count as usize).min(12)]
    }

    /// Active explicit row tracks parsed from `grid-template-rows`.
    /// Empty when the property is unset; every row is then a `grid-auto-rows` row.
    pub fn grid_row_tracks(&self) -> &[GridTrack] {
        &self.grid_template_rows[..(self.grid_row_count as usize).min(12)]
    }

    /// Style a child element starts from. Copies `color`, `font_size`, and
    /// `underline`, sets `border_color` to that color, and resets everything else.
    fn inherited_child(self) -> Self {
        Self {
            color: self.color,
            font_size: self.font_size,
            underline: self.underline,
            border_color: self.color,
            ..Self::initial()
        }
    }
}

/// Computed styles indexed by [`NodeId`]. The vector is owned.
/// Slots that the walk never wrote stay at [`ComputedStyle::initial`].
#[derive(Clone, Debug, PartialEq)]
pub struct StyleMap {
    styles: Vec<ComputedStyle>,
    root_font_px: f32,
    viewport_width: f32,
    viewport_height: f32,
}

impl StyleMap {
    /// Borrow the style for `id`. An id past the map returns a static initial
    /// style, not an error. The reference lives as long as `self`.
    pub fn get(&self, id: NodeId) -> &ComputedStyle {
        self.styles.get(id.index()).unwrap_or(&FALLBACK)
    }

    /// Computed `font-size` of the `html` element, in CSS pixels.
    /// `rem` uses this value. It stays 16 when `html` does not set a font size.
    pub fn root_font_px(&self) -> f32 {
        self.root_font_px
    }

    /// Viewport width in CSS pixels, used for `vw` and `vmin` / `vmax`.
    pub fn viewport_width(&self) -> f32 {
        self.viewport_width
    }

    /// Viewport height in CSS pixels, used for `vh` and `vmin` / `vmax`.
    pub fn viewport_height(&self) -> f32 {
        self.viewport_height
    }
}

const FALLBACK: ComputedStyle = ComputedStyle::initial();

/// Parse `input` into an owned style sheet.
///
/// Comments (`/* */`) are replaced with a space. Rules inside `@media` keep
/// that query. An inner `@media` does not combine with the outer one.
/// `@keyframes` blocks go to [`Stylesheet::keyframes`]. Other at-rules are
/// skipped. The input is borrowed.
pub fn parse_css(input: &str) -> Stylesheet {
    let input = strip_comments(input);
    let mut sheet = Stylesheet::default();
    parse_css_rules(&input, None, &mut sheet);
    sheet
}

/// Index of the `}` that closes a body whose opening `{` was already consumed.
///
/// Nested braces are counted. If the body never closes, the length is returned
/// and the caller treats the rest as the body.
fn find_matching_brace(src: &str) -> usize {
    let mut depth = 1usize;
    for (idx, ch) in src.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return idx;
                }
            }
            _ => {}
        }
    }
    src.len()
}

/// Split `rest` into rules and keyframes, appending to `sheet`.
///
/// `active_media` is cloned onto each rule. `@media` replaces it for the
/// nested body instead of intersecting the two queries. A prelude that starts
/// with `@` and is not `@media` or `@keyframes` is dropped, body included.
/// A rule whose selector list has an invalid selector is dropped too.
fn parse_css_rules(mut rest: &str, active_media: Option<MediaQuery>, sheet: &mut Stylesheet) {
    while let Some(open) = rest.find('{') {
        let selector_src = rest[..open].trim();
        rest = &rest[open + 1..];
        let close = find_matching_brace(rest);
        let body_src = &rest[..close];
        rest = if close < rest.len() {
            &rest[close + 1..]
        } else {
            ""
        };
        if let Some(cond) = selector_src.strip_prefix("@media") {
            let mq = MediaQuery::parse(cond);
            parse_css_rules(body_src, Some(mq), sheet);
            continue;
        }
        if let Some(name) = selector_src.strip_prefix("@keyframes") {
            sheet
                .keyframes
                .extend(animation::parse_keyframes(name, body_src));
            continue;
        }
        if selector_src.starts_with('@') {
            continue;
        }
        let Some(selectors) = parse_selectors(selector_src) else {
            continue;
        };
        if selectors.is_empty() {
            continue;
        }
        sheet.rules.push(Rule {
            selectors,
            declarations: parse_declarations(body_src),
            media: active_media.clone(),
        });
    }
}

/// Cascade `sheet` onto `doc` at a viewport width of 960 CSS pixels.
///
/// Both inputs are borrowed. The returned map is owned. Equivalent to
/// [`compute_styles_with_viewport`] with `960.0`.
pub fn compute_styles(doc: &Document, sheet: &Stylesheet) -> StyleMap {
    compute_styles_with_viewport(doc, sheet, 960.0)
}

/// Cascade `sheet` onto `doc` for `viewport_width` in CSS pixels.
///
/// The viewport height stored for `vh` is the same number as the width.
/// Call [`compute_styles_for_viewport`] when the window height is known.
pub fn compute_styles_with_viewport(
    doc: &Document,
    sheet: &Stylesheet,
    viewport_width: f32,
) -> StyleMap {
    compute_styles_for_viewport(doc, sheet, viewport_width, viewport_width)
}

/// Cascade `sheet` onto `doc` for a window `viewport_width` by `viewport_height`.
///
/// `vw` is a percentage of the width. `vh` is a percentage of the height.
/// `vmin` uses the smaller side and `vmax` the larger. A negative side is
/// treated as 0. `@media` still tests the width only.
pub fn compute_styles_for_viewport(
    doc: &Document,
    sheet: &Stylesheet,
    viewport_width: f32,
    viewport_height: f32,
) -> StyleMap {
    cascade_document(doc, sheet, viewport_width, viewport_height, None)
}

/// Cascade `sheet` onto `doc` across multiple CPU cores when `body` has independent child subtrees.
///
/// Produces bit-exact output matching [`compute_styles_for_viewport`].
pub fn compute_styles_parallel(
    doc: &Document,
    sheet: &Stylesheet,
    viewport_width: f32,
    viewport_height: f32,
) -> StyleMap {
    let viewport_width = viewport_width.max(0.0);
    let viewport_height = viewport_height.max(0.0);
    let total_nodes = doc_len(doc);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(32);
    let body_id = doc.body();
    let body_children: Vec<NodeId> = doc.children(body_id).to_vec();
    if workers <= 1 || body_children.len() < 2 {
        return compute_styles_for_viewport(doc, sheet, viewport_width, viewport_height);
    }
    let mut styles = vec![ComputedStyle::initial(); total_nodes];
    let html_id = doc.html();
    let mut root_vars = HashMap::new();
    let mut cx = Cascade {
        sheet,
        viewport_width,
        root_font_px: 16.0,
        animator: None,
    };
    if let NodeData::Element { tag, .. } = doc.get(html_id).data() {
        let html_style = cascade_element(
            doc,
            html_id,
            tag,
            &ComputedStyle::initial(),
            &mut root_vars,
            &cx,
        );
        if tag == "html" {
            cx.root_font_px = html_style.font_size;
        }
        if let Some(slot) = styles.get_mut(html_id.index()) {
            *slot = html_style;
        }
        for &child in doc.children(html_id) {
            if child != body_id {
                walk(
                    doc,
                    child,
                    &html_style,
                    &html_style,
                    &root_vars,
                    &mut cx,
                    &mut styles,
                );
            }
        }
        let mut body_vars = root_vars.clone();
        let body_style = match doc.get(body_id).data() {
            NodeData::Element { tag: btag, .. } => {
                cascade_element(doc, body_id, btag, &html_style, &mut body_vars, &cx)
            }
            _ => html_style,
        };
        if let Some(slot) = styles.get_mut(body_id.index()) {
            *slot = body_style;
        }
        let root_font_px = cx.root_font_px;
        let chunk_size = body_children.len().div_ceil(workers);
        let branch_results: Vec<Vec<(usize, ComputedStyle)>> = std::thread::scope(|s| {
            let mut handles = Vec::new();
            for chunk in body_children.chunks(chunk_size) {
                let body_vars_ref = &body_vars;
                handles.push(s.spawn(move || {
                    let mut local = Vec::new();
                    let worker_cx = Cascade {
                        sheet,
                        viewport_width,
                        root_font_px,
                        animator: None,
                    };
                    for &child_id in chunk {
                        walk_subtree_into(
                            doc,
                            child_id,
                            &body_style,
                            body_vars_ref,
                            &worker_cx,
                            &mut local,
                        );
                    }
                    local
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        for group in branch_results {
            for (idx, st) in group {
                if let Some(slot) = styles.get_mut(idx) {
                    *slot = st;
                }
            }
        }
        return StyleMap {
            styles,
            root_font_px,
            viewport_width,
            viewport_height,
        };
    }
    compute_styles_for_viewport(doc, sheet, viewport_width, viewport_height)
}

fn walk_subtree_into(
    doc: &Document,
    id: NodeId,
    parent: &ComputedStyle,
    parent_vars: &HashMap<String, String>,
    cx: &Cascade,
    out: &mut Vec<(usize, ComputedStyle)>,
) {
    let mut vars = parent_vars.clone();
    let style = match doc.get(id).data() {
        NodeData::Element { tag, .. } => cascade_element(doc, id, tag, parent, &mut vars, cx),
        _ => *parent,
    };
    out.push((id.index(), style));
    for &child in doc.children(id) {
        walk_subtree_into(doc, child, &style, &vars, cx, out);
    }
}

/// Like [`compute_styles_for_viewport`], with transitions run by `animator`
/// at its current time.
///
/// Call [`Animator::tick`] first to move the time. Each element's freshly
/// cascaded style is compared with the one from the animator's previous
/// pass; changed properties with a `transition` start moving, and running
/// transitions are written into the result before children inherit from
/// it. The first pass for an element starts nothing. See [`Animator`] for
/// a frame loop.
pub fn compute_styles_animated(
    doc: &Document,
    sheet: &Stylesheet,
    viewport_width: f32,
    viewport_height: f32,
    animator: &mut Animator,
) -> StyleMap {
    animator.begin_pass();
    let styles = cascade_document(doc, sheet, viewport_width, viewport_height, Some(animator));
    animator.end_pass();
    styles
}

/// Shared body of the `compute_styles` entry points.
fn cascade_document(
    doc: &Document,
    sheet: &Stylesheet,
    viewport_width: f32,
    viewport_height: f32,
    animator: Option<&mut Animator>,
) -> StyleMap {
    let viewport_width = viewport_width.max(0.0);
    let viewport_height = viewport_height.max(0.0);
    let mut styles = vec![ComputedStyle::initial(); doc_len(doc)];
    let mut cx = Cascade {
        sheet,
        viewport_width,
        root_font_px: 16.0,
        animator,
    };
    walk(
        doc,
        doc.root(),
        &ComputedStyle::initial(),
        &ComputedStyle::initial(),
        &HashMap::new(),
        &mut cx,
        &mut styles,
    );
    let mut root_font_px = cx.root_font_px;
    if let Some(html_style) = styles.get(doc.html().index()) {
        root_font_px = html_style.font_size;
    }
    StyleMap {
        styles,
        root_font_px,
        viewport_width,
        viewport_height,
    }
}

/// What stays the same across one cascade walk.
struct Cascade<'a> {
    sheet: &'a Stylesheet,
    viewport_width: f32,
    /// Font size of `html` once it is styled; 16 before that.
    root_font_px: f32,
    animator: Option<&'a mut Animator>,
}

/// One past the largest node index reachable from the root.
/// Unreachable detached nodes do not count. `doc` is borrowed.
fn doc_len(doc: &Document) -> usize {
    let mut count = 0;
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        count = count.max(id.index() + 1);
        stack.extend(doc.children(id).iter().copied());
    }
    count
}

/// Write `id`'s used style and recurse to its children.
///
/// Elements start from [`ComputedStyle::inherited_child`], then user-agent
/// defaults, then author rules, then the inline `style` attribute. Text and
/// the document node copy `parent` unchanged. `out` must be long enough for
/// every reachable index; a short slot is skipped.
///
/// `parent_base` is the parent's style before animation. With an animator,
/// each element is cascaded from it for change detection, and again from
/// the animated `parent` when the two differ, so children inherit animated
/// values without that counting as a change of their own.
fn walk(
    doc: &Document,
    id: NodeId,
    parent: &ComputedStyle,
    parent_base: &ComputedStyle,
    parent_vars: &HashMap<String, String>,
    cx: &mut Cascade,
    out: &mut [ComputedStyle],
) {
    let mut vars = parent_vars.clone();
    let (style, base) = match doc.get(id).data() {
        NodeData::Element { tag, .. } => {
            let base = cascade_element(doc, id, tag, parent_base, &mut vars, cx);
            if tag == "html" {
                cx.root_font_px = base.font_size;
            }
            let style = if cx.animator.is_none() {
                base
            } else {
                let mut style = if parent == parent_base {
                    base
                } else {
                    let mut scratch = parent_vars.clone();
                    cascade_element(doc, id, tag, parent, &mut scratch, cx)
                };
                let input = animation::AnimInput {
                    keyframes: &cx.sheet.keyframes,
                    vars: &vars,
                    parent_font_px: parent.font_size,
                    root_font_px: cx.root_font_px,
                };
                if let Some(animator) = cx.animator.as_deref_mut() {
                    animator.apply(id, &base, &mut style, &input);
                }
                style
            };
            (style, base)
        }
        _ => (*parent, *parent_base),
    };
    if let Some(slot) = out.get_mut(id.index()) {
        *slot = style;
    }
    let children: Vec<NodeId> = doc.children(id).to_vec();
    for child in children {
        walk(doc, child, &style, &base, &vars, cx, out);
    }
}

/// Cascade one element from `parent`: inherited values, user-agent
/// defaults, author rules, then the inline `style` attribute.
fn cascade_element(
    doc: &Document,
    id: NodeId,
    tag: &str,
    parent: &ComputedStyle,
    vars: &mut HashMap<String, String>,
    cx: &Cascade,
) -> ComputedStyle {
    let parent_font_px = parent.font_size;
    let mut style = parent.inherited_child();
    apply_user_agent(&mut style, tag);
    if doc.get(id).data().attr("hidden").is_some() {
        style.display = Display::None;
    }
    apply_author(
        &mut style,
        vars,
        doc,
        id,
        cx.sheet,
        cx.viewport_width,
        parent_font_px,
        cx.root_font_px,
    );
    style
}

/// Apply author rules that match `id` and then its inline `style` attribute.
///
/// A rule whose `@media` does not match `viewport_width` is skipped. Only the
/// winning selector's specificity is kept per rule. Rules are sorted by
/// specificity, then source order, so a later tied rule overrides an earlier
/// one. Declarations are applied in four groups, each later group winning:
/// normal author declarations, normal inline ones, important author ones,
/// and important inline ones. Custom properties are resolved in the same
/// order before any other property is applied.
fn apply_author(
    style: &mut ComputedStyle,
    vars: &mut HashMap<String, String>,
    doc: &Document,
    id: NodeId,
    sheet: &Stylesheet,
    viewport_width: f32,
    parent_font_px: f32,
    root_font_px: f32,
) {
    let mut matched = Vec::new();
    for (order, rule) in sheet.rules.iter().enumerate() {
        if let Some(mq) = &rule.media {
            if !mq.matches(viewport_width) {
                continue;
            }
        }
        let specificity = rule
            .selectors
            .iter()
            .filter(|selector| selector.matches(doc, id))
            .map(Selector::specificity)
            .max();
        if let Some(specificity) = specificity {
            matched.push((specificity, order, rule));
        }
    }
    matched.sort_by_key(|(specificity, order, _)| (*specificity, *order));
    let inline = doc
        .get(id)
        .data()
        .attr("style")
        .map(parse_declarations)
        .unwrap_or_default();
    let mut ordered: Vec<&Declaration> = Vec::new();
    for important in [false, true] {
        for (_, _, rule) in &matched {
            ordered.extend(
                rule.declarations
                    .iter()
                    .filter(|declaration| declaration.important == important),
            );
        }
        ordered.extend(
            inline
                .iter()
                .filter(|declaration| declaration.important == important),
        );
    }
    for declaration in &ordered {
        if declaration.name.starts_with("--") {
            let resolved = resolve_css_vars(declaration.value.trim(), vars);
            if !resolved.is_empty() {
                vars.insert(declaration.name.clone(), resolved);
            }
        }
    }
    for declaration in ordered {
        apply_declaration(style, vars, declaration, parent_font_px, root_font_px);
    }
}

/// Tag defaults applied before author CSS. `tag` is borrowed and already lowercase.
///
/// Block tags become `display: block`. `base`, `head`, `link`, `meta`,
/// `option`, `script`, `style`, and `title` become `display: none`. `body`
/// gets an 8px margin. Tables, lists, rules, links, forms, and form controls
/// get the fixed padding, borders, and colors written below. Heading sizes
/// replace the inherited font size for `h1` through `h4` and `small`.
fn apply_user_agent(style: &mut ComputedStyle, tag: &str) {
    if is_block_tag(tag) {
        style.display = Display::Block;
    }
    if matches!(
        tag,
        "base" | "head" | "link" | "meta" | "option" | "script" | "style" | "title"
    ) {
        style.display = Display::None;
    }
    if tag == "body" {
        style.margin = Edge::all(8.0);
    }
    if tag == "table" {
        style.margin.top = 6.0;
        style.margin.bottom = 6.0;
    }
    if tag == "th" || tag == "td" {
        style.padding = Edge::all(6.0);
        style.border = Edge::all(1.0);
        style.border_color = Color {
            r: 68,
            g: 60,
            b: 52,
            a: 255,
        };
    }
    if tag == "ul" || tag == "ol" {
        style.padding.left = 24.0;
        style.margin.top = 6.0;
        style.margin.bottom = 6.0;
    }
    if tag == "hr" {
        style.height = Size::Px(1.0);
        style.background = Some(Color {
            r: 128,
            g: 128,
            b: 128,
            a: 255,
        });
        style.margin.top = 8.0;
        style.margin.bottom = 8.0;
    }
    if tag == "a" {
        style.color = Color {
            r: 0,
            g: 102,
            b: 204,
            a: 255,
        };
        style.underline = true;
    }
    if tag == "form" {
        style.margin.top = 6.0;
        style.margin.bottom = 10.0;
    }
    if tag == "input" || tag == "textarea" || tag == "select" {
        style.display = Display::Block;
        style.background = Some(Color::WHITE);
        style.color = Color {
            r: 24,
            g: 28,
            b: 36,
            a: 255,
        };
        style.border = Edge::all(1.0);
        style.border_color = Color {
            r: 138,
            g: 148,
            b: 166,
            a: 255,
        };
        style.border_radius = 4.0;
        style.padding = Edge {
            top: 6.0,
            right: 8.0,
            bottom: 6.0,
            left: 8.0,
        };
        style.margin.top = 4.0;
        style.margin.bottom = 4.0;
    }
    if tag == "button" {
        style.display = Display::Block;
        style.background = Some(Color {
            r: 43,
            g: 53,
            b: 72,
            a: 255,
        });
        style.color = Color {
            r: 244,
            g: 241,
            b: 234,
            a: 255,
        };
        style.border = Edge::all(1.0);
        style.border_color = Color {
            r: 68,
            g: 82,
            b: 108,
            a: 255,
        };
        style.border_radius = 4.0;
        style.padding = Edge {
            top: 6.0,
            right: 12.0,
            bottom: 6.0,
            left: 12.0,
        };
        style.margin.top = 4.0;
        style.margin.bottom = 4.0;
    }
    style.font_size = match tag {
        "h1" => 32.0,
        "h2" => 24.0,
        "h3" => 20.0,
        "h4" => 18.0,
        "small" => 13.0,
        _ => style.font_size,
    };
}

/// Tags that the user agent displays as blocks. Not a CSS parser.
fn is_block_tag(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "canvas"
            | "caption"
            | "center"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "dd"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "li"
            | "main"
            | "nav"
            | "noscript"
            | "ol"
            | "p"
            | "picture"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
            | "video"
    )
}

/// Apply one declaration. Unknown properties and unparsed values are ignored.
///
/// `display: grid` and `var(--token)` are applied when the value parses. On
/// `width`, `height`, `max-width`, and the offsets, `px`, `%`, `em`, `rem`,
/// `vw`, `vh`, `vmin`, and `vmax` are kept. Another unit becomes
/// [`Size::Auto`]. `ch` is kept for layout. `font-size` resolves `%` and `em` against `parent_font_px`,
/// and `rem` against `root_font_px`. Margin, padding, border, and `gap` still
/// accept only `px` and `0`. `overflow` values `scroll` and
/// `auto` are stored as [`Overflow::Hidden`].
fn apply_declaration(
    style: &mut ComputedStyle,
    vars: &mut HashMap<String, String>,
    declaration: &Declaration,
    parent_font_px: f32,
    root_font_px: f32,
) {
    if declaration.name.starts_with("--") {
        return;
    }
    let resolved = resolve_css_vars(declaration.value.trim(), vars);
    let value = resolved.trim();
    if value.is_empty() {
        return;
    }
    match declaration.name.as_str() {
        "display" => {
            style.display = match value {
                "block" | "inline-block" | "list-item" | "flow-root" => Display::Block,
                "inline" => Display::Inline,
                "flex" | "inline-flex" => Display::Flex,
                "grid" | "inline-grid" => Display::Grid,
                "none" => Display::None,
                _ => return,
            };
        }
        "color" => {
            if let Some(color) = Color::parse(value) {
                style.color = color;
            }
        }
        "background" | "background-color" | "background-image" => {
            if value.eq_ignore_ascii_case("transparent") || value.eq_ignore_ascii_case("none") {
                style.background = None;
                style.background_gradient = None;
            } else if let Some(grad) = parse_linear_gradient(value) {
                style.background_gradient = Some(grad);
                style.background = Some(grad.start_color);
            } else if let Some(color) = Color::parse(value) {
                style.background = Some(color);
                style.background_gradient = None;
            }
        }
        "box-shadow" => {
            if value.eq_ignore_ascii_case("none") {
                style.box_shadow = None;
            } else if let Some(shadow) = parse_box_shadow(value) {
                style.box_shadow = Some(shadow);
            }
        }
        "font-size" => {
            if let Some(size) = parse_font_size(value, parent_font_px, root_font_px) {
                style.font_size = size.max(0.0);
            }
        }
        "text-decoration" => {
            if value.eq_ignore_ascii_case("underline") {
                style.underline = true;
            } else if value.eq_ignore_ascii_case("none") {
                style.underline = false;
            }
        }
        "margin" => {
            if let Some(edge) = parse_edge(value) {
                style.margin = edge;
            }
        }
        "margin-top" => {
            if let Some(px) = parse_px(value) {
                style.margin.top = px;
            }
        }
        "margin-right" => {
            if let Some(px) = parse_px(value) {
                style.margin.right = px;
            }
        }
        "margin-bottom" => {
            if let Some(px) = parse_px(value) {
                style.margin.bottom = px;
            }
        }
        "margin-left" => {
            if let Some(px) = parse_px(value) {
                style.margin.left = px;
            }
        }
        "padding" => {
            if let Some(edge) = parse_edge(value) {
                style.padding = edge;
            }
        }
        "padding-top" => {
            if let Some(px) = parse_px(value) {
                style.padding.top = px;
            }
        }
        "padding-right" => {
            if let Some(px) = parse_px(value) {
                style.padding.right = px;
            }
        }
        "padding-bottom" => {
            if let Some(px) = parse_px(value) {
                style.padding.bottom = px;
            }
        }
        "padding-left" => {
            if let Some(px) = parse_px(value) {
                style.padding.left = px;
            }
        }
        "border" => {
            apply_border_shorthand(style, value);
        }
        "border-width" => {
            if let Some(edge) = parse_edge(value) {
                style.border = edge;
            }
        }
        "border-color" => {
            if let Some(color) = Color::parse(value) {
                style.border_color = color;
            }
        }
        "border-top" => {
            if let Some((w, c)) = parse_border_side(value) {
                style.border.top = w;
                if let Some(color) = c {
                    style.border_color = color;
                }
            }
        }
        "border-right" => {
            if let Some((w, c)) = parse_border_side(value) {
                style.border.right = w;
                if let Some(color) = c {
                    style.border_color = color;
                }
            }
        }
        "border-bottom" => {
            if let Some((w, c)) = parse_border_side(value) {
                style.border.bottom = w;
                if let Some(color) = c {
                    style.border_color = color;
                }
            }
        }
        "border-left" => {
            if let Some((w, c)) = parse_border_side(value) {
                style.border.left = w;
                if let Some(color) = c {
                    style.border_color = color;
                }
            }
        }
        "border-radius" => {
            if let Some(px) = parse_px(value) {
                style.border_radius = px.max(0.0);
            }
        }
        "width" => style.width = parse_size(value),
        "height" => style.height = parse_size(value),
        "max-width" => style.max_width = parse_size(value),
        "position" => {
            style.position = match value.to_ascii_lowercase().as_str() {
                "static" => Position::Static,
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" => Position::Sticky,
                _ => return,
            };
        }
        "top" => style.top = parse_size(value),
        "right" => style.right = parse_size(value),
        "bottom" => style.bottom = parse_size(value),
        "left" => style.left = parse_size(value),
        "transform" => {
            if let Some(list) = parse_transform(value) {
                style.transform = list;
            }
        }
        "opacity" => {
            let parsed = match value.strip_suffix('%') {
                Some(pct) => pct.trim().parse::<f32>().ok().map(|p| p / 100.0),
                None => value.parse::<f32>().ok(),
            };
            if let Some(opacity) = parsed.filter(|o| !o.is_nan()) {
                style.opacity = opacity.clamp(0.0, 1.0);
            }
        }
        "transition"
        | "transition-property"
        | "transition-duration"
        | "transition-delay"
        | "transition-timing-function" => {
            let name = declaration.name.as_str();
            animation::apply_transition_declaration(&mut style.transitions, name, value);
        }
        "animation"
        | "animation-name"
        | "animation-duration"
        | "animation-delay"
        | "animation-iteration-count"
        | "animation-direction"
        | "animation-fill-mode"
        | "animation-timing-function"
        | "animation-play-state" => {
            let name = declaration.name.as_str();
            animation::apply_animation_declaration(&mut style.animations, name, value);
        }
        "transform-origin" => {
            if let Some(origin) = parse_transform_origin(value) {
                style.transform_origin = origin;
            }
        }
        "z-index" => {
            if let Ok(z) = value.parse::<i32>() {
                style.z_index = z;
            }
        }
        "overflow" => {
            style.overflow = match value.to_ascii_lowercase().as_str() {
                "hidden" | "scroll" | "auto" => Overflow::Hidden,
                "visible" => Overflow::Visible,
                _ => return,
            };
        }
        "flex-direction" => {
            style.flex_direction = match value {
                "column" => FlexDirection::Column,
                "row" => FlexDirection::Row,
                _ => return,
            };
        }
        "flex-wrap" => {
            style.flex_wrap = match value {
                "wrap" => FlexWrap::Wrap,
                "nowrap" => FlexWrap::NoWrap,
                _ => return,
            };
        }
        "flex-grow" => {
            if let Ok(grow) = value.parse::<f32>() {
                if grow.is_finite() {
                    style.flex_grow = grow.max(0.0);
                }
            }
        }
        "justify-content" => {
            style.justify_content = match value {
                "flex-start" | "start" => JustifyContent::Start,
                "center" => JustifyContent::Center,
                "flex-end" | "end" => JustifyContent::End,
                "space-between" => JustifyContent::SpaceBetween,
                _ => return,
            };
        }
        "align-items" => {
            style.align_items = match value {
                "flex-start" | "start" => AlignItems::Start,
                "center" => AlignItems::Center,
                "flex-end" | "end" => AlignItems::End,
                "stretch" => AlignItems::Stretch,
                _ => return,
            };
        }
        "grid-template-columns" => {
            if value.eq_ignore_ascii_case("none") {
                style.grid_column_count = 0;
            } else if let Some((tracks, count)) = parse_grid_template(value) {
                style.grid_template_columns = tracks;
                style.grid_column_count = count;
            }
        }
        "grid-template-rows" => {
            if value.eq_ignore_ascii_case("none") {
                style.grid_row_count = 0;
            } else if let Some((tracks, count)) = parse_grid_template(value) {
                style.grid_template_rows = tracks;
                style.grid_row_count = count;
            }
        }
        "grid-column" => {
            if let Some(placement) = parse_grid_placement(value) {
                style.grid_column = placement;
            }
        }
        "grid-row" => {
            if let Some(placement) = parse_grid_placement(value) {
                style.grid_row = placement;
            }
        }
        "grid-column-start" => {
            if let Some(line) = parse_grid_line(value) {
                style.grid_column.start = line;
            }
        }
        "grid-column-end" => {
            if let Some(line) = parse_grid_line(value) {
                style.grid_column.end = line;
            }
        }
        "grid-row-start" => {
            if let Some(line) = parse_grid_line(value) {
                style.grid_row.start = line;
            }
        }
        "grid-row-end" => {
            if let Some(line) = parse_grid_line(value) {
                style.grid_row.end = line;
            }
        }
        "grid-area" => {
            if let Some((row, column)) = parse_grid_area(value) {
                style.grid_row = row;
                style.grid_column = column;
            }
        }
        "grid-auto-rows" => {
            // Only one track is kept. A list such as `10px 20px` is not cycled.
            if let Some(track) = parse_single_grid_track(value) {
                style.grid_auto_rows = track;
            }
        }
        "gap" | "grid-gap" => {
            if let Some(px) = parse_px(value) {
                style.gap = px.max(0.0);
            }
        }
        _ => {}
    }
}

/// `border: none` and `border: 0` clear the width. Otherwise the first parsed
/// length and color in the shorthand set every side. Style keywords such as
/// `solid` are ignored by [`parse_border_side`].
fn resolve_css_vars(input: &str, vars: &HashMap<String, String>) -> String {
    let mut out = input.to_string();
    for _ in 0..8 {
        let lower = out.to_ascii_lowercase();
        let Some(pos) = lower.find("var(") else {
            break;
        };
        let after = &out[pos + 4..];
        let mut depth = 1usize;
        let mut close_rel = None;
        let mut comma_rel = None;
        for (idx, ch) in after.char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close_rel = Some(idx);
                        break;
                    }
                }
                ',' if depth == 1 && comma_rel.is_none() => {
                    comma_rel = Some(idx);
                }
                _ => {}
            }
        }
        let Some(close_idx) = close_rel else {
            return String::new();
        };
        let (var_name, fallback) = match comma_rel {
            Some(c) => (after[..c].trim(), Some(after[c + 1..close_idx].trim())),
            None => (after[..close_idx].trim(), None),
        };
        let replacement = if let Some(val) = vars.get(&var_name.to_ascii_lowercase()) {
            val.clone()
        } else if let Some(fb) = fallback {
            fb.to_string()
        } else {
            return String::new();
        };
        out.replace_range(pos..=(pos + 4 + close_idx), &replacement);
    }
    out
}

/// One grid line: `auto`, a non-zero integer, `span N`, `N span`, or `span`
/// (span 1). Line names are not supported and return `None`, as do 0 and a
/// span below 1. Numbers past [`MAX_GRID_LINE`] are clamped.
fn parse_grid_line(value: &str) -> Option<GridLine> {
    let tokens: Vec<&str> = value.split_whitespace().collect();
    let clamp = |n: i64| n.clamp(-i64::from(MAX_GRID_LINE), i64::from(MAX_GRID_LINE)) as i16;
    let int = |t: &str| t.parse::<i64>().ok();
    match tokens.as_slice() {
        [one] if one.eq_ignore_ascii_case("auto") => Some(GridLine::Auto),
        [one] if one.eq_ignore_ascii_case("span") => Some(GridLine::Span(1)),
        [one] => {
            let n = int(one)?;
            (n != 0).then(|| GridLine::Line(clamp(n)))
        }
        [a, b] if a.eq_ignore_ascii_case("span") || b.eq_ignore_ascii_case("span") => {
            let num = if a.eq_ignore_ascii_case("span") { b } else { a };
            let n = int(num)?;
            (n > 0).then(|| GridLine::Span(clamp(n)))
        }
        _ => None,
    }
}

/// `start` or `start / end` for `grid-column` and `grid-row`. A missing end is `auto`.
fn parse_grid_placement(value: &str) -> Option<GridPlacement> {
    let mut parts = value.split('/');
    let start = parse_grid_line(parts.next()?)?;
    let end = match parts.next() {
        Some(end) => parse_grid_line(end)?,
        None => GridLine::Auto,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(GridPlacement { start, end })
}

/// `grid-area: row-start / column-start / row-end / column-end` with numeric
/// lines or spans. Missing parts are `auto`. Area names are not supported.
/// Returns the row placement, then the column placement.
fn parse_grid_area(value: &str) -> Option<(GridPlacement, GridPlacement)> {
    let parts: Vec<&str> = value.split('/').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut lines = [GridLine::Auto; 4];
    for (slot, part) in lines.iter_mut().zip(&parts) {
        *slot = parse_grid_line(part)?;
    }
    Some((
        GridPlacement {
            start: lines[0],
            end: lines[2],
        },
        GridPlacement {
            start: lines[1],
            end: lines[3],
        },
    ))
}

/// One track: a length, a percentage, `Nfr`, `auto`, or `minmax(min, max)`.
/// `None` for anything else, including a flexible minimum such as `minmax(1fr, 2fr)`.
fn parse_single_grid_track(token: &str) -> Option<GridTrack> {
    let t = token.trim();
    let is_minmax = t
        .get(..7)
        .is_some_and(|head| head.eq_ignore_ascii_case("minmax("));
    if is_minmax && t.ends_with(')') && t.len() > 7 {
        let inner = &t[7..t.len() - 1];
        let (min_src, max_src) = inner.split_once(',')?;
        let min = parse_track_breadth(min_src)?;
        let max = parse_track_breadth(max_src)?;
        if matches!(min, TrackBreadth::Fr(_)) {
            return None;
        }
        return Some(GridTrack::MinMax(min, max));
    }
    Some(match parse_track_breadth(t)? {
        TrackBreadth::Px(px) => GridTrack::Px(px),
        TrackBreadth::Percent(pct) => GridTrack::Percent(pct),
        TrackBreadth::Fr(fr) => GridTrack::Fr(fr),
        TrackBreadth::Auto => GridTrack::Auto,
    })
}

/// A single track size: `auto`, `Nfr`, a pixel length, or a percentage.
/// Negative sizes are clamped to 0. A flexible size is at least `0.01fr`.
fn parse_track_breadth(token: &str) -> Option<TrackBreadth> {
    let t = token.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Some(TrackBreadth::Auto);
    }
    if let Some(fr_str) = strip_unit(t, "fr") {
        let fr = fr_str.parse::<f32>().ok()?;
        return (fr >= 0.0 && fr.is_finite()).then_some(TrackBreadth::Fr(fr.max(0.01)));
    }
    match parse_length(t)? {
        Size::Px(px) if px.is_finite() => Some(TrackBreadth::Px(px.max(0.0))),
        Size::Percent(pct) if pct.is_finite() => Some(TrackBreadth::Percent(pct.max(0.0))),
        _ => None,
    }
}

/// Track list for `grid-template-columns` or `grid-template-rows`.
///
/// Tokens are tracks or `repeat(N, tracks...)`. At most 12 tracks are kept;
/// later ones are dropped. `None` when a token does not parse or the list is empty.
fn parse_grid_template(value: &str) -> Option<([GridTrack; 12], u8)> {
    let mut tracks = [GridTrack::Fr(1.0); 12];
    let mut count = 0usize;
    for token in split_css_tokens(value) {
        let lower = token.to_ascii_lowercase();
        if let Some(inner) = lower
            .strip_prefix("repeat(")
            .and_then(|s| s.strip_suffix(')'))
        {
            let (rep_str, body) = inner.split_once(',')?;
            let reps = rep_str.trim().parse::<usize>().ok()?.min(12);
            let sub_tokens = split_css_tokens(body.trim());
            if sub_tokens.is_empty() {
                return None;
            }
            for _ in 0..reps {
                for &sub in &sub_tokens {
                    if count < 12 {
                        tracks[count] = parse_single_grid_track(sub)?;
                        count += 1;
                    }
                }
            }
        } else if count < 12 {
            tracks[count] = parse_single_grid_track(token)?;
            count += 1;
        }
    }
    (count > 0).then_some((tracks, count as u8))
}

fn parse_calc(expr: &str) -> Option<Size> {
    let mut normalized = String::with_capacity(expr.len() + 8);
    let bytes = expr.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let ch = b as char;
        if (ch == '+' || ch == '-') && i > 0 {
            let prev = bytes[i - 1] as char;
            let next = bytes.get(i + 1).copied().unwrap_or(b' ') as char;
            if prev.is_whitespace() && next.is_whitespace() {
                normalized.push(' ');
                normalized.push(ch);
                normalized.push(' ');
                continue;
            }
        }
        normalized.push(ch);
    }
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut px = 0.0f32;
    let mut percent = 0.0f32;
    let mut em = 0.0f32;
    let mut rem = 0.0f32;
    let mut sign = 1.0f32;
    let mut idx = 0usize;
    while idx < tokens.len() {
        let tok = tokens[idx];
        idx += 1;
        if tok == "+" {
            sign = 1.0;
            continue;
        }
        if tok == "-" {
            sign = -1.0;
            continue;
        }
        let mut scale = sign;
        let mut term_str = tok;
        if idx + 1 < tokens.len() && (tokens[idx] == "*" || tokens[idx] == "/") {
            let op = tokens[idx];
            let rhs = tokens[idx + 1];
            idx += 2;
            if let Ok(num) = term_str.parse::<f32>() {
                if op == "*" {
                    scale *= num;
                    term_str = rhs;
                } else {
                    return None;
                }
            } else if let Ok(num) = rhs.parse::<f32>() {
                if op == "*" {
                    scale *= num;
                } else if num != 0.0 {
                    scale /= num;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        let term = if term_str == "0" {
            Size::Px(0.0)
        } else if let Some(n) = term_str.strip_suffix("rem") {
            Size::Rem(n.trim().parse::<f32>().ok()?)
        } else if let Some(n) = term_str.strip_suffix("em") {
            Size::Em(n.trim().parse::<f32>().ok()?)
        } else if let Some(n) = term_str.strip_suffix("px") {
            Size::Px(n.trim().parse::<f32>().ok()?)
        } else if let Some(n) = term_str.strip_suffix('%') {
            Size::Percent(n.trim().parse::<f32>().ok()?)
        } else {
            return None;
        };
        match term {
            Size::Px(v) => px += v * scale,
            Size::Percent(v) => percent += v * scale,
            Size::Em(v) => em += v * scale,
            Size::Rem(v) => rem += v * scale,
            _ => return None,
        }
        sign = 1.0;
    }
    if percent == 0.0 && em == 0.0 && rem == 0.0 {
        Some(Size::Px(px))
    } else {
        Some(Size::Calc {
            px,
            percent,
            em,
            rem,
        })
    }
}

fn apply_border_shorthand(style: &mut ComputedStyle, value: &str) {
    if value.eq_ignore_ascii_case("none") || value == "0" {
        style.border = Edge::zero();
        return;
    }
    if let Some((width, color)) = parse_border_side(value) {
        style.border = Edge::all(width);
        if let Some(color) = color {
            style.border_color = color;
        }
    }
}

/// Split a border side into a pixel width and an optional color.
///
/// `None` when no length token parses. `solid`, `dashed`, `dotted`, and
/// `double` are skipped. The color is `None` when no color token parses.
fn parse_border_side(value: &str) -> Option<(f32, Option<Color>)> {
    let mut width = None;
    let mut color = None;
    for token in split_css_tokens(value) {
        if let Some(px) = parse_px(token) {
            width = Some(px);
        } else if !matches!(
            token.to_ascii_lowercase().as_str(),
            "solid" | "dashed" | "dotted" | "double"
        ) {
            if let Some(c) = Color::parse(token) {
                color = Some(c);
            }
        }
    }
    Some((width?, color))
}

/// Whitespace-separated tokens. Parentheses keep internal spaces, so
/// `rgb(0, 0, 0)` stays one token. The slices borrow `value`.
fn split_css_tokens(value: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut depth = 0usize;
    for (idx, ch) in value.char_indices() {
        match ch {
            '(' => {
                if start.is_none() {
                    start = Some(idx);
                }
                depth += 1;
            }
            ')' => {
                depth = depth.saturating_sub(1);
            }
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&value[s..idx]);
                }
            }
            _ => {
                if start.is_none() {
                    start = Some(idx);
                }
            }
        }
    }
    if let Some(s) = start {
        out.push(&value[s..]);
    }
    out
}

/// `auto` and `none` become [`Size::Auto`]. `px`, `%`, `em`, `rem`, `vw`,
/// `vh`, `vmin`, `vmax`, `ch`, and `0` keep their unit. Any other unit becomes
/// [`Size::Auto`].
fn parse_size(value: &str) -> Size {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") || value.eq_ignore_ascii_case("none") {
        Size::Auto
    } else {
        parse_length(value).unwrap_or(Size::Auto)
    }
}

/// `0`, or a number with a `px`, `%`, `em`, `rem`, `vw`, `vh`, `vmin`,
/// `vmax`, or `ch` suffix. `rem` is matched before `em`, and `vmin` / `vmax` before
/// `vw` / `vh`. Other units, including a bare number other than `0`, return
/// `None`.
fn parse_length(value: &str) -> Option<Size> {
    let value = value.trim();
    // `get` rather than an index: byte 5 can fall inside a multi-byte character.
    let is_calc = value
        .get(..5)
        .is_some_and(|head| head.eq_ignore_ascii_case("calc("));
    if value.len() > 5 && is_calc && value.ends_with(')') {
        return parse_calc(&value[5..value.len() - 1]);
    }
    let value = value.trim();
    if value == "0" {
        return Some(Size::Px(0.0));
    }
    if let Some(num) = value.strip_suffix("rem") {
        return num.trim().parse::<f32>().ok().map(Size::Rem);
    }
    if let Some(num) = value.strip_suffix("em") {
        return num.trim().parse::<f32>().ok().map(Size::Em);
    }
    if let Some(num) = value.strip_suffix("px") {
        return num.trim().parse::<f32>().ok().map(Size::Px);
    }
    if let Some(num) = strip_unit(value, "vmin") {
        return num.parse::<f32>().ok().map(Size::Vmin);
    }
    if let Some(num) = strip_unit(value, "vmax") {
        return num.parse::<f32>().ok().map(Size::Vmax);
    }
    if let Some(num) = strip_unit(value, "vh") {
        return num.parse::<f32>().ok().map(Size::Vh);
    }
    if let Some(num) = strip_unit(value, "vw") {
        return num.parse::<f32>().ok().map(Size::Vw);
    }
    if let Some(num) = strip_unit(value, "ch") {
        return num.parse::<f32>().ok().map(Size::Ch);
    }
    if let Some(num) = value.strip_suffix('%') {
        return num.trim().parse::<f32>().ok().map(Size::Percent);
    }
    None
}

fn strip_unit<'a>(value: &'a str, unit: &str) -> Option<&'a str> {
    if value.len() <= unit.len() {
        return None;
    }
    let split = value.len() - unit.len();
    if value.is_char_boundary(split) && value[split..].eq_ignore_ascii_case(unit) {
        Some(value[..split].trim())
    } else {
        None
    }
}

/// Resolve `font-size`. `%` and `em` use the parent font size. `rem` uses the
/// root font size. `px` and `0` are already pixels. Other units, including
/// viewport units and `ch`, return `None`.
fn parse_font_size(value: &str, parent_px: f32, root_px: f32) -> Option<f32> {
    Some(match parse_length(value)? {
        Size::Px(value) => value,
        Size::Em(value) => parent_px * value,
        Size::Rem(value) => root_px * value,
        Size::Percent(value) => parent_px * value / 100.0,
        Size::Vw(_) | Size::Vh(_) | Size::Vmin(_) | Size::Vmax(_) | Size::Ch(_) => return None,
        Size::Calc {
            px,
            percent,
            em,
            rem,
        } => px + (parent_px * percent / 100.0) + (parent_px * em) + (root_px * rem),
        Size::Auto => return None,
    })
}

/// One to four pixel lengths into an [`Edge`], using the CSS shorthand cycle.
/// `None` when a token is not a pixel length or there are more than four.
fn parse_edge(value: &str) -> Option<Edge> {
    let parts: Vec<f32> = split_css_tokens(value)
        .into_iter()
        .map(parse_px)
        .collect::<Option<_>>()?;
    Some(match parts.as_slice() {
        [all] => Edge::all(*all),
        [vertical, horizontal] => Edge {
            top: *vertical,
            right: *horizontal,
            bottom: *vertical,
            left: *horizontal,
        },
        [top, horizontal, bottom] => Edge {
            top: *top,
            right: *horizontal,
            bottom: *bottom,
            left: *horizontal,
        },
        [top, right, bottom, left] => Edge {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        },
        _ => return None,
    })
}

/// Parse a `transform` value: `none`, or up to [`MAX_TRANSFORM_FNS`]
/// functions separated by whitespace. Any bad function, unit, or argument
/// count rejects the whole value.
fn parse_transform(value: &str) -> Option<TransformList> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some(TransformList::NONE);
    }
    let mut fns = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let name = rest[..open].trim().to_ascii_lowercase();
        let close = open + rest[open..].find(')')?;
        let args: Vec<&str> = rest[open + 1..close]
            .split(|c: char| c == ',' || c.is_ascii_whitespace())
            .filter(|arg| !arg.is_empty())
            .collect();
        fns.push(parse_transform_fn(&name, &args)?);
        if fns.len() > MAX_TRANSFORM_FNS {
            return None;
        }
        rest = rest[close + 1..].trim_start();
    }
    TransformList::new(&fns).filter(|list| !list.is_none())
}

/// One transform function from its lowercase name and split arguments.
fn parse_transform_fn<'a>(name: &str, args: &[&'a str]) -> Option<TransformFn> {
    let one = |args: &[&'a str]| (args.len() == 1).then(|| args[0]);
    let length = |arg: &str| {
        parse_length(arg)
            .filter(|size| !matches!(size, Size::Calc { .. }) && size_number(*size).is_finite())
    };
    let zero = Size::Px(0.0);
    Some(match name {
        "translate" => match args {
            [x] => TransformFn::Translate(length(x)?, zero),
            [x, y] => TransformFn::Translate(length(x)?, length(y)?),
            _ => return None,
        },
        "translatex" => TransformFn::Translate(length(one(args)?)?, zero),
        "translatey" => TransformFn::Translate(zero, length(one(args)?)?),
        "scale" => match args {
            [s] => {
                let s = parse_scale_number(s)?;
                TransformFn::Scale(s, s)
            }
            [x, y] => TransformFn::Scale(parse_scale_number(x)?, parse_scale_number(y)?),
            _ => return None,
        },
        "scalex" => TransformFn::Scale(parse_scale_number(one(args)?)?, 1.0),
        "scaley" => TransformFn::Scale(1.0, parse_scale_number(one(args)?)?),
        "rotate" => TransformFn::Rotate(parse_angle(one(args)?)?),
        "skew" => match args {
            [x] => TransformFn::Skew(parse_angle(x)?, 0.0),
            [x, y] => TransformFn::Skew(parse_angle(x)?, parse_angle(y)?),
            _ => return None,
        },
        "skewx" => TransformFn::Skew(parse_angle(one(args)?)?, 0.0),
        "skewy" => TransformFn::Skew(0.0, parse_angle(one(args)?)?),
        "matrix" => {
            if args.len() != 6 {
                return None;
            }
            let mut m = [0.0f32; 6];
            for (slot, arg) in m.iter_mut().zip(args) {
                *slot = parse_finite(arg)?;
            }
            TransformFn::Matrix(m)
        }
        _ => return None,
    })
}

/// The number inside a single-unit [`Size`], for finiteness checks.
fn size_number(size: Size) -> f32 {
    match size {
        Size::Auto => 0.0,
        Size::Px(v)
        | Size::Percent(v)
        | Size::Em(v)
        | Size::Rem(v)
        | Size::Vw(v)
        | Size::Vh(v)
        | Size::Vmin(v)
        | Size::Vmax(v)
        | Size::Ch(v) => v,
        Size::Calc {
            px,
            percent,
            em,
            rem,
        } => px + percent + em + rem,
    }
}

/// A finite plain number.
fn parse_finite(value: &str) -> Option<f32> {
    value.trim().parse::<f32>().ok().filter(|v| v.is_finite())
}

/// A scale factor: a plain number, or a percentage where `50%` is `0.5`.
fn parse_scale_number(value: &str) -> Option<f32> {
    match value.trim().strip_suffix('%') {
        Some(pct) => parse_finite(pct).map(|p| p / 100.0),
        None => parse_finite(value),
    }
}

/// An angle in radians from `deg`, `rad`, `grad`, `turn`, or a bare `0`.
fn parse_angle(value: &str) -> Option<f32> {
    let value = value.trim();
    if value == "0" {
        return Some(0.0);
    }
    let lower = value.to_ascii_lowercase();
    // `grad` before `rad`, which it ends with.
    let units = [
        ("deg", std::f32::consts::PI / 180.0),
        ("grad", std::f32::consts::PI / 200.0),
        ("rad", 1.0),
        ("turn", std::f32::consts::TAU),
    ];
    let (num, per_unit) = units
        .iter()
        .find_map(|&(unit, per_unit)| lower.strip_suffix(unit).map(|n| (n, per_unit)))?;
    parse_finite(num)
        .map(|n| n * per_unit)
        .filter(|a| a.is_finite())
}

/// Parse `transform-origin`: one or two of a length, a percentage, or
/// `left`, `center`, `right`, `top`, `bottom`. A third value (the z
/// offset) must be a length and is ignored, since only 2D is drawn.
fn parse_transform_origin(value: &str) -> Option<TransformOrigin> {
    let parts: Vec<&str> = value.split_ascii_whitespace().collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    if let Some(z) = parts.get(2) {
        parse_length(z).filter(|size| matches!(size, Size::Px(_)))?;
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Axis {
        X,
        Y,
        Center,
        Length,
    }
    let term = |part: &str| -> Option<(Axis, Size)> {
        Some(match part.to_ascii_lowercase().as_str() {
            "left" => (Axis::X, Size::Percent(0.0)),
            "right" => (Axis::X, Size::Percent(100.0)),
            "top" => (Axis::Y, Size::Percent(0.0)),
            "bottom" => (Axis::Y, Size::Percent(100.0)),
            "center" => (Axis::Center, Size::Percent(50.0)),
            _ => {
                let size = parse_length(part)
                    .filter(|s| !matches!(s, Size::Calc { .. }) && size_number(*s).is_finite())?;
                (Axis::Length, size)
            }
        })
    };
    let (a_axis, a) = term(parts[0])?;
    let Some(second) = parts.get(1) else {
        return Some(match a_axis {
            Axis::Y => TransformOrigin {
                x: Size::Percent(50.0),
                y: a,
            },
            _ => TransformOrigin {
                x: a,
                y: Size::Percent(50.0),
            },
        });
    };
    let (b_axis, b) = term(second)?;
    // Keywords may name y first (`top left`); a length always pairs with
    // the axis its position implies.
    match (a_axis, b_axis) {
        (Axis::Y, Axis::Y) | (Axis::X, Axis::X) => None,
        (Axis::Y, Axis::X | Axis::Center) | (Axis::Center, Axis::X) => {
            Some(TransformOrigin { x: b, y: a })
        }
        (Axis::Y, Axis::Length) | (Axis::Length, Axis::X) => None,
        _ => Some(TransformOrigin { x: a, y: b }),
    }
}

/// Parse `0` or a number with a `px` suffix. `%`, `em`, `rem`, and other units
/// return `None`, so margin, padding, border, and `gap` leave the previous value.
fn parse_px(value: &str) -> Option<f32> {
    match parse_length(value) {
        Some(Size::Px(value)) => Some(value),
        _ => None,
    }
}

/// Parse `offset-x offset-y blur spread color`. At least two lengths are required.
/// Missing blur and spread are 0. A missing color is black at alpha 128.
/// `None` when fewer than two lengths parse. `inset` is not recognized.
fn parse_box_shadow(value: &str) -> Option<BoxShadow> {
    let mut lengths = Vec::new();
    let mut color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 128,
    };
    for token in split_css_tokens(value) {
        if let Some(px) = parse_px(token) {
            lengths.push(px);
        } else if let Some(c) = Color::parse(token) {
            color = c;
        }
    }
    if lengths.len() < 2 {
        return None;
    }
    Some(BoxShadow {
        offset_x: lengths[0],
        offset_y: lengths[1],
        blur_radius: lengths.get(2).copied().unwrap_or(0.0).max(0.0),
        spread_radius: lengths.get(3).copied().unwrap_or(0.0),
        color,
    })
}

/// Parse `linear-gradient(...)`. `None` if the function is not that name or
/// fewer than two color stops remain after the direction.
///
/// A leading `<angle>deg` sets the angle. `to right`, `to left`, `to top`,
/// and `to bottom` map to 90, 270, 0, and 180. Any other `to ` direction
/// becomes 135. With no direction the angle is 180 (downward). Only the first
/// and last color stops are kept. Stop positions are ignored.
fn parse_linear_gradient(value: &str) -> Option<LinearGradient> {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("linear-gradient(")
        .and_then(|s| s.strip_suffix(')'))?;

    let mut args = Vec::new();
    let mut start = 0usize;
    let mut depth = 0usize;
    for (idx, ch) in inner.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                args.push(inner[start..idx].trim());
                start = idx + 1;
            }
            _ => {}
        }
    }
    args.push(inner[start..].trim());
    if args.len() < 2 {
        return None;
    }

    let mut angle_deg = 180.0f32;
    let mut color_start_idx = 0usize;
    let first = args[0];
    if let Some(deg_str) = first.strip_suffix("deg") {
        if let Ok(deg) = deg_str.trim().parse::<f32>() {
            angle_deg = deg;
            color_start_idx = 1;
        }
    } else if first.starts_with("to ") {
        angle_deg = match first {
            "to right" => 90.0,
            "to left" => 270.0,
            "to top" => 0.0,
            "to bottom" => 180.0,
            _ => 135.0,
        };
        color_start_idx = 1;
    }

    if args.len() < color_start_idx + 2 {
        return None;
    }
    let parse_stop_color = |stop: &str| -> Option<Color> {
        let tokens = split_css_tokens(stop);
        Color::parse(tokens.first().copied()?)
    };
    let start_color = parse_stop_color(args[color_start_idx])?;
    let end_color = parse_stop_color(args[args.len() - 1])?;
    Some(LinearGradient {
        angle_deg,
        start_color,
        end_color,
    })
}

/// Split a selector list on commas outside brackets and quotes.
///
/// `None` when any selector in the list is invalid, so the caller drops the
/// whole rule. A selector that is valid but uses something this crate does
/// not match yet (a combinator other than space, an attribute selector, a
/// pseudo-class) is left out of the list and never matches, while the other
/// selectors in the rule still apply.
fn parse_selectors(source: &str) -> Option<Vec<Selector>> {
    if !selector_check::is_valid_selector_list(source) {
        return None;
    }
    Some(
        selector_check::split_selector_list(source)
            .into_iter()
            .filter_map(parse_selector)
            .collect(),
    )
}

/// Parse one selector. Compounds are split on whitespace, so the combinator
/// is always the descendant combinator. `None` when any compound fails or the
/// selector is empty. `>`, `+`, and `~` make the compound fail.
fn parse_selector(source: &str) -> Option<Selector> {
    let mut chain = Vec::new();
    for token in source.split_whitespace() {
        let compound = parse_compound_selector(token)?;
        chain.push(compound);
    }
    if chain.is_empty() {
        None
    } else {
        Some(Selector { chain })
    }
}

/// Parse one compound: `*`, a tag, and then `#id` or `.class` pieces.
///
/// `None` for an empty token, a bad character, or an empty identifier.
/// The tag is lowercased. Id and class text keep their case.
fn parse_compound_selector(token: &str) -> Option<CompoundSelector> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if token.eq_ignore_ascii_case(":root") {
        return Some(CompoundSelector {
            tag: Some("html".to_string()),
            ..CompoundSelector::default()
        });
    }
    if token == "*" {
        return Some(CompoundSelector {
            universal: true,
            ..CompoundSelector::default()
        });
    }
    let mut out = CompoundSelector::default();
    let mut chars = token.chars().peekable();
    let mut mode = 't'; // 't' = tag, '#' = id, '.' = class

    while chars.peek().is_some() {
        if let Some(&prefix @ ('#' | '.')) = chars.peek() {
            mode = prefix;
            chars.next();
        }
        let mut ident = String::new();
        while let Some(&ch) = chars.peek() {
            if ch == '#' || ch == '.' {
                break;
            }
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ident.push(ch);
                chars.next();
            } else {
                return None;
            }
        }
        if ident.is_empty() {
            return None;
        }
        match mode {
            't' => out.tag = Some(ident.to_ascii_lowercase()),
            '#' => out.id = Some(ident),
            '.' => out.classes.push(ident),
            _ => return None,
        }
        mode = ' ';
    }

    if out.is_empty() { None } else { Some(out) }
}

/// Parse `name: value` pairs separated by `;`.
///
/// Names are owned and lowercased. Values are owned and trimmed. A trailing
/// `!important` (any case, with optional space after the `!`) is taken off
/// the value and sets [`Declaration::important`]. A pair with an empty name,
/// or with an empty value once the flag is gone, is dropped. `source` is
/// borrowed.
pub fn parse_declarations(source: &str) -> Vec<Declaration> {
    source
        .split(';')
        .filter_map(|part| {
            let (name, value) = part.split_once(':')?;
            let name = name.trim().to_ascii_lowercase();
            let (value, important) = split_important(value.trim());
            if name.is_empty() || value.is_empty() {
                None
            } else {
                Some(Declaration {
                    name,
                    value: value.to_string(),
                    important,
                })
            }
        })
        .collect()
}

/// Split a trailing `!important` off `value`.
///
/// Only a `!` outside quotes and parentheses counts, so text such as
/// `"a !important"` or `url(x!important)` stays in the value. The flag
/// must be the last thing in the value: `red !important x` is returned
/// unchanged and not important, and fails later as an unknown value. The
/// returned value is trimmed.
fn split_important(value: &str) -> (&str, bool) {
    let mut bang = None;
    let mut quote = None;
    let mut depth = 0usize;
    let mut chars = value.char_indices();
    while let Some((idx, ch)) = chars.next() {
        match quote {
            Some(q) => {
                if ch == '\\' {
                    chars.next();
                } else if ch == q {
                    quote = None;
                }
            }
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '\\' => {
                    chars.next();
                }
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                '!' if depth == 0 => bang = Some(idx),
                _ => {}
            },
        }
    }
    let Some(bang) = bang else {
        return (value, false);
    };
    if quote.is_some() || !value[bang + 1..].trim().eq_ignore_ascii_case("important") {
        return (value, false);
    }
    (value[..bang].trim(), true)
}

/// Copy `input` with `/* ... */` comments replaced by one space.
/// An unclosed comment runs to the end of the string. The result is owned.
fn strip_comments(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(inner) = chars.next() {
                if inner == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse the inside of `rgb()` or `rgba()`. Channels are integers 0–255.
/// When `has_alpha` is set, a dotted alpha of at most 1 is a fraction.
/// `None` when the channel count or a parse fails.
fn parse_rgb(inner: &str, has_alpha: bool) -> Option<Color> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if (!has_alpha && parts.len() != 3) || (has_alpha && parts.len() != 4) {
        return None;
    }
    let r: u8 = parts[0].parse().ok()?;
    let g: u8 = parts[1].parse().ok()?;
    let b: u8 = parts[2].parse().ok()?;
    let a = if has_alpha {
        let raw: f32 = parts[3].parse().ok()?;
        if raw <= 1.0 && parts[3].contains('.') {
            (raw.clamp(0.0, 1.0) * 255.0).round() as u8
        } else {
            raw.clamp(0.0, 255.0).round() as u8
        }
    } else {
        255
    };
    Some(Color { r, g, b, a })
}

/// Parse 3, 4, 6, or 8 hex digits. Short digits are expanded. `None` otherwise.
fn parse_hex(hex: &str) -> Option<Color> {
    let chars: Vec<char> = hex.chars().collect();
    let component = |text: &[char]| -> Option<u8> {
        let value = u8::from_str_radix(&text.iter().collect::<String>(), 16).ok()?;
        Some(value)
    };
    let expand = |ch: char| -> Option<u8> {
        let nibble = ch.to_digit(16)? as u8;
        Some(nibble * 16 + nibble)
    };
    match chars.as_slice() {
        [r, g, b] => Some(Color {
            r: expand(*r)?,
            g: expand(*g)?,
            b: expand(*b)?,
            a: 255,
        }),
        [r, g, b, a] => Some(Color {
            r: expand(*r)?,
            g: expand(*g)?,
            b: expand(*b)?,
            a: expand(*a)?,
        }),
        [r1, r2, g1, g2, b1, b2] => Some(Color {
            r: component(&[*r1, *r2])?,
            g: component(&[*g1, *g2])?,
            b: component(&[*b1, *b2])?,
            a: 255,
        }),
        [r1, r2, g1, g2, b1, b2, a1, a2] => Some(Color {
            r: component(&[*r1, *r2])?,
            g: component(&[*g1, *g2])?,
            b: component(&[*b1, *b2])?,
            a: component(&[*a1, *a2])?,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Cascade order, inheritance, media queries, and a few declaration parsers.

    use super::*;
    use anvil_dom::parse_html;

    #[test]
    fn later_type_rule_beats_an_earlier_universal() {
        let doc = parse_html("<p>Hi</p>");
        let sheet = parse_css("* { color: red } p { color: #00ff00; font-size: 20px; }");
        let styles = compute_styles(&doc, &sheet);
        let paragraph = doc
            .children(doc.body())
            .iter()
            .copied()
            .find(|id| doc.get(*id).data().tag() == Some("p"))
            .unwrap();
        let style = styles.get(paragraph);
        assert_eq!(
            style.color,
            Color {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            }
        );
        assert_eq!(style.font_size, 20.0);
        assert_eq!(style.display, Display::Block);
    }

    #[test]
    fn color_inherits_and_head_stays_hidden() {
        let doc = parse_html("<style>div { color: #ff0000 }</style><div><span>Hi</span></div>");
        let styles = compute_styles(&doc, &parse_css(&doc.embedded_css()));
        let style_node = doc
            .children(doc.head())
            .iter()
            .copied()
            .find(|id| doc.get(*id).data().tag() == Some("style"))
            .unwrap();
        assert_eq!(styles.get(style_node).display, Display::None);
        let div = doc.children(doc.body())[0];
        let span = doc.children(div)[0];
        assert_eq!(doc.get(span).data().tag(), Some("span"));
        assert_eq!(styles.get(span).color, Color::parse("#ff0000").unwrap());
    }

    #[test]
    fn links_get_a_default_color_that_author_rules_can_override() {
        let doc = parse_html("<p><a href=\"next.html\">Go</a></p>");
        let default_styles = compute_styles(&doc, &parse_css("p { color: #111111 }"));
        let p = doc.children(doc.body())[0];
        let a = doc.children(p)[0];
        assert_eq!(
            default_styles.get(a).color,
            Color {
                r: 0,
                g: 102,
                b: 204,
                a: 255
            }
        );
        assert!(default_styles.get(a).underline);

        let custom_styles = compute_styles(
            &doc,
            &parse_css("a { color: #e8c37a; text-decoration: none; }"),
        );
        assert_eq!(custom_styles.get(a).color, Color::parse("#e8c37a").unwrap());
        assert!(!custom_styles.get(a).underline);
    }

    #[test]
    fn matches_id_class_descendant_and_inline_styles() {
        let doc = parse_html(
            "<div id=\"card\" class=\"panel\"><p class=\"lead\" style=\"padding-left: 14px\">Hi</p></div>",
        );
        let sheet = parse_css(
            ".panel { border: 2px solid rgb(10, 20, 30); display: flex; gap: 12px; }
             #card .lead { color: #123456; }
             p { color: #ffffff; }",
        );
        let styles = compute_styles(&doc, &sheet);
        let card = doc.get_element_by_id("card").unwrap();
        let lead = doc.query_selector(".lead").unwrap();
        assert_eq!(styles.get(card).display, Display::Flex);
        assert_eq!(styles.get(card).gap, 12.0);
        assert_eq!(styles.get(card).border, Edge::all(2.0));
        assert_eq!(
            styles.get(card).border_color,
            Color {
                r: 10,
                g: 20,
                b: 30,
                a: 255
            }
        );
        assert_eq!(styles.get(lead).color, Color::parse("#123456").unwrap());
        assert_eq!(styles.get(lead).padding.left, 14.0);
    }

    #[test]
    fn parses_positioned_layout_z_index_and_overflow() {
        let doc = parse_html("<div id=\"modal\"><input id=\"q\" type=\"text\"></div>");
        let sheet = parse_css(
            "#modal { position: fixed; top: 12px; left: 24px; z-index: 10; overflow: hidden; }",
        );
        let styles = compute_styles(&doc, &sheet);
        let modal = doc.get_element_by_id("modal").unwrap();
        let q = doc.get_element_by_id("q").unwrap();
        assert_eq!(styles.get(modal).position, Position::Fixed);
        assert_eq!(styles.get(modal).top, Size::Px(12.0));
        assert_eq!(styles.get(modal).left, Size::Px(24.0));
        assert_eq!(styles.get(modal).z_index, 10);
        assert_eq!(styles.get(modal).overflow, Overflow::Hidden);
        assert_eq!(styles.get(q).border_radius, 4.0);
    }

    #[test]
    fn parses_box_shadow_and_linear_gradient() {
        let doc = parse_html("<div id=\"card\"></div>");
        let sheet = parse_css(
            "#card { background: linear-gradient(135deg, #3a342c, #25221e); box-shadow: 0px 4px 12px rgba(0, 0, 0, 160); }",
        );
        let styles = compute_styles(&doc, &sheet);
        let card = doc.get_element_by_id("card").unwrap();
        let grad = styles.get(card).background_gradient.expect("gradient");
        assert_eq!(grad.angle_deg, 135.0);
        let shadow = styles.get(card).box_shadow.expect("box-shadow");
        assert_eq!(shadow.offset_y, 4.0);
        assert_eq!(shadow.blur_radius, 12.0);
    }

    #[test]
    fn evaluates_media_queries_across_viewport_widths_and_color_schemes() {
        let doc = parse_html("<div id=\"box\" class=\"responsive\">Text</div>");
        let sheet = parse_css(
            ".responsive { width: 800px; color: #111111; }
             @media (max-width: 600px) {
               .responsive { width: 320px; }
             }
             @media (prefers-color-scheme: dark) {
               .responsive { color: #e8c37a; }
             }
             @media (prefers-color-scheme: light) {
               .responsive { color: #ff0000; }
             }",
        );
        let box_id = doc.get_element_by_id("box").unwrap();

        let desktop = compute_styles_with_viewport(&doc, &sheet, 960.0);
        assert_eq!(desktop.get(box_id).width, Size::Px(800.0));
        assert_eq!(desktop.get(box_id).color, Color::parse("#e8c37a").unwrap());

        let mobile = compute_styles_with_viewport(&doc, &sheet, 480.0);
        assert_eq!(mobile.get(box_id).width, Size::Px(320.0));
        assert_eq!(mobile.get(box_id).color, Color::parse("#e8c37a").unwrap());
    }

    #[test]
    fn keeps_px_percent_em_rem_and_viewport_units() {
        let doc = parse_html(
            "<div id=\"box\"><div id=\"child\"></div><p hidden>No</p><p id=\"shown\" hidden style=\"display:block\">Yes</p></div>",
        );
        let sheet = parse_css(
            "html { font-size: 20px; }
             #box { font-size: 2rem; width: 50%; height: 1.5em; flex-wrap: wrap; }
             #child { width: 10vw; height: 20vh; max-width: 4rem; top: 25%; left: 20ch; font-size: 50%; flex-grow: 2; display: inline-block; }",
        );
        let styles = compute_styles_for_viewport(&doc, &sheet, 400.0, 200.0);
        let box_id = doc.get_element_by_id("box").unwrap();
        let child = doc.get_element_by_id("child").unwrap();
        assert_eq!(styles.root_font_px(), 20.0);
        assert_eq!(styles.viewport_width(), 400.0);
        assert_eq!(styles.viewport_height(), 200.0);
        assert_eq!(styles.get(box_id).font_size, 40.0);
        assert_eq!(styles.get(box_id).width, Size::Percent(50.0));
        assert_eq!(styles.get(box_id).height, Size::Em(1.5));
        assert_eq!(styles.get(box_id).flex_wrap, FlexWrap::Wrap);
        assert_eq!(styles.get(child).width, Size::Vw(10.0));
        assert_eq!(styles.get(child).height, Size::Vh(20.0));
        assert_eq!(styles.get(child).max_width, Size::Rem(4.0));
        assert_eq!(styles.get(child).top, Size::Percent(25.0));
        assert_eq!(styles.get(child).left, Size::Ch(20.0));
        assert_eq!(styles.get(child).font_size, 20.0);
        assert_eq!(styles.get(child).flex_grow, 2.0);
        assert_eq!(styles.get(child).display, Display::Block);
        let hidden = doc
            .children(box_id)
            .iter()
            .copied()
            .find(|id| {
                doc.get(*id).data().tag() == Some("p") && doc.text_content(*id).contains("No")
            })
            .unwrap();
        assert_eq!(styles.get(hidden).display, Display::None);
        let shown = doc.get_element_by_id("shown").unwrap();
        assert_eq!(styles.get(shown).display, Display::Block);
        assert_eq!(Size::Percent(50.0).used_px(200.0, 16.0, 16.0), Some(100.0));
        assert_eq!(Size::Em(2.0).used_px(0.0, 30.0, 16.0), Some(60.0));
        assert_eq!(Size::Rem(1.5).used_px(0.0, 30.0, 20.0), Some(30.0));
        assert_eq!(Size::Vw(50.0).used_px(200.0, 16.0, 16.0), None);
    }

    #[test]
    fn resolves_custom_properties_calc_lengths_and_grid_tracks() {
        let doc = parse_html("<div id=\"grid\"><div id=\"card\"></div></div>");
        let sheet = parse_css(
            ":root { --accent: #ff6b24; --gap-px: 16px; }
             #grid { display: grid; grid-template-columns: 120px repeat(2, 1fr); gap: var(--gap-px); }
             #card { color: var(--accent); background: var(--missing, #112233); width: calc(100% - 24px); padding: calc(10px + 6px); }",
        );
        let styles = compute_styles(&doc, &sheet);
        let grid_id = doc.get_element_by_id("grid").unwrap();
        let card_id = doc.get_element_by_id("card").unwrap();

        assert_eq!(styles.get(grid_id).display, Display::Grid);
        assert_eq!(styles.get(grid_id).gap, 16.0);
        assert_eq!(
            styles.get(grid_id).grid_tracks(),
            &[GridTrack::Px(120.0), GridTrack::Fr(1.0), GridTrack::Fr(1.0)]
        );
        assert_eq!(styles.get(card_id).color, Color::parse("#ff6b24").unwrap());
        assert_eq!(
            styles.get(card_id).background,
            Some(Color::parse("#112233").unwrap())
        );
        assert_eq!(styles.get(card_id).padding, Edge::all(16.0));
        assert_eq!(
            styles.get(card_id).width.used_px(300.0, 16.0, 16.0),
            Some(276.0)
        );
    }

    #[test]
    fn parses_grid_row_tracks_minmax_and_auto_rows() {
        let doc = parse_html("<div id=\"g\"></div><div id=\"h\"></div>");
        let sheet = parse_css(
            "#g { grid-template-rows: 20px auto 1fr repeat(2, minmax(10px, 2fr)) 25%; grid-auto-rows: minmax(5px, auto); }
             #h { grid-template-rows: minmax(1fr, 2fr); grid-template-columns: auto 10px; grid-auto-rows: bogus; }",
        );
        let styles = compute_styles(&doc, &sheet);
        let g = doc.get_element_by_id("g").unwrap();
        let h = doc.get_element_by_id("h").unwrap();
        assert_eq!(
            styles.get(g).grid_row_tracks(),
            &[
                GridTrack::Px(20.0),
                GridTrack::Auto,
                GridTrack::Fr(1.0),
                GridTrack::MinMax(TrackBreadth::Px(10.0), TrackBreadth::Fr(2.0)),
                GridTrack::MinMax(TrackBreadth::Px(10.0), TrackBreadth::Fr(2.0)),
                GridTrack::Percent(25.0),
            ]
        );
        assert_eq!(
            styles.get(g).grid_auto_rows,
            GridTrack::MinMax(TrackBreadth::Px(5.0), TrackBreadth::Auto)
        );
        // A flexible minimum is invalid, so the declaration is dropped.
        assert!(styles.get(h).grid_row_tracks().is_empty());
        assert_eq!(styles.get(h).grid_auto_rows, GridTrack::Auto);
        assert_eq!(
            styles.get(h).grid_tracks(),
            &[GridTrack::Auto, GridTrack::Px(10.0)]
        );
    }

    #[test]
    fn parses_grid_lines_spans_and_areas() {
        let doc = parse_html(
            "<div id=\"a\"></div><div id=\"b\"></div><div id=\"c\"></div><div id=\"d\"></div>",
        );
        let sheet = parse_css(
            "#a { grid-column: 2 / span 3; grid-row: span 2; }
             #b { grid-area: 1 / -1 / 3; grid-column-end: 4 span; }
             #c { grid-column: 0; grid-row: name / 2; grid-row-start: 5000; }
             #d { grid-column: span 0; grid-row: 1 / 2 / 3; grid-area: 2 / 3 / 4 / 5 / 6; }",
        );
        let styles = compute_styles(&doc, &sheet);
        let get = |id: &str| *styles.get(doc.get_element_by_id(id).unwrap());
        let place = |start, end| GridPlacement { start, end };
        assert_eq!(
            get("a").grid_column,
            place(GridLine::Line(2), GridLine::Span(3))
        );
        assert_eq!(get("a").grid_row, place(GridLine::Span(2), GridLine::Auto));
        assert_eq!(
            get("b").grid_row,
            place(GridLine::Line(1), GridLine::Line(3))
        );
        assert_eq!(
            get("b").grid_column,
            place(GridLine::Line(-1), GridLine::Span(4))
        );
        // Line 0 and named lines are invalid; a huge line is clamped.
        assert_eq!(get("c").grid_column, GridPlacement::default());
        assert_eq!(
            get("c").grid_row,
            place(GridLine::Line(MAX_GRID_LINE), GridLine::Auto)
        );
        assert_eq!(get("d").grid_column, GridPlacement::default());
        assert_eq!(get("d").grid_row, GridPlacement::default());
    }

    #[test]
    fn parses_transform_lists_and_origins() {
        let doc = parse_html(
            "<div id=\"a\"></div><div id=\"b\"></div><div id=\"c\"></div><div id=\"d\"></div>",
        );
        let sheet = parse_css(
            "#a { transform: translate(10px, 50%) scale(2) rotate(0.5turn) skewY(0);
                  transform-origin: right top; }
             #b { transform: translateX(1em) translateY(-3px) scaleX(150%) scaleY(-1)
                  matrix(1, 0, 0, 1, 5, 6) skew(90deg, 1rad) rotate(200grad);
                  transform-origin: 10px; }
             #c { transform: rotate(45deg); transform: rotate(45); transform-origin: 0 bottom 0;
                  transform-origin: left 5px left; }
             #d { transform: scale(1) scale(1) scale(1) scale(1) scale(1) scale(1) scale(1);
                  transform: none; transform-origin: center left; }",
        );
        let styles = compute_styles(&doc, &sheet);
        let get = |id: &str| *styles.get(doc.get_element_by_id(id).unwrap());
        use std::f32::consts::PI;
        assert_eq!(
            get("a").transform.functions(),
            [
                TransformFn::Translate(Size::Px(10.0), Size::Percent(50.0)),
                TransformFn::Scale(2.0, 2.0),
                TransformFn::Rotate(PI),
                TransformFn::Skew(0.0, 0.0),
            ]
        );
        assert_eq!(
            get("a").transform_origin,
            TransformOrigin {
                x: Size::Percent(100.0),
                y: Size::Percent(0.0)
            }
        );
        assert_eq!(
            get("b").transform.functions(),
            [
                TransformFn::Translate(Size::Em(1.0), Size::Px(0.0)),
                TransformFn::Translate(Size::Px(0.0), Size::Px(-3.0)),
                TransformFn::Scale(1.5, 1.0),
                TransformFn::Scale(1.0, -1.0),
                TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 5.0, 6.0]),
                TransformFn::Skew(PI / 2.0, 1.0),
                TransformFn::Rotate(PI),
            ]
        );
        assert_eq!(
            get("b").transform_origin,
            TransformOrigin {
                x: Size::Px(10.0),
                y: Size::Percent(50.0)
            }
        );
        // A bare number is not an angle, so the second rotate is dropped;
        // two horizontal keywords are invalid, so the 3-value origin stays.
        assert_eq!(
            get("c").transform.functions(),
            [TransformFn::Rotate(PI / 4.0)]
        );
        assert_eq!(
            get("c").transform_origin,
            TransformOrigin {
                x: Size::Px(0.0),
                y: Size::Percent(100.0)
            }
        );
        // `center left` puts the keyword on its own axis.
        assert!(get("d").transform.is_none());
        assert_eq!(
            get("d").transform_origin,
            TransformOrigin {
                x: Size::Percent(0.0),
                y: Size::Percent(50.0)
            }
        );
        // Lists longer than the limit, unknown functions, and broken syntax
        // are rejected whole.
        let ten = "scale(1) ".repeat(MAX_TRANSFORM_FNS + 1);
        for bad in [
            ten.as_str(),
            "rotate(10deg) spin(3)",
            "translate(1px, 2px, 3px)",
            "matrix(1, 0, 0, 1, 0)",
            "translate(calc(1px + 2px))",
            "scale(inf)",
            "rotate(10deg",
            "(1)",
            "translate(1)",
        ] {
            assert_eq!(parse_transform(bad), None, "{bad}");
        }
        assert_eq!(parse_transform_origin("top 10px"), None);
        assert_eq!(parse_transform_origin("10px left"), None);
        assert_eq!(parse_transform_origin("top bottom"), None);
        assert_eq!(parse_transform_origin("1px 2px 3%"), None);
    }

    #[test]
    fn parses_opacity_and_transition_lists() {
        let doc = parse_html("<div id=\"a\"></div><div id=\"b\"></div><div id=\"c\"></div>");
        let sheet = parse_css(
            "#a { opacity: 40%; transition: opacity 1s ease-in, transform 250ms steps(4, jump-none) -50ms; }
             #b { opacity: 2; transition-property: color, width; transition-duration: 2s;
                  transition-timing-function: cubic-bezier(0.1, -1, 0.9, 2), linear;
                  transition-delay: 10ms, 20ms, 30ms; transition: bogus 1s 2s 3s; }
             #c { opacity: -1; opacity: nope; transition: 1s; transition-property: none;
                  transition-duration: -1s; transition-timing-function: cubic-bezier(2, 0, 0, 1); }",
        );
        let styles = compute_styles(&doc, &sheet);
        let get = |id: &str| *styles.get(doc.get_element_by_id(id).unwrap());
        assert_eq!(get("a").opacity, 0.4);
        assert_eq!(get("b").opacity, 1.0);
        assert_eq!(get("c").opacity, 0.0);
        let a = get("a").transitions;
        assert_eq!(
            a.properties(),
            [AnimProperty::Opacity, AnimProperty::Transform]
        );
        assert_eq!(
            a.spec_for(AnimProperty::Opacity),
            Some(TransitionSpec {
                duration_ms: 1000.0,
                delay_ms: 0.0,
                timing: TimingFunction::CubicBezier(0.42, 0.0, 1.0, 1.0),
            })
        );
        assert_eq!(
            a.spec_for(AnimProperty::Transform),
            Some(TransitionSpec {
                duration_ms: 250.0,
                delay_ms: -50.0,
                timing: TimingFunction::Steps(4, StepPosition::None),
            })
        );
        assert_eq!(a.spec_for(AnimProperty::Color), None);
        // Short lists repeat; the bad shorthand (three times) is ignored.
        let b = get("b").transitions;
        assert_eq!(
            b.spec_for(AnimProperty::Width),
            Some(TransitionSpec {
                duration_ms: 2000.0,
                delay_ms: 20.0,
                timing: TimingFunction::Linear,
            })
        );
        assert_eq!(
            b.spec_for(AnimProperty::Color).map(|s| s.timing),
            Some(TimingFunction::CubicBezier(0.1, -1.0, 0.9, 2.0))
        );
        // `none` turns everything off; the bad duration and curve are ignored.
        let c = get("c").transitions;
        assert!(c.properties().is_empty());
        assert_eq!(c.spec_for(AnimProperty::Opacity), None);
    }

    #[test]
    fn timing_functions_follow_their_curves() {
        let f = |s: &str| TimingFunction::parse(s).unwrap();
        assert_eq!(f("linear").eval(0.25), 0.25);
        // The curve constants are f32, so symmetric curves are off by ~1e-8.
        assert!((f("ease-in-out").eval(0.5) - 0.5).abs() < 1e-6);
        assert!((f("ease").eval(0.5) - 0.802_403_4).abs() < 1e-5);
        assert!((f("ease-in").eval(0.5) - 0.315_356_2).abs() < 1e-5);
        assert!((f("ease-out").eval(0.5) - 0.684_643_8).abs() < 1e-5);
        assert!((f("cubic-bezier(0, 0, 1, 1)").eval(0.3) - 0.3).abs() < 1e-6);
        assert_eq!(f("ease").eval(-1.0), 0.0);
        assert_eq!(f("ease").eval(f64::NAN), 0.0);
        assert_eq!(f("ease").eval(2.0), 1.0);
        assert_eq!(f("steps(4)").eval(0.3), 0.25);
        assert_eq!(f("steps(4, start)").eval(0.3), 0.5);
        assert_eq!(f("steps(5, jump-none)").eval(0.5), 0.5);
        assert_eq!(f("steps(3, jump-both)").eval(0.5), 0.5);
        assert_eq!(f("step-end").eval(0.99), 0.0);
        assert_eq!(f("step-start").eval(0.0), 1.0);
        assert_eq!(f("steps(2)").eval(1.0), 1.0);
        for bad in [
            "steps(0)",
            "steps(1, jump-none)",
            "steps(2, middle)",
            "cubic-bezier(1.5, 0, 0, 1)",
            "cubic-bezier(0, 0, 1)",
            "bounce",
        ] {
            assert_eq!(TimingFunction::parse(bad), None, "{bad}");
        }
    }

    /// Restyle `doc` at `now` and return `id`'s style.
    fn frame(
        animator: &mut Animator,
        doc: &Document,
        sheet: &Stylesheet,
        now: f64,
        id: &str,
    ) -> ComputedStyle {
        animator.tick(now);
        let styles = compute_styles_animated(doc, sheet, 800.0, 600.0, animator);
        *styles.get(doc.get_element_by_id(id).unwrap())
    }

    #[test]
    fn runs_transitions_on_an_injected_clock() {
        let mut doc = parse_html("<div id=\"a\"><p id=\"kid\">x</p></div>");
        let sheet = parse_css(
            "#a { opacity: 1; color: #ff0000; width: 10px;
                  transition: opacity 100ms linear, color 100ms linear 50ms, width 100ms linear; }
             #kid { transition: opacity 1s linear; }",
        );
        let clock = ManualClock::new();
        let mut animator = Animator::new();
        let a = doc.get_element_by_id("a").unwrap();
        // The first pass only records the style.
        let style = frame(&mut animator, &doc, &sheet, clock.now_ms(), "a");
        assert_eq!(style.opacity, 1.0);
        assert!(!animator.is_animating());

        doc.set_attribute(a, "style", "opacity: 0; color: #0000ff; width: 50%");
        clock.set(1000.0);
        let style = frame(&mut animator, &doc, &sheet, clock.now_ms(), "a");
        assert_eq!(style.opacity, 1.0);
        assert_eq!(
            style.width,
            Size::Calc {
                px: 10.0,
                percent: 0.0,
                em: 0.0,
                rem: 0.0
            }
        );
        assert!(animator.is_animating());

        clock.advance(25.0);
        assert!(animator.tick_clock(&clock));
        let style = frame(&mut animator, &doc, &sheet, clock.now_ms(), "a");
        assert_eq!(style.opacity, 0.75);
        assert_eq!(
            style.width,
            Size::Calc {
                px: 7.5,
                percent: 12.5,
                em: 0.0,
                rem: 0.0
            }
        );
        // Color waits out its 50ms delay at the old value.
        assert_eq!(
            style.color,
            Color {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );

        clock.advance(75.0);
        let style = frame(&mut animator, &doc, &sheet, clock.now_ms(), "a");
        assert_eq!(style.opacity, 0.0);
        assert_eq!(style.width, Size::Percent(50.0));
        assert_eq!(
            style.color,
            Color {
                r: 128,
                g: 0,
                b: 128,
                a: 255
            }
        );
        // The child inherits the moving color.
        let kid = frame(&mut animator, &doc, &sheet, clock.now_ms(), "kid");
        assert_eq!(kid.color, style.color);

        assert!(!animator.tick(1150.0));
        let style = frame(&mut animator, &doc, &sheet, 1150.0, "a");
        assert_eq!(
            style.color,
            Color {
                r: 0,
                g: 0,
                b: 255,
                a: 255
            }
        );
        assert!(!animator.is_animating());
        // Time does not run backward.
        animator.tick(5.0);
        assert_eq!(animator.now_ms(), 1150.0);
    }

    #[test]
    fn interrupted_transitions_restart_from_the_current_value() {
        let mut doc = parse_html("<div id=\"a\"></div>");
        let sheet = parse_css("#a { transition: transform 100ms linear, height 100ms linear; }");
        let a = doc.get_element_by_id("a").unwrap();
        let mut animator = Animator::new();
        frame(&mut animator, &doc, &sheet, 0.0, "a");
        doc.set_attribute(a, "style", "transform: translateX(100px); height: 20px");
        frame(&mut animator, &doc, &sheet, 0.0, "a");
        let style = frame(&mut animator, &doc, &sheet, 50.0, "a");
        assert_eq!(
            style.transform.functions(),
            [TransformFn::Translate(Size::Px(50.0), Size::Px(0.0))]
        );
        // `auto` to a length does not blend, so height jumps.
        assert_eq!(style.height, Size::Px(20.0));
        // Going back starts at 50px and takes the full duration.
        doc.set_attribute(a, "style", "transform: none");
        frame(&mut animator, &doc, &sheet, 50.0, "a");
        let style = frame(&mut animator, &doc, &sheet, 100.0, "a");
        assert_eq!(
            style.transform.functions(),
            [TransformFn::Translate(Size::Px(25.0), Size::Px(0.0))]
        );
        // Lists of different shapes blend as matrices.
        doc.set_attribute(a, "style", "transform: rotate(0deg); transition: none");
        frame(&mut animator, &doc, &sheet, 300.0, "a");
        doc.set_attribute(a, "style", "transform: scale(2)");
        frame(&mut animator, &doc, &sheet, 300.0, "a");
        let style = frame(&mut animator, &doc, &sheet, 350.0, "a");
        assert_eq!(
            style.transform.functions(),
            [TransformFn::Matrix([1.5, 0.0, 0.0, 1.5, 0.0, 0.0])]
        );
        // A hidden element starts nothing, and removed elements are forgotten.
        doc.set_attribute(a, "style", "display: none; transform: scale(3)");
        frame(&mut animator, &doc, &sheet, 500.0, "a");
        assert!(!animator.is_animating());
        doc.set_attribute(a, "style", "transform: scale(4)");
        frame(&mut animator, &doc, &sheet, 500.0, "a");
        assert!(!animator.is_animating());
        let root = doc.body();
        doc.set_attribute(a, "style", "transform: scale(1)");
        doc.remove_child(root, a).unwrap();
        frame_all(&mut animator, &doc, &sheet, 600.0);
        assert!(!animator.is_animating());
    }

    #[test]
    fn far_apart_ends_blend_exactly_at_both_ends() {
        let (a, b) = (AnimValue::Number(1e30), AnimValue::Number(-370.0));
        assert_eq!(interpolate(a, b, 0.0), Some(a));
        assert_eq!(interpolate(a, b, 1.0), Some(b));
        assert_eq!(interpolate(b, a, 0.0), Some(b));
        assert_eq!(interpolate(b, a, 1.0), Some(a));
        let (a, b) = (
            AnimValue::Length(Size::Px(1e30)),
            AnimValue::Length(Size::Px(-370.0)),
        );
        assert_eq!(interpolate(a, b, 0.0), Some(a));
        assert_eq!(interpolate(a, b, 1.0), Some(b));

        // A running transition starts on the old value and reaches the new one.
        let mut doc = parse_html("<div id=\"a\"></div>");
        let sheet = parse_css("#a { margin-left: 1e30px; transition: margin-left 100ms linear; }");
        let a = doc.get_element_by_id("a").unwrap();
        let mut animator = Animator::new();
        let style = frame(&mut animator, &doc, &sheet, 0.0, "a");
        assert_eq!(style.margin.left, 1e30);
        doc.set_attribute(a, "style", "margin-left: -370px");
        let style = frame(&mut animator, &doc, &sheet, 0.0, "a");
        assert_eq!(style.margin.left, 1e30);
        assert!(animator.is_animating());
        let style = frame(&mut animator, &doc, &sheet, 100.0, "a");
        assert_eq!(style.margin.left, -370.0);
        assert!(!animator.is_animating());
    }

    /// Restyle `doc` at `now` without reading any element.
    fn frame_all(animator: &mut Animator, doc: &Document, sheet: &Stylesheet, now: f64) {
        animator.tick(now);
        compute_styles_animated(doc, sheet, 800.0, 600.0, animator);
    }

    #[test]
    fn parses_keyframes_and_animation_lists() {
        let sheet = parse_css(
            "@keyframes slide { from { margin-left: 0 } 50%, 75% { margin-left: 10px }
                                to { opacity: 0.5 } 120% { color: red } bogus { color: red } }
             @media (min-width: 1px) { @keyframes 'quoted' { 0% { opacity: 0 } } }
             @keyframes none { from { opacity: 0 } }
             @keyframes two words { from { opacity: 0 } }
             div { color: blue; }",
        );
        assert_eq!(sheet.rules.len(), 1);
        let names: Vec<&str> = sheet.keyframes.iter().map(|k| k.name.as_str()).collect();
        assert_eq!(names, ["slide", "quoted"]);
        let offsets: Vec<u32> = sheet.keyframes[0]
            .frames
            .iter()
            .map(|f| f.offset_millionths)
            .collect();
        assert_eq!(offsets, [0, 500_000, 750_000, 1_000_000]);
        assert_eq!(
            sheet.keyframes[0].frames[1].declarations,
            [Declaration {
                name: "margin-left".to_string(),
                value: "10px".to_string(),
                important: false,
            }]
        );

        let doc = parse_html("<div id=\"a\"></div><div id=\"b\"></div>");
        let sheet = parse_css(
            "#a { animation: slide 2s ease-out 100ms infinite alternate-reverse both paused,
                             'fade' 50ms; }
             #b { animation-name: x, y, none; animation-duration: 1s, 2s; animation-delay: -1s;
                  animation-iteration-count: 2.5; animation-direction: reverse, normal;
                  animation-fill-mode: forwards; animation-timing-function: steps(2);
                  animation: slide 1s 2s 3s; animation-duration: -1s; }",
        );
        let styles = compute_styles(&doc, &sheet);
        let get = |id: &str| *styles.get(doc.get_element_by_id(id).unwrap());
        assert_eq!(
            get("a").animations.specs(),
            [
                AnimationSpec {
                    name_hash: animation_name_hash("slide"),
                    duration_ms: 2000.0,
                    delay_ms: 100.0,
                    iterations: f32::INFINITY,
                    direction: AnimationDirection::AlternateReverse,
                    fill: FillMode::Both,
                    timing: TimingFunction::CubicBezier(0.0, 0.0, 0.58, 1.0),
                },
                AnimationSpec {
                    name_hash: animation_name_hash("fade"),
                    duration_ms: 50.0,
                    delay_ms: 0.0,
                    iterations: 1.0,
                    direction: AnimationDirection::Normal,
                    fill: FillMode::None,
                    timing: TimingFunction::EASE,
                },
            ]
        );
        // `none` entries are skipped; shorter lists repeat; the bad
        // shorthand and the negative duration are ignored.
        let b = get("b").animations.specs();
        assert_eq!(b.len(), 2);
        assert_eq!(
            b[1],
            AnimationSpec {
                name_hash: animation_name_hash("y"),
                duration_ms: 2000.0,
                delay_ms: -1000.0,
                iterations: 2.5,
                direction: AnimationDirection::Normal,
                fill: FillMode::Forwards,
                timing: TimingFunction::Steps(2, StepPosition::End),
            }
        );
        assert_eq!(b[0].direction, AnimationDirection::Reverse);
    }

    #[test]
    fn runs_keyframe_animations_on_an_injected_clock() {
        let mut doc = parse_html("<div id=\"a\"></div>");
        let sheet = parse_css(
            "@keyframes slide {
                from { margin-left: 0; width: auto }
                50% { margin-left: 100px; animation-timing-function: linear }
                to { margin-left: 50px; width: 100px }
             }
             @keyframes fade { from { opacity: 0 } }
             #a { animation: slide 1s linear 100ms 2 alternate both, fade 100ms linear; }",
        );
        let a = doc.get_element_by_id("a").unwrap();
        let mut animator = Animator::new();
        let at = |animator: &mut Animator, doc: &Document, now: f64| {
            let style = frame(animator, doc, &sheet, now, "a");
            (style.margin.left, style.opacity, style.width)
        };
        // Waiting on the delay, filled backwards from the first keyframe.
        assert_eq!(at(&mut animator, &doc, 0.0), (0.0, 0.0, Size::Auto));
        assert!(animator.is_animating());
        assert_eq!(at(&mut animator, &doc, 25.0), (0.0, 0.25, Size::Auto));
        // 250ms in: halfway to the 50% keyframe. `auto` does not blend.
        assert_eq!(at(&mut animator, &doc, 350.0), (50.0, 1.0, Size::Auto));
        assert_eq!(at(&mut animator, &doc, 850.0), (75.0, 1.0, Size::Px(100.0)));
        // The second run plays backward.
        assert_eq!(
            at(&mut animator, &doc, 1350.0),
            (75.0, 1.0, Size::Px(100.0))
        );
        // Done after two runs; filled forwards at the start of the curve.
        assert!(animator.tick(2099.0));
        assert_eq!(at(&mut animator, &doc, 2200.0), (0.0, 1.0, Size::Auto));
        assert!(!animator.is_animating());

        // Keeping the name keeps the start time: 2100ms into a 4s run is
        // 52.5%, just past the middle keyframe. A new name starts now.
        doc.set_attribute(a, "style", "animation-duration: 4s, 100ms");
        let (left, _, _) = at(&mut animator, &doc, 2200.0);
        assert_eq!(left, 97.5);
        doc.set_attribute(a, "style", "animation: fade 100ms linear infinite");
        assert_eq!(at(&mut animator, &doc, 3000.0).1, 0.0);
        assert_eq!(at(&mut animator, &doc, 3150.0).1, 0.5);
        assert!(animator.tick(1e12));
        // Removing the animation stops it.
        doc.set_attribute(a, "style", "animation: none");
        assert_eq!(at(&mut animator, &doc, 1e12), (0.0, 1.0, Size::Auto));
        assert!(!animator.is_animating());
    }

    #[test]
    fn far_apart_keyframes_fill_forwards_with_their_last_value() {
        let doc = parse_html("<div id=\"a\"></div><div id=\"b\"></div>");
        let sheet = parse_css(
            "@keyframes far { from { margin-left: 1e30px } to { margin-left: -370px } }
             #a { animation: far 100ms linear forwards; }
             #b { animation: far 100ms linear reverse forwards; }",
        );
        let mut animator = Animator::new();
        let mut left = |now: f64, id: &str| frame(&mut animator, &doc, &sheet, now, id).margin.left;
        assert_eq!(left(0.0, "a"), 1e30);
        assert_eq!(left(0.0, "b"), -370.0);
        assert_eq!(left(100.0, "a"), -370.0);
        assert_eq!(left(5000.0, "a"), -370.0);
        // Played backward, the fill keeps the `from` value.
        assert_eq!(left(5000.0, "b"), 1e30);
        assert!(!animator.is_animating());

        let html = "<style>
            :root { --accent: #2266aa; }
            .card { width: 120px; padding: 8px; color: var(--accent); }
            .card p { font-size: 14px; margin: 4px; }
        </style>
        <div class=\"card\" id=\"c1\"><p>One</p></div>
        <div class=\"card\" id=\"c2\"><p>Two</p></div>
        <div class=\"card\" id=\"c3\"><p>Three</p></div>
        <div class=\"card\" id=\"c4\"><p>Four</p></div>";
        let parallel_doc = parse_html(html);
        let parallel_sheet = parse_css(&parallel_doc.embedded_css());
        let sequential = compute_styles_for_viewport(&parallel_doc, &parallel_sheet, 800.0, 600.0);
        let parallel = compute_styles_parallel(&parallel_doc, &parallel_sheet, 800.0, 600.0);
        assert_eq!(parallel, sequential);
        assert!(!animator.is_animating());
    }
}
