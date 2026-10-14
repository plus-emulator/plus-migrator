//! Works out which emulator the source database belongs to.

use std::collections::HashSet;

use crate::context::Flavor;
use anyhow::{Result, bail};

/// Tables every Arcturus-lineage hotel has. Missing any means this is not one.
const ARCTURUS_CORE: &[&str] = &[
    "users",
    "items",
    "items_base",
    "rooms",
    "catalog_items",
    "catalog_pages",
];

/// Tables only Polaris creates (its Flyway history, plus tables from its own
/// migrations), so a vanilla Arcturus database never has them.
const POLARIS_MARKERS: &[&str] = &[
    "flyway_schema_history",
    "wired_emulator_settings",
    "room_wired_settings",
];

pub fn detect(tables: &HashSet<String>) -> Result<Flavor> {
    let missing: Vec<&str> = ARCTURUS_CORE
        .iter()
        .copied()
        .filter(|t| !tables.contains(*t))
        .collect();
    if !missing.is_empty() {
        if tables.contains("furniture") && tables.contains("catalog_offers") {
            bail!(
                "the source looks like a Plus database, not Arcturus/Polaris (did you swap --source and --target?)"
            );
        }
        bail!(
            "the source is not an Arcturus 3.x / Polaris database: missing tables {}",
            missing.join(", ")
        );
    }
    if POLARIS_MARKERS.iter().any(|t| tables.contains(*t)) {
        return Ok(Flavor::Polaris);
    }
    Ok(Flavor::Arcturus)
}
