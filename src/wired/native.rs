//! Version 2 authority consumed by Plus dev e3dc5f34. Runtime IntParams are
//! NOT the stored editor fields: sources, quantifiers and selections are separate.
use super::{Config, Snapshot};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::LazyLock;

static CODES: LazyLock<HashMap<String, (i64, i64)>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("native_codes.json")).expect("native codes")
});

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct Native {
    version: i64,
    category: i64,
    native_code: i64,
    owned_int_params: Vec<i64>,
    text: String,
    primary_items: Vec<Reference>,
    secondary_items: Vec<Reference>,
    furni_source_types: Vec<i64>,
    user_source_types: Vec<i64>,
    variable_ids: Vec<String>,
    delay: Option<i64>,
    quantifier: Option<i64>,
    filter: Option<bool>,
    inverse: Option<bool>,
    saved_state: SavedState,
}
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Reference {
    item_id: i64,
    wall: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct SavedState {
    snapshots: Vec<Snapshot>,
}

pub(super) fn convert(name: &str, c: Config, wall: impl Fn(i64) -> bool) -> Result<Native> {
    let &(category, code) = CODES
        .get(name)
        .context("box absent from pinned Plus registry")?;
    let p = &c.int_params;
    let at = |i| p.get(i).copied().unwrap_or(0);
    let mut n = Native {
        version: 2,
        category,
        native_code: code,
        owned_int_params: vec![],
        text: c.text,
        primary_items: c
            .selected_items
            .into_iter()
            .map(|id| Reference {
                item_id: id,
                wall: wall(id),
            })
            .collect(),
        secondary_items: c
            .secondary_selected_items
            .into_iter()
            .map(|id| Reference {
                item_id: id,
                wall: wall(id),
            })
            .collect(),
        furni_source_types: vec![],
        user_source_types: vec![],
        variable_ids: vec![],
        delay: (category == 1).then_some(c.delay),
        quantifier: (category == 2).then_some(0),
        filter: None,
        inverse: None,
        saved_state: SavedState {
            snapshots: c.snapshots,
        },
    };
    let positive = match name {
        "wf_cnd_not_furni_on" => "wf_cnd_has_furni_on",
        "wf_cnd_not_hv_avtrs" => "wf_cnd_furnis_hv_avtrs",
        "wf_cnd_not_stuff_is" => "wf_cnd_stuff_is",
        "wf_cnd_not_trggrer_on" => "wf_cnd_trggrer_on_frn",
        "wf_cnd_not_in_group" => "wf_cnd_actor_in_group",
        "wf_cnd_not_user_count" => "wf_cnd_user_count_in",
        "wf_cnd_not_wearing_fx" => "wf_cnd_wearing_effect",
        "wf_cnd_not_wearing_b" => "wf_cnd_wearing_badge",
        "wf_cnd_not_match_snap" => "wf_cnd_match_snapshot",
        "wf_cnd_not_in_team" => "wf_cnd_actor_in_team",
        _ => name,
    };
    match positive {
        "wf_trg_says_something" => n.owned_int_params = vec![at(2), at(0), at(1)],
        "wf_trg_walks_on_furni" | "wf_trg_walks_off_furni" => n.furni_source_types = vec![at(0)],
        "wf_trg_state_changed" => n.furni_source_types = vec![at(1)],
        "wf_trg_stuff_state" => {
            n.owned_int_params = vec![at(0)];
            n.furni_source_types = vec![at(1)];
        }
        "wf_trg_bot_reached_stf" => {
            n.furni_source_types = vec![at(0)];
            n.user_source_types = vec![at(1)];
        }
        "wf_trg_bot_reached_avtr" => n.user_source_types = vec![at(0)],
        "wf_trg_periodically"
        | "wf_trg_period_long"
        | "wf_trg_period_short"
        | "wf_trg_at_given_time"
        | "wf_trg_at_time_long"
        | "wf_trg_score_achieved" => n.owned_int_params = p.clone(),
        "wf_trg_enter_room" | "wf_trg_collision" | "wf_trg_game_starts" | "wf_trg_game_ends" => {}
        "wf_act_toggle_state" => {
            n.owned_int_params = vec![at(0)];
            n.furni_source_types = vec![at(1)];
        }
        "wf_act_toggle_to_rnd" | "wf_act_chase" | "wf_act_flee" | "wf_act_call_stacks" => {
            n.furni_source_types = vec![at(0)]
        }
        "wf_act_reset_timers" => {}
        "wf_act_teleport_to" => {
            n.owned_int_params = vec![at(0)];
            n.furni_source_types = vec![at(1)];
            n.user_source_types = vec![at(2)];
        }
        "wf_act_match_to_sshot" => {
            n.owned_int_params = p.get(..4).context("missing runtime parameters")?.to_vec();
            n.furni_source_types = vec![at(4)];
        }
        "wf_act_move_rotate" => {
            let directions = [-1, 8, 9, 10, 0, 2, 4, 6, 1, 3, 5, 7];
            let rotations = [0, 2, 4, 6];
            n.owned_int_params = vec![
                directions
                    .iter()
                    .position(|v| *v == at(0))
                    .context("invalid movement")? as i64,
                rotations
                    .iter()
                    .position(|v| *v == at(1))
                    .context("invalid rotation")? as i64,
            ];
            n.furni_source_types = vec![at(2)];
        }
        "wf_act_move_to_dir" => {
            n.owned_int_params = vec![at(0), at(1), at(3)];
            n.furni_source_types = vec![at(2)];
        }
        "wf_act_move_furni_to" => {
            n.owned_int_params = vec![at(0), at(1)];
            // Legacy Arcturus moves the triggering furni toward the picked target.
            n.furni_source_types = vec![0, 101];
            n.secondary_items = std::mem::take(&mut n.primary_items);
        }
        "wf_act_give_score" => {
            n.owned_int_params = vec![at(0), c.score_quota_per_game.unwrap_or(0)];
            n.user_source_types = vec![0];
        }
        "wf_act_give_score_tm" => {
            n.owned_int_params = vec![at(0), c.score_quota_per_game.unwrap_or(0), at(2)]
        }
        "wf_act_show_message" => {
            n.owned_int_params = p.get(1..4).context("missing runtime parameters")?.to_vec();
            n.user_source_types = vec![at(0)];
        }
        "wf_act_kick_user" | "wf_act_leave_team" => n.user_source_types = vec![at(0)],
        "wf_act_mute_triggerer" => {
            n.owned_int_params = vec![at(0)];
            n.user_source_types = vec![at(1)];
        }
        "wf_act_join_team" => {
            n.owned_int_params = vec![at(1), at(0)];
            n.user_source_types = vec![at(2)];
        }
        "wf_act_give_reward" => {
            n.owned_int_params = p.get(..4).context("missing runtime parameters")?.to_vec();
            n.user_source_types = vec![at(4)];
        }
        "wf_act_bot_teleport" | "wf_act_bot_move" => {
            n.furni_source_types = vec![at(0)];
            n.user_source_types = vec![at(1)];
        }
        "wf_act_bot_talk" => {
            n.owned_int_params = vec![at(0), at(2), 0];
            n.user_source_types = vec![at(1)];
        }
        "wf_act_bot_talk_to_avatar" => {
            n.owned_int_params = vec![at(0), at(3), 0];
            n.user_source_types = vec![at(2), at(1)];
        }
        "wf_act_bot_give_handitem" | "wf_act_bot_follow_avatar" => {
            n.owned_int_params = vec![at(0)];
            n.user_source_types = vec![at(2), at(1)];
        }
        "wf_act_bot_clothes" => n.user_source_types = vec![at(0)],
        "wf_cnd_has_furni_on" | "wf_cnd_furnis_hv_avtrs" => {
            n.owned_int_params = vec![at(0)];
            n.furni_source_types = vec![at(1)];
        }
        "wf_cnd_stuff_is" => {
            n.furni_source_types = vec![0, 101];
            n.text.clear();
            n.primary_items.clear();
            n.quantifier = Some(at(2));
        }
        "wf_cnd_trggrer_on_frn" => {
            n.furni_source_types = vec![at(0)];
            n.user_source_types = vec![at(1)];
            n.quantifier = Some(at(2));
        }
        "wf_cnd_actor_in_group" => {
            n.user_source_types = vec![at(0)];
            n.text = if at(1) == 1 {
                at(2).to_string()
            } else {
                String::new()
            };
            n.quantifier = Some(at(3));
        }
        "wf_cnd_user_count_in" => {
            n.owned_int_params = p.get(..2).context("missing runtime parameters")?.to_vec()
        }
        "wf_cnd_wearing_effect" | "wf_cnd_actor_in_team" | "wf_cnd_has_handitem" => {
            n.owned_int_params = vec![at(0)];
            n.user_source_types = vec![at(1)];
            n.quantifier = Some(at(2));
        }
        "wf_cnd_wearing_badge" => {
            n.user_source_types = vec![at(0)];
            n.quantifier = Some(at(1));
        }
        "wf_cnd_time_less_than" | "wf_cnd_time_more_than" | "wf_cnd_date_rng_active" => {
            n.owned_int_params = p.clone()
        }
        "wf_cnd_match_snapshot" => {
            n.owned_int_params = p.get(..4).context("missing runtime parameters")?.to_vec();
            n.furni_source_types = vec![at(4)];
            n.quantifier = Some(at(5));
        }
        "wf_xtra_unseen" => {}
        "wf_xtra_random" => n.owned_int_params = vec![at(1), at(0)],
        "wf_xtra_or_eval" => {
            n.owned_int_params = vec![at(0), 0, 0];
            n.furni_source_types = vec![100];
        }
        _ => bail!("no reviewed version-2 converter for {name}"),
    }
    Ok(n)
}
