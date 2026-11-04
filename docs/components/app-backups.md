# Library backups (`app-backups`)

> Layer: backend feature (src-tauri) on top of a crate module (`fly-storage` `backup.rs`). Added for v2.5.0 after a beta upgrade test found that the schema v3 step, the re-embed backfill and the compression job rewrite the library with no automatic copy of the database.

## Purpose
Before an update changes the user's library, the app saves a copy of the **database** (`flyonthewall.db`) to `<data dir>/backups/`, checks it, and keeps the newest three plus the last copy from before each recent schema change. Recordings and attachments are **not** copied: compression is lossless and verified sample for sample, and a copy would be tens of GB.

## Files it owns
- `crates/fly-storage/src/backup.rs`: the whole mechanism (names, the pre-open check, the copy, verification, retention, the settings rows, the notice state). Public module `fly_storage::backup`.
- `src-tauri/src/backups.rs`: `BackupStatus`, the 3 commands, the compression job's backup (`backup_before_compression`, `before_first_compression_pass`), the `backup:status` event.
- `src-tauri/src/state.rs` (`open_library`, `APP_VERSION`): runs the backup step, then the open, then records the result. See [app-shell](app-shell.md).
- `src-tauri/src/startup.rs` (`slow_open_step`, `LibraryStep`): sends a launch that will take a backup down the background path, with a "Saving a backup of your library…" step. See [app-shell](app-shell.md).
- `src-tauri/src/audio_store.rs` (`run_pass`): one call to `before_first_compression_pass`.
- Frontend: `backupModel.ts`, `components/BackupNotice.tsx` (`useBackupStatus`, `useBackupNotice`), `components/BackupRow.tsx`, the `no-backup` entry in `AppNotices.tsx`, the backup step in `LibraryGate.tsx`.

## When a backup is taken
`backup::before_open(data_dir, running_version, free_space, on_backup)` runs **before** `Storage::open*`, which is what runs any schema step. It reads two things through a read-only connection (the file's `user_version` and the `app.last_run_version` settings row) and backs up when:

| Condition | Label in the file name |
|---|---|
| A schema step will run on the next open (`user_version != SCHEMA_VERSION`, the file isn't brand new) | the recorded version if it differs from the running one, else `schemaN` (the file's `user_version`) |
| The recorded app version differs from the running one, or none was recorded (every build before this feature) | the recorded version, else `schemaN` |

It does nothing on a fresh install (no file, or a file with no tables), and nothing when the version and schema both match: a second launch on the same version takes **no** backup. Builds before this feature record no version, so the first launch of v2.5.0 over v2.4.0 produces `flyonthewall-schema2-<timestamp>.db`.

**Compression.** The compression job rewrites rows and manifests after it converts audio. `backup.before_compression = done` is set when a verified backup exists from before it could have run (the startup backup counts) or the library was born under this feature. If it isn't set (the startup backup was skipped for disk space, or the install predates backups), `before_first_compression_pass` takes one (labelled with the running version) before the first pass that has work, once per run of the app. It runs on the job's thread through its own read-only connection and holds no storage lock.

## How a backup is made
1. `backups/` is created; the free-space rule is checked (below).
2. Leftover `*.db.partial` files of a crashed run are removed (only exact backup names plus `.partial`).
3. `VACUUM INTO` a temp name `flyonthewall-<label>-<timestamp>.db.partial`, through a separate **read-only** connection: a consistent snapshot of the committed state with the WAL folded in, without blocking the app's own writer, and without the pages of deleted text.
4. The temp file is synced (`sync_all`), then opened and checked: `PRAGMA quick_check` must say `ok` and its `user_version` must equal the source's.
5. Renamed into place (`rename_with_retry`; the folder is synced on Unix). Only now does it count as a backup. Any failure removes the temp file and leaves no backup.
6. Retention (`apply_retention` → `expired`): the new file, the newest others up to [`KEEP_BACKUPS`] = 3 in all, and the newest copy of each of the latest [`KEEP_SCHEMAS`] = 3 schema versions found in the folder. The last part is the point: a backup is taken at every app-version change, so after three routine updates a plain "newest 3" would have evicted the copy from before the schema change, the one a bad migration needs. A backup's schema is its `user_version`, read from the 100-byte SQLite header (`complete_schema`; no database open). That read also decides what counts: a file whose header isn't SQLite's, or whose length isn't a whole number of pages or disagrees with the header's page count (truncated by a full disk, replaced by a stub, appended to) takes no slot, is not deleted and is logged as damaged, so the user can still look at it. Only names matching exactly `flyonthewall-<label>-<YYYYMMDDTHHMMSSZ>.db` that are regular files are ever listed or deleted; anything else in the folder is left alone. In practice a folder holds at most five: the newest three and the last copy of each of the two older schemas, and fewer when the schema is rarely bumped.

A new name's stamp is later than every stamp already in the folder (`unique_name`), because the listing and retention order by stamp: a clock set back (or standing still, as in tests) must not file the newest backup as an old one.

**Free space.** `max(2 × database size (file + WAL), 200 MB)` must be free on the disk holding `backups/` (`audio_store::free_space`; an unknown amount doesn't block). Less than that skips the backup: a log line (`skipped the library backup: not enough free disk space`) and the one-time notice. Startup is never held up: every problem is an `Outcome` for `after_open` to record, never an error from the open path.

## Notice and Settings row
- `backup.notice` is a `NoticeKind` while the one-time notice is due: `space` / `failed` for the copy before an upgrade, `compression_space` / `compression_failed` for the compression job's own copy. The frontend shows one dismissible warning, "No backup of your library was saved", whose wording follows the kind: before an upgrade it says the library was upgraded without a copy; before compression it says the recordings are checked sample for sample but the notes have no safety copy, and (for lack of space) that the app tries again next launch, since the copy stays owed. The upgrade wording would be wrong for the compression step (nothing was upgraded), so the two are separate kinds.
- **Once per app version.** `backup.notice_version` holds the app version the notice was last raised for, and `raise_notice` does nothing when it equals the running version. It outlives the notice, so dismissing it keeps it dismissed: a backup skipped for space is owed again every launch (the compression job retries it), and without this each launch would raise the notice again after the user dismissed it. Whichever step fails first tells the user; the other stays quiet behind it. A new app version is a new upgrade and tells again. An unreadable library at the check only raises it when the version changed.
- Settings › Your files and privacy › **Backups**: the count and an **Open folder** button. No restore button.

## Restore
Restoring is a manual file copy, with the app closed:

1. Quit Fly on the Wall (and any program using its MCP server, such as Claude Desktop, so nothing holds the database).
2. Open the data folder (Settings › Your files and privacy › Show folder). It holds `flyonthewall.db`, and possibly `flyonthewall.db-wal` and `flyonthewall.db-shm`.
3. **Delete `flyonthewall.db-wal` and `flyonthewall.db-shm`.** They belong to the database you are replacing, and must not be paired with the restored file.
4. Copy (don't move) the backup from `backups/` over `flyonthewall.db`. A backup name says what the library was: `flyonthewall-2.4.0-20261103T101500Z.db` is the library as of version 2.4.0, saved at that UTC time; `schema2` means a library from before versions were recorded.
5. Start the app. It sees the restored file as it was: an older schema is upgraded again (after saving a fresh backup of the restored file), and recordings are untouched. Notes and edits made after the backup was taken are not in it.

To go back to the older app version too, install that version before step 5 (the v2.5 notes explain what an older build does to a v2.5 library).

## Integration with the rest of the app
- **Delete all my data** removes `backups/` with the other content folders, right away (`privacy.rs` `CONTENT_DIRS`), and the restart wipe would remove it anyway. It holds the backup lock meanwhile, so a backup in flight can't outlive the folder. A backup contains notes and transcripts.
- **Self-heal and the portability backfill** scan `recordings/` and `recordings/_unlinked/` only; the v2 orphan sweep scans `recordings/` and `notes/`. Nothing scans the data dir root for meeting folders, so `backups/` is never seen. A test puts a manifest-bearing folder and a note mirror inside `backups/` and asserts nothing is healed, parked or rewritten.
- **The MCP server never opens a backup.** It opens `<data dir>/flyonthewall.db` only (`Storage::open_existing`), and `open_existing` and `Storage::open` both refuse a folder that holds backup files (`refuse_backup_dir`), so `--data-dir <data>/backups` fails instead of creating a library there.
- **The startup gate**: `startup::slow_open_step()` returns `Backup` when `backup::is_due` says a backup will be taken, so the work goes down the background path (a large library takes seconds to copy) and the screen reads "Saving a backup of your library…" (an indeterminate bar) until the upgrade's first progress report moves it to "Upgrading your library". The copy reports no percentage, so `on_backup` is called with the bytes about to be copied (database + WAL) and the gate's `total` carries them during this step; after 3 s the screen adds a line such as "Copying 1.4 GB. 12 s so far." (`backupStepHint`, the elapsed time counted in the browser), so a large library reads as work, not a hang.
- **Open-retry and self-heal are untouched.** An unreadable or locked library at the check is retried briefly (3 × 400 ms), then yields `Unchecked`, and the open that follows runs its own retry (`OpenRetry::STARTUP`) and error dialog exactly as before.

## Surface area
### Public API (`fly_storage::backup`)
| Item | Called by |
|---|---|
| `before_open(data_dir, version, free_space, on_backup: FnMut(bytes)) -> Outcome`, `before_open_with(…, &Patience)` | `state::open_library` |
| `after_open(&Storage, version, &Outcome)` | `state::open_library` |
| `is_due(data_dir, version) -> bool` | `startup::slow_open_step` |
| `before_compression(data_dir, version, free_space) -> Outcome`, `compression_backup_due(&Storage)`, `after_compression_backup(&Storage, version, &Outcome)` | `backups::backup_before_compression` |
| `list(data_dir) -> Vec<BackupFile>` (newest first), `is_backup_dir`, `refuse_backup_dir` | `backups::status`, `Storage::open*` |
| `pending_notice(&Storage)`, `dismiss_notice(&Storage)`, `lock()` | `backups.rs`, `privacy::perform_delete_all` |
| `BACKUP_DIR`, `KEEP_BACKUPS`, `KEEP_SCHEMAS`, `MIN_FREE_BYTES`, `needed_free_bytes`, the four settings keys | everywhere above |

`Outcome` is `NewLibrary`, `NotNeeded`, `Saved(BackupFile)`, `NoSpace { free, needed }`, `Failed(String)` or `Unchecked(String)`. `NoticeKind` is `NoSpace`, `Failed`, `CompressionNoSpace` or `CompressionFailed` (strings `space`, `failed`, `compression_space`, `compression_failed`).

### Tauri commands
| Command | Arguments | Returns | Called from (UI) |
|---|---|---|---|
| `backup_status` | none | `Result<BackupStatus { count, notice: "space" \| "failed" \| "compression_space" \| "compression_failed" \| null }, String>` | `ui-settings` (`BackupRow.tsx`), `ui-notices` (`BackupNotice.tsx`), via `api.backupStatus` |
| `dismiss_backup_notice` | none | `Result<(), String>` | `ui-notices` (`BackupNotice.tsx`), via `api.dismissBackupNotice` |
| `reveal_backups_dir` | none (creates the folder when missing) | `Result<(), String>` | `ui-settings` (`BackupRow.tsx` Open folder), via `api.revealBackupsDir` |

All three are mirrored in `frontend/src/devMock.ts` (`fotwMockBackups = N`, `fotwMockBackupNotice = space | failed | compression_space | compression_failed`; `?fotwMockUpgrade=backup` shows the step with a made-up 1.5 GB size).

### Events
| Event | Direction | Payload | Other end |
|---|---|---|---|
| `backup:status` | Emits (`backups.rs`, when the compression job's backup ran) | `BackupStatus` | `BackupNotice.tsx` (`useBackupStatus`) |
| `library:upgrade` | Emits `LibraryStatus { phase, step: "backup" \| "upgrade", done, total }` (`step` is new; in the backup step `total` is the bytes being copied) | | `LibraryGate.tsx` |

### Settings keys
| Key | Writer | Meaning |
|---|---|---|
| `app.last_run_version` | `backup::after_open` (every launch whose version differs) | the app version that last opened the library; read before the open, through a read-only connection |
| `backup.before_compression` | `after_open`, `after_compression_backup` | `done` once a backup from before compression exists (or the library is new) |
| `backup.notice` | `after_open`, `after_compression_backup`; deleted by `dismiss_backup_notice` | a `NoticeKind` string while the notice is due |
| `backup.notice_version` | `raise_notice` (with `backup.notice`) | the app version the notice was last raised for; a dismissal doesn't delete it, so the notice stays dismissed for that version |

None of the four is in the webview's settings allowlist.

### Files
| Path | Notes |
|---|---|
| `<data dir>/backups/flyonthewall-<label>-<YYYYMMDDTHHMMSSZ>.db` | `<label>` is `[A-Za-z0-9._+]`, at most 40 characters (a `-` in a version becomes `_`). No `-wal`/`-shm`. |
| `<data dir>/backups/….db.partial` | in progress; removed by the next run if a crash left it |

## Invariants
- A backup is read from the live library through a **read-only** connection and never takes the storage lock; the copy of a library can't block a write.
- A file counts as a backup only after it was synced, passed `quick_check` and carried the source's `user_version`, and only after the rename.
- Only exact-pattern regular files in `backups/` are listed or deleted. Retention counts only files that still look like a complete database, and never deletes one that doesn't. The new backup always survives retention, and is always stamped after the others, even with the clock set back.
- The newest copy of each of the latest three schema versions is never evicted by routine updates.
- No backup problem ever fails or delays the open beyond the short check retry; the user is told once.
- A backup holds the whole library (notes, transcripts, settings) as it was when it was made, so "Delete all my data" must remove the folder, and it does. `VACUUM INTO` writes no free pages, so text deleted from the live library before a backup isn't carried into it. Deleting a note afterwards does **not** rewrite older backups: it leaves them when they are replaced by newer ones or by "Delete all my data". Each update replaces the oldest of the newest three, but the last copy from before a schema change stays until a later schema change pushes it out, so a deleted note can outlive the newest copies. The Settings row says so.

## Tests
| Layer | Where | What |
|---|---|---|
| unit | `crates/fly-storage/src/backup.rs` | name pattern, label sanitising, the free-space rule, notice kinds; the header check (`complete_schema`: good, truncated, a page short or long, stub, wrong magic); retention as a pure function (`expired`: pre-migration copy kept, schema cut-off, new file always kept); stamps always after the newest one, even with the clock set back |
| integration | `crates/fly-storage/tests/backup.rs` (21) | a v2 library gets exactly one verified backup (including a row only in the WAL; the gate callback gets its size); a second launch on the same version none; fresh install none; schema step with a matching version; retention keeps 3 and touches only exact names; the copy from before the schema change survives six version bumps (and reads back the v2 rows); the last copy of each recent schema is kept and older schemas go; a truncated or stub file takes no slot and is left alone; a dismissed notice stays dismissed across launches and across the startup and compression steps until the version changes; low disk skips with the notice and the open still runs (exact threshold, unknown space); an unwritable `backups` path; corrupt and locked libraries don't crash the step; the compression backup is owed once; restore by the documented steps (the restored library reads back the known notes); backups never opened as a library; `backups/` invisible to self-heal and the v2 upgrade |
| unit | `src-tauri/src/state.rs` | the launch order (backup reported before the upgrade's progress), one backup then none, low disk, corrupt library reaches the open error, fresh install |
| unit | `src-tauri/src/backups.rs` | status and notice, the compression backup with a concurrent writer on the real `Mutex<Storage>`, a skipped one stays owed, and a dismissed notice stays dismissed (until the version changes) |
| unit | `src-tauri/src/privacy.rs` | Delete all removes `backups/`; the restart wipe removes it |
| frontend | `backupModel.test.ts`, `BackupRow.test.tsx`, `AppNotices.test.tsx`, `LibraryGate.test.tsx` | copy (including the compression wording and the size/time hint), the row, the notice (dismiss once, event, waits out a recording), the backup step of the gate and its hint |

## Changing it safely
- A new schema step needs nothing here: `check` compares the file's `user_version` with `SCHEMA_VERSION`.
- Don't add a second place that deletes from `backups/`, and don't widen the name pattern: the retention guarantee is that nothing else in the folder is touched.
- Anything else that starts reading the data dir root must skip `backups/` (it holds copies of everything).
- Keep `before_open` ahead of `Storage::open*` in every launch path; `open_library` is the one place that orders them.
