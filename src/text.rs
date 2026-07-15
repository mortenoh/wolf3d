//! The end-of-episode / help text-page renderer (WOLFSRC/WL_TEXT.C
//! `ShowArticle` / `PageLayout` / `HandleCommand`). The end-game articles and
//! the "Read This!" help are stored as text chunks in VGAGRAPH with an embedded
//! caret-command markup; this module splits an article into pages and paints one
//! page onto the framebuffer over the paper backdrop the original drew.
//!
//! ## Caret commands
//!
//! Inspecting the real WL6 `T_ENDART*` / `T_HELPART` chunk bytes, the articles
//! use this subset (case-insensitive after `^`, per `toupper` in the original):
//!
//! - `^P` — page break. The rest of the line is skipped (help pages tag them
//!   `^PAGE 3`; the trailing `AGE 3` is a comment the original also skips).
//! - `^E` — end of the article.
//! - `^Cnn` — set the font color to the two-hex-digit palette byte `nn`.
//! - `^Gy,x,pic` — draw picture chunk `pic` at pixel `(x & ~7, y)` and reserve a
//!   text margin beside it, exactly like `VWB_DrawPic` + the margin loop.
//!
//! Text otherwise flows word-by-word with the small proportional font, wrapping
//! at the right margin and honoring literal newlines and tabs.

use crate::assets::VgaGraph;
use crate::assets::vgagraph::Picture;
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::font::Font;

/// WL_TEXT.C layout constants.
const BACKCOLOR: u8 = 0x11;
const LEFTMARGIN: i32 = 16;
const RIGHTMARGIN: i32 = 16;
const PICMARGIN: i32 = 8;
const TOPMARGIN: i32 = 16;
const SPACEWIDTH: i32 = 7;
const SCREENMID: i32 = WIDTH as i32 / 2;

/// The paper-window frame pics (GFXV_WL6.H): drawn by `PageLayout`.
const H_TOPWINDOWPIC: usize = 6;
const H_LEFTWINDOWPIC: usize = 7;
const H_RIGHTWINDOWPIC: usize = 8;
const H_BOTTOMINFOPIC: usize = 9;

/// The parsed pages of one article plus the font, ready to render any page.
pub struct TextScreen {
    font: Font,
    top: Picture,
    left: Picture,
    right: Picture,
    bottom: Picture,
    /// Each page's raw markup text (between successive `^P` markers, up to `^E`).
    pages: Vec<String>,
}

impl TextScreen {
    /// Decode the article chunk `chunk` and split it into pages. The frame pics
    /// are cached here so a redraw needs only the framebuffer.
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

    /// Draw page `page` (clamped) over the paper backdrop. `vga` is needed to
    /// decode any `^G` embedded pictures on demand.
    pub fn render(&self, fb: &mut Framebuffer, vga: &VgaGraph, page: usize) {
        // PageLayout: paper fill + window frame.
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

        let Some(text) = self.pages.get(page.min(self.pages.len().saturating_sub(1))) else {
            return;
        };
        self.layout(fb, vga, text);
    }

    /// Walk one page's markup, drawing words and honoring the caret commands.
    fn layout(&self, fb: &mut Framebuffer, vga: &VgaGraph, text: &str) {
        let fh = self.font.height() as i32;
        let mut color = 0u8;
        let mut px = LEFTMARGIN;
        let mut py = TOPMARGIN;
        // Per-row left/right margins, widened beside a `^G` picture.
        let mut left_margin = LEFTMARGIN;
        let mut right_margin = WIDTH as i32 - RIGHTMARGIN;
        let mut margin_bottom = 0; // rows above this y keep the widened margins

        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == b'^' {
                // A caret command.
                let cmd = bytes.get(i + 1).copied().unwrap_or(0).to_ascii_uppercase();
                match cmd {
                    b'E' | b'P' => break, // end of page / article
                    b'C' => {
                        // Two hex digits -> color byte.
                        let hi = hexval(bytes.get(i + 2).copied().unwrap_or(b'0'));
                        let lo = hexval(bytes.get(i + 3).copied().unwrap_or(b'0'));
                        color = (hi << 4) | lo;
                        i += 4;
                        continue;
                    }
                    b'G' => {
                        // ^Gy,x,pic
                        let (nums, adv) = parse_numbers(&bytes[i + 2..]);
                        if nums.len() == 3 {
                            let (gy, gx, pic) = (nums[0], nums[1] & !7, nums[2] as usize);
                            let picture = vga.pic(pic);
                            blit(fb, &picture, gx, gy);
                            // Reserve a text margin beside the picture for the
                            // rows it spans (VWB margin loop): left if the pic
                            // sits left of center, right otherwise.
                            let picmid = gx + picture.width as i32 / 2;
                            let margin = picture.width as i32 + PICMARGIN;
                            margin_bottom = gy + picture.height as i32;
                            if picmid > SCREENMID {
                                right_margin = WIDTH as i32 - margin;
                            } else {
                                left_margin = LEFTMARGIN + margin;
                                if px < left_margin {
                                    px = left_margin;
                                }
                            }
                        }
                        i += 2 + adv;
                        continue;
                    }
                    _ => {
                        // Unknown command: skip the caret and its letter.
                        i += 2;
                        continue;
                    }
                }
            }

            // Drop the widened margins once we've flowed past the picture.
            if py >= margin_bottom {
                left_margin = LEFTMARGIN;
                right_margin = WIDTH as i32 - RIGHTMARGIN;
            }

            match c {
                b'\n' => {
                    px = left_margin;
                    py += fh;
                    i += 1;
                }
                b'\r' => i += 1,
                b' ' => {
                    px += SPACEWIDTH;
                    i += 1;
                }
                b'\t' => {
                    // Advance to the next 64-pixel tab column (the help TOC uses
                    // tabs to line its dotted leaders into columns).
                    px = ((px - left_margin) / 64 + 1) * 64 + left_margin;
                    i += 1;
                }
                _ => {
                    // Gather the whole word, measure it, wrap if needed, draw it.
                    let start = i;
                    while i < bytes.len()
                        && !matches!(bytes[i], b' ' | b'\n' | b'\r' | b'\t' | b'^')
                    {
                        i += 1;
                    }
                    let word = &text[start..i];
                    let w = self.font.text_width(word) as i32;
                    if px + w > right_margin {
                        px = left_margin;
                        py += fh;
                    }
                    self.font.draw(fb, px, py, word, color);
                    px += w;
                }
            }
        }
    }
}

/// Split an article into pages. Every page is the text following a `^P` marker
/// (the rest of the `^P` line is dropped) up to the next `^P` or the closing
/// `^E`. Text before the first `^P` is ignored (there is none in the articles).
fn split_pages(text: &str) -> Vec<String> {
    let mut pages = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find the next ^P.
        if bytes[i] == b'^' && bytes.get(i + 1).map(|b| b.to_ascii_uppercase()) == Some(b'P') {
            // Skip to end of the ^P line.
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            if i < bytes.len() {
                i += 1; // consume the newline
            }
            // Collect until the next ^P or ^E.
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

/// Parse a leading `y,x,pic` number list, returning the values and how many
/// bytes were consumed (up to and including the trailing non-digit run).
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
