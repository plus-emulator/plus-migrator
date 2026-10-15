# Polaris WIRED migration coverage

Source serializers reviewed against Polaris commit
`34bc0d49511c659fd0054d35d957669ebd6ce9ed`.
Configuration compilation checked against Plus commit
`3f59869d` (the pinned development target with the snapshot/roller compatibility
patches described in the README). Other Polaris forks can store different fields.

## Added conversions

44 source box names now use direct Polaris JSON-to-Plus schema-2 conversions,
rather than passing through the legacy Arcturus parameter layout:

- Selectors: picked furniture, furniture by type/on furniture/in an area/at an
  altitude/from a signal, users on furniture/in an area/from a signal/by name,
  type, hand item, team and action.
- Triggers: furniture/tile/user clicks, received signals, clock counters, leaving
  the room and avatar actions.
- Effects: clock control/adjustment, altitude, positive/negative signals,
  freeze/unfreeze, furniture-to-furniture and user-to-furniture movement.
- Add-ons: animation time, execution limits, carrying users, movement without
  animation and ordered execution.
- Conditions: actor direction, altitude, team rank/score, movement validation,
  positive/negative avatar actions and positive/negative furniture comparison.

The exact names, synthetic source payloads and expected fields are in
[`tests/fixtures/polaris-wired.json`](../tests/fixtures/polaris-wired.json).
These are synthetic fixtures, not exported player data. Empty/default source
configurations remain subject to the existing import path; the movement-validation
fixture verifies its empty JSON contract, not an independently configured source row.

The converters preserve primary and secondary item selections, source selectors,
condition quantifiers, selector filters/inversion, delays and action parameters.
Altitude uses exact hundredths; clock half-seconds and execution windows retain
their original units. Missing/foreign-room selections, unexpected fields, mistyped
values and unrepresentable precision are reported instead of discarded. Plus's
actual configuration compiler still validates every converted configuration before
normal migration can write to the destination.

## Deliberate blockers

- Polaris group selectors check group membership; the pinned Plus selector checks
  the equipped group. They are not interchangeable.
- Polaris remote selectors execute individually selected selectors; the target
  executes stacks. Those setups require a separate behavioral adapter.
- Nonzero signal channels and selector delays have no reviewed equivalent here.
- Variable definitions and persisted variable values, neighborhood selectors and
  other serializers not covered above still need their own adapters. A known box
  name alone is not evidence that its configuration can migrate.

These changes do not certify all official/custom WIRED, every Polaris variant, or
a complete live-hotel transfer. The preflight report remains authoritative for the
specific input database. No live database was modified for these tests.

## Verification

Run the commands in the README to generate both JSONL fixture sets and feed them
to the Plus contract validator. The suite checks every new converter, malformed
fields, missing selections, incompatible selectors, numeric precision, action-code
translation and source-code translation. The validator also compares selected
compiled runtime fields for clock timing, furniture movement and text preservation.

At this revision: 16 Rust tests pass; Clippy passes with warnings denied; the actual
Plus compiler accepts 69 standard and 44 Polaris fixture records, with zero errors.
This verifies serialization/projection, not end-to-end in-room gameplay.
