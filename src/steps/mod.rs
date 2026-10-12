pub mod interactions;
pub mod prep;

mod bots;
mod catalog;
mod furniture;
mod groups;
mod items;
mod rooms;
mod social;
mod users;

use crate::context::{Step, step};

pub fn all() -> Vec<Step> {
    vec![
        step!("furniture", 1, furniture::run),
        step!("catalog", 1, catalog::run),
        step!("users", 1, users::run),
        step!("social", 1, social::run),
        step!("rooms", 1, rooms::run),
        step!("groups", 1, groups::run),
        step!("items", 1, items::run),
        step!("bots", 1, bots::run),
        step!("wired", 1, crate::wired::run),
        step!("marketplace", 2, items::marketplace_credits),
    ]
}
