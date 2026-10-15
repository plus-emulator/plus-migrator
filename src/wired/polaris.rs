//! Reviewed Polaris JSON -> Plus schema-2 editor records. These conversions use
//! the source serializers, not the similarly named Arcturus runtime parameters.
//! Unknown fields and unrepresentable values are blockers, never defaults.
use super::{Src, native};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;

// These serializers are recognized, but target runtime semantics differ.
pub(super) const BLOCKED: &[&str] = &["wf_slc_remote", "wf_slc_users_group"];

pub(super) const NAMES: &[&str] = &[
    "wf_slc_furni_picks",
    "wf_slc_furni_bytype",
    "wf_slc_furni_onfurni",
    "wf_slc_users_onfurni",
    "wf_slc_furni_area",
    "wf_slc_users_area",
    "wf_slc_furni_altitude",
    "wf_slc_furni_signal",
    "wf_slc_users_signal",
    "wf_slc_users_byname",
    "wf_slc_users_bytype",
    "wf_slc_users_handitem",
    "wf_slc_users_team",
    "wf_slc_users_byaction",
    "wf_trg_click_furni",
    "wf_trg_click_tile",
    "wf_trg_click_user",
    "wf_trg_recv_signal",
    "wf_trg_clock_counter",
    "wf_trg_leave_room",
    "wf_trg_user_performs_action",
    "wf_act_control_clock",
    "wf_act_adjust_clock",
    "wf_act_set_altitude",
    "wf_act_send_signal",
    "wf_act_neg_send_signal",
    "wf_act_freeze",
    "wf_act_unfreeze",
    "wf_act_furni_to_furni",
    "wf_act_user_to_furni",
    "wf_xtra_anim_time",
    "wf_xtra_execution_limit",
    "wf_xtra_mov_carry_users",
    "wf_xtra_mov_no_animation",
    "wf_xtra_exec_in_order",
    "wf_cnd_actor_dir",
    "wf_cnd_has_altitude",
    "wf_cnd_team_has_rank",
    "wf_cnd_team_has_score",
    "wf_cnd_valid_moves",
    "wf_cnd_user_performs_action",
    "wf_cnd_not_user_performs_action",
    "wf_cnd_stuff_is",
    "wf_cnd_not_stuff_is",
];

struct Data<'a> {
    value: &'a Value,
    used: RefCell<HashSet<String>>,
}
impl Data<'_> {
    fn field(&self, key: &str) -> Result<&Value> {
        self.used.borrow_mut().insert(key.to_owned());
        self.value
            .get(key)
            .with_context(|| format!("missing Polaris field {key}"))
    }
    fn int(&self, key: &str) -> Result<i64> {
        let n = self
            .field(key)?
            .as_i64()
            .with_context(|| format!("{key} must be an integer"))?;
        ensure!(
            i32::try_from(n).is_ok(),
            "{key} exceeds the target integer range"
        );
        Ok(n)
    }
    fn flag(&self, key: &str) -> Result<bool> {
        self.field(key)?
            .as_bool()
            .with_context(|| format!("{key} must be a boolean"))
    }
    fn text(&self, key: &str) -> Result<String> {
        self.field(key)?
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("{key} must be text"))
    }
    fn zero_if_present(&self, key: &str) -> Result<()> {
        if self.value.get(key).is_some() {
            ensure!(
                self.int(key)? == 0,
                "nonzero {key} has no reviewed Plus equivalent"
            );
        }
        Ok(())
    }
    fn picks(&self, key: &str, x: &Src) -> Result<Value> {
        let values = self
            .field(key)?
            .as_array()
            .with_context(|| format!("{key} must be an item list"))?;
        let mut seen = HashSet::new();
        let mut refs = vec![];
        for value in values {
            let id = value
                .as_i64()
                .context("picked item id must be an integer")?;
            ensure!(
                id > 0 && u32::try_from(id).is_ok(),
                "invalid picked item id"
            );
            let item = x
                .items
                .get(&id)
                .filter(|i| i.room == x.room)
                .context("picked item is missing or belongs to a different room")?;
            if seen.insert(id) {
                refs.push(json!({"ItemId": id, "Wall": item.wall}));
            }
        }
        Ok(json!(refs))
    }
    fn finish(&self) -> Result<()> {
        let used = self.used.borrow();
        let unknown: Vec<_> = self
            .value
            .as_object()
            .context("Polaris configuration must be an object")?
            .keys()
            .filter(|k| !used.contains(*k))
            .cloned()
            .collect();
        ensure!(
            unknown.is_empty(),
            "unreviewed Polaris fields: {}",
            unknown.join(", ")
        );
        Ok(())
    }
    // Preserve exact hundredths; do not round a source altitude into a new value.
    fn altitude(&self) -> Result<i64> {
        let s = self.text("altitude")?;
        let (whole, frac) = s.split_once('.').unwrap_or((&s, ""));
        ensure!(
            !whole.is_empty()
                && whole.bytes().all(|c| c.is_ascii_digit())
                && frac.bytes().all(|c| c.is_ascii_digit()),
            "invalid altitude"
        );
        ensure!(
            frac.len() <= 2 || frac[2..].bytes().all(|c| c == b'0'),
            "altitude cannot be represented exactly in hundredths"
        );
        let whole: i64 = whole.parse().context("altitude out of range")?;
        ensure!(whole <= 80, "altitude exceeds Plus limit");
        let cents = format!("{:0<2}", &frac[..frac.len().min(2)]).parse::<i64>()?;
        let result = whole * 100 + cents;
        ensure!(result <= 8000, "altitude exceeds Plus limit");
        Ok(result)
    }
    fn action(&self) -> Result<(i64, String)> {
        let action = self.int("selectedAction")?;
        let sign_filter = self.flag("signFilterEnabled")?;
        let sign = self.int("signId")?;
        let dance_filter = self.flag("danceFilterEnabled")?;
        let dance = self.int("danceId")?;
        let code = match action {
            1..=3 => action - 1,
            4..=8 => action,
            9 => 10,
            10 => 11,
            11 => 67,
            _ => bail!("unsupported avatar action"),
        };
        let text = match action {
            9 if sign_filter => {
                ensure!((0..=17).contains(&sign), "invalid sign");
                sign.to_string()
            }
            10 if dance_filter => {
                ensure!((1..=4).contains(&dance), "invalid dance");
                format!("dance {dance}")
            }
            _ => String::new(),
        };
        Ok((code, text))
    }
}

pub(super) fn convert(name: &str, target: &str, x: &Src) -> Result<Value> {
    ensure!(
        !BLOCKED.contains(&name),
        "Polaris selector behavior has no equivalent in the pinned Plus runtime"
    );
    ensure!(
        NAMES.contains(&name),
        "no reviewed Polaris converter for {name}"
    );
    let d = Data {
        value: x
            .json
            .as_ref()
            .context("this Polaris converter requires JSON")?,
        used: RefCell::new(HashSet::new()),
    };
    ensure!(
        d.value.is_object(),
        "Polaris configuration must be an object"
    );
    let (category, code) = native::identity(target)?;
    let mut n = json!({
        "Version":2,"Category":category,"NativeCode":code,"OwnedIntParams":[],"Text":"",
        "PrimaryItems":[],"SecondaryItems":[],"FurniSourceTypes":[],"UserSourceTypes":[],
        "VariableIds":[],"Delay":null,"Quantifier":null,"Filter":null,"Inverse":null,
        "SavedState":{"Snapshots":[]}
    });
    if category == 1 {
        n["Delay"] = json!(d.int("delay")?);
    }
    if category == 2 {
        n["Quantifier"] = json!(0);
    }
    if category == 3 {
        n["Filter"] = json!(d.flag("filterExisting")?);
        n["Inverse"] = json!(d.flag("invert")?);
        // Source selectors execute during selection, not the delayed action pass.
        d.zero_if_present("delay")?;
    }
    match name {
        "wf_slc_furni_picks" => n["PrimaryItems"] = d.picks("pickedFurniIds", x)?,
        "wf_slc_furni_signal" | "wf_slc_users_signal" => {}
        "wf_slc_furni_bytype" => {
            let source = match d.int("sourceType")? {
                0 => 100,
                1 => 201,
                2 => 0,
                _ => bail!("unsupported by-type source"),
            };
            n["FurniSourceTypes"] = json!([source]);
            n["OwnedIntParams"] = json!([i64::from(d.flag("matchState")?)]);
            n["PrimaryItems"] = d.picks("pickedFurniIds", x)?;
        }
        "wf_slc_furni_onfurni" | "wf_slc_users_onfurni" => {
            n["FurniSourceTypes"] = json!([d.int("furniSource")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
            if name == "wf_slc_furni_onfurni" {
                n["OwnedIntParams"] = json!([d.int("selectionType")?]);
            }
        }
        "wf_slc_furni_area" | "wf_slc_users_area" => {
            n["OwnedIntParams"] = json!([
                d.int("rootX")?,
                d.int("rootY")?,
                d.int("width")?,
                d.int("height")?
            ]);
        }
        "wf_slc_furni_altitude" => {
            n["OwnedIntParams"] = json!([d.altitude()?, d.int("comparison")?])
        }
        "wf_slc_users_byname" => n["Text"] = json!(d.text("namesText")?),
        "wf_slc_users_bytype" => n["OwnedIntParams"] = json!([d.int("entityType")?]),
        "wf_slc_users_handitem" => n["OwnedIntParams"] = json!([d.int("handItemId")?]),
        "wf_slc_users_team" => n["OwnedIntParams"] = json!([d.int("teamType")?]),
        "wf_slc_users_byaction"
        | "wf_trg_user_performs_action"
        | "wf_cnd_user_performs_action"
        | "wf_cnd_not_user_performs_action" => {
            let (code, text) = d.action()?;
            n["OwnedIntParams"] = json!([code]);
            n["Text"] = json!(text);
            if category == 2 {
                n["UserSourceTypes"] = json!([d.int("userSource")?]);
                n["Quantifier"] = json!(d.int("quantifier")?);
            }
        }
        "wf_trg_click_furni" | "wf_trg_click_tile" | "wf_trg_recv_signal" => {
            n["FurniSourceTypes"] = json!([d.int("furniSource")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
            if name == "wf_trg_recv_signal" {
                d.zero_if_present("channel")?;
            }
        }
        "wf_trg_click_user" => {
            n["OwnedIntParams"] = json!([
                i64::from(d.flag("blockMenuOpen")?),
                i64::from(d.flag("doNotRotate")?)
            ])
        }
        "wf_trg_leave_room" => n["Text"] = json!(d.text("username")?),
        "wf_trg_clock_counter" | "wf_act_adjust_clock" => {
            let steps = d.int("halfSecondSteps")?;
            ensure!((0..=119).contains(&steps), "invalid half-second steps");
            let mut ints = vec![steps / 2, d.int("minutes")?, steps % 2];
            if category == 1 {
                ints.push(d.int("operator")?);
            }
            n["OwnedIntParams"] = json!(ints);
            n["FurniSourceTypes"] = json!([d.int("furniSource")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
        }
        "wf_act_control_clock" | "wf_act_set_altitude" => {
            n["OwnedIntParams"] = if name == "wf_act_control_clock" {
                json!([d.int("action")?])
            } else {
                json!([d.altitude()?, d.int("operator")?])
            };
            n["FurniSourceTypes"] = json!([d.int("furniSource")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
        }
        "wf_act_send_signal" | "wf_act_neg_send_signal" => {
            d.zero_if_present("channel")?;
            n["OwnedIntParams"] = json!([
                i64::from(d.flag("signalPerFurni")?),
                i64::from(d.flag("signalPerUser")?)
            ]);
            n["FurniSourceTypes"] = json!([d.int("antennaSource")?, d.int("furniForward")?]);
            n["UserSourceTypes"] = json!([d.int("userForward")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
            n["SecondaryItems"] = d.picks("forwardItemIds", x)?;
        }
        "wf_act_freeze" | "wf_act_unfreeze" => {
            n["UserSourceTypes"] = json!([d.int("userSource")?]);
            if name == "wf_act_freeze" {
                let effect = d.int("effectId")?;
                let index = [218, 12, 11, 53, 163]
                    .iter()
                    .position(|&v| v == effect)
                    .context("freeze effect has no Plus equivalent")?;
                n["OwnedIntParams"] = json!([index, i64::from(d.flag("cancelOnTeleport")?)]);
            }
        }
        "wf_act_furni_to_furni" => {
            n["FurniSourceTypes"] = json!([d.int("moveSource")?, d.int("targetSource")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
            n["SecondaryItems"] = d.picks("targetItemIds", x)?;
        }
        "wf_act_user_to_furni" => {
            n["OwnedIntParams"] = json!([d.int("walkMode")?]);
            n["FurniSourceTypes"] = json!([d.int("furniSource")?]);
            n["UserSourceTypes"] = json!([d.int("userSource")?]);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
        }
        "wf_xtra_anim_time" => n["OwnedIntParams"] = json!([d.int("durationMs")?]),
        "wf_xtra_execution_limit" => {
            let ms = d.int("timeWindowMs")?;
            ensure!(
                ms % 500 == 0,
                "execution window cannot be represented in half-second pulses"
            );
            n["OwnedIntParams"] = json!([d.int("maxExecutions")?, ms / 500]);
        }
        "wf_xtra_mov_carry_users" => {
            n["OwnedIntParams"] = json!([d.int("carryMode")?]);
            n["UserSourceTypes"] = json!([d.int("userSource")?]);
        }
        "wf_xtra_mov_no_animation" | "wf_xtra_exec_in_order" | "wf_cnd_valid_moves" => {}
        "wf_cnd_actor_dir" | "wf_cnd_team_has_rank" | "wf_cnd_team_has_score" => {
            n["OwnedIntParams"] = match name {
                "wf_cnd_actor_dir" => json!([d.int("directionMask")?]),
                "wf_cnd_team_has_rank" => json!([d.int("teamType")?, d.int("placement")?]),
                _ => json!([d.int("teamType")?, d.int("score")?, d.int("comparison")?]),
            };
            n["UserSourceTypes"] = json!([d.int("userSource")?]);
            n["Quantifier"] = json!(d.int("quantifier")?);
        }
        "wf_cnd_has_altitude" => {
            n["OwnedIntParams"] = json!([d.altitude()?, d.int("comparison")?]);
            n["FurniSourceTypes"] = json!([d.int("furniSource")?]);
            n["Quantifier"] = json!(d.int("quantifier")?);
            n["PrimaryItems"] = d.picks("itemIds", x)?;
        }
        "wf_cnd_stuff_is" | "wf_cnd_not_stuff_is" => {
            n["FurniSourceTypes"] = json!([d.int("furniSource")?, d.int("compareFurniSource")?]);
            n["Quantifier"] = json!(d.int("quantifier")?);
            n["PrimaryItems"] = d.picks("primaryItemIds", x)?;
            n["SecondaryItems"] = d.picks("secondaryItemIds", x)?;
        }
        _ => unreachable!("reviewed name lacks implementation"),
    }
    d.finish()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wired::{BOXES, RoomItem};
    use std::collections::HashMap;

    fn convert_fixture(name: &str, data: Value) -> Result<Value> {
        let ctx = crate::wired::tests::context();
        let items = HashMap::from([(101, false), (102, false), (103, true)].map(|(id, wall)| {
            (
                id,
                RoomItem {
                    room: 7,
                    base: 1001,
                    z: 0.0,
                    extra: String::new(),
                    wall,
                },
            )
        }));
        let x = Src {
            json: Some(data),
            raw: "",
            room: 7,
            items: &items,
            ctx: &ctx,
        };
        let target = BOXES["polaris"]
            .get(name)
            .context("missing target mapping")?;
        convert(name, target, &x)
    }

    #[test]
    fn every_reviewed_polaris_converter_preserves_editor_fields() {
        let fixtures: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/polaris-wired.json")).unwrap();
        let mut covered = HashSet::new();
        let mut lines = vec![];
        for row in fixtures {
            let name = row["name"].as_str().unwrap();
            let converted = convert_fixture(name, row["source"].clone())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            for (field, value) in row["expected"].as_object().unwrap() {
                assert_eq!(&converted[field], value, "{name}: {field}");
            }
            assert_eq!(converted["Version"], 2);
            covered.insert(name.to_owned());
            lines.push(
                json!({"item_id":1,"box_name":BOXES["polaris"][name],"configuration":converted,"expected_runtime":row.get("expected_runtime")})
                    .to_string(),
            );
        }
        assert_eq!(covered, NAMES.iter().map(|s| s.to_string()).collect());
        if let Ok(path) = std::env::var("PLUS_MIGRATE_POLARIS_FIXTURES") {
            std::fs::write(path, lines.join("\n") + "\n").unwrap();
        }
    }

    #[test]
    fn rejects_unknown_missing_or_mistyped_source_fields() {
        let source =
            json!({"pickedFurniIds":[101],"filterExisting":false,"invert":false,"delay":0});
        for (field, value) in [
            ("custom", json!(1)),
            ("delay", json!(1)),
            ("filterExisting", json!(0)),
            ("pickedFurniIds", json!([999])),
            ("pickedFurniIds", json!([-1])),
        ] {
            let mut changed = source.clone();
            changed[field] = value;
            assert!(
                convert_fixture("wf_slc_furni_picks", changed).is_err(),
                "{field}"
            );
        }
        for field in ["pickedFurniIds", "filterExisting", "invert"] {
            let mut changed = source.clone();
            changed.as_object_mut().unwrap().remove(field);
            assert!(convert_fixture("wf_slc_furni_picks", changed).is_err());
        }
        let wall = convert_fixture(
            "wf_slc_furni_picks",
            json!({"pickedFurniIds":[103,103],"filterExisting":false,"invert":false}),
        )
        .unwrap();
        assert_eq!(wall["PrimaryItems"], json!([{"ItemId":103,"Wall":true}]));
    }

    #[test]
    fn preserves_action_codes_and_exact_numeric_units() {
        for (source, target) in [
            (1, 0),
            (2, 1),
            (3, 2),
            (4, 4),
            (5, 5),
            (6, 6),
            (7, 7),
            (8, 8),
            (9, 10),
            (10, 11),
            (11, 67),
        ] {
            let result = convert_fixture("wf_trg_user_performs_action", json!({"selectedAction":source,"signFilterEnabled":false,"signId":0,"danceFilterEnabled":false,"danceId":0})).unwrap();
            assert_eq!(result["OwnedIntParams"], json!([target]));
            assert_eq!(result["Text"], "");
        }
        for (source, target) in [(0, 100), (1, 201), (2, 0)] {
            let result = convert_fixture("wf_slc_furni_bytype", json!({"sourceType":source,"matchState":false,"pickedFurniIds":[],"filterExisting":false,"invert":false})).unwrap();
            assert_eq!(result["FurniSourceTypes"], json!([target]));
        }
        for (source, target) in [("0", 0), ("1.2", 120), ("1.2500", 125), ("80.00", 8000)] {
            let result = convert_fixture(
                "wf_slc_furni_altitude",
                json!({"altitude":source,"comparison":0,"filterExisting":false,"invert":false}),
            )
            .unwrap();
            assert_eq!(result["OwnedIntParams"], json!([target, 0]));
        }
    }

    #[test]
    fn rejects_lossy_values_and_incompatible_selector_behavior() {
        for altitude in ["1.234", "80.01", "-1", "NaN", "99999999999999999999999"] {
            assert!(convert_fixture("wf_slc_furni_altitude", json!({"altitude":altitude,"comparison":0,"filterExisting":false,"invert":false})).is_err());
        }
        assert!(
            convert_fixture(
                "wf_xtra_execution_limit",
                json!({"maxExecutions":1,"timeWindowMs":501})
            )
            .is_err()
        );
        assert!(
            convert_fixture(
                "wf_trg_recv_signal",
                json!({"furniSource":100,"itemIds":[101],"channel":1})
            )
            .is_err()
        );
        for name in BLOCKED {
            assert!(convert_fixture(name, json!({})).is_err());
        }
    }
}
