//! The link-preview image of a round (R12.2, D7): 1200x630, the day's picks
//! as columns in the site's colours - winner green, numbers taken by more
//! than one player in blue, the rest grey - under the name and the date, in a
//! pixel font drawn here so the image needs no font files. Nothing personal
//! is in it. Rendered on first request and kept.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::stats::Tally;

const W: usize = 1200;
const H: usize = 630;
const BG: [u8; 3] = [0x0c, 0x0c, 0x0c];
const FG: [u8; 3] = [0xf2, 0xf2, 0xf2];
const DIM: [u8; 3] = [0x8f, 0x8f, 0x8f];
const GREEN: [u8; 3] = [0x16, 0xc6, 0x0c];
const BLUE: [u8; 3] = [0x3b, 0x78, 0xff];
const GREY: [u8; 3] = [0x5a, 0x5a, 0x5a];
const RULE: [u8; 3] = [0x2a, 0x2a, 0x2a];

/// Rendered images by (pool, date). A closed round never changes, so an
/// image made once is good for ever; the map is small (one per round).
#[derive(Default)]
pub struct Cache {
    images: Mutex<HashMap<String, std::sync::Arc<Vec<u8>>>>,
}

impl Cache {
    pub fn get_or_render(&self, key: &str, render: impl FnOnce() -> Vec<u8>) -> std::sync::Arc<Vec<u8>> {
        if let Some(png) = self.images.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
            return png.clone();
        }
        let png = std::sync::Arc::new(render());
        self.images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string(), png.clone());
        png
    }
}

pub(crate) struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    pub(crate) fn new(w: usize, h: usize) -> Self {
        let mut px = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            px.extend_from_slice(&BG);
        }
        Self { w, h, px }
    }

    pub(crate) fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u8; 3]) {
        for yy in y.min(self.h)..(y + h).min(self.h) {
            for xx in x.min(self.w)..(x + w).min(self.w) {
                let i = (yy * self.w + xx) * 3;
                self.px[i..i + 3].copy_from_slice(&c);
            }
        }
    }

    /// Colour `c` over the pixel at (x, y), `alpha` of 255 of it: for
    /// anti-aliased text and shapes.
    pub(crate) fn blend(&mut self, x: usize, y: usize, c: [u8; 3], alpha: u8) {
        if x >= self.w || y >= self.h || alpha == 0 {
            return;
        }
        let i = (y * self.w + x) * 3;
        let a = u32::from(alpha);
        for (px, ink) in self.px[i..i + 3].iter_mut().zip(c) {
            *px = ((u32::from(ink) * a + u32::from(*px) * (255 - a) + 127) / 255) as u8;
        }
    }

    /// Text in the 5x7 font, each font pixel `scale` pixels square. Returns
    /// the width drawn.
    fn text(&mut self, x: usize, y: usize, scale: usize, s: &str, c: [u8; 3]) -> usize {
        let mut cx = x;
        for ch in s.chars() {
            if let Some(rows) = glyph(ch) {
                for (r, bits) in rows.iter().enumerate() {
                    for col in 0..5 {
                        if bits & (0b10000 >> col) != 0 {
                            self.rect(cx + col * scale, y + r * scale, scale, scale, c);
                        }
                    }
                }
            }
            cx += 6 * scale;
        }
        cx - x
    }

    pub(crate) fn png(self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w as u32, self.h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("PNG header into a Vec");
            writer.write_image_data(&self.px).expect("PNG data into a Vec");
        }
        out
    }
}

fn text_width(s: &str, scale: usize) -> usize {
    s.chars().count() * 6 * scale
}

/// The round's preview: `tallies` lowest value first, as the day page has.
pub fn round_image(date: &str, tallies: &[Tally], winner: Option<i64>, players: i64) -> Vec<u8> {
    let mut c = Canvas::new(W, H);
    let pad = 72;

    // "i guess low", with "low" on a green block as in the header.
    let s = 6;
    let w = c.text(pad, pad, s, "i guess ", FG);
    let low_w = text_width("low", s);
    c.rect(pad + w - s, pad - s, low_w + s, 7 * s + 2 * s, GREEN);
    c.text(pad + w, pad, s, "low", BG);
    let shown = crate::templates::eu(date);
    let date_w = text_width(&shown, 5);
    c.text(W - pad - date_w, pad + 4, 5, &shown, DIM);

    // The columns: every number up to a little past the winner, or the
    // first thirty picked, whichever is more telling.
    let span = winner
        .map(|w| w + 3)
        .unwrap_or(0)
        .max(tallies.iter().take(30).map(|t| t.value).next_back().unwrap_or(1))
        .clamp(5, 40);
    let max = tallies.iter().filter(|t| t.value <= span).map(|t| t.count).max().unwrap_or(1).max(1);
    let (top, bottom) = (200, H - 150);
    let area = W - 2 * pad;
    let slot = area / span as usize;
    let bar = (slot * 7 / 10).max(2);
    c.rect(pad, bottom + 2, area, 2, RULE);
    for t in tallies.iter().filter(|t| t.value >= 1 && t.value <= span) {
        let h = ((bottom - top) as i64 * t.count / max).max(4) as usize;
        let colour = if Some(t.value) == winner {
            GREEN
        } else if t.count > 1 {
            BLUE
        } else {
            GREY
        };
        let x = pad + (t.value as usize - 1) * slot + (slot - bar) / 2;
        c.rect(x, bottom - h, bar, h, colour);
    }

    // The outcome, under the axis.
    let line = match winner {
        Some(w) => format!("{w} won  {players} players"),
        None => format!("nobody won  {players} players"),
    };
    c.text(pad, H - 110, 6, &line, FG);
    c.png()
}

/// A 5x7 glyph, top row first, the high bit leftmost. Only what the
/// preview needs: lowercase letters, digits, '-' and space.
fn glyph(ch: char) -> Option<[u8; 7]> {
    Some(match ch {
        ' ' => [0; 7],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        'a' => [0, 0, 0b01110, 0b00001, 0b01111, 0b10001, 0b01111],
        'b' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b11110],
        'd' => [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10001, 0b01111],
        'e' => [0, 0, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'g' => [0, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'i' => [0b00100, 0, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'n' => [0, 0, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0, 0, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0, 0, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000],
        'r' => [0, 0, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0, 0, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        'u' => [0, 0, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'w' => [0, 0, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        'y' => [0, 0, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(pairs: &[(i64, i64)]) -> Vec<Tally> {
        pairs.iter().map(|&(value, count)| Tally { value, count }).collect()
    }

    #[test]
    fn the_preview_is_a_1200_by_630_png() {
        let png = round_image("2026-10-21", &t(&[(1, 3), (2, 2), (3, 1), (9, 1)]), Some(3), 7);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(png.as_slice());
        let reader = decoder.read_info().unwrap();
        let info = reader.info();
        assert_eq!((info.width, info.height), (1200, 630));
    }

    #[test]
    fn every_character_the_preview_writes_has_a_glyph() {
        for s in ["i guess low", "21.10.2026", "1234567890 won  41 players", "nobody won  0 players"] {
            for ch in s.chars() {
                assert!(glyph(ch).is_some(), "{ch:?} in {s:?}");
            }
        }
    }

    #[test]
    fn an_image_is_rendered_once_and_kept() {
        let cache = Cache::default();
        let mut renders = 0;
        let a = cache.get_or_render("global:2026-10-21", || { renders += 1; vec![1] });
        let b = cache.get_or_render("global:2026-10-21", || { renders += 1; vec![2] });
        assert_eq!((a.as_slice(), b.as_slice(), renders), (&[1u8][..], &[1u8][..], 1));
    }
}
