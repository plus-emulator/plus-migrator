//! Friends, ignores, searches, favourites, bans, room visits, chat logs.

use std::sync::Arc;

use anyhow::Result;

use crate::context::Ctx;
use crate::db::{RowStream, i, s, unix_dt};

/// Far-future expiry for permanent bans: Plus deletes NULL-expiry bans at boot.
const PERMANENT: &str = "2099-12-31 00:00:00";

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    let both = "JOIN users a ON a.id = t.{a} JOIN users b ON b.id = t.{b}";
    let pair = |a: &str, b: &str| both.replace("{a}", a).replace("{b}", b);

    // Friendships: Arcturus already stores both directions; INSERT IGNORE plus
    // the mirrored row guarantees both exist.
    if ctx.has_src("messenger_friendships") {
        let w = ctx.writer_ignore("messenger_friendships", &["user_one_id", "user_two_id", "relationship"]).await?;
        let mut rows = RowStream::open(
            &ctx.src,
            &format!("SELECT t.* FROM messenger_friendships t {} WHERE t.user_one_id <> t.user_two_id", pair("user_one_id", "user_two_id")),
        )
        .await?;
        while let Some(r) = rows.next().await? {
            let (a, b, rel) = (r.i64("user_one_id"), r.i64("user_two_id"), r.i64("relation").clamp(0, 3));
            w.sink.send(vec![i(a), i(b), i(rel)]).await?;
            w.sink.send(vec![i(b), i(a), i(0)]).await?;
        }
        w.finish().await?;
    }
    copy(&ctx, "messenger_friendrequests", &pair("user_from_id", "user_to_id"), "messenger_requests", &["from_id", "to_id"], &["user_from_id", "user_to_id"]).await?;
    copy(&ctx, "users_ignored", &pair("user_id", "target_id"), "user_ignores", &["user_id", "ignore_id"], &["user_id", "target_id"]).await?;
    copy(&ctx, "users_favorite_rooms", "JOIN users a ON a.id = t.user_id JOIN rooms r ON r.id = t.room_id", "user_favorites", &["user_id", "room_id"], &["user_id", "room_id"]).await?;

    if ctx.has_src("users_saved_searches") {
        let w = ctx.writer("user_saved_searches", &["user_id", "filter", "search_code"]).await?;
        let mut rows = RowStream::open(&ctx.src, "SELECT t.* FROM users_saved_searches t JOIN users a ON a.id = t.user_id").await?;
        while let Some(r) = rows.next().await? {
            w.sink.send(vec![i(r.i64("user_id")), s(r.text("filter")), s(r.str("search_code"))]).await?;
        }
        w.finish().await?;
    }

    bans(&ctx).await?;

    if ctx.has_src("room_enter_log") {
        let w = ctx.writer("user_roomvisits", &["user_id", "room_id", "entry_timestamp", "exit_timestamp"]).await?;
        let mut rows = RowStream::open(&ctx.src, "SELECT t.* FROM room_enter_log t JOIN users a ON a.id = t.user_id JOIN rooms r ON r.id = t.room_id").await?;
        while let Some(r) = rows.next().await? {
            w.sink.send(vec![i(r.i64("user_id")), i(r.i64("room_id")), unix_dt(r.i64("timestamp")), unix_dt(r.i64("exit_timestamp"))]).await?;
        }
        w.finish().await?;
    }

    if ctx.has_src("chatlogs_room") {
        let w = ctx.writer("chatlogs", &["user_id", "room_id", "message", "timestamp"]).await?;
        let mut rows = RowStream::open(&ctx.src, "SELECT room_id, user_from_id, message, timestamp FROM chatlogs_room WHERE user_from_id > 0").await?;
        while let Some(r) = rows.next().await? {
            w.sink.send(vec![i(r.i64("user_from_id")), i(r.i64("room_id").max(0)), s(r.text("message")), unix_dt(r.i64("timestamp"))]).await?;
        }
        w.finish().await?;
    }
    if ctx.has_src("messenger_offline") {
        let w = ctx.writer("messenger_offline_messages", &["to_id", "from_id", "message", "timestamp"]).await?;
        let mut rows = RowStream::open(&ctx.src, &format!("SELECT t.* FROM messenger_offline t {}", pair("user_id", "user_from_id"))).await?;
        while let Some(r) = rows.next().await? {
            w.sink.send(vec![i(r.i64("user_id")), i(r.i64("user_from_id")), s(r.text("message")), unix_dt(r.i64("timestamp"))]).await?;
        }
        w.finish().await?;
    }
    Ok(())
}

/// Straight two-column copy with source-side orphan filtering.
async fn copy(ctx: &Ctx, src: &str, join: &str, dst: &str, dst_cols: &[&str], src_cols: &[&str]) -> Result<()> {
    if !ctx.has_src(src) {
        return Ok(());
    }
    let w = ctx.writer_ignore(dst, dst_cols).await?;
    let mut rows = RowStream::open(&ctx.src, &format!("SELECT t.* FROM `{src}` t {join}")).await?;
    while let Some(r) = rows.next().await? {
        w.sink.send(src_cols.iter().map(|c| i(r.i64(c))).collect()).await?;
    }
    w.finish().await?;
    Ok(())
}

/// One Arcturus ban row becomes one Plus row per banned identity. User bans
/// are keyed by username in Plus. Expired bans are skipped.
async fn bans(ctx: &Ctx) -> Result<()> {
    if !ctx.has_src("bans") {
        return Ok(());
    }
    let maps = ctx.maps();
    let now = chrono::Utc::now().timestamp();
    let w = ctx.writer("bans", &["bantype", "value", "reason", "expire", "added_by", "added_date"]).await?;
    let mut rows = RowStream::open(&ctx.src, "SELECT * FROM bans").await?;
    while let Some(r) = rows.next().await? {
        let expire = r.i64("ban_expire");
        if expire > 0 && expire < now && expire < i32::MAX as i64 {
            continue;
        }
        let expire = if expire <= 0 || expire >= i32::MAX as i64 || expire - now > 70 * 365 * 86_400 {
            s(PERMANENT)
        } else {
            unix_dt(expire)
        };
        let kind = r.str("type").to_ascii_lowercase();
        let staff = maps.user_names.get(&r.i64("user_staff_id")).cloned().unwrap_or_default();
        let mut targets = vec![];
        if matches!(kind.as_str(), "account" | "super") {
            if let Some(name) = maps.user_names.get(&r.i64("user_id")) {
                targets.push(("user", name.clone()));
            }
        }
        if matches!(kind.as_str(), "ip" | "super") && !r.str("ip").is_empty() {
            targets.push(("ip", r.str("ip")));
        }
        if matches!(kind.as_str(), "machine" | "super") && !r.str("machine_id").is_empty() {
            targets.push(("machine", r.str("machine_id")));
        }
        for (t, value) in targets {
            w.sink
                .send(vec![s(t), s(value), s(r.text("ban_reason")), expire.clone(), s(staff.clone()), unix_dt(r.i64("timestamp"))])
                .await?;
        }
    }
    w.finish().await?;
    Ok(())
}
