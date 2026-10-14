# Arcturus production migration findings

**Result: incomplete. Do not switch this hotel to the migrated database.** This
review found and fixed data-loss bugs, but a Rust importer cannot substitute for
missing target runtime behavior. All testing used a dedicated local MariaDB server
on loopback port 13379. The live plusemu.dev database was not accessed or modified.

## Evidence and scope

Reference source: Morningstar `e33ac57a518d83afad5ad103599e8f3d03c37508`.
Target: PlusEMU dev `e3dc5f3437a78900d985821a13172db5d9051dd0`, plus the companion
snapshot compatibility patch. The supplied production database is a customized
Arcturus schema; its exact running emulator commit was not supplied. Reference
registrations are evidence of known handlers, not proof that an asset is official.

A recovery archive was restored into a third isolated database. Extended checksums
matched for **all 174 base tables and 445,830 rows**. This proves preservation in the
archive for this fixture. It is separate from playable/runtime migration coverage.

The fixture has 128 users, 140 rooms, 28,010 item records and 343 configured Wired
items. 280 configurations use the standard converter set. The other 63 use fork
handlers: 59 exact-keyword triggers and four custom effects. All 280 converted
configurations, and synthetic configurations covering all 69 standard mapping
entries, passed the actual target Plus configuration compiler.

Final verification: 12 Rust tests passed; Clippy passed with warnings denied;
89 targeted Plus tests passed. All 17 core reconciliation queries returned zero
discrepancies (floating coordinates compared within 1e-9, finer than the source's
six decimal places). All declared destination foreign keys passed the orphan audit.
Rejected preflight, same-database aliases and partial-run requests returned nonzero;
checksums confirmed that both source and target remained unchanged. The final recovery
SQL has the same SHA-256 as the archive whose full restore was verified.

See the [interaction inventory](interaction-map.md) for all 361 encountered interaction
names and the 69 standard Wired name mappings. No player text or credentials are
included in that inventory.

## Fixed defects

| Area | Previous behavior | Change |
|---|---|---|
| Target safety | Target cleared before validation; partial runs cleared unrelated data | Non-mutating preflight, identity checks, mandatory offline source/archive, partial runs rejected |
| SQL/text | Strings shortened; encoding guessed; insert errors ignored | Exact text preservation, destination length checks, strict writes |
| Wired storage | Schema 1 runtime-shaped JSON rejected by current Plus | Schema 2 native editor authority checked by Plus itself |
| Wired movement | North and south source values swapped | Source direction 4 becomes south; 6 becomes north |
| Snapshot effects | Space sentinel captured as actual state | Explicit skip-state metadata; requires companion Plus patch; 29 production boxes affected |
| Wired rewards | Claim IDs treated as array positions | Resolve original prize IDs; stream complete claim histories; missing claims block |
| Room models | Heightmap-only matching lost entrance/wall differences | Room-local geometry, entrance, wall height and public items preserved |
| Wardrobe | Long outfits silently truncated to 120 characters | 256-character destination; source fixture includes 215-character outfits |
| Mottos | Target fixed-width CHAR discarded trailing spaces | Variable-width 127-character field preserves source mottos |
| Bots/pets | Fractional Z rounded | Preserve doubles; eight production coordinates affected |
| Catalog | Composite source IDs collided in target; attached badges dropped | Deterministic variant IDs, attached badge products, partial-bundle blockers |
| Achievements | Cumulative totals reduced by completed levels | Preserve cumulative progress |
| Friendships | Synthesized reverse rows overwrote independent relationships | Copy source directions first; synthesize only missing reverses |
| Crafting | Target cleared without importing source recipes | Recipes, ingredients, altar links and learned recipes converted |
| HC history | Overlapping intervals double-counted; canceled future grants possible | Union of elapsed history; honor active subscription state |
| Private messages | Chat history omitted | Private logs copied to Plus console logs |

The 8 extra catalog variants receive IDs above the source maximum. Source furniture
IDs colliding with target hard-coded definitions are remapped consistently. Item and
user IDs otherwise stay unchanged, except extra wrappers for multi-item gifts and
pet IDs offset into Plus's shared bot/pet table.

## Remaining blockers

The generated report is authoritative for the actual input. In this fixture:

- Plus lacks fully equivalent behavior for several source handlers, including
  teleport tiles, pet food/drink/nests, gym equipment and Trax composition. Highscore
  payload support in Plus is not equivalent to Arcturus's complete leaderboard
  producers/reset lifecycle. Superficially similar names are not safe substitutes.
- Custom handlers remain unsupported, including the 63 configured custom Wireds.
- Some catalog products refer to absent/unusable definitions; partial bundles now
  report missing products too. Six achievement-progress rows have no matching Plus
  achievement. A duplicate badge has conflicting equipped slots.
- Two owner-zero item rows have no gift wrapper. Their originals are archived;
  inventing an owner would be incorrect.
- Five furniture definitions have different male/female effects, whereas Plus
  exposes one effect field. They require target behavior support.
- Populated tables without adapters are explicitly reported. These include source
  configuration/reference data, room votes, moderation/support history, target-offer
  purchase state, CMS data and custom systems. Archival retention is not an active
  target feature. Even an adapted table may have unsupported fields.
- Rank mapping targets existing Plus roles by security level. It is not a complete
  translation of every Arcturus permission. CMS authentication, permissions,
  external assets and in-client room/gameplay acceptance still need review against
  the actual deployment.

Legacy non-JSON snapshot formats, Polaris-specific configurations without a reviewed
schema-2 converter, unsupported reward kinds and target limits produce blockers;
they must not be replaced with working-looking defaults. A successful configuration
compile cannot establish gameplay parity for every combination of boxes and room
state. This patch does not certify a complete production cutover.

## Further acceptance work

1. Implement and test missing official runtime behaviors in Plus; then add adapters
   tied to those implementations. Preserve source history/state, not only item names.
2. Resolve source orphans and contradictory records explicitly, with originals retained.
3. Add field-level adapters for remaining hotel/CMS systems or document their separate
   migration destinations. Verify the actual source emulator commit and client assets.
4. Rehearse again on fresh isolated databases. Require a clean report, full reconciliation,
   archive restore verification and in-client checks of rooms, inventories, pets,
   accounts, catalog purchases and Wired game sequences before considering cutover.
