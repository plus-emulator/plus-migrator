//! users and every per-user table.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use crate::context::Ctx;
use crate::db::{RowStream, b, i, opt_s, query_rows, s, unix_dt};

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    let maps = ctx.maps();
    let has_last_login = ctx.dst_schema.has_column("users", "last_login");

    let mut cols = vec![
        "id", "username", "password", "mail", "rank", "credits", "look", "gender", "motto", "online", "ip_last",
        "ip_reg", "vip", "account_created", "last_online",
    ];
    if has_last_login {
        cols.push("last_login");
    }
    let uw = ctx.writer("users", &cols).await?;
    let sw = ctx
        .writer(
            "users_settings",
            &["user_id", "home_room", "block_newfriends", "volume", "focus_preference", "chat_preference", "pets_muted", "bots_muted", "ignore_invites", "allow_gifts", "friend_bar_state", "disable_forced_effects", "allow_mimic"],
        )
        .await?;
    let stw = ctx
        .writer(
            "user_statistics",
            &["id", "OnlineTime", "Respect", "RespectGiven", "DailyRespectPoints", "DailyPetRespectPoints", "AchievementScore", "groupid", "forum_posts"],
        )
        .await?;
    let iw = ctx.writer("user_info", &["user_id", "bans", "cautions", "cfhs", "cfhs_abusive"]).await?;
    let rw = ctx.writer("user_roles", &["user_id", "role_id"]).await?;

    let guilds: std::collections::HashSet<i64> = if ctx.has_src("guilds") {
        query_rows(&ctx.src, "SELECT id FROM guilds").await?.iter().map(|r| r.i64("id")).collect()
    } else {
        Default::default()
    };

    let mut rows = RowStream::open(
        &ctx.src,
        "SELECT u.*, s.home_room AS s_home_room, s.block_friendrequests, s.volume_system, s.volume_furni, \
                s.volume_trax, s.block_camera_follow, s.old_chat, s.ignore_pets, s.ignore_bots, s.block_roominvites, \
                s.online_time, s.respects_received, s.respects_given, s.daily_respect_points, \
                s.daily_pet_respect_points, s.achievement_score, s.guild_id, s.forums_post_count, s.cfh_bans, \
                s.cfh_warnings, s.cfh_send, s.cfh_abusive \
         FROM users u LEFT JOIN users_settings s ON s.user_id = u.id",
    )
    .await?;
    while let Some(r) = rows.next().await? {
        let id = r.i64("id");
        let rank = maps.real_ranks.get(&id).copied().unwrap_or(1);
        let role = maps.roles.get(&rank);
        let mut row = vec![
            i(id),
            s(r.text("username")),
            opt_s(password(&r.str("password"))),
            opt_s(r.opt_str("mail").filter(|m| !m.is_empty())),
            i(role.map(|r| r.security_level).unwrap_or(1)),
            i(r.i64("credits")),
            s(r.str("look")),
            s(if r.str("gender").eq_ignore_ascii_case("f") { "F" } else { "M" }),
            s(r.text("motto")),
            b(false),
            s(r.str("ip_current")),
            s(r.str("ip_register")),
            b(false),
            unix_dt(r.i64("account_created")),
            unix_dt(r.i64("last_online")),
        ];
        if has_last_login {
            row.push(i(r.i64("last_login").max(0)));
        }
        uw.sink.send(row).await?;

        let home = [r.i64("s_home_room"), r.i64("home_room")]
            .into_iter()
            .find(|h| *h > 0 && maps.rooms.contains(h))
            .unwrap_or(0);
        let vol = |c: &str| if r.is_null(c) { 100 } else { r.i64(c).clamp(0, 100) };
        sw.sink
            .send(vec![
                i(id),
                i(home),
                b(r.bool("block_friendrequests")),
                s(format!("{},{},{}", vol("volume_system"), vol("volume_furni"), vol("volume_trax"))),
                b(r.bool("block_camera_follow")),
                b(r.bool("old_chat")),
                b(r.bool("ignore_pets")),
                b(r.bool("ignore_bots")),
                b(r.bool("block_roominvites")),
                b(true),
                b(true),
                b(false),
                b(true),
            ])
            .await?;
        let guild = r.i64("guild_id");
        stw.sink
            .send(vec![
                i(id),
                i(r.i64("online_time").max(0)),
                i(r.i64("respects_received")),
                i(r.i64("respects_given")),
                i(if r.is_null("daily_respect_points") { 3 } else { r.i64("daily_respect_points") }),
                i(if r.is_null("daily_pet_respect_points") { 3 } else { r.i64("daily_pet_respect_points") }),
                i(r.i64("achievement_score")),
                i(if guilds.contains(&guild) { guild } else { 0 }),
                i(r.i64("forums_post_count")),
            ])
            .await?;
        iw.sink
            .send(vec![i(id), i(r.i64("cfh_bans")), i(r.i64("cfh_warnings")), i(r.i64("cfh_send")), i(r.i64("cfh_abusive"))])
            .await?;
        if let Some(role) = role {
            rw.sink.send(vec![i(id), i(role.id)]).await?;
        }
    }
    for w in [uw, sw, stw, iw, rw] {
        w.finish().await?;
    }

    currencies(&ctx).await?;
    badges(&ctx).await?;
    wardrobe(&ctx).await?;
    achievements(&ctx).await?;
    club(&ctx).await
}

/// Plus's CMS treats `$2a$`/`$2b$` as plaintext; the hash is the same with `$2y$`.
fn password(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    for p in ["$2a$", "$2b$", "$2x$"] {
        if let Some(rest) = raw.strip_prefix(p) {
            return Some(format!("$2y${rest}"));
        }
    }
    Some(raw.to_string())
}

const USER_JOIN: &str = "JOIN users u ON u.id = t.user_id";

async fn currencies(ctx: &Ctx) -> Result<()> {
    if !ctx.has_src("users_currency") {
        return Ok(());
    }
    let w = ctx.writer_ignore("user_currencies", &["user_id", "type", "amount"]).await?;
    let mut rows =
        RowStream::open(&ctx.src, &format!("SELECT t.* FROM users_currency t {USER_JOIN} WHERE t.type >= 0")).await?;
    while let Some(r) = rows.next().await? {
        w.sink.send(vec![i(r.i64("user_id")), i(r.i64("type")), i(r.i64("amount"))]).await?;
    }
    w.finish().await?;
    Ok(())
}

async fn badges(ctx: &Ctx) -> Result<()> {
    let w = ctx.writer_ignore("user_badges", &["user_id", "badge_id", "badge_slot"]).await?;
    let defs = ctx.writer_ignore("badge_definitions", &["code", "required_right"]).await?;
    let mut rows = RowStream::open(&ctx.src, &format!("SELECT t.* FROM users_badges t {USER_JOIN}")).await?;
    while let Some(r) = rows.next().await? {
        let code = r.str("badge_code");
        if code.is_empty() {
            continue;
        }
        defs.sink.send(vec![s(crate::text::clip(&code, 35)), s("")]).await?;
        w.sink.send(vec![i(r.i64("user_id")), s(code), i(r.i64("slot_id").clamp(0, 5))]).await?;
    }
    w.finish().await?;
    defs.finish().await?;

    if ctx.has_src("users_effects") {
        let w = ctx
            .writer("user_effects", &["user_id", "effect_id", "total_duration", "is_activated", "quantity", "activated_stamp"])
            .await?;
        let mut rows = RowStream::open(&ctx.src, &format!("SELECT t.* FROM users_effects t {USER_JOIN}")).await?;
        while let Some(r) = rows.next().await? {
            let activated = r.i64("activation_timestamp");
            w.sink
                .send(vec![
                    i(r.i64("user_id")),
                    i(r.i64("effect")),
                    i(r.i64("duration")),
                    b(activated > 0),
                    i(r.i64("total").max(1)),
                    unix_dt(activated),
                ])
                .await?;
        }
        w.finish().await?;
    }
    Ok(())
}

async fn wardrobe(ctx: &Ctx) -> Result<()> {
    if ctx.has_src("users_clothing") {
        let w = ctx.writer_ignore("user_clothing", &["user_id", "part_id", "part"]).await?;
        let mut rows = RowStream::open(&ctx.src, &format!("SELECT t.* FROM users_clothing t {USER_JOIN}")).await?;
        while let Some(r) = rows.next().await? {
            w.sink.send(vec![i(r.i64("user_id")), i(r.i64("clothing_id")), s("")]).await?;
        }
        w.finish().await?;
    }
    if ctx.has_src("users_wardrobe") {
        // Plus has no unique key here but assumes one row per (user, slot).
        let w = ctx.writer("user_wardrobe", &["user_id", "slot_id", "look", "gender"]).await?;
        let mut rows = RowStream::open(
            &ctx.src,
            &format!("SELECT t.user_id, t.slot_id, MAX(t.look) look, MAX(t.gender) gender FROM users_wardrobe t {USER_JOIN} GROUP BY t.user_id, t.slot_id"),
        )
        .await?;
        while let Some(r) = rows.next().await? {
            let gender = if r.str("gender").eq_ignore_ascii_case("f") { "F" } else { "M" };
            w.sink.send(vec![i(r.i64("user_id")), i(r.i64("slot_id")), s(r.str("look")), s(gender)]).await?;
        }
        w.finish().await?;
    }
    Ok(())
}

/// Arcturus keeps a running total per achievement; Plus keeps completed levels
/// plus progress towards the next, measured against Plus's own thresholds.
async fn achievements(ctx: &Ctx) -> Result<()> {
    if !ctx.has_src("users_achievements") {
        return Ok(());
    }
    let mut levels: HashMap<String, Vec<i64>> = HashMap::new();
    for r in query_rows(&ctx.dst, "SELECT group_name, progress_needed FROM achievements ORDER BY group_name, level").await? {
        levels.entry(r.str("group_name").to_lowercase()).or_default().push(r.i64("progress_needed"));
    }
    let w = ctx.writer_ignore("user_achievements", &["userid", "group", "level", "progress"]).await?;
    let mut rows = RowStream::open(
        &ctx.src,
        &format!("SELECT t.user_id, t.achievement_name, MAX(t.progress) progress FROM users_achievements t {USER_JOIN} GROUP BY t.user_id, t.achievement_name"),
    )
    .await?;
    let mut unknown = 0;
    while let Some(r) = rows.next().await? {
        let name = r.str("achievement_name");
        let group = if name.to_ascii_uppercase().starts_with("ACH_") { name } else { format!("ACH_{name}") };
        let Some(steps) = levels.get(&group.to_lowercase()) else {
            unknown += 1;
            continue;
        };
        let total = r.i64("progress");
        let level = steps.iter().filter(|need| total >= **need).count();
        let done = if level > 0 { steps[level - 1] } else { 0 };
        w.sink.send(vec![i(r.i64("user_id")), s(group), i(level as i64), i((total - done).max(0))]).await?;
    }
    w.finish().await?;
    if unknown > 0 {
        ctx.note(format!("{unknown} achievement progress rows are for achievements Plus does not have"));
    }
    Ok(())
}

/// HC history. Paydays already paid out by Arcturus are recorded so Plus does
/// not back-pay every month since the user's first subscription.
async fn club(ctx: &Ctx) -> Result<()> {
    if !ctx.has_src("users_subscriptions") {
        return Ok(());
    }
    let mut subs: HashMap<i64, Vec<(i64, i64, bool)>> = HashMap::new();
    for r in query_rows(
        &ctx.src,
        &format!("SELECT t.user_id, t.timestamp_start, t.duration, t.active FROM users_subscriptions t {USER_JOIN} WHERE UPPER(t.subscription_type) = 'HABBO_CLUB' ORDER BY t.timestamp_start"),
    )
    .await?
    {
        subs.entry(r.i64("user_id")).or_default().push((r.i64("timestamp_start"), r.i64("duration"), r.bool("active")));
    }
    let claimed: HashMap<i64, i64> = query_rows(&ctx.src, "SELECT user_id, hc_gifts_claimed FROM users_settings")
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| (r.i64("user_id"), r.i64("hc_gifts_claimed")))
        .collect();

    let mw = ctx
        .writer(
            "user_club_memberships",
            &["user_id", "expires_at", "started_at", "first_started_at", "past_seconds", "modified_at", "gifts_claimed"],
        )
        .await?;
    let iw = ctx.writer_ignore("club_membership_intervals", &["user_id", "started_at", "expires_at"]).await?;
    let pw = ctx.writer_ignore("club_paydays", &["user_id", "payday", "spent", "streak_bonus", "spending_bonus", "paid"]).await?;
    let now = chrono::Utc::now().timestamp();

    for (user, list) in subs {
        let first = list.first().map(|x| x.0).unwrap_or(now);
        let current = list.iter().filter(|x| x.2).max_by_key(|x| x.0 + x.1).copied();
        let expires = list.iter().map(|x| x.0 + x.1).max().unwrap_or(first);
        let started = current.map(|x| x.0).unwrap_or(first);
        let past: i64 = list.iter().filter(|x| Some(**x) != current).map(|x| x.1).sum();
        let elapsed = past + (expires.min(now) - started).max(0);
        let earned = (elapsed + 31 * 86_400 - 1) / (31 * 86_400);
        mw.sink
            .send(vec![
                i(user),
                unix_dt(expires),
                unix_dt(started),
                unix_dt(first),
                i(past),
                unix_dt(now),
                i(claimed.get(&user).copied().unwrap_or(0).clamp(0, earned)),
            ])
            .await?;
        for (start, dur, _) in &list {
            iw.sink.send(vec![i(user), unix_dt(*start), unix_dt(start + dur)]).await?;
        }
        for payday in month_starts(first, now) {
            pw.sink.send(vec![i(user), s(payday), i(0), i(0), i(0), b(true)]).await?;
        }
    }
    for w in [mw, iw, pw] {
        w.finish().await?;
    }
    Ok(())
}

/// First-of-month dates (UTC) after `from` up to `to`.
fn month_starts(from: i64, to: i64) -> Vec<String> {
    use chrono::{Datelike, NaiveDate};
    let (Some(a), Some(z)) = (chrono::DateTime::from_timestamp(from, 0), chrono::DateTime::from_timestamp(to, 0)) else {
        return vec![];
    };
    let mut out = vec![];
    let (mut y, mut m) = (a.year(), a.month());
    loop {
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
        let Some(d) = NaiveDate::from_ymd_opt(y, m, 1) else { break };
        if d > z.date_naive() {
            break;
        }
        out.push(format!("{d} 00:00:00"));
    }
    out
}

