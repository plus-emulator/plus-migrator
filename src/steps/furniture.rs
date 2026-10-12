//! items_base -> furniture.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::Value as Json;

use crate::context::Ctx;
use crate::db::{RowStream, b, f, i, null, query_rows, s};

const COLUMNS: &[&str] = &[
    "id", "item_name", "public_name", "type", "width", "length", "stack_height", "can_stack", "can_sit",
    "is_walkable", "sprite_id", "allow_recycle", "allow_trade", "allow_marketplace_sell", "allow_gift",
    "allow_inventory_stack", "interaction_type", "behaviour_data", "interaction_modes_count", "vending_ids",
    "height_adjustable", "effect_id", "wired_id", "is_rare", "extra_rot", "custom_params", "has_furnidata",
    "revision", "category", "default_dir", "xdim", "ydim", "part_colors", "name", "description", "ad_url",
    "excluded_dynamic", "special_type", "can_stand_on", "can_sit_on", "can_lay_on", "furni_line", "environment",
    "rare",
];

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    let maps = ctx.maps();
    let furnidata = match &ctx.furnidata {
        Some(path) => load_furnidata(path)?,
        None => {
            ctx.note("no --furnidata given: Plus will not serve furnidata for imported furni; keep serving the hotel's own FurnitureData.json");
            HashMap::new()
        }
    };
    let clothing: HashMap<String, String> = if ctx.has_src("catalog_clothing") {
        query_rows(&ctx.src, "SELECT name, setid FROM catalog_clothing")
            .await?
            .iter()
            .map(|r| (r.str("name").to_ascii_lowercase(), r.str("setid")))
            .collect()
    } else {
        HashMap::new()
    };

    let w = ctx.writer("furniture", COLUMNS).await?;
    let mut seen_sprite = HashSet::new();
    let mut seen_class = HashSet::new();
    let mut fell_back: HashMap<String, usize> = HashMap::new();
    let mut rows = RowStream::open(&ctx.src, "SELECT * FROM items_base ORDER BY id").await?;
    while let Some(r) = rows.next().await? {
        let Some(def) = maps.furni(r.i64("id")) else { continue };
        let mut interaction = def.interaction.clone();
        if interaction == "default" && def.wired_box.is_none() {
            if r.bool("allow_lay") {
                interaction = "bed".into();
            } else if def.fell_back {
                *fell_back.entry(def.src_interaction.clone()).or_default() += 1;
            }
        }

        let fd = furnidata.get(&(def.kind.clone(), def.sprite_id));
        let class_key = def.item_name.to_lowercase();
        let has_fd = fd.is_some()
            && matches!(def.kind.as_str(), "s" | "i")
            && !seen_sprite.contains(&(def.kind.clone(), def.sprite_id))
            && !seen_class.contains(&class_key);
        if has_fd {
            seen_sprite.insert((def.kind.clone(), def.sprite_id));
            seen_class.insert(class_key.clone());
        }
        let fd = if has_fd { fd } else { None };
        let fs = |k: &str| fd.and_then(|e| e.get(k)).map(json_str).map(s).unwrap_or(null());
        let fi = |k: &str, d: i64| fd.and_then(|e| e.get(k)).map(|v| json_i64(v)).unwrap_or(d);

        let custom_params = if interaction == "purchasable_clothing" {
            clothing.get(&class_key).cloned().map(s).unwrap_or(null())
        } else {
            fs("customparams")
        };

        w.sink
            .send(vec![
                i(def.dst_id as i64),
                s(def.item_name.clone()),
                s(r.text("public_name")),
                s(def.kind.clone()),
                i(r.i64("width").max(1)),
                i(r.i64("length").max(1)),
                f(r.f64("stack_height")),
                b(r.bool("allow_stack")),
                b(r.bool("allow_sit")),
                b(r.bool("allow_walk")),
                i(def.sprite_id),
                b(r.bool("allow_recycle")),
                b(r.bool("allow_trade")),
                b(r.bool("allow_marketplace_sell")),
                b(r.bool("allow_gift")),
                b(r.bool("allow_inventory_stack")),
                s(interaction),
                i(def.behaviour_data),
                i(def.modes.max(1)),
                s(number_list(&r.str("vending_ids"), true)),
                s(number_list(&r.str("multiheight"), false)),
                i(r.i64("effect_id_male").max(r.i64("effect_id"))),
                i(0),
                b(fi("rare", 0) != 0),
                i(0),
                custom_params,
                b(has_fd),
                i(fi("revision", 0)),
                fs("category"),
                i(fi("defaultdir", 0)),
                i(fi("xdim", r.i64("width").max(1))),
                i(fi("ydim", r.i64("length").max(1))),
                fd.and_then(|e| e.pointer("/partcolors/color"))
                    .and_then(Json::as_array)
                    .map(|a| s(a.iter().map(json_str).collect::<Vec<_>>().join(",")))
                    .unwrap_or(null()),
                fs("name"),
                fs("description"),
                fs("adurl"),
                b(fi("excludeddynamic", 0) != 0),
                i(fi("specialtype", 1)),
                b(fi("canstandon", 0) != 0),
                b(fi("cansiton", 0) != 0),
                b(fi("canlayon", 0) != 0),
                fs("furniline"),
                fs("environment"),
                b(fi("rare", 0) != 0),
            ])
            .await?;
    }
    w.finish().await?;

    if !fell_back.is_empty() {
        let mut list: Vec<_> = fell_back.into_iter().collect();
        list.sort_by(|a, b| b.1.cmp(&a.1));
        let names: Vec<String> = list.iter().map(|(k, n)| format!("{k} ({n})")).collect();
        ctx.note(format!("interaction types Plus has no behaviour for, now plain furni: {}", names.join(", ")));
    }
    Ok(())
}

/// Arcturus separates vending ids and heights with `;`; Plus needs `,` and
/// skips the whole furni row on anything that is not a number.
fn number_list(raw: &str, ints: bool) -> String {
    let parts: Vec<&str> = raw
        .split([';', ','])
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| if ints { p.parse::<i64>().is_ok() } else { p.parse::<f64>().is_ok() })
        .collect();
    if parts.is_empty() || parts == ["0"] { "0".into() } else { parts.join(",") }
}

fn json_str(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

fn json_i64(v: &Json) -> i64 {
    match v {
        Json::Number(n) => n.as_i64().unwrap_or(0),
        Json::Bool(b) => *b as i64,
        Json::String(s) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

/// Nitro FurnitureData.json, keyed by (type, sprite id).
fn load_furnidata(path: &std::path::Path) -> Result<HashMap<(String, i64), Json>> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let doc: Json = serde_json::from_slice(&raw).context("FurnitureData.json is not valid JSON")?;
    let mut out = HashMap::new();
    for (section, kind) in [("roomitemtypes", "s"), ("wallitemtypes", "i")] {
        if let Some(list) = doc.pointer(&format!("/{section}/furnitype")).and_then(Json::as_array) {
            for e in list {
                let id = e.get("id").map(json_i64).unwrap_or(0);
                out.insert((kind.to_string(), id), e.clone());
            }
        }
    }
    Ok(out)
}

