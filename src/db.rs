//! Database plumbing: connection setup, schema introspection, a lenient row
//! reader for source rows, and a pipelined multi-row INSERT writer for the
//! target.
//!
//! Writes go through the text protocol as extended INSERTs (several MB per
//! statement) inside one transaction per batch, with foreign key and unique
//! checks disabled. That is the fastest way to load MariaDB short of LOAD DATA,
//! and it lets steps run concurrently without caring about insert order.
//! Referential integrity is instead enforced by the steps themselves (orphans
//! are filtered at the source) and checked afterwards by `verify`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use mysql_async::prelude::*;
use mysql_async::{Conn, Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, Row, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Bytes of VALUES text accumulated before a statement is sent. Kept well below
/// max_allowed_packet (64MB on the Plus stack, 16MB MariaDB default).
const BATCH_BYTES: usize = 4 * 1024 * 1024;

pub fn pool(url: &str, max: usize) -> Result<Pool> {
    let opts = Opts::from_url(url).with_context(|| format!("invalid database url: {url}"))?;
    let constraints = PoolConstraints::new(1, max.max(1)).context("pool size")?;
    let builder = OptsBuilder::from_opts(opts)
        .pool_opts(PoolOpts::default().with_constraints(constraints))
        .init(vec![
            "SET NAMES utf8mb4 COLLATE utf8mb4_unicode_ci",
            "SET SESSION group_concat_max_len = 16777216",
        ]);
    Ok(Pool::new(builder))
}

/// A target connection configured for bulk loading.
pub async fn bulk_conn(pool: &Pool) -> Result<Conn> {
    let mut conn = pool.get_conn().await?;
    conn.query_drop(
        "SET SESSION foreign_key_checks = 0, unique_checks = 0, \
         sql_mode = 'STRICT_ALL_TABLES,NO_AUTO_VALUE_ON_ZERO,NO_ENGINE_SUBSTITUTION', \
         time_zone = '+00:00'",
    )
    .await?;
    Ok(conn)
}

// ---------------------------------------------------------------------------
// Schema introspection
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ColumnInfo {
    pub name: String,
    pub generated: bool,
    /// Character limit for char/varchar/text columns.
    pub max_chars: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Schema {
    tables: HashMap<String, Vec<ColumnInfo>>,
}

impl Schema {
    pub async fn load(pool: &Pool) -> Result<Schema> {
        let mut conn = pool.get_conn().await?;
        let rows: Vec<Row> = conn
            .query(
                "SELECT table_name, column_name, data_type, column_type, is_nullable, \
                        column_default, extra, character_maximum_length \
                 FROM information_schema.columns WHERE table_schema = DATABASE() \
                 ORDER BY table_name, ordinal_position",
            )
            .await?;
        let mut tables: HashMap<String, Vec<ColumnInfo>> = HashMap::new();
        for row in rows {
            let r = SrcRow::new(row);
            let extra = r.str("extra").to_ascii_lowercase();
            let data_type = r.str("data_type").to_ascii_lowercase();
            let max_chars = r.opt_i64("character_maximum_length").and_then(|n| {
                // TEXT/LONGTEXT report byte limits; only bound short columns.
                (matches!(data_type.as_str(), "char" | "varchar")).then_some(n as usize)
            });
            tables.entry(r.str("table_name").to_ascii_lowercase()).or_default().push(ColumnInfo {
                name: r.str("column_name"),
                generated: extra.contains("generated") || extra.contains("virtual") || extra.contains("stored"),
                max_chars,
            });
        }
        Ok(Schema { tables })
    }

    pub fn has_table(&self, table: &str) -> bool {
        self.tables.contains_key(&table.to_ascii_lowercase())
    }

    pub fn columns(&self, table: &str) -> Option<&[ColumnInfo]> {
        self.tables.get(&table.to_ascii_lowercase()).map(Vec::as_slice)
    }

    pub fn has_column(&self, table: &str, column: &str) -> bool {
        self.columns(table)
            .is_some_and(|cols| cols.iter().any(|c| c.name.eq_ignore_ascii_case(column)))
    }

}

// ---------------------------------------------------------------------------
// Source rows
// ---------------------------------------------------------------------------

/// A source row read by column name, tolerant of the schema drift between
/// Arcturus releases and Polaris: a missing column reads as empty/zero rather
/// than failing, and numbers stored as strings (common in old dumps) parse.
pub struct SrcRow {
    index: Arc<HashMap<String, usize>>,
    values: Vec<Value>,
}

impl SrcRow {
    pub fn new(row: Row) -> SrcRow {
        let index = row
            .columns_ref()
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name_str().to_ascii_lowercase(), i))
            .collect();
        SrcRow { index: Arc::new(index), values: row.unwrap() }
    }

    fn with_index(index: Arc<HashMap<String, usize>>, row: Row) -> SrcRow {
        SrcRow { index, values: row.unwrap() }
    }

    pub fn raw(&self, col: &str) -> Option<&Value> {
        self.index.get(col).and_then(|&i| self.values.get(i))
    }

    pub fn is_null(&self, col: &str) -> bool {
        matches!(self.raw(col), None | Some(Value::NULL))
    }

    pub fn opt_str(&self, col: &str) -> Option<String> {
        match self.raw(col)? {
            Value::NULL => None,
            Value::Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
            Value::Int(n) => Some(n.to_string()),
            Value::UInt(n) => Some(n.to_string()),
            Value::Float(n) => Some(n.to_string()),
            Value::Double(n) => Some(n.to_string()),
            v @ (Value::Date(..) | Value::Time(..)) => {
                Some(v.as_sql(false).trim_matches('\'').to_string())
            }
        }
    }

    pub fn str(&self, col: &str) -> String {
        self.opt_str(col).unwrap_or_default()
    }

    /// User-facing text, with latin1 mojibake repaired (see `text::repair`).
    pub fn text(&self, col: &str) -> String {
        let raw = self.str(col);
        match crate::text::repair(&raw) {
            std::borrow::Cow::Borrowed(_) => raw,
            std::borrow::Cow::Owned(fixed) => fixed,
        }
    }

    pub fn opt_i64(&self, col: &str) -> Option<i64> {
        match self.raw(col)? {
            Value::NULL => None,
            Value::Int(n) => Some(*n),
            Value::UInt(n) => Some(*n as i64),
            Value::Float(n) => Some(*n as i64),
            Value::Double(n) => Some(*n as i64),
            Value::Bytes(b) => {
                let s = std::str::from_utf8(b).ok()?.trim();
                s.parse::<i64>().ok().or_else(|| s.parse::<f64>().ok().map(|f| f as i64))
            }
            _ => None,
        }
    }

    pub fn i64(&self, col: &str) -> i64 {
        self.opt_i64(col).unwrap_or(0)
    }

    pub fn f64(&self, col: &str) -> f64 {
        match self.raw(col) {
            Some(Value::Int(n)) => *n as f64,
            Some(Value::UInt(n)) => *n as f64,
            Some(Value::Float(n)) => *n as f64,
            Some(Value::Double(n)) => *n,
            Some(Value::Bytes(b)) => std::str::from_utf8(b)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// Arcturus stores booleans as '0'/'1' enums, tinyints, or occasionally 'true'.
    pub fn bool(&self, col: &str) -> bool {
        match self.raw(col) {
            Some(Value::Bytes(b)) => {
                matches!(std::str::from_utf8(b).unwrap_or("").trim(), "1" | "true" | "TRUE" | "yes")
            }
            _ => self.i64(col) != 0,
        }
    }

}

/// Rows streamed from the source by a background task, so reading, transforming
/// and writing overlap. Rows are never fully buffered, so tables of any size
/// stream in constant memory.
pub struct RowStream {
    rx: mpsc::Receiver<Result<SrcRow>>,
}

impl RowStream {
    pub async fn open(pool: &Pool, sql: &str) -> Result<RowStream> {
        let mut conn = pool.get_conn().await?;
        let sql = sql.to_string();
        let (tx, rx) = mpsc::channel::<Result<SrcRow>>(4096);
        // Fail on the query itself before returning, so callers get a clear error.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<()>>();
        tokio::spawn(async move {
            let mut result = match conn.query_iter(sql.as_str()).await {
                Ok(r) => {
                    let _ = ready_tx.send(Ok(()));
                    r
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(anyhow::anyhow!("source query failed: {e}\n  {sql}")));
                    return;
                }
            };
            let mut index: Option<Arc<HashMap<String, usize>>> = None;
            loop {
                match result.next().await {
                    Ok(Some(row)) => {
                        let idx = index
                            .get_or_insert_with(|| {
                                Arc::new(
                                    row.columns_ref()
                                        .iter()
                                        .enumerate()
                                        .map(|(i, c)| (c.name_str().to_ascii_lowercase(), i))
                                        .collect(),
                                )
                            })
                            .clone();
                        if tx.send(Ok(SrcRow::with_index(idx, row))).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(e) => {
                        let _ = tx.send(Err(anyhow::anyhow!("reading source failed: {e}\n  {sql}"))).await;
                        return;
                    }
                }
            }
        });
        ready_rx.await.context("source reader stopped")??;
        Ok(RowStream { rx })
    }

    pub async fn next(&mut self) -> Result<Option<SrcRow>> {
        self.rx.recv().await.transpose()
    }
}

pub async fn query_rows(pool: &Pool, sql: &str) -> Result<Vec<SrcRow>> {
    let mut rows = Vec::new();
    let mut stream = RowStream::open(pool, sql).await?;
    while let Some(r) = stream.next().await? {
        rows.push(r);
    }
    Ok(rows)
}

pub async fn scalar_i64(pool: &Pool, sql: &str) -> Result<i64> {
    let mut conn = pool.get_conn().await?;
    let v: Option<Value> = conn.query_first(sql).await.with_context(|| sql.to_string())?;
    Ok(match v {
        Some(Value::Int(n)) => n,
        Some(Value::UInt(n)) => n as i64,
        Some(Value::Bytes(b)) => String::from_utf8_lossy(&b).trim().parse().unwrap_or(0),
        _ => 0,
    })
}

pub async fn table_set(pool: &Pool) -> Result<HashSet<String>> {
    let mut conn = pool.get_conn().await?;
    let names: Vec<String> = conn
        .query("SELECT table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
        .await?;
    Ok(names.into_iter().map(|n| n.to_ascii_lowercase()).collect())
}

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Sending half of a table writer. Rows are column-ordered per `columns`.
pub struct Sink {
    tx: mpsc::Sender<Vec<Value>>,
    width: usize,
    table: String,
}

impl Sink {
    pub async fn send(&self, row: Vec<Value>) -> Result<()> {
        debug_assert_eq!(row.len(), self.width, "row width mismatch for {}", self.table);
        if row.len() != self.width {
            bail!("row for {} has {} values, expected {}", self.table, row.len(), self.width);
        }
        self.tx.send(row).await.map_err(|_| anyhow::anyhow!("writer for {} stopped", self.table))
    }
}

pub struct Writer {
    pub sink: Sink,
    handle: JoinHandle<Result<u64>>,
}

impl Writer {
    /// Opens a writer for `table`. Every column must exist in the target and
    /// none may be generated; checked up front so schema drift fails fast,
    /// before anything is written.
    pub async fn open(
        pool: &Pool,
        schema: &Schema,
        table: &str,
        columns: &[&str],
        ignore: bool,
    ) -> Result<Writer> {
        let infos = schema
            .columns(table)
            .with_context(|| format!("target table `{table}` does not exist"))?;
        let mut limits = Vec::with_capacity(columns.len());
        for col in columns {
            let info = infos
                .iter()
                .find(|c| c.name.eq_ignore_ascii_case(col))
                .with_context(|| format!("target column `{table}`.`{col}` does not exist"))?;
            if info.generated {
                bail!("target column `{table}`.`{col}` is generated and cannot be written");
            }
            limits.push(info.max_chars);
        }

        let header = format!(
            "INSERT {}INTO `{}` ({}) VALUES ",
            if ignore { "IGNORE " } else { "" },
            table,
            columns.iter().map(|c| format!("`{c}`")).collect::<Vec<_>>().join(", ")
        );
        let (tx, mut rx) = mpsc::channel::<Vec<Value>>(8192);
        let conn = bulk_conn(pool).await?;
        let table_name = table.to_string();

        let handle = tokio::spawn(async move {
            let mut conn = conn;
            let mut buf = String::with_capacity(BATCH_BYTES + 64 * 1024);
            let mut total = 0u64;
            let mut pending = 0usize;
            while let Some(row) = rx.recv().await {
                if pending == 0 {
                    buf.clear();
                    buf.push_str(&header);
                } else {
                    buf.push(',');
                }
                buf.push('(');
                for (i, v) in row.iter().enumerate() {
                    if i > 0 {
                        buf.push(',');
                    }
                    push_sql_value(&mut buf, v, limits[i]);
                }
                buf.push(')');
                pending += 1;
                if buf.len() >= BATCH_BYTES {
                    flush(&mut conn, &buf, &table_name).await?;
                    total += pending as u64;
                    pending = 0;
                }
            }
            if pending > 0 {
                flush(&mut conn, &buf, &table_name).await?;
                total += pending as u64;
            }
            Ok(total)
        });

        Ok(Writer {
            sink: Sink { tx, width: columns.len(), table: table.to_string() },
            handle,
        })
    }

    /// Closes the channel and waits for the final batch; returns rows written.
    pub async fn finish(self) -> Result<u64> {
        drop(self.sink);
        self.handle.await.context("writer task panicked")?
    }
}

async fn flush(conn: &mut Conn, sql: &str, table: &str) -> Result<()> {
    conn.query_drop(sql).await.map_err(|e| {
        let preview: String = sql.chars().take(600).collect();
        anyhow::anyhow!("insert into `{table}` failed: {e}\n  statement starts: {preview}")
    })
}

/// Appends `v` as an SQL literal. Strings longer than `limit` characters are cut
/// to fit (the column would otherwise reject them under strict mode); returns
/// whether that happened.
fn push_sql_value(buf: &mut String, v: &Value, limit: Option<usize>) -> bool {
    match v {
        Value::NULL => buf.push_str("NULL"),
        Value::Int(n) => {
            use std::fmt::Write;
            let _ = write!(buf, "{n}");
        }
        Value::UInt(n) => {
            use std::fmt::Write;
            let _ = write!(buf, "{n}");
        }
        Value::Float(f) => push_float(buf, *f as f64),
        Value::Double(f) => push_float(buf, *f),
        Value::Bytes(b) => {
            let s = String::from_utf8_lossy(b);
            let (s, cut) = match limit {
                Some(max) if s.chars().count() > max => {
                    (std::borrow::Cow::Owned(s.chars().take(max).collect::<String>()), true)
                }
                _ => (s, false),
            };
            push_escaped(buf, &s);
            return cut;
        }
        other => buf.push_str(&other.as_sql(false)),
    }
    false
}

fn push_float(buf: &mut String, f: f64) {
    use std::fmt::Write;
    if f.is_finite() {
        let _ = write!(buf, "{f}");
    } else {
        buf.push('0');
    }
}

fn push_escaped(buf: &mut String, s: &str) {
    buf.reserve(s.len() + 2);
    buf.push('\'');
    for ch in s.chars() {
        match ch {
            '\'' => buf.push_str("\\'"),
            '\\' => buf.push_str("\\\\"),
            '\0' => buf.push_str("\\0"),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\x1a' => buf.push_str("\\Z"),
            c => buf.push(c),
        }
    }
    buf.push('\'');
}

// ---------------------------------------------------------------------------
// Value helpers for building target rows
// ---------------------------------------------------------------------------

pub fn s(v: impl Into<String>) -> Value {
    Value::Bytes(v.into().into_bytes())
}

pub fn i(v: i64) -> Value {
    Value::Int(v)
}

pub fn b(v: bool) -> Value {
    Value::Int(v as i64)
}

pub fn f(v: f64) -> Value {
    Value::Double(v)
}

pub fn null() -> Value {
    Value::NULL
}

pub fn opt_s(v: Option<String>) -> Value {
    v.map(s).unwrap_or(Value::NULL)
}

/// A unix timestamp as a DATETIME literal; non-positive means unknown.
pub fn unix_dt(ts: i64) -> Value {
    if ts <= 0 {
        return Value::NULL;
    }
    match chrono::DateTime::from_timestamp(ts, 0) {
        Some(dt) => s(dt.format("%Y-%m-%d %H:%M:%S").to_string()),
        None => Value::NULL,
    }
}

/// Several writers on separate connections fed round-robin, for the big tables
/// where a single connection's insert rate is the bottleneck.
pub struct MultiWriter {
    writers: Vec<Writer>,
    next: std::sync::atomic::AtomicUsize,
}

impl MultiWriter {
    pub async fn open(pool: &Pool, schema: &Schema, table: &str, columns: &[&str], n: usize) -> Result<MultiWriter> {
        let mut writers = Vec::with_capacity(n);
        for _ in 0..n.max(1) {
            writers.push(Writer::open(pool, schema, table, columns, false).await?);
        }
        Ok(MultiWriter { writers, next: Default::default() })
    }

    pub async fn send(&self, row: Vec<Value>) -> Result<()> {
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) / 512;
        self.writers[n % self.writers.len()].sink.send(row).await
    }

    pub async fn finish(self) -> Result<u64> {
        let mut total = 0;
        for w in self.writers {
            total += w.finish().await?;
        }
        Ok(total)
    }
}
