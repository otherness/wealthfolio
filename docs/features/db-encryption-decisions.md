# Database encryption — decision log and parking notes

**Status:** built, reviewed, fixed — **not shipped**. Parked pending a decision
on scope and on the two gaps in _Open proposals_ below.

**Origin:** [#441](https://github.com/wealthfolio/wealthfolio/issues/441).

This document is the context needed to pick the work back up cold. It records
what exists, what it protects, what it does not, the alternatives considered,
and the recommended sequence. `db-encryption-design.md` covers the mechanism;
this covers the decisions.

---

## 1. What is built

| Layer             | Behaviour                                                                                                                                                                            |
| ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------ |
| Cipher            | SQLCipher via `libsqlite3-sys` `bundled-sqlcipher-vendored-openssl`, always compiled in. With no key applied it behaves as plain SQLite, so there is one build configuration.        |
| Desktop / iOS key | 32 random bytes in the OS keychain (`keyring` 2.3.3). Transparent unlock, no prompt. Opt-in, off by default. Android unsupported (no persistent keyring backend).                    |
| Server key        | HKDF-SHA256 over `WF_SECRET_KEY`, info `"wealthfolio-db"`. Nothing stored, so a `.db` opens on any instance sharing the secret.                                                      |
| Server switch     | `WF_DB_REQUIRE_ENCRYPTION` — a _requirement_, not an action. Fails closed if it disagrees with the probed state of the file. Creation policy for a database that does not exist yet. |
| Conversion        | Offline `wealthfolio-server db encrypt                                                                                                                                               | decrypt`. In-process on desktop via `DatabaseRuntime`. |
| Detection         | Always by probing the file. A retained key never implies intent — it is deliberately kept after a disable.                                                                           |

**The `DatabaseRuntime` refactor stands on its own merits.** It replaced a
restore path that copied files with retry loops and could leave a stale WAL to
replay against the restored database. Atomic rename, verified candidates,
rollback, proven exclusive access — that is a correctness win for _restore_,
which every user touches, independent of encryption. Keep it regardless of what
happens to the feature.

---

## 2. Threat model — be precise about this

### What it protects

- A stolen or resold device, powered off.
- A copied database file.
- A leaked backup archive **on the server only** (see the matrix below).
- A disk image, VM snapshot, or decommissioned drive.
- macOS only: another application cannot silently read the key — the keychain
  ACL prompts for the login password for any binary other than the app.

### What it does not protect

- **Anyone at your unlocked machine.** They open Wealthfolio and see everything;
  transparent unlock is the design. This is the substantive difference from
  Portfolio Performance's model.
- **Windows/Linux: any process running as the user.** Credential Manager (DPAPI)
  and Secret Service have no per-application access control. Only macOS gates
  this.
- Host compromise on the server.
- **Desktop/iOS backups** — they are plaintext. See below.

### Relation to #441

The issue's first paragraph — _"anyone with access to the file can read its
contents"_ — **is** addressed on all platforms. The mechanism it proposed (_"set
a password… on first launch"_) is **not**; both commenters assumed a launch-time
prompt, and one cited Portfolio Performance explicitly.

So it is a partial match, not a mismatch. Reply on the issue saying which half
is covered and why, rather than closing it — and let the response tell you
whether the passphrase model has real demand. Demand signal so far is weak: one
👍, one requester, two commenters.

---

## 3. The backup matrix — the sharpest open problem

One **Backup** button, three code paths (`use-backup-restore.ts:46`,
`use-export-data.ts:51`):

| Platform | Calls                                 | Encrypted?                            |
| -------- | ------------------------------------- | ------------------------------------- |
| Web      | `db::backup_database` → faithful copy | **Yes**, when the server is encrypted |
| Desktop  | `db::export_portable_backup`          | **No — always plaintext**             |
| iOS      | `db::export_portable_backup`          | **No — always plaintext**             |

Consequences:

- **Desktop encryption protects the database at rest but not one artifact the
  user carries off the machine.** Enable encryption, press Backup, and a
  plaintext copy of everything lands in a folder that may well be synced.
- The plaintext export is currently the _accidental recovery net_ for a lost
  keychain. That matters for ordering — see §5.
- The server has **no portable export at all**, so an encrypted server's backups
  only restore on an instance sharing `WF_SECRET_KEY`.
- `commands::utilities::backup_database` (registered `lib.rs:477`) has **no
  reachable caller** — both hooks only call it under `isWeb`, which routes to
  the HTTP API. Either the desktop backup manager should use it, or delete it.

---

## 4. Open proposals

### 4a. Encrypted export on desktop — yes, but passphrase-based only

- **Do not** encrypt exports with the device key. That produces backups that
  restore only on that machine — the flaw the server already has.
- **Do** derive from a user passphrase: portable, no keychain dependency,
  matches what #441 actually pictured.
- Cheaper than it looks: SQLCipher's `PRAGMA key = 'text'` runs its own
  PBKDF2-HMAC-SHA512 (256k iterations) against the salt already in the file
  header. No Argon2, no salt management. Add a passphrase variant to
  `DbEncryptionKey`, which is hex-only today because it deliberately skips the
  KDF.
- Real work is UX plus restore: `probe()` needs a third outcome — _encrypted,
  key unknown, prompt for a passphrase_.
- Failure mode is **contained**: a forgotten passphrase kills one file, not the
  live database. Very different stakes from passphrase-on-launch.
- Arguably **higher marginal value than the database encryption itself on
  desktop**, since FileVault/BitLocker already cover the stolen-laptop case
  while nothing covers a file dropped in Dropbox.

### 4b. Recovery key export — treat as a blocker for desktop encryption

- **Today there is no recovery path at all.** Keychain lost → database
  unopenable → entire financial history gone.
- Realistic loss vectors: Windows profile/DPAPI loss or OS reinstall; Linux
  `~/.local/share/keyrings` lost; and the common one — the user copies `app.db`
  to a new machine and the key does not travel with it.
- The read-back check added in review only prevents _enabling_ against a broken
  keychain. It does nothing about losing it later.
- Precedent is strong: BitLocker and FileVault recovery keys, 1Password Secret
  Key.
- Shape: show once at enable time, explicit reveal, "save this somewhere safe",
  re-viewable in settings behind a confirmation. The clipboard risk is real and
  is strictly better than unrecoverable loss.

### Ordering matters

Ship 4a without 4b and you remove the accidental recovery net that the plaintext
export provides today. **4b before or with desktop encryption; 4a after.**

---

## 5. Recommended sequence

1. **This release — desktop only.** Encryption opt-in, plus 4b (recovery key),
   plus: a warning at the Backup button that the export is unencrypted, the
   export-warning string made conditional on `status.supported`, and the dead
   `backup_database` command resolved.
2. **Next release — server side.** After verifying Coolify and a clean Debian
   LXC, and after adding a uid-ownership guard (see §6).
3. **Later, on demand — passphrase export (4a)**, which also answers #441
   properly.
4. **Keep the `DatabaseRuntime` refactor regardless.**

Rationale for splitting: the server half carries by far the largest support
surface for a solo maintainer — fail-closed startup that can stop an instance,
five documented platforms, an offline CLI, the Unraid uid trap, an unverified
perl dependency, Coolify uncharted. Shipping desktop first shrinks the blast
radius of the release most likely to surface something unexpected.

**Note on an earlier misjudgement:** the server's _security_ benefit is not
thin. Backups are the most common way self-hosted data escapes, backup jobs
capture the data volume and not the orchestrator's env store, and the server's
backups do inherit encryption. The argument for deferring the server side is
operational, not cryptographic.

---

## 6. Server / platform notes

- **Unraid** — the CA template runs the container as `--user=99:100`, not the
  image's `1000:1000`. The one-shot conversion container must match, or the
  converted database and the owner-only `scratch/` directory end up owned by a
  user the server cannot use. **Worth a code-level guard:** after installing the
  converted file, compare its ownership against the directory and fail loudly
  with the fix. Also: Unraid's Console button execs into a _running_ container,
  which the conversion refuses to work against.
- **Unraid template repo** (`wealthfolio/wealthfolio-unraid`) needs
  `WF_DB_REQUIRE_ENCRYPTION` added as a Variable.
- **Proxmox community-scripts LXC** builds from source. Vendored OpenSSL is
  configured by a Perl script (`openssl/Configure` is `#!/usr/bin/env perl`), so
  perl must be present at build time. Alpine needed an explicit package; on
  Debian `perl-base` is Essential, so **verify on a clean LXC** whether the full
  `perl` package is required before asserting a failure. The durable fix is
  issue [#563](https://github.com/wealthfolio/wealthfolio/issues/563) — publish
  arm64 server prebuilds (`packaging/server-prebuild/` already does amd64) so
  that path stops compiling at all.
- **Coolify** — listed on the website, uncharted here. Needs a documented
  recipe: containers are recreated on redeploy, and the conversion needs the app
  stopped.
- **Vendored OpenSSL trade-off** — right for shipped artifacts (static musl,
  cross-compiled amd64/arm64, no runtime `libssl`; macOS/Windows have no usable
  system OpenSSL). The cost is that **OpenSSL CVEs become a release
  responsibility**. Want Dependabot on `openssl-src` or a release-time check. Do
  **not** add a Cargo feature to opt into system OpenSSL: it contradicts the
  one-build-configuration rule, and a rarely-built crypto config is a
  rarely-tested one.

---

## 7. `WF_SECRET_KEY` — the escalation nobody has documented yet

Before this feature, losing or changing it meant losing stored broker
credentials and invalidating sessions: bad, recoverable. **With encryption
enabled it is also the database key — losing it means the database is
permanently unopenable.**

Every doc still describes the old blast radius, and the docs teach
`-e WF_SECRET_KEY=$(openssl rand -base64 32)` _inline in `docker run`_, which
mints a fresh secret on every container recreate. Under encryption that is a
data-loss footgun.

- Fix the description at `README.md:276` and `:583`, `apps/server/README.md:21`,
  and the website's configuration page. It derives **three** keys now.
- Fix or annotate the inline examples: `README.md` lines 537, 552, 566, and the
  website's "Quick taste" one-liner.
- `packaging/server-prebuild/README.md:25` is **already correct** — the unquoted
  heredoc expands once into a persistent `.env`. That is the pattern the Docker
  examples should imitate.
- Consider decoupling: Rails' Active Record Encryption deliberately keeps
  encryption keys separate from `secret_key_base` and supports a key _list_ for
  rotation. A distinct `WF_DB_KEY`, falling back to the derived one, would make
  rotation incremental instead of the current four-step
  stop-decrypt-reconfigure-encrypt dance.

---

## 8. How comparable products solve this

- **Most self-hosted apps do not encrypt the database** (Nextcloud, Immich,
  Paperless-ngx, Home Assistant, Gitea, Miniflux). They document full-disk
  encryption. Where they encrypt, it is field-level for specific secrets.
  Shipping this opt-in and off by default is already above the norm.
- **Portfolio Performance** — file-based, whole-file AES under a user
  passphrase, prompted on open. Its data file travels (users sync it through
  consumer cloud storage), so a passphrase is the only thing that helps.
  Whole-file AES works because it loads everything into memory; our live SQLite
  needs page-level encryption, so the _mechanism_ difference is forced, not
  chosen. The _key-management_ difference is a genuine choice.
- **Signal Desktop** — SQLCipher plus OS keychain, the same shape as ours. Its
  history is the cautionary tale: the key sat in a plaintext `config.json` for
  years. These designs fail at key storage, not at the cipher.
- **Vaultwarden / Actual Budget** — no database encryption; the _payload_ is
  encrypted client-side, so the server cannot read it. Different threat model:
  protects against a compromised server, which at-rest encryption does not.
- **Django** — derives field encryption from `SECRET_KEY`, same coupling and
  same rotation drawback as our server model.

---

## 9. Review findings — all fixed on this branch

Ten findings from a high-effort review, all verified against the code:

1. `.gitignore` `db/` was unanchored and swallowed the new `db/encryption.rs`
   and `db/maintenance.rs` — the feature's core would never have been committed.
2. Keychain key was never read back before encrypting and deleting the plaintext
   rollback copy → permanent data loss on a non-persisting keyring.
3. Restore staged only the main file, dropping a backup's `-wal` sidecar and
   silently losing its newest transactions.
4. A missing/zero-length `app.db` was silently recreated plaintext despite an
   encryption opt-in → now recorded by a marker file beside the database.
5. A crash between install and cleanup left a full plaintext copy in `backups/`
   forever → an enable's pre-operation backup now goes to `scratch/`, which
   startup clears.
6. Encrypted server backups are unrestorable on desktop while the UI promised
   otherwise (string corrected; the underlying gap remains — see §3).
7. `db encrypt` on a missing path silently created an empty encrypted database
   and reported success → now refuses.
8. `bootstrap` purged the shared `scratch/` before proving exclusivity, so the
   offline CLI could delete a running server's in-flight snapshots → purging
   moved to startup paths only.
9. Whole-database conversion and backups blocked the async runtime → now
   `spawn_blocking`.
10. Windows sharing-violation retries had been dropped, so a transient AV lock
    aborted maintenance → bounded retry restored.

Also done: `DbKey` → `DbEncryptionKey` and `db/key.rs` → `db/encryption.rs`
("key" is overloaded in a Diesel crate); `WF_DB_ENCRYPTION` →
`WF_DB_REQUIRE_ENCRYPTION` (it asserts, it does not act); `access` folded into
`Live` so paired state sits under one lock; MCP teardown reordered so the server
is stopped _after_ the worker that starts it.

---

## 10. Still open

- Android reports `supported: false`, so the settings card shows self-hosted
  **server** instructions on a phone. `supported` is overloaded — it means both
  "server-managed" and "platform unsupported"; it needs a third state.
- The export warning renders unconditionally, so web users are told exports stay
  unencrypted, which is false there.
- The 11 new `database_encryption_*` i18n keys exist only in `en`; the other
  eight locales fall back to English.
- `commands::utilities::backup_database` appears unreachable (§3).
- Consider a `Ctx` command-argument extractor (`tauri::ipc::CommandArg`) to
  replace the ~290 copy-pasted `state.context()?` preludes. Deferred
  deliberately: it fixes no bug, and the subtle part is that extractor failures
  surface as invoke-time errors rather than the command's own error type. It
  pays for itself the day the gate's behaviour needs to change.
