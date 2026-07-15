//! The high-score table (WL_INTER.C `DrawHighScores` / `CheckHighScore`), the
//! seven-entry board shown after an episode win or a game over. The original
//! kept the scores in the config file; here they live in their own small
//! versioned file next to the save slots (`highscores.bin`), written with the
//! same [`crate::savegame`] `Writer`/`Reader` primitives.

use crate::savegame::{Reader, SaveError, Writer, saves_dir};

/// WL_DEF.H `MaxScores`: the board holds seven entries.
pub const MAX_SCORES: usize = 7;
/// WL_DEF.H `MaxHighName`: longest name the entry field accepts. The original
/// allowed 57; we cap tighter so the name fits the drawn name column.
pub const MAX_HIGH_NAME: usize = 24;

/// On-disk format magic + version for `highscores.bin`.
const MAGIC: &[u8; 8] = b"WOLF3DHI";
const VERSION: u16 = 1;

/// One board row: a name, a score, and how far that run got. `completed` is the
/// 1-based overall level index reached (the "Level" column in the original).
#[derive(Clone, Debug, PartialEq)]
pub struct HighScore {
    pub name: String,
    pub score: i32,
    pub completed: u32,
}

/// The factory default board (WL_INTER.C `Scores[]`): the id Software crew, each
/// seeded at 10000 points having "completed" level 1.
pub fn default_table() -> Vec<HighScore> {
    const DEFAULTS: [(&str, i32, u32); MAX_SCORES] = [
        ("id software-'92", 10000, 1),
        ("Adrian Carmack", 10000, 1),
        ("John Carmack", 10000, 1),
        ("Kevin Cloud", 10000, 1),
        ("Tom Hall", 10000, 1),
        ("John Romero", 10000, 1),
        ("Jay Wilbur", 10000, 1),
    ];
    DEFAULTS
        .iter()
        .map(|&(name, score, completed)| HighScore {
            name: name.to_string(),
            score,
            completed,
        })
        .collect()
}

/// Path of the high-score file (alongside the save slots).
pub fn path() -> std::path::PathBuf {
    saves_dir().join("highscores.bin")
}

/// Serialize a board to a versioned byte buffer.
pub fn write_table(table: &[HighScore]) -> Vec<u8> {
    let mut w = Writer::new();
    w.buf.extend_from_slice(MAGIC);
    w.put_u16(VERSION);
    w.put_u32(table.len() as u32);
    for e in table {
        w.put_str(&e.name);
        w.put_i32(e.score);
        w.put_u32(e.completed);
    }
    w.buf
}

/// Parse a board from bytes; errors on bad magic/version/truncation.
pub fn read_table(data: &[u8]) -> Result<Vec<HighScore>, SaveError> {
    let mut r = Reader::new(data);
    if r.get_bytes(8)? != MAGIC {
        return Err(SaveError::BadMagic);
    }
    let version = r.get_u16()?;
    if version != VERSION {
        return Err(SaveError::BadVersion(version));
    }
    let n = r.get_u32()? as usize;
    let mut table = Vec::with_capacity(n.min(MAX_SCORES));
    for _ in 0..n {
        let name = r.get_str()?;
        let score = r.get_i32()?;
        let completed = r.get_u32()?;
        table.push(HighScore {
            name,
            score,
            completed,
        });
    }
    Ok(table)
}

/// Load the board from disk, falling back to the factory default when the file
/// is missing or unreadable/corrupt. The result is always exactly [`MAX_SCORES`]
/// rows, sorted high-to-low.
pub fn load() -> Vec<HighScore> {
    let mut table = std::fs::read(path())
        .ok()
        .and_then(|d| read_table(&d).ok())
        .unwrap_or_else(default_table);
    normalize(&mut table);
    table
}

/// Write the board to disk (creating the directory). IO errors are returned.
pub fn store(table: &[HighScore]) -> Result<(), SaveError> {
    std::fs::create_dir_all(saves_dir())?;
    std::fs::write(path(), write_table(table))?;
    Ok(())
}

/// Pad/trim to exactly [`MAX_SCORES`] rows and sort high-to-low (score, then
/// completed as the tiebreak — the original's ordering).
fn normalize(table: &mut Vec<HighScore>) {
    table.sort_by(|a, b| b.score.cmp(&a.score).then(b.completed.cmp(&a.completed)));
    table.truncate(MAX_SCORES);
    while table.len() < MAX_SCORES {
        table.push(HighScore {
            name: String::new(),
            score: 0,
            completed: 0,
        });
    }
}

/// Where a `(score, completed)` run would land on the board, or `None` if it
/// does not beat the last place (WL_INTER.C `CheckHighScore` qualification: a
/// strictly higher score, or an equal score reaching a further level).
pub fn qualifying_slot(table: &[HighScore], score: i32, completed: u32) -> Option<usize> {
    table
        .iter()
        .position(|e| score > e.score || (score == e.score && completed > e.completed))
}

/// Insert a run at its qualifying slot, shifting lower rows down and dropping
/// the last (WL_INTER.C `CheckHighScore`). Returns the slot it landed in, or
/// `None` if it did not qualify. The inserted row's name starts empty, ready for
/// the name-entry field to fill.
pub fn insert(table: &mut Vec<HighScore>, score: i32, completed: u32) -> Option<usize> {
    let slot = qualifying_slot(table, score, completed)?;
    table.insert(
        slot,
        HighScore {
            name: String::new(),
            score,
            completed,
        },
    );
    table.truncate(MAX_SCORES);
    Some(slot)
}
