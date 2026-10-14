//! Wired: maps Arcturus/Polaris boxes to Plus boxes and translates their
//! configuration (items.wired_data) into Plus's wired_item_configurations JSON.
//!
//! Source class-specific JSON is decoded into an intermediate runtime shape,
//! then translated into Plus schema-2 native editor authority by `native`.
//! Only that native representation is persisted. Unsupported formats are blockers,
//! never empty default configurations. Selected item identities remain unchanged.

mod native;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value as J, json};

use crate::context::{Ctx, Flavor};
use crate::db::{RowStream, i, query_rows, s};

static BOXES: LazyLock<HashMap<String, HashMap<String, String>>> =
    LazyLock::new(|| serde_json::from_str(include_str!("boxes.json")).expect("boxes.json"));
/// Reward prizes Plus cannot give (credits, pixels, points, respect).
static DROPPED_PRIZES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static SPECS: LazyLock<HashMap<String, Spec>> =
    LazyLock::new(|| serde_json::from_str(include_str!("polaris.json")).expect("polaris.json"));

/// The Plus box a source wired interaction becomes (None = not a box Plus has).
pub fn target_box(flavor: Flavor, src: &str) -> Option<&'static str> {
    let key = match flavor {
        Flavor::Arcturus => "arcturus",
        Flavor::Polaris => "polaris",
    };
    BOXES.get(key)?.get(src).map(|s| s.as_str())
}

#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
struct Config {
    version: i64,
    int_params: Vec<i64>,
    text: String,
    selected_items: Vec<i64>,
    delay: i64,
    selection_code: i64,
    score_quota_per_game: Option<i64>,
    temporary_placement: Option<J>,
    secondary_selected_items: Vec<i64>,
    furni_sources: HashMap<String, i64>,
    user_sources: HashMap<String, i64>,
    variable_ids: Vec<i64>,
    snapshots: Vec<Snapshot>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Snapshot {
    item_id: i64,
    definition_id: i64,
    x: i64,
    y: i64,
    z: f64,
    rotation: i64,
    state: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    skip_state_restore: bool,
}

#[derive(serde::Deserialize)]
struct Spec {
    ints: Vec<IntSpec>,
    text: Option<String>,
    sel: Option<String>,
    sec: Option<String>,
    delay: Option<i64>,
}

#[derive(serde::Deserialize)]
struct IntSpec {
    f: Option<String>,
    c: Option<i64>,
    b: Option<u8>,
    d: Option<i64>,
    i: Option<usize>,
}

struct RoomItem {
    room: i64,
    base: i64,
    z: f64,
    extra: String,
    wall: bool,
}

/// What a decoder sees: the parsed source data and the box's room.
struct Src<'a> {
    json: Option<J>,
    raw: &'a str,
    room: i64,
    items: &'a HashMap<i64, RoomItem>,
    ctx: &'a Ctx,
}

impl Src<'_> {
    fn n(&self, k: &str, d: i64) -> i64 {
        match self.json.as_ref().and_then(|j| j.get(k)) {
            Some(J::Number(n)) => n.as_i64().unwrap_or(d),
            Some(J::Bool(b)) => *b as i64,
            Some(J::String(s)) => s.trim().parse().unwrap_or(d),
            _ => d,
        }
    }
    fn b(&self, k: &str) -> i64 {
        (self.n(k, 0) != 0) as i64
    }
    fn s(&self, k: &str) -> String {
        match self.json.as_ref().and_then(|j| j.get(k)) {
            Some(J::String(s)) => s.clone(),
            Some(J::Null) | None => String::new(),
            Some(v) => v.to_string(),
        }
    }
    /// A JSON string field, or the whole legacy value when the data is not JSON.
    fn s_or_raw(&self, k: &str) -> String {
        if self.json.is_some() {
            self.s(k)
        } else {
            self.raw.to_string()
        }
    }
    fn has(&self, k: &str) -> bool {
        self.json.as_ref().is_some_and(|j| j.get(k).is_some())
    }
    /// Legacy (pre-JSON) field `n` after splitting on `sep`.
    fn part(&self, sep: char, n: usize) -> String {
        self.raw.split(sep).nth(n).unwrap_or("").to_string()
    }
    fn part_n(&self, sep: char, n: usize, d: i64) -> i64 {
        self.part(sep, n).trim().parse().unwrap_or(d)
    }
    /// Picked furni: JSON list (ints or objects with item_id/itemId), filtered
    /// to furni that were migrated into the same room.
    fn ids(&self, k: &str) -> Vec<i64> {
        let raw: Vec<i64> = match self.json.as_ref().and_then(|j| j.get(k)) {
            Some(J::Array(a)) => a.iter().filter_map(item_id).collect(),
            Some(J::String(s)) => split_ids(s),
            _ => vec![],
        };
        self.keep(raw)
    }
    fn keep(&self, raw: Vec<i64>) -> Vec<i64> {
        let mut seen = HashSet::new();
        raw.into_iter()
            .filter(|id| {
                *id > 0
                    && self.items.get(id).is_some_and(|it| it.room == self.room)
                    && seen.insert(*id)
            })
            .collect()
    }
    fn legacy_ids(&self, sep: char, n: usize) -> Vec<i64> {
        self.keep(split_ids(&self.part(sep, n)))
    }
    fn team(&self, k: &str) -> i64 {
        match self.json.as_ref().and_then(|j| j.get(k)) {
            Some(J::String(name)) => match name.to_ascii_uppercase().as_str() {
                "RED" => 1,
                "GREEN" => 2,
                "BLUE" => 3,
                "YELLOW" => 4,
                other => other.parse().unwrap_or(0),
            },
            _ => self.n(k, 0),
        }
    }
    /// Polaris source selector, or `fallback` for Arcturus. A source of 0 with
    /// picked furni means "picked" in both emulators, so it becomes 100.
    fn source(&self, k: &str, fallback: i64, picked: bool) -> i64 {
        let v = self.n(k, fallback);
        if v == 0 && picked { 100 } else { v }
    }
    fn delay(&self, _max: i64) -> i64 {
        self.n("delay", 0)
    }
    fn snapshots(&self, list: &str) -> (Vec<i64>, Vec<Snapshot>) {
        let mut sel = vec![];
        let mut snaps = vec![];
        let entries: Vec<J> = match self.json.as_ref().and_then(|j| j.get(list)) {
            Some(J::Array(a)) => a.clone(),
            _ => vec![],
        };
        for e in entries {
            let Some(id) = item_id(&e) else { continue };
            let Some(it) = self.items.get(&id).filter(|it| it.room == self.room) else {
                continue;
            };
            let Some(def) = self.ctx.maps().furni_id(it.base) else {
                continue;
            };
            let get = |k: &str| e.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            let state = match e.get("state").and_then(|v| v.as_str()) {
                None => it.extra.clone(),
                Some(st) => st.to_string(),
            };
            sel.push(id);
            snaps.push(Snapshot {
                item_id: id,
                definition_id: def as i64,
                x: get("x"),
                y: get("y"),
                z: e.get("z").and_then(|v| v.as_f64()).unwrap_or(it.z),
                rotation: get("rotation"),
                skip_state_restore: list == "items" && state == " ",
                state,
            });
        }
        (sel, snaps)
    }
}

fn item_id(v: &J) -> Option<i64> {
    match v {
        J::Number(n) => n.as_i64(),
        J::String(s) => s.trim().parse().ok(),
        J::Object(o) => ["item_id", "itemId", "id"]
            .iter()
            .find_map(|k| o.get(*k))
            .and_then(|v| v.as_i64()),
        _ => None,
    }
}

fn split_ids(s: &str) -> Vec<i64> {
    s.split([';', ',', '\r', '\t'])
        .filter_map(|p| p.trim().parse().ok())
        .collect()
}

/// Arcturus placeholders -> Plus placeholders.
fn placeholders(t: &str) -> String {
    t.replace("%user%", "%USERNAME%")
        .replace("%username%", "%USERNAME%")
        .replace("%online_count%", "%USERSONLINE%")
        .replace("%online%", "%USERSONLINE%")
}

fn cfg(ints: Vec<i64>) -> Config {
    Config {
        version: 1,
        int_params: ints,
        ..Default::default()
    }
}

/// Hand-written decoders for the Arcturus box set (and the same boxes in Polaris).
fn decode_known(name: &str, x: &Src) -> Option<Config> {
    let json = x.json.is_some();
    let ids_or = |k: &str, sep: char, n: usize| if json { x.ids(k) } else { x.legacy_ids(sep, n) };
    let legacy_delay = |sep: char, n: usize, _max: i64| x.part_n(sep, n, 0);
    let delay = |max: i64| {
        if json {
            x.delay(max)
        } else {
            legacy_delay('\t', 0, max)
        }
    };

    let c = match name {
        // ---- triggers ----
        "wf_trg_walks_on_furni" | "wf_trg_walks_off_furni" => {
            let sel = ids_or("itemIds", ':', 2);
            let mut c = cfg(vec![x.source("furniSource", 100, !sel.is_empty())]);
            c.selected_items = sel;
            c
        }
        "wf_trg_enter_room" => {
            let mut c = cfg(vec![]);
            c.text = (x.s_or_raw("username").trim()).to_string();
            c
        }
        "wf_trg_says_something" => {
            let (owner, key) = if json {
                (x.b("ownerOnly"), x.s("key"))
            } else {
                ((x.part('\t', 0) == "1") as i64, x.part('\t', 1))
            };
            let mode = if x.has("matchMode") {
                x.n("matchMode", 0)
            } else if key.trim().is_empty() {
                2
            } else {
                0
            };
            let mut c = cfg(vec![mode, x.n("hideMessage", 1), owner]);
            c.text = key.trim().to_string();
            c
        }
        "wf_trg_periodically"
        | "wf_trg_period_short"
        | "wf_trg_at_given_time"
        | "wf_trg_period_long"
        | "wf_trg_at_time_long" => {
            let ms = if json {
                x.n(
                    if name.contains("given") || name.contains("time_long") {
                        "executeTime"
                    } else {
                        "repeatTime"
                    },
                    0,
                )
            } else {
                x.raw.trim().parse().unwrap_or(0)
            };
            // (unit ms, Arcturus minimum, value used below it, Plus max)
            let (unit, min, floor, _max) = match name {
                "wf_trg_period_long" => (5000, 5000, 100_000, 120),
                "wf_trg_at_time_long" => (5000, 500, 10_000, 120),
                "wf_trg_at_given_time" => (500, 500, 10_000, 1200),
                _ => (500, 500, 10_000, 120),
            };
            let ms = if ms < min { floor } else { ms };
            cfg(vec![((ms + unit / 2) / unit)])
        }
        "wf_trg_state_changed" | "wf_trg_stuff_state" => {
            let sel = ids_or("itemIds", ':', 2);
            let mut c = cfg(vec![0, x.source("furniSource", 100, !sel.is_empty())]);
            c.selected_items = sel;
            c
        }
        "wf_trg_collision" | "wf_trg_game_starts" | "wf_trg_game_ends" | "wf_xtra_unseen" => {
            cfg(vec![])
        }
        "wf_trg_bot_reached_stf" => {
            let sel = if json {
                x.ids("itemIds")
            } else {
                x.legacy_ids(':', 1)
            };
            let mut c = cfg(vec![100, 100]);
            c.text = if json { x.s("botName") } else { x.part(':', 0) }.to_string();
            c.selected_items = sel;
            c
        }
        "wf_trg_bot_reached_avtr" => {
            let mut c = cfg(vec![100]);
            c.text = x.s_or_raw("botName").to_string();
            c
        }
        "wf_trg_score_achieved" => cfg(vec![
            if json {
                x.n("score", 0)
            } else {
                x.raw.trim().parse().unwrap_or(0)
            }
            .max(0),
            0,
        ]),

        // ---- effects ----
        "wf_act_toggle_state"
        | "wf_act_toggle_to_rnd"
        | "wf_act_chase"
        | "wf_act_flee"
        | "wf_act_call_stacks" => {
            let sel = ids_or("itemIds", '\t', 1);
            let max = if matches!(name, "wf_act_call_stacks" | "wf_act_chase" | "wf_act_flee") {
                3600
            } else {
                20
            };
            let src = x.source("furniSource", 100, !sel.is_empty());
            let mut c = cfg(if name == "wf_act_toggle_state" {
                vec![0, src]
            } else {
                vec![src]
            });
            c.selected_items = sel;
            c.delay = delay(max);
            c
        }
        "wf_act_reset_timers" => {
            let mut c = cfg(vec![]);
            c.delay = if json {
                x.delay(3600)
            } else {
                x.raw.trim().parse::<i64>().unwrap_or(0)
            };
            c
        }
        "wf_act_teleport_to" => {
            let sel = ids_or("itemIds", '\t', 1);
            let mut c = cfg(vec![
                x.b("fastTeleport"),
                x.source("furniSource", 100, !sel.is_empty()),
                x.n("userSource", 0),
            ]);
            c.selected_items = sel;
            c.delay = delay(20);
            c
        }
        "wf_act_match_to_sshot" => {
            if !json {
                return None;
            }
            let (sel, snaps) = x.snapshots("items");
            let mut c = cfg(vec![
                x.b("state"),
                x.b("direction"),
                x.b("position"),
                x.b("altitude"),
                100,
            ]);
            c.selected_items = sel;
            c.snapshots = snaps;
            c.delay = x.delay(20);
            c
        }
        "wf_act_move_rotate" => {
            let (dir, rot, sel, d) = if json {
                (
                    x.n("direction", 0),
                    x.n("rotation", 0),
                    x.ids("itemIds"),
                    x.delay(20),
                )
            } else {
                (
                    x.part_n('\t', 0, 0),
                    x.part_n('\t', 1, 0),
                    x.legacy_ids('\t', 3),
                    legacy_delay('\t', 2, 20),
                )
            };
            let dir = match dir {
                1 => 8,
                2 => 9,
                3 => 10,
                4 => 4,
                5 => 2,
                6 => 0,
                7 => 6,
                _ => -1,
            };
            let rot = match rot {
                1 => 2,
                2 => 4,
                3 => 6,
                _ => 0,
            };
            let mut c = cfg(vec![
                dir,
                rot,
                x.source("furniSource", 100, !sel.is_empty()),
                0,
            ]);
            c.selected_items = sel;
            c.delay = d;
            c
        }
        "wf_act_move_furni_to" => {
            let (dir, sel, d) = if json {
                (x.n("direction", 0), x.ids("itemIds"), x.delay(20))
            } else {
                (
                    x.part_n('\t', 0, 0),
                    x.legacy_ids('\t', 3),
                    legacy_delay('\t', 2, 20),
                )
            };
            let mut c = cfg(vec![(dir) & !1, x.n("spacing", 1), 100]);
            c.selected_items = sel;
            c.delay = d;
            c
        }
        "wf_act_move_to_dir" => {
            if !json {
                return None;
            }
            let start = match x.json.as_ref().and_then(|j| j.get("start_direction")) {
                Some(J::String(n)) => [
                    "NORTH",
                    "NORTH_EAST",
                    "EAST",
                    "SOUTH_EAST",
                    "SOUTH",
                    "SOUTH_WEST",
                    "WEST",
                    "NORTH_WEST",
                ]
                .iter()
                .position(|d| d.eq_ignore_ascii_case(n))
                .unwrap_or(0) as i64,
                _ => x.n("start_direction", 0),
            };
            let sel = x.ids("items");
            let mut c = cfg(vec![start, x.n("blocked_action", 0).clamp(0, 6), 100, 1]);
            c.selected_items = sel;
            c.delay = x.delay(3600);
            c
        }
        "wf_act_give_score" | "wf_act_give_score_tm" => {
            let tm = name.ends_with("_tm");
            let (score, count, team, d) = if json {
                (
                    x.n("score", 1),
                    x.n("count", 1),
                    x.team("team"),
                    x.delay(3600),
                )
            } else if tm {
                (
                    x.part_n(';', 0, 1),
                    x.part_n(';', 1, 1),
                    x.part_n(';', 2, 1),
                    legacy_delay(';', 3, 3600),
                )
            } else {
                (
                    x.part_n(';', 0, 1),
                    x.part_n(';', 1, 1),
                    0,
                    legacy_delay(';', 2, 3600),
                )
            };
            let mut c = cfg(vec![score, 0, if tm { team } else { 0 }]);
            c.score_quota_per_game = Some(count);
            c.delay = d;
            c
        }
        "wf_act_show_message" | "wf_act_alert" => {
            let msg = if json {
                x.s("message")
            } else {
                x.part('\t', 1)
            };
            let mut c = cfg(vec![
                x.n("userSource", 0),
                x.n("visibilitySelection", 0),
                x.n("bubbleStyle", 34),
                x.n("bubbleWidthOverride", -1),
            ]);
            c.text = placeholders(&msg);
            c.delay = delay(3600);
            c
        }
        "wf_act_kick_user" => {
            let mut c = cfg(vec![x.n("userSource", 0)]);
            c.text = placeholders(&if json {
                x.s("message")
            } else {
                x.part('\t', 1)
            });
            c.delay = delay(3600);
            c
        }
        "wf_act_mute_triggerer" => {
            let len = if json {
                x.n("length", 1)
            } else {
                x.part_n('\t', 1, 1)
            };
            let mut c = cfg(vec![len, x.n("userSource", 0)]);
            c.text = placeholders(&if json {
                x.s("message")
            } else {
                x.part('\t', 2)
            });
            c.delay = delay(3600);
            c
        }
        "wf_act_join_team" => {
            let team = if json {
                x.team("team")
            } else {
                x.part_n('\t', 1, 1)
            };
            let mut c = cfg(vec![0, team, x.n("userSource", 0), 0]);
            c.delay = delay(3600);
            c
        }
        "wf_act_leave_team" => {
            let mut c = cfg(vec![x.n("userSource", 0)]);
            c.delay = if json {
                x.delay(3600)
            } else {
                x.raw.trim().parse::<i64>().unwrap_or(0)
            };
            c
        }
        "wf_act_give_reward" => return give_reward(x),
        "wf_act_bot_teleport" | "wf_act_bot_move" => {
            let (bot, sel) = if json {
                (x.s("bot_name"), x.ids("items"))
            } else {
                let p = x.part('\t', 1);
                let mut it = p.split(';');
                (
                    it.next().unwrap_or("").to_string(),
                    x.keep(split_ids(&it.collect::<Vec<_>>().join(";"))),
                )
            };
            let mut c = cfg(vec![100, 100]);
            c.text = bot.to_string();
            c.selected_items = sel;
            c.delay = delay(3600);
            c
        }
        "wf_act_bot_talk" | "wf_act_bot_talk_to_avatar" => {
            let (mode, bot, msg) = if json {
                (x.n("mode", 0), x.s("bot_name"), x.s("message"))
            } else {
                (x.part_n('\t', 1, 0), x.part('\t', 2), x.part('\t', 3))
            };
            let mut c = cfg(if name == "wf_act_bot_talk" {
                vec![mode.clamp(0, 1), 100, -1]
            } else {
                vec![mode.clamp(0, 1), 0, 100, -1]
            });
            c.text = format!("{}\t{}", bot, placeholders(&msg));
            c.delay = delay(3600);
            c
        }
        "wf_act_bot_give_handitem" => {
            let (hand, bot) = if json {
                (x.n("item_id", 0), x.s("bot_name"))
            } else {
                (x.part_n('\t', 1, 0), x.part('\t', 2))
            };
            let mut c = cfg(vec![hand.max(0), 0, 100]);
            c.text = bot.to_string();
            c.delay = delay(3600);
            c
        }
        "wf_act_bot_follow_avatar" => {
            let (mode, bot) = if json {
                (x.n("mode", 1), x.s("bot_name"))
            } else {
                (x.part_n('\t', 1, 1), x.part('\t', 2))
            };
            let mut c = cfg(vec![mode.clamp(0, 1), 0, 100]);
            c.text = bot.to_string();
            c.delay = delay(3600);
            c
        }
        "wf_act_bot_clothes" => {
            let (bot, look) = if json {
                (x.s("bot_name"), x.s("look"))
            } else {
                (x.part('\t', 1), x.part('\t', 2))
            };
            let mut c = cfg(vec![100]);
            c.text = format!("{}\t{}", bot, look);
            c.delay = delay(3600);
            c
        }

        // ---- conditions ----
        "wf_cnd_has_furni_on" | "wf_cnd_not_furni_on" => {
            let (all, sel) = if json {
                (x.b("all"), x.ids("itemIds"))
            } else {
                ((x.part(':', 0) == "1") as i64, x.legacy_ids(':', 1))
            };
            let flag = if name == "wf_cnd_not_furni_on" {
                1 - all
            } else {
                all
            };
            let mut c = cfg(vec![flag, x.source("furniSource", 100, !sel.is_empty())]);
            c.selected_items = sel;
            c
        }
        "wf_cnd_furnis_hv_avtrs" | "wf_cnd_not_hv_avtrs" => {
            let sel = if json {
                x.ids("itemIds")
            } else {
                x.legacy_ids(':', 1)
            };
            let mut c = cfg(vec![
                if name == "wf_cnd_furnis_hv_avtrs" {
                    1
                } else {
                    0
                },
                x.source("furniSource", 100, !sel.is_empty()),
            ]);
            c.selected_items = sel;
            c
        }
        "wf_cnd_stuff_is" | "wf_cnd_not_stuff_is" => {
            let sel = if json {
                x.ids("itemIds")
            } else {
                x.legacy_ids('\u{0}', 0)
            };
            let mut c = cfg(vec![0, 100, 0]);
            c.text = sel
                .iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(";");
            c.secondary_selected_items = sel.clone();
            c.selected_items = sel;
            c
        }
        "wf_cnd_trggrer_on_frn" | "wf_cnd_not_trggrer_on" => {
            let sel = if json {
                x.ids("itemIds")
            } else {
                x.legacy_ids('\u{0}', 0)
            };
            let mut c = cfg(vec![
                x.source("furniSource", 100, !sel.is_empty()),
                x.n("userSource", 0),
                x.n("quantifier", 0),
            ]);
            c.selected_items = sel;
            c
        }
        "wf_cnd_actor_in_group" | "wf_cnd_not_in_group" => cfg(vec![
            x.n("userSource", 0),
            x.n("groupType", 0),
            x.n("selectedGroupId", 0),
            x.n("quantifier", 0),
        ]),
        "wf_cnd_user_count_in" | "wf_cnd_not_user_count" => {
            let (lo, hi) = if json {
                (x.n("lowerLimit", 0), x.n("upperLimit", 50))
            } else {
                (x.part_n(':', 0, 0), x.part_n(':', 1, 50))
            };
            let lo = lo.max(0);
            cfg(vec![lo, hi.max(lo), x.n("userSource", 0)])
        }
        "wf_cnd_wearing_effect" | "wf_cnd_not_wearing_fx" => {
            let fx = if json {
                x.n("effectId", 0)
            } else {
                x.raw.trim().parse().unwrap_or(0)
            };
            cfg(vec![fx.max(0), x.n("userSource", 0), x.n("quantifier", 1)])
        }
        "wf_cnd_wearing_badge" | "wf_cnd_not_wearing_b" => {
            let mut c = cfg(vec![x.n("userSource", 0), x.n("quantifier", 0)]);
            c.text = (x.s_or_raw("badge").trim()).to_string();
            c
        }
        "wf_cnd_time_less_than" | "wf_cnd_time_more_than" => cfg(vec![
            if json {
                x.n("cycles", 0)
            } else {
                x.raw.trim().parse().unwrap_or(0)
            }
            .max(0),
        ]),
        "wf_cnd_match_snapshot" | "wf_cnd_not_match_snap" => {
            if !json {
                return None;
            }
            let (sel, snaps) = x.snapshots("settings");
            let mut c = cfg(vec![
                x.b("state"),
                x.b("direction"),
                x.b("position"),
                x.b("altitude"),
                100,
                x.n("quantifier", 0),
            ]);
            c.selected_items = sel;
            c.snapshots = snaps;
            c
        }
        "wf_cnd_actor_in_team" | "wf_cnd_not_in_team" => {
            let team = if json {
                x.team("teamColor")
            } else {
                x.raw.trim().parse::<i64>().unwrap_or(0)
            };
            cfg(vec![team, x.n("userSource", 0), x.n("quantifier", 1)])
        }
        "wf_cnd_has_handitem" => cfg(vec![
            if json {
                x.n("handItemId", 0)
            } else {
                x.raw.trim().parse().unwrap_or(0)
            }
            .max(0),
            x.n("userSource", 0),
            x.n("quantifier", 1),
        ]),
        "wf_cnd_date_rng_active" => {
            let (a, b) = if json {
                (x.n("startDate", 0), x.n("endDate", 0))
            } else {
                (x.part_n('\t', 0, 0), x.part_n('\t', 1, 0))
            };
            let (a, b) = (a.max(0), b.max(0));
            cfg(vec![a, if b > 0 && b < a { a } else { b }])
        }

        // ---- addons ----
        "wf_xtra_random" => cfg(vec![1, 0]),
        "wf_xtra_or_eval" => cfg(vec![1, 0, 1]),
        _ => return None,
    };
    Some(c)
}

/// Rewards: badges and furni carry over; currency/respect prizes cannot.
fn give_reward(x: &Src) -> Option<Config> {
    x.json.as_ref()?;
    let maps = x.ctx.maps();
    let mut entries = vec![];
    if let Some(J::Array(list)) = x.json.as_ref().and_then(|j| j.get("rewards")) {
        for r in list.iter() {
            let p = r.get("probability").and_then(|v| v.as_i64()).unwrap_or(100);
            let data = r.get("data").and_then(|v| v.as_str()).unwrap_or("");
            if r.get("badge").and_then(|v| v.as_bool()).unwrap_or(false) {
                entries.push(format!("0,{},{p}", (data)));
            } else if let Some(base) = data
                .strip_prefix("furni#")
                .and_then(|v| v.parse::<i64>().ok())
            {
                if let Some(dst) = maps.furni_id(base) {
                    entries.push(format!("1,furni#{dst},{p}"));
                }
            } else {
                DROPPED_PRIZES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    let interval = x.n("reward_time", 0).clamp(0, 3);
    let n = if matches!(interval, 1 | 2) {
        x.n("limit_interval", 1)
    } else {
        1
    };
    let mut c = cfg(vec![
        interval,
        x.b("unique_rewards"),
        x.n("limit", 0),
        n,
        x.n("userSource", 0),
    ]);
    c.text = entries.join(";");
    c.delay = x.delay(3600);
    Some(c)
}

/// Polaris-only boxes whose JSON fields copy straight into IntParams.
fn decode_spec(spec: &Spec, x: &Src) -> Option<Config> {
    x.json.as_ref()?;
    let sel = spec
        .sel
        .as_deref()
        .map(|f| x.ids(f.split('.').next().unwrap_or(f)))
        .unwrap_or_default();
    let ints = spec
        .ints
        .iter()
        .map(|p| match (&p.f, p.c) {
            (_, Some(c)) => c,
            (Some(f), _) => {
                let v = match p.i {
                    Some(idx) => x
                        .json
                        .as_ref()
                        .and_then(|j| j.get(f))
                        .and_then(|a| a.get(idx))
                        .and_then(|v| v.as_i64())
                        .unwrap_or(p.d.unwrap_or(0)),
                    None => x.n(f, p.d.unwrap_or(0)),
                };
                let v = if p.b.is_some() { (v != 0) as i64 } else { v };
                if v == 0 && f.to_ascii_lowercase().ends_with("furnisource") && !sel.is_empty() {
                    100
                } else {
                    v
                }
            }
            _ => 0,
        })
        .collect();
    let mut c = cfg(ints);
    c.text = spec.text.as_deref().map(|f| x.s(f)).unwrap_or_default();
    c.secondary_selected_items = spec.sec.as_deref().map(|f| x.ids(f)).unwrap_or_default();
    c.selected_items = sel;
    c.delay = spec.delay.map(|max| x.delay(max)).unwrap_or(0);
    Some(c)
}

pub async fn run(ctx: Arc<Ctx>) -> Result<()> {
    DROPPED_PRIZES.store(0, std::sync::atomic::Ordering::Relaxed);
    let maps = ctx.maps();
    let boxes = query_rows(
        &ctx.src,
        "SELECT i.id, i.room_id, i.item_id, i.wired_data FROM items i JOIN users u ON u.id = i.user_id WHERE i.wired_data <> ''",
    )
    .await?;
    let rooms: HashSet<i64> = boxes
        .iter()
        .map(|r| r.i64("room_id"))
        .filter(|r| *r > 0)
        .collect();

    let mut items: HashMap<i64, RoomItem> = HashMap::new();
    if !rooms.is_empty() {
        let list = rooms
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let mut rows = RowStream::open(
            &ctx.src,
            &format!(
                "SELECT id, room_id, item_id, z, extra_data FROM items WHERE room_id IN ({list})"
            ),
        )
        .await?;
        while let Some(r) = rows.next().await? {
            if maps.furni_id(r.i64("item_id")).is_some() {
                items.insert(
                    r.i64("id"),
                    RoomItem {
                        room: r.i64("room_id"),
                        base: r.i64("item_id"),
                        z: r.f64("z"),
                        extra: r.text("extra_data"),
                        wall: maps.furni(r.i64("item_id")).is_some_and(|d| d.kind == "i"),
                    },
                );
            }
        }
    }

    let w = ctx
        .writer(
            "wired_item_configurations",
            &["item_id", "box_name", "schema_version", "configuration"],
        )
        .await?;
    let mut unconverted: HashMap<String, usize> = HashMap::new();
    let mut rewards: HashMap<i64, HashMap<i64, String>> = HashMap::new();
    for r in &boxes {
        let Some(def) = maps.furni(r.i64("item_id")) else {
            continue;
        };
        let Some(target) = def.wired_box else {
            continue;
        };
        let raw = r.str("wired_data");
        if raw.trim_start().starts_with('{') && serde_json::from_str::<J>(&raw).is_err() {
            ctx.note(format!(
                "Wired item {}: malformed JSON; configuration not replaced with defaults",
                r.i64("id")
            ));
            continue;
        }
        let x = Src {
            json: raw
                .trim_start()
                .starts_with('{')
                .then(|| serde_json::from_str(&raw).ok())
                .flatten(),
            raw: &raw,
            room: r.i64("room_id"),
            items: &items,
            ctx: &ctx,
        };
        let src_name = def.src_interaction.as_str();
        let config = match (ctx.flavor, SPECS.get(src_name)) {
            (Flavor::Polaris, Some(spec)) => decode_spec(spec, &x),
            _ => decode_known(src_name, &x),
        };
        let Some(config) = config else {
            *unconverted.entry(src_name.to_string()).or_default() += 1;
            continue;
        };
        if target == "wf_act_give_reward" {
            let mut codes = HashMap::new();
            if let Some(list) = x
                .json
                .as_ref()
                .and_then(|j| j.get("rewards"))
                .and_then(J::as_array)
            {
                for reward in list {
                    let id = reward.get("id").and_then(J::as_i64).unwrap_or(0);
                    let raw = reward.get("data").and_then(J::as_str).unwrap_or("");
                    let code = if reward.get("badge").and_then(J::as_bool).unwrap_or(false) {
                        Some(raw.to_string())
                    } else {
                        raw.strip_prefix("furni#")
                            .and_then(|s| s.parse().ok())
                            .and_then(|id| maps.furni_id(id))
                            .map(|id| format!("furni#{id}"))
                    };
                    if let Some(code) = code
                        && codes.insert(id, code).is_some()
                    {
                        ctx.note(format!(
                            "Wired reward item {} has ambiguous duplicate prize ids",
                            r.i64("id")
                        ));
                    }
                }
            }
            rewards.insert(r.i64("id"), codes);
        }
        let native = match native::convert(target, config, |id| {
            items.get(&id).is_some_and(|it| it.wall)
        }) {
            Ok(native) => native,
            Err(error) => {
                ctx.note(format!("Wired item {} ({src_name}): {error}", r.i64("id")));
                continue;
            }
        };
        let serialized = serde_json::to_value(&native)?;
        if ctx.validate_only {
            ctx.wired_validation
                .lock()
                .unwrap()
                .push(json!({"item_id":r.i64("id"),"box_name":target,"configuration":serialized}));
        }
        w.sink
            .send(vec![
                i(r.i64("id")),
                s(target),
                i(2),
                s(serialized.to_string()),
            ])
            .await?;
    }
    w.finish().await?;
    if !unconverted.is_empty() {
        let list: Vec<String> = unconverted
            .iter()
            .map(|(k, n)| format!("{k} ({n})"))
            .collect();
        ctx.note(format!(
            "WIRED CONFIGURATION BLOCKERS (cannot translate): {}",
            list.join(", ")
        ));
    }

    let dropped = DROPPED_PRIZES.load(std::sync::atomic::Ordering::Relaxed);
    if dropped > 0 {
        ctx.note(format!("{dropped} wired reward prizes were currency/respect, which Plus rewards cannot give; they were left out"));
    }
    room_settings(&ctx).await?;
    reward_claims(&ctx, &rewards).await
}

/// Polaris per-room wired permissions. A Polaris room without a row is owner
/// only, while Plus would let rights holders edit, so those get explicit 0/0.
async fn room_settings(ctx: &Ctx) -> Result<()> {
    if ctx.flavor != Flavor::Polaris {
        return Ok(());
    }
    let mut set: HashMap<i64, (i64, i64, String)> = ctx
        .maps()
        .rooms
        .iter()
        .map(|r| (*r, (0, 0, String::new())))
        .collect();
    if ctx.has_src("room_wired_settings") {
        for r in query_rows(&ctx.src, "SELECT * FROM room_wired_settings").await? {
            let tz = r.str("timezone");
            let tz = if tz == "UTC" || tz.contains('/') {
                tz
            } else {
                String::new()
            };
            set.insert(
                r.i64("room_id"),
                (r.i64("inspect_mask") & 15, r.i64("modify_mask") & 14, tz),
            );
        }
    }
    let w = ctx
        .writer(
            "room_wired_settings",
            &["room_id", "inspect_mask", "modify_mask", "timezone"],
        )
        .await?;
    for (room, (inspect, modify, tz)) in set {
        if ctx.maps().room_models.contains_key(&room) {
            w.sink
                .send(vec![i(room), i(inspect | modify), i(modify), s(tz)])
                .await?;
        }
    }
    w.finish().await?;
    Ok(())
}

/// wired_rewards_given -> wired_reward_state, so once-only rewards stay claimed.
async fn reward_claims(ctx: &Ctx, rewards: &HashMap<i64, HashMap<i64, String>>) -> Result<()> {
    if !ctx.has_src("wired_rewards_given") {
        return Ok(());
    }
    // Stream individual claims: GROUP_CONCAT silently truncates a large reward history.
    let mut history: HashMap<(i64, i64), (i64, i64, Vec<String>)> = HashMap::new();
    let mut missing = 0;
    let mut rows = RowStream::open(&ctx.src, "SELECT wired_item,user_id,reward_id,timestamp FROM wired_rewards_given ORDER BY wired_item,user_id,timestamp,reward_id").await?;
    while let Some(r) = rows.next().await? {
        let key = (r.i64("wired_item"), r.i64("user_id"));
        let Some(code) = rewards
            .get(&key.0)
            .and_then(|codes| codes.get(&r.i64("reward_id")))
        else {
            missing += 1;
            continue;
        };
        if !ctx.maps().user_names.contains_key(&key.1) {
            missing += 1;
            continue;
        }
        let entry = history.entry(key).or_default();
        entry.0 += 1;
        entry.1 = entry.1.max(r.i64("timestamp"));
        entry.2.push(code.clone());
    }
    if missing > 0 {
        ctx.note(format!("{missing} Wired reward claims have no migrated prize/user; repeat-claim protection is incomplete"));
    }
    let mut claims: HashMap<i64, serde_json::Map<String, J>> = HashMap::new();
    for ((item, user), (count, timestamp, received)) in history {
        let at = chrono::DateTime::from_timestamp(timestamp, 0)
            .unwrap_or_default()
            .to_rfc3339();
        claims.entry(item).or_default().insert(
            user.to_string(),
            json!({ "Count": count, "LastClaimAt": at, "ReceivedCodes": received }),
        );
    }
    let w = ctx
        .writer("wired_reward_state", &["item_id", "claims"])
        .await?;
    for (item, map) in claims {
        w.sink
            .send(vec![i(item), s(J::Object(map).to_string())])
            .await?;
    }
    w.finish().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steps::prep::{FurniDef, Maps};
    use std::sync::{Mutex, OnceLock};
    fn context() -> Ctx {
        let mut maps = Maps::default();
        maps.furni.insert(
            1001,
            FurniDef {
                dst_id: 2001,
                kind: "s".into(),
                item_name: "test".into(),
                sprite_id: 1,
                src_interaction: "default".into(),
                interaction: "default".into(),
                behaviour_data: 0,
                modes: 2,
                wired_box: None,
                fell_back: false,
            },
        );
        let lock = OnceLock::new();
        lock.set(maps).unwrap();
        Ctx {
            interaction_coverage: OnceLock::new(),
            validate_only: true,
            source_counts: OnceLock::new(),
            wired_validation: Mutex::new(vec![]),
            src: crate::db::pool("mysql://localhost/source", 1).unwrap(),
            dst: crate::db::pool("mysql://localhost/target", 1).unwrap(),
            flavor: Flavor::Arcturus,
            src_tables: HashSet::new(),
            dst_schema: Default::default(),
            furnidata: None,
            maps: lock,
            notes: Mutex::new(vec![]),
        }
    }
    fn sample() -> J {
        serde_json::from_str(r#"{"itemIds":[101],"items":[{"item_id":101,"state":"1","rotation":2,"x":3,"y":4}],
            "settings":[{"item_id":101,"state":"1","rotation":2,"x":3,"y":4}],
            "state":true,"direction":true,"position":true,"delay":2,"rotation":0,
            "ownerOnly":true,"key":"hello","username":"Example","repeatTime":10000,"executeTime":10000,
            "botName":"Helper","bot_name":"Helper","score":5,"count":2,"team":"RED","teamColor":"RED",
            "length":1,"message":"Hello %user%","look":"hd-180-1","item_id":2,"mode":1,
            "all":true,"lowerLimit":2,"upperLimit":40,"effectId":1,"badge":"TEST","cycles":4,"handItemId":2,
            "startDate":1700000000,"endDate":1900000000,"start_direction":"EAST","blocked_action":2,
            "rewards":[{"id":7,"badge":true,"data":"TEST","probability":50},{"id":9,"badge":false,"data":"furni#1001","probability":50}],
            "reward_time":0,"unique_rewards":true,"limit":25,"limit_interval":1}"#).unwrap()
    }
    #[test]
    fn every_standard_decoder_produces_native_authority() {
        let ctx = context();
        let items = HashMap::from([(
            101,
            RoomItem {
                room: 7,
                base: 1001,
                z: 1.5,
                extra: "0".into(),
                wall: false,
            },
        )]);
        let data = sample();
        let raw = data.to_string();
        let x = Src {
            json: Some(data),
            raw: &raw,
            room: 7,
            items: &items,
            ctx: &ctx,
        };
        let mut lines = Vec::new();
        let mut names: Vec<_> = BOXES["arcturus"].iter().collect();
        names.sort();
        for (source, target) in names {
            let c = decode_known(source, &x).unwrap_or_else(|| panic!("missing decoder {source}"));
            let native =
                native::convert(target, c, |_| false).unwrap_or_else(|e| panic!("{source}: {e}"));
            let native = serde_json::to_value(native).unwrap();
            assert_eq!(native["Version"], 2, "{source}");
            assert!(
                native.get("IntParams").is_none(),
                "runtime fields must not be persisted"
            );
            lines.push(json!({"item_id":1,"box_name":target,"configuration":native}).to_string());
        }
        if let Ok(path) = std::env::var("PLUS_MIGRATE_CONTRACT_FIXTURES") {
            std::fs::write(path, lines.join("\n") + "\n").unwrap();
        }
    }
    #[test]
    fn arcturus_south_and_north_are_not_swapped() {
        let ctx = context();
        let items = HashMap::new();
        for (source, destination) in [
            (0, -1),
            (1, 8),
            (2, 9),
            (3, 10),
            (4, 4),
            (5, 2),
            (6, 0),
            (7, 6),
        ] {
            let x = Src {
                json: Some(json!({"direction":source,"rotation":0,"delay":0,"itemIds":[]})),
                raw: "",
                room: 7,
                items: &items,
                ctx: &ctx,
            };
            assert_eq!(
                decode_known("wf_act_move_rotate", &x).unwrap().int_params[0],
                destination
            );
        }
    }
    #[test]
    fn native_uses_owned_fields_and_source_footers() {
        let mut c = cfg(vec![1, 100, 0]);
        c.selected_items = vec![101];
        c.delay = 3;
        let v = serde_json::to_value(native::convert("wf_act_teleport_to", c, |_| false).unwrap())
            .unwrap();
        assert_eq!(v["OwnedIntParams"], json!([1]));
        assert_eq!(v["FurniSourceTypes"], json!([100]));
        assert_eq!(v["UserSourceTypes"], json!([0]));
        assert_eq!(v["Delay"], 3);
        assert_eq!(v["PrimaryItems"], json!([{"ItemId":101,"Wall":false}]));
    }
    #[test]
    fn snapshot_ids_remap_definitions_without_recapturing_state() {
        let ctx = context();
        let items = HashMap::from([(
            101,
            RoomItem {
                room: 7,
                base: 1001,
                z: 1.5,
                extra: "changed".into(),
                wall: false,
            },
        )]);
        let x = Src {
            json: Some(sample()),
            raw: "",
            room: 7,
            items: &items,
            ctx: &ctx,
        };
        let (_, snapshots) = x.snapshots("items");
        assert_eq!(snapshots[0].definition_id, 2001);
        assert_eq!(snapshots[0].item_id, 101);
        assert_eq!(snapshots[0].state, "1");
        assert_eq!(snapshots[0].x, 3);
        assert_eq!(snapshots[0].z, 1.5);
    }
    #[test]
    fn source_skip_marker_is_only_effect_metadata() {
        let ctx = context();
        let items = HashMap::from([(
            101,
            RoomItem {
                room: 7,
                base: 1001,
                z: 0.0,
                extra: "changed".into(),
                wall: false,
            },
        )]);
        let mut data = sample();
        data["items"][0]["state"] = json!(" ");
        data["settings"][0]["state"] = json!(" ");
        let x = Src {
            json: Some(data),
            raw: "",
            room: 7,
            items: &items,
            ctx: &ctx,
        };
        let (_, effect) = x.snapshots("items");
        let (_, condition) = x.snapshots("settings");
        assert!(effect[0].skip_state_restore);
        assert!(!condition[0].skip_state_restore);
        assert_eq!(effect[0].state, " ");
    }
}
