//! Arcturus / Polaris interaction types to PlusEMU interaction types.
//!
//! Sources: Arcturus `ItemManager` registrations, Polaris additions, and the
//! Plus parser (`HabboHotel/Items/InteractionTypes.cs`). The reference table is
//! the one PlusEMU's own catalog importer uses
//! (`scripts/import-habbo-catalog.py`, `REFERENCE_INTERACTIONS`), extended with
//! name-based rules for furni Arcturus drives by classname rather than type.

/// Every interaction string the Plus parser accepts (InteractionTypes.cs).
pub const PLUS_INTERACTIONS: &[&str] = &[
    "area_hide",
    "invis_control",
    "wired_chest_furni",
    "wired_chest_coins",
    "wired_contract_payment",
    "wired_contract_trade",
    "wired_contract_reward",
    "wired_room_linker",
    "wired_label",
    "wired_link",
    "default",
    "gate",
    "postit",
    "dimmer",
    "trophy",
    "bed",
    "scoreboard",
    "vendingmachine",
    "alert",
    "onewaygate",
    "loveshuffler",
    "habbowheel",
    "dice",
    "hopper",
    "bottle",
    "teleport",
    "pool",
    "roller",
    "fbgate",
    "icetag_pole",
    "icetag_field",
    "iceskates",
    "rollerskate",
    "lowpool",
    "haloweenpool",
    "ball",
    "green_goal",
    "yellow_goal",
    "red_goal",
    "blue_goal",
    "green_score",
    "yellow_score",
    "blue_score",
    "red_score",
    "bb_blue_gate",
    "bb_red_gate",
    "bb_yellow_gate",
    "bb_green_gate",
    "bb_patch",
    "bb_blue_score",
    "bb_red_score",
    "bb_yellow_score",
    "bb_green_score",
    "banzaicounter",
    "bb_teleport",
    "banzaipuck",
    "bb_pyramid",
    "freezetimer",
    "freezeexit",
    "freezeredcounter",
    "freezebluecounter",
    "freezeyellowcounter",
    "freezegreencounter",
    "freezeyellowgate",
    "freezeredgate",
    "freezegreengate",
    "freezebluegate",
    "freezetileblock",
    "freezetile",
    "jukebox",
    "musicdisc",
    "pressure_pad",
    "wf_floor_switch1",
    "wf_floor_switch2",
    "puzzlebox",
    "water",
    "gift",
    "background",
    "mannequin",
    "vip_gate",
    "roombg",
    "gld_item",
    "gld_gate",
    "guild_forum",
    "tent",
    "tent_small",
    "badge_display",
    "tile_walkmagic",
    "stacktool",
    "television",
    "wired_effect",
    "wired_trigger",
    "wired_condition",
    "wired_selector",
    "wired_addon",
    "wired_variable",
    "floor",
    "wallpaper",
    "landscape",
    "badge",
    "crackable_egg",
    "effect",
    "deal",
    "roomdeal",
    "horse_saddle_1",
    "horse_saddle_2",
    "horse_hairstyle",
    "horse_body_dye",
    "horse_hair_dye",
    "gnome_box",
    "bot",
    "purchasable_clothing",
    "pet_breeding_box",
    "arrow",
    "lovelock",
    "cannon",
    "wf_upcounter1",
    "wf_upcounter2",
    "wf_game_upcounter1",
    "wf_game_upcounter2",
    "counter",
    "camera_picture",
    "fx_provider",
    "exchange",
    "skateboard",
    "pet",
];

/// Source names whose Plus equivalent has a different name.
const RENAMES: &[(&str, &str)] = &[
    ("crafting", "default"),
    ("", "default"),
    ("normal", "default"),
    ("switch", "default"),
    ("multiheight", "default"),
    ("clothing", "purchasable_clothing"),
    ("crackable", "crackable_egg"),
    ("crackables", "crackable_egg"),
    ("crackable_master", "crackable_egg"),
    ("pressureplate", "pressure_pad"),
    ("floor_switch", "wf_floor_switch1"),
    ("love_lock", "lovelock"),
    ("guild_furni", "gld_item"),
    ("guild_gate", "gld_gate"),
    ("club_gate", "vip_gate"),
    ("vendingmachine_no_sides", "vendingmachine"),
    ("vending", "vendingmachine"),
    ("puzzle_box", "puzzlebox"),
    ("football", "ball"),
    ("rebug_football", "ball"),
    ("stack_helper", "stacktool"),
    ("stack_walk_helper", "stacktool"),
    ("tile_walk_magic", "tile_walkmagic"),
    ("pyramid", "bb_pyramid"),
    ("external_image", "camera_picture"),
    ("tile_fxprovider_nfs", "fx_provider"),
    ("colorwheel", "habbowheel"),
    ("spinning_bottle", "bottle"),
    ("rollerskate_field", "rollerskate"),
    ("background_toner", "roombg"),
    ("ads_bg", "background"),
    ("youtube", "television"),
    ("yt_tv", "television"),
    ("breeding_nest", "pet_breeding_box"),
    ("vote_counter", "scoreboard"),
    ("game_timer", "freezetimer"),
    ("football_gate", "fbgate"),
    ("battlebanzai_puck", "banzaipuck"),
    ("battlebanzai_sphere", "bb_pyramid"),
    ("battlebanzai_random_teleport", "bb_teleport"),
    ("battlebanzai_tile", "bb_patch"),
    ("freeze_tile", "freezetile"),
    ("freeze_block", "freezetileblock"),
    ("freeze_exit", "freezeexit"),
    ("water_item", "water"),
    ("wateritem", "water"),
    ("game_upcounter", "wf_game_upcounter1"),
];

const COLOURS: &[&str] = &["red", "blue", "green", "yellow"];

/// What a source furni definition becomes in Plus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translated {
    pub interaction: String,
    pub behaviour_data: i64,
    /// Source interaction had no Plus behaviour and now acts as a plain switch.
    pub fell_back: bool,
}

fn plain(name: &str) -> Translated {
    Translated {
        interaction: name.to_string(),
        behaviour_data: 0,
        fell_back: false,
    }
}

/// Translates a non-wired furni definition. Wired boxes are handled by the wired
/// map, which decides the box name; see `steps::wired`.
pub fn translate(src_interaction: &str, item_name: &str, kind: &str) -> Translated {
    let it = src_interaction.trim().to_ascii_lowercase();
    let name = item_name.trim().to_ascii_lowercase();

    // These names used to be mapped to superficially similar handlers. That changes
    // behavior (automatic teleport vs a door, avatar FX vs hand items, random vs toggle,
    // HC checks, group checks, subscriptions vs crack prizes). Require a real adapter.
    if matches!(
        it.as_str(),
        "teleporttile"
            | "club_teleporttile"
            | "effect_vendingmachine"
            | "effect_vendingmachine_no_sides"
            | "random_state"
            | "switch_remote_control"
            | "pressureplate_group"
            | "crackable_subscription_box"
            | "crackable_monster"
            | "costume_hopper"
            | "costume_hoppper"
            | "club_hopper"
            | "trax_machine"
            | "pet_food"
            | "pet_drink"
            | "pet_toy"
            | "nest"
    ) {
        return Translated {
            interaction: "default".into(),
            behaviour_data: 0,
            fell_back: true,
        };
    }

    // Credit furni: Arcturus redeems by classname (CF_10_coin_gold = 10 credits).
    if let Some(value) = credit_value(&name) {
        return Translated {
            interaction: "exchange".into(),
            behaviour_data: value,
            fell_back: false,
        };
    }
    // Room decorations are consumed by classname in Arcturus.
    if kind == "i" && matches!(name.as_str(), "floor" | "wallpaper" | "landscape") {
        return plain(&name);
    }
    if let Some(horse) = horse_item(&name) {
        return plain(horse);
    }
    if let Some(piece) = game_piece(&it, &name) {
        return plain(&piece);
    }
    if it.starts_with("pet") && it[3..].chars().all(|c| c.is_ascii_digit()) && it.len() > 3 {
        return plain("pet");
    }
    if let Some((_, to)) = RENAMES.iter().find(|(from, _)| *from == it) {
        return plain(to);
    }
    if PLUS_INTERACTIONS.contains(&it.as_str()) {
        return plain(&it);
    }
    Translated {
        interaction: "default".into(),
        behaviour_data: 0,
        fell_back: true,
    }
}

fn credit_value(name: &str) -> Option<i64> {
    let rest = name
        .strip_prefix("cfc_")
        .or_else(|| name.strip_prefix("cf_"))?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('_') {
        return None;
    }
    digits.parse().ok().filter(|v| *v > 0)
}

fn horse_item(name: &str) -> Option<&'static str> {
    if name.starts_with("horse_hairdye") {
        Some("horse_hair_dye")
    } else if name.starts_with("horse_dye") {
        Some("horse_body_dye")
    } else if name.starts_with("horse_hairstyle") {
        Some("horse_hairstyle")
    } else if name == "horse_saddle1" {
        Some("horse_saddle_1")
    } else if name == "horse_saddle2" {
        Some("horse_saddle_2")
    } else {
        None
    }
}

/// Game pieces, named per colour in both emulators but spelled differently.
fn game_piece(it: &str, name: &str) -> Option<String> {
    for c in COLOURS {
        let pairs = [
            (format!("football_counter_{c}"), format!("{c}_score")),
            (format!("football_goal_{c}"), format!("{c}_goal")),
            (format!("battlebanzai_counter_{c}"), format!("bb_{c}_score")),
            (format!("battlebanzai_gate_{c}"), format!("bb_{c}_gate")),
            (format!("freeze_counter_{c}"), format!("freeze{c}counter")),
            (format!("freeze_gate_{c}"), format!("freeze{c}gate")),
        ];
        if let Some((_, to)) = pairs.iter().find(|(from, _)| from == it) {
            return Some(to.clone());
        }
    }
    // Upcounters and floor switches are wf_* classnames that are not boxes.
    for exact in [
        "wf_floor_switch1",
        "wf_floor_switch2",
        "wf_upcounter1",
        "wf_upcounter2",
        "wf_game_upcounter1",
        "wf_game_upcounter2",
    ] {
        if name == exact || it == exact {
            return Some(exact.to_string());
        }
    }
    None
}

/// Registrations from Morningstar e33ac57a; this distinguishes a known source
/// handler from a fork/plugin name, not official provenance of the asset itself.
pub fn reference_handler(name: &str) -> Option<&'static str> {
    static HANDLERS: std::sync::LazyLock<std::collections::HashMap<String, String>> =
        std::sync::LazyLock::new(|| {
            serde_json::from_str(include_str!("arcturus_interactions.json")).unwrap()
        });
    HANDLERS.get(name).map(String::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn genuine_renames_and_identity() {
        assert_eq!(
            translate("guild_furni", "gld_flag", "s").interaction,
            "gld_item"
        );
        assert_eq!(
            translate("battlebanzai_gate_red", "bb_gate_r", "s").interaction,
            "bb_red_gate"
        );
        assert_eq!(
            translate("default", "CF_50_goldbar", "s").behaviour_data,
            50
        );
        assert_eq!(
            translate("default", "wallpaper", "i").interaction,
            "wallpaper"
        );
        assert_eq!(translate("crafting", "altar", "s").interaction, "default");
    }
    #[test]
    fn similar_names_are_not_behavior_equivalence() {
        for interaction in [
            "teleporttile",
            "effect_vendingmachine",
            "random_state",
            "crackable_subscription_box",
            "club_hopper",
        ] {
            assert!(
                translate(interaction, "example", "s").fell_back,
                "{interaction}"
            );
        }
        assert!(translate("custom_plugin", "example", "s").fell_back);
    }
}
