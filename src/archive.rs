//! A byte-preserving recovery copy, including source tables Plus cannot consume.
//! This is recovery evidence, never a claim of runtime compatibility.
use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use mysql_async::{Pool, Row, Value, prelude::Queryable};

pub fn ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

pub async fn save(pool: &Pool, directory: &Path) -> Result<BTreeMap<String, u64>> {
    std::fs::create_dir(directory).context("archive directory must not already exist")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("source.sql"))?;
    let mut out = BufWriter::new(file);
    writeln!(
        out,
        "SET NAMES binary;\nSET FOREIGN_KEY_CHECKS=0;\nSET SQL_MODE='NO_AUTO_VALUE_ON_ZERO';"
    )?;
    let mut conn = pool.get_conn().await?;
    // Prevent the server converting latin1/BLOB bytes before they are archived.
    conn.query_drop("SET character_set_results=binary").await?;
    let tables: Vec<String> = conn.query("SELECT table_name FROM information_schema.tables WHERE table_schema=DATABASE() AND table_type='BASE TABLE' ORDER BY table_name").await?;
    let mut counts = BTreeMap::new();
    for table in tables {
        let name = ident(&table);
        let ddl: Row = conn
            .query_first(format!("SHOW CREATE TABLE {name}"))
            .await?
            .context("missing source DDL")?;
        let ddl: String = ddl.get(1).context("invalid source DDL")?;
        writeln!(out, "{ddl};")?;
        let columns: Vec<String> = conn.exec("SELECT column_name FROM information_schema.columns WHERE table_schema=DATABASE() AND table_name=? AND extra NOT LIKE '%GENERATED%' ORDER BY ordinal_position", (&table,)).await?;
        let columns = columns
            .iter()
            .map(|c| ident(c))
            .collect::<Vec<_>>()
            .join(",");
        let mut rows = conn
            .query_iter(format!("SELECT {columns} FROM {name}"))
            .await?;
        let mut count = 0;
        while let Some(row) = rows.next().await? {
            write!(out, "INSERT INTO {name} ({columns}) VALUES (")?;
            for (index, value) in row.unwrap().iter().enumerate() {
                if index > 0 {
                    write!(out, ",")?;
                }
                match value {
                    Value::Bytes(bytes) => {
                        write!(out, "X'")?;
                        for byte in bytes {
                            write!(out, "{byte:02x}")?;
                        }
                        write!(out, "'")?;
                    }
                    _ => write!(out, "{}", value.as_sql(false))?,
                }
            }
            writeln!(out, ");")?;
            count += 1;
        }
        rows.drop_result().await?;
        counts.insert(table, count);
    }
    writeln!(out, "SET FOREIGN_KEY_CHECKS=1;")?;
    out.flush()?;
    out.get_ref().sync_all()?;
    conn.query_drop("SET NAMES utf8mb4 COLLATE utf8mb4_unicode_ci")
        .await?;
    std::fs::write(
        directory.join("counts.json"),
        serde_json::to_vec_pretty(&counts)?,
    )?;
    Ok(counts)
}

/// Compare the server's own identity, not just URL strings (localhost and
/// 127.0.0.1, DNS aliases and alternate accounts can reach the same database).
pub async fn require_distinct(src: &Pool, dst: &Pool) -> Result<()> {
    async fn identity(pool: &Pool) -> Result<(String, String, String)> {
        let mut conn = pool.get_conn().await?;
        conn.query_first("SELECT @@hostname, @@datadir, DATABASE()")
            .await?
            .context("database must be specified")
    }
    if identity(src).await? == identity(dst).await? {
        bail!("source and target resolve to the same database; refusing to modify it");
    }
    Ok(())
}
