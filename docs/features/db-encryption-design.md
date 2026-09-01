# Optional Database Encryption Design

- Date: 2026-08-28 (rev 6 — key retention + verification levels)
- Status: target design + implementation plan (no code written yet)
- Scope: desktop (macOS/Windows/Linux) + iOS + server/Docker. Android deferred.
- Primary input: source audit of `crates/storage-sqlite`, `apps/tauri`,
  `apps/server`, and the Docker build; SQLCipher behavior verified against
  Zetetic's published API documentation; review findings on device sync,
  restore, and conversion cleanup; connection-ownership audit of `providers.rs`
  and `write_actor.rs`.

## Executive Summary

Add **optional, at-rest encryption** of the main SQLite database (`app.db`)
using **SQLCipher**, enabled through the single `libsqlite3-sys` that both
Diesel and `rusqlite` already link. Encryption is **opt-in and off by default**;
a build with SQLCipher compiled in opens existing plaintext databases unchanged.

The design is deliberately minimal: **one key, two sources, no metadata files.**

- **Desktop/iOS** — 32 random bytes generated once, stored in the OS keychain
  via the existing `keyring` path.
- **Server/Docker** — derived from `WF_SECRET_KEY` via HKDF. Stateless: nothing
  stored, and a `.db` copied to any instance sharing the secret simply opens.

No DMK/KEK envelope, no sidecar file, no passphrase scaffolding, no plaintext
header. Three file-level operations — conversion, restore, and device-sync
snapshots — are first-class parts of Phase 1 because each interacts with the
key.

Every file-replacing operation runs **in process, to completion, before an
intentional restart**, through a single `DatabaseMaintenanceCoordinator`.
**Normal startup never inspects candidate files, pending markers, or staged
restores.** It opens `app.db` and nothing else.

### Revision history

**Rev 2 — envelope removed.** The first draft wrapped a random Data Master Key
with a per-platform Key-Encryption-Key, forcing an unencrypted sidecar to hold
the wrapped key, nonce, salt, KDF parameters, and version. Its only benefit was
cheap rotation, which does not survive scrutiny: re-wrapping rotates the
_wrapping_, not the data key, so anyone who captured the DMK keeps access.
Rotation was already a manual operation, and `PRAGMA rekey` is seconds at this
data scale. Deleting the envelope deleted the sidecar and everything serving it.

**Rev 3 — plaintext header removed, file-level operations added.** Rev 2 adopted
`cipher_plaintext_header_size` and a 96-hex `key‖salt` format on every platform,
justified by an iOS process-kill hazard. That justification was overstated: the
hazard's precondition is absent here (see Key Format). Removing it restores the
normal fully-encrypted header, the plain 64-hex key, and drops explicit salt
management. Rev 3 also corrects three repository interactions rev 2 got wrong or
omitted — device-sync snapshots, restore-under-a-live-pool, and the retained
plaintext conversion copy.

**Rev 4 — restart-time reconcile replaced by an in-process coordinator.** Rev 3
staged conversion and restore through a `startup_reconcile()` that ran before
pool creation. That was simple to implement — no pool exists at startup — but it
taxed every launch with a check for pending work, and surfaced failures one
restart away from the action that caused them, when no rollback context
remained. Rev 4 moves the work into the session that requests it: verify, tear
down the database runtime, replace, verify again, roll back on failure, then
restart. Startup returns to doing exactly one thing.

**Rev 5 — the teardown mechanism made concrete.** Rev 4 named the prerequisite
but described it as making repositories borrow a pool cell, which would have
meant changing 36 constructor signatures. Rev 5 replaces that with a stable
`DatabaseRuntime` holding a takeable `Option<Arc<ServiceContext>>` — no
repository signature changes — and corrects rev 4's claim that Tauri has no
state-removal API (it has one; it is deprecated and unsafe). Rev 5 also
specifies _how_ the zero-connection precondition is proven, extends runtime
rebuild to failure paths, and corrects the device-sync temp-file inventory from
one site to three.

**Rev 6 — the key is never deleted.** Revs 4–5 deleted the desktop/iOS key on
disable and minted a fresh one on re-enable. That was a data-loss bug: internal
and pre-operation backups faithfully inherit the source database's encryption
(Decision 14), so deleting the key orphans every encrypted backup — including
the pre-operation backup taken by the very disable that deletes it, which is the
rollback artifact for that operation. Rev 6 retains the key permanently, reuses
it on re-enable, and moves deletion and rotation out of Phase 1 entirely. Rev 6
also pins down the three verification levels, including the fact that
`cipher_integrity_check` signals success by returning **no rows**.

## Decisions (locked)

| #   | Decision                       | Choice                                                                                       |
| --- | ------------------------------ | -------------------------------------------------------------------------------------------- |
| 1   | Threat model                   | Transparent at-rest now; passphrase deferred entirely to Phase 2                             |
| 2   | Default posture                | Opt-in, off by default; existing DBs stay plaintext                                          |
| 3   | Server/Docker key source       | Derived from `WF_SECRET_KEY` via HKDF — nothing stored                                       |
| 4   | Reversibility                  | Desktop: reversible in-app. Server: explicit maintenance command, fail closed                |
| 5   | Android                        | Out of scope; blocked on prerequisites                                                       |
| 6   | Server key rotation            | Manual, and must rotate JWT + `secrets.json` + DB coherently                                 |
| 7   | Conversion timing              | In-process, completes **before** the intentional restart                                     |
| 8   | SQLCipher crypto backend       | Vendored OpenSSL, uniform across all targets                                                 |
| 9   | Key model                      | Direct key — no DMK/KEK envelope, no sidecar file                                            |
| 10  | Key format                     | Plain 64-hex raw key; SQLCipher-managed salt; **no** plaintext header                        |
| 11  | Detection                      | Probe by opening; **never** mint a key during detection                                      |
| 12  | Cargo feature flag             | None — SQLCipher always compiled in, one build configuration                                 |
| 13  | User-facing backup export      | Decrypted and portable, via a **separate** explicit API                                      |
| 14  | Internal/pre-operation backups | Faithful copies — inherit the source database's encryption                                   |
| 15  | Device-sync snapshots          | Explicit `KEY ''` — snapshots stay plaintext on the wire                                     |
| 16  | Restore                        | In-process via the maintenance coordinator; same path as conversion                          |
| 17  | Plaintext residue after enable | **Both** the replaced file and the plaintext pre-operation backup deleted after verification |
| 18  | Restored backup's settings     | Never override the current device's encryption policy                                        |
| 19  | Maintenance mechanism          | One `DatabaseMaintenanceCoordinator` for restore, enable, and disable                        |
| 20  | Normal startup                 | Opens `app.db` only — never inspects candidates, markers, or staged restores                 |
| 21  | Zero-connection guarantee      | Hard precondition; abort untouched if it cannot be proven                                    |
| 22  | Post-maintenance continuation  | Desktop restarts unconditionally; iOS rebuilds the runtime or blocks DB use                  |
| 23  | Key retention                  | Key is **never deleted** — retained in the keychain across disable                           |
| 24  | Re-enable                      | Reuses the existing key; deletion and rotation are Phase 1 non-goals                         |
| 25  | Candidate files                | Uniquely named; ignored by startup; swept only when maintenance next begins                  |
| 26  | Runtime teardown               | Stable `DatabaseRuntime` + `Option::take`; never `Manager::unmanage()`                       |
| 27  | Enable failure                 | Key is retained so the operation can be retried safely                                       |
| 28  | Key presence ≠ encrypted       | Key existence says nothing about the file; only probing determines state                     |
| 29  | Verification levels            | `sqlite_master` for key; `integrity_check` before destructive steps                          |

## Non-Goals

- **Android.** No `gen/android` project exists (only `gen/apple`), and `keyring`
  has no Android backend — it compiles to a non-persistent, insecure in-memory
  mock. Blocked on generating the Android project plus a real Keystore-backed
  secret store. The `KeyProvider` trait is shaped so an Android implementation
  drops in without touching call sites.
- **Passphrase mode.** Deferred to Phase 2 and not designed here. Deferring
  costs nothing: adding it later over a keychain-stored random key is a single
  `PRAGMA rekey`, so there is no lock-in penalty and no reason to build
  scaffolding now.
- Encrypting secrets that already have protection: the OS keychain (desktop/iOS)
  and the ChaCha20Poly1305 `secrets.json` (server).
- **Key deletion and key rotation.** Phase 1 has no way to remove or replace a
  desktop/iOS key. Disabling encryption does _not_ delete it (see Key
  Retention). Both are separate future operations with their own requirements —
  chiefly re-encrypting or consciously abandoning every encrypted backup that
  key opens.
- Full-disk / OS-level encryption (FileVault/BitLocker) — complementary.

## Approach: SQLCipher via the shared `libsqlite3-sys`

Diesel 2.3.12 and `rusqlite` 0.34 both resolve to a **single** `libsqlite3-sys`
0.32.0. Enabling SQLCipher on that one crate transparently encrypts Diesel's
entire query layer — no second SQLite, and no application-level column crypto
(which would break querying, sorting, and aggregation across nearly every
table).

- `libsqlite3-sys = { version = "0.32", features = ["bundled-sqlcipher-vendored-openssl"] }`
- **Always compiled in — no Cargo feature gate.** SQLCipher with no key applied
  behaves as plain SQLite and opens existing plaintext databases unchanged, so
  unencrypted users are unaffected. A feature flag would mean two build
  configurations to test forever.
- Validation gate (Phase 0): confirm `bundled` (from `rusqlite`) and the
  SQLCipher feature coexist on the same `libsqlite3-sys` and link on every
  target, including musl.

## Key Model

**One key per database. No wrapping, no derivation chain, no stored metadata.**

| Platform                | Key source                                                       | Stored where                                   |
| ----------------------- | ---------------------------------------------------------------- | ---------------------------------------------- |
| Desktop (mac/win/linux) | 32 random bytes, generated once                                  | OS keychain, one entry (`keyring`)             |
| iOS                     | same as desktop                                                  | Keychain (`Security.framework` already linked) |
| Server / Docker         | `HKDF-SHA256(WF_SECRET_KEY, info = "wealthfolio-db")` → 32 bytes | nothing stored — derived every boot            |
| Android                 | —                                                                | blocked (see Non-Goals)                        |

### Key retention — the key outlives the encryption

**The key is created once and never deleted.** `KeyProvider` needs exactly two
methods: `existing()` for detection and opening, and `create()` for the first
enable when no key exists. There is no delete path in Phase 1.

Disabling encryption decrypts the database but **leaves the key in the
keychain**, and a later enable **reuses it**. The reason is backup
recoverability: internal and pre-operation backups faithfully inherit the source
database's encryption (Decision 14), so deleting the key would orphan every
encrypted backup it opens — including the pre-operation backup taken by the
disable itself, which is that operation's own rollback artifact.

**Key presence does not mean the database is encrypted.** After a disable, a
retained key alongside a plaintext database is the _normal_ state, not an
anomaly. Only probing the file determines its actual state — which is why
detection tries keyed, then falls back to unkeyed, and never infers from key
existence.

A retained key protects nothing while the database is plaintext, and it sits in
the OS keychain either way, so retention costs no meaningful security. Removing
or replacing a key is **rotation**, an explicit non-goal for Phase 1.

### Key format — plain raw key, SQLCipher-managed salt

```
PRAGMA key = "x'<64 hex chars = 32-byte key>'";
```

Per SQLCipher's API documentation, a 64-character hex key is used directly as
the raw encryption key, skipping the KDF — correct here, since the input is
already a random or HKDF-derived key rather than a passphrase. SQLCipher
generates and stores a random salt in the first 16 bytes of the database.

**Why no plaintext header (reversal from rev 2).** Rev 2 set
`cipher_plaintext_header_size = 32` and carried the salt in a 96-hex key string,
citing an iOS process kill. The documentation's actual wording is that the
pragma is _"primarily intended for use on iOS when a WAL mode database will be
stored in a shared container,"_ where _"iOS actually examines a database file to
determine whether it is an SQLite database in WAL mode"_ and kills the process
if it cannot identify the header. **The shared-container precondition does not
hold here:** the iOS database lives in the normal application sandbox
(`db/mod.rs` `get_db_path`), and the app declares no App Group entitlement — its
iOS entitlements contain only `com.apple.developer.associated-domains`. WAL mode
alone does not trigger the hazard.

Dropping the plaintext header removes explicit salt management, restores a fully
encrypted header, and returns the key to a plain 32 bytes. If Phase 0 device
testing contradicts this, the fallback is to reinstate
`cipher_plaintext_header_size = 32` with the 96-hex `key‖salt` format and store
the salt alongside the key in the same keychain entry — a contained change,
because nothing else depends on the key's length.

## Detection: probe by opening

Ask the `KeyProvider` whether a key **already exists** — never get-or-create —
then attempt to open and read `sqlite_master`. On failure, retry the other way
on a fresh connection. Two attempts, startup only.

**Key generation happens only in the explicit enable path.** This is the rule
that keeps "off by default" true: a get-or-create provider consulted during
detection would mint a key, and the keyed open of a _missing_ file would create
a brand-new encrypted database and succeed — silently encrypting a fresh install
that never opted in.

Bootstrap states, resolved explicitly:

| State                          | Action                                                                      |
| ------------------------------ | --------------------------------------------------------------------------- |
| Missing DB                     | Create per policy: desktop default plaintext; server per `WF_DB_REQUIRE_ENCRYPTION` |
| Existing DB + key present      | Try keyed; on failure retry unkeyed on a fresh connection                   |
| Existing DB + no key           | Try unkeyed only                                                            |
| Plaintext DB, enable requested | Only here: generate and persist a key, then convert                         |
| Both attempts fail             | Report wrong-key-or-corruption; **never** generate a replacement key        |

This probe is the **whole** of startup's database logic: it opens `app.db` and
nothing more. It never looks for candidate files, pending markers, or staged
restores — those exist only inside a maintenance operation that always runs to
completion first.

The `app_settings` toggle records **intent** for the UI, never truth. In the
final case the first 16 bytes usefully distinguish a corrupt plaintext file
(`SQLite format 3\0`) from an encrypted one with an unavailable key — used for
the error message only, never as the detection mechanism.

## Verification Levels

Three checks with three different jobs. Using the wrong one — or the wrong
success condition — is a real source of bugs, so each is pinned down here.

**1. Key correctness (cheap, every open).**

```sql
SELECT count(*) FROM sqlite_master;
```

This is SQLCipher's documented key test: _"if this throws an error, the key was
incorrect. If it succeeds and returns a numeric value, the key is correct."_ It
forces the first page's schema to be read and decrypted. This is the query
behind "probe by opening" above.

**2. Structural integrity (before anything destructive).**

```sql
PRAGMA integrity_check;
```

Standard SQLite. A healthy database returns **exactly one row containing `ok`**;
anything else — more rows, or a different value — is a failure. Run this on the
user's selected backup before using it, and on the candidate before installing
it.

**3. Cryptographic integrity (encrypted candidates only).**

```sql
PRAGMA cipher_integrity_check;
```

Verifies each page against its stored HMAC, detecting pages _"likely modified
after they were written."_ **Success is signalled by returning no rows at all**
— per the documentation, _"If no results are returned then the database was
found to be externally consistent."_

> **Do not apply level 2's success condition to level 3.** Asserting "one row
> saying `ok`" against `cipher_integrity_check` would report every healthy
> encrypted database as corrupt. Zero rows is the pass.

It requires the correct key and HMAC enabled (the SQLCipher 4 default), so run
it after level 1 succeeds.

**After replacement:** reopen the installed `app.db` with the intended key and
run level 1 before restarting. Flush the candidate and the parent directory
around the atomic replace wherever the platform supports it.

## Code Integration Points

SQLCipher requires `PRAGMA key` as the **first** statement on every connection,
before any other PRAGMA. There are **six** production connection opens, all but
one in `crates/storage-sqlite/src/db/mod.rs`:

| #   | Site                                                                  | Notes                                                                 |
| --- | --------------------------------------------------------------------- | --------------------------------------------------------------------- |
| 1   | `init()` — `establish` (~line 42)                                     | Key before the `journal_mode`/`foreign_keys` batch                    |
| 2   | `run_migrations()` — `establish` (~line 65)                           | Key before the migration PRAGMAs                                      |
| 3   | `ConnectionCustomizer::on_acquire()` (~line 555)                      | The pool — **also covers the write actor**, which draws from the pool |
| 4   | `backup_database_to_file()` — `RusqliteConnection::open` (~line 682)  | Without the key, `wal_checkpoint`/`VACUUM INTO` fail                  |
| 5   | Pre-replacement checkpoint (today `restore_database_safe`, ~line 842) | Checkpoints the _outgoing_ DB — needs its key                         |
| 6   | Post-replacement verification (today ~line 826)                       | Opens the _incoming_ DB — needs the incoming key                      |

Sites 5 and 6 are why restore is not a pure file copy: the two opens straddle
the swap and may need **different** keys. Under rev 4 both move inside the
maintenance coordinator, where the outgoing and incoming keys are known
explicitly, rather than being incidental opens in a restore helper.

Mechanism: a `DbEncryptionKey` newtype wrapping the 64-hex string with an `apply(conn)`
that issues the key pragma first, and a `KeyProvider` trait with two methods
kept deliberately distinct — `existing()` for detection and `create()` for the
enable path. Wired at the two pool-construction call sites: the `apps/tauri`
context providers and `apps/server`'s `build_state`.

## Database Maintenance Coordinator

Restore, enable, and disable are the same operation: **replace `app.db` with a
verified candidate while nothing is connected.** One
`DatabaseMaintenanceCoordinator` owns all three.

**Core rule:** the work runs in process and finishes _before_ the intentional
restart. Normal startup never checks for `app.db.new`, a pending-restore marker,
or a staged operation — it opens `app.db` and proceeds.

### Prerequisite: a takeable database runtime

The coordinator's steps 7–8 are load-bearing, and today's architecture cannot
satisfy them. Verified against the code:

- The pool is `Arc`-cloned **36 times** in `apps/tauri/src/context/providers.rs`
  into repositories and services, all held by `ServiceContext`, which is
  `handle.manage(Arc::clone(&context))`-ed into Tauri state.
- The write actor takes a pooled connection at spawn (`write_actor.rs`,
  `let mut conn = acquire_writer_connection(&pool)?`) and holds it for the life
  of its task, spawned as a bare `tokio::spawn` with **no retained
  `JoinHandle`** — nothing to signal, nothing to await.
- Critically, the actor's pool handle **does not travel through
  `ServiceContext`**: `providers.rs:78` passes `pool.as_ref().clone()`, an
  independent inner `Pool` clone owned by the task. Dropping the context
  therefore does _not_ release it.

Consequently "drop every connection" is unreachable by draining work alone, and
step 8's abort would fire on every attempt as the code stands.

**The shape: a stable `DatabaseRuntime` with a takeable context.** Manage a
long-lived `DatabaseRuntime` in Tauri state that owns
`Mutex<Option<Arc<ServiceContext>>>`. Maintenance takes the `Option`; every one
of the 36 pool clones dies with the context. **No repository signature changes**
— they keep taking `Arc<Pool<...>>` exactly as they do now.

`DatabaseRuntime` must support: entering maintenance mode and rejecting new
database commands; cancelling and joining background workers that hold context
clones; draining and explicitly stopping the write actor; dropping all services,
repositories, pool clones, and connections; proving no SQLite connections
remain; and **reinitializing the context** — needed both when replacement fails
and on iOS, where there is no process restart.

**Do not use `Manager::unmanage()`.** It exists (Tauri 2.11.4), but it is
`#[deprecated(since = "2.3.0")]` and its own documentation warns it is _UNSAFE_
and "will cause previously obtained references … to become dangling references."
That documentation prescribes exactly the design above: _"If you really want to
unmanage a state, use `std::sync::Mutex` and `Option::take` to wrap the state
instead."_ (Rev 4 wrongly said Tauri had no removal API; it has one, and it must
not be used.)

### Proving zero connections (step 8)

The abort condition is only meaningful if it is checked. Three checks, **in this
order** — the ordering is not optional:

1. **Stop background workers**, then **signal and join the write actor.** Its
   `JoinHandle` must actually complete; only then is its independent `Pool`
   clone released. Doing this after taking the context would deadlock the next
   check.
2. **`Arc::into_inner(ctx)` returns `Some`.** This is a proof of sole ownership:
   if any worker still holds a clone, it returns `None` and maintenance aborts
   having touched nothing.
3. **Exclusive-open probe.** Reopen the file and take
   `PRAGMA locking_mode = EXCLUSIVE` with a write transaction. It succeeds only
   if no connection anywhere still holds the database.

Check 3 matters because the platforms fail differently. On **Windows** an atomic
replace fails outright with a sharing violation while any handle is open — loud,
but only on Windows. On **POSIX** the replace _succeeds_ and leaves surviving
connections reading and writing the old, orphaned inode: silent divergence,
which is worse. The exclusive probe catches both before anything is replaced.
(Today's restore papers over the Windows half with `copy_with_retries` and
journal-mode juggling rather than closing handles.)

### Restore

1. User-facing backups remain deliberately plaintext and portable; internal and
   pre-operation backups preserve the live database's encryption state.
2. Validate the selected backup **read-only** and run `PRAGMA integrity_check`.
3. Build a uniquely named candidate beside the live database —
   `app.db.restore.<uuid>.new`. **Never modify or consume the user's backup.**
4. If device policy is encrypted, produce the candidate with `sqlcipher_export`
   under the current device key; otherwise produce a plaintext candidate.
5. Reconcile the candidate's `app_settings` encryption value to the **current
   device policy**. The flag belongs to the device, not to the backup.
6. Run `PRAGMA integrity_check` on the candidate; for encrypted candidates also
   run `PRAGMA cipher_integrity_check`. Checkpoint, close, and fsync it.
7. Enter exclusive maintenance mode: reject new database work, stop device sync
   and background tasks, drain the write actor, checkpoint the live database,
   and close and drop every pooled and standalone connection.
8. **Prove zero connections by the three checks above; if any fails, abort
   without touching `app.db`.** Aborting is always safe; proceeding on an
   assumption is not.
9. Create a consistent **pre-operation backup** of the current database. It
   inherits the source database's encryption (Decision 14) — which makes it
   plaintext when the source is plaintext. See the enable exception below.
10. Remove the old `-wal`/`-shm`, then atomically replace `app.db` with the
    verified candidate **on the same filesystem**. Flush the parent directory
    where the platform supports it.
11. Reopen the new `app.db` with the expected key and verify. On failure, roll
    back from the pre-operation backup **and reinitialize the runtime** — a
    rollback that leaves no live context strands the app with no database.
12. **Desktop: restart unconditionally** — not a prompt the user can decline.
    **iOS: rebuild the complete database runtime, or block further database use
    and require the user to reopen the app.** Never continue on the old pool.

Step 12 replaces today's behavior, where restore swaps the file while the pool
is live (a 200 ms sleep is not shutdown) and then _offers_ a restart the user
may decline — `app_handle.restart()` sits behind an OkCancel dialog and a
`#[cfg(not(any(target_os = "ios", target_os = "android")))]` gate, so iOS has no
restart path at all. That gap is why iOS needs the explicit runtime-rebuild or
block branch.

### Enable and disable

Both run through the same coordinator, with the same teardown, verification, and
rollback.

- **Enable:** generate and store the key **before** building the encrypted
  candidate. If any later step fails, **keep the key** — the database is still
  plaintext, and the retained key makes the operation safe to retry. Detection
  handles this state already: key present + plaintext database falls through to
  the unkeyed open (see the bootstrap table).

  **Enable is the one operation that must clean up after itself.** Its source is
  plaintext, so _two_ readable copies exist at step 9: the file being replaced
  and the pre-operation backup. Both must be deleted once the encrypted database
  is verified. Deleting only the replaced file — as an earlier revision
  specified — would leave a complete, readable copy of every account, holding,
  and transaction sitting in the backup directory, defeating the feature for the
  user who just enabled it. Retain them only until verification succeeds; a user
  who wants a rollback point takes an explicit backup first, and the UI says so.

- **Disable:** decrypt with the existing key, verify and install the plaintext
  candidate, and **keep the key in the keychain permanently**. Its pre-operation
  backup is _encrypted_ (the source was), so it is retained like any other
  internal backup — and stays readable precisely because the key is never
  deleted.
- **Re-enable reuses the existing key.** No fresh key is minted, so encrypted
  backups taken before the disable stay openable afterward.

### Crash behavior

| Moment                     | Outcome                                                |
| -------------------------- | ------------------------------------------------------ |
| Before replacement         | The old `app.db` remains authoritative                 |
| During atomic replacement  | Either the complete old file or the complete new file  |
| After replacement          | Ordinary startup opens the already-installed `app.db`  |
| Stale candidates left over | Ignored by startup; swept when maintenance next begins |

No recovery branch runs at startup, because no state is ever left that startup
would need to interpret.

## Device Sync — snapshots must attach with `KEY ''`

**Rev 2 claimed device sync was unaffected. That was wrong.** App-sync exports
selected rows into an attached snapshot file and later restores downloaded
snapshots through a second attachment
(`crates/storage-sqlite/src/sync/app_sync/repository.rs`, the export and restore
`ATTACH DATABASE` sites). Neither carries a `KEY` clause.

SQLCipher's documented behavior: _"If no KEY paramater is specified then the
attached database will use the exact same raw key and database salt as the main
database (or none if the main database is plaintext)."_ So once the main
database is encrypted, the exported snapshot is silently encrypted with **that
device's** key and uploaded in that form, while the receiving device attaches it
expecting plaintext. The result is silent cross-device breakage that only
manifests between a converted device and an unconverted one.

**Fix: pass `KEY ''` explicitly on both attachments.** Snapshots stay plaintext,
which is what the wire format expects and what device sync's transport-level
E2EE already protects. This preserves today's behavior exactly and is a two-line
change — but it must be explicit, because the _default_ silently changes meaning
the moment the main database is keyed.

**Related exposure (pre-existing, now worth stating): three temp-file sites, not
one.** Every snapshot path writes a plaintext `.db` into `std::env::temp_dir()`:

| Path             | Site                                                         |
| ---------------- | ------------------------------------------------------------ |
| Export (upload)  | `crates/storage-sqlite/src/sync/app_sync/repository.rs:3149` |
| Download (Tauri) | `apps/tauri/src/commands/device_sync/snapshot.rs:525`        |
| Server           | `apps/server/src/api/device_sync_engine.rs:1293`             |

These are plaintext copies of synced financial rows in a shared location, and
they read far worse once the product claims encryption at rest. Fix **all
three**: place them in app-private storage with restrictive permissions and
delete them on **every exit path**, including error and early-return paths — not
just the success path. Scope this as cleanup, not a sync redesign.

## Backups — two operations, two meanings

| Operation                                           | Behavior                             |
| --------------------------------------------------- | ------------------------------------ |
| `backup_database_to_file` (internal, pre-operation) | Faithful copy — inherits encryption  |
| `export_portable_backup` (new, user-facing)         | Explicitly decrypted, portable `.db` |

Do **not** make the generic backup function always decrypt. The user-facing
export produces a portable plaintext file — preserving exactly today's
semantics, where a backup restores on any machine — and the UI must state
plainly that the exported file is unencrypted.

Rationale: the Phase 1 threat model is a lost or stolen device. The live
database is protected; a backup the user deliberately exports to their own
storage remains their responsibility, as today. The alternative — encrypting
exports with the keychain-held random key — is strictly safer but creates a
data-loss trap: if the machine dies, every backup becomes unopenable. That
option needs a key-escrow story ("show me my recovery key") and is deferred with
passphrase mode.

Server backups need no special handling: derived keys make them portable across
any instance sharing `WF_SECRET_KEY`.

Relevant to the existing iOS backup export path (issue #1184).

**Waived:** SQLCipher documents that `sqlcipher_export` copies neither
`user_version` nor `auto_vacuum`. Both are unused across this codebase, so there
is nothing to preserve. Recorded so a future reader does not rediscover it as a
defect; revisit only if either is ever adopted.

## Build & Docker

- `bundled-sqlcipher-vendored-openssl` compiles OpenSSL from source →
  deterministic, no dependence on any base-image OpenSSL, one crypto path on
  every target. Needs `perl` (and `make`) at build time.
- **Dockerfile change** (builder stage `apk add`, currently
  `clang lld build-base git file pkgconfig`): add `perl`. `build-base` already
  provides `make`/`gcc`. The existing `openssl-dev`/`openssl-libs-static` become
  unnecessary for SQLCipher but may stay for other `openssl-sys` users.
- **Phase 0 must prove** the vendored OpenSSL cross-build inside the `xx`
  pipeline on the musl target — the one genuinely unproven bit.
- **Fallback (documented, not default):** if `openssl-src` fights the `xx`
  toolchain, use server-only non-vendored `bundled-sqlcipher` against the Alpine
  `openssl-libs-static` already installed, with desktop/iOS staying vendored.
  Downside: two crypto codepaths to test.

Rationale for vendored-everywhere: reproducible pinned OpenSSL; the "let the
distro patch it" argument is nullified by static linking (a rebuild is required
either way); no musl system-OpenSSL probing; one identically-tested crypto
stack.

## iOS Export Compliance (`ITSAppUsesNonExemptEncryption`)

A US export-control declaration (EAR), not an engineering flag. The app pins
`ITSAppUsesNonExemptEncryption=false` in `Info.ios.plist` — truthful so far,
because TLS and ChaCha20Poly1305-for-secrets sit in exempt buckets. SQLCipher
introduces AES-256 at-rest encryption, so the declaration must be re-confirmed.

- At-rest encryption with standard algorithms, limited to protecting the user's
  own local data and not the app's primary function, generally qualifies for a
  standard EAR exemption. Realistic paths: keep `false`, or set `true` and
  answer App Store Connect's exemption follow-ups.
- Because SQLCipher statically links its own crypto rather than Apple's, the
  "only uses Apple OS encryption" exemption does not apply; relying on the
  mass-market / user's-own-data exemption can, in strict form, carry an **annual
  self-classification report** to BIS/NSA (informational; no license, no fee).
- Simpler-compliance alternative: SQLCipher's CommonCrypto backend on iOS keeps
  it in the Apple-OS-crypto lane, at the cost of iOS not sharing the uniform
  build.
- **This is legal, not engineering.** Owner: whoever handles App Store
  compliance. Confirm before the first encrypted iOS build ships.

## Phasing

- **Phase 0 — Build spike and device check (no product change).** Prove
  SQLCipher compiles and links on macOS, Windows, Linux, iOS, and the
  Docker/musl `xx` target. Confirm on a real iOS device that a WAL-mode
  SQLCipher database in the normal sandbox backgrounds without the process kill
  — validating the decision to drop the plaintext header. _Verify:_ keyed
  round-trip; wrong key rejected; plaintext DB opens with no key applied.
- **Phase 1a — Database lifecycle (prerequisite, no user-visible change).**
  Introduce the stable `DatabaseRuntime` holding
  `Mutex<Option<Arc<ServiceContext>>>`; add the maintenance gate that rejects
  new database commands; make background workers cancellable and joinable; give
  the write actor a shutdown signal and a retained `JoinHandle`; implement the
  three-check zero-connection proof and context reinitialization. Repository
  signatures are untouched. _Verify:_ the runtime tears down and rebuilds at
  will, and the exclusive-open probe passes while torn down. Independently
  valuable — it also fixes today's live-pool restore, which is a bug with or
  without encryption.
- **Phase 1b — Transparent at-rest (desktop + iOS + server).** `KeyProvider`
  with separate `existing()`/`create()`, the six open-site patches, probe
  detection with the bootstrap table, the `DatabaseMaintenanceCoordinator`
  covering restore, enable **and** disable, `KEY ''` on both device-sync
  attachments, the split backup APIs, the settings toggle (copy the
  desktop/mobile cfg-split in `apps/tauri/src/commands/settings.rs` around
  `menu_bar_visible`; UI card modeled on
  `apps/frontend/src/pages/settings/general/auto-update-settings.tsx`), and
  server env handling — all file replacement routed through the
  `DatabaseMaintenanceCoordinator`.
- **Phase 2 — Optional passphrase (desktop + iOS), if wanted.** Introduce a
  versioned key-envelope artifact _then_, designed against real requirements,
  and bundle it atomically with every backup. Prefer Argon2id over SQLCipher's
  default PBKDF2-HMAC-SHA512 at 256,000 iterations: Argon2id is memory-hard,
  which matters against GPU/ASIC attack on an offline-captured financial
  database. Adding this later is a single `PRAGMA rekey`.
- **Phase 3 — Android (blocked).** Generate the Android project and a real
  Keystore-backed secret store first.

## Test Matrix (Phase 1 exit criteria)

- Missing DB on first launch → stays plaintext on desktop; encrypted on server
  only when `WF_DB_REQUIRE_ENCRYPTION=1`.
- Corrupt DB → reported as corruption; no replacement key generated.
- Missing keychain entry with an encrypted DB → clear error, no data touched.
- Enable → restart → data intact; no plaintext file remains beside the database.
- Disable (desktop) → restores plaintext; encrypted original removed.
- Restore under encryption: plaintext backup into an encrypted device, and the
  reverse; restored `app_settings` never flips the device's policy.
- Device sync between an encrypted device and a plaintext device, both
  directions.
- `VACUUM INTO` backup and portable export against an encrypted source.
- `cipher_integrity_check` on a healthy encrypted database returns **zero
  rows**, and the code treats that as success (guards against the "expect `ok`"
  inversion).
- `integrity_check` on a healthy database returns exactly one `ok` row, and a
  corrupted file is rejected before any destructive step.
- Process interruption after **each** conversion and restore rename.
- Server: boot encrypted from `WF_SECRET_KEY`; boot with the flag unset against
  an encrypted DB → fail closed.

Coordinator-specific:

- **Outstanding connections:** with a connection deliberately held open,
  maintenance aborts and `app.db` is byte-identical afterward.
- **Restore across states:** plaintext backup → encrypted device, and encrypted
  backup → plaintext device; both land on the _device's_ policy, not the
  backup's.
- **Interruption immediately before replacement** → old database intact and
  openable.
- **Interruption immediately after replacement** → new database intact; ordinary
  startup opens it with no recovery branch.
- **Rollback:** force post-replacement verification to fail; the pre-operation
  backup is reinstated and the database opens.
- **Startup ignores candidates:** leave `app.db.restore.<uuid>.new` and a stale
  `app.db.new` beside the database; assert startup opens `app.db`, never touches
  either, and that they are swept only when maintenance next begins.
- **Disable interrupted:** force failure mid-disable; the key still exists and
  the encrypted database still opens.
- **User backup is never consumed:** the selected backup file is byte-identical
  after a successful restore and after an aborted one.

Lifecycle-specific (Phase 1a):

- Maintenance mode **rejects new database operations** while active.
- The write actor and every background worker shut down cleanly and are joined.
- `Arc::into_inner` returns `None` — and maintenance aborts — when a worker
  still holds a context clone.
- The exclusive-open probe fails while any connection remains, and succeeds once
  teardown completes.
- Context reinitialization restores a working app after a rolled-back
  replacement, with no restart.
- Enable/disable round-trip preserves all data; a forced failure mid-enable
  leaves the plaintext database intact **and the key retained** for retry.
- **After a successful enable, no plaintext copy survives anywhere** — neither
  the replaced file nor the pre-operation backup. Assert on directory contents,
  not just on `app.db`.
- After a disable, the key is **still present** and the plaintext database opens
  via the unkeyed fallback.
- An encrypted internal backup taken **before** a disable is still openable
  afterward — the regression that key deletion would have caused.
- Re-enable after a disable reuses the same key, and pre-disable encrypted
  backups remain openable.
- Two encrypted devices with **different** keys exchange sync snapshots
  successfully.

## Files Expected to Change (reference; nothing changed yet)

- `Cargo.toml` / `crates/storage-sqlite/Cargo.toml` — `libsqlite3-sys` SQLCipher
  feature.
- `crates/storage-sqlite/src/db/mod.rs` — `DbEncryptionKey`, key application at all six
  open sites, probe detection, split backup APIs, candidate build/verify/replace
  primitives.
- `crates/storage-sqlite/src/db/` (new) — `DatabaseMaintenanceCoordinator`,
  including candidate build/verify, the three-check zero-connection proof,
  atomic replace, and rollback.
- `crates/storage-sqlite/src/db/write_actor.rs` — shutdown signal, retained
  `JoinHandle`, awaited teardown so its pooled connection is released.
- `apps/tauri/src/context/` — new stable `DatabaseRuntime` owning
  `Mutex<Option<Arc<ServiceContext>>>`, the maintenance gate, and context
  reinitialization. **Repository constructors are unchanged**; `providers.rs`
  gains a rebuildable construction path rather than 36 signature edits.
- `crates/storage-sqlite/src/sync/app_sync/repository.rs` — `KEY ''` on both
  snapshot attachments; relocate the temp snapshot.
- `apps/tauri/src/commands/device_sync/snapshot.rs` and
  `apps/server/src/api/device_sync_engine.rs` — app-private temp snapshots,
  deleted on every exit path (with `sync/app_sync/repository.rs`, three sites
  total).
- `apps/tauri/src/commands/utilities.rs` — restore delegates to the coordinator;
  unconditional desktop restart replaces the declinable prompt; explicit iOS
  runtime-rebuild-or-block branch.
- `crates/storage-sqlite/src/settings/repository.rs` + core settings model —
  encryption-intent setting.
- `apps/tauri/` — keychain `KeyProvider`, settings command cfg-split, context
  wiring.
- `apps/server/` — HKDF `KeyProvider`, `WF_DB_REQUIRE_ENCRYPTION`, disable command,
  `build_state`.
- `apps/frontend/` — settings toggle card, TS `Settings` type, adapter, export
  warning.
- `Dockerfile` — add `perl` to the builder stage.
- `docs/self-host/README.md` — encrypted-server setup; coherent rotation of
  JWT + secrets + DB.
- `apps/tauri/gen/apple/.../Info.ios.plist` + compliance sign-off.

No implementation estimate is given: restore staging and device-sync integration
must be scoped against the real code first.
