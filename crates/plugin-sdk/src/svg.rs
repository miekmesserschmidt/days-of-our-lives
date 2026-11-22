//! A tiny SVG string builder. Attributes are passed as raw strings, e.g. `r#"fill="#fff" rx="2""#`.

use std::fmt::Write;

/// Escape text for use in SVG text content or attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Check a colour value from a config file: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`,
/// or a plain CSS name like `white`.
pub fn check_color(c: &str) -> Result<(), String> {
    let ok = match c.strip_prefix('#') {
        Some(hex) => matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit()),
        None => !c.is_empty() && c.chars().all(|c| c.is_ascii_alphabetic()),
    };
    if ok { Ok(()) } else { Err(format!("invalid colour {c:?}")) }
}

/// `n` formatted with at most 2 decimals and no trailing zeros, to keep SVGs small and stable.
pub fn num(n: f64) -> String {
    let s = format!("{n:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

pub struct Svg {
    body: String,
    width: f64,
    height: f64,
}

impl Svg {
    pub fn new(width: f64, height: f64) -> Self {
        Svg { body: String::new(), width, height }
    }

    pub fn raw(&mut self, s: &str) -> &mut Self {
        self.body.push_str(s);
        self
    }

    pub fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, attrs: &str) -> &mut Self {
        let _ = write!(self.body, r#"<rect x="{}" y="{}" width="{}" height="{}" {attrs}/>"#, num(x), num(y), num(w.max(0.0)), num(h.max(0.0)));
        self
    }

    pub fn line(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, attrs: &str) -> &mut Self {
        let _ = write!(self.body, r#"<line x1="{}" y1="{}" x2="{}" y2="{}" {attrs}/>"#, num(x1), num(y1), num(x2), num(y2));
        self
    }

    /// Text is escaped here.
    pub fn text(&mut self, x: f64, y: f64, text: &str, attrs: &str) -> &mut Self {
        let _ = write!(self.body, r#"<text x="{}" y="{}" {attrs}>{}</text>"#, num(x), num(y), esc(text));
        self
    }

    pub fn image(&mut self, x: f64, y: f64, w: f64, h: f64, href: &str, attrs: &str) -> &mut Self {
        let _ = write!(self.body, r#"<image x="{}" y="{}" width="{}" height="{}" href="{}" {attrs}/>"#, num(x), num(y), num(w), num(h), esc(href));
        self
    }

    pub fn open_group(&mut self, id: &str, attrs: &str) -> &mut Self {
        let _ = write!(self.body, r#"<g id="{}" {attrs}>"#, esc(id));
        self
    }

    pub fn close_group(&mut self) -> &mut Self {
        self.body.push_str("</g>");
        self
    }

    /// An (almost) invisible rectangle with an id, used as a hit area for mouse
    /// `targets`. Fully invisible shapes are dropped by the SVG parser, so it gets a tiny fill.
    pub fn hit_area(&mut self, id: &str, x: f64, y: f64, w: f64, h: f64) -> &mut Self {
        let attrs = format!(r##"id="{}" fill="#000" fill-opacity="0.01""##, esc(id));
        self.rect(x, y, w, h, &attrs)
    }

    pub fn finish(&self) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">{}</svg>"#,
            self.body,
            w = num(self.width),
            h = num(self.height)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_escaped_svg() {
        let mut s = Svg::new(10.0, 5.5);
        s.text(1.0, 2.25, "a<b & \"c\"", "");
        assert_eq!(
            s.finish(),
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 5.5" width="10" height="5.5"><text x="1" y="2.25" >a&lt;b &amp; &quot;c&quot;</text></svg>"#
        );
    }

    #[test]
    fn checks_colours() {
        assert!(check_color("#89b4fa").is_ok() && check_color("#0006").is_ok() && check_color("white").is_ok());
        assert!(check_color("#12").is_err() && check_color("#gggggg").is_err() && check_color("").is_err());
    }
}
