//! Preflight: checks the target is a fresh Plus database, clears the content
//! the source replaces, and builds the lookup tables every step shares.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result, bail};
use mysql_async::prelude::*;

use crate::context::Ctx;
use crate::db::{self, query_rows, scalar_i64};
use crate::steps::interactions;
use crate::wired;

/// Target furniture ids the Plus C# code hard-codes. Their rows are kept (as
/// emulator-only aliases) so those features keep working, and any source
/// definition that happens to use the same id moves to a fresh id.
///   320  pet food handed out with pet purchases (CatalogPurchaseService)
///   4221 / 4450 saddles returned to inventory (ItemUtility)
const PINNED_FURNITURE: &[u32] = &[320, 4221, 4450];

/// Tables holding hotel data. All are emptied before a run so the migrator can
/// be re-run against the same target.
pub const HOTEL_TABLES: &[&str] = &[
    "users", "users_settings", "user_statistics", "user_info", "user_currencies", "user_badges",
    "user_clothing", "user_wardrobe", "user_effects", "user_achievements", "user_favorites", "user_ignores",
    "user_saved_searches", "user_roomvisits", "user_club_memberships", "club_membership_intervals",
    "club_paydays", "club_credit_spending", "club_gift_claims", "user_roles", "user_permissions", "user_quests",
    "user_presents", "user_vouchers", "user_sessions", "user_access_tokens", "user_remember_tokens",
    "bans", "chatlogs", "chatlogs_console", "messenger_friendships", "messenger_requests",
    "messenger_offline_messages", "groups", "group_memberships", "group_requests", "group_forums",
    "group_forum_threads", "group_forum_messages", "group_forum_read_markers", "group_forum_post_limits",
    "rooms", "room_rights", "room_bans", "room_promotions", "room_filter", "room_wired_settings",
    "navigator_publics", "items", "items_groups", "room_items_moodlight", "room_items_toner",
    "room_items_tele_links", "room_music_players", "room_music_playlist", "room_music_songs",
    "wired_items", "wired_item_configurations", "wired_reward_state", "wired_variable_values",
    "wired_variable_locks", "bots", "bots_petdata", "bots_speech", "catalog_marketplace_offers",
    "catalog_marketplace_data", "catalog_vouchers", "camera_media", "camera_publications", "camera_purchases",
    "camera_quota", "camera_accounts", "camera_competition_entries",
];

/// Furniture and catalog content that the source hotel's own replaces.
pub const CONTENT_TABLES: &[&str] = &[
    "catalog_page_offers", "catalog_offer_limited", "catalog_offer_products", "club_gift_offers",
    "catalog_offers", "catalog_page_texts", "catalog_page_images", "catalog_pages", "catalog_promotions",
    "catalog_bot_presets", "crafting_recipes_ingredients", "crafting_altars_recipes", "crafting_recipes",
    "user_crafting_recipes", "recycler_prizes", "room_music_disc_definitions",
];

/// A source furni definition and what it becomes in Plus.
#[derive(Debug, Clone)]
pub struct FurniDef {
    pub dst_id: u32,
    /// Plus type letter: s, i, e, h, v, r, b or p.
    pub kind: String,
    pub item_name: String,
    pub sprite_id: i64,
    pub src_interaction: String,
    pub interaction: String,
    pub behaviour_data: i64,
    pub modes: i64,
    /// Plus wired box name when this is a wired box.
    pub wired_box: Option<&'static str>,
    /// The source interaction has no Plus behaviour.
    pub fell_back: bool,
}

#[derive(Debug, Clone)]
pub struct Role {
    pub id: i64,
    pub slug: String,
    pub security_level: i64,
}

#[derive(Debug, Default)]
pub struct Maps {
    /// Source items_base id -> definition.
    pub furni: HashMap<u32, FurniDef>,
    /// Source rank id -> Plus role (absent = the implicit default role).
    pub roles: BTreeMap<i64, Role>,
    /// Source rank id -> catalog page permission key.
    pub rank_permissions: BTreeMap<i64, String>,
    /// Source navigator flat category id -> Plus navigator category id.
    pub categories: HashMap<i64, i64>,
    pub category_fallback: i64,
    /// Source room id -> Plus room model id.
    pub room_models: HashMap<i64, String>,
    /// Room models to create (custom floor plans and models Plus lacks).
    pub new_models: Vec<RoomModel>,
    /// Lower-cased username -> user id, for formats that store names.
    pub user_ids: HashMap<String, i64>,
    pub user_names: HashMap<i64, String>,
    /// Source user id -> real rank, after undoing Polaris temporary ranks.
    pub real_ranks: HashMap<i64, i64>,
    /// Next free id for items the migrator has to create (gift splits).
    pub next_item_id: AtomicI64,
    /// Added to Arcturus pet ids so they do not collide with bot ids.
    pub pet_id_offset: i64,
    /// Source rooms that exist (for filtering references to deleted rooms).
    pub rooms: HashSet<i64>,
    /// Plus navigator category ids that exist.
    pub plus_categories: HashSet<i64>,
}

impl Maps {
    pub fn furni_id(&self, src: i64) -> Option<u32> {
        u32::try_from(src).ok().and_then(|id| self.furni.get(&id)).map(|d| d.dst_id)
    }

    pub fn furni(&self, src: i64) -> Option<&FurniDef> {
        u32::try_from(src).ok().and_then(|id| self.furni.get(&id))
    }

    pub fn alloc_item_id(&self) -> i64 {
        self.next_item_id.fetch_add(1, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct RoomModel {
    pub id: String,
    pub door_x: i64,
    pub door_y: i64,
    pub door_z: f64,
    pub door_dir: i64,
    pub heightmap: String,
    pub wall_height: i64,
    pub required_club_level: i64,
}

pub async fn prepare(ctx: &Ctx, force: bool) -> Result<()> {
    check_target(ctx, force).await?;
    clear_target(ctx).await?;
    let maps = build_maps(ctx).await?;
    ctx.maps.set(maps).map_err(|_| anyhow::anyhow!("prepare ran twice"))?;
    Ok(())
}

async fn check_target(ctx: &Ctx, force: bool) -> Result<()> {
    if !ctx.dst_schema.has_table("furniture") || !ctx.dst_schema.has_table("catalog_offers") {
        bail!("the target is not a Plus database; import Database/FreshInstall.sql into it first");
    }
    if !force && scalar_i64(&ctx.dst, "SELECT COUNT(*) FROM users").await? > 0 {
        bail!("the target already has users; pass --force to wipe it");
    }
    Ok(())
}

async fn clear_target(ctx: &Ctx) -> Result<()> {
    let mut conn = db::bulk_conn(&ctx.dst).await?;
    for t in HOTEL_TABLES.iter().chain(CONTENT_TABLES) {
        if ctx.dst_schema.has_table(t) {
            conn.query_drop(format!("TRUNCATE TABLE `{t}`")).await.with_context(|| format!("clearing {t}"))?;
        }
    }
    // Custom models belonged to the previous hotel; standard models stay.
    conn.query_drop("DELETE FROM room_models WHERE custom = 1").await?;

    let camera = scalar_i64(
        &ctx.dst,
        "SELECT CAST(value AS UNSIGNED) FROM server_settings WHERE `key` = 'camera.item_id'",
    )
    .await
    .unwrap_or(0);
    let mut keep: Vec<String> = PINNED_FURNITURE.iter().map(|id| id.to_string()).collect();
    if camera > 0 {
        keep.push(camera.to_string());
    }
    conn.query_drop(format!("DELETE FROM furniture WHERE id NOT IN ({})", keep.join(","))).await?;
    // Kept rows stay emulator-only so the source's furnidata owns their classnames.
    conn.query_drop("UPDATE furniture SET has_furnidata = 0").await?;
    Ok(())
}

async fn build_maps(ctx: &Ctx) -> Result<Maps> {
    let mut maps = Maps { category_fallback: 36, ..Default::default() };

    // ---- furniture -------------------------------------------------------
    let kept: HashSet<u32> = query_rows(&ctx.dst, "SELECT id FROM furniture")
        .await?
        .iter()
        .map(|r| r.i64("id") as u32)
        .collect();
    let base = query_rows(
        &ctx.src,
        "SELECT id, item_name, type, sprite_id, interaction_type, interaction_modes_count FROM items_base ORDER BY id",
    )
    .await?;
    let max_src = base.iter().map(|r| r.i64("id")).max().unwrap_or(0);
    let mut fresh = max_src.max(0) as u32 + 1;
    for r in &base {
        let src_id = r.i64("id") as u32;
        let dst_id = if kept.contains(&src_id) {
            while kept.contains(&fresh) {
                fresh += 1;
            }
            fresh += 1;
            fresh - 1
        } else {
            src_id
        };
        let item_name = r.text("item_name");
        let kind = furni_kind(&r.str("type"));
        let src_interaction = r.str("interaction_type").trim().to_ascii_lowercase();
        let wired_box = wired::target_box(ctx.flavor, &src_interaction);
        let (interaction, behaviour_data, fell_back) = match wired_box {
            Some(name) => (name.to_string(), 0, false),
            None => {
                let t = interactions::translate(&src_interaction, &item_name, &kind);
                (t.interaction, t.behaviour_data, t.fell_back)
            }
        };
        maps.furni.insert(
            src_id,
            FurniDef {
                dst_id,
                kind,
                item_name,
                sprite_id: r.i64("sprite_id"),
                src_interaction,
                interaction,
                behaviour_data,
                modes: r.i64("interaction_modes_count"),
                wired_box,
                fell_back,
            },
        );
    }

    // ---- ranks -------------------------------------------------------------
    let roles = query_rows(&ctx.dst, "SELECT id, slug, security_level, weight FROM roles ORDER BY weight DESC, id")
        .await?;
    // Page keys each role holds; a page gated for a rank uses the strongest key
    // its role actually has, so staff of that rank and above can open it.
    let mut role_keys: HashMap<i64, Vec<String>> = HashMap::new();
    for r in query_rows(&ctx.dst, "SELECT role_id, permission_key FROM role_permissions WHERE permission_key LIKE 'catalog.pages.%'").await? {
        role_keys.entry(r.i64("role_id")).or_default().push(r.str("permission_key"));
    }
    let weight_of = |slug: &str| roles.iter().find(|r| r.str("slug") == slug).map(|r| r.i64("weight")).unwrap_or(-1);
    let rank_table = if ctx.has_src("permission_ranks") { "permission_ranks" } else { "permissions" };
    if ctx.has_src(rank_table) {
        for r in query_rows(&ctx.src, &format!("SELECT * FROM `{rank_table}`")).await? {
            let rank = r.i64("id");
            let level = r.opt_i64("level").unwrap_or(rank).clamp(1, 7);
            let name = r.text("rank_name").to_ascii_lowercase();
            let Some(role) = pick_role(&roles, &name, level) else { continue };
            if let Some(key) = role_keys.get(&role.id).and_then(|keys| {
                keys.iter().max_by_key(|k| weight_of(k.trim_start_matches("catalog.pages."))).cloned()
            }) {
                maps.rank_permissions.insert(rank, key);
            }
            if role.slug != "default" {
                maps.roles.insert(rank, role);
            }
        }
    }

    // ---- users ------------------------------------------------------------
    for r in query_rows(&ctx.src, "SELECT id, username, `rank` FROM users").await? {
        let id = r.i64("id");
        let name = r.text("username");
        maps.user_ids.insert(name.to_lowercase(), id);
        maps.user_names.insert(id, name);
        maps.real_ranks.insert(id, r.i64("rank"));
    }
    // Polaris temporary ranks: users.rank holds the temporary one.
    if ctx.has_src("user_temporary_ranks") {
        for r in query_rows(&ctx.src, "SELECT user_id, previous_rank_id FROM user_temporary_ranks").await? {
            maps.real_ranks.insert(r.i64("user_id"), r.i64("previous_rank_id"));
        }
    }

    // ---- navigator categories ----------------------------------------------
    let plus_cats = query_rows(
        &ctx.dst,
        "SELECT id, category_identifier, public_name, category_type FROM navigator_categories",
    )
    .await?;
    maps.plus_categories = plus_cats.iter().map(|r| r.i64("id")).collect();
    if !maps.plus_categories.contains(&36) {
        maps.category_fallback = plus_cats
            .iter()
            .find(|r| r.str("category_type") == "category")
            .map(|r| r.i64("id"))
            .unwrap_or(0);
    }
    if ctx.has_src("navigator_flatcats") {
        for r in query_rows(&ctx.src, "SELECT id, caption_save, caption FROM navigator_flatcats").await? {
            let label = format!("{} {}", r.str("caption_save"), r.str("caption")).to_ascii_lowercase();
            maps.categories.insert(r.i64("id"), category_for(&label, maps.category_fallback));
        }
    }

    // ---- room models -------------------------------------------------------
    build_room_models(ctx, &mut maps).await?;

    maps.rooms = query_rows(&ctx.src, "SELECT id FROM rooms").await?.iter().map(|r| r.i64("id")).collect();
    let max_item = scalar_i64(&ctx.src, "SELECT COALESCE(MAX(id), 0) FROM items").await?;
    maps.next_item_id = AtomicI64::new(max_item + 1);
    let max_bot = if ctx.has_src("bots") { scalar_i64(&ctx.src, "SELECT COALESCE(MAX(id), 0) FROM bots").await? } else { 0 };
    maps.pet_id_offset = max_bot;

    Ok(maps)
}

fn furni_kind(t: &str) -> String {
    let t = t.trim().to_ascii_lowercase();
    match t.as_str() {
        "s" | "i" | "e" | "h" | "v" | "r" | "b" | "p" => t,
        _ => "s".into(),
    }
}

/// Mirrors the CMS rule (PlusPlayerRepository::roleId): the role at the rank's
/// security level with the highest weight. VIP-style ranks map to the VIP roles
/// rather than to staff roles of the same level.
fn pick_role(roles: &[db::SrcRow], rank_name: &str, level: i64) -> Option<Role> {
    let as_role = |r: &db::SrcRow| Role { id: r.i64("id"), slug: r.str("slug"), security_level: r.i64("security_level") };
    let by_slug = |slug: &str| roles.iter().find(|r| r.str("slug") == slug).map(as_role);
    if rank_name.contains("vip") || rank_name.contains("premium") {
        let slug = if rank_name.contains("gold") || rank_name.contains("super") { "gold_vip" } else { "vip" };
        if let Some(role) = by_slug(slug) {
            return Some(role);
        }
    }
    if level <= 1 {
        return by_slug("default");
    }
    roles.iter().find(|r| r.i64("security_level") == level).map(as_role)
}

/// Arcturus flat categories carry localisation keys such as
/// `${navigator.flatcategory.global.CHAT}`; match them to Plus's user categories.
fn category_for(label: &str, fallback: i64) -> i64 {
    const RULES: &[(&[&str], i64)] = &[
        (&["chat", "chill", "discussion", "debate"], 29),
        (&["trad", "casino", "shop"], 30),
        (&["game", "event", "quiz", "maze"], 31),
        (&["party", "club", "music", "dance"], 32),
        (&["role", "rp", "life"], 33),
        (&["help", "support", "guide"], 34),
        (&["agenc", "fansite", "official", "staff"], 35),
    ];
    for (words, id) in RULES {
        if words.iter().any(|w| label.contains(w)) {
            return *id;
        }
    }
    fallback
}

/// Resolves every room's floor plan. Custom plans (`override_model='1'`) become
/// `model_bc_<room id>`, the name Plus's floor editor uses. A standard model is
/// shared only when Plus has the same model with the same heightmap; otherwise
/// the source model is copied under `arc_<name>`, because Plus moves any floor
/// item outside the heightmap to inventory on first room load.
async fn build_room_models(ctx: &Ctx, maps: &mut Maps) -> Result<()> {
    let plus_models: HashMap<String, String> = query_rows(&ctx.dst, "SELECT id, heightmap FROM room_models WHERE custom = 0")
        .await?
        .into_iter()
        .map(|r| (r.str("id").to_ascii_lowercase(), normalize_heightmap(&r.str("heightmap")).0))
        .collect();

    let mut src_models: HashMap<String, db::SrcRow> = HashMap::new();
    if ctx.has_src("room_models") {
        for r in query_rows(&ctx.src, "SELECT * FROM room_models").await? {
            src_models.insert(r.str("name").to_ascii_lowercase(), r);
        }
    }
    let mut custom: HashMap<i64, db::SrcRow> = HashMap::new();
    if ctx.has_src("room_models_custom") {
        for r in query_rows(&ctx.src, "SELECT * FROM room_models_custom").await? {
            custom.insert(r.i64("id"), r);
        }
    }

    let mut copied: HashMap<String, String> = HashMap::new();
    for room in query_rows(&ctx.src, "SELECT id, model, override_model, wall_height FROM rooms").await? {
        let room_id = room.i64("id");
        if room.bool("override_model") {
            if let Some(c) = custom.get(&room_id) {
                let id = format!("model_bc_{room_id}");
                maps.new_models.push(model_from(&id, c, room.i64("wall_height"), 0));
                maps.room_models.insert(room_id, id);
                continue;
            }
        }
        let name = room.str("model").to_ascii_lowercase();
        let src = src_models.get(&name);
        let same_as_plus = match (src, plus_models.get(&name)) {
            (Some(s), Some(p)) => normalize_heightmap(&s.str("heightmap")).0 == *p,
            (None, Some(_)) => true,
            _ => false,
        };
        if same_as_plus {
            maps.room_models.insert(room_id, name);
        } else if let Some(s) = src {
            let id = copied
                .entry(name.clone())
                .or_insert_with(|| {
                    let id = crate::text::clip(&format!("arc_{name}"), 100);
                    maps.new_models.push(model_from(&id, s, -1, if s.bool("club_only") { 2 } else { 0 }));
                    id
                })
                .clone();
            maps.room_models.insert(room_id, id);
        } else {
            // The room references a model that exists nowhere; the rooms step drops it.
        }
    }
    Ok(())
}

fn model_from(id: &str, r: &db::SrcRow, wall_height: i64, club: i64) -> RoomModel {
    let (heightmap, _) = normalize_heightmap(&r.str("heightmap"));
    let (door_x, door_y) = (r.i64("door_x"), r.i64("door_y"));
    RoomModel {
        id: id.to_string(),
        door_x,
        door_y,
        door_z: tile_height(&heightmap, door_x, door_y).unwrap_or(0.0),
        door_dir: r.i64("door_dir").clamp(0, 7),
        heightmap,
        wall_height: wall_height.clamp(-1, 15),
        required_club_level: club,
    }
}

/// Puts a heightmap in the exact form Plus parses: rows separated by `\r`,
/// tiles `x`, `0-9`, `a-w`, every row padded to the same width. Returns the map
/// and how many tiles had to be blocked because Plus cannot represent them.
pub fn normalize_heightmap(raw: &str) -> (String, usize) {
    let unified = raw.replace("\r\n", "\r").replace('\n', "\r");
    let mut rows: Vec<String> = unified.split('\r').map(|r| r.trim().to_string()).collect();
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    while rows.first().is_some_and(|r| r.is_empty()) {
        rows.remove(0);
    }
    let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    let mut fixed = 0;
    let rows: Vec<String> = rows
        .iter()
        .map(|row| {
            let mut out: String = row
                .chars()
                .map(|c| {
                    let c = c.to_ascii_lowercase();
                    if c == 'x' || c.is_ascii_digit() || ('a'..='w').contains(&c) {
                        c
                    } else {
                        fixed += 1;
                        'x'
                    }
                })
                .collect();
            while out.chars().count() < width {
                out.push('x');
            }
            out
        })
        .collect();
    (rows.join("\r"), fixed)
}

pub fn tile_height(heightmap: &str, x: i64, y: i64) -> Option<f64> {
    let row = heightmap.split('\r').nth(usize::try_from(y).ok()?)?;
    let c = row.chars().nth(usize::try_from(x).ok()?)?;
    match c {
        '0'..='9' => Some((c as u8 - b'0') as f64),
        'a'..='w' => Some((c as u8 - b'a' + 10) as f64),
        _ => None,
    }
}
