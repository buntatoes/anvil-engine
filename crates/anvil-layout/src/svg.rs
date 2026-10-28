// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato

//! Inline `<svg>` vector icon and shape rasterizer for `anvil-layout`.
//!
//! Converts `<svg>`, `<g>`, `<path>`, `<rect>`, `<circle>`, `<ellipse>`,
//! `<line>`, `<polyline>`, and `<polygon>` subtrees into anti-aliased RGBA
//! [`Image`] surfaces sized to the element's layout box.

use anvil_dom::{Document, NodeData, NodeId};
use anvil_image::Image;
use anvil_style::{Color, StyleMap};

/// Intrinsic `(width, height)` in CSS pixels for an `<svg>` element when CSS
/// does not specify explicit dimensions.
///
/// Precedence:
/// 1. `width` and `height` HTML attributes on `<svg>` (when numeric/px).
/// 2. `viewBox="min-x min-y width height"` aspect ratio / dimensions.
/// 3. Default `24x24` icon box.
pub fn intrinsic_svg_size(doc: &Document, id: NodeId) -> (f32, f32) {
    let data = doc.get(id).data();
    let attr_w = data.attr("width").and_then(parse_svg_length);
    let attr_h = data.attr("height").and_then(parse_svg_length);
    let vb = data.attr("viewBox").and_then(parse_view_box);

    let (w, h) = match (attr_w, attr_h, vb) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some((_, _, vw, vh))) if vw > 0.0 => (w, (w * vh / vw).max(1.0)),
        (None, Some(h), Some((_, _, vw, vh))) if vh > 0.0 => ((h * vw / vh).max(1.0), h),
        (Some(w), None, _) => (w, w),
        (None, Some(h), _) => (h, h),
        (None, None, Some((_, _, vw, vh))) => {
            if vw <= 64.0 && vh <= 64.0 {
                (vw.max(12.0), vh.max(12.0))
            } else {
                let scale = (24.0 / vw.max(vh)).min(1.0);
                ((vw * scale).max(16.0), (vh * scale).max(16.0))
            }
        }
        (None, None, None) => (24.0, 24.0),
    };
    (w.clamp(1.0, 1024.0), h.clamp(1.0, 1024.0))
}

/// Rasterize an `<svg>` DOM subtree into an RGBA [`Image`] of `(width, height)` pixels.
pub fn render_svg_node(
    doc: &Document,
    styles: &StyleMap,
    svg_id: NodeId,
    width: u32,
    height: u32,
) -> Option<Image> {
    let w = width.clamp(1, 1024);
    let h = height.clamp(1, 1024);
    let data = doc.get(svg_id).data();
    let (vb_x, vb_y, vb_w, vb_h) = data
        .attr("viewBox")
        .and_then(parse_view_box)
        .unwrap_or_else(|| {
            let aw = data
                .attr("width")
                .and_then(parse_svg_length)
                .unwrap_or(w as f32)
                .max(1.0);
            let ah = data
                .attr("height")
                .and_then(parse_svg_length)
                .unwrap_or(h as f32)
                .max(1.0);
            (0.0, 0.0, aw, ah)
        });
    if vb_w <= 0.0 || vb_h <= 0.0 {
        return None;
    }

    let scale_x = (w as f32) / vb_w;
    let scale_y = (h as f32) / vb_h;
    let scale = scale_x.min(scale_y);
    let offset_x = ((w as f32) - vb_w * scale) * 0.5 - vb_x * scale;
    let offset_y = ((h as f32) - vb_h * scale) * 0.5 - vb_y * scale;
    let xform = SvgTransform {
        scale,
        offset_x,
        offset_y,
    };

    let base_color = styles.get(svg_id).color;
    let inherited = InheritedPaint {
        fill: PaintAttr::DefaultColor,
        stroke: PaintAttr::None,
        stroke_width: 1.5,
        opacity: 1.0,
        current_color: base_color,
    }
    .apply_node(doc, styles, svg_id);

    let mut pixels = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut drew_any = false;
    for &child in doc.children(svg_id) {
        if render_child(doc, styles, child, &xform, &inherited, w, h, &mut pixels) {
            drew_any = true;
        }
    }
    if !drew_any {
        return None;
    }
    Image::from_rgba(w, h, pixels).ok()
}

#[derive(Clone, Copy)]
struct SvgTransform {
    scale: f32,
    offset_x: f32,
    offset_y: f32,
}

impl SvgTransform {
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            x * self.scale + self.offset_x,
            y * self.scale + self.offset_y,
        )
    }
}

#[derive(Clone, Copy)]
enum PaintAttr {
    DefaultColor,
    None,
    Solid(Color),
}

#[derive(Clone, Copy)]
struct InheritedPaint {
    fill: PaintAttr,
    stroke: PaintAttr,
    stroke_width: f32,
    opacity: f32,
    current_color: Color,
}

impl InheritedPaint {
    fn apply_node(self, doc: &Document, styles: &StyleMap, id: NodeId) -> Self {
        let mut next = self;
        let style = styles.get(id);
        next.current_color = style.color;
        let data = doc.get(id).data();
        if let Some(fill_raw) = data.attr("fill").or_else(|| inline_prop(data, "fill")) {
            next.fill = parse_paint_attr(fill_raw, next.current_color);
        }
        if let Some(stroke_raw) = data.attr("stroke").or_else(|| inline_prop(data, "stroke")) {
            next.stroke = parse_paint_attr(stroke_raw, next.current_color);
        }
        if let Some(sw_raw) = data
            .attr("stroke-width")
            .or_else(|| inline_prop(data, "stroke-width"))
            && let Some(sw) = parse_svg_length(sw_raw)
        {
            next.stroke_width = sw.clamp(0.25, 64.0);
        }
        if let Some(op_raw) = data
            .attr("opacity")
            .or_else(|| inline_prop(data, "opacity"))
            && let Ok(op) = op_raw.trim().parse::<f32>()
        {
            next.opacity = (next.opacity * op.clamp(0.0, 1.0)).clamp(0.0, 1.0);
        }
        next
    }

    fn resolved_fill(&self, doc: &Document, id: NodeId) -> Option<Color> {
        let data = doc.get(id).data();
        let fill_op = data
            .attr("fill-opacity")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        let c = match self.fill {
            PaintAttr::None => return None,
            PaintAttr::DefaultColor => self.current_color,
            PaintAttr::Solid(c) => c,
        };
        let a = ((c.a as f32) * self.opacity * fill_op)
            .round()
            .clamp(0.0, 255.0) as u8;
        (a > 0).then_some(Color { a, ..c })
    }

    fn resolved_stroke(&self, doc: &Document, id: NodeId) -> Option<(Color, f32)> {
        let data = doc.get(id).data();
        let stroke_op = data
            .attr("stroke-opacity")
            .and_then(|v| v.trim().parse::<f32>().ok())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        let c = match self.stroke {
            PaintAttr::None | PaintAttr::DefaultColor => return None,
            PaintAttr::Solid(c) => c,
        };
        let a = ((c.a as f32) * self.opacity * stroke_op)
            .round()
            .clamp(0.0, 255.0) as u8;
        (a > 0 && self.stroke_width > 0.0).then_some((Color { a, ..c }, self.stroke_width))
    }
}

fn inline_prop<'a>(data: &'a NodeData, name: &str) -> Option<&'a str> {
    let raw = data.attr("style")?;
    for decl in raw.split(';') {
        let (k, v) = decl.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim());
        }
    }
    None
}

fn parse_paint_attr(raw: &str, current_color: Color) -> PaintAttr {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") || trimmed.starts_with("url(") {
        return PaintAttr::None;
    }
    if trimmed.eq_ignore_ascii_case("currentColor") || trimmed.eq_ignore_ascii_case("inherit") {
        return PaintAttr::Solid(current_color);
    }
    if let Some(color) = Color::parse(trimmed) {
        return PaintAttr::Solid(color);
    }
    PaintAttr::Solid(current_color)
}

#[derive(Clone)]
struct Subpath {
    points: Vec<(f32, f32)>,
    closed: bool,
}

fn render_child(
    doc: &Document,
    styles: &StyleMap,
    id: NodeId,
    xform: &SvgTransform,
    parent_paint: &InheritedPaint,
    w: u32,
    h: u32,
    pixels: &mut [u8],
) -> bool {
    let Some(tag) = doc.get(id).data().tag() else {
        return false;
    };
    if matches!(
        tag,
        "defs"
            | "clippath"
            | "mask"
            | "lineargradient"
            | "radialgradient"
            | "pattern"
            | "filter"
            | "marker"
            | "style"
            | "title"
            | "desc"
            | "metadata"
            | "symbol"
    ) {
        return false;
    }
    let paint = parent_paint.apply_node(doc, styles, id);
    let mut local_xform = *xform;
    if let Some(tf) = doc.get(id).data().attr("transform")
        && let Some((tx, ty)) = parse_translate(tf)
    {
        local_xform.offset_x += tx * local_xform.scale;
        local_xform.offset_y += ty * local_xform.scale;
    }

    if tag == "g" || tag == "svg" || tag == "a" {
        let mut any = false;
        for &child in doc.children(id) {
            if render_child(doc, styles, child, &local_xform, &paint, w, h, pixels) {
                any = true;
            }
        }
        return any;
    }

    let subpaths = match tag {
        "path" => doc
            .get(id)
            .data()
            .attr("d")
            .map(|d| parse_svg_path(d, &local_xform))
            .unwrap_or_default(),
        "rect" => build_rect_subpath(doc.get(id).data(), &local_xform),
        "circle" => build_circle_subpath(doc.get(id).data(), &local_xform),
        "ellipse" => build_ellipse_subpath(doc.get(id).data(), &local_xform),
        "line" => build_line_subpath(doc.get(id).data(), &local_xform),
        "polyline" => build_poly_subpath(doc.get(id).data(), &local_xform, false),
        "polygon" => build_poly_subpath(doc.get(id).data(), &local_xform, true),
        _ => Vec::new(),
    };

    if subpaths.is_empty() {
        return false;
    }

    let mut drew = false;
    if tag != "line"
        && let Some(fill_color) = paint.resolved_fill(doc, id)
    {
        let even_odd = doc
            .get(id)
            .data()
            .attr("fill-rule")
            .is_some_and(|r| r.trim().eq_ignore_ascii_case("evenodd"));
        fill_subpaths(&subpaths, fill_color, even_odd, w, h, pixels);
        drew = true;
    }
    if let Some((stroke_color, sw)) = paint.resolved_stroke(doc, id) {
        let px_sw = (sw * local_xform.scale).max(1.0);
        stroke_subpaths(&subpaths, stroke_color, px_sw, w, h, pixels);
        drew = true;
    }
    drew
}

fn build_rect_subpath(data: &NodeData, xform: &SvgTransform) -> Vec<Subpath> {
    let x = data.attr("x").and_then(parse_svg_length).unwrap_or(0.0);
    let y = data.attr("y").and_then(parse_svg_length).unwrap_or(0.0);
    let w = data.attr("width").and_then(parse_svg_length).unwrap_or(0.0);
    let h = data
        .attr("height")
        .and_then(parse_svg_length)
        .unwrap_or(0.0);
    if w <= 0.0 || h <= 0.0 {
        return Vec::new();
    }
    vec![Subpath {
        points: vec![
            xform.map(x, y),
            xform.map(x + w, y),
            xform.map(x + w, y + h),
            xform.map(x, y + h),
        ],
        closed: true,
    }]
}

fn build_circle_subpath(data: &NodeData, xform: &SvgTransform) -> Vec<Subpath> {
    let cx = data.attr("cx").and_then(parse_svg_length).unwrap_or(0.0);
    let cy = data.attr("cy").and_then(parse_svg_length).unwrap_or(0.0);
    let r = data.attr("r").and_then(parse_svg_length).unwrap_or(0.0);
    ellipse_points(cx, cy, r, r, xform)
}

fn build_ellipse_subpath(data: &NodeData, xform: &SvgTransform) -> Vec<Subpath> {
    let cx = data.attr("cx").and_then(parse_svg_length).unwrap_or(0.0);
    let cy = data.attr("cy").and_then(parse_svg_length).unwrap_or(0.0);
    let rx = data.attr("rx").and_then(parse_svg_length).unwrap_or(0.0);
    let ry = data.attr("ry").and_then(parse_svg_length).unwrap_or(0.0);
    ellipse_points(cx, cy, rx, ry, xform)
}

fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32, xform: &SvgTransform) -> Vec<Subpath> {
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    let steps = 32usize;
    let mut points = Vec::with_capacity(steps);
    for i in 0..steps {
        let theta = (i as f32) * std::f32::consts::TAU / (steps as f32);
        let x = cx + rx * theta.cos();
        let y = cy + ry * theta.sin();
        points.push(xform.map(x, y));
    }
    vec![Subpath {
        points,
        closed: true,
    }]
}

fn build_line_subpath(data: &NodeData, xform: &SvgTransform) -> Vec<Subpath> {
    let x1 = data.attr("x1").and_then(parse_svg_length).unwrap_or(0.0);
    let y1 = data.attr("y1").and_then(parse_svg_length).unwrap_or(0.0);
    let x2 = data.attr("x2").and_then(parse_svg_length).unwrap_or(0.0);
    let y2 = data.attr("y2").and_then(parse_svg_length).unwrap_or(0.0);
    vec![Subpath {
        points: vec![xform.map(x1, y1), xform.map(x2, y2)],
        closed: false,
    }]
}

fn build_poly_subpath(data: &NodeData, xform: &SvgTransform, closed: bool) -> Vec<Subpath> {
    let Some(raw) = data.attr("points") else {
        return Vec::new();
    };
    let nums = collect_numbers(raw);
    let mut points = Vec::with_capacity(nums.len() / 2);
    for pair in nums.chunks_exact(2) {
        points.push(xform.map(pair[0], pair[1]));
    }
    if points.len() < 2 {
        return Vec::new();
    }
    vec![Subpath { points, closed }]
}

fn parse_svg_path(d: &str, xform: &SvgTransform) -> Vec<Subpath> {
    let mut subpaths: Vec<Subpath> = Vec::new();
    let mut current: Vec<(f32, f32)> = Vec::new();
    let mut cur_x = 0.0f32;
    let mut cur_y = 0.0f32;
    let mut start_x = 0.0f32;
    let mut start_y = 0.0f32;
    let mut prev_ctrl: Option<(f32, f32)> = None;

    let tokens = tokenize_path(d);
    let mut idx = 0usize;
    let mut cmd = 'M';

    while idx < tokens.len() {
        match tokens[idx] {
            PathToken::Cmd(c) => {
                cmd = c;
                idx += 1;
                if matches!(cmd, 'Z' | 'z') {
                    if !current.is_empty() {
                        subpaths.push(Subpath {
                            points: std::mem::take(&mut current),
                            closed: true,
                        });
                    }
                    cur_x = start_x;
                    cur_y = start_y;
                    prev_ctrl = None;
                }
            }
            PathToken::Num(_) => {
                let take = |idx: &mut usize, tokens: &[PathToken]| -> Option<f32> {
                    if let Some(PathToken::Num(n)) = tokens.get(*idx) {
                        *idx += 1;
                        Some(*n)
                    } else {
                        None
                    }
                };
                match cmd {
                    'M' | 'm' => {
                        let (Some(x), Some(y)) = (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if !current.is_empty() {
                            subpaths.push(Subpath {
                                points: std::mem::take(&mut current),
                                closed: false,
                            });
                        }
                        if cmd == 'm' {
                            cur_x += x;
                            cur_y += y;
                        } else {
                            cur_x = x;
                            cur_y = y;
                        }
                        start_x = cur_x;
                        start_y = cur_y;
                        current.push(xform.map(cur_x, cur_y));
                        cmd = if cmd == 'm' { 'l' } else { 'L' };
                        prev_ctrl = None;
                    }
                    'L' | 'l' => {
                        let (Some(x), Some(y)) = (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if cmd == 'l' {
                            cur_x += x;
                            cur_y += y;
                        } else {
                            cur_x = x;
                            cur_y = y;
                        }
                        current.push(xform.map(cur_x, cur_y));
                        prev_ctrl = None;
                    }
                    'H' | 'h' => {
                        let Some(x) = take(&mut idx, &tokens) else {
                            break;
                        };
                        cur_x = if cmd == 'h' { cur_x + x } else { x };
                        current.push(xform.map(cur_x, cur_y));
                        prev_ctrl = None;
                    }
                    'V' | 'v' => {
                        let Some(y) = take(&mut idx, &tokens) else {
                            break;
                        };
                        cur_y = if cmd == 'v' { cur_y + y } else { y };
                        current.push(xform.map(cur_x, cur_y));
                        prev_ctrl = None;
                    }
                    'C' | 'c' => {
                        let (Some(mut x1), Some(mut y1)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        let (Some(mut x2), Some(mut y2)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        let (Some(mut x), Some(mut y)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if cmd == 'c' {
                            x1 += cur_x;
                            y1 += cur_y;
                            x2 += cur_x;
                            y2 += cur_y;
                            x += cur_x;
                            y += cur_y;
                        }
                        flatten_cubic(cur_x, cur_y, x1, y1, x2, y2, x, y, xform, &mut current);
                        prev_ctrl = Some((x2, y2));
                        cur_x = x;
                        cur_y = y;
                    }
                    'S' | 's' => {
                        let (Some(mut x2), Some(mut y2)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        let (Some(mut x), Some(mut y)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if cmd == 's' {
                            x2 += cur_x;
                            y2 += cur_y;
                            x += cur_x;
                            y += cur_y;
                        }
                        let (x1, y1) = match prev_ctrl {
                            Some((px, py)) => (2.0 * cur_x - px, 2.0 * cur_y - py),
                            None => (cur_x, cur_y),
                        };
                        flatten_cubic(cur_x, cur_y, x1, y1, x2, y2, x, y, xform, &mut current);
                        prev_ctrl = Some((x2, y2));
                        cur_x = x;
                        cur_y = y;
                    }
                    'Q' | 'q' => {
                        let (Some(mut x1), Some(mut y1)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        let (Some(mut x), Some(mut y)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if cmd == 'q' {
                            x1 += cur_x;
                            y1 += cur_y;
                            x += cur_x;
                            y += cur_y;
                        }
                        flatten_quad(cur_x, cur_y, x1, y1, x, y, xform, &mut current);
                        prev_ctrl = Some((x1, y1));
                        cur_x = x;
                        cur_y = y;
                    }
                    'T' | 't' => {
                        let (Some(mut x), Some(mut y)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if cmd == 't' {
                            x += cur_x;
                            y += cur_y;
                        }
                        let (x1, y1) = match prev_ctrl {
                            Some((px, py)) => (2.0 * cur_x - px, 2.0 * cur_y - py),
                            None => (cur_x, cur_y),
                        };
                        flatten_quad(cur_x, cur_y, x1, y1, x, y, xform, &mut current);
                        prev_ctrl = Some((x1, y1));
                        cur_x = x;
                        cur_y = y;
                    }
                    'A' | 'a' => {
                        let (Some(rx), Some(ry)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        let (Some(_rot), Some(large_arc), Some(sweep)) = (
                            take(&mut idx, &tokens),
                            take(&mut idx, &tokens),
                            take(&mut idx, &tokens),
                        ) else {
                            break;
                        };
                        let (Some(mut x), Some(mut y)) =
                            (take(&mut idx, &tokens), take(&mut idx, &tokens))
                        else {
                            break;
                        };
                        if cmd == 'a' {
                            x += cur_x;
                            y += cur_y;
                        }
                        flatten_arc(
                            cur_x,
                            cur_y,
                            rx.abs(),
                            ry.abs(),
                            large_arc != 0.0,
                            sweep != 0.0,
                            x,
                            y,
                            xform,
                            &mut current,
                        );
                        cur_x = x;
                        cur_y = y;
                        prev_ctrl = None;
                    }
                    _ => {
                        idx += 1;
                    }
                }
            }
        }
    }
    if !current.is_empty() {
        subpaths.push(Subpath {
            points: current,
            closed: false,
        });
    }
    subpaths
}

fn flatten_cubic(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    xform: &SvgTransform,
    out: &mut Vec<(f32, f32)>,
) {
    let steps = 10usize;
    for i in 1..=steps {
        let t = (i as f32) / (steps as f32);
        let mt = 1.0 - t;
        let x = mt * mt * mt * x0 + 3.0 * mt * mt * t * x1 + 3.0 * mt * t * t * x2 + t * t * t * x3;
        let y = mt * mt * mt * y0 + 3.0 * mt * mt * t * y1 + 3.0 * mt * t * t * y2 + t * t * t * y3;
        out.push(xform.map(x, y));
    }
}

fn flatten_quad(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    xform: &SvgTransform,
    out: &mut Vec<(f32, f32)>,
) {
    let steps = 8usize;
    for i in 1..=steps {
        let t = (i as f32) / (steps as f32);
        let mt = 1.0 - t;
        let x = mt * mt * x0 + 2.0 * mt * t * x1 + t * t * x2;
        let y = mt * mt * y0 + 2.0 * mt * t * y1 + t * t * y2;
        out.push(xform.map(x, y));
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_arc(
    x0: f32,
    y0: f32,
    mut rx: f32,
    mut ry: f32,
    large_arc: bool,
    sweep: bool,
    x: f32,
    y: f32,
    xform: &SvgTransform,
    out: &mut Vec<(f32, f32)>,
) {
    if rx <= 0.001 || ry <= 0.001 || ((x - x0).abs() < 0.001 && (y - y0).abs() < 0.001) {
        out.push(xform.map(x, y));
        return;
    }
    let dx2 = (x0 - x) * 0.5;
    let dy2 = (y0 - y) * 0.5;
    let lambda = (dx2 * dx2) / (rx * rx) + (dy2 * dy2) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let rx2 = rx * rx;
    let ry2 = ry * ry;
    let num = (rx2 * ry2 - rx2 * dy2 * dy2 - ry2 * dx2 * dx2).max(0.0);
    let den = (rx2 * dy2 * dy2 + ry2 * dx2 * dx2).max(0.0001);
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let coef = sign * (num / den).sqrt();
    let cxp = coef * (rx * dy2 / ry);
    let cyp = coef * -(ry * dx2 / rx);
    let cx = cxp + (x0 + x) * 0.5;
    let cy = cyp + (y0 + y) * 0.5;

    let start_ang = ((y0 - cy) / ry).atan2((x0 - cx) / rx);
    let end_ang = ((y - cy) / ry).atan2((x - cx) / rx);
    let mut delta = end_ang - start_ang;
    if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    } else if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    }
    let steps = 12usize;
    for i in 1..=steps {
        let ang = start_ang + delta * ((i as f32) / (steps as f32));
        out.push(xform.map(cx + rx * ang.cos(), cy + ry * ang.sin()));
    }
}

fn fill_subpaths(
    subpaths: &[Subpath],
    color: Color,
    even_odd: bool,
    w: u32,
    h: u32,
    pixels: &mut [u8],
) {
    let mut edges: Vec<(f32, f32, f32, f32)> = Vec::new();
    for sp in subpaths {
        if sp.points.len() < 3 {
            continue;
        }
        for i in 0..sp.points.len() {
            let p0 = sp.points[i];
            let p1 = sp.points[(i + 1) % sp.points.len()];
            if (p0.1 - p1.1).abs() > 1e-4 {
                edges.push((p0.0, p0.1, p1.0, p1.1));
            }
        }
    }
    if edges.is_empty() {
        return;
    }

    let mut crossings: Vec<(f32, i32)> = Vec::new();
    let offsets = [0.125f32, 0.375, 0.625, 0.875];
    let mut row_cov = vec![0u8; w as usize];

    for py in 0..h {
        row_cov.fill(0);
        for &sub_y in &offsets {
            let y = (py as f32) + sub_y;
            crossings.clear();
            for &(x0, y0, x1, y1) in &edges {
                let (ymin, ymax, dir) = if y0 < y1 { (y0, y1, 1) } else { (y1, y0, -1) };
                if y >= ymin && y < ymax {
                    let t = (y - y0) / (y1 - y0);
                    let ix = x0 + t * (x1 - x0);
                    crossings.push((ix, dir));
                }
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if even_odd {
                for pair in crossings.chunks_exact(2) {
                    add_span_coverage(&mut row_cov, pair[0].0, pair[1].0, w);
                }
            } else {
                let mut winding = 0i32;
                let mut span_start = 0.0f32;
                for &(ix, dir) in &crossings {
                    if winding == 0 {
                        span_start = ix;
                    }
                    winding += dir;
                    if winding == 0 {
                        add_span_coverage(&mut row_cov, span_start, ix, w);
                    }
                }
            }
        }
        for (px, &cov) in row_cov.iter().enumerate() {
            if cov > 0 {
                let alpha = ((color.a as u32) * (cov as u32) / 4).min(255) as u8;
                blend_pixel(pixels, w, px as u32, py, Color { a: alpha, ..color });
            }
        }
    }
}

fn add_span_coverage(row_cov: &mut [u8], x0: f32, x1: f32, w: u32) {
    let start = x0.floor().max(0.0) as u32;
    let end = x1.ceil().min(w as f32) as u32;
    for px in start..end {
        let center = (px as f32) + 0.5;
        if center >= x0 && center <= x1 {
            row_cov[px as usize] = row_cov[px as usize].saturating_add(1);
        }
    }
}

fn stroke_subpaths(
    subpaths: &[Subpath],
    color: Color,
    stroke_width: f32,
    w: u32,
    h: u32,
    pixels: &mut [u8],
) {
    let half = (stroke_width * 0.5).max(0.5);
    for sp in subpaths {
        if sp.points.len() < 2 {
            continue;
        }
        let seg_count = if sp.closed {
            sp.points.len()
        } else {
            sp.points.len() - 1
        };
        for i in 0..seg_count {
            let p0 = sp.points[i];
            let p1 = sp.points[(i + 1) % sp.points.len()];
            draw_segment(p0, p1, half, color, w, h, pixels);
        }
    }
}

fn draw_segment(
    p0: (f32, f32),
    p1: (f32, f32),
    half_width: f32,
    color: Color,
    w: u32,
    h: u32,
    pixels: &mut [u8],
) {
    let pad = half_width + 1.0;
    let min_x = p0.0.min(p1.0 - pad).floor().max(0.0) as u32;
    let max_x = (p0.0.max(p1.0) + pad).ceil().min(w as f32) as u32;
    let min_y = (p0.1.min(p1.1) - pad).floor().max(0.0) as u32;
    let max_y = (p0.1.max(p1.1) + pad).ceil().min(h as f32) as u32;
    let dx = p1.0 - p0.0;
    let dy = p1.1 - p0.1;
    let len_sq = dx * dx + dy * dy;

    for py in min_y..max_y {
        let cy = (py as f32) + 0.5;
        for px in min_x..max_x {
            let cx = (px as f32) + 0.5;
            let t = if len_sq > 1e-6 {
                (((cx - p0.0) * dx + (cy - p0.1) * dy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let proj_x = p0.0 + t * dx;
            let proj_y = p0.1 + t * dy;
            let dist = ((cx - proj_x).powi(2) + (cy - proj_y).powi(2)).sqrt();
            let cov = (half_width + 0.5 - dist).clamp(0.0, 1.0);
            if cov > 0.0 {
                let alpha = ((color.a as f32) * cov).round().clamp(0.0, 255.0) as u8;
                blend_pixel(pixels, w, px, py, Color { a: alpha, ..color });
            }
        }
    }
}

fn blend_pixel(pixels: &mut [u8], w: u32, x: u32, y: u32, src: Color) {
    if src.a == 0 {
        return;
    }
    let idx = ((y * w + x) as usize) * 4;
    let Some(dst) = pixels.get_mut(idx..idx + 4) else {
        return;
    };
    let sa = (src.a as f32) / 255.0;
    let da = (dst[3] as f32) / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= 1e-5 {
        return;
    }
    dst[0] = (((src.r as f32) * sa + (dst[0] as f32) * da * (1.0 - sa)) / out_a)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[1] = (((src.g as f32) * sa + (dst[1] as f32) * da * (1.0 - sa)) / out_a)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[2] = (((src.b as f32) * sa + (dst[2] as f32) * da * (1.0 - sa)) / out_a)
        .round()
        .clamp(0.0, 255.0) as u8;
    dst[3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[derive(Clone, Copy)]
enum PathToken {
    Cmd(char),
    Num(f32),
}

fn tokenize_path(d: &str) -> Vec<PathToken> {
    let mut out = Vec::new();
    let bytes = d.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() || b == b',' {
            i += 1;
            continue;
        }
        if b.is_ascii_alphabetic() {
            out.push(PathToken::Cmd(b as char));
            i += 1;
            continue;
        }
        let start = i;
        if bytes[i] == b'+' || bytes[i] == b'-' {
            i += 1;
        }
        let mut seen_dot = false;
        while i < bytes.len() {
            let c = bytes[i];
            if c.is_ascii_digit() {
                i += 1;
            } else if c == b'.' && !seen_dot {
                seen_dot = true;
                i += 1;
            } else if (c == b'e' || c == b'E') && i > start {
                i += 1;
                if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                    i += 1;
                }
            } else {
                break;
            }
        }
        if i > start {
            if let Ok(val) = d[start..i].parse::<f32>() {
                out.push(PathToken::Num(val));
            }
        } else {
            i += 1;
        }
    }
    out
}

fn collect_numbers(raw: &str) -> Vec<f32> {
    tokenize_path(raw)
        .into_iter()
        .filter_map(|t| match t {
            PathToken::Num(n) => Some(n),
            PathToken::Cmd(_) => None,
        })
        .collect()
}

fn parse_svg_length(raw: &str) -> Option<f32> {
    let s = raw.trim();
    if s.ends_with('%') {
        return None;
    }
    let num = s
        .trim_end_matches("px")
        .trim_end_matches("pt")
        .trim_end_matches("em")
        .trim_end_matches("rem")
        .trim();
    num.parse::<f32>().ok().filter(|v| *v > 0.0)
}

fn parse_view_box(raw: &str) -> Option<(f32, f32, f32, f32)> {
    let nums = collect_numbers(raw);
    if nums.len() >= 4 {
        Some((nums[0], nums[1], nums[2], nums[3]))
    } else {
        None
    }
}

fn parse_translate(raw: &str) -> Option<(f32, f32)> {
    let idx = raw.find("translate(")?;
    let rest = &raw[idx + "translate(".len()..];
    let end = rest.find(')')?;
    let nums = collect_numbers(&rest[..end]);
    match nums.as_slice() {
        [tx] => Some((*tx, 0.0)),
        [tx, ty, ..] => Some((*tx, *ty)),
        _ => None,
    }
}
