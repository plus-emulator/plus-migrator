//! items (owned and placed furniture) and their side tables.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use roaring::RoaringTreemap;

use crate::context::Ctx;
use crate::db::{MultiWriter, RowStream, b, f, i, query_rows, s};
use crate::steps::prep::FurniDef;

const SEP: char = '\u{5}';
const MOOD_COLOURS: &[&str] = &[
    "#000000", "#0053F7", "#EA4532", "#82F349", "#74F5F5", "#E759DE", "#F2F851",
];

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    let maps = ctx.maps();

    // Unopened gift contents are items owned by user 0.
    let mut consumed_gift_contents = HashSet::new();
    let mut gift_contents: HashMap<i64, (i64, String)> = HashMap::new();
    for r in query_rows(
        &ctx.src,
        "SELECT id, item_id, extra_data FROM items WHERE user_id = 0",
    )
    .await?
    {
        gift_contents.insert(r.i64("id"), (r.i64("item_id"), r.text("extra_data")));
    }
    // Open marketplace listings (user_id -1) go back to their seller.
    let mut listed: HashMap<i64, i64> = HashMap::new();
    if ctx.has_src("marketplace_items") {
        for r in query_rows(
            &ctx.src,
            "SELECT item_id, user_id FROM marketplace_items WHERE state IN (1, 3)",
        )
        .await?
        {
            listed.insert(r.i64("item_id"), r.i64("user_id"));
        }
    }
    let crack_targets: HashMap<i64, i64> = if ctx.has_src("items_crackable") {
        query_rows(&ctx.src, "SELECT item_id, count FROM items_crackable")
            .await?
            .iter()
            .map(|r| (r.i64("item_id"), r.i64("count")))
            .collect()
    } else {
        HashMap::new()
    };
    let guilds: std::collections::HashSet<i64> = if ctx.has_src("guilds") {
        query_rows(&ctx.src, "SELECT id FROM guilds")
            .await?
            .iter()
            .map(|r| r.i64("id"))
            .collect()
    } else {
        Default::default()
    };

    let iw = MultiWriter::open(
        &ctx.dst,
        &ctx.dst_schema,
        "items",
        &[
            "id",
            "user_id",
            "room_id",
            "base_item",
            "extra_data",
            "x",
            "y",
            "z",
            "rot",
            "wall_pos",
            "limited_number",
            "limited_stack",
        ],
        6,
        ctx.validate_only,
    )
    .await?;
    let pw = ctx
        .writer("user_presents", &["item_id", "base_id", "extra_data"])
        .await?;
    let gw = ctx.writer("items_groups", &["id", "group_id"]).await?;
    let tw = ctx
        .writer(
            "room_items_toner",
            &["id", "enabled", "data1", "data2", "data3"],
        )
        .await?;

    let mut written = RoaringTreemap::new();
    let mut dimmers: Vec<(i64, i64, String)> = Vec::new();
    let mut discs_in_jukebox: HashMap<i64, i64> = HashMap::new();
    let (mut no_owner, mut no_furni, mut empty_gifts) = (0u64, 0u64, 0u64);

    let mut rows = RowStream::open(&ctx.src, "SELECT * FROM items WHERE user_id <> 0").await?;
    while let Some(r) = rows.next().await? {
        let id = r.i64("id");
        let mut owner = r.i64("user_id");
        if owner < 0 {
            match listed.get(&id) {
                Some(seller) => owner = *seller,
                None => continue, // sold listing: the buyer already has a copy
            }
        }
        if !maps.user_names.contains_key(&owner) {
            no_owner += 1;
            continue;
        }
        let Some(def) = maps
            .furni(r.i64("item_id"))
            .filter(|d| matches!(d.kind.as_str(), "s" | "i"))
        else {
            no_furni += 1;
            continue;
        };
        let src_room = r.i64("room_id");
        let room = if src_room > 0
            && maps.room_models.contains_key(&src_room)
            && maps.rooms.contains(&src_room)
        {
            src_room
        } else {
            0
        };
        if src_room == -1 {
            discs_in_jukebox.insert(id, owner);
        }
        let extra = r.text("extra_data");

        // Gifts: Plus wraps exactly one furni per box, so each content becomes
        // its own box; the first keeps the original item id.
        if def.interaction == "gift" {
            let gift = parse_gift(&extra);
            let contents: Vec<(i64, String)> = gift
                .ids
                .iter()
                .filter_map(|cid| gift_contents.get(cid))
                .filter_map(|(base, ex)| maps.furni_id(*base).map(|dst| (dst as i64, ex.clone())))
                .collect();
            if contents.len() != gift.ids.len() {
                ctx.note(format!(
                    "Gift {id}: one or more content items cannot be translated"
                ));
            }
            for cid in &gift.ids {
                if gift_contents
                    .get(cid)
                    .is_some_and(|(base, _)| maps.furni_id(*base).is_some())
                    && !consumed_gift_contents.insert(*cid)
                {
                    ctx.note(format!(
                        "Gift content {cid} is referenced by multiple wrappers"
                    ));
                }
            }
            if contents.is_empty() {
                empty_gifts += 1;
                continue;
            }
            let sender = gift
                .sender
                .as_deref()
                .and_then(|n| maps.user_ids.get(&n.to_lowercase()))
                .copied()
                .unwrap_or(owner);
            let owner_name = maps.user_names.get(&owner).cloned().unwrap_or_default();
            for (n, (content, content_extra)) in contents.into_iter().enumerate() {
                let item_id = if n == 0 { id } else { maps.alloc_item_id() };
                let data = [
                    owner_name.clone(),
                    gift.message.clone(),
                    sender.to_string(),
                    content.to_string(),
                    def.sprite_id.to_string(),
                    gift.colour.to_string(),
                    gift.ribbon.to_string(),
                ]
                .join(&SEP.to_string());
                let (item_room, x, y, z, rot) = if n == 0 {
                    (room, r.i64("x"), r.i64("y"), r.f64("z"), r.i64("rot"))
                } else {
                    (0, 0, 0, 0.0, 0)
                };
                send_item(
                    &iw,
                    item_id,
                    owner,
                    item_room,
                    def,
                    data,
                    x,
                    y,
                    z,
                    rot,
                    String::new(),
                    0,
                    0,
                )
                .await?;
                pw.sink
                    .send(vec![i(item_id), i(content), s(content_extra)])
                    .await?;
                written.insert(item_id as u64);
            }
            continue;
        }

        let extra = match def.interaction.as_str() {
            "dimmer" => {
                if room > 0 {
                    dimmers.push((id, room, extra.clone()));
                }
                extra
            }
            "roombg" => {
                let p: Vec<i64> = extra
                    .split(':')
                    .map(|v| v.trim().parse().unwrap_or(0))
                    .collect();
                if p.len() == 4 {
                    tw.sink
                        .send(vec![i(id), b(p[0] != 0), i(p[1]), i(p[2]), i(p[3])])
                        .await?;
                }
                String::new()
            }
            "mannequin" => {
                let mut p = extra.splitn(3, ':');
                let gender = if p.next().unwrap_or("m").trim().eq_ignore_ascii_case("f") {
                    "f"
                } else {
                    "m"
                };
                let figure = p.next().unwrap_or("").trim().to_string();
                let name = p.next().unwrap_or("").to_string();
                format!("{gender}{SEP}{figure}{SEP}{name}")
            }
            "lovelock" => lovelock(&extra),
            "crackable_egg" => {
                let target = crack_targets
                    .get(&r.i64("item_id"))
                    .copied()
                    .unwrap_or(def.modes)
                    .max(1);
                format!(
                    "\n{}\n{target}",
                    extra.trim().parse::<i64>().unwrap_or(0).clamp(0, target)
                )
            }
            "background" => ads_background(&extra),
            "fbgate" => match extra.split(';').next().filter(|f| !f.is_empty()) {
                Some(figure) => format!("M:{figure}"),
                None => String::new(),
            },
            "camera_picture" => camera_picture(
                &extra,
                maps.user_names
                    .get(&owner)
                    .map(String::as_str)
                    .unwrap_or(""),
            ),
            "scoreboard" | "freezetimer" | "banzaicounter" | "counter" | "red_score"
            | "blue_score" | "green_score" | "yellow_score" => first_number(&extra),
            "gld_gate" | "teleport" if extra.trim().is_empty() => "0".into(),
            _ => extra,
        };
        let (stack, serial) = limited(&r.str("limited_data"));
        send_item(
            &iw,
            id,
            owner,
            room,
            def,
            extra,
            r.i64("x"),
            r.i64("y"),
            r.f64("z"),
            r.i64("rot"),
            r.str("wall_pos"),
            serial,
            stack,
        )
        .await?;
        written.insert(id as u64);

        let guild = r.i64("guild_id");
        if guild > 0
            && guilds.contains(&guild)
            && matches!(
                def.interaction.as_str(),
                "gld_item" | "gld_gate" | "guild_forum"
            )
        {
            gw.sink.send(vec![i(id), i(guild)]).await?;
        }
    }
    iw.finish().await?;
    for w in [pw, gw, tw] {
        w.finish().await?;
    }
    if no_owner + no_furni + empty_gifts > 0 {
        ctx.note(format!(
            "items left out: {no_owner} whose owner no longer exists, {no_furni} whose furni definition does not exist, {empty_gifts} empty gifts"
        ));
    }

    let unwrapped = gift_contents
        .len()
        .saturating_sub(consumed_gift_contents.len());
    if unwrapped > 0 {
        ctx.note(format!("{unwrapped} owner-zero items have no migrated gift wrapper; retained only in recovery archive"));
    }
    teleports(&ctx, &written).await?;
    moodlights(&ctx, &dimmers).await?;
    music(&ctx, &written, &discs_in_jukebox).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn send_item(
    sink: &MultiWriter,
    id: i64,
    owner: i64,
    room: i64,
    def: &FurniDef,
    extra: String,
    x: i64,
    y: i64,
    z: f64,
    rot: i64,
    wall_pos: String,
    serial: i64,
    stack: i64,
) -> Result<()> {
    let wall = if def.kind == "i" {
        wall_pos
    } else {
        String::new()
    };
    sink.send(vec![
        i(id),
        i(owner),
        i(room),
        i(def.dst_id as i64),
        s(extra),
        i(x),
        i(y),
        f(z),
        i(rot),
        s(wall),
        i(serial),
        i(stack),
    ])
    .await
}

/// Arcturus `limited_data` is "stack:serial".
fn limited(raw: &str) -> (i64, i64) {
    let mut p = raw
        .split(':')
        .map(|v| v.trim().parse::<i64>().unwrap_or(0).max(0));
    let (stack, serial) = (p.next().unwrap_or(0), p.next().unwrap_or(0));
    if stack == 0 { (0, 0) } else { (stack, serial) }
}

struct Gift {
    ids: Vec<i64>,
    colour: i64,
    ribbon: i64,
    message: String,
    sender: Option<String>,
}

/// count \t id1..idN \t colour \t ribbon \t showSender \t message [\t sender \t look]
fn parse_gift(raw: &str) -> Gift {
    let p: Vec<&str> = raw.split('\t').collect();
    let count = p
        .first()
        .and_then(|c| c.trim().parse::<usize>().ok())
        .unwrap_or(0)
        .min(p.len().saturating_sub(1));
    let ids = p[1..=count]
        .iter()
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let at = |n: usize| p.get(count + n).map(|v| v.to_string()).unwrap_or_default();
    let show_sender = at(3).trim() == "1";
    Gift {
        ids,
        colour: at(1).trim().parse().unwrap_or(0),
        ribbon: at(2).trim().parse().unwrap_or(0),
        message: at(4),
        sender: show_sender.then(|| at(5)).filter(|n| !n.is_empty()),
    }
}

/// "1\tname1\tname2\tlook1\tlook2\tD-M-YYYY" -> Plus's char-5 form with dd/MM/yyyy.
fn lovelock(raw: &str) -> String {
    let p: Vec<&str> = raw.split('\t').collect();
    if p.len() < 6 || p[0] != "1" {
        return String::new();
    }
    let d: Vec<&str> = p[5].split(['-', '/']).collect();
    let date = if d.len() == 3 {
        format!("{:0>2}/{:0>2}/{}", d[0], d[1], d[2])
    } else {
        p[5].to_string()
    };
    [p[0], p[1], p[2], p[3], p[4], &date].join(&SEP.to_string())
}

/// "key=value;key=value" -> "key\tvalue\n...". Backslashes break Plus's parser.
fn ads_background(raw: &str) -> String {
    raw.split(';')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| {
            format!(
                "{}\t{}",
                k.trim().replace('\\', ""),
                v.trim().replace('\\', "")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Arcturus {"t","u","s","w"} -> Plus {"w","t","o","oi","s","n","m"}.
fn camera_picture(raw: &str, owner: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_string();
    };
    let num = |k: &str| {
        v.get(k)
            .and_then(|x| {
                x.as_i64()
                    .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
            })
            .unwrap_or(0)
    };
    serde_json::json!({
        "w": v.get("w").and_then(|x| x.as_str()).unwrap_or(""),
        "t": num("t"),
        "o": owner,
        "oi": num("u"),
        "s": num("s"),
        "n": "",
        "m": "",
    })
    .to_string()
}

fn first_number(raw: &str) -> String {
    raw.split(['\t', ',', ':'])
        .filter_map(|p| p.trim().parse::<i64>().ok())
        .next_back()
        .map(|n| n.max(0).to_string())
        .unwrap_or_else(|| "0".into())
}

async fn teleports(ctx: &Ctx, written: &RoaringTreemap) -> Result<()> {
    if !ctx.has_src("items_teleports") {
        return Ok(());
    }
    let w = ctx
        .writer_ignore("room_items_tele_links", &["tele_one_id", "tele_two_id"])
        .await?;
    let mut rows = RowStream::open(
        &ctx.src,
        "SELECT teleport_one_id a, teleport_two_id b FROM items_teleports",
    )
    .await?;
    while let Some(r) = rows.next().await? {
        let (a, b) = (r.i64("a"), r.i64("b"));
        if a > 0 && b > 0 && written.contains(a as u64) && written.contains(b as u64) {
            w.sink.send(vec![i(a), i(b)]).await?;
            w.sink.send(vec![i(b), i(a)]).await?;
        }
    }
    w.finish().await?;
    Ok(())
}

/// Arcturus keeps a room's three moodlight presets on `rooms.moodlight_data`
/// ("E,P,B,#colour,I;" x3); Plus keeps them per dimmer item.
async fn moodlights(ctx: &Ctx, dimmers: &[(i64, i64, String)]) -> Result<()> {
    if dimmers.is_empty() {
        return Ok(());
    }
    let presets: HashMap<i64, String> =
        query_rows(&ctx.src, "SELECT id, moodlight_data FROM rooms")
            .await?
            .into_iter()
            .map(|r| (r.i64("id"), r.str("moodlight_data")))
            .collect();
    let w = ctx
        .writer(
            "room_items_moodlight",
            &[
                "item_id",
                "enabled",
                "current_preset",
                "preset_one",
                "preset_two",
                "preset_three",
            ],
        )
        .await?;
    for (item, room, active) in dimmers {
        let mut out = vec!["#000000,255,0".to_string(); 3];
        for (n, entry) in presets
            .get(room)
            .map(String::as_str)
            .unwrap_or("")
            .split(';')
            .filter(|e| !e.is_empty())
            .take(3)
            .enumerate()
        {
            out[n] = mood_preset(entry);
        }
        let a: Vec<&str> = active.split(',').collect();
        let enabled = a.first().is_some_and(|v| v.trim() == "2");
        let current = a
            .get(1)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(1)
            .clamp(1, 3);
        w.sink
            .send(vec![
                i(*item),
                b(enabled),
                i(current),
                s(out[0].clone()),
                s(out[1].clone()),
                s(out[2].clone()),
            ])
            .await?;
    }
    w.finish().await?;
    Ok(())
}

/// "E,P,B,#RRGGBB,I" -> "#RRGGBB,I,bgOnly".
fn mood_preset(entry: &str) -> String {
    let p: Vec<&str> = entry.split(',').map(str::trim).collect();
    if p.len() < 5 {
        return "#000000,255,0".into();
    }
    let colour = MOOD_COLOURS
        .iter()
        .find(|c| c.eq_ignore_ascii_case(p[3]))
        .copied()
        .unwrap_or("#000000");
    let intensity = p[4].parse::<i64>().unwrap_or(255).clamp(0, 255);
    format!("{colour},{intensity},{}", if p[2] == "2" { 1 } else { 0 })
}

/// soundtracks -> room_music_songs; room_trax/trax_playlist -> players and playlists.
async fn music(ctx: &Ctx, written: &RoaringTreemap, discs: &HashMap<i64, i64>) -> Result<()> {
    if ctx.has_src("soundtracks") {
        let w = ctx
            .writer_ignore(
                "room_music_songs",
                &["id", "code", "name", "creator", "trax_data", "length_ms"],
            )
            .await?;
        let mut rows = RowStream::open(&ctx.src, "SELECT * FROM soundtracks WHERE id > 0").await?;
        while let Some(r) = rows.next().await? {
            let track = r.str("track");
            if track.trim().is_empty() {
                continue;
            }
            w.sink
                .send(vec![
                    i(r.i64("id")),
                    s(r.str("code")),
                    s(r.text("name")),
                    s(r.text("author")),
                    s(track),
                    i((r.i64("length").max(1) * 1000).min(i32::MAX as i64 / 20)),
                ])
                .await?;
        }
        w.finish().await?;
    }
    if !(ctx.has_src("room_trax") && ctx.has_src("trax_playlist")) || discs.is_empty() {
        return Ok(());
    }
    let disc_song: HashMap<i64, i64> = query_rows(
        &ctx.src,
        "SELECT id, extra_data FROM items WHERE room_id = -1",
    )
    .await?
    .iter()
    .map(|r| {
        (
            r.i64("id"),
            r.str("extra_data")
                .split('\n')
                .nth(6)
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0),
        )
    })
    .collect();
    let players = ctx
        .writer_ignore("room_music_players", &["item_id", "start_index", "version"])
        .await?;
    let list = ctx
        .writer_ignore(
            "room_music_playlist",
            &["player_id", "disc_id", "position", "song_id"],
        )
        .await?;
    let mut positions: HashMap<i64, i64> = HashMap::new();
    let mut rows = RowStream::open(&ctx.src, "SELECT p.trax_item_id, p.item_id FROM trax_playlist p JOIN room_trax t ON t.trax_item_id = p.trax_item_id").await?;
    while let Some(r) = rows.next().await? {
        let (player, disc) = (r.i64("trax_item_id"), r.i64("item_id"));
        let song = disc_song.get(&disc).copied().unwrap_or(0);
        if song <= 0
            || !written.contains(player as u64)
            || !written.contains(disc as u64)
            || !discs.contains_key(&disc)
        {
            continue;
        }
        let pos = positions.entry(player).or_insert(-1);
        if *pos == -1 {
            players.sink.send(vec![i(player), i(0), i(0)]).await?;
        }
        if *pos >= 19 {
            continue;
        }
        *pos += 1;
        list.sink
            .send(vec![i(player), i(disc), i(*pos), i(song)])
            .await?;
    }
    players.finish().await?;
    list.finish().await?;
    Ok(())
}

/// Credits for marketplace sales the seller never collected.
pub async fn marketplace_credits(ctx: Arc<Ctx>) -> Result<()> {
    if !ctx.has_src("marketplace_items") {
        return Ok(());
    }
    let owed = query_rows(
        &ctx.src,
        "SELECT user_id, SUM(price) owed FROM marketplace_items WHERE state = 2 GROUP BY user_id",
    )
    .await?;
    if ctx.validate_only {
        return Ok(());
    }
    let mut conn = crate::db::bulk_conn(&ctx.dst).await?;
    let mut total = 0;
    for r in owed {
        use mysql_async::prelude::Queryable;
        conn.exec_drop(
            "UPDATE users SET credits = credits + ? WHERE id = ?",
            (r.i64("owed"), r.i64("user_id")),
        )
        .await?;
        total += r.i64("owed");
    }
    if total > 0 {
        println!("  {total} uncollected marketplace credits paid to sellers");
    }
    Ok(())
}
