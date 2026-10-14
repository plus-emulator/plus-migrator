//! catalog_pages / catalog_items -> Plus's normalised catalog.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;

use crate::context::Ctx;
use crate::db::{RowStream, SrcRow, b, i, null, query_rows, s, scalar_i64};

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    pages(&ctx).await?;
    offers(&ctx).await
}

/// Arcturus layout name -> the client wire name Plus stores.
fn layout(name: &str) -> &str {
    match name {
        "frontpage" => "frontpage4",
        "club_gift" => "club_gifts",
        "guilds" => "guild_frontpage",
        "guild_furni" => "guild_custom_furni",
        "recent_purchases" | "plasto" | "root" | "" => "default_3x3",
        "niko" => "monkey",
        other => other,
    }
}

/// Which Arcturus page columns land in the image and text slots, per layout
/// (reproduces each Arcturus layout's serializer).
fn slots(layout: &str) -> (&'static [&'static str], &'static [&'static str]) {
    const H: &str = "page_headline";
    const T: &str = "page_teaser";
    const SP: &str = "page_special";
    const T1: &str = "page_text1";
    const T2: &str = "page_text2";
    const TD: &str = "page_text_details";
    const TT: &str = "page_text_teaser";
    match layout {
        "frontpage" => (&[H, T], &[T1, T2, TT]),
        "pets" | "pets2" | "pets3" | "productpage1" => (&[H, T], &[T1, T2, TD, TT]),
        "bots" => (&[H, T], &[T1, TD, T2]),
        "single_bundle" => (&[H, T, ""], &[T1, TD, TT, T2]),
        "guilds" | "guild_furni" | "guild_forum" => (&[H, T], &[T1, TD, TT]),
        "club_buy" => (&[H, T], &[]),
        "club_gift" | "info_duckets" | "info_loyalty" => (&[H], &[T1]),
        "roomads" | "mad_money" => (&[H, T], &[T1, T2]),
        "soundmachine" => (&[H, T], &[T1, TD]),
        "info_pets" => (&[H, T], &[T1, "", TT]),
        "marketplace" | "marketplace_own_items" => (&[], &[]),
        _ => (&[H, T, SP], &[T1, TD, TT]),
    }
}

async fn pages(ctx: &Ctx) -> Result<()> {
    let maps = ctx.maps();
    let rows = query_rows(&ctx.src, "SELECT * FROM catalog_pages ORDER BY id").await?;
    let ids: HashSet<i64> = rows.iter().map(|r| r.i64("id")).collect();

    let pw = ctx
        .writer(
            "catalog_pages",
            &[
                "id",
                "parent_id",
                "caption",
                "icon",
                "required_permission",
                "position",
                "link",
                "layout",
                "visible",
                "enabled",
                "required_club_level",
            ],
        )
        .await?;
    let iw = ctx
        .writer("catalog_page_images", &["page_id", "slot", "image"])
        .await?;
    let tw = ctx
        .writer("catalog_page_texts", &["page_id", "slot", "text"])
        .await?;
    let mut links = HashSet::new();

    for r in &rows {
        let id = r.i64("id");
        let parent = r.i64("parent_id");
        let src_layout = r.str("page_layout").to_ascii_lowercase();
        let link = r.str("caption_save").trim().to_string();
        let link = (!link.is_empty() && links.insert(link.to_lowercase())).then_some(link);
        let permission = if r.i64("min_rank") > 1 {
            maps.rank_permissions.get(&r.i64("min_rank")).cloned()
        } else if r.bool("vip_only") {
            Some("catalog.pages.vip".to_string())
        } else {
            None
        };
        pw.sink
            .send(vec![
                i(id),
                if parent > 0 && parent != id && ids.contains(&parent) {
                    i(parent)
                } else {
                    null()
                },
                s(r.text("caption")),
                i(r.i64("icon_image")),
                permission.map(s).unwrap_or(null()),
                i(r.i64("order_num")),
                link.map(s).unwrap_or(null()),
                s(layout(&src_layout)),
                b(r.bool("visible")),
                b(r.bool("enabled")),
                i(if r.bool("club_only") { 1 } else { 0 }),
            ])
            .await?;
        let (images, texts) = slots(&src_layout);
        for (slot, col) in images.iter().enumerate() {
            iw.sink
                .send(vec![i(id), i(slot as i64), s(page_text(r, col))])
                .await?;
        }
        for (slot, col) in texts.iter().enumerate() {
            tw.sink
                .send(vec![i(id), i(slot as i64), s(page_text(r, col))])
                .await?;
        }
    }
    pw.finish().await?;
    iw.finish().await?;
    tw.finish().await?;
    Ok(())
}

fn page_text(r: &SrcRow, col: &str) -> String {
    if col.is_empty() {
        String::new()
    } else {
        r.text(col)
    }
}

async fn offers(ctx: &Ctx) -> Result<()> {
    let maps = ctx.maps();
    let pages: HashMap<i64, String> =
        query_rows(&ctx.src, "SELECT id, includes FROM catalog_pages")
            .await?
            .iter()
            .map(|r| (r.i64("id"), r.str("includes")))
            .collect();
    // Arcturus `includes`: a page also shows the items of the listed pages.
    let mut included_by: HashMap<i64, Vec<i64>> = HashMap::new();
    for (page, inc) in &pages {
        for other in inc
            .split([';', ','])
            .filter_map(|p| p.trim().parse::<i64>().ok())
        {
            included_by.entry(other).or_default().push(*page);
        }
    }

    let ow = ctx
        .writer(
            "catalog_offers",
            &[
                "id",
                "localization_key",
                "cost_credits",
                "cost_points",
                "points_type",
                "club_level",
                "bulk_purchase",
                "enabled",
                "preview_image",
            ],
        )
        .await?;
    let prw = ctx
        .writer(
            "catalog_offer_products",
            &[
                "offer_id",
                "position",
                "product_type",
                "furniture_id",
                "effect_id",
                "badge_code",
                "bot_preset_id",
                "pet_type",
                "amount",
                "extra_param",
            ],
        )
        .await?;
    let pow = ctx
        .writer("catalog_page_offers", &["page_id", "offer_id", "position"])
        .await?;
    let lw = ctx
        .writer("catalog_offer_limited", &["offer_id", "stack", "sold"])
        .await?;
    let bw = ctx
        .writer(
            "catalog_bot_presets",
            &["id", "name", "figure", "gender", "motto", "ai_type"],
        )
        .await?;
    let badges = ctx
        .writer_ignore("badge_definitions", &["code", "required_right"])
        .await?;

    let mut skipped = 0usize;
    let mut missing_products = 0usize;
    // Some Arcturus forks key catalog_items by (id, extradata). Plus keys offers by id.
    // Keep every variant, with deterministic new ids for additional rows.
    let mut next_id =
        scalar_i64(&ctx.src, "SELECT COALESCE(MAX(id),0)+1 FROM catalog_items").await?;
    let mut used_ids = HashSet::new();
    let mut rows = RowStream::open(
        &ctx.src,
        "SELECT * FROM catalog_items ORDER BY id, extradata",
    )
    .await?;
    while let Some(r) = rows.next().await? {
        let source_id = r.i64("id");
        let id = if used_ids.insert(source_id) {
            source_id
        } else {
            let id = next_id;
            next_id += 1;
            anyhow::ensure!(
                id <= i32::MAX as i64,
                "catalog variant id exceeds Plus range"
            );
            id
        };
        let page = r.i64("page_id");
        let name = r.text("catalog_name");
        let extradata = r.text("extradata");
        let default_amount = r.i64("amount").max(1);

        let mut products = Vec::new();
        for (item, amount) in parse_item_ids(&r.str("item_ids"), default_amount) {
            let Some(def) = maps.furni(item) else {
                missing_products += 1;
                continue;
            };
            if !(1..=1000).contains(&amount) {
                ctx.note(format!("Catalog offer {source_id}: product amount {amount} exceeds Plus purchase bounds"));
            }
            let product = match def.kind.as_str() {
                _ if def.interaction == "pet" => {
                    match pet_type(&def.item_name, &def.src_interaction) {
                        Some(t) => Product::Pet(t),
                        None => {
                            missing_products += 1;
                            continue;
                        }
                    }
                }
                "s" | "i" => Product::Furni(
                    def.dst_id,
                    amount,
                    furni_param(&name, &def.interaction, &extradata),
                ),
                "e" => Product::Effect(effect_id(&def.item_name, def.sprite_id)),
                "b" => Product::Badge(def.item_name.to_string()),
                "r" => Product::Bot(id),
                "p" => match pet_type(&def.item_name, &def.src_interaction) {
                    Some(t) => Product::Pet(t),
                    None => {
                        missing_products += 1;
                        continue;
                    }
                },
                _ => {
                    missing_products += 1;
                    continue;
                }
            };
            if let Product::Bot(preset) = product {
                let kv = parse_kv(&extradata);
                let ai = if def.item_name.contains("bartender") {
                    "bartender"
                } else {
                    "generic"
                };
                bw.sink
                    .send(vec![
                        i(preset),
                        s(kv.get("name").cloned().unwrap_or_else(|| "Bot".into())),
                        s(kv.get("figure").cloned().unwrap_or_default()),
                        s(
                            if kv
                                .get("gender")
                                .is_some_and(|g| g.eq_ignore_ascii_case("f"))
                            {
                                "F"
                            } else {
                                "M"
                            },
                        ),
                        s(kv.get("motto").cloned().unwrap_or_default()),
                        s(ai),
                    ])
                    .await?;
            }
            products.push(product);
        }
        let attached_badge = r.str("badge");
        if !attached_badge.is_empty()
            && !products
                .iter()
                .any(|p| matches!(p, Product::Badge(code) if *code==attached_badge))
        {
            products.push(Product::Badge(attached_badge));
        }
        if products.is_empty() {
            skipped += 1;
            continue;
        }
        // Plus convention: an attached badge sits at position 0.
        products.sort_by_key(|p| !matches!(p, Product::Badge(_)));

        ow.sink
            .send(vec![
                i(id),
                s(name.to_string()),
                i(r.i64("cost_credits").max(0)),
                i(r.i64("cost_points").max(0)),
                i(r.i64("points_type").max(0)),
                i(if r.bool("club_only") { 1 } else { 0 }),
                b(r.bool("have_offer")),
                b(true),
                s(""),
            ])
            .await?;
        for (pos, p) in products.iter().enumerate() {
            let (kind, furni, effect, badge, bot, pet, amount, param) = match p {
                Product::Furni(f, a, x) => (
                    "furni",
                    i(*f as i64),
                    null(),
                    null(),
                    null(),
                    null(),
                    *a,
                    x.clone(),
                ),
                Product::Effect(e) => (
                    "effect",
                    null(),
                    i(*e),
                    null(),
                    null(),
                    null(),
                    1,
                    String::new(),
                ),
                Product::Badge(c) => (
                    "badge",
                    null(),
                    null(),
                    s(c.clone()),
                    null(),
                    null(),
                    1,
                    String::new(),
                ),
                Product::Bot(b) => (
                    "bot",
                    null(),
                    null(),
                    null(),
                    i(*b),
                    null(),
                    1,
                    String::new(),
                ),
                Product::Pet(t) => (
                    "pet",
                    null(),
                    null(),
                    null(),
                    null(),
                    i(*t),
                    1,
                    String::new(),
                ),
            };
            if let Product::Badge(c) = p {
                badges.sink.send(vec![s(c.clone()), s("")]).await?;
            }
            prw.sink
                .send(vec![
                    i(id),
                    i(pos as i64),
                    s(kind),
                    furni,
                    effect,
                    badge,
                    bot,
                    pet,
                    i(amount),
                    s(param),
                ])
                .await?;
        }
        if pages.contains_key(&page) {
            pow.sink
                .send(vec![i(page), i(id), i(r.i64("order_number"))])
                .await?;
        }
        for other in included_by.get(&page).into_iter().flatten() {
            if *other != page {
                pow.sink
                    .send(vec![i(*other), i(id), i(r.i64("order_number"))])
                    .await?;
            }
        }
        let stack = r.i64("limited_stack");
        if stack > 0 {
            lw.sink
                .send(vec![
                    i(id),
                    i(stack),
                    i(r.i64("limited_sells").clamp(0, stack)),
                ])
                .await?;
        }
    }
    for w in [ow, prw, pow, lw, bw, badges] {
        w.finish().await?;
    }
    if missing_products > 0 {
        ctx.note(format!("{missing_products} catalog products have no usable furniture/pet definition, including partial bundles"));
    }
    if skipped > 0 {
        ctx.note(format!(
            "{skipped} catalog offers have no usable products and were left out"
        ));
    }
    Ok(())
}

enum Product {
    Furni(u32, i64, String),
    Effect(i64),
    Badge(String),
    Bot(i64),
    Pet(i64),
}

/// "123" or "123;456:2;789" (bundle with optional per-item amounts).
fn parse_item_ids(raw: &str, default_amount: i64) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for part in raw.split(';').map(str::trim).filter(|p| !p.is_empty()) {
        let mut it = part.split(':');
        let Some(id) = it.next().and_then(|v| v.trim().parse::<i64>().ok()) else {
            continue;
        };
        let amount = it
            .next()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default_amount);
        // Arcturus keys bundles by id, so a repeated id overwrites the earlier one.
        match out.iter_mut().find(|(i, _)| *i == id) {
            Some(e) => e.1 = amount,
            None => out.push((id, amount)),
        }
    }
    out
}

/// The product extra param: decoration pattern numbers come from the catalog
/// name (`wallpaper_single_101` -> `101`), everything else from extradata.
fn furni_param(catalog_name: &str, interaction: &str, extradata: &str) -> String {
    if matches!(interaction, "wallpaper" | "floor" | "landscape")
        && let Some(n) = catalog_name.split('_').nth(2)
    {
        return n.to_string();
    }
    (extradata).to_string()
}

fn effect_id(item_name: &str, sprite: i64) -> i64 {
    let digits: String = item_name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits
        .chars()
        .rev()
        .collect::<String>()
        .parse()
        .unwrap_or(sprite)
}

/// Pet type from `a0 pet12` names or `pet12` interactions.
fn pet_type(item_name: &str, interaction: &str) -> Option<i64> {
    for src in [item_name, interaction] {
        if let Some(pos) = src.rfind("pet")
            && let Ok(n) = src[pos + 3..].trim().parse()
        {
            return Some(n);
        }
    }
    None
}

/// "name:Robbie;motto:Hi;figure:hd-180;gender:M"
fn parse_kv(raw: &str) -> HashMap<String, String> {
    raw.split(';')
        .filter_map(|p| p.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect()
}
