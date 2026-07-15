//! Loaders for the original game data files (.WL6), expected in `data/`.

pub mod maps;
pub mod palette;
pub mod vgagraph;
pub mod vswap;

use std::path::PathBuf;

pub use maps::{Level, MAP_SIZE, MapSet};
pub use vgagraph::VgaGraph;
pub use vswap::{TEX_SIZE, VSwap};

/// The data directory: `data/` under the crate root.
pub fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")
}
