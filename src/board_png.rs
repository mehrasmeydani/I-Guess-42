//! The 42 round's leaderboard as an image, at `/leaderboard.png`: the most
//! wins and the longest streaks, drawn as the leaderboard page draws them -
//! the place, the player, and a bar against the leader with the count beside
//! it, the first three in violet, blue and red, everyone after in green -
//! then a QR code to the site. In the site's monospace (DejaVu Sans Mono,
//! embedded), at twice the page's size so it reads from across a room.
//! 1920x1080, a presentation slide's 16:9, for the screen it is shown on:
//! the two tables side by side, the QR code on the right. Made once a day.

use std::sync::{Arc, LazyLock, Mutex};

use crate::og::Canvas;

const W: usize = 1920;
const H: usize = 1080;
const PAD: usize = 96;
/// Each table's width, the gap between them, and where the QR code's side
/// begins.
const TABLE_W: usize = 610;
const GAP: usize = 64;
const SIDE_LEFT: usize = PAD + 2 * TABLE_W + GAP + 72;

// The page's colours (style.css).
const BG: [u8; 3] = [0x0c, 0x0c, 0x0c];
const FG: [u8; 3] = [0xc4, 0xc4, 0xc4];
const HI: [u8; 3] = [0xff, 0xff, 0xff];
const DIM: [u8; 3] = [0x6e, 0x6e, 0x6e];
const STATE: [u8; 3] = [0x16, 0xc6, 0x0c];
const FIRST: [u8; 3] = [0xc6, 0x78, 0xdd];
const ACTION: [u8; 3] = [0x3b, 0x78, 0xff];
const ACTION_HI: [u8; 3] = [0x5a, 0x8c, 0xff];
const LOSS: [u8; 3] = [0xe7, 0x48, 0x56];
const RULE: [u8; 3] = [0xcc, 0xcc, 0xcc];
const ROW_RULE: [u8; 3] = [0x2a, 0x2a, 0x2a];

/// Rows on the image: the page shows 15, a picture reads better with 10.
pub const ROWS: usize = 10;

static REGULAR: LazyLock<fontdue::Font> = LazyLock::new(|| font(include_bytes!("fonts/DejaVuSansMono.ttf")));
static BOLD: LazyLock<fontdue::Font> = LazyLock::new(|| font(include_bytes!("fonts/DejaVuSansMono-Bold.ttf")));

fn font(bytes: &[u8]) -> fontdue::Font {
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).expect("the embedded font parses")
}

/// One image that changes once a day: kept under the day it was made for,
/// and made again when the day moves on.
#[derive(Default)]
pub struct Daily {
    last: Mutex<Option<(String, Arc<Vec<u8>>)>>,
}

impl Daily {
    pub fn get(&self, day: &str) -> Option<Arc<Vec<u8>>> {
        match &*self.last.lock().unwrap_or_else(|e| e.into_inner()) {
            Some((d, png)) if d == day => Some(png.clone()),
            _ => None,
        }
    }

    pub fn put(&self, day: &str, png: Vec<u8>) -> Arc<Vec<u8>> {
        let png = Arc::new(png);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Some((day.to_string(), png.clone()));
        png
    }
}

/// One row: place (shared on a tie), 42 login, and the score: wins, or the
/// longest streak.
pub struct Line {
    pub rank: i64,
    pub login: String,
    pub score: i64,
}

/// A row of the streaks: also the streak still going, 0 if none.
pub struct StreakLine {
    pub line: Line,
    pub now: i64,
}

/// `s` at `px` pixels with its baseline at `y`, from `x`; the width drawn.
fn text(c: &mut Canvas, font: &fontdue::Font, px: f32, x: usize, y: usize, s: &str, colour: [u8; 3]) -> usize {
    let mut pen = x as f32;
    for ch in s.chars() {
        let (m, coverage) = font.rasterize(ch, px);
        let left = pen.round() as i64 + i64::from(m.xmin);
        let top = y as i64 - m.height as i64 - i64::from(m.ymin);
        for row in 0..m.height {
            for col in 0..m.width {
                let (gx, gy) = (left + col as i64, top + row as i64);
                if gx >= 0 && gy >= 0 {
                    c.blend(gx as usize, gy as usize, colour, coverage[row * m.width + col]);
                }
            }
        }
        pen += m.advance_width;
    }
    (pen - x as f32).round() as usize
}

fn width(font: &fontdue::Font, px: f32, s: &str) -> usize {
    s.chars().map(|ch| font.metrics(ch, px).advance_width).sum::<f32>().round() as usize
}

/// `s` cut to fit `room` pixels, ending in "…" when it was cut.
fn fit(font: &fontdue::Font, px: f32, s: &str, room: usize) -> String {
    if width(font, px, s) <= room {
        return s.to_string();
    }
    let mut out: String = s.to_string();
    while !out.is_empty() && width(font, px, &format!("{out}…")) > room {
        out.pop();
    }
    format!("{out}…")
}

/// A place's colours, as the page has them: (place and count, bar).
fn colours(rank: i64) -> ([u8; 3], [u8; 3]) {
    match rank {
        1 => (FIRST, FIRST),
        2 => (ACTION_HI, ACTION),
        3 => (LOSS, LOSS),
        _ => (STATE, STATE),
    }
}

/// The image: the most wins and the longest streaks as of the round closed
/// on `date`, best first (only the first `ROWS` of each drawn), and a QR
/// code to `url`.
pub fn render(date: &str, wins: &[Line], streaks: &[StreakLine], url: &str) -> Vec<u8> {
    let mut c = Canvas::new(W, H);

    // The header's brand: "i guess low", "low" on a green block; what this
    // is and when, at the other end of the slide.
    let brand_px = 52.0;
    let base = PAD + 50;
    let w = text(&mut c, &BOLD, brand_px, PAD, base, "i guess ", HI);
    let low_w = width(&BOLD, brand_px, "low");
    c.rect(PAD + w - 4, base - 47, low_w + 26, 63, STATE);
    text(&mut c, &BOLD, brand_px, PAD + w + 9, base, "low", BG);
    let about = format!("the 42 round leaderboard · {}", crate::templates::eu(date));
    let about_w = width(&REGULAR, 30.0, &about);
    text(&mut c, &REGULAR, 30.0, W - PAD - about_w, base, &about, DIM);

    let wins_x = PAD;
    let streaks_x = PAD + TABLE_W + GAP;
    table(&mut c, wins_x, "most wins", "wins", None, &wins.iter().collect::<Vec<_>>());
    let now: Vec<i64> = streaks.iter().map(|s| s.now).collect();
    table(&mut c, streaks_x, "longest streaks", "days", Some(&now), &streaks.iter().map(|s| &s.line).collect::<Vec<_>>());

    // The other side: how to play, and where, around a QR code on white,
    // as scanners want it; level with the tables.
    let side_w = W - PAD - SIDE_LEFT;
    let centre = |c: &mut Canvas, font: &fontdue::Font, px: f32, y: usize, s: &str, colour: [u8; 3]| {
        let w = width(font, px, s);
        text(c, font, px, SIDE_LEFT + side_w.saturating_sub(w) / 2, y, s, colour);
    };
    centre(&mut c, &BOLD, 42.0, TITLE_BASE, "scan to play", STATE);
    let qr_size = side_w.min(400);
    let qr_top = TITLE_BASE + 70;
    qr(&mut c, SIDE_LEFT + (side_w - qr_size) / 2, qr_top, qr_size, url);
    let host = url.split("://").nth(1).unwrap_or(url).trim_end_matches('/');
    let below = qr_top + qr_size;
    centre(&mut c, &BOLD, 38.0, below + 76, host, HI);
    centre(&mut c, &REGULAR, 24.0, below + 150, "lowest unique number wins", FG);
    centre(&mut c, &REGULAR, 24.0, below + 190, "one guess a day", DIM);
    c.png()
}

const TITLE_BASE: usize = 262;

/// One table at `x`, `TABLE_W` wide: its title with the rule running to its
/// edge as on the page, the column heads, and up to `ROWS` rows of place,
/// player, (for streaks) the one still going, and the score as a bar with
/// the count.
fn table(c: &mut Canvas, x: usize, title: &str, score_head: &str, now: Option<&[i64]>, rows: &[&Line]) {
    let right = x + TABLE_W;
    let tw = text(c, &BOLD, 42.0, x, TITLE_BASE, title, HI);
    c.rect(x + tw + 28, TITLE_BASE - 15, right.saturating_sub(x + tw + 28), 4, RULE);

    let px = 32.0;
    let place_w = width(&BOLD, px, "00") + 32;
    let score_x = x + TABLE_W * 60 / 100;
    let score_w = right - score_x;
    let now_w = if now.is_some() { width(&REGULAR, px, "now") + 24 } else { 0 };
    let now_x = score_x - now_w;
    let head_base = 352;
    text(c, &REGULAR, 26.0, x, head_base, "#", DIM);
    text(c, &REGULAR, 26.0, x + place_w, head_base, "player", DIM);
    if now.is_some() {
        text(c, &REGULAR, 26.0, now_x, head_base, "now", DIM);
    }
    text(c, &REGULAR, 26.0, score_x, head_base, score_head, DIM);
    c.rect(x, head_base + 18, TABLE_W, 4, RULE);

    let top = head_base + 22;
    let row_h = 62;
    if rows.is_empty() {
        text(c, &REGULAR, px, x, top + 44, "nobody yet", DIM);
    }
    let best = rows.iter().map(|r| r.score).max().unwrap_or(1).max(1);
    for (i, r) in rows.iter().take(ROWS).enumerate() {
        let y = top + i * row_h;
        let base = y + 43;
        let (ink, bar) = colours(r.rank);
        text(c, &BOLD, px, x, base, &format!("{:02}", r.rank), ink);
        let (face, who) = if r.rank == 1 { (&*BOLD, HI) } else { (&*REGULAR, FG) };
        let name = fit(face, px, &r.login, now_x - x - place_w - 28);
        text(c, face, px, x + place_w, base, &name, who);
        // The streak still going, as the page's "now" column: a dash if none.
        if let Some(now) = now {
            match now.get(i).copied().unwrap_or(0) {
                0 => text(c, &REGULAR, px, now_x, base, "–", DIM),
                n => text(c, &REGULAR, px, now_x, base, &n.to_string(), FG),
            };
        }
        // The bar is 70% of the cell at most, as on the page, leaving room
        // for the count.
        let bar_w = ((score_w - width(&BOLD, px, "000") - 16) * 7 / 10 * r.score.max(0) as usize / best as usize).max(4);
        c.rect(score_x, base - 21, bar_w, 18, bar);
        text(c, &BOLD, px, score_x + bar_w + 14, base, &r.score.to_string(), ink);
        c.rect(x, y + row_h - 2, TABLE_W, 2, ROW_RULE);
    }
}

/// A QR code of `text`, dark modules on a white square with its quiet zone,
/// as large as fits `size` pixels at whole pixels a module.
fn qr(c: &mut Canvas, x: usize, y: usize, size: usize, text: &str) {
    let Ok(code) = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) else {
        return;
    };
    let n = code.size() as usize;
    let quiet = 3;
    let m = (size / (n + 2 * quiet)).max(1);
    let drawn = m * (n + 2 * quiet);
    let (x, y) = (x + size.saturating_sub(drawn) / 2, y + size.saturating_sub(drawn) / 2);
    c.rect(x, y, drawn, drawn, HI);
    for row in 0..n {
        for col in 0..n {
            if code.get_module(col as i32, row as i32) {
                c.rect(x + (quiet + col) * m, y + (quiet + row) * m, m, m, BG);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_is_a_png_of_its_size() {
        let line = |rank, login: &str, score| Line { rank, login: login.into(), score };
        let streaks = [StreakLine { line: line(1, "megardes", 29), now: 29 }, StreakLine { line: line(2, "jdoe", 12), now: 0 }];
        let png = render("2026-10-27", &[line(1, "megardes", 4), line(2, "a-very-long-login-indeed-yes", 2)], &streaks, "https://iguesslow.com/");
        let reader = png::Decoder::new(png.as_slice()).read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (W as u32, H as u32));
        assert_eq!(fit(&REGULAR, 34.0, "abc", 1000), "abc");
        assert!(fit(&REGULAR, 34.0, "abcdefghijklmnop", 120).ends_with('…'));
    }

    #[test]
    fn a_daily_image_is_made_again_when_the_day_moves_on() {
        let daily = Daily::default();
        assert!(daily.get("2026-10-28").is_none());
        daily.put("2026-10-28", vec![1]);
        assert_eq!(daily.get("2026-10-28").unwrap().as_slice(), &[1]);
        assert!(daily.get("2026-10-29").is_none());
    }
}


