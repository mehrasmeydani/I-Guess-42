//! The link-preview image of a round (R12.2, D7): 1200x630, the day's picks
//! as columns in the site's colours - winner green, numbers taken by more
//! than one player in blue, the rest grey - under the name and the date, in a
//! pixel font drawn here so the image needs no font files. Nothing personal
//! is in it. Rendered on first request and kept.
//!
//! Also the 42 round's leaderboard as an image (`board_image`), to put up on
//! a screen or send round: rebuilt once a day, with a QR code to the site.

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

/// One image that changes once a day: kept under the day it was made for,
/// and made again when the day moves on.
#[derive(Default)]
pub struct Daily {
    last: Mutex<Option<(String, std::sync::Arc<Vec<u8>>)>>,
}

impl Daily {
    pub fn get(&self, day: &str) -> Option<std::sync::Arc<Vec<u8>>> {
        match &*self.last.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((d, png)) if d == day => Some(png.clone()),
            _ => None,
        }
    }

    pub fn put(&self, day: &str, png: Vec<u8>) -> std::sync::Arc<Vec<u8>> {
        let png = std::sync::Arc::new(png);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((day.to_string(), png.clone()));
        png
    }
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        let mut px = Vec::with_capacity(w * h * 3);
        for _ in 0..w * h {
            px.extend_from_slice(&BG);
        }
        Self { w, h, px }
    }

    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: [u8; 3]) {
        for yy in y.min(self.h)..(y + h).min(self.h) {
            for xx in x.min(self.w)..(x + w).min(self.w) {
                let i = (yy * self.w + xx) * 3;
                self.px[i..i + 3].copy_from_slice(&c);
            }
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

    fn png(self) -> Vec<u8> {
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
    masthead(&mut c, pad, 6);
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

/// "i guess low", with "low" on a green block as in the header; at `pad`
/// from the top left, in 5x7 pixels of `s` each.
fn masthead(c: &mut Canvas, pad: usize, s: usize) {
    let w = c.text(pad, pad, s, "i guess ", FG);
    let low_w = text_width("low", s);
    c.rect(pad + w - s, pad - s, low_w + s, 7 * s + 2 * s, GREEN);
    c.text(pad + w, pad, s, "low", BG);
}

/// `s` cut to `max` characters, the last one a '.' when it was longer.
fn fit(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max.saturating_sub(1)).chain(std::iter::once('.')).collect()
}

/// One leaderboard row on the image: rank, 42 login, score.
pub struct BoardLine {
    pub rank: i64,
    pub login: String,
    pub score: i64,
}

const BOARD_W: usize = 1200;
const BOARD_H: usize = 1350;

/// The 42 round's leaderboard: the most wins and the longest streaks, ten
/// each, side by side, as of the round closed on `date`; below them how to
/// play and a QR code to `url`. Portrait, so it fills a phone or a screen
/// on its side. Only 42 logins and counts are in it, as on the page.
pub fn board_image(date: &str, wins: &[BoardLine], streaks: &[BoardLine], url: &str) -> Vec<u8> {
    let mut c = Canvas::new(BOARD_W, BOARD_H);
    let pad = 72;

    masthead(&mut c, pad, 6);
    let shown = crate::templates::eu(date);
    c.text(BOARD_W - pad - text_width(&shown, 5), pad + 4, 5, &shown, DIM);
    c.text(pad, 168, 4, "leaderboard - the 42 round", DIM);
    c.rect(pad, 228, BOARD_W - 2 * pad, 2, RULE);

    let gap = 64;
    let col = (BOARD_W - 2 * pad - gap) / 2;
    board_column(&mut c, pad, col, "most wins", "rounds won", wins, GREEN);
    board_column(&mut c, pad + col + gap, col, "longest streaks", "days in a row", streaks, BLUE);

    // How to play, and the code to scan, under a rule.
    let foot = 1050;
    c.rect(pad, foot, BOARD_W - 2 * pad, 2, RULE);
    let qr_box = 240;
    let qr_x = BOARD_W - pad - qr_box;
    let qr_y = foot + 32;
    qr(&mut c, qr_x, qr_y, qr_box, url);
    let host = url.split("://").nth(1).unwrap_or(url).trim_end_matches('/').to_lowercase();
    let room = (qr_x - pad - 32) / 30;
    c.text(pad, foot + 44, 4, "lowest unique number wins.", FG);
    c.text(pad, foot + 90, 4, "one guess a day.", DIM);
    c.text(pad, foot + 170, 5, "scan to play", GREEN);
    c.text(pad, foot + 220, 5, &fit(&host, room), FG);
    c.png()
}

/// A column of the board: title, what is counted, then up to ten rows, each
/// a rank, a login, the score and a bar of it against the top one.
fn board_column(c: &mut Canvas, x: usize, w: usize, title: &str, what: &str, rows: &[BoardLine], colour: [u8; 3]) {
    c.text(x, 268, 5, title, FG);
    c.text(x, 320, 3, what, DIM);
    let top = 372;
    if rows.is_empty() {
        c.text(x, top, 4, "nobody yet", DIM);
        return;
    }
    let s = 4;
    let rank_w = text_width("00", s) + 24;
    let best = rows.iter().map(|r| r.score).max().unwrap_or(1).max(1);
    for (i, r) in rows.iter().take(10).enumerate() {
        let y = top + i * 66;
        c.text(x, y, s, &format!("{:02}", r.rank), if r.rank == 1 { colour } else { DIM });
        let score = r.score.to_string();
        let score_w = text_width(&score, s);
        let room = (w - rank_w - score_w - 24) / (6 * s);
        c.text(x + rank_w, y, s, &fit(&r.login.to_lowercase(), room), FG);
        c.text(x + w - score_w, y, s, &score, FG);
        let bar_w = (w - rank_w) * r.score.max(0) as usize / best as usize;
        c.rect(x + rank_w, y + 36, w - rank_w, 4, RULE);
        c.rect(x + rank_w, y + 36, bar_w.max(4), 4, colour);
    }
}

/// A QR code of `text`, dark on light with its quiet zone, as large as fits
/// in `size` pixels square at whole pixels a module, centred there.
fn qr(c: &mut Canvas, x: usize, y: usize, size: usize, text: &str) {
    let Ok(code) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else {
        return;
    };
    let n = code.size() as usize;
    let quiet = 3;
    let m = (size / (n + 2 * quiet)).max(1);
    let drawn = m * (n + 2 * quiet);
    let (x, y) = (x + (size - drawn.min(size)) / 2, y + (size - drawn.min(size)) / 2);
    c.rect(x, y, drawn, drawn, FG);
    for row in 0..n {
        for col in 0..n {
            if code.get_module(col as i32, row as i32) {
                c.rect(x + (quiet + col) * m, y + (quiet + row) * m, m, m, BG);
            }
        }
    }
}

/// A 5x7 glyph, top row first, the high bit leftmost. Lowercase letters,
/// digits, and the few marks the images write.
fn glyph(ch: char) -> Option<[u8; 7]> {
    Some(match ch {
        ' ' => [0; 7],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        ':' => [0, 0b01100, 0b01100, 0, 0b01100, 0b01100, 0],
        '/' => [0, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0],
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
        'c' => [0, 0, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110],
        'd' => [0b00001, 0b00001, 0b01101, 0b10011, 0b10001, 0b10001, 0b01111],
        'e' => [0, 0, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'f' => [0b00110, 0b01001, 0b01000, 0b11100, 0b01000, 0b01000, 0b01000],
        'g' => [0, 0b01111, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'h' => [0b10000, 0b10000, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'i' => [0b00100, 0, 0b01100, 0b00100, 0b00100, 0b00100, 0b01110],
        'j' => [0b00010, 0, 0b00110, 0b00010, 0b00010, 0b10010, 0b01100],
        'k' => [0b10000, 0b10000, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010],
        'l' => [0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'm' => [0, 0, 0b11010, 0b10101, 0b10101, 0b10001, 0b10001],
        'n' => [0, 0, 0b10110, 0b11001, 0b10001, 0b10001, 0b10001],
        'o' => [0, 0, 0b01110, 0b10001, 0b10001, 0b10001, 0b01110],
        'p' => [0, 0, 0b11110, 0b10001, 0b11110, 0b10000, 0b10000],
        'q' => [0, 0, 0b01101, 0b10011, 0b01111, 0b00001, 0b00001],
        'r' => [0, 0, 0b10110, 0b11001, 0b10000, 0b10000, 0b10000],
        's' => [0, 0, 0b01111, 0b10000, 0b01110, 0b00001, 0b11110],
        't' => [0b01000, 0b01000, 0b11100, 0b01000, 0b01000, 0b01001, 0b00110],
        'u' => [0, 0, 0b10001, 0b10001, 0b10001, 0b10011, 0b01101],
        'v' => [0, 0, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'w' => [0, 0, 0b10001, 0b10001, 0b10101, 0b10101, 0b01010],
        'x' => [0, 0, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'y' => [0, 0, 0b10001, 0b10001, 0b01111, 0b00001, 0b01110],
        'z' => [0, 0, 0b11111, 0b00010, 0b00100, 0b01000, 0b11111],
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
    fn the_board_is_a_png_with_every_letter_drawn() {
        let line = |rank, login: &str, score| BoardLine { rank, login: login.into(), score };
        let wins = [line(1, "megardes", 4), line(2, "abcdefghijklmnopqrstuvwxyz_-", 2)];
        let png = board_image("2026-10-27", &wins, &[], "https://iguesslow.com");
        let reader = png::Decoder::new(png.as_slice()).read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (1200, 1350));
        for ch in "abcdefghijklmnopqrstuvwxyz0123456789_-.:/".chars() {
            assert!(glyph(ch).is_some(), "{ch:?}");
        }
        assert_eq!(fit("abcdef", 4), "abc.");
        assert_eq!(fit("abc", 4), "abc");
    }

    #[test]
    fn a_daily_image_is_made_again_when_the_day_moves_on() {
        let daily = Daily::default();
        assert!(daily.get("2026-10-28").is_none());
        daily.put("2026-10-28", vec![1]);
        assert_eq!(daily.get("2026-10-28").unwrap().as_slice(), &[1]);
        assert!(daily.get("2026-10-29").is_none());
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
