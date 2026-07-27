//! Per-level statistics, par times, the between-floors intermission
//! (WOLFSRC/WL_INTER.C `LevelCompleted`) and the "Get Psyched!" load screen.
//!
//! The scoring math is factored into pure functions ([`LevelStats::kill_ratio`]
//! and friends, [`compute_bonus`]) so it can be checked against the original's
//! formulas without any rendering. The [`Intermission`] then counts those values
//! up on screen with the tic sound, exactly as `LevelCompleted` does; the score
//! bonus itself is awarded up front by the caller, so skipping the animation
//! (any key) never changes the total.

use crate::assets::VgaGraph;
use crate::assets::palette::PALETTE;
use crate::assets::vgagraph::{self, Picture};
use crate::fb::{Framebuffer, HEIGHT, WIDTH};
use crate::font::Font;
use crate::highscore::HighScore;
use crate::savegame::{Reader, SaveError, Writer};
use crate::sound as snd;

/// WL_INTER.C `PAR_AMOUNT`: score per second under par.
pub const PAR_AMOUNT: i32 = 500;
/// WL_INTER.C `PERCENT100AMT`: score for a perfect (100%) ratio in a category.
pub const PERCENT100AMT: i32 = 10000;
/// WL_INTER.C `VWB_Bar(0,0,320,200-STATUSLINES,127)`: the intermission backdrop.
const VIEWCOLOR: u8 = 127;

/// WL_GAME.C / WL_INTER.C `parTimes[]`: par time per level in minutes, indexed by
/// `episode*10 + floor` (the overall map index). Boss (floor 9) and secret
/// (floor 10) levels have no par (0.0). Registered WL6 values, six episodes.
pub const PAR_TIMES: [f32; 60] = [
    // Episode 1
    1.5, 2.0, 2.0, 3.5, 3.0, 3.0, 2.5, 2.5, 0.0, 0.0, //
    // Episode 2
    1.5, 3.5, 3.0, 2.0, 4.0, 6.0, 1.0, 3.0, 0.0, 0.0, //
    // Episode 3
    1.5, 1.5, 2.5, 2.5, 3.5, 2.5, 2.0, 6.0, 0.0, 0.0, //
    // Episode 4
    2.0, 2.0, 1.5, 1.0, 4.5, 3.5, 2.0, 4.5, 0.0, 0.0, //
    // Episode 5
    2.5, 1.5, 2.5, 2.5, 4.0, 3.0, 4.5, 3.5, 0.0, 0.0, //
    // Episode 6
    6.5, 4.0, 4.5, 6.0, 5.0, 5.5, 5.5, 8.5, 0.0, 0.0, //
];

/// Par time for an overall level index, in whole seconds (0 = no par).
pub fn par_seconds(level_idx: usize) -> i32 {
    PAR_TIMES.get(level_idx).map_or(0, |&m| (m * 60.0) as i32)
}

/// A "MM:SS" par-time label, or "??:??" when the level has no par.
pub fn par_string(level_idx: usize) -> String {
    let s = par_seconds(level_idx);
    if s == 0 {
        "??:??".to_string()
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

// =============================================================================
// LEVEL STATISTICS (WL_GAME.C gamestate kill/secret/treasure totals + counts)
// =============================================================================

/// Kill / secret / treasure counters and elapsed time for the current floor.
/// The `*_total`s are counted at level setup; the counts rise during play.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct LevelStats {
    pub kills: i32,
    pub kill_total: i32,
    pub secrets: i32,
    pub secret_total: i32,
    pub treasure: i32,
    pub treasure_total: i32,
    /// Elapsed simulation time on this floor, in seconds.
    pub time: f32,
}

fn ratio(count: i32, total: i32) -> i32 {
    if total > 0 {
        (count * 100 / total).clamp(0, 100)
    } else {
        0 // WL_INTER.C: a zero total reads as 0%, not 100%
    }
}

impl LevelStats {
    pub fn kill_ratio(&self) -> i32 {
        ratio(self.kills, self.kill_total)
    }
    pub fn secret_ratio(&self) -> i32 {
        ratio(self.secrets, self.secret_total)
    }
    pub fn treasure_ratio(&self) -> i32 {
        ratio(self.treasure, self.treasure_total)
    }

    pub fn save(&self, w: &mut Writer) {
        w.put_i32(self.kills);
        w.put_i32(self.kill_total);
        w.put_i32(self.secrets);
        w.put_i32(self.secret_total);
        w.put_i32(self.treasure);
        w.put_i32(self.treasure_total);
        w.put_f32(self.time);
    }

    pub fn load(r: &mut Reader) -> Result<Self, SaveError> {
        Ok(Self {
            kills: r.get_i32()?,
            kill_total: r.get_i32()?,
            secrets: r.get_i32()?,
            secret_total: r.get_i32()?,
            treasure: r.get_i32()?,
            treasure_total: r.get_i32()?,
            time: r.get_f32()?,
        })
    }
}

/// The averaged episode figures shown on the "YOU WIN!" screen (WL_INTER.C
/// `Victory`): the episode's total time and its averaged kill/secret/treasure
/// ratios.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct VictoryStats {
    pub time_sec: i32,
    pub kill: i32,
    pub secret: i32,
    pub treasure: i32,
}

/// WL_INTER.C bonus: `timeleft*PAR_AMOUNT + PERCENT100AMT` per perfect ratio.
/// `timeleft` is the seconds under par (0 when at/over par or the level has no
/// par). Returns `(timeleft, total_bonus)`.
pub fn compute_bonus(time_sec: i32, par_sec: i32, kr: i32, sr: i32, tr: i32) -> (i32, i32) {
    let timeleft = if par_sec > 0 && time_sec < par_sec {
        par_sec - time_sec
    } else {
        0
    };
    let bonus = timeleft * PAR_AMOUNT
        + PERCENT100AMT * (kr == 100) as i32
        + PERCENT100AMT * (sr == 100) as i32
        + PERCENT100AMT * (tr == 100) as i32;
    (timeleft, bonus)
}

// =============================================================================
// INTERMISSION (WL_INTER.C LevelCompleted)
// =============================================================================

/// Points added to the displayed bonus per second, and percent counted per
/// second, while the totals tick up. Cosmetic — the score is awarded up front.
const BONUS_RATE: f32 = 50000.0;
const RATIO_RATE: f32 = 150.0;

/// The animated floor-completed screen. Built after finalizing the floor's
/// stats; the score bonus is already applied when this is constructed.
pub struct Intermission {
    pub floor: i32,
    pub time_sec: i32,
    pub par_str: String,
    /// Kill / secret / treasure percentages.
    pub ratios: [i32; 3],
    pub timeleft: i32,
    pub time_bonus: i32,
    pub total_bonus: i32,
    /// The overall level index to load when the player continues.
    pub next_level: usize,

    // --- Count-up animation state ---
    disp_bonus: i32,
    disp_ratio: [i32; 3],
    breathe: f32,
    /// 0 = time bonus, 1..=3 = kill/secret/treasure ratios, 4 = done.
    phase: usize,
}

impl Intermission {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        floor: i32,
        time_sec: i32,
        par_str: String,
        ratios: [i32; 3],
        timeleft: i32,
        total_bonus: i32,
        next_level: usize,
    ) -> Self {
        Self {
            floor,
            time_sec,
            par_str,
            ratios,
            timeleft,
            time_bonus: timeleft * PAR_AMOUNT,
            total_bonus,
            next_level,
            disp_bonus: 0,
            disp_ratio: [0; 3],
            breathe: 0.0,
            phase: 0,
        }
    }

    /// Advance the count-up one frame, returning any sound-enum ids to play
    /// (the tic ENDBONUS1SND, the per-category ENDBONUS2SND / PERCENT100SND /
    /// NOBONUSSND). Idempotent once every counter has settled.
    pub fn update(&mut self, dt: f32) -> Vec<u8> {
        self.breathe += dt;
        let mut sounds = Vec::new();
        match self.phase {
            0 => {
                let prev = self.disp_bonus;
                self.disp_bonus = (prev + (BONUS_RATE * dt) as i32).min(self.time_bonus);
                if self.disp_bonus / 5000 != prev / 5000 {
                    sounds.push(snd::ENDBONUS1SND as u8);
                }
                if self.disp_bonus >= self.time_bonus {
                    if self.time_bonus > 0 {
                        sounds.push(snd::ENDBONUS2SND as u8);
                    }
                    self.phase = 1;
                }
            }
            1..=3 => {
                let idx = self.phase - 1;
                let target = self.ratios[idx];
                let prev = self.disp_ratio[idx];
                let next = (prev + (RATIO_RATE * dt) as i32).clamp(prev, target);
                if next / 10 != prev / 10 {
                    sounds.push(snd::ENDBONUS1SND as u8);
                }
                self.disp_ratio[idx] = next;
                if next >= target {
                    if target == 100 {
                        self.disp_bonus += PERCENT100AMT;
                        sounds.push(snd::PERCENT100SND as u8);
                    } else if target == 0 {
                        sounds.push(snd::NOBONUSSND as u8);
                    } else {
                        sounds.push(snd::ENDBONUS2SND as u8);
                    }
                    self.phase += 1;
                }
            }
            _ => {}
        }
        sounds
    }
}

// =============================================================================
// RENDERING
// =============================================================================

/// Decoded intermission / load-screen art plus a font, built once at startup.
pub struct InterGfx {
    font: Font,
    guy: [Picture; 2],
    nums: Vec<Picture>,
    psyched: Picture,
    bjwin: Picture,
    highscores: Picture,
    /// WL6 column headers (C_NAMEPIC / C_LEVELPIC / C_SCOREPIC). Absent on
    /// Spear, where HIGHSCORESPIC already paints the headers.
    hs_headers: Option<[Picture; 3]>,
    /// True under Spear of Destiny (full-width high-score banner).
    sod: bool,
    /// The load screen paints itself in the menu's border color, so it follows
    /// the variant (red on WL6, blue on Spear).
    bord: u8,
}

impl InterGfx {
    pub fn new(vga: &VgaGraph, variant: &crate::variant::Variant) -> Self {
        let gfx = &variant.gfx;
        let hs_headers = match (gfx.hs_name, gfx.hs_level, gfx.hs_score) {
            (Some(n), Some(l), Some(s)) => Some([vga.pic(n), vga.pic(l), vga.pic(s)]),
            _ => None,
        };
        Self {
            bord: crate::menu::Colors::for_variant(variant.is_sod()).bord(),
            font: Font::load(vga, 0),
            guy: [vga.pic(gfx.l_guy), vga.pic(gfx.l_guy2)],
            nums: (0..10).map(|d| vga.pic(gfx.l_num0 + d)).collect(),
            psyched: vga.pic(gfx.get_psyched),
            bjwin: vga.pic(gfx.l_bjwins),
            highscores: vga.pic(gfx.high_scores),
            hs_headers,
            sod: variant.is_sod(),
        }
    }

    /// Overlay the deathcam caption (WL_ACT2.C `Write(0,7,STR_SEEAGAIN)`).
    pub fn draw_seeagain(&self, fb: &mut Framebuffer) {
        self.font.draw_centered(fb, 8, "LET'S SEE THAT AGAIN", 0x13);
        self.font.draw_centered(
            fb,
            8 + self.font.height() as i32 + 2,
            "IN SLOW MOTION...",
            0x13,
        );
    }

    /// WL_INTER.C `Victory`: the blue backdrop, the victorious BJ pic, the
    /// "YOU WIN!" banner, and the episode's total time plus averaged ratios.
    pub fn render_victory(&self, fb: &mut Framebuffer, stats: &VictoryStats) {
        fill(fb, VIEWCOLOR);
        blit(fb, &self.bjwin, 8, 4);
        self.font.draw(fb, 18 * 8, 2 * 8, "You Win!", 0x13);

        // Right-hand stats column (WL_INTER.C Write calls).
        let x = 14 * 8;
        self.font.draw(fb, x, 9 * 8, "Total Time", 0x17);
        self.draw_time(fb, 30 * 8, 9 * 8, stats.time_sec);
        let rows = [
            ("Average Kill", stats.kill),
            ("Average Secret", stats.secret),
            ("Average Treasure", stats.treasure),
        ];
        for (i, (label, val)) in rows.iter().enumerate() {
            let y = (12 + i as i32 * 2) * 8;
            self.font.draw(fb, x, y, label, 0x17);
            self.right(fb, 37 * 8, y, &format!("{val}%"));
        }
    }

    /// WL_INTER.C `DrawHighScores`: the board header and the seven ranked rows
    /// (name / level reached / score). `editing` marks a slot mid-name-entry.
    ///
    /// Headers are never drawn as text: WL6 uses C_NAMEPIC / C_LEVELPIC /
    /// C_SCOREPIC sprites; Spear bakes the headers into HIGHSCORESPIC itself.
    pub fn render_high_scores(
        &self,
        fb: &mut Framebuffer,
        table: &[HighScore],
        editing: Option<(usize, &str)>,
    ) {
        // ClearMScreen + DrawStripes(10): variant bord fill, black header band.
        fill(fb, self.bord);
        bar(fb, 0, 10, WIDTH as i32, 24, 0);

        if self.sod {
            // SPEAR: full-width banner that already includes Name/Level/Score.
            blit(fb, &self.highscores, 0, 0);
        } else {
            // WL6: title banner (HIGHSCORESPIC at x=48) plus column-header pics.
            blit(fb, &self.highscores, 48, 0);
            if let Some([name, level, score]) = &self.hs_headers {
                blit(fb, name, 4 * 8, 68);
                blit(fb, level, 20 * 8, 68);
                blit(fb, score, 28 * 8, 68);
            }
        }

        // Rows: PrintY = 76 + 16 * i (WL_INTER.C).
        let base_color = if self.sod { 0x13 } else { 0x0f };
        for (i, e) in table.iter().enumerate() {
            let y = 76 + i as i32 * 16;
            let is_edit = editing.map(|(s, _)| s) == Some(i);
            let color = if is_edit { 0x13 } else { base_color };
            let name = if let Some((_, n)) = editing.filter(|_| is_edit) {
                format!("{n}_")
            } else {
                e.name.clone()
            };
            let name_x = if self.sod { 16 } else { 4 * 8 };
            self.font.draw(fb, name_x, y, &name, color);
            // Level: right-aligned (22*8 - w on WL6, 194 - w on SOD).
            let level = e.completed.to_string();
            let lw = self.font.text_width(&level) as i32;
            let level_right = if self.sod { 194 } else { 22 * 8 };
            self.font.draw(fb, level_right - lw, y, &level, color);
            // Score: right-aligned (34*8 - 8 - w on WL6).
            let score = e.score.to_string();
            let sw = self.font.text_width(&score) as i32;
            let score_right = if self.sod { 34 * 8 } else { 34 * 8 - 8 };
            self.font.draw(fb, score_right - sw, y, &score, color);
        }
    }

    /// LevelCompleted: the blue backdrop, the breathing BJ, the floor number,
    /// time / par, and the three counting ratios.
    pub fn render_intermission(&self, fb: &mut Framebuffer, it: &Intermission) {
        fill(fb, VIEWCOLOR);
        let breather = &self.guy[(it.breathe * 2.0) as usize & 1];
        blit(fb, breather, 0, 16);

        self.font.draw(fb, 14 * 8, 2 * 8, "Floor", 0x17);
        self.font.draw(fb, 14 * 8, 4 * 8, "Completed", 0x17);
        self.font
            .draw(fb, 26 * 8, 2 * 8, &it.floor.to_string(), 0x17);

        // BONUS (right-aligned to cell 36).
        self.font.draw(fb, 14 * 8, 7 * 8, "Bonus", 0x17);
        self.right(fb, 36 * 8, 7 * 8, &it.disp_bonus.to_string());

        // TIME / PAR.
        self.font.draw(fb, 16 * 8, 10 * 8, "Time", 0x17);
        self.draw_time(fb, 26 * 8, 10 * 8, it.time_sec);
        self.font.draw(fb, 16 * 8, 12 * 8, "Par", 0x17);
        self.font.draw(fb, 26 * 8, 12 * 8, &it.par_str, 0x17);

        // Ratios: labels left, counting percentages right-aligned to cell 37.
        let labels = ["Kill Ratio", "Secret Ratio", "Treasure Ratio"];
        // WL_INTER.C Write columns 9 / 5 / 1 (in 8-pixel cells).
        let label_x = [9 * 8, 5 * 8, 8];
        for i in 0..3 {
            let y = (14 + i as i32 * 2) * 8;
            self.font.draw(fb, label_x[i], y, labels[i], 0x17);
            self.right(fb, 37 * 8, y, &format!("{}%", it.disp_ratio[i]));
        }
    }

    /// The "Get Psyched!" load screen: the menu border backdrop, the GETPSYCHED
    /// art centered, and a progress strip that fills as the (instant) load
    /// "runs".
    pub fn render_get_psyched(&self, fb: &mut Framebuffer, progress: f32) {
        fill(fb, self.bord);
        let px = (WIDTH as i32 - self.psyched.width as i32) / 2;
        let py = HEIGHT as i32 / 2 - self.psyched.height as i32 / 2;
        blit(fb, &self.psyched, px, py);
        // A thin progress bar just below the art.
        let bar_x = px;
        let bar_y = py + self.psyched.height as i32 + 4;
        let bar_w = self.psyched.width as i32;
        bar(fb, bar_x, bar_y, bar_w, 6, 0x00);
        let fill_w = (bar_w as f32 * progress.clamp(0.0, 1.0)) as i32;
        bar(fb, bar_x, bar_y, fill_w, 6, 0x53);
    }

    /// Right-align `text` so its last glyph ends at pixel `x_right`.
    fn right(&self, fb: &mut Framebuffer, x_right: i32, y: i32, text: &str) {
        let w = self.font.text_width(text) as i32;
        self.font.draw(fb, x_right - w, y, text, 0x17);
    }

    /// The big level-end MM:SS time using the L_NUM digit pics.
    fn draw_time(&self, fb: &mut Framebuffer, x: i32, y: i32, time_sec: i32) {
        let t = time_sec.min(99 * 60);
        let (min, sec) = (t / 60, t % 60);
        let mut cx = x;
        blit(fb, &self.nums[(min / 10) as usize], cx, y);
        cx += 2 * 8;
        blit(fb, &self.nums[(min % 10) as usize], cx, y);
        cx += 2 * 8;
        self.font.draw(fb, cx, y, ":", 0x17);
        cx += 8;
        blit(fb, &self.nums[(sec / 10) as usize], cx, y);
        cx += 2 * 8;
        blit(fb, &self.nums[(sec % 10) as usize], cx, y);
    }
}

// --- Framebuffer primitives (palette-indexed, like the menu) ---------------

fn fill(fb: &mut Framebuffer, color: u8) {
    fb.pixels.fill(PALETTE[color as usize]);
}

fn bar(fb: &mut Framebuffer, x: i32, y: i32, w: i32, h: i32, color: u8) {
    let rgba = PALETTE[color as usize];
    for yy in y..y + h {
        if yy < 0 || yy as usize >= HEIGHT {
            continue;
        }
        for xx in x..x + w {
            if xx >= 0 && (xx as usize) < WIDTH {
                fb.pixels[yy as usize * WIDTH + xx as usize] = rgba;
            }
        }
    }
}

fn blit(fb: &mut Framebuffer, pic: &vgagraph::Picture, dx: i32, dy: i32) {
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
