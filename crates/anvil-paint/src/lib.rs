// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Display list built from a laid-out box tree.
//!
//! Fourth stage of the pipeline. anvil-layout has already placed boxes.
//! This crate walks that tree into a z-ordered [`DisplayList`] and does not
//! draw pixels. anvil-raster consumes the list.
//!
//! Coordinates are borrowed from layout rects: origin at the top-left of the
//! laid-out tree, x to the right, y downward. Lengths are CSS pixels.
//!
//! Paint order for each box is shadow, then background (gradient, rounded
//! rect, or fill plus square borders), then a clip when overflow is hidden,
//! then image and text, then children from low `z-index` to high. `position:
//! fixed` wraps that range in [`DisplayItem::PushFixed`] / [`DisplayItem::PopFixed`].
//!
//! A sticky box against the viewport also adds a [`Layer`] over its range,
//! so the rasterizer can move it for the scroll position. A transformed box
//! adds a transform layer the same way; items inside it keep their
//! untransformed page coordinates. A box with `opacity` below 1 adds an
//! opacity layer. When one box has several, the sticky layer is outermost,
//! then the transform, then the opacity.
//!
//! [`DisplayList::scaled`] multiplies lengths for zoom. Callers use 50%–250%
//! (`0.5`–`2.5`). The clamp inside that method is wider (`0.25`–`4.0`).
//! There is no node cap here; the list is as long as the box tree.

use anvil_layout::{Color, Edge, Image, LayoutBox, Overflow, Position};
pub use anvil_layout::{Rect, StickyConstraint, Transform2D};

/// One painting command. Positions and sizes are CSS pixels, y downward.
///
/// Colors are copied. [`DisplayItem::Text`] and [`DisplayItem::Image`] own
/// their text and image. Other variants store only numbers and colors.
#[derive(Clone, Debug, PartialEq)]
pub enum DisplayItem {
    /// Intersect the current clip with this rectangle. Paired with [`Self::PopClip`].
    /// The rectangle is the content box (border box inset by the border widths).
    PushClip {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
    /// Remove the clip pushed by the matching [`Self::PushClip`].
    PopClip,
    /// Mark the start of a `position: fixed` subtree. The rasterizer treats
    /// items until [`Self::PopFixed`] as fixed to the viewport.
    PushFixed,
    /// End the fixed layer opened by the matching [`Self::PushFixed`].
    PopFixed,
    /// A shadow drawn before the box fill. `x`/`y`/`width`/`height` are the
    /// border box. Offsets and blur are not applied here; they are stored for
    /// the rasterizer. `radius` is the box corner radius.
    BoxShadow {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        offset_x: f32,
        offset_y: f32,
        blur_radius: f32,
        spread_radius: f32,
        radius: f32,
        color: Color,
    },
    /// A two-stop linear gradient filling the border box. `angle_deg` is the
    /// CSS angle (0 is up, 90 is right) and is not converted to a vector here.
    /// `border_width` is the largest of the four border sides, not a per-side width.
    LinearGradientRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        border_width: f32,
        angle_deg: f32,
        start_color: Color,
        end_color: Color,
        border_color: Color,
    },
    /// An axis-aligned fill, also used for square border strips and underlines.
    Rect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
    },
    /// A filled rounded rectangle. Used when `border-radius` is positive and
    /// the box has no gradient. A zero-alpha fill still carries the border.
    RoundedRect {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        border_width: f32,
        fill_color: Color,
        border_color: Color,
    },
    /// An image painted in the content box. `image` is an owned clone.
    /// `border_radius` is the box radius minus the top border, floored at 0.
    Image {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        border_radius: f32,
        image: Image,
    },
    /// A text run. `x`/`y` is the top-left of the run in the layout's y-down
    /// space. `text` is owned. The font size is in CSS pixels.
    Text {
        x: f32,
        y: f32,
        text: String,
        color: Color,
        font_size: f32,
    },
}

/// Owned sequence of [`DisplayItem`]s in paint order.
///
/// `layers` marks item ranges that the rasterizer draws as a group. They are
/// sorted by `start`, outer layers before the layers they contain, and two
/// layers either nest or do not overlap. A list with no layers draws exactly
/// as before layers existed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    pub items: Vec<DisplayItem>,
    pub layers: Vec<Layer>,
}

/// A range `start..end` of [`DisplayList::items`] with one effect.
///
/// The range covers one box and its descendants. For a fixed box it sits
/// inside that box's [`DisplayItem::PushFixed`] / [`DisplayItem::PopFixed`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub start: usize,
    pub end: usize,
    pub effect: LayerEffect,
}

/// What the rasterizer does with a [`Layer`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LayerEffect {
    /// Move the range by [`StickyConstraint::offset`] for the scroll position.
    Sticky(StickyConstraint),
    /// Draw the range through this page-space map, after any enclosing
    /// layers. Clips inside the range are mapped with it; clips outside it
    /// stay screen-aligned.
    Transform(Transform2D),
    /// Draw the range as one group, then blend it at this opacity in
    /// `0..1`, so overlapping items inside do not show through each other.
    Opacity(f32),
}

impl Layer {
    /// This layer with its geometry scaled by `s`, for zoom.
    pub fn scaled(&self, s: f32) -> Self {
        let effect = match self.effect {
            LayerEffect::Sticky(c) => {
                let sr = |r: Rect| Rect {
                    x: r.x * s,
                    y: r.y * s,
                    width: r.width * s,
                    height: r.height * s,
                };
                let so = |v: Option<f32>| v.map(|v| v * s);
                LayerEffect::Sticky(StickyConstraint {
                    rect: sr(c.rect),
                    margin: Edge {
                        top: c.margin.top * s,
                        right: c.margin.right * s,
                        bottom: c.margin.bottom * s,
                        left: c.margin.left * s,
                    },
                    containing: sr(c.containing),
                    top: so(c.top),
                    right: so(c.right),
                    bottom: so(c.bottom),
                    left: so(c.left),
                })
            }
            LayerEffect::Transform(m) => LayerEffect::Transform(m.scaled(s)),
            LayerEffect::Opacity(o) => LayerEffect::Opacity(o),
        };
        Self { effect, ..*self }
    }
}

impl DisplayList {
    /// Return a scaled copy of this display list.
    ///
    /// `scale` is a unitless zoom factor. Values outside `0.25`..=`4.0` are
    /// clamped. A factor within epsilon of `1` returns a clone and does not
    /// rescale. Lengths, radii, blur, spread, border width, and font size are
    /// multiplied. Colors and gradient angles are copied. Text and images are
    /// cloned. [`DisplayItem::PopClip`], [`DisplayItem::PushFixed`], and
    /// [`DisplayItem::PopFixed`] are unchanged. The receiver is borrowed.
    pub fn scaled(&self, scale: f32) -> Self {
        // Wider than the 50%–250% viewport zoom so a caller can probe the edges.
        let s = scale.clamp(0.25, 4.0);
        if (s - 1.0).abs() < f32::EPSILON {
            return self.clone();
        }
        let items = self
            .items
            .iter()
            .map(|item| item.mapped(s, 0.0, 0.0))
            .collect();
        let layers = self.layers.iter().map(|layer| layer.scaled(s)).collect();
        Self { items, layers }
    }
}

impl DisplayItem {
    /// This item scaled by `s` about the origin, then moved by `(tx, ty)`.
    ///
    /// Positions become `p * s + t`. Sizes, radii, blur, spread, border
    /// width, and font size are multiplied by `s`. Colors, gradient angles,
    /// text, and images are copied. Markers are unchanged.
    pub fn mapped(&self, s: f32, tx: f32, ty: f32) -> Self {
        match self {
            DisplayItem::PushClip {
                x,
                y,
                width,
                height,
            } => DisplayItem::PushClip {
                x: x * s + tx,
                y: y * s + ty,
                width: width * s,
                height: height * s,
            },
            DisplayItem::PopClip => DisplayItem::PopClip,
            DisplayItem::PushFixed => DisplayItem::PushFixed,
            DisplayItem::PopFixed => DisplayItem::PopFixed,
            DisplayItem::BoxShadow {
                x,
                y,
                width,
                height,
                offset_x,
                offset_y,
                blur_radius,
                spread_radius,
                radius,
                color,
            } => DisplayItem::BoxShadow {
                x: x * s + tx,
                y: y * s + ty,
                width: width * s,
                height: height * s,
                offset_x: offset_x * s,
                offset_y: offset_y * s,
                blur_radius: blur_radius * s,
                spread_radius: spread_radius * s,
                radius: radius * s,
                color: *color,
            },
            DisplayItem::LinearGradientRect {
                x,
                y,
                width,
                height,
                radius,
                border_width,
                angle_deg,
                start_color,
                end_color,
                border_color,
            } => DisplayItem::LinearGradientRect {
                x: x * s + tx,
                y: y * s + ty,
                width: width * s,
                height: height * s,
                radius: radius * s,
                border_width: border_width * s,
                angle_deg: *angle_deg,
                start_color: *start_color,
                end_color: *end_color,
                border_color: *border_color,
            },
            DisplayItem::Rect {
                x,
                y,
                width,
                height,
                color,
            } => DisplayItem::Rect {
                x: x * s + tx,
                y: y * s + ty,
                width: width * s,
                height: height * s,
                color: *color,
            },
            DisplayItem::RoundedRect {
                x,
                y,
                width,
                height,
                radius,
                border_width,
                fill_color,
                border_color,
            } => DisplayItem::RoundedRect {
                x: x * s + tx,
                y: y * s + ty,
                width: width * s,
                height: height * s,
                radius: radius * s,
                border_width: border_width * s,
                fill_color: *fill_color,
                border_color: *border_color,
            },
            DisplayItem::Image {
                x,
                y,
                width,
                height,
                border_radius,
                image,
            } => DisplayItem::Image {
                x: x * s + tx,
                y: y * s + ty,
                width: width * s,
                height: height * s,
                border_radius: border_radius * s,
                image: image.clone(),
            },
            DisplayItem::Text {
                x,
                y,
                text,
                color,
                font_size,
            } => DisplayItem::Text {
                x: x * s + tx,
                y: y * s + ty,
                text: text.clone(),
                color: *color,
                font_size: font_size * s,
            },
        }
    }
}

/// Build an owned display list by walking `tree` in paint order.
///
/// `tree` is borrowed. Empty boxes (non-positive width or height) contribute
/// no fill, shadow, or image. Children are still visited.
pub fn paint(tree: &LayoutBox) -> DisplayList {
    let mut items = Vec::new();
    let mut layers = Vec::new();
    walk(tree, &mut items, &mut layers, 0);
    // Layers were pushed as boxes finished, inner first. Sorting by start,
    // longest first, then tree depth and push order puts each outer layer
    // before what it contains, even when two cover the same items.
    layers.sort_by_key(|&(depth, order, layer): &(usize, usize, Layer)| {
        (layer.start, std::cmp::Reverse(layer.end), depth, order)
    });
    let layers = layers.into_iter().map(|(_, _, layer)| layer).collect();
    DisplayList { items, layers }
}

/// Append one box and its descendants. See the crate docs for paint order.
///
/// Child order is [`LayoutBox::stacking_key`], then source index, ascending,
/// so a higher `z-index` paints later and ties keep tree order, with sticky
/// boxes after plain siblings of the same `z-index`. The sort is not stable across equal
/// keys beyond that index.
fn walk(
    tree: &LayoutBox,
    items: &mut Vec<DisplayItem>,
    layers: &mut Vec<(usize, usize, Layer)>,
    depth: usize,
) {
    let is_fixed = tree.position == Position::Fixed;
    if is_fixed {
        items.push(DisplayItem::PushFixed);
    }
    let layer_start = items.len();
    if let Some(shadow) = tree.box_shadow
        && tree.rect.width > 0.0
        && tree.rect.height > 0.0
        && shadow.color.a > 0
    {
        items.push(DisplayItem::BoxShadow {
            x: tree.rect.x,
            y: tree.rect.y,
            width: tree.rect.width,
            height: tree.rect.height,
            offset_x: shadow.offset_x,
            offset_y: shadow.offset_y,
            blur_radius: shadow.blur_radius,
            spread_radius: shadow.spread_radius,
            radius: tree.border_radius,
            color: shadow.color,
        });
    }
    if let Some(grad) = tree.background_gradient {
        if tree.rect.width > 0.0 && tree.rect.height > 0.0 {
            let border_width = tree
                .border
                .top
                .max(tree.border.right)
                .max(tree.border.bottom)
                .max(tree.border.left);
            items.push(DisplayItem::LinearGradientRect {
                x: tree.rect.x,
                y: tree.rect.y,
                width: tree.rect.width,
                height: tree.rect.height,
                radius: tree.border_radius,
                border_width,
                angle_deg: grad.angle_deg,
                start_color: grad.start_color,
                end_color: grad.end_color,
                border_color: tree.border_color,
            });
        }
    } else if tree.border_radius > 0.0 && tree.rect.width > 0.0 && tree.rect.height > 0.0 {
        let fill_color = tree.background.unwrap_or(Color {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        });
        let border_width = tree
            .border
            .top
            .max(tree.border.right)
            .max(tree.border.bottom)
            .max(tree.border.left);
        if fill_color.a > 0 || border_width > 0.0 {
            items.push(DisplayItem::RoundedRect {
                x: tree.rect.x,
                y: tree.rect.y,
                width: tree.rect.width,
                height: tree.rect.height,
                radius: tree.border_radius,
                border_width,
                fill_color,
                border_color: tree.border_color,
            });
        }
    } else {
        if let Some(color) = tree.background
            && tree.rect.width > 0.0
            && tree.rect.height > 0.0
        {
            items.push(DisplayItem::Rect {
                x: tree.rect.x,
                y: tree.rect.y,
                width: tree.rect.width,
                height: tree.rect.height,
                color,
            });
        }
        paint_borders(tree, items);
    }

    // Clip is the padding edge, y-down, and only when overflow is hidden.
    // `scroll` and `auto` are already stored as hidden by anvil-style.
    let is_clipped =
        tree.overflow == Overflow::Hidden && tree.rect.width > 0.0 && tree.rect.height > 0.0;
    if is_clipped {
        items.push(DisplayItem::PushClip {
            x: tree.rect.x + tree.border.left,
            y: tree.rect.y + tree.border.top,
            width: (tree.rect.width - tree.border.left - tree.border.right).max(0.0),
            height: (tree.rect.height - tree.border.top - tree.border.bottom).max(0.0),
        });
    }

    if let Some(image) = &tree.image {
        let ix = tree.rect.x + tree.border.left;
        let iy = tree.rect.y + tree.border.top;
        let iw = (tree.rect.width - tree.border.left - tree.border.right).max(0.0);
        let ih = (tree.rect.height - tree.border.top - tree.border.bottom).max(0.0);
        if iw > 0.0 && ih > 0.0 {
            items.push(DisplayItem::Image {
                x: ix,
                y: iy,
                width: iw,
                height: ih,
                border_radius: (tree.border_radius - tree.border.top).max(0.0),
                image: image.clone(),
            });
        }
    }
    for run in &tree.text {
        items.push(DisplayItem::Text {
            x: run.x,
            y: run.y,
            text: run.text.clone(),
            color: run.color,
            font_size: run.font_size,
        });
        // Underline sits below the run top, not on a font baseline.
        if run.underline && run.width > 0.0 {
            items.push(DisplayItem::Rect {
                x: run.x,
                y: run.y + run.font_size * 1.18,
                width: run.width,
                height: 1.0,
                color: run.color,
            });
        }
    }
    let mut order: Vec<(usize, &LayoutBox)> = tree.children.iter().enumerate().collect();
    order.sort_by_key(|(idx, child)| (child.stacking_key(), *idx));
    for (_, child) in order {
        walk(child, items, layers, depth + 1);
    }
    if is_clipped {
        items.push(DisplayItem::PopClip);
    }
    let layer_end = items.len();
    if layer_end > layer_start {
        let effects = [
            tree.sticky.map(LayerEffect::Sticky),
            tree.transform.map(LayerEffect::Transform),
            (tree.opacity < 1.0).then_some(LayerEffect::Opacity(tree.opacity.max(0.0))),
        ];
        for effect in effects.into_iter().flatten() {
            let layer = Layer {
                start: layer_start,
                end: layer_end,
                effect,
            };
            layers.push((depth, layers.len(), layer));
        }
    }
    if is_fixed {
        items.push(DisplayItem::PopFixed);
    }
}

/// Paint each positive border side as its own rectangle, in the box's y-down space.
///
/// Sides are top, bottom, left, right. A side thicker than the box is clamped
/// to the box. Zero-area boxes are skipped. Corner overlaps are not mitered.
/// Rounded boxes and gradients do not call this; they carry one border width.
fn paint_borders(tree: &LayoutBox, items: &mut Vec<DisplayItem>) {
    let b = tree.border;
    let r = tree.rect;
    let color = tree.border_color;
    if r.width <= 0.0 || r.height <= 0.0 {
        return;
    }
    if b.top > 0.0 {
        items.push(DisplayItem::Rect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: b.top.min(r.height),
            color,
        });
    }
    if b.bottom > 0.0 {
        let h = b.bottom.min(r.height);
        items.push(DisplayItem::Rect {
            x: r.x,
            y: r.y + r.height - h,
            width: r.width,
            height: h,
            color,
        });
    }
    if b.left > 0.0 {
        items.push(DisplayItem::Rect {
            x: r.x,
            y: r.y,
            width: b.left.min(r.width),
            height: r.height,
            color,
        });
    }
    if b.right > 0.0 {
        let w = b.right.min(r.width);
        items.push(DisplayItem::Rect {
            x: r.x + r.width - w,
            y: r.y,
            width: w,
            height: r.height,
            color,
        });
    }
}

#[cfg(test)]
mod tests {
    //! One box with a fill, four border strips, a text run, and an underline.

    use super::*;
    use anvil_layout::{Edge, NodeId, Rect, TextRun};

    #[test]
    fn paints_background_borders_text_and_underline() {
        let tree = LayoutBox {
            node: NodeId::from_raw(1),
            rect: Rect {
                x: 1.0,
                y: 2.0,
                width: 30.0,
                height: 10.0,
            },
            position: Position::Static,
            z_index: 0,
            overflow: Overflow::Visible,
            background: Some(Color::BLACK),
            background_gradient: None,
            box_shadow: None,
            border: Edge::all(1.0),
            border_color: Color::BLACK,
            border_radius: 0.0,
            image: None,
            sticky: None,
            transform: None,
            opacity: 1.0,
            text: vec![TextRun {
                node: NodeId::from_raw(2),
                text: "Hi".to_string(),
                x: 1.0,
                y: 2.0,
                width: 10.0,
                font_size: 10.0,
                color: Color::BLACK,
                underline: true,
            }],
            children: Vec::new(),
        };
        let list = paint(&tree);
        assert!(matches!(list.items[0], DisplayItem::Rect { .. }));
        assert!(
            list.items
                .iter()
                .any(|item| matches!(item, DisplayItem::Text { text, .. } if text == "Hi"))
        );
        assert!(list.items.len() >= 7);
    }
}
