//! plus-migrate: ports an Arcturus 3.5.x or Polaris hotel into a fresh PlusEMU
//! database — players, rooms, furniture, catalog, wired and more.

mod context;
mod db;
mod detect;
mod steps;
mod text;
mod wired;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(name = "plus-migrate", version, about = "Port an Arcturus 3.5.x / Polaris hotel into a fresh PlusEMU database")]
struct Cli {
    /// Source database, e.g. mysql://user:pass@host:3306/arcturus
    #[arg(long, env = "PLUS_MIGRATE_SOURCE")]
    source: String,
    /// Target (fresh Plus) database, e.g. mysql://user:pass@host:3306/plus
    #[arg(long, env = "PLUS_MIGRATE_TARGET")]
    target: String,
    /// The hotel's FurnitureData.json, to fill Plus's furnidata columns.
    #[arg(long)]
    furnidata: Option<std::path::PathBuf>,
    /// Wipe the target even if it already contains users.
    #[arg(long)]
    force: bool,
    /// Run only these steps (comma separated).
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let started = Instant::now();

    let src = db::pool(&cli.source, 32)?;
    let dst = db::pool(&cli.target, 32)?;
    let src_tables = db::table_set(&src).await.context("connecting to source")?;
    let dst_schema = db::Schema::load(&dst).await.context("connecting to target")?;
    let flavor = detect::detect(&src_tables)?;
    println!("source: {flavor:?}");

    let ctx = Arc::new(context::Ctx {
        src,
        dst,
        flavor,
        src_tables,
        dst_schema,
        furnidata: cli.furnidata,
        maps: OnceLock::new(),
        notes: Mutex::new(Vec::new()),
    });

    steps::prep::prepare(&ctx, cli.force).await?;
    context::run_steps(ctx.clone(), steps::all(), &cli.only).await?;

    let notes = ctx.notes.lock().unwrap();
    if !notes.is_empty() {
        println!("\nnotes:");
        for n in notes.iter() {
            println!("  - {n}");
        }
    }
    println!("\ndone in {:.1}s", started.elapsed().as_secs_f64());
    Ok(())
}
