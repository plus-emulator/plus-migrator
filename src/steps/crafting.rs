//! Preserve the hotel's crafting definitions and learned recipes, remapping base ids.
use crate::{
    context::Ctx,
    db::{RowStream, b, i, null, s},
};
use anyhow::Result;
use std::sync::Arc;

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    if !ctx.has_src("crafting_recipes") {
        return Ok(());
    }
    let w = ctx
        .writer(
            "crafting_recipes",
            &[
                "id",
                "code",
                "product_code",
                "reward_item_id",
                "enabled",
                "secret",
                "remaining",
                "achievement",
            ],
        )
        .await?;
    let mut valid = std::collections::HashSet::new();
    let mut rows = RowStream::open(&ctx.src, "SELECT * FROM crafting_recipes").await?;
    while let Some(r) = rows.next().await? {
        let Some(reward) = ctx.maps().furni_id(r.i64("reward")) else {
            ctx.note(format!(
                "Crafting recipe {} has a missing reward",
                r.i64("id")
            ));
            continue;
        };
        valid.insert(r.i64("id"));
        w.sink
            .send(vec![
                i(r.i64("id")),
                s(r.str("product_name")),
                s(r.str("product_name")),
                i(reward as i64),
                b(r.bool("enabled")),
                b(r.bool("secret")),
                if r.bool("limited") {
                    i(r.i64("remaining"))
                } else {
                    null()
                },
                s(r.str("achievement")),
            ])
            .await?;
    }
    w.finish().await?;
    for (table, target, source_col, target_col) in [
        (
            "crafting_altars_recipes",
            "crafting_altars_recipes",
            "altar_id",
            "altar_item_id",
        ),
        (
            "crafting_recipes_ingredients",
            "crafting_recipes_ingredients",
            "item_id",
            "item_id",
        ),
    ] {
        if !ctx.has_src(table) {
            continue;
        }
        let ingredients = table == "crafting_recipes_ingredients";
        let mut columns = vec!["recipe_id", target_col];
        if ingredients {
            columns.push("amount");
        }
        let w = ctx.writer(target, &columns).await?;
        let sql = if ingredients {
            "SELECT recipe_id,item_id,MIN(amount) amount,COUNT(DISTINCT amount) variants FROM crafting_recipes_ingredients GROUP BY recipe_id,item_id".to_string()
        } else {
            format!("SELECT * FROM {table}")
        };
        let mut rows = RowStream::open(&ctx.src, &sql).await?;
        while let Some(r) = rows.next().await? {
            let recipe = r.i64("recipe_id");
            if ingredients && r.i64("variants") > 1 {
                ctx.note(format!(
                    "Crafting recipe {recipe} has contradictory duplicate ingredient amounts"
                ));
            }
            let Some(item) = ctx.maps().furni_id(r.i64(source_col)) else {
                ctx.note(format!(
                    "{table}: recipe {recipe} references missing furniture"
                ));
                continue;
            };
            if !valid.contains(&recipe) {
                ctx.note(format!("{table}: missing recipe {recipe}"));
                continue;
            }
            let mut row = vec![i(recipe), i(item as i64)];
            if ingredients {
                row.push(i(r.i64("amount")));
            }
            w.sink.send(row).await?;
        }
        w.finish().await?;
    }
    if ctx.has_src("users_recipes") {
        let w = ctx
            .writer("user_crafting_recipes", &["user_id", "recipe_id"])
            .await?;
        let mut rows = RowStream::open(&ctx.src, "SELECT * FROM users_recipes").await?;
        while let Some(r) = rows.next().await? {
            if !valid.contains(&r.i64("recipe"))
                || !ctx.maps().user_names.contains_key(&r.i64("user_id"))
            {
                ctx.note("Learned crafting recipe references a missing user or recipe");
                continue;
            }
            w.sink
                .send(vec![i(r.i64("user_id")), i(r.i64("recipe"))])
                .await?;
        }
        w.finish().await?;
    }
    Ok(())
}
