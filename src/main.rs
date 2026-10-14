//! Offline Arcturus -> Plus migration, with a non-mutating preflight and recovery archive.
mod archive;
mod audit;
mod context;
mod db;
mod detect;
mod steps;
mod wired;

use anyhow::{Context, Result, bail};
use clap::Parser;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Parser)]
#[command(
    name = "plus-migrate",
    version,
    about = "Rehearse and migrate an offline Arcturus hotel into a fresh Plus database"
)]
struct Cli {
    #[arg(long, env = "PLUS_MIGRATE_SOURCE")]
    source: String,
    #[arg(long, env = "PLUS_MIGRATE_TARGET")]
    target: String,
    #[arg(long)]
    furnidata: Option<PathBuf>,
    /// Validate conversions without clearing or writing any target table.
    #[arg(long)]
    dry_run: bool,
    /// Confirm the SOURCE is an offline snapshot (all source writers stopped).
    #[arg(long)]
    source_offline: bool,
    /// New private directory for the complete source recovery archive. Required to write.
    #[arg(long)]
    archive_dir: Option<PathBuf>,
    /// JSON result (no player text, password hashes or connection URLs).
    #[arg(long, default_value = "migration-report.json")]
    report: PathBuf,
    /// Executable tests/PlusContract validator built against the exact target Plus revision.
    #[arg(long)]
    wired_validator: Option<PathBuf>,
    /// Explicitly clear an already populated target. Never bypasses preflight or identity checks.
    #[arg(long)]
    force: bool,
    /// Diagnostic rehearsal only: write despite reported compatibility blockers; exit remains nonzero.
    #[arg(long)]
    allow_incomplete: bool,
    /// Partial runs are rejected: they can destroy unrelated hotel data.
    #[arg(long, value_delimiter = ',')]
    only: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if !cli.only.is_empty() {
        bail!("--only is unsafe for a complete hotel migration and is no longer supported");
    }
    if !cli.source_offline {
        bail!(
            "use an offline source snapshot and pass --source-offline; concurrent source writes cannot be migrated consistently"
        );
    }
    if !cli.dry_run && cli.archive_dir.is_none() {
        bail!("--archive-dir is required before any target writes");
    }
    let src = db::pool(&cli.source, 32)?;
    let dst = db::pool(&cli.target, 32)?;
    archive::require_distinct(&src, &dst).await?;
    let src_tables = db::table_set(&src).await.context("connecting to source")?;
    let mut dst_schema = db::Schema::load(&dst)
        .await
        .context("connecting to target")?;
    dst_schema.widen("user_wardrobe", "look", 256);
    dst_schema.widen("users", "motto", 127);
    let flavor = detect::detect(&src_tables)?;
    let mut ctx = Arc::new(context::Ctx {
        interaction_coverage: OnceLock::new(),
        source_counts: OnceLock::new(),
        validate_only: true,
        wired_validation: Mutex::new(Vec::new()),
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
    ctx.interaction_coverage
        .set(audit::interactions(&ctx).await?)
        .unwrap();
    ctx.source_counts
        .set(audit::inventory(&ctx).await?)
        .unwrap();
    println!("Preflight ({flavor:?}): target unchanged");
    let validation = context::run_steps(ctx.clone(), steps::all(), &[]).await;
    if let Err(error) = &validation {
        ctx.note(format!("preflight failed: {error:#}"));
    }
    let wired_checked = match validate_wired(&ctx, cli.wired_validator.as_deref()).await {
        Ok(count) => count,
        Err(error) => {
            ctx.note(format!("Wired validator failed: {error:#}"));
            0
        }
    };
    write_report(&cli.report, &ctx, "preflight", None, wired_checked)?;
    let blocked = !ctx.notes.lock().unwrap().is_empty() || validation.is_err();
    if cli.dry_run || validation.is_err() || blocked && !cli.allow_incomplete {
        if blocked {
            bail!(
                "preflight found compatibility blockers; target unchanged. See {}",
                cli.report.display()
            );
        }
        println!("Preflight passed. No target data written.");
        return Ok(());
    }
    let counts = archive::save(&ctx.src, cli.archive_dir.as_deref().unwrap()).await?;
    Arc::get_mut(&mut ctx)
        .context("unfinished preflight tasks")?
        .validate_only = false;
    ctx.maps().next_item_id.store(
        db::scalar_i64(&ctx.src, "SELECT COALESCE(MAX(id),0)+1 FROM items").await?,
        std::sync::atomic::Ordering::Relaxed,
    );
    steps::prep::clear_target(&ctx).await?;
    {
        use mysql_async::prelude::Queryable;
        let mut conn = ctx.dst.get_conn().await?;
        conn.query_drop("ALTER TABLE users MODIFY motto VARCHAR(127) NULL")
            .await?;
        conn.query_drop("ALTER TABLE bots MODIFY z DOUBLE NOT NULL DEFAULT 0")
            .await?;
        conn.query_drop("ALTER TABLE user_wardrobe MODIFY look VARCHAR(256) NOT NULL")
            .await?;
    }
    let migrated = context::run_steps(ctx.clone(), steps::all(), &[]).await;
    if let Err(error) = &migrated {
        ctx.note(format!("migration failed: {error:#}"));
    }
    if migrated.is_ok()
        && let Err(error) = audit::verify(&ctx).await
    {
        ctx.note(format!("integrity audit failed: {error:#}"));
    }
    // Never call a partly compatible run successful, even with diagnostic override.
    let incomplete = !ctx.notes.lock().unwrap().is_empty() || migrated.is_err();
    write_report(
        &cli.report,
        &ctx,
        if incomplete {
            "incomplete"
        } else {
            "converted"
        },
        Some(counts),
        wired_checked,
    )?;
    if incomplete {
        bail!(
            "migration is INCOMPLETE; do not switch the hotel. See {}",
            cli.report.display()
        );
    }
    println!(
        "Conversion completed. Validate the hotel in Plus before cutover; recovery archive retained."
    );
    Ok(())
}

async fn validate_wired(ctx: &context::Ctx, executable: Option<&std::path::Path>) -> Result<usize> {
    use tokio::io::AsyncWriteExt;
    let records = ctx.wired_validation.lock().unwrap().clone();
    if records.is_empty() {
        return Ok(0);
    }
    let Some(executable) = executable else {
        ctx.note("Wired runtime validation required: build tests/PlusContract against the target and pass --wired-validator");
        return Ok(0);
    };
    let mut child = tokio::process::Command::new(executable)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().context("validator stdin")?;
    let payload = records.iter().map(|r| format!("{r}\n")).collect::<String>();
    let send = tokio::spawn(async move {
        input.write_all(payload.as_bytes()).await?;
        input.shutdown().await
    });
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(300),
        child.wait_with_output(),
    )
    .await
    .context("Wired validator timed out")??;
    send.await??;
    let result: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("Wired validator must return JSON with total/errors")?;
    if !output.status.success()
        || result.get("total").and_then(serde_json::Value::as_u64) != Some(records.len() as u64)
        || result.get("errors").and_then(serde_json::Value::as_u64) != Some(0)
    {
        ctx.note(format!(
            "Plus rejected Wired configurations: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
        return Ok(0);
    }
    println!(
        "Wired runtime validation: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    );
    Ok(records.len())
}

fn write_report(
    path: &std::path::Path,
    ctx: &context::Ctx,
    status: &str,
    archive_counts: Option<std::collections::BTreeMap<String, u64>>,
    wired_checked: usize,
) -> Result<()> {
    let mut blockers = ctx.notes.lock().unwrap().clone();
    blockers.sort();
    blockers.dedup();
    let furniture_remaps: std::collections::BTreeMap<_, _> = ctx
        .maps()
        .furni
        .iter()
        .filter(|(id, def)| **id != def.dst_id)
        .map(|(id, def)| (*id, def.dst_id))
        .collect();
    let report = serde_json::json!({"status":status,"furniture_id_remaps":furniture_remaps,"blockers":blockers,"archive_rows":archive_counts,"source_inventory":ctx.source_counts.get(),"table_coverage":ctx.source_counts.get().map(audit::table_coverage),"interactions":ctx.interaction_coverage.get(),
        "source_family":format!("{:?}",ctx.flavor),
        "wired_records_generated":ctx.wired_validation.lock().unwrap().len(),
        "wired_records_checked":wired_checked,
        "target_contract":"PlusEMU dev e3dc5f3437a78900d985821a13172db5d9051dd0 plus SkipStateRestore companion patch (Wired schema 2)",
        "runtime_cutover_verified":false});
    std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
