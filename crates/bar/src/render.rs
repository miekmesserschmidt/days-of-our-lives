//! SVG frames: parsing (usvg), fitting into a slot, rasterising for cairo, and mapping
//! pointer positions back to SVG units + the ids of the elements under the pointer.

use gtk4::cairo;
use resvg::{tiny_skia, usvg};
use std::sync::{Arc, OnceLock};

fn options() -> &'static usvg::Options<'static> {
    static OPTS: OnceLock<usvg::Options<'static>> = OnceLock::new();
    OPTS.get_or_init(|| {
        let mut opt = usvg::Options::default();
        let mut db = usvg::fontdb::Database::new();
        db.load_system_fonts();
        opt.fontdb = Arc::new(db);
        opt
    })
}

pub struct Frame {
    tree: usvg::Tree,
    /// Cached raster for (width, height, scale).
    cache: Option<((i32, i32, u64), cairo::ImageSurface)>,
}

/// How the frame's picture is placed in a slot of a given size: centred, aspect kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub scale: f64,
    pub dx: f64,
    pub dy: f64,
    pub w: f64,
    pub h: f64,
}

impl Fit {
    pub fn new(svg_w: f64, svg_h: f64, slot_w: f64, slot_h: f64) -> Fit {
        let scale = if svg_w > 0.0 && svg_h > 0.0 { (slot_w / svg_w).min(slot_h / svg_h) } else { 1.0 };
        Fit { scale, dx: (slot_w - svg_w * scale) / 2.0, dy: (slot_h - svg_h * scale) / 2.0, w: svg_w, h: svg_h }
    }

    /// Slot coordinates → SVG user units, and whether the point is on the picture.
    pub fn to_svg(self, x: f64, y: f64) -> (f64, f64, bool) {
        let (sx, sy) = ((x - self.dx) / self.scale, (y - self.dy) / self.scale);
        (sx, sy, sx >= 0.0 && sy >= 0.0 && sx <= self.w && sy <= self.h)
    }

    /// SVG user units → slot coordinates.
    pub fn to_slot(self, x: f64, y: f64) -> (f64, f64) {
        (x * self.scale + self.dx, y * self.scale + self.dy)
    }
}

impl Frame {
    pub fn parse(svg: &str) -> Result<Frame, usvg::Error> {
        Ok(Frame { tree: usvg::Tree::from_str(svg, options())?, cache: None })
    }

    pub fn fit(&self, slot_w: f64, slot_h: f64) -> Fit {
        let s = self.tree.size();
        Fit::new(s.width() as f64, s.height() as f64, slot_w, slot_h)
    }

    /// Ids of elements under the point (SVG units), innermost first.
    pub fn hit_test(&self, x: f64, y: f64) -> Vec<String> {
        let mut out = Vec::new();
        hit_group(self.tree.root(), x as f32, y as f32, &mut out);
        out
    }

    /// Paint into a cairo context of `w × h` logical pixels at display scale `scale`.
    pub fn paint(&mut self, cr: &cairo::Context, w: i32, h: i32, scale: f64) {
        let key = (w, h, scale.to_bits());
        if self.cache.as_ref().map(|c| c.0) != Some(key) {
            self.cache = self.rasterise(w, h, scale).map(|s| (key, s));
        }
        if let Some((_, surface)) = &self.cache {
            cr.save().ok();
            cr.scale(1.0 / scale, 1.0 / scale);
            let _ = cr.set_source_surface(surface, 0.0, 0.0);
            let _ = cr.paint();
            cr.restore().ok();
        }
    }

    fn rasterise(&self, w: i32, h: i32, scale: f64) -> Option<cairo::ImageSurface> {
        let (pw, ph) = (((w as f64) * scale).ceil() as u32, ((h as f64) * scale).ceil() as u32);
        let mut pixmap = tiny_skia::Pixmap::new(pw.max(1), ph.max(1))?;
        let fit = self.fit(w as f64, h as f64);
        let t = tiny_skia::Transform::from_scale(scale as f32, scale as f32)
            .pre_translate(fit.dx as f32, fit.dy as f32)
            .pre_scale(fit.scale as f32, fit.scale as f32);
        resvg::render(&self.tree, t, &mut pixmap.as_mut());
        let data = rgba_to_cairo(pixmap.take());
        let stride = cairo::Format::ARgb32.stride_for_width(pw).ok()?;
        debug_assert_eq!(stride as u32, pw * 4);
        cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, pw as i32, ph as i32, stride).ok()
    }
}

/// tiny-skia gives premultiplied RGBA; cairo's ARGB32 is premultiplied native-endian
/// 0xAARRGGBB, i.e. bytes B, G, R, A on little-endian machines.
pub fn rgba_to_cairo(mut data: Vec<u8>) -> Vec<u8> {
    for px in data.chunks_exact_mut(4) {
        let [r, g, b, a] = [px[0], px[1], px[2], px[3]];
        let v = u32::from_be_bytes([a, r, g, b]);
        px.copy_from_slice(&v.to_ne_bytes());
    }
    data
}

fn contains(r: Option<usvg::Rect>, x: f32, y: f32) -> bool {
    r.is_some_and(|r| x >= r.left() && x <= r.right() && y >= r.top() && y <= r.bottom())
}

/// Hit testing only looks at elements with an `id`; unnamed decorations (labels drawn on
/// top of a rectangle, say) never block what is below them. Children are checked topmost
/// first and the search stops at the first named hit. A named group is hit when any of its
/// descendants' shapes contains the point; its id follows the ids found inside it.
/// Returns (named hit found, any shape contains the point).
fn hit_group(g: &usvg::Group, x: f32, y: f32, out: &mut Vec<String>) -> (bool, bool) {
    let mut geometric = false;
    for node in g.children().iter().rev() {
        let (id, named, geo) = match node {
            usvg::Node::Group(child) => {
                let (named, geo) = hit_group(child, x, y, out);
                (child.id(), named, geo)
            }
            usvg::Node::Path(p) => (p.id(), false, contains(p.abs_bounding_box(), x, y)),
            usvg::Node::Image(i) => (i.id(), false, contains(Some(i.abs_bounding_box()), x, y)),
            usvg::Node::Text(t) => (t.id(), false, contains(Some(t.abs_bounding_box()), x, y)),
        };
        geometric |= geo;
        if named || (geo && !id.is_empty()) {
            if !id.is_empty() {
                out.push(id.to_owned());
            }
            return (true, true);
        }
    }
    (false, geometric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterboxes_and_round_trips() {
        let f = Fit::new(100.0, 50.0, 40.0, 40.0);
        assert_eq!((f.scale, f.dx, f.dy), (0.4, 0.0, 10.0));
        assert_eq!(f.to_svg(20.0, 20.0), (50.0, 25.0, true));
        assert!(!f.to_svg(20.0, 5.0).2);
        assert_eq!(f.to_slot(50.0, 25.0), (20.0, 20.0));
    }

    #[test]
    fn hit_test_returns_innermost_first_and_keeps_hit_areas() {
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 20" width="100" height="20">
            <g id="ws-1"><rect id="hit-1" x="0" y="0" width="50" height="20" fill="#000" fill-opacity="0.01"/>
              <rect id="win-a" x="10" y="5" width="10" height="10" fill="red"/></g>
            <g id="ws-2"><rect x="50" y="0" width="50" height="20" fill="#000" fill-opacity="0.01"/></g>
            <text x="0" y="15" font-size="14" fill="white">a label on top of everything</text>
        </svg>"##;
        let f = Frame::parse(svg).unwrap();
        assert_eq!(f.hit_test(15.0, 10.0), vec!["win-a", "ws-1"]);
        assert_eq!(f.hit_test(30.0, 10.0), vec!["hit-1", "ws-1"]);
        assert_eq!(f.hit_test(70.0, 10.0), vec!["ws-2"]);
        assert!(f.hit_test(70.0, 30.0).is_empty());
    }

    #[test]
    fn converts_premultiplied_rgba_to_cairo_bytes() {
        let out = rgba_to_cairo(vec![1, 2, 3, 4]);
        assert_eq!(u32::from_ne_bytes(out.try_into().unwrap()), 0x04010203);
    }
}
