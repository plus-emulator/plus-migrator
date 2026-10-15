//! Source inventory and post-write integrity checks. A recovery copy does not
//! turn an unimplemented gameplay feature into a supported one.
use crate::{
    archive::ident,
    context::Ctx,
    db::{query_rows, scalar_i64},
};
use anyhow::Result;
use std::collections::BTreeMap;

// A table being listed means an adapter reads it, not that every field is
// compatible. Unknown populated tables must never disappear from the result.
const ADAPTED_TABLES: &[&str] = &[
    "users",
    "users_settings",
    "users_currency",
    "users_badges",
    "users_effects",
    "users_clothing",
    "users_wardrobe",
    "users_achievements",
    "users_subscriptions",
    "permissions",
    "permission_ranks",
    "user_temporary_ranks",
    "navigator_flatcats",
    "room_models",
    "room_models_custom",
    "rooms",
    "room_rights",
    "room_bans",
    "room_promotions",
    "room_wordfilter",
    "navigator_publics",
    "guilds",
    "guilds_members",
    "guilds_forums_threads",
    "guilds_forums_comments",
    "items",
    "items_base",
    "items_crackable",
    "items_teleports",
    "marketplace_items",
    "catalog_clothing",
    "catalog_pages",
    "catalog_items",
    "crafting_recipes",
    "crafting_recipes_ingredients",
    "crafting_altars_recipes",
    "users_recipes",
    "messenger_friendships",
    "messenger_friendrequests",
    "users_ignored",
    "users_favorite_rooms",
    "users_saved_searches",
    "room_enter_log",
    "chatlogs_room",
    "chatlogs_private",
    "messenger_offline",
    "bans",
    "bots",
    "users_pets",
    "soundtracks",
    "room_trax",
    "trax_playlist",
    "room_wired_settings",
    "wired_rewards_given",
];

pub fn table_coverage(counts: &BTreeMap<String, i64>) -> serde_json::Value {
    serde_json::Value::Array(counts.iter().map(|(table,count)|serde_json::json!({
        "table":table,"rows":count,"disposition":if ADAPTED_TABLES.contains(&table.as_str()) {
            "adapter present; field-level limitations may apply"
        } else {"recovery archive only; no runtime adapter"}
    })).collect())
}

pub async fn inventory(ctx: &Ctx) -> Result<BTreeMap<String, i64>> {
    let mut tables: Vec<_> = ctx.src_tables.iter().collect();
    tables.sort();
    let mut counts = BTreeMap::new();
    for table in tables {
        counts.insert(
            table.clone(),
            scalar_i64(&ctx.src, &format!("SELECT COUNT(*) FROM {}", ident(table))).await?,
        );
    }
    for (table, &count) in &counts {
        if count > 0 && !ADAPTED_TABLES.contains(&table.as_str()) {
            ctx.note(format!(
                "{table}: {count} source rows have no runtime adapter (recovery archive only)"
            ));
        }
    }
    for (kind, query) in [
        (
            "views",
            "SELECT COUNT(*) FROM information_schema.views WHERE table_schema=DATABASE()",
        ),
        (
            "triggers",
            "SELECT COUNT(*) FROM information_schema.triggers WHERE trigger_schema=DATABASE()",
        ),
        (
            "routines",
            "SELECT COUNT(*) FROM information_schema.routines WHERE routine_schema=DATABASE()",
        ),
        (
            "events",
            "SELECT COUNT(*) FROM information_schema.events WHERE event_schema=DATABASE()",
        ),
    ] {
        let count = scalar_i64(&ctx.src, query).await?;
        if count > 0 {
            ctx.note(format!("{count} source {kind} require a separate schema backup and compatibility review; the recovery archive covers base tables only"));
        }
    }
    Ok(counts)
}

pub async fn verify(ctx: &Ctx) -> Result<()> {
    for (source, target) in [
        ("users", "users"),
        ("rooms", "rooms"),
        ("users_currency", "user_currencies"),
    ] {
        if !ctx.has_src(source) {
            continue;
        }
        let a = scalar_i64(&ctx.src, &format!("SELECT COUNT(*) FROM {}", ident(source))).await?;
        let b = scalar_i64(&ctx.dst, &format!("SELECT COUNT(*) FROM {}", ident(target))).await?;
        if a != b {
            ctx.note(format!(
                "Integrity: {source} -> {target}: {a} source rows, {b} target rows"
            ));
        }
    }
    let actual_users = query_rows(&ctx.dst, "SELECT id,credits FROM users")
        .await?
        .into_iter()
        .map(|r| (r.i64("id"), r.i64("credits")))
        .collect::<BTreeMap<_, _>>();
    let mut expected_users = query_rows(&ctx.src, "SELECT id,credits FROM users")
        .await?
        .into_iter()
        .map(|r| (r.i64("id"), r.i64("credits")))
        .collect::<BTreeMap<_, _>>();
    if ctx.has_src("marketplace_items") {
        for r in query_rows(
            &ctx.src,
            "SELECT user_id,SUM(price) owed FROM marketplace_items WHERE state=2 GROUP BY user_id",
        )
        .await?
        {
            if let Some(value) = expected_users.get_mut(&r.i64("user_id")) {
                *value += r.i64("owed");
            }
        }
    }
    if actual_users != expected_users {
        ctx.note("Integrity: user identities/credit balances differ from source plus uncollected marketplace sales");
    }
    if ctx.has_src("users_currency") {
        async fn balances(
            pool: &mysql_async::Pool,
            table: &str,
        ) -> Result<BTreeMap<(i64, i64), i64>> {
            Ok(query_rows(
                pool,
                &format!(
                    "SELECT user_id,type,SUM(amount) amount FROM {table} GROUP BY user_id,type"
                ),
            )
            .await?
            .into_iter()
            .map(|r| ((r.i64("user_id"), r.i64("type")), r.i64("amount")))
            .collect())
        }
        if balances(&ctx.src, "users_currency").await?
            != balances(&ctx.dst, "user_currencies").await?
        {
            ctx.note("Integrity: per-user currency balances differ");
        }
    }
    for (source_sql, target_sql, label) in [
        (
            "SELECT user_id,badge_code code FROM users_badges",
            "SELECT user_id,badge_id code FROM user_badges",
            "badge ownership",
        ),
        (
            "SELECT id user_id,'' code FROM rooms",
            "SELECT id user_id,'' code FROM rooms",
            "room identities",
        ),
    ] {
        async fn keys(
            pool: &mysql_async::Pool,
            sql: &str,
        ) -> Result<std::collections::BTreeSet<(i64, String)>> {
            Ok(query_rows(pool, sql)
                .await?
                .into_iter()
                .map(|r| (r.i64("user_id"), r.str("code")))
                .collect())
        }
        if keys(&ctx.src, source_sql).await? != keys(&ctx.dst, target_sql).await? {
            ctx.note(format!("Integrity: {label} differ"));
        }
    }
    for model in &ctx.maps().new_models {
        use mysql_async::prelude::Queryable;
        let mut conn = ctx.dst.get_conn().await?;
        let actual:Option<(i64,i64,f64,i64,String,String,i64)>=conn.exec_first(
            "SELECT door_x,door_y,door_z,door_dir,heightmap,public_items,wall_height FROM room_models WHERE id=?",(&model.id,)).await?;
        if actual
            != Some((
                model.door_x,
                model.door_y,
                model.door_z,
                model.door_dir,
                model.heightmap.clone(),
                model.public_items.clone(),
                model.wall_height,
            ))
        {
            ctx.note(format!("Integrity: geometry changed for {}", model.id));
        }
    }
    // FK_CHECKS=1 does not validate existing rows. Audit each declared composite
    // relationship explicitly, including generated reference columns.
    let keys=query_rows(&ctx.dst,"SELECT constraint_name,table_name,column_name,referenced_table_name,referenced_column_name FROM information_schema.key_column_usage WHERE table_schema=DATABASE() AND referenced_table_name IS NOT NULL ORDER BY table_name,constraint_name,ordinal_position").await?;
    let mut constraints: BTreeMap<(String, String, String), Vec<(String, String)>> =
        BTreeMap::new();
    for r in keys {
        constraints
            .entry((
                r.str("table_name"),
                r.str("constraint_name"),
                r.str("referenced_table_name"),
            ))
            .or_default()
            .push((r.str("column_name"), r.str("referenced_column_name")));
    }
    for ((table, key, parent), columns) in constraints {
        let not_null = columns
            .iter()
            .map(|(c, _)| format!("c.{} IS NOT NULL", ident(c)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let matching = columns
            .iter()
            .map(|(c, p)| format!("c.{}=p.{}", ident(c), ident(p)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let count=scalar_i64(&ctx.dst,&format!("SELECT COUNT(*) FROM {} c WHERE {not_null} AND NOT EXISTS (SELECT 1 FROM {} p WHERE {matching})",ident(&table),ident(&parent))).await?;
        if count > 0 {
            ctx.note(format!(
                "Integrity: {table}.{key} has {count} orphan references to {parent}"
            ));
        }
    }
    Ok(())
}

pub async fn interactions(ctx: &Ctx) -> Result<serde_json::Value> {
    let counts = query_rows(
        &ctx.src,
        "SELECT item_id,COUNT(*) owned,SUM(wired_data<>'') configured FROM items GROUP BY item_id",
    )
    .await?
    .into_iter()
    .map(|r| (r.i64("item_id"), (r.i64("owned"), r.i64("configured"))))
    .collect::<BTreeMap<_, _>>();
    let mut groups: BTreeMap<String, (i64, i64, i64, std::collections::BTreeSet<String>, bool)> =
        BTreeMap::new();
    for (base, definition) in &ctx.maps().furni {
        let row = groups
            .entry(definition.src_interaction.clone())
            .or_default();
        let (owned, configured) = counts.get(&(*base as i64)).copied().unwrap_or_default();
        row.0 += 1;
        row.1 += owned;
        row.2 += configured;
        row.3.insert(definition.interaction.clone());
        row.4 |= definition.fell_back;
    }
    Ok(serde_json::Value::Array(groups.into_iter().map(|(name,(definitions,items,configured,targets,blocked))|serde_json::json!({
        "source":name,"reference_handler":crate::steps::interactions::reference_handler(&name),
        "definitions":definitions,"items":items,"configured":configured,"targets":targets,
        "interaction_mapping":if blocked {"unsupported"} else {"mapped; configuration validation separate"}
    })).collect()))
}
