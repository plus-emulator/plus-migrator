# plus-migrator

Offline Arcturus-to-Plus conversion with preflight checks, a source recovery archive,
and a compatibility report. **The production fixture is not ready for a lossless
runtime cutover.** Unsupported interactions and source data stop a normal run;
keeping their original rows in an archive does not make them work in Plus.

The tested contracts are Morningstar `e33ac57a518d83afad5ad103599e8f3d03c37508`
and PlusEMU dev `e3dc5f3437a78900d985821a13172db5d9051dd0`, with the companion
`WiredFurniSnapshot.SkipStateRestore` and roller activation patches. A fork's schema/behavior can differ.
Polaris detection remains available, but full Polaris compatibility is unverified.
See [compatibility findings](docs/compatibility.md) and the
[361-interaction inventory](docs/interaction-map.md).

## Isolated rehearsal

Use a separate database server with no access to the live hotel. Import an offline
source dump into one database and the pinned Plus `Database/FreshInstall.sql` into
another. Stop **all** writers to both staging databases during the run. The source
connection only reads data and changes its own session settings.

Apply [the companion snapshot patch](compat/plus-arcturus-snapshot.patch) to the
pinned **staging** Plus checkout before building the validator. Also apply the
[roller activation patch](compat/plus-roller-activation.patch): the pinned Plus
legacy engine otherwise leaves rollers idle despite correct interaction mappings. The patch is also
available in the local Plus branch `fix/arcturus-wired-snapshot-state` (`ba1a8e83`).

```bash
git -C /absolute/path/to/PlusEMU apply /absolute/path/to/plus-migrator/compat/plus-arcturus-snapshot.patch
git -C /absolute/path/to/PlusEMU apply /absolute/path/to/plus-migrator/compat/plus-roller-activation.patch
```

Build the migrator and the validator against that same Plus code:

```bash
cargo build --release --locked
# Requires the .NET SDK specified by the target Plus checkout.
dotnet build tests/PlusContract/PlusContract.csproj -p:PlusRoot=/absolute/path/to/PlusEMU
```

The `--wired-validator` argument must be an executable accepting JSONL on stdin.
The generated `tests/PlusContract/bin/Debug/net10.0/PlusContract` apphost works with
a normally installed .NET runtime. For a private SDK installation, set `DOTNET_ROOT`
or use an executable wrapper that runs `dotnet /absolute/path/to/PlusContract.dll`.

Set `PLUS_MIGRATE_SOURCE` and `PLUS_MIGRATE_TARGET` privately to the staging MySQL
URLs, then validate without changing the target:

```bash
./target/release/plus-migrate \
  --source-offline --dry-run \
  --wired-validator /absolute/path/to/validate-wired \
  --report /private/path/preflight.json
```

For a write run, omit `--dry-run` and provide `--archive-dir /private/path/new-backup`.
The archive directory must not already exist. `--force` permits replacing a populated
staging database; it does not bypass compatibility checks. `--only` is rejected
because independent partial migrations cannot safely preserve cross-table references.

A diagnostic test may additionally use `--allow-incomplete`. It writes the
representable subset so you can investigate problems, but **still exits nonzero**
when blockers remain. Never use that result for cutover. Restore/recreate the staging
target before retrying a failed write; writes are not a hotel-wide transaction.

`--furnidata /path/to/FurnitureData.json` enriches furniture definitions. Retain the
source client assets, badge images and camera/media files separately: a SQL dump
does not contain these files.

## Preservation and validation

- Preflight runs the converters and string/schema checks before clearing tables.
- Source and destination database identity are checked using server metadata,
  including when different hostnames resolve to the same database.
- All source **base tables**, including unknown tables, are saved with original
  DDL and byte-preserving values before target writes. Views, triggers, routines,
  events and external files need a separate backup; their presence is reported.
- The JSON report inventories every source table, lists interactions and their
  target mappings, reports remapped furniture IDs, and records blockers.
- Wired uses Plus schema **2**, with owned editor parameters, source selectors,
  selections, delays and saved snapshots. Each converted configuration must compile
  through the actual Plus registry/projection. This verifies the storage contract;
  it does not prove every in-room behavior is equivalent.
- No string truncation, mojibake repair, default Wired reset, or ordinary
  `INSERT IGNORE` is used to conceal conversion errors. Explicit duplicate handling
  remains for sets such as badge definitions and symmetric links.
- Post-write checks compare user/room identities, credits including marketplace
  proceeds, currencies, badge ownership, model geometry and every declared foreign key.
- The migrator widens wardrobe looks to 256 characters, mottos to `VARCHAR(127)` (preserving trailing
  spaces), and bot/pet Z coordinates to
  `DOUBLE`, matching the source values and Plus's runtime `double` coordinate types.

Passwords, private messages and other personal data remain in the private source
and recovery archive. Do not check dumps, archives or production JSONL into Git.
Reports contain identifiers and schema/interaction names, and should also stay private.

## Tests

```bash
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
PLUS_MIGRATE_CONTRACT_FIXTURES=/tmp/standard-boxes.jsonl cargo test --locked
/path/to/validate-wired < /tmp/standard-boxes.jsonl
```

The generated fixture covers all 69 standard source Wired mapping entries. The
production rehearsal additionally tested 280 configured standard boxes against Plus.
The companion Plus patch has execution tests for both state-update paths and the
single-snapshot fallback. See the findings for the limits of this evidence.

For the isolated production fixture, core reconciliation can be repeated with:

```bash
python3 tests/reconcile.py --socket /path/to/isolated/mysql.sock \
  --source arc_source --target plus_test
```

This read-only check compares known fields; passing it does not clear the separate
compatibility report or certify every omitted source field.

Rejection-path tests for the same disposable server (local test account without a password):

```bash
python3 tests/safety.py --socket /path/to/isolated/mysql.sock --port 13379 \
  --binary /absolute/path/to/plus-migrator/target/release/plus-migrate \
  --validator /absolute/path/to/validate-wired --artifacts /private/path/new-test-results
```

Do not run other database writers while the safety test compares checksums.
