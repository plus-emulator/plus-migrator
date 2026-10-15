//! guilds -> groups, memberships, requests and forums.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;

use crate::context::Ctx;
use crate::db::{RowStream, b, i, query_rows, s, unix_dt};

fn forum_level(v: &str) -> i64 {
    match v.to_ascii_uppercase().as_str() {
        "MEMBERS" => 1,
        "ADMINS" => 2,
        "OWNER" => 3,
        _ => 0,
    }
}

fn post_state(state: i64) -> i64 {
    if state >= 20 {
        20
    } else if state >= 10 {
        10
    } else {
        0
    }
}

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    if !ctx.has_src("guilds") {
        return Ok(());
    }
    let gw = ctx
        .writer(
            "groups",
            &[
                "id",
                "name",
                "desc",
                "badge",
                "owner_id",
                "created",
                "room_id",
                "state",
                "colour1",
                "colour2",
                "admindeco",
                "forum_enabled",
            ],
        )
        .await?;
    let fw = ctx
        .writer(
            "group_forums",
            &[
                "group_id",
                "read_permission",
                "post_permission",
                "thread_permission",
                "moderate_permission",
                "message_count",
            ],
        )
        .await?;
    let guilds = query_rows(
        &ctx.src,
        "SELECT t.* FROM guilds t JOIN users u ON u.id = t.user_id",
    )
    .await?;
    let mut forums = Vec::new();
    for r in &guilds {
        let id = r.i64("id");
        let state = match r.i64("state") {
            1 => "1",
            2 | 4 => "2",
            _ => "0",
        };
        gw.sink
            .send(vec![
                i(id),
                s(r.text("name")),
                s(r.text("description")),
                s(r.str("badge")),
                i(r.i64("user_id")),
                unix_dt(r.i64("date_created")),
                i(r.i64("room_id").max(0)),
                s(state),
                i(r.i64("color_one").max(1)),
                i(r.i64("color_two").max(1)),
                b(!r.bool("rights")),
                b(r.bool("forum")),
            ])
            .await?;
        forums.push((
            id,
            forum_level(&r.str("read_forum")),
            forum_level(&r.str("post_messages")),
            forum_level(&r.str("post_threads")),
            forum_level(&r.str("mod_forum")).max(2),
        ));
    }
    gw.finish().await?;

    let mw = ctx
        .writer_ignore("group_memberships", &["group_id", "user_id", "rank"])
        .await?;
    let rw = ctx
        .writer_ignore("group_requests", &["group_id", "user_id"])
        .await?;
    let mut rows = RowStream::open(
        &ctx.src,
        "SELECT t.* FROM guilds_members t JOIN guilds g ON g.id = t.guild_id JOIN users u ON u.id = t.user_id JOIN users o ON o.id = g.user_id",
    )
    .await?;
    while let Some(r) = rows.next().await? {
        let (g, u) = (r.i64("guild_id"), r.i64("user_id"));
        match r.i64("level_id") {
            0 | 1 => mw.sink.send(vec![i(g), i(u), i(1)]).await?,
            2 => mw.sink.send(vec![i(g), i(u), i(0)]).await?,
            3 => rw.sink.send(vec![i(g), i(u)]).await?,
            _ => {}
        }
    }
    mw.finish().await?;
    rw.finish().await?;

    // Forums: Plus numbers messages per group from 1 and per thread from 0.
    let mut group_count: HashMap<i64, i64> = HashMap::new();
    if ctx.has_src("guilds_forums_threads") && ctx.has_src("guilds_forums_comments") {
        let threads = query_rows(
            &ctx.src,
            "SELECT t.* FROM guilds_forums_threads t JOIN guilds g ON g.id = t.guild_id JOIN users u ON u.id = t.opener_id",
        )
        .await?;
        let thread_group: HashMap<i64, i64> = threads
            .iter()
            .map(|t| (t.i64("id"), t.i64("guild_id")))
            .collect();
        let mut thread_stats: HashMap<i64, (i64, i64)> = HashMap::new();
        let w = ctx
            .writer(
                "group_forum_messages",
                &[
                    "group_id",
                    "id",
                    "thread_id",
                    "message_index",
                    "author_id",
                    "body",
                    "created_at",
                    "state",
                    "moderator_id",
                ],
            )
            .await?;
        let mut rows = RowStream::open(
            &ctx.src,
            "SELECT c.* FROM guilds_forums_comments c JOIN users u ON u.id = c.user_id ORDER BY c.created_at, c.id",
        )
        .await?;
        while let Some(r) = rows.next().await? {
            let thread = r.i64("thread_id");
            let Some(&group) = thread_group.get(&thread) else {
                continue;
            };
            let id = group_count.entry(group).or_default();
            *id += 1;
            let stats = thread_stats.entry(thread).or_default();
            let created = unix_dt(r.i64("created_at").max(1));
            w.sink
                .send(vec![
                    i(group),
                    i(*id),
                    i(thread),
                    i(stats.0),
                    i(r.i64("user_id")),
                    s(r.text("message")),
                    created,
                    i(post_state(r.i64("state"))),
                    i(r.i64("admin_id")),
                ])
                .await?;
            stats.0 += 1;
            stats.1 = *id;
        }
        w.finish().await?;

        let tw = ctx
            .writer(
                "group_forum_threads",
                &[
                    "id",
                    "group_id",
                    "author_id",
                    "title",
                    "pinned",
                    "locked",
                    "created_at",
                    "updated_at",
                    "message_count",
                    "last_message_id",
                    "state",
                    "moderator_id",
                ],
            )
            .await?;
        for t in &threads {
            let (count, last) = thread_stats.get(&t.i64("id")).copied().unwrap_or((0, 0));
            let state = t.i64("state");
            tw.sink
                .send(vec![
                    i(t.i64("id")),
                    i(t.i64("guild_id")),
                    i(t.i64("opener_id")),
                    s(t.text("subject")),
                    b(t.bool("pinned")),
                    b(t.bool("locked") || state == 1),
                    unix_dt(t.i64("created_at").max(1)),
                    unix_dt(t.i64("updated_at").max(t.i64("created_at")).max(1)),
                    i(count),
                    i(last),
                    i(post_state(state)),
                    i(t.i64("admin_id")),
                ])
                .await?;
        }
        tw.finish().await?;
    }
    for (g, read, post, thread, moderate) in forums {
        fw.sink
            .send(vec![
                i(g),
                i(read),
                i(post),
                i(thread),
                i(moderate),
                i(group_count.get(&g).copied().unwrap_or(0)),
            ])
            .await?;
    }
    fw.finish().await?;
    Ok(())
}
