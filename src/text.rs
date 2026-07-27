//! The end-of-episode / help text-page renderer (WOLFSRC/WL_TEXT.C
//! `ShowArticle` / `PageLayout` / `HandleCommand`). The end-game articles and
//! the "Read This!" help are stored as text chunks in VGAGRAPH with an embedded
//! caret-command markup; this module splits an article into pages and paints one
//! page onto the framebuffer over the paper backdrop the original drew.
//!
//! ## Caret commands
//!
//! - `^P` — page break (rest of line skipped).
//! - `^E` — end of the article.
//! - `^Cnn` — set the font color to the two-hex-digit palette byte `nn`.
//! - `^Gy,x,pic` — draw picture chunk `pic` at pixel `(x & ~7, y)` and reserve
//!   per-row text margins beside it.
//!
//! Layout stops at TEXTROWS so body text never paints over the bottom info bar.
//! Tabs advance to the next 8-pixel boundary (`(px + 8) & !7`), matching WL_TEXT.C.

use crate::assets::VgaGraph;
use crate::assets::vgagraph::Picture;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::font::Font;

const BACKCOLOR: u8 = 0x11;
const LEFTMARGIN: i32 = 16;
const RIGHTMARGIN: i32 = 16;
const PICMARGIN: i32 = 8;
const TOPMARGIN: i32 = 16;
const BOTTOMMARGIN: i32 = 32;
const SPACEWIDTH: i32 = 7;
const SCREENMID: i32 = WIDTH as i32 / 2;
/// `(200 - TOPMARGIN - BOTTOMMARGIN) / FONTHEIGHT` with the height-10 font.
const TEXTROWS: usize = ((200 - TOPMARGIN - BOTTOMMARGIN) / 10) as usize;

const H_TOPWINDOWPIC: usize = 6;
const H_LEFTWINDOWPIC: usize = 7;
const H_RIGHTWINDOWPIC: usize = 8;
const H_BOTTOMINFOPIC: usize = 9;

pub struct TextScreen {
    font: Font,
    top: Picture,
    left: Picture,
    right: Picture,
    bottom: Picture,
    pages: Vec<String>,
}

impl TextScreen {
    pub fn new(vga: &VgaGraph, chunk: usize) -> Self {
        let raw = vga.raw_chunk(chunk);
        let text = String::from_utf8_lossy(&raw).into_owned();
        Self {
            font: Font::load(vga, 0),
            top: vga.pic(H_TOPWINDOWPIC),
            left: vga.pic(H_LEFTWINDOWPIC),
            right: vga.pic(H_RIGHTWINDOWPIC),
            bottom: vga.pic(H_BOTTOMINFOPIC),
            pages: split_pages(&text),
        }
    }

    pub fn num_pages(&self) -> usize {
        self.pages.len()
    }

    pub fn render(&self, fb: &mut Framebuffer, vga: &VgaGraph, page: usize) {
        fill(fb, BACKCOLOR);
        blit(fb, &self.top, 0, 0);
        blit(fb, &self.left, 0, 8);
        blit(fb, &self.right, WIDTH as i32 - self.right.width as i32, 8);
        blit(
            fb,
            &self.bottom,
            8,
            HEIGHT as i32 - self.bottom.height as i32,
        );

        let page = page.min(self.pages.len().saturating_sub(1));
        if let Some(text) = self.pages.get(page) {
            self.layout(fb, vga, text);
        }

        let label = format!("pg {} of {}", page + 1, self.pages.len().max(1));
        self.font.draw(fb, 213, 183, &label, 0x4f);
    }

    fn layout(&self, fb: &mut Framebuffer, vga: &VgaGraph, text: &str) {
        let fh = self.font.height() as i32;
        let mut color = 0u8;
        let mut px = LEFTMARGIN;
        let mut rowon: usize = 0;
        let mut py = TOPMARGIN;
        let mut left_margin = [LEFTMARGIN; TEXTROWS];
        let mut right_margin = [WIDTH as i32 - RIGHTMARGIN; TEXTROWS];

        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'^' {
                let cmd = bytes.get(i + 1).copied().unwrap_or(0).to_ascii_uppercase();
                match cmd {
                    b'E' | b'P' => break,
                    b'C' => {
                        let hi = hexval(bytes.get(i + 2).copied().unwrap_or(b'0'));
                        let lo = hexval(bytes.get(i + 3).copied().unwrap_or(b'0'));
                        color = (hi << 4) | lo;
                        i += 4;
                        continue;
                    }
                    b'G' => {
                        let (nums, adv) = parse_numbers(&bytes[i + 2..]);
                        if nums.len() == 3 {
                            let (gy, gx, pic) = (nums[0], nums[1] & !7, nums[2] as usize);
                            let picture = vga.pic(pic);
                            blit(fb, &picture, gx, gy);
                            let top_row = ((gy - TOPMARGIN) / fh).max(0) as usize;
                            let bottom_row =
                                ((gy + picture.height as i32 - TOPMARGIN) / fh).max(0) as usize;
                            let margin = picture.width as i32 + PICMARGIN;
                            let picmid = gx + picture.width as i32 / 2;
                            for r in top_row..=bottom_row.min(TEXTROWS.saturating_sub(1)) {
                                if picmid > SCREENMID {
                                    right_margin[r] = WIDTH as i32 - margin;
                                } else {
                                    left_margin[r] = LEFTMARGIN + margin;
                                }
                            }
                            if px < left_margin[rowon.min(TEXTROWS - 1)] {
                                px = left_margin[rowon.min(TEXTROWS - 1)];
                            }
                        }
                        i += 2 + adv;
                        continue;
                    }
                    _ => {
                        i += 2;
                        continue;
                    }
                }
            }

            match c {
                b'\n' => {
                    if !newline(&mut rowon, &mut px, &mut py, fh, &left_margin) {
                        return;
                    }
                    i += 1;
                }
                b'\r' => i += 1,
                b' ' => {
                    px += SPACEWIDTH;
                    i += 1;
                }
                b'\t' => {
                    // WL_TEXT.C: next 8-pixel column, not 64.
                    px = (px + 8) & !7;
                    i += 1;
                }
                _ if c <= 32 => i += 1,
                _ => {
                    let start = i;
                    while i < bytes.len() && bytes[i] > 32 && bytes[i] != b'^' {
                        i += 1;
                    }
                    let word = &text[start..i];
                    let w = self.font.text_width(word) as i32;
                    while px + w > right_margin[rowon.min(TEXTROWS - 1)] {
                        if !newline(&mut rowon, &mut px, &mut py, fh, &left_margin) {
                            return;
                        }
                    }
                    self.font.draw(fb, px, py, word, color);
                    px += w;
                    while i < bytes.len() && bytes[i] == b' ' {
                        px += SPACEWIDTH;
                        i += 1;
                    }
                }
            }
        }
    }
}

fn newline(
    rowon: &mut usize,
    px: &mut i32,
    py: &mut i32,
    fh: i32,
    left_margin: &[i32; TEXTROWS],
) -> bool {
    *rowon += 1;
    if *rowon >= TEXTROWS {
        return false;
    }
    *px = left_margin[*rowon];
    *py += fh;
    true
}

fn split_pages(text: &str) -> Vec<String> {
    let mut pages = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'^' && bytes.get(i + 1).map(|b| b.to_ascii_uppercase()) == Some(b'P') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1;
            }
            let start = i;
            while i < bytes.len() {
                if bytes[i] == b'^' {
                    let nx = bytes.get(i + 1).map(|b| b.to_ascii_uppercase());
                    if nx == Some(b'P') || nx == Some(b'E') {
                        break;
                    }
                }
                i += 1;
            }
            pages.push(text[start..i].to_string());
            if i < bytes.len()
                && bytes[i] == b'^'
                && bytes.get(i + 1).map(|b| b.to_ascii_uppercase()) == Some(b'E')
            {
                break;
            }
        } else {
            i += 1;
        }
    }
    if pages.is_empty() {
        pages.push(text.to_string());
    }
    pages
}

fn parse_numbers(bytes: &[u8]) -> (Vec<i32>, usize) {
    let mut nums = Vec::new();
    let mut i = 0;
    loop {
        while i < bytes.len() && bytes[i] == b',' {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            break;
        }
        nums.push(
            std::str::from_utf8(&bytes[start..i])
                .unwrap()
                .parse()
                .unwrap_or(0),
        );
        if i >= bytes.len() || bytes[i] != b',' {
            break;
        }
    }
    (nums, i)
}

fn hexval(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

fn fill(fb: &mut Framebuffer, color: u8) {
    fb.pixels
        .fill(crate::assets::palette::PALETTE[color as usize]);
}

fn blit(fb: &mut Framebuffer, pic: &Picture, dx: i32, dy: i32) {
    for row in 0..pic.height as i32 {
        let y = dy + row;
        if y < 0 || y as usize >= HEIGHT {
            continue;
        }
        for col in 0..pic.width as i32 {
            let x = dx + col;
            if x < 0 || x as usize >= WIDTH {
                continue;
            }
            fb.pixels[y as usize * WIDTH + x as usize] =
                pic.pixels[row as usize * pic.width + col as usize];
        }
    }
}
