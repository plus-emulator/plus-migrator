//! Room models, rooms and room satellites.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Result;

use crate::context::Ctx;
use crate::db::{RowStream, b, f, i, query_rows, s, unix_dt};

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    let maps = ctx.maps();

    let mw = ctx
        .writer(
            "room_models",
            &[
                "id",
                "door_x",
                "door_y",
                "door_z",
                "door_dir",
                "heightmap",
                "public_items",
                "required_club_level",
                "custom",
                "wall_height",
            ],
        )
        .await?;
    for m in &maps.new_models {
        mw.sink
            .send(vec![
                s(m.id.clone()),
                i(m.door_x),
                i(m.door_y),
                f(m.door_z),
                i(m.door_dir),
                s(m.heightmap.clone()),
                s(m.public_items.clone()),
                i(m.required_club_level),
                b(true),
                i(m.wall_height),
            ])
            .await?;
    }
    mw.finish().await?;

    let guilds: HashSet<i64> = if ctx.has_src("guilds") {
        query_rows(&ctx.src, "SELECT id FROM guilds")
            .await?
            .iter()
            .map(|r| r.i64("id"))
            .collect()
    } else {
        HashSet::new()
    };
    let w = ctx
        .writer(
            "rooms",
            &[
                "id",
                "roomtype",
                "caption",
                "owner",
                "description",
                "category",
                "state",
                "users_now",
                "users_max",
                "model_name",
                "score",
                "tags",
                "password",
                "wallpaper",
                "floor",
                "landscape",
                "wallthick",
                "floorthick",
                "group_id",
                "mute_settings",
                "ban_settings",
                "kick_settings",
                "chat_mode",
                "chat_size",
                "chat_speed",
                "chat_extra_flood",
                "chat_hearing_distance",
                "trade_settings",
                "allow_pets",
                "allow_pets_eat",
                "room_blocking_disabled",
                "allow_hidewall",
                "hide_wired",
            ],
        )
        .await?;
    let mut missing_model = 0;
    let mut rows = RowStream::open(
        &ctx.src,
        "SELECT t.* FROM rooms t JOIN users u ON u.id = t.owner_id",
    )
    .await?;
    while let Some(r) = rows.next().await? {
        let id = r.i64("id");
        let Some(model) = maps.room_models.get(&id) else {
            missing_model += 1;
            continue;
        };
        let guild = r.i64("guild_id");
        let tags: Vec<String> = r
            .text("tags")
            .split([';', ','])
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .take(2)
            .collect();
        let state = match r.str("state").as_str() {
            "locked" | "password" | "invisible" => r.str("state"),
            _ => "open".into(),
        };
        w.sink
            .send(vec![
                i(id),
                s(if r.bool("is_public") {
                    "public"
                } else {
                    "private"
                }),
                s(r.text("name")),
                i(r.i64("owner_id")),
                s(r.text("description")),
                i(maps
                    .categories
                    .get(&r.i64("category"))
                    .copied()
                    .unwrap_or(maps.category_fallback)),
                s(state),
                i(0),
                i(r.i64("users_max").max(1)),
                s(model.clone()),
                i(r.i64("score")),
                s(tags.join(",")),
                s(r.str("password")),
                s(r.str("paper_wall")),
                s(r.str("paper_floor")),
                s(r.str("paper_landscape")),
                i(r.i64("thickness_wall").clamp(-2, 1)),
                i(r.i64("thickness_floor").clamp(-2, 1)),
                i(if guilds.contains(&guild) { guild } else { 0 }),
                b(r.i64("who_can_mute") != 0),
                b(r.i64("who_can_ban") != 0),
                s(r.i64("who_can_kick").clamp(0, 2).to_string()),
                i(r.i64("chat_mode").clamp(0, 1)),
                i(r.i64("chat_weight").clamp(0, 2)),
                i(r.i64("chat_speed").clamp(0, 2)),
                i(r.i64("chat_protection").clamp(0, 2)),
                i(r.i64("chat_hearing_distance").clamp(0, 99)),
                i(r.i64("trade_mode").clamp(0, 2)),
                b(r.bool("allow_other_pets")),
                b(r.bool("allow_other_pets_eat")),
                b(r.bool("allow_walkthrough")),
                b(r.bool("allow_hidewall")),
                b(r.bool("hidewired")),
            ])
            .await?;
    }
    w.finish().await?;
    if missing_model > 0 {
        ctx.note(format!(
            "{missing_model} rooms use a room model that exists in neither hotel and were left out"
        ));
    }

    satellites(&ctx).await
}

async fn satellites(ctx: &Ctx) -> Result<()> {
    let both = "JOIN rooms r ON r.id = t.room_id JOIN users u ON u.id = t.user_id";
    let now = chrono::Utc::now().timestamp();

    if ctx.has_src("room_rights") {
        let w = ctx
            .writer_ignore("room_rights", &["room_id", "user_id"])
            .await?;
        let mut rows =
            RowStream::open(&ctx.src, &format!("SELECT t.* FROM room_rights t {both}")).await?;
        while let Some(r) = rows.next().await? {
            w.sink
                .send(vec![i(r.i64("room_id")), i(r.i64("user_id"))])
                .await?;
        }
        w.finish().await?;
    }
    if ctx.has_src("room_bans") {
        let w = ctx
            .writer_ignore("room_bans", &["user_id", "room_id", "expire"])
            .await?;
        let mut rows =
            RowStream::open(&ctx.src, &format!("SELECT t.* FROM room_bans t {both}")).await?;
        while let Some(r) = rows.next().await? {
            let ends = r.i64("ends");
            if ends > 0 && ends < now {
                continue;
            }
            let expire = if ends <= 0 {
                s("2099-12-31 00:00:00")
            } else {
                unix_dt(ends)
            };
            w.sink
                .send(vec![i(r.i64("user_id")), i(r.i64("room_id")), expire])
                .await?;
        }
        w.finish().await?;
    }
    if ctx.has_src("room_promotions") {
        let w = ctx
            .writer_ignore(
                "room_promotions",
                &[
                    "room_id",
                    "title",
                    "description",
                    "timestamp_start",
                    "timestamp_expire",
                    "category_id",
                ],
            )
            .await?;
        let mut rows = RowStream::open(
            &ctx.src,
            "SELECT t.* FROM room_promotions t JOIN rooms r ON r.id = t.room_id",
        )
        .await?;
        while let Some(r) = rows.next().await? {
            if r.i64("end_timestamp") < now {
                continue;
            }
            w.sink
                .send(vec![
                    i(r.i64("room_id")),
                    s(r.text("title")),
                    s(r.text("description")),
                    unix_dt(r.i64("start_timestamp")),
                    unix_dt(r.i64("end_timestamp")),
                    i(r.i64("category")),
                ])
                .await?;
        }
        w.finish().await?;
    }
    if ctx.has_src("room_wordfilter") {
        let w = ctx
            .writer_ignore("room_filter", &["room_id", "word"])
            .await?;
        let mut rows = RowStream::open(
            &ctx.src,
            "SELECT t.* FROM room_wordfilter t JOIN rooms r ON r.id = t.room_id",
        )
        .await?;
        while let Some(r) = rows.next().await? {
            w.sink
                .send(vec![i(r.i64("room_id")), s(r.text("word").to_string())])
                .await?;
        }
        w.finish().await?;
    }

    // Public rooms: Arcturus lists them in navigator_publics, or flags is_public.
    let sql = if ctx.has_src("navigator_publics") {
        "SELECT r.id, r.name, r.description, p.visible FROM navigator_publics p JOIN rooms r ON r.id = p.room_id"
    } else {
        "SELECT r.id, r.name, r.description, 1 AS visible FROM rooms r WHERE r.is_public = '1'"
    };
    let w = ctx
        .writer_ignore(
            "navigator_publics",
            &[
                "room_id",
                "caption",
                "description",
                "image_url",
                "order_num",
                "enabled",
            ],
        )
        .await?;
    let mut rows = RowStream::open(&ctx.src, sql).await?;
    let mut order = 0;
    while let Some(r) = rows.next().await? {
        order += 1;
        w.sink
            .send(vec![
                i(r.i64("id")),
                s(r.text("name")),
                s(r.text("description")),
                s(""),
                i(order),
                b(r.bool("visible")),
            ])
            .await?;
    }
    w.finish().await?;
    Ok(())
}
