//! Shared state for a migration run and the parallel step runner.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use anyhow::Result;
use mysql_async::Pool;

use crate::db::{Schema, Writer};
use crate::steps::prep::Maps;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Arcturus,
    Polaris,
}

pub struct Ctx {
    pub validate_only: bool,
    pub interaction_coverage: OnceLock<serde_json::Value>,
    pub source_counts: OnceLock<std::collections::BTreeMap<String, i64>>,
    pub wired_validation: Mutex<Vec<serde_json::Value>>,
    pub src: Pool,
    pub dst: Pool,
    pub flavor: Flavor,
    pub src_tables: HashSet<String>,
    pub dst_schema: Schema,
    pub furnidata: Option<std::path::PathBuf>,
    pub maps: OnceLock<Maps>,
    /// Things that could not be carried over, printed at the end.
    pub notes: Mutex<Vec<String>>,
}

impl Ctx {
    pub fn has_src(&self, table: &str) -> bool {
        self.src_tables.contains(&table.to_ascii_lowercase())
    }

    pub fn maps(&self) -> &Maps {
        self.maps.get().expect("maps not built")
    }

    pub async fn writer(&self, table: &str, columns: &[&str]) -> Result<Writer> {
        Writer::open(
            &self.dst,
            &self.dst_schema,
            table,
            columns,
            false,
            self.validate_only,
        )
        .await
    }

    /// Writer that skips rows whose key already exists (reference data).
    pub async fn writer_ignore(&self, table: &str, columns: &[&str]) -> Result<Writer> {
        Writer::open(
            &self.dst,
            &self.dst_schema,
            table,
            columns,
            true,
            self.validate_only,
        )
        .await
    }

    pub fn note(&self, msg: impl Into<String>) {
        self.notes.lock().unwrap().push(msg.into());
    }
}

pub type StepFn = fn(Arc<Ctx>) -> Pin<Box<dyn Future<Output = Result<()>> + Send>>;

/// Steps in the same phase run concurrently; phases run in order.
pub struct Step {
    pub name: &'static str,
    pub phase: u8,
    pub run: StepFn,
}

macro_rules! step {
    ($name:literal, $phase:literal, $f:path) => {
        $crate::context::Step {
            name: $name,
            phase: $phase,
            run: |ctx| Box::pin($f(ctx)),
        }
    };
}
pub(crate) use step;

pub async fn run_steps(ctx: Arc<Ctx>, steps: Vec<Step>, only: &[String]) -> Result<()> {
    let steps: Vec<Step> = steps
        .into_iter()
        .filter(|s| only.is_empty() || only.iter().any(|o| o == s.name))
        .collect();
    let mut phases: Vec<u8> = steps.iter().map(|s| s.phase).collect();
    phases.sort_unstable();
    phases.dedup();
    for phase in phases {
        let mut set = tokio::task::JoinSet::new();
        for s in steps.iter().filter(|s| s.phase == phase) {
            let (name, run, ctx) = (s.name, s.run, ctx.clone());
            set.spawn(async move {
                let t = Instant::now();
                let r = run(ctx).await;
                match &r {
                    Ok(()) => println!("  {name:<22} {:>6.1}s", t.elapsed().as_secs_f64()),
                    Err(e) => eprintln!("  {name:<22} FAILED: {e:#}"),
                }
                r
            });
        }
        let mut errors = Vec::new();
        while let Some(r) = set.join_next().await {
            match r {
                Ok(Ok(())) => {}
                Ok(Err(e)) => errors.push(format!("{e:#}")),
                Err(e) => errors.push(e.to_string()),
            }
        }
        if !errors.is_empty() {
            anyhow::bail!("migration stopped: {}", errors.join("; "));
        }
    }
    Ok(())
}
