//! AI demo forge: multi-trial search for fair complete-floor attract demos.
//!
//! Scoring: end level +100, each kill +10, each secret +1 (faster breaks ties).
//! Warm start: existing `demos/eXmY.dm` is re-scored; the file is only overwritten
//! when a better run is found.
//!
//! Invoked via `wolf3d forge` (see the binary CLI).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::ai::{self, Policy, SearchFocus};
use crate::demorec;
use crate::game::{Game, TIC};

/// Options for a forge run.
#[derive(Clone, Debug)]
pub struct ForgeOptions {
    /// Overall level indices to search (0-based). Empty = every registered floor.
    pub levels: Vec<usize>,
    pub out_dir: PathBuf,
    pub iters: u64,
    pub threads: usize,
    pub max_secs: f32,
    pub progress_every: u64,
    pub god: bool,
    pub focus: SearchFocus,
}

impl Default for ForgeOptions {
    fn default() -> Self {
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        Self {
            levels: Vec::new(),
            out_dir: PathBuf::from("demos"),
            iters: 50_000,
            threads,
            max_secs: 120.0,
            progress_every: 0, // filled in run() if 0
            god: false,
            focus: SearchFocus::Secrets,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteStatus {
    Improved,
    Kept,
}

/// Run the forge for the configured levels. Returns process-style exit code (0/1).
pub fn run(mut opts: ForgeOptions) -> i32 {
    if opts.levels.is_empty() {
        let probe = Game::new(0);
        opts.levels = (0..probe.maps.num_levels()).collect();
    }
    if opts.progress_every == 0 {
        opts.progress_every = (opts.iters / 25).max(200);
    }
    let max_tics = (opts.max_secs / TIC).ceil() as u32;
    std::fs::create_dir_all(&opts.out_dir).expect("create demos dir");

    println!("wolf3d forge");
    println!(
        "  levels {}  trials/level {}  threads {}  max {:.0}s/trial  warm-start on",
        opts.levels.len(),
        opts.iters,
        opts.threads,
        opts.max_secs
    );
    println!(
        "  mode: {}  focus: {}",
        if opts.god { "god" } else { "mortal" },
        match opts.focus {
            SearchFocus::Score => "score",
            SearchFocus::Secrets => "secrets first",
        }
    );
    println!("  score: finish +100, kill +10, secret +1  (faster wins ties)");
    println!();

    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut kept = 0usize;
    let mut improved = 0usize;
    // Aggregate got vs map totals across successful floors.
    let mut sum_pts = 0i64;
    let mut sum_ideal = 0i64;
    let mut sum_k = 0i64;
    let mut sum_kt = 0i64;
    let mut sum_s = 0i64;
    let mut sum_st = 0i64;
    let mut sum_t = 0i64;
    let mut sum_tt = 0i64;
    let mut sum_pickups = 0i64;
    let mut perfect = 0usize;

    for &level_idx in &opts.levels {
        let name = level_name(level_idx);
        print!("{name}");
        let _ = std::io::stdout().flush();
        let t0 = Instant::now();
        match generate_one(
            level_idx,
            &opts.out_dir,
            opts.iters,
            opts.threads,
            max_tics,
            opts.progress_every,
            opts.god,
            opts.focus,
        ) {
            Ok((_path, result, status)) => {
                ok += 1;
                match status {
                    WriteStatus::Improved => improved += 1,
                    WriteStatus::Kept => kept += 1,
                }
                let demo_tics = result.demo.as_ref().map(|d| d.tics.len()).unwrap_or(0);
                let tag = match status {
                    WriteStatus::Improved => "improved",
                    WriteStatus::Kept => "unchanged",
                };
                let pts = result.points();
                let ideal = result.ideal_points();
                let pct = result.ideal_ratio() * 100.0;
                if pts >= ideal && ideal > 0 {
                    perfect += 1;
                }
                sum_pts += i64::from(pts);
                sum_ideal += i64::from(ideal);
                sum_k += i64::from(result.kills);
                sum_kt += i64::from(result.kill_total);
                sum_s += i64::from(result.secrets);
                sum_st += i64::from(result.secret_total);
                sum_t += i64::from(result.treasure);
                sum_tt += i64::from(result.treasure_total);
                sum_pickups += i64::from(result.pickups);

                // One clear block per floor — got vs map max, not "x/y ideal".
                println!();
                println!("  result     {tag}");
                println!(
                    "  time       {:.1}s play  ({} tics)   search {:.1}s",
                    demo_tics as f32 / 70.0,
                    demo_tics,
                    t0.elapsed().as_secs_f32()
                );
                println!(
                    "  score      {pts} of {ideal} max  ({pct:.0}%)   hp {}",
                    result.health
                );
                println!(
                    "  kills      {} of {} on map",
                    result.kills, result.kill_total
                );
                println!(
                    "  secrets    {} of {} on map",
                    result.secrets, result.secret_total
                );
                println!(
                    "  treasure   {} of {} on map  (not scored)",
                    result.treasure, result.treasure_total
                );
                println!("  pickups    {} collected", result.pickups);
                println!();
            }
            Err(e) => {
                fail += 1;
                println!();
                println!("  result     failed  ({:.1}s)", t0.elapsed().as_secs_f32());
                println!("  error      {e}");
                println!();
            }
        }
    }

    println!("summary");
    println!("  floors     {ok} ok, {fail} failed  ({improved} improved, {kept} unchanged)");
    if ok > 0 && sum_ideal > 0 {
        let pct = sum_pts as f32 / sum_ideal as f32 * 100.0;
        println!("  score      {sum_pts} of {sum_ideal} max across floors  ({pct:.1}%)");
        println!("  kills      {sum_k} of {sum_kt} on maps");
        println!("  secrets    {sum_s} of {sum_st} on maps");
        println!("  treasure   {sum_t} of {sum_tt} on maps  (not scored)");
        println!("  pickups    {sum_pickups} collected");
        println!("  perfect    {perfect} of {ok} floors hit map max score");
    }
    if fail > 0 { 1 } else { 0 }
}

#[allow(clippy::too_many_arguments)]
fn generate_one(
    level_idx: usize,
    out_dir: &Path,
    iters: u64,
    threads: usize,
    max_tics: u32,
    progress_every: u64,
    god: bool,
    focus: SearchFocus,
) -> Result<(PathBuf, ai::TrialResult, WriteStatus), String> {
    let path_out = out_dir.join(format!("{}.dm", level_name(level_idx)));

    let warm = load_warm_start(level_idx, &path_out, god, focus);
    if let Some(ref w) = warm {
        // Brief status on the same line, then a full block after search.
        eprint!(
            "  (warm: score {} · {} kills · {} secrets · {:.0}s) … searching",
            w.points(),
            w.kills,
            w.secrets,
            w.tics as f32 / 70.0
        );
        let _ = std::io::stderr().flush();
    } else {
        eprint!("  searching");
        let _ = std::io::stderr().flush();
    }

    let mut game = Game::new(level_idx);
    let mut best = warm.clone().unwrap_or_else(|| {
        let mut policy = Policy::full_clear(1);
        configure_policy(&mut policy, god, focus);
        let mut result = ai::run_trial(&mut game, level_idx, policy, max_tics, false);
        focus.rank(&mut result);
        result
    });

    // Seed with a mix of full-clear and any% policies.
    for s in 1u32..32 {
        let seed = s.wrapping_mul(1103515245).wrapping_add(12345);
        let mut policy = if s.is_multiple_of(4) {
            Policy::speedrun(seed)
        } else {
            Policy::full_clear(seed)
        };
        configure_policy(&mut policy, god, focus);
        let mut r = ai::run_trial(&mut game, level_idx, policy, max_tics, false);
        focus.rank(&mut r);
        if r.fitness > best.fitness {
            best = r;
        }
    }

    let searched = ai::search_level(
        level_idx,
        iters,
        threads,
        max_tics,
        progress_every,
        Some(best.clone()).filter(|b| b.completed),
        god,
        focus,
    );
    if searched.fitness > best.fitness {
        best = searched;
    }

    if best.completed && best.demo.is_none() {
        let cap = best
            .tics
            .saturating_add(140)
            .min(max_tics)
            .max(best.tics + 35);
        let rec = ai::run_trial_record(&mut game, level_idx, best.policy.clone(), cap);
        let mut rec = rec;
        focus.rank(&mut rec);
        if rec.completed {
            best = rec;
        }
    }

    if !best.completed {
        if let Some(w) = warm.filter(|w| w.completed) {
            return Ok((path_out, w, WriteStatus::Kept));
        }
        return Err(format!(
            "no completion in {iters} trials (best fitness {}, k={}, tics={})",
            best.fitness, best.kills, best.tics
        ));
    }

    let status = if warm.as_ref().is_none_or(|w| best.fitness > w.fitness) {
        let demo = best.demo.clone().ok_or("completed trial missing demo")?;
        demo.write_to(&path_out)
            .map_err(|e| format!("write {}: {e}", path_out.display()))?;
        WriteStatus::Improved
    } else {
        if let Some(w) = warm {
            best = w;
        }
        WriteStatus::Kept
    };

    Ok((path_out, best, status))
}

fn configure_policy(policy: &mut Policy, god: bool, focus: SearchFocus) {
    policy.god = god;
    if focus == SearchFocus::Secrets {
        policy.seek_secrets = true;
        policy.hunt_kills = false;
    }
}

fn load_warm_start(
    level_idx: usize,
    path: &Path,
    god: bool,
    focus: SearchFocus,
) -> Option<ai::TrialResult> {
    if !path.is_file() {
        return None;
    }
    let demo = demorec::load_path(path).ok()?;
    if demo.level_idx != level_idx {
        return None;
    }
    // Legacy generator shortcuts are not forge champions. Mortal mode also
    // never warm-starts from a god recording.
    if demo.clear_actors
        || (!god && demo.god)
        || demo.tics.iter().any(|input| input.turn_delta != 0.0)
    {
        return None;
    }
    let mut result = ai::evaluate_demo(demo)?;
    focus.rank(&mut result);
    Some(result)
}

fn level_name(level_idx: usize) -> String {
    let ep = level_idx / 10 + 1;
    let fl = level_idx % 10 + 1;
    format!("e{ep}m{fl}")
}
