# plus-migrator

Ports an Arcturus Morningstar 3.5.x or Polaris hotel into a fresh PlusEMU database:
players, rooms, furniture, the hotel's own catalog, wired and more. Source ids are
kept 1:1, so links between rows (and wired item picks) carry over untouched.

## Usage

1. Create an empty database and import Plus's `Database/FreshInstall.sql` (PlusEMU `dev`).
2. Stop the emulator and the CMS (`docker compose stop emulator cms-php` in plus-stack).
3. Run:

```bash
cargo build --release
./target/release/plus-migrate \
  --source mysql://user:pass@host:3306/arcturus \
  --target mysql://user:pass@host:3306/plus \
  --furnidata /path/to/FurnitureData.json   # optional, recommended
```

`--force` wipes a target that already has users. `--only users,rooms` runs selected steps.
The source is only read. A 100k-user / 200k-room / 5M-item hotel migrates in about 80s.

## What is migrated

| Source | Plus |
|---|---|
| users, users_settings, users_currency | users, users_settings, user_statistics, user_info, user_currencies |
| ranks (`permissions` / Polaris `permission_ranks`) | user_roles (by security level; VIP ranks -> vip roles) |
| badges, effects, clothing, wardrobe, achievements | user_badges (+ badge_definitions), user_effects, user_clothing, user_wardrobe, user_achievements |
| users_subscriptions (HC) | user_club_memberships, intervals, past paydays (no back-pay burst) |
| friends, requests, ignores, favourites, saved searches, bans, room visits, chat logs, offline messages | their Plus tables |
| rooms, custom floor plans, rights, bans, promotions, word filter, public rooms | rooms, room_models (`model_bc_<id>`), ... |
| guilds, members, requests, forums | groups, group_memberships, group_requests, group_forum_* |
| items_base | furniture (interaction types translated; `--furnidata` fills furnidata columns) |
| catalog_pages, catalog_items | catalog_pages, page texts/images, catalog_offers, offer products, limited stock, bot presets |
| items | items, plus gifts (user_presents), teleport links, group furni, moodlights, toners, jukeboxes and songs |
| wired boxes | wired_item_configurations, reward claim history, Polaris room wired permissions |
| bots, users_pets | bots, bots_speech, bots_petdata |
| open/expired marketplace listings, uncollected sales | items back to the seller, credits paid to the seller |

Anything that cannot be carried over is listed under `notes:` at the end of a run.

## Known limits

- Plus hard-codes a few furniture ids (pet food 320, saddles 4221/4450, the camera
  photo item). Those target rows are kept, and source furni that collide get new ids.
- Wired: every Arcturus 3.5.5 box is translated. Polaris boxes with a Plus
  equivalent always become that box. 83 Polaris-only boxes and the shared ones keep
  their settings. Other Polaris-only boxes start with Plus defaults (listed in notes).
  Boxes Plus lacks (wf_blob, highscores, chests, ...) become plain furni.
- Plus rewards only give badges and furni, so currency prizes in reward boxes are dropped (reported).
- Polaris wired variables and monsterplant data are not migrated.
- Passwords: `$2a$`/`$2b$` bcrypt is rewritten to `$2y$`. Legacy md5/sha hashes cannot log in through the CMS.

`src/wired/boxes.json` and `src/wired/polaris.json` are generated from the wired
research map (source interaction -> Plus box, and Polaris field-copy specs).
