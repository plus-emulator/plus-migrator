//! bots + users_pets -> Plus's single bots table (pets get bots_petdata).

use std::sync::Arc;

use anyhow::Result;

use crate::context::Ctx;
use crate::db::{RowStream, b, i, s, unix_dt};

const BOT_COLUMNS: &[&str] = &[
    "id", "room_id", "user_id", "ai_type", "name", "motto", "look", "x", "y", "z", "rotation", "walk_mode", "gender",
    "automatic_chat", "speaking_interval", "mix_sentences", "chat_bubble",
];

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    let maps = ctx.maps();
    let room = |id: i64| if maps.rooms.contains(&id) { id } else { 0 };
    let w = ctx.writer("bots", BOT_COLUMNS).await?;
    let speech = ctx.writer("bots_speech", &["bot_id", "text"]).await?;

    if ctx.has_src("bots") {
        let mut rows = RowStream::open(&ctx.src, "SELECT t.* FROM bots t JOIN users u ON u.id = t.user_id").await?;
        while let Some(r) = rows.next().await? {
            let id = r.i64("id");
            let ai = if r.str("type").eq_ignore_ascii_case("bartender") { "bartender" } else { "generic" };
            w.sink
                .send(vec![
                    i(id),
                    i(room(r.i64("room_id"))),
                    i(r.i64("user_id")),
                    s(ai),
                    s(r.text("name")),
                    s(r.text("motto")),
                    s(r.str("figure")),
                    i(r.i64("x")),
                    i(r.i64("y")),
                    i(r.f64("z").round() as i64),
                    i(r.i64("rot")),
                    s(if r.bool("freeroam") { "freeroam" } else { "stand" }),
                    s(if r.str("gender").eq_ignore_ascii_case("f") { "F" } else { "M" }),
                    s(if r.bool("chat_auto") { "true" } else { "false" }),
                    i(r.i64("chat_delay").max(1)),
                    b(r.bool("chat_random")),
                    i(if r.is_null("bubble_id") { 2 } else { r.i64("bubble_id") }),
                ])
                .await?;
            for line in r.text("chat_lines").split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty()) {
                speech.sink.send(vec![i(id), s(crate::text::clip(line, 200))]).await?;
            }
        }
    }
    speech.finish().await?;

    if ctx.has_src("users_pets") {
        let pd = ctx
            .writer(
                "bots_petdata",
                &["id", "type", "race", "color", "energy", "experience", "nutrition", "respect", "createstamp", "have_saddle", "hairdye", "pethair", "anyone_ride", "gnome_clothing"],
            )
            .await?;
        let mut rows = RowStream::open(&ctx.src, "SELECT t.* FROM users_pets t JOIN users u ON u.id = t.user_id").await?;
        while let Some(r) = rows.next().await? {
            let id = r.i64("id") + maps.pet_id_offset;
            w.sink
                .send(vec![
                    i(id),
                    i(room(r.i64("room_id"))),
                    i(r.i64("user_id")),
                    s("pet"),
                    s(r.text("name")),
                    s(""),
                    s(""),
                    i(r.i64("x")),
                    i(r.i64("y")),
                    i(r.f64("z").round() as i64),
                    i(r.i64("rot")),
                    s("freeroam"),
                    s("M"),
                    s("false"),
                    i(30),
                    b(false),
                    i(2),
                ])
                .await?;
            let saddle = if !r.bool("saddle") {
                0
            } else if maps.furni(r.i64("saddle_item_id")).is_some_and(|d| d.item_name.contains("saddle2")) {
                10
            } else {
                9
            };
            let hunger = r.i64("hunger").clamp(0, 100);
            let gnome = r.str("gnome_data");
            pd.sink
                .send(vec![
                    i(id),
                    i(r.i64("type").max(0)),
                    s(r.i64("race").to_string()),
                    s(r.str("color")),
                    i(r.i64("energy").clamp(0, 100)),
                    i(r.i64("experience").max(0)),
                    i(150 - hunger * 3 / 2),
                    i(r.i64("respect")),
                    unix_dt(r.i64("created")),
                    i(saddle),
                    i(r.i64("hair_color")),
                    i(r.i64("hair_style")),
                    b(r.bool("ride")),
                    s(if gnome.is_empty() { "-1".to_string() } else { gnome }),
                ])
                .await?;
        }
        pd.finish().await?;
    }
    w.finish().await?;
    Ok(())
}
