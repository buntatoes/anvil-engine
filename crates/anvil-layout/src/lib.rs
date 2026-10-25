// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Block, inline, flex, table, and positioned layout, plus hit testing.
//!
//! Third stage of the pipeline. anvil-style has computed a `StyleMap` for a
//! anvil-dom `Document`. This crate borrows both and returns an owned
//! [`LayoutBox`] tree. anvil-paint walks that tree into a display list. This
//! crate does not paint pixels.
//!
//! The origin is the top-left of the initial containing block. X increases to
//! the right and y increases downward, in CSS pixels. [`hit_test`] uses the
//! same space. A point on the right or bottom edge is outside the box.
//!
//! `position: fixed` is placed against `(0, 0)` with the parent's containing
//! width and the parent's border-box height. It is not a separate viewport.
//! `position: absolute` uses the parent's border box. A percentage width uses
//! the containing block width. A percentage height uses the containing block
//! height when that height is set, and is treated as `auto` when it is not.
//! `em` uses the element's font size. `rem` uses [`StyleMap::root_font_px`].
//! A flex row wraps when `flex-wrap` is `wrap`. `flex-grow` shares positive
//! free space on each of those lines and does not shrink items. Column flex
//! does not wrap or grow. A table row splits its content width into equal
//! columns with a 24px floor; column widths are not shared across rows. Line breaking uses
//! [`measure`], a fixed advance of half the font size per character, not
//! [`measure_proportional`].

use std::collections::HashMap;
use std::sync::OnceLock;

pub use anvil_dom::NodeId;
use anvil_dom::{Document, NodeData};
pub use anvil_image::Image;
pub use anvil_style::{
    AlignItems, BoxShadow, Color, Edge, FlexDirection, FlexWrap, JustifyContent, LinearGradient,
    Overflow, Position,
};
use anvil_style::{ComputedStyle, Display, GridTrack, Size, StyleMap};
use anvil_ttf::TtfFont;

/// Axis-aligned box. `x` and `y` are the top-left in y-down CSS pixels.
/// Width and height grow to the right and downward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// True when `(x, y)` is inside, including the top and left edges and
    /// excluding the right and bottom edges. A zero-area rect contains nothing.
    pub fn contains(self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// One painted line fragment. `text` is owned. `x` and `y` are the top-left
/// of the run in y-down pixels. `node` is the text node, or the element that
/// produced a form label or list marker.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub node: NodeId,
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub font_size: f32,
    pub color: Color,
    pub underline: bool,
}

/// A border box and the children laid out inside it.
///
/// `rect` is the border box, margin excluded, in y-down pixels. `text` and
/// `children` are owned. `image` is an owned clone of a decoded anvil-image
/// bitmap when one was supplied for this node; otherwise `None`.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutBox {
    pub node: NodeId,
    pub rect: Rect,
    pub position: Position,
    pub z_index: i32,
    pub overflow: Overflow,
    pub background: Option<Color>,
    pub background_gradient: Option<LinearGradient>,
    pub box_shadow: Option<BoxShadow>,
    pub border: Edge,
    pub border_color: Color,
    pub border_radius: f32,
    pub image: Option<Image>,
    pub text: Vec<TextRun>,
    pub children: Vec<LayoutBox>,
}

/// A slice of inline text before it is broken into line tokens.
/// `text` is owned. The node is the text node or the element that synthesized it.
#[derive(Clone, Debug)]
struct Segment {
    node: NodeId,
    text: String,
    font_size: f32,
    color: Color,
    underline: bool,
}

/// One word, a single space, a forced break (`\n`), or one character split
/// from a word that was wider than the line.
#[derive(Clone, Debug)]
struct Token {
    node: NodeId,
    text: String,
    font_size: f32,
    color: Color,
    underline: bool,
}

/// Lay out `doc` from the `html` element in a viewport `viewport_width` pixels wide.
///
/// `doc` and `styles` are borrowed. No images are supplied, so an `<img>` or
/// `<video>` uses its attributes or the fallback size. No control is focused.
/// The returned tree is owned. The origin is the top-left; y grows downward.
pub fn layout_document(doc: &Document, styles: &StyleMap, viewport_width: f32) -> LayoutBox {
    let empty = HashMap::new();
    layout_document_interactive(doc, styles, viewport_width, &empty, None)
}

/// Like [`layout_document`], with decoded images keyed by node id.
///
/// `images` is borrowed. A missing entry leaves the element without a bitmap.
/// Intrinsic width and height come from the image when the attributes are absent.
pub fn layout_document_with_images(
    doc: &Document,
    styles: &StyleMap,
    viewport_width: f32,
    images: &HashMap<NodeId, Image>,
) -> LayoutBox {
    layout_document_interactive(doc, styles, viewport_width, images, None)
}

/// Lay out `doc`, optionally focusing one control.
///
/// A negative viewport width is treated as 0. When `focused_input` is a text
/// control, the laid-out text gains a trailing `|`. A password is masked with
/// one `*` per character. A block box whose id matches also uses the focus
/// border color. `doc`, `styles`, and `images` are borrowed. The tree is owned.
pub fn layout_document_interactive(
    doc: &Document,
    styles: &StyleMap,
    viewport_width: f32,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
) -> LayoutBox {
    layout_node(
        doc,
        styles,
        images,
        focused_input,
        doc.html(),
        // Origin: top-left of the initial containing block. Y increases downward.
        0.0,
        0.0,
        viewport_width.max(0.0),
        None,
        None,
    )
}

/// The front-most node under `(x, y)` in y-down pixels.
///
/// `tree` is borrowed. Children are tested from highest `z-index` to lowest,
/// and from last sibling to first when the index ties. Text runs are tested
/// after children, in reverse order, and a hit returns the run's node, which
/// is often a text node. The box itself is last. `None` when the point misses
/// the tree. Overflow `hidden` rejects a point outside this box before
/// descendants are tested, so clipped children are not hits.
pub fn hit_test(tree: &LayoutBox, x: f32, y: f32) -> Option<NodeId> {
    // Hidden overflow clips hit testing to the border box, not the padding edge.
    if tree.overflow == Overflow::Hidden && !tree.rect.contains(x, y) {
        return None;
    }
    let mut order: Vec<(usize, &LayoutBox)> = tree.children.iter().enumerate().collect();
    order.sort_by_key(|(idx, child)| (child.z_index, *idx));
    for (_, child) in order.into_iter().rev() {
        if let Some(hit) = hit_test(child, x, y) {
            return Some(hit);
        }
    }
    for run in tree.text.iter().rev() {
        let rect = Rect {
            x: run.x,
            y: run.y,
            width: run.width,
            height: line_height(run.font_size),
        };
        if rect.contains(x, y) {
            return Some(run.node);
        }
    }
    tree.rect.contains(x, y).then_some(tree.node)
}

#[allow(clippy::too_many_arguments)]
/// Lay out `id` inside a containing block that is `containing_width` pixels wide.
///
/// `x` and `y` are the top-left of the margin edge, y downward. A `tr` uses
/// the table-row algorithm even if its display is flex. Other flex containers
/// use [`layout_flex`]. Everything else is a block. Relative offsets are applied
/// after that used position: a positive `left` or `top` moves right or down.
/// `left` wins over `right`, and `top` wins over `bottom`.
fn layout_node(
    doc: &Document,
    styles: &StyleMap,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
    id: NodeId,
    x: f32,
    y: f32,
    containing_width: f32,
    containing_height: Option<f32>,
    list_marker: Option<String>,
) -> LayoutBox {
    let style = *styles.get(id);
    let mut b = if doc.get(id).data().tag() == Some("tr") {
        layout_table_row(
            doc,
            styles,
            images,
            focused_input,
            id,
            x,
            y,
            containing_width,
            containing_height,
        )
    } else if style.display == Display::Grid {
        layout_grid(
            doc,
            styles,
            images,
            focused_input,
            id,
            x,
            y,
            containing_width,
            containing_height,
        )
    } else if style.display == Display::Flex {
        layout_flex(
            doc,
            styles,
            images,
            focused_input,
            id,
            x,
            y,
            containing_width,
            containing_height,
        )
    } else {
        layout_block(
            doc,
            styles,
            images,
            focused_input,
            id,
            x,
            y,
            containing_width,
            containing_height,
            list_marker,
        )
    };
    if style.position == Position::Relative {
        // Positive top/left move down/right. Positive bottom/right move up/left.
        // A percentage uses the containing block. An unresolved percentage is 0.
        let dx = match used_length(style.left, Some(containing_width), &style, styles) {
            Some(l) => l,
            None => used_length(style.right, Some(containing_width), &style, styles)
                .map(|r| -r)
                .unwrap_or(0.0),
        };
        let dy = match used_length(style.top, containing_height, &style, styles) {
            Some(t) => t,
            None => used_length(style.bottom, containing_height, &style, styles)
                .map(|bt| -bt)
                .unwrap_or(0.0),
        };
        offset_box(&mut b, dx, dy);
    }
    b
}

/// Resolve `size` against an optional containing-block `basis`.
///
/// `None` means `auto`, including a percentage whose basis was not passed.
/// `em` uses the element's font size. `rem` uses the root font size.
/// `vw`, `vh`, `vmin`, and `vmax` use the viewport on `styles`.
fn used_length(
    size: Size,
    basis: Option<f32>,
    style: &ComputedStyle,
    styles: &StyleMap,
) -> Option<f32> {
    let vw = styles.viewport_width();
    let vh = styles.viewport_height();
    match size {
        Size::Auto => None,
        Size::Vw(value) => Some(vw * value / 100.0),
        Size::Vh(value) => Some(vh * value / 100.0),
        Size::Vmin(value) => Some(vw.min(vh) * value / 100.0),
        Size::Vmax(value) => Some(vw.max(vh) * value / 100.0),
        Size::Percent(_) => size.used_px(basis?, style.font_size, styles.root_font_px()),
        Size::Calc { percent, .. } if percent != 0.0 => {
            size.used_px(basis?, style.font_size, styles.root_font_px())
        }
        other => other.used_px(0.0, style.font_size, styles.root_font_px()),
    }
}

/// Border-box width from `width` and `max-width`.
///
/// A definite `width` is the content width; padding and border are added.
/// `auto`, and a percentage with no basis, fills the containing block minus
/// horizontal margins. A definite `max-width` caps the border box. The result
/// is never negative.
fn resolve_border_width(style: &ComputedStyle, containing_width: f32, styles: &StyleMap) -> f32 {
    let padding_h = style.padding.left + style.padding.right;
    let border_h = style.border.left + style.border.right;
    let mut border_width = match used_length(style.width, Some(containing_width), style, styles) {
        Some(width) => (width + padding_h + border_h).max(0.0),
        None => (containing_width - style.margin.left - style.margin.right).max(0.0),
    };
    if let Some(max_w) = used_length(style.max_width, Some(containing_width), style, styles) {
        let max_border_w = (max_w + padding_h + border_h).max(0.0);
        border_width = border_width.min(max_border_w);
    }
    border_width
}

#[allow(clippy::too_many_arguments)]
/// Block layout. In-flow children stack downward from the content top.
///
/// Absolute and fixed children are collected and placed after the in-flow
/// height is known, so they do not advance `cursor_y`. Inline runs are flushed
/// before each block child. List items receive a marker: `N. ` under `ol`,
/// otherwise `- `. The marker's node is the `li`.
fn layout_block(
    doc: &Document,
    styles: &StyleMap,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
    id: NodeId,
    x: f32,
    y: f32,
    containing_width: f32,
    containing_height: Option<f32>,
    list_marker: Option<String>,
) -> LayoutBox {
    let style = *styles.get(id);
    let padding = style.padding;
    let margin = style.margin;
    let border = style.border;
    let child_cb_height = used_length(style.height, containing_height, &style, styles);

    let mut border_width = resolve_border_width(&style, containing_width, styles);
    let data = doc.get(id).data();
    let tag = data.tag().unwrap_or("");
    let is_img = tag == "img" || tag == "video";
    let is_checkbox = doc.is_checkbox_input(id);
    let is_text_ctrl = doc.is_text_input(id);
    let is_textarea = tag == "textarea";
    let is_select = doc.is_select_control(id);
    let is_submit_input = tag == "input" && doc.is_submit_control(id);
    let decoded_image = if is_img {
        images.get(&id).cloned()
    } else {
        None
    };

    if matches!(style.width, Size::Auto) {
        if is_img {
            let attr_w = data
                .attr("width")
                .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
                .or_else(|| decoded_image.as_ref().map(|img| img.width as f32))
                .unwrap_or(120.0);
            border_width = attr_w + padding.left + padding.right + border.left + border.right;
        } else if is_checkbox {
            border_width = 20.0;
        } else if is_submit_input {
            let label = data.attr("value").unwrap_or("Submit");
            border_width = measure(label, style.font_size)
                + padding.left
                + padding.right
                + border.left
                + border.right
                + 8.0;
        } else if is_textarea {
            let cols = data
                .attr("cols")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(32.0)
                .clamp(8.0, 120.0);
            border_width = (cols * 8.0 + padding.left + padding.right + border.left + border.right)
                .min(containing_width.max(80.0));
        } else if is_select {
            let label = doc.select_display_label(id);
            border_width = (measure(&label, style.font_size)
                + 32.0
                + padding.left
                + padding.right
                + border.left
                + border.right)
                .max(140.0)
                .min(containing_width.max(60.0));
        } else if is_text_ctrl {
            border_width = 360.0_f32.min(containing_width.max(60.0));
        } else if tag == "button" {
            let label = doc.text_content(id);
            border_width = (measure(&label, style.font_size)
                + padding.left
                + padding.right
                + border.left
                + border.right
                + 8.0)
                .min(containing_width.max(48.0));
        }
    }

    let content_width =
        (border_width - padding.left - padding.right - border.left - border.right).max(0.0);
    let border_x = x + margin.left;
    let border_y = y + margin.top;
    let content_x = border_x + border.left + padding.left;
    let mut cursor_y = border_y + border.top + padding.top;
    let mut children = Vec::new();
    let mut text = Vec::new();
    let mut inline_ids = Vec::new();
    let mut pending_marker = list_marker;

    if is_img && decoded_image.is_none() {
        if let Some(alt) = data.attr("alt") {
            if !alt.trim().is_empty() {
                let segments = vec![Segment {
                    node: id,
                    text: format!("[{alt}]"),
                    font_size: style.font_size,
                    color: style.color,
                    underline: false,
                }];
                let (runs, h) = place_lines(&segments, content_x, cursor_y, content_width);
                text.extend(runs);
                cursor_y += h;
            }
        }
    } else if is_checkbox {
        let mark = if doc.is_checked(id) { "X" } else { " " };
        let segments = vec![Segment {
            node: id,
            text: mark.to_string(),
            font_size: style.font_size,
            color: style.color,
            underline: false,
        }];
        let (runs, h) = place_lines(&segments, content_x, cursor_y, content_width.max(16.0));
        text.extend(runs);
        cursor_y += h;
    } else if is_submit_input {
        let label = data.attr("value").unwrap_or("Submit");
        let segments = vec![Segment {
            node: id,
            text: label.to_string(),
            font_size: style.font_size,
            color: style.color,
            underline: false,
        }];
        let (runs, h) = place_lines(&segments, content_x, cursor_y, content_width.max(24.0));
        text.extend(runs);
        cursor_y += h;
    } else if is_select {
        let label = format!("{}  v", doc.select_display_label(id));
        let segments = vec![Segment {
            node: id,
            text: label,
            font_size: style.font_size,
            color: style.color,
            underline: false,
        }];
        let (runs, h) = place_lines(&segments, content_x, cursor_y, content_width.max(24.0));
        text.extend(runs);
        cursor_y += h;
    } else if (tag == "input" || is_textarea) && is_text_ctrl {
        let raw_val = doc.input_value(id);
        let is_pw = data
            .attr("type")
            .is_some_and(|t| t.trim().eq_ignore_ascii_case("password"));
        let val = if is_pw {
            "*".repeat(raw_val.chars().count())
        } else {
            raw_val
        };
        let is_focused = focused_input == Some(id);
        let (display_str, text_color) = if is_focused {
            (format!("{val}|"), style.color)
        } else if !val.is_empty() {
            (val, style.color)
        } else if let Some(ph) = data.attr("placeholder") {
            (
                ph.to_string(),
                Color {
                    r: 138,
                    g: 148,
                    b: 166,
                    a: 255,
                },
            )
        } else {
            (" ".to_string(), style.color)
        };
        let single_line_str = if !is_textarea {
            let max_chars = ((content_width.max(24.0) / (style.font_size * 0.5).max(1.0)).floor()
                as usize)
                .max(4);
            let chars: Vec<char> = display_str.chars().collect();
            if chars.len() > max_chars {
                if is_focused {
                    chars[chars.len() - max_chars..].iter().collect()
                } else {
                    chars[..max_chars].iter().collect()
                }
            } else {
                display_str
            }
        } else {
            display_str
        };
        let segments = vec![Segment {
            node: id,
            text: single_line_str,
            font_size: style.font_size,
            color: text_color,
            underline: false,
        }];
        // Single-line controls do not wrap. Overflow keeps the tail while focused
        // and the head otherwise. Textareas wrap inside the content box.
        let wrap_w = if !is_textarea {
            content_width.max(4096.0)
        } else {
            content_width.max(16.0)
        };
        let (runs, h) = place_lines(&segments, content_x, cursor_y, wrap_w);
        text.extend(runs);
        cursor_y += h;
    }

    let parent_tag = tag;
    let mut li_index = 0_usize;
    let mut out_of_flow = Vec::new();

    if tag != "input" && !is_textarea && !is_select {
        for child in doc.children(id) {
            if is_skipped(doc, styles, *child) {
                continue;
            }
            let child_style = styles.get(*child);
            if matches!(child_style.position, Position::Absolute | Position::Fixed) {
                out_of_flow.push(*child);
                continue;
            }
            if is_block_box(doc, styles, *child) {
                place_inlines(
                    doc,
                    styles,
                    id,
                    &mut pending_marker,
                    &mut inline_ids,
                    content_x,
                    &mut cursor_y,
                    content_width,
                    &mut text,
                );
                let child_marker = if doc.get(*child).data().tag() == Some("li") {
                    li_index += 1;
                    if parent_tag == "ol" {
                        Some(format!("{li_index}. "))
                    } else {
                        Some("- ".to_string())
                    }
                } else {
                    None
                };
                let child_box = layout_node(
                    doc,
                    styles,
                    images,
                    focused_input,
                    *child,
                    content_x,
                    cursor_y,
                    content_width,
                    child_cb_height,
                    child_marker,
                );
                cursor_y +=
                    child_style.margin.top + child_box.rect.height + child_style.margin.bottom;
                children.push(child_box);
            } else {
                inline_ids.push(*child);
            }
        }
        place_inlines(
            doc,
            styles,
            id,
            &mut pending_marker,
            &mut inline_ids,
            content_x,
            &mut cursor_y,
            content_width,
            &mut text,
        );
    }

    let flow_height = cursor_y - (border_y + border.top + padding.top);
    let content_height = match used_length(style.height, containing_height, &style, styles) {
        Some(height) => height.max(flow_height),
        None if is_img => data
            .attr("height")
            .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
            .or_else(|| decoded_image.as_ref().map(|img| img.height as f32))
            .unwrap_or(36.0)
            .max(flow_height),
        None if is_checkbox => 18.0,
        None if is_textarea => {
            let rows = data
                .attr("rows")
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(3.0)
                .clamp(1.0, 24.0);
            flow_height.max(line_height(style.font_size) * rows)
        }
        None if is_text_ctrl || is_select => flow_height.max(line_height(style.font_size)),
        None => flow_height,
    };
    let border_height = content_height + padding.top + padding.bottom + border.top + border.bottom;
    let parent_rect = Rect {
        x: border_x,
        y: border_y,
        width: border_width,
        height: border_height,
    };

    for oof_id in out_of_flow {
        let oof_box = layout_out_of_flow(
            doc,
            styles,
            images,
            focused_input,
            oof_id,
            parent_rect,
            containing_width,
        );
        children.push(oof_box);
    }

    let border_color = if focused_input == Some(id) {
        Color {
            r: 232,
            g: 195,
            b: 122,
            a: 255,
        }
    } else {
        style.border_color
    };

    LayoutBox {
        node: id,
        rect: parent_rect,
        position: style.position,
        z_index: style.z_index,
        overflow: style.overflow,
        background: style.background,
        background_gradient: style.background_gradient,
        box_shadow: style.box_shadow,
        border,
        border_color,
        border_radius: style.border_radius,
        image: decoded_image,
        text,
        children,
    }
}

/// Place an absolute or fixed box after its static position was computed at the origin.
///
/// Fixed uses a reference box at `(0, 0)` whose width is `viewport_width` (the
/// containing width of the parent that pulled this box out of flow, not a
/// separate viewport) and whose height is the parent's border-box height.
/// Absolute uses the parent's border box. `left` wins over `right` and `top`
/// wins over `bottom`. Missing offsets sit on the reference origin. The shift
/// moves the whole subtree, including text runs.
fn layout_out_of_flow(
    doc: &Document,
    styles: &StyleMap,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
    id: NodeId,
    parent_rect: Rect,
    viewport_width: f32,
) -> LayoutBox {
    let style = *styles.get(id);
    // Fixed origin is the top-left of the initial coordinates, y downward.
    // The height is the parent's border box, not the viewport height.
    let ref_rect = if style.position == Position::Fixed {
        Rect {
            x: 0.0,
            y: 0.0,
            width: viewport_width,
            height: parent_rect.height,
        }
    } else {
        parent_rect
    };
    let mut b = layout_node(
        doc,
        styles,
        images,
        focused_input,
        id,
        0.0,
        0.0,
        ref_rect.width,
        Some(ref_rect.height),
        None,
    );
    let target_x = match used_length(style.left, Some(ref_rect.width), &style, styles) {
        Some(l) => ref_rect.x + l,
        None => match used_length(style.right, Some(ref_rect.width), &style, styles) {
            Some(r) => ref_rect.x + ref_rect.width - b.rect.width - r,
            None => ref_rect.x,
        },
    };
    let target_y = match used_length(style.top, Some(ref_rect.height), &style, styles) {
        Some(t) => ref_rect.y + t,
        None => match used_length(style.bottom, Some(ref_rect.height), &style, styles) {
            Some(bt) => ref_rect.y + ref_rect.height - b.rect.height - bt,
            None => ref_rect.y,
        },
    };
    let dx = target_x - b.rect.x;
    let dy = target_y - b.rect.y;
    offset_box(&mut b, dx, dy);
    b
}

#[allow(clippy::too_many_arguments)]
/// Lay out a `tr`. Cells share the content width equally, with a 24px floor.
///
/// Each cell is laid out as its own box with that column width as its
/// containing width. The row's content height is the tallest cell, and every
/// cell's border-box height is then overwritten to that height. Cells are
/// placed left to right. This is not a column-balancing table: colspans,
/// row groups, and column widths from the first row are not tracked.
fn layout_table_row(
    doc: &Document,
    styles: &StyleMap,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
    id: NodeId,
    x: f32,
    y: f32,
    containing_width: f32,
    containing_height: Option<f32>,
) -> LayoutBox {
    let style = *styles.get(id);
    let padding = style.padding;
    let margin = style.margin;
    let border = style.border;
    let border_width = resolve_border_width(&style, containing_width, styles);
    let content_width =
        (border_width - padding.left - padding.right - border.left - border.right).max(0.0);
    let border_x = x + margin.left;
    let border_y = y + margin.top;
    let content_x = border_x + border.left + padding.left;
    let content_y = border_y + border.top + padding.top;

    let cell_ids: Vec<NodeId> = doc
        .children(id)
        .iter()
        .copied()
        .filter(|cid| !is_skipped(doc, styles, *cid))
        .collect();
    let col_count = cell_ids.len().max(1);
    // Equal columns. The floor can make the row wider than the containing block.
    let col_width = (content_width / (col_count as f32)).max(24.0);

    let mut children = Vec::with_capacity(cell_ids.len());
    let mut row_height = 0.0_f32;
    let mut cursor_x = content_x;

    for cid in cell_ids {
        let cell_box = layout_node(
            doc,
            styles,
            images,
            focused_input,
            cid,
            cursor_x,
            content_y,
            col_width,
            containing_height,
            None,
        );
        row_height = row_height.max(cell_box.rect.height);
        cursor_x += cell_box.rect.width;
        children.push(cell_box);
    }

    for cell in &mut children {
        cell.rect.height = row_height;
    }

    let content_height = match used_length(style.height, containing_height, &style, styles) {
        Some(h) => h.max(row_height),
        None => row_height,
    };
    let border_height = content_height + padding.top + padding.bottom + border.top + border.bottom;

    LayoutBox {
        node: id,
        rect: Rect {
            x: border_x,
            y: border_y,
            width: border_width,
            height: border_height,
        },
        position: style.position,
        z_index: style.z_index,
        overflow: style.overflow,
        background: style.background,
        background_gradient: style.background_gradient,
        box_shadow: style.box_shadow,
        border,
        border_color: style.border_color,
        border_radius: style.border_radius,
        image: None,
        text: Vec::new(),
        children,
    }
}

/// One flex item after its base border box has been measured.
#[allow(clippy::too_many_arguments)]
fn layout_grid(
    doc: &Document,
    styles: &StyleMap,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
    id: NodeId,
    x: f32,
    y: f32,
    containing_width: f32,
    containing_height: Option<f32>,
) -> LayoutBox {
    let style = *styles.get(id);
    let padding = style.padding;
    let margin = style.margin;
    let border = style.border;
    let border_width = resolve_border_width(&style, containing_width, styles);
    let content_width =
        (border_width - padding.left - padding.right - border.left - border.right).max(0.0);
    let border_x = x + margin.left;
    let border_y = y + margin.top;
    let content_x = border_x + border.left + padding.left;
    let content_y = border_y + border.top + padding.top;
    let specified_height = used_length(style.height, containing_height, &style, styles);

    let default_track = [GridTrack::Fr(1.0)];
    let tracks = if style.grid_column_count > 0 {
        style.grid_tracks()
    } else {
        &default_track
    };
    let col_count = tracks.len().max(1);
    let total_col_gap = style.gap * ((col_count.saturating_sub(1)) as f32);

    let mut fixed_sum = 0.0f32;
    let mut fr_sum = 0.0f32;
    for &t in tracks {
        match t {
            GridTrack::Px(px) => fixed_sum += px,
            GridTrack::Percent(pct) => fixed_sum += content_width * pct / 100.0,
            GridTrack::Fr(fr) => fr_sum += fr.max(0.0),
        }
    }
    let free_w = (content_width - fixed_sum - total_col_gap).max(0.0);
    let mut col_widths = Vec::with_capacity(col_count);
    let mut col_offsets = Vec::with_capacity(col_count);
    let mut run_x = content_x;
    for &t in tracks {
        let w = match t {
            GridTrack::Px(px) => px,
            GridTrack::Percent(pct) => content_width * pct / 100.0,
            GridTrack::Fr(fr) => {
                if fr_sum > 0.0 {
                    free_w * (fr.max(0.0) / fr_sum)
                } else {
                    0.0
                }
            }
        };
        col_offsets.push(run_x);
        col_widths.push(w.max(0.0));
        run_x += w.max(0.0) + style.gap;
    }

    let mut in_flow = Vec::new();
    let mut out_of_flow = Vec::new();
    for &cid in doc.children(id) {
        if is_skipped(doc, styles, cid) {
            continue;
        }
        if let NodeData::Text(t) = doc.get(cid).data() {
            if t.trim().is_empty() {
                continue;
            }
        }
        let cstyle = styles.get(cid);
        if matches!(cstyle.position, Position::Absolute | Position::Fixed) {
            out_of_flow.push(cid);
        } else {
            in_flow.push(cid);
        }
    }

    let mut children = Vec::with_capacity(in_flow.len() + out_of_flow.len());
    let mut cursor_y = content_y;
    for (row_idx, row_ids) in in_flow.chunks(col_count).enumerate() {
        if row_idx > 0 {
            cursor_y += style.gap;
        }
        let row_start = children.len();
        let mut row_h = 0.0f32;
        for (col_idx, &cid) in row_ids.iter().enumerate() {
            let cell_x = col_offsets[col_idx];
            let cell_w = col_widths[col_idx];
            let child_box = layout_node(
                doc,
                styles,
                images,
                focused_input,
                cid,
                cell_x,
                cursor_y,
                cell_w,
                specified_height,
                None,
            );
            let cstyle = styles.get(cid);
            let outer_h = cstyle.margin.top + child_box.rect.height + cstyle.margin.bottom;
            row_h = row_h.max(outer_h);
            children.push(child_box);
        }
        if style.align_items == AlignItems::Stretch {
            for item_box in &mut children[row_start..] {
                let cstyle = styles.get(item_box.node);
                if matches!(cstyle.height, Size::Auto) {
                    item_box.rect.height = (row_h - cstyle.margin.top - cstyle.margin.bottom)
                        .max(item_box.rect.height);
                }
            }
        }
        cursor_y += row_h;
    }

    let flow_height = (cursor_y - content_y).max(0.0);
    let content_height = specified_height.unwrap_or(0.0).max(flow_height);
    let parent_rect = Rect {
        x: border_x,
        y: border_y,
        width: border_width,
        height: content_height + padding.top + padding.bottom + border.top + border.bottom,
    };

    for oof_id in out_of_flow {
        children.push(layout_out_of_flow(
            doc,
            styles,
            images,
            focused_input,
            oof_id,
            parent_rect,
            containing_width,
        ));
    }

    LayoutBox {
        node: id,
        rect: parent_rect,
        position: style.position,
        z_index: style.z_index,
        overflow: style.overflow,
        background: style.background,
        background_gradient: style.background_gradient,
        box_shadow: style.box_shadow,
        border,
        border_color: style.border_color,
        border_radius: style.border_radius,
        image: None,
        text: Vec::new(),
        children,
    }
}

struct FlexItem {
    node_box: LayoutBox,
    outer_w: f32,
    outer_h: f32,
    grow: f32,
}

#[allow(clippy::too_many_arguments)]
/// Flex layout.
///
/// A column stacks children downward with `gap` between them and ignores
/// `justify-content`, `align-items`, `flex-wrap`, and `flex-grow`. A row
/// measures each item once. `flex-wrap: wrap` starts a new line when the next
/// item does not fit. `flex-grow` then shares positive free space on each line
/// and does not shrink an overflowing line. `space-between` with one item
/// matches start. `align-items: stretch` only increases the border-box height
/// of an item whose height is `auto`.
fn layout_flex(
    doc: &Document,
    styles: &StyleMap,
    images: &HashMap<NodeId, Image>,
    focused_input: Option<NodeId>,
    id: NodeId,
    x: f32,
    y: f32,
    containing_width: f32,
    containing_height: Option<f32>,
) -> LayoutBox {
    let style = *styles.get(id);
    let padding = style.padding;
    let margin = style.margin;
    let border = style.border;
    let border_width = resolve_border_width(&style, containing_width, styles);
    let content_width =
        (border_width - padding.left - padding.right - border.left - border.right).max(0.0);
    let border_x = x + margin.left;
    let border_y = y + margin.top;
    let content_x = border_x + border.left + padding.left;
    let content_y = border_y + border.top + padding.top;
    let specified_height = used_length(style.height, containing_height, &style, styles);

    let visible_children: Vec<NodeId> = doc
        .children(id)
        .iter()
        .copied()
        .filter(|child| !is_skipped(doc, styles, *child))
        .collect();

    if style.flex_direction == FlexDirection::Column {
        let mut cursor_y = content_y;
        let mut children = Vec::new();
        for (idx, child) in visible_children.iter().copied().enumerate() {
            if idx > 0 {
                cursor_y += style.gap;
            }
            let child_box = layout_node(
                doc,
                styles,
                images,
                focused_input,
                child,
                content_x,
                cursor_y,
                content_width,
                specified_height,
                None,
            );
            let child_style = styles.get(child);
            cursor_y += child_style.margin.top + child_box.rect.height + child_style.margin.bottom;
            children.push(child_box);
        }
        let flow_height = (cursor_y - content_y).max(0.0);
        let content_height = specified_height.unwrap_or(0.0).max(flow_height);
        return LayoutBox {
            node: id,
            rect: Rect {
                x: border_x,
                y: border_y,
                width: border_width,
                height: content_height + padding.top + padding.bottom + border.top + border.bottom,
            },
            position: style.position,
            z_index: style.z_index,
            overflow: style.overflow,
            background: style.background,
            background_gradient: style.background_gradient,
            box_shadow: style.box_shadow,
            border,
            border_color: style.border_color,
            border_radius: style.border_radius,
            image: None,
            text: Vec::new(),
            children,
        };
    }

    let mut items = Vec::new();
    for child in &visible_children {
        let child_style = styles.get(*child);
        let resolved_content_w =
            used_length(child_style.width, Some(content_width), child_style, styles);
        let basis_width = if let Some(w) = resolved_content_w {
            w + child_style.padding.left
                + child_style.padding.right
                + child_style.border.left
                + child_style.border.right
                + child_style.margin.left
                + child_style.margin.right
        } else {
            let child_data = doc.get(*child).data();
            let child_tag = child_data.tag().unwrap_or("");
            let intrinsic_content_w = if child_tag == "img" || child_tag == "video" {
                child_data
                    .attr("width")
                    .and_then(|v| v.trim_end_matches("px").parse::<f32>().ok())
                    .or_else(|| images.get(child).map(|img| img.width as f32))
                    .unwrap_or(120.0)
            } else if child_tag == "input" && doc.is_text_input(*child) {
                (content_width - 136.0).clamp(160.0, 560.0)
            } else if child_tag == "input" && doc.is_submit_control(*child) {
                let lbl = child_data.attr("value").unwrap_or("Submit");
                measure(lbl, child_style.font_size) + 12.0
            } else if child_tag == "button" {
                measure(&doc.text_content(*child), child_style.font_size) + 12.0
            } else {
                measure(&doc.text_content(*child), child_style.font_size)
            };
            (intrinsic_content_w
                + child_style.padding.left
                + child_style.padding.right
                + child_style.border.left
                + child_style.border.right
                + child_style.margin.left
                + child_style.margin.right)
                .min(content_width)
        };
        let item_containing = if resolved_content_w.is_some() {
            content_width
        } else {
            basis_width
        };
        let child_box = layout_node(
            doc,
            styles,
            images,
            focused_input,
            *child,
            0.0,
            0.0,
            item_containing,
            specified_height,
            None,
        );
        let outer_w = child_style.margin.left + child_box.rect.width + child_style.margin.right;
        let outer_h = child_style.margin.top + child_box.rect.height + child_style.margin.bottom;
        items.push(FlexItem {
            node_box: child_box,
            outer_w,
            outer_h,
            grow: child_style.flex_grow,
        });
    }

    let wrap = style.flex_wrap == FlexWrap::Wrap;
    let mut lines: Vec<Vec<FlexItem>> = Vec::new();
    let mut current: Vec<FlexItem> = Vec::new();
    let mut line_width = 0.0_f32;
    for item in items {
        let gap = if current.is_empty() { 0.0 } else { style.gap };
        if wrap && !current.is_empty() && line_width + gap + item.outer_w > content_width {
            lines.push(std::mem::take(&mut current));
            line_width = 0.0;
        }
        let gap = if current.is_empty() { 0.0 } else { style.gap };
        line_width += gap + item.outer_w;
        current.push(item);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }

    let mut children = Vec::new();
    let mut cursor_y = content_y;
    let mut lines_cross = 0.0_f32;
    for (line_index, line) in lines.iter_mut().enumerate() {
        if line_index > 0 {
            cursor_y += style.gap;
            lines_cross += style.gap;
        }
        let count = line.len();
        let gaps = if count > 1 {
            style.gap * ((count - 1) as f32)
        } else {
            0.0
        };
        let used: f32 = line.iter().map(|item| item.outer_w).sum();
        let free_space = (content_width - used - gaps).max(0.0);
        let grow_sum: f32 = line.iter().map(|item| item.grow).sum();
        if free_space > 0.0 && grow_sum > 0.0 {
            for item in line.iter_mut() {
                if item.grow > 0.0 {
                    let extra = free_space * item.grow / grow_sum;
                    item.node_box.rect.width += extra;
                    item.outer_w += extra;
                }
            }
        }
        let used_after: f32 = line.iter().map(|item| item.outer_w).sum();
        let free_after = (content_width - used_after - gaps).max(0.0);
        let (mut cursor_x, step_gap) = match style.justify_content {
            JustifyContent::Start => (content_x, style.gap),
            JustifyContent::Center => (content_x + free_after * 0.5, style.gap),
            JustifyContent::End => (content_x + free_after, style.gap),
            JustifyContent::SpaceBetween if count > 1 => {
                (content_x, style.gap + free_after / ((count - 1) as f32))
            }
            JustifyContent::SpaceBetween => (content_x, style.gap),
        };
        let line_height = line.iter().map(|item| item.outer_h).fold(0.0_f32, f32::max);
        for item in line.iter_mut() {
            let child_style = styles.get(item.node_box.node);
            let y_offset = match style.align_items {
                AlignItems::Start | AlignItems::Stretch => 0.0,
                AlignItems::Center => ((line_height - item.outer_h) * 0.5).max(0.0),
                AlignItems::End => (line_height - item.outer_h).max(0.0),
            };
            let dx = (cursor_x + child_style.margin.left) - item.node_box.rect.x;
            let dy = (cursor_y + y_offset + child_style.margin.top) - item.node_box.rect.y;
            offset_box(&mut item.node_box, dx, dy);
            if style.align_items == AlignItems::Stretch && matches!(child_style.height, Size::Auto)
            {
                item.node_box.rect.height =
                    (line_height - child_style.margin.top - child_style.margin.bottom)
                        .max(item.node_box.rect.height);
            }
            cursor_x += child_style.margin.left
                + item.node_box.rect.width
                + child_style.margin.right
                + step_gap;
        }
        lines_cross += line_height;
        cursor_y += line_height;
        for item in line.drain(..) {
            children.push(item.node_box);
        }
    }

    let content_height = specified_height.unwrap_or(0.0).max(lines_cross);
    LayoutBox {
        node: id,
        rect: Rect {
            x: border_x,
            y: border_y,
            width: border_width,
            height: content_height + padding.top + padding.bottom + border.top + border.bottom,
        },
        position: style.position,
        z_index: style.z_index,
        overflow: style.overflow,
        background: style.background,
        background_gradient: style.background_gradient,
        box_shadow: style.box_shadow,
        border,
        border_color: style.border_color,
        border_radius: style.border_radius,
        image: None,
        text: Vec::new(),
        children,
    }
}

/// Add `dx` and `dy` to this box, its text runs, and every descendant.
/// Positive `dy` moves the subtree downward.
fn offset_box(tree: &mut LayoutBox, dx: f32, dy: f32) {
    tree.rect.x += dx;
    tree.rect.y += dy;
    for run in &mut tree.text {
        run.x += dx;
        run.y += dy;
    }
    for child in &mut tree.children {
        offset_box(child, dx, dy);
    }
}

#[allow(clippy::too_many_arguments)]
/// Flush queued inline children into line boxes and advance `cursor_y`.
///
/// A pending list marker is inserted as the first segment. An empty queue
/// with no marker does nothing. `cursor_y` is the top of the next line, y downward.
fn place_inlines(
    doc: &Document,
    styles: &StyleMap,
    block_id: NodeId,
    pending_marker: &mut Option<String>,
    inline_ids: &mut Vec<NodeId>,
    content_x: f32,
    cursor_y: &mut f32,
    content_width: f32,
    text: &mut Vec<TextRun>,
) {
    if inline_ids.is_empty() && pending_marker.is_none() {
        return;
    }
    let mut segments = Vec::new();
    if let Some(marker) = pending_marker.take() {
        let block_style = styles.get(block_id);
        segments.push(Segment {
            node: block_id,
            text: marker,
            font_size: block_style.font_size,
            color: block_style.color,
            underline: false,
        });
    }
    for id in inline_ids.drain(..) {
        gather(doc, styles, id, styles.get(id), &mut segments);
    }
    let (runs, height) = place_lines(&segments, content_x, *cursor_y, content_width);
    text.extend(runs);
    *cursor_y += height;
}

/// Append inline text for `id` and its descendants. `display: none` stops the walk.
/// A `br` becomes a segment whose text is a single newline.
fn gather(
    doc: &Document,
    styles: &StyleMap,
    id: NodeId,
    style: &ComputedStyle,
    out: &mut Vec<Segment>,
) {
    match doc.get(id).data() {
        NodeData::Text(text) => out.push(Segment {
            node: id,
            text: text.clone(),
            font_size: style.font_size,
            color: style.color,
            underline: style.underline,
        }),
        NodeData::Element { tag, .. } => {
            if styles.get(id).display == Display::None {
                return;
            }
            if tag == "br" {
                out.push(Segment {
                    node: id,
                    text: "\n".to_string(),
                    font_size: style.font_size,
                    color: style.color,
                    underline: false,
                });
                return;
            }
            let style = *styles.get(id);
            for child in doc.children(id) {
                gather(doc, styles, *child, &style, out);
            }
        }
        NodeData::Document => {}
    }
}

/// True for an element whose used display is `none`. Text nodes are not skipped.
fn is_skipped(doc: &Document, styles: &StyleMap, id: NodeId) -> bool {
    matches!(doc.get(id).data(), NodeData::Element { .. })
        && styles.get(id).display == Display::None
}

/// True for `img`, `video`, a block or flex element, or an element that
/// contains one of those. Text nodes are inline.
fn is_block_box(doc: &Document, styles: &StyleMap, id: NodeId) -> bool {
    match doc.get(id).data() {
        NodeData::Element { tag, .. } => {
            tag == "img"
                || tag == "video"
                || matches!(
                    styles.get(id).display,
                    Display::Block | Display::Flex | Display::Grid
                )
                || contains_block(doc, styles, id)
        }
        _ => false,
    }
}

/// True when any descendant is `img`, `video`, block, or flex.
fn contains_block(doc: &Document, styles: &StyleMap, id: NodeId) -> bool {
    for child in doc.children(id) {
        if let NodeData::Element { tag, .. } = doc.get(*child).data() {
            if tag == "img"
                || tag == "video"
                || matches!(
                    styles.get(*child).display,
                    Display::Block | Display::Flex | Display::Grid
                )
            {
                return true;
            }
        }
        if contains_block(doc, styles, *child) {
            return true;
        }
    }
    false
}

/// Break `segments` into line boxes inside `max_width`, starting at
/// `(origin_x, origin_y)` with y downward.
///
/// Returns the owned runs and the total height. A line that would pass
/// `max_width` starts a new line. A space at the start of a line is dropped.
/// An empty line from a newline advances 16px. Other lines use the maximum
/// [`line_height`] on that line. Adjacent tokens with the same node, size,
/// color, and underline are merged into one run.
fn place_lines(
    segments: &[Segment],
    origin_x: f32,
    origin_y: f32,
    max_width: f32,
) -> (Vec<TextRun>, f32) {
    let tokens = explode_wide(tokenize(segments), max_width);
    let mut lines: Vec<Vec<Token>> = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    let mut line_width = 0.0;

    for token in tokens {
        if token.text == "\n" {
            trim_trailing_space(&mut current);
            lines.push(std::mem::take(&mut current));
            line_width = 0.0;
            continue;
        }
        if token.text == " " && (current.is_empty() || line_width == 0.0) {
            continue;
        }
        let width = measure(&token.text, token.font_size);
        // Overflow wraps before this token. A wrapping space is discarded.
        if line_width > 0.0 && line_width + width > max_width {
            trim_trailing_space(&mut current);
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            line_width = 0.0;
            if token.text == " " {
                continue;
            }
        }
        line_width += width;
        current.push(token);
    }
    trim_trailing_space(&mut current);
    if !current.is_empty() {
        lines.push(current);
    }

    let mut runs = Vec::new();
    let mut y = origin_y;
    for line in lines {
        if line.is_empty() {
            y += 16.0;
            continue;
        }
        let height = line
            .iter()
            .map(|token| line_height(token.font_size))
            .fold(0.0_f32, f32::max);
        let mut x = origin_x;
        let mut combined: Option<TextRun> = None;
        for token in line {
            let same = combined.as_ref().is_some_and(|run| {
                run.node == token.node
                    && run.font_size == token.font_size
                    && run.color == token.color
                    && run.underline == token.underline
            });
            if same {
                let run = combined.as_mut().expect("checked");
                run.text.push_str(&token.text);
                run.width = measure(&run.text, run.font_size);
            } else {
                if let Some(run) = combined.take() {
                    x += run.width;
                    runs.push(run);
                }
                let width = measure(&token.text, token.font_size);
                combined = Some(TextRun {
                    node: token.node,
                    text: token.text,
                    x,
                    y,
                    width,
                    font_size: token.font_size,
                    color: token.color,
                    underline: token.underline,
                });
            }
        }
        if let Some(run) = combined {
            runs.push(run);
        }
        y += height;
    }
    (runs, y - origin_y)
}

/// Split segments into words and separate space tokens. A segment whose text
/// is exactly `\n` stays one forced-break token. Other whitespace was already
/// collapsed by the HTML parser.
fn tokenize(segments: &[Segment]) -> Vec<Token> {
    let mut tokens = Vec::new();
    for segment in segments {
        if segment.text == "\n" {
            tokens.push(Token {
                node: segment.node,
                text: "\n".to_string(),
                font_size: segment.font_size,
                color: segment.color,
                underline: segment.underline,
            });
            continue;
        }
        let mut word = String::new();
        for ch in segment.text.chars() {
            if ch == ' ' {
                if !word.is_empty() {
                    tokens.push(Token {
                        node: segment.node,
                        text: std::mem::take(&mut word),
                        font_size: segment.font_size,
                        color: segment.color,
                        underline: segment.underline,
                    });
                }
                tokens.push(Token {
                    node: segment.node,
                    text: " ".to_string(),
                    font_size: segment.font_size,
                    color: segment.color,
                    underline: segment.underline,
                });
            } else {
                word.push(ch);
            }
        }
        if !word.is_empty() {
            tokens.push(Token {
                node: segment.node,
                text: word,
                font_size: segment.font_size,
                color: segment.color,
                underline: segment.underline,
            });
        }
    }
    tokens
}

/// Split any word wider than `max_width` into one token per character so
/// [`place_lines`] can wrap it. Spaces and newlines are left whole.
/// A non-positive `max_width` returns the tokens unchanged, so nothing wraps.
fn explode_wide(tokens: Vec<Token>, max_width: f32) -> Vec<Token> {
    if max_width <= 0.0 {
        return tokens;
    }
    let mut out = Vec::new();
    for token in tokens {
        if token.text == " "
            || token.text == "\n"
            || measure(&token.text, token.font_size) <= max_width
        {
            out.push(token);
            continue;
        }
        for ch in token.text.chars() {
            out.push(Token {
                node: token.node,
                text: ch.to_string(),
                font_size: token.font_size,
                color: token.color,
                underline: token.underline,
            });
        }
    }
    out
}

/// Drop space tokens at the end of a line before it is committed.
fn trim_trailing_space(line: &mut Vec<Token>) {
    while line.last().is_some_and(|token| token.text == " ") {
        line.pop();
    }
}

/// Width used by line breaking: half of `font_size` per Unicode scalar.
/// This is not the proportional width from anvil-ttf.
pub fn measure(text: &str, font_size: f32) -> f32 {
    text.chars().count() as f32 * font_size * 0.5
}

/// Proportional width from the cached system font in anvil-ttf.
///
/// The font is loaded once. If that load returns nothing, the anvil-ttf
/// fallback width is used. Layout does not call this; callers that want a
/// measured string do. `text` is borrowed.
pub fn measure_proportional(text: &str, font_size: f32) -> f32 {
    static SYSTEM_FONT: OnceLock<Option<TtfFont>> = OnceLock::new();
    let font = SYSTEM_FONT.get_or_init(|| TtfFont::load_system_font(false, false));
    if let Some(ttf) = font {
        ttf.measure_text(text, font_size)
    } else {
        anvil_ttf::measure_proportional_fallback(text, font_size)
    }
}

/// Line box height: `font_size` times 1.25. The extra quarter is leading,
/// not a separate font metric.
pub fn line_height(font_size: f32) -> f32 {
    font_size * 1.25
}

#[cfg(test)]
mod tests {
    //! Line wrapping, hit testing, flex, images, and table columns.

    use super::*;
    use anvil_dom::parse_html;
    use anvil_style::{compute_styles, compute_styles_for_viewport, parse_css};

    fn render(html: &str, width: f32) -> (Document, LayoutBox) {
        let doc = parse_html(html);
        let styles = compute_styles(&doc, &parse_css(&doc.embedded_css()));
        let tree = layout_document(&doc, &styles, width);
        (doc, tree)
    }

    fn collect_tag<'a>(
        doc: &Document,
        tree: &'a LayoutBox,
        tag: &str,
        out: &mut Vec<&'a LayoutBox>,
    ) {
        if doc.get(tree.node).data().tag() == Some(tag) {
            out.push(tree);
        }
        for child in &tree.children {
            collect_tag(doc, child, tag, out);
        }
    }

    #[test]
    fn wraps_a_line_on_the_width() {
        let segments = vec![Segment {
            node: NodeId::from_raw(1),
            text: "hello hello".to_string(),
            font_size: 10.0,
            color: Color::BLACK,
            underline: false,
        }];
        let (runs, height) = place_lines(&segments, 0.0, 0.0, 30.0);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "hello");
        assert_eq!(runs[1].text, "hello");
        assert_eq!(runs[1].y, 12.5);
        assert_eq!(height, 25.0);
    }

    #[test]
    fn hits_an_inline_text_node_before_its_block() {
        let (doc, tree) = render(
            "<style>html, body, p { margin: 0; padding: 0; font-size: 10px; }</style>
             <p><a href=\"next.html\">go</a></p>",
            100.0,
        );
        let hit = hit_test(&tree, 2.0, 2.0).unwrap();
        assert_eq!(doc.get(hit).data().text(), Some("go"));
        let parent = doc.get(hit).parent().unwrap();
        assert_eq!(doc.get(parent).data().tag(), Some("a"));
    }

    #[test]
    fn stacks_blocks_and_pads_text() {
        let (doc, tree) = render(
            "<style>
                html, body { margin: 0; padding: 0; }
                div { margin: 0; padding: 10px; font-size: 10px; background: #ff0000; }
             </style>
             <div>a</div><div>b</div>",
            100.0,
        );
        let mut divs = Vec::new();
        collect_tag(&doc, &tree, "div", &mut divs);
        assert_eq!(divs.len(), 2);
        assert_eq!(divs[0].rect.y, 0.0);
        assert_eq!(divs[1].rect.y, 32.5);
        assert_eq!(divs[0].text[0].x, 10.0);
        assert_eq!(divs[0].text[0].y, 10.0);
        assert_eq!(divs[0].background, Color::parse("#ff0000"));
    }

    #[test]
    fn viewport_lengths_follow_the_window() {
        let doc = parse_html(
            "<style>html, body, div { margin: 0; padding: 0; }</style>
             <div id=\"box\" style=\"width: 50vw; height: 25vh;\"></div>",
        );
        let styles =
            compute_styles_for_viewport(&doc, &parse_css(&doc.embedded_css()), 200.0, 80.0);
        let tree = layout_document(&doc, &styles, 200.0);
        let mut divs = Vec::new();
        collect_tag(&doc, &tree, "div", &mut divs);
        assert_eq!(divs.len(), 1);
        assert_eq!(divs[0].rect.width, 100.0);
        assert_eq!(divs[0].rect.height, 20.0);
    }

    #[test]
    fn lays_out_flex_row_children_with_gap() {
        let (doc, tree) = render(
            "<style>
                html, body { margin: 0; padding: 0; }
                .row { display: flex; gap: 10px; }
                .item { width: 40px; height: 20px; }
             </style>
             <div class=\"row\"><div class=\"item\">A</div><div class=\"item\">B</div></div>",
            200.0,
        );
        let mut divs = Vec::new();
        collect_tag(&doc, &tree, "div", &mut divs);
        assert_eq!(divs.len(), 3);
        assert_eq!(divs[1].rect.x, 0.0);
        assert_eq!(divs[2].rect.x, 50.0);
    }

    #[test]
    fn sizes_decoded_image_and_measures_proportional_text() {
        let doc = parse_html("<img id=\"logo\" src=\"logo.png\" alt=\"Logo\">");
        let styles = compute_styles(&doc, &parse_css("html, body { margin: 0; padding: 0; }"));
        let logo_id = doc.get_element_by_id("logo").unwrap();
        let mut images = HashMap::new();
        images.insert(logo_id, Image::new(64, 48));

        let tree = layout_document_with_images(&doc, &styles, 300.0, &images);
        let mut imgs = Vec::new();
        collect_tag(&doc, &tree, "img", &mut imgs);
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].rect.width, 64.0);
        assert_eq!(imgs[0].rect.height, 48.0);
        assert!(imgs[0].image.is_some());

        assert!(measure_proportional("MMMM", 16.0) > measure_proportional("iiii", 16.0));
    }

    #[test]
    fn positions_out_of_flow_boxes_and_respects_z_index_and_overflow_in_hit_test() {
        let doc = parse_html(
            "<style>
                html, body { margin: 0; padding: 0; }
                #under { position: absolute; top: 10px; left: 10px; width: 60px; height: 60px; z-index: 1; }
                #over  { position: absolute; top: 10px; left: 10px; width: 60px; height: 60px; z-index: 5; }
                #clip  { width: 40px; height: 40px; overflow: hidden; }
             </style>
             <div id=\"over\"></div>
             <div id=\"under\"></div>
             <div id=\"clip\"><input id=\"field\" type=\"text\" value=\"abc\"></div>",
        );
        let styles = compute_styles(&doc, &parse_css(&doc.embedded_css()));
        let field = doc.get_element_by_id("field").unwrap();
        let over = doc.get_element_by_id("over").unwrap();
        let tree = layout_document_interactive(&doc, &styles, 300.0, &HashMap::new(), Some(field));
        assert_eq!(hit_test(&tree, 20.0, 20.0), Some(over));
    }

    #[test]
    fn lays_out_html_tables_textareas_and_select_controls() {
        let (doc, tree) = render(
            "<style>html, body, table, tr, td { margin: 0; }</style>
             <table>
               <tr><td>Cell 1</td><td>Cell 2</td></tr>
             </table>
             <textarea id=\"notes\" rows=\"4\">Hello</textarea>
             <select id=\"mode\"><option selected>Dark Mode</option></select>",
            400.0,
        );
        let mut tds = Vec::new();
        collect_tag(&doc, &tree, "td", &mut tds);
        assert_eq!(tds.len(), 2);
        assert_eq!(tds[0].rect.x, 0.0);
        assert_eq!(tds[1].rect.x, 200.0);
        assert_eq!(tds[0].rect.height, tds[1].rect.height);

        let mut textareas = Vec::new();
        collect_tag(&doc, &tree, "textarea", &mut textareas);
        assert_eq!(textareas.len(), 1);
        assert!(textareas[0].rect.height >= 4.0 * 16.0);

        let mut selects = Vec::new();
        collect_tag(&doc, &tree, "select", &mut selects);
        assert_eq!(selects.len(), 1);
        assert!(selects[0].text[0].text.contains("Dark Mode"));
    }

    #[test]
    fn resolves_percent_em_rem_and_wraps_a_flex_row_that_grows() {
        let (doc, tree) = render(
            "<style>
                html, body, div { margin: 0; padding: 0; }
                html { font-size: 20px; }
                #host { width: 200px; height: 100px; background: #111111; }
                #pct { width: 50%; height: 40%; background: #ff0000; }
                #em { font-size: 1.5rem; width: 2em; height: 10px; background: #00ff00; }
                #row { display: flex; flex-wrap: wrap; width: 100px; background: #0000ff; }
                #row div { width: 40px; height: 10px; }
                #grow { display: flex; width: 100px; height: 10px; }
                #g1 { width: 20px; height: 10px; flex-grow: 1; background: #abcdef; }
                #g2 { width: 20px; height: 10px; flex-grow: 2; background: #fedcba; }
             </style>
             <div id=\"host\"><div id=\"pct\"></div><div id=\"em\"></div></div>
             <div id=\"row\"><div id=\"a\"></div><div id=\"b\"></div><div id=\"c\"></div></div>
             <div id=\"grow\"><div id=\"g1\"></div><div id=\"g2\"></div></div>",
            200.0,
        );
        let rect = |id: &str| {
            fn find<'a>(doc: &Document, tree: &'a LayoutBox, id: &str) -> Option<&'a LayoutBox> {
                if doc.get(tree.node).data().attr("id") == Some(id) {
                    return Some(tree);
                }
                for child in &tree.children {
                    if let Some(found) = find(doc, child, id) {
                        return Some(found);
                    }
                }
                None
            }
            find(&doc, &tree, id).unwrap().rect
        };
        assert_eq!(rect("pct").width, 100.0);
        assert_eq!(rect("pct").height, 40.0);
        assert_eq!(rect("pct").x, 0.0);
        assert_eq!(rect("pct").y, 0.0);
        assert_eq!(rect("em").width, 60.0);
        assert_eq!(rect("em").height, 10.0);
        assert_eq!(rect("em").y, 40.0);
        assert_eq!(rect("a").x, 0.0);
        assert_eq!(rect("b").x, 40.0);
        assert_eq!(rect("c").x, 0.0);
        assert_eq!(rect("c").y, rect("a").y + 10.0);
        assert_eq!(rect("g1").width, 40.0);
        assert_eq!(rect("g2").width, 60.0);
        assert_eq!(rect("g2").x, rect("g1").x + 40.0);
    }

    #[test]
    fn lays_out_css_grid_tracks_and_calc_lengths() {
        let (doc, tree) = render(
            "<style>
                html, body, div { margin: 0; padding: 0; }
                :root { --grid-gap: 10px; }
                #grid { display: grid; grid-template-columns: 100px 1fr 2fr; gap: var(--grid-gap); width: 420px; }
                .cell { height: 30px; }
                #wide { width: calc(100% - 20px); height: 24px; }
             </style>
             <div id=\"grid\">
               <div id=\"c1\" class=\"cell\"></div>
               <div id=\"c2\" class=\"cell\"></div>
               <div id=\"c3\" class=\"cell\"></div>
               <div id=\"c4\" class=\"cell\"></div>
             </div>
             <div id=\"wide\"></div>",
            500.0,
        );
        let rect = |id: &str| {
            fn find<'a>(doc: &Document, tree: &'a LayoutBox, id: &str) -> Option<&'a LayoutBox> {
                if doc.get(tree.node).data().attr("id") == Some(id) {
                    return Some(tree);
                }
                for child in &tree.children {
                    if let Some(found) = find(doc, child, id) {
                        return Some(found);
                    }
                }
                None
            }
            find(&doc, &tree, id).unwrap().rect
        };
        // 420px total - 20px (two 10px gaps) - 100px fixed = 300px for 3fr -> 1fr = 100px, 2fr = 200px
        assert_eq!(rect("c1").x, 0.0);
        assert_eq!(rect("c1").width, 100.0);
        assert_eq!(rect("c2").x, 110.0);
        assert_eq!(rect("c2").width, 100.0);
        assert_eq!(rect("c3").x, 220.0);
        assert_eq!(rect("c3").width, 200.0);
        assert_eq!(rect("c4").x, 0.0);
        assert_eq!(rect("c4").y, 40.0);
        assert_eq!(rect("wide").width, 480.0);
    }
}
