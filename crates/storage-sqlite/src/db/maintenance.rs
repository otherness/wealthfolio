//! Whole-file database maintenance: restore, enable encryption, disable
//! encryption.
//!
//! All three are the same operation — replace `app.db` with a verified candidate
//! while nothing is connected — so one implementation serves all three.
//!
//! The work runs in process and finishes *before* the caller's intentional
//! restart. Normal startup therefore never inspects candidate files, pending
//! markers or staged restores: it opens `app.db` and nothing else. Any candidate
//! left behind by a crash is inert, and is swept the next time maintenance runs.
//!
//! # Precondition
//!
//! The caller must have torn the database runtime down first: stopped background
//! workers, joined the write actor, and dropped every pooled and standalone
//! connection. [`run`] proves that independently with an exclusive-open probe and
//! aborts without touching `app.db` if the proof fails — aborting is always safe,
//! proceeding on an assumption is not.

use log::{error, info, warn};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

use rusqlite::Connection as RusqliteConnection;
use wealthfolio_core::errors::{DatabaseError, Error, Result};

use super::{
    backup_database_to_file, copy_database, create_backup_filename, probe, remove_database_files,
    remove_database_sidecars, verify_key, DbAccess, DbEncryptionKey,
};

/// Marks the files this module creates beside the live database. Startup ignores
/// them; maintenance sweeps them when it next begins.
const CANDIDATE_MARKER: &str = ".maintenance-";

/// What to replace the live database with.
pub enum MaintenanceRequest {
    /// Reinstate the contents of a backup file, landing on this device's current
    /// encryption state regardless of the backup's own.
    Restore {
        backup_path: PathBuf,
        /// The key this device holds, tried when the backup turns out to be
        /// encrypted. It is deliberately *not* the live database's key: the key
        /// is retained across a disable, so a plaintext device must still be
        /// able to open backups taken while it was encrypted.
        device_key: Option<Arc<DbEncryptionKey>>,
    },
    /// Convert the live database to encrypted under `key`.
    Enable { key: Arc<DbEncryptionKey> },
    /// Convert the live database back to plaintext.
    Disable,
}

impl MaintenanceRequest {
    fn label(&self) -> &'static str {
        match self {
            Self::Restore { .. } => "restore",
            Self::Enable { .. } => "enable-encryption",
            Self::Disable => "disable-encryption",
        }
    }

    /// The encryption state the database must be in when the operation finishes.
    fn target_key(&self, current: &DbAccess) -> Option<Arc<DbEncryptionKey>> {
        match self {
            // A restored backup never overrides the device's encryption policy.
            Self::Restore { .. } => current.key().cloned(),
            Self::Enable { key } => Some(Arc::clone(key)),
            Self::Disable => None,
        }
    }
}

pub struct MaintenanceOutcome {
    /// How to reopen the database now that the operation has completed.
    pub access: DbAccess,
    /// The pre-operation backup, when one is still on disk. Enabling encryption
    /// deletes its own, because that copy is plaintext — so a path here after an
    /// enable means the deletion failed and a readable copy remains.
    pub pre_operation_backup: Option<String>,
}

/// Replaces `app.db` with a verified candidate.
///
/// Ordering matters and is not negotiable: every step that can fail runs before
/// anything destructive, and the pre-operation backup exists before the atomic
/// replace so a failed verification can be rolled back.
pub fn run(
    app_data_dir: &str,
    current: &DbAccess,
    request: MaintenanceRequest,
) -> Result<MaintenanceOutcome> {
    let db_path = PathBuf::from(current.path());
    let workspace = Workspace::new(&db_path)?;
    workspace.sweep_stale_candidates();

    info!(
        "Starting database maintenance: {} ({} -> {})",
        request.label(),
        state_label(current.is_encrypted()),
        state_label(request.target_key(current).is_some()),
    );

    // Step 1: prove nothing is connected. Aborting here has touched nothing.
    prove_exclusive_access(current)?;

    let target_key = request.target_key(current);
    let candidate = DbAccess::new(path_str(&workspace.candidate)?, target_key.clone());

    // Steps 2-6: build and verify the candidate. Everything here is recoverable
    // by deleting the candidate; the live database has not been touched.
    let build = build_candidate(&request, current, &candidate, &workspace);
    if let Err(e) = build {
        workspace.discard();
        return Err(e);
    }

    // Step 7: a consistent pre-operation backup, the rollback artifact. It is a
    // faithful copy, so it inherits the source database's encryption.
    //
    // Enabling encryption puts its copy in the scratch directory rather than in
    // `backups/`: that copy is plaintext, it is deleted at step 10, and startup
    // clears scratch — so a crash between the install and that deletion cannot
    // leave a complete readable copy of the database sitting beside the
    // encrypted one for good. Every other operation writes a backup the user is
    // meant to keep, which belongs in `backups/`.
    let enabling = matches!(request, MaintenanceRequest::Enable { .. });
    let pre_operation_backup = if enabling {
        // Creates the directory owner-only, which matters more here than for a
        // snapshot: this file is the whole database in the clear.
        super::scratch_dir_beside(&db_path)?;
        path_str(&workspace.pre_operation)?.to_string()
    } else {
        create_unused_backup_path(app_data_dir)?
    };
    if let Err(e) = backup_database_to_file(current, &pre_operation_backup) {
        workspace.discard();
        return Err(e);
    }
    info!("Pre-operation backup written to {}", pre_operation_backup);

    // Step 8: install the candidate. `fs::rename` over the live database is
    // atomic on the same filesystem, so a crash here leaves either the complete
    // old file or the complete new one.
    if let Err(e) = install(&workspace.candidate, &db_path) {
        workspace.discard();
        return Err(e);
    }

    // Step 9: reopen the installed file with the intended key and verify it.
    let installed = DbAccess::new(current.path(), target_key);
    if let Err(e) = verify_installed(&installed) {
        error!(
            "Verification of the installed database failed ({}); rolling back",
            e
        );
        roll_back(&workspace, &pre_operation_backup, current)?;
        return Err(e);
    }

    // Step 10: enabling encryption is the one operation that must clean up after
    // itself. Its source was plaintext, so the pre-operation backup is a
    // complete readable copy of every account, holding and transaction — leaving
    // it behind would defeat the feature for the user who just enabled it. The
    // replaced file is already gone, consumed by the atomic rename above.
    //
    // A failure here is reported, not propagated: the new database is installed
    // and verified, and returning an error now would make the caller reopen an
    // encrypted file with the outgoing plaintext key.
    let pre_operation_backup = if enabling {
        match remove_database_files(&pre_operation_backup) {
            Ok(()) => {
                info!("Removed the plaintext pre-operation backup after verification");
                None
            }
            Err(e) => {
                error!(
                    "The database is encrypted, but the plaintext pre-operation backup at {} \
                     could not be removed ({}). Delete it manually.",
                    pre_operation_backup, e
                );
                Some(pre_operation_backup)
            }
        }
    } else {
        Some(pre_operation_backup)
    };

    info!("Database maintenance completed: {}", request.label());
    Ok(MaintenanceOutcome {
        access: installed,
        pre_operation_backup,
    })
}

/// Proves that no connection anywhere still holds the database, by taking
/// `PRAGMA locking_mode = EXCLUSIVE` and a write transaction.
///
/// The platforms fail differently without this. On Windows an atomic replace
/// fails outright with a sharing violation while any handle is open — loud, but
/// only on Windows. On POSIX the replace *succeeds* and leaves surviving
/// connections reading and writing the old, orphaned inode: silent divergence,
/// which is worse.
pub fn prove_exclusive_access(access: &DbAccess) -> Result<()> {
    let conn = access.connect_rusqlite()?;
    conn.execute_batch(
        "PRAGMA locking_mode = EXCLUSIVE;
         BEGIN IMMEDIATE;
         COMMIT;",
    )
    .map_err(|e| {
        Error::Database(DatabaseError::TransactionFailed(format!(
            "Database maintenance aborted: connections to {} are still open ({e}). \
             Nothing was modified.",
            access.path()
        )))
    })
}

fn build_candidate(
    request: &MaintenanceRequest,
    current: &DbAccess,
    candidate: &DbAccess,
    workspace: &Workspace,
) -> Result<()> {
    match request {
        MaintenanceRequest::Restore {
            backup_path,
            device_key,
        } => {
            // Never modify or consume the user's backup: work on a scratch copy,
            // which also avoids read-only-open edge cases on WAL-mode files.
            if !backup_path.exists() {
                return Err(Error::Database(DatabaseError::RestoreFailed(format!(
                    "Backup file not found: {}",
                    backup_path.display()
                ))));
            }
            stage_backup(backup_path, &workspace.scratch)?;

            // A backup may be plaintext (a portable export) or encrypted with
            // this device's key (an internal or pre-operation backup).
            let source = probe(
                path_str(&workspace.scratch)?,
                device_key.clone().or_else(|| current.key().cloned()),
            )?;
            let conn = source.connect_rusqlite()?;
            verify_key(&conn)?;
            integrity_check(&conn)?;
            drop(conn);

            copy_database(&source, candidate.path(), candidate.key().map(Arc::as_ref))?;
            remove_database_files(path_str(&workspace.scratch)?)?;
        }
        MaintenanceRequest::Enable { .. } | MaintenanceRequest::Disable => {
            copy_database(current, candidate.path(), candidate.key().map(Arc::as_ref))?;
        }
    }

    verify_candidate(candidate)
}

/// Copies a backup into the scratch slot, sidecars included.
///
/// Not every backup is self-contained. The app's own exports are checkpointed
/// copies, but a backup that is a plain file copy of a live database — the
/// `.pre-restore-*` artifacts older versions wrote, or a user's own copy — keeps
/// its most recent transactions in `-wal`. Staging the main file alone leaves a
/// database that opens and passes its integrity check at the last checkpoint,
/// so those transactions would be dropped with no error anywhere.
fn stage_backup(backup_path: &Path, scratch: &Path) -> Result<()> {
    let staged = |e: std::io::Error| {
        Error::Database(DatabaseError::RestoreFailed(format!(
            "Failed to stage the selected backup: {e}"
        )))
    };

    fs::copy(backup_path, scratch).map_err(staged)?;

    for suffix in ["-wal", "-shm"] {
        let sidecar = sidecar_path(backup_path, suffix);
        if sidecar.exists() {
            fs::copy(&sidecar, sidecar_path(scratch, suffix)).map_err(staged)?;
        }
    }

    Ok(())
}

/// `path` with `suffix` appended to its file name, the way SQLite names the WAL
/// and shared-memory files beside a database.
fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Levels 1-3 on the candidate, then flush it to disk.
fn verify_candidate(candidate: &DbAccess) -> Result<()> {
    let conn = candidate.connect_rusqlite()?;
    verify_key(&conn)?;
    integrity_check(&conn)?;
    if candidate.is_encrypted() {
        cipher_integrity_check(&conn)?;
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap_or_else(|e| warn!("WAL checkpoint of the candidate failed: {}", e));
    drop(conn);

    fsync_file(Path::new(candidate.path()))
}

fn verify_installed(installed: &DbAccess) -> Result<()> {
    let conn = installed.connect_rusqlite()?;
    verify_key(&conn)
}

/// Level 2: standard SQLite structural integrity. A healthy database returns
/// **exactly one row containing `ok`**; anything else is a failure.
fn integrity_check(conn: &RusqliteConnection) -> Result<()> {
    let rows = pragma_rows(conn, "PRAGMA integrity_check;")?;
    if rows.len() == 1 && rows[0].eq_ignore_ascii_case("ok") {
        return Ok(());
    }
    Err(Error::Database(DatabaseError::RestoreFailed(format!(
        "Database failed its integrity check: {}",
        rows.join("; ")
    ))))
}

/// Level 3: SQLCipher page-HMAC verification, for encrypted databases only.
///
/// **Success is signalled by returning no rows at all.** Applying level 2's
/// "one row saying `ok`" condition here would report every healthy encrypted
/// database as corrupt.
fn cipher_integrity_check(conn: &RusqliteConnection) -> Result<()> {
    let rows = pragma_rows(conn, "PRAGMA cipher_integrity_check;")?;
    if rows.is_empty() {
        return Ok(());
    }
    Err(Error::Database(DatabaseError::Encryption(format!(
        "Encrypted database failed its cipher integrity check: {}",
        rows.join("; ")
    ))))
}

fn pragma_rows(conn: &RusqliteConnection, sql: &str) -> Result<Vec<String>> {
    let mut statement = conn
        .prepare(sql)
        .map_err(|e| Error::Database(DatabaseError::QueryFailed(e.to_string())))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<String>>>())
        .map_err(|e| Error::Database(DatabaseError::QueryFailed(e.to_string())))?;
    Ok(rows)
}

fn install(candidate: &Path, db_path: &Path) -> Result<()> {
    // The outgoing WAL and shared-memory files describe the outgoing file. They
    // must not survive to be replayed against the incoming one. `app.db` itself
    // is *not* removed: the rename replaces it in one step, so a crash here
    // leaves either the complete old file or the complete new one. Deleting it
    // first would open a window where there is no database at all.
    remove_database_sidecars(path_str(db_path)?)?;

    fs::rename(candidate, db_path).map_err(|e| {
        Error::Database(DatabaseError::RestoreFailed(format!(
            "Failed to install the verified database: {e}"
        )))
    })?;
    fsync_parent_dir(db_path);
    Ok(())
}

/// Reinstates the pre-operation backup and confirms it opens.
///
/// A rollback that leaves an unopenable database is worse than the failure it is
/// recovering from, so it stages the copy and installs it through the same
/// atomic rename, then reports its own failure loudly.
fn roll_back(workspace: &Workspace, pre_operation_backup: &str, original: &DbAccess) -> Result<()> {
    let staged = &workspace.rollback;
    fs::copy(pre_operation_backup, staged).map_err(|e| {
        Error::Database(DatabaseError::RestoreFailed(format!(
            "Rollback failed: could not stage {pre_operation_backup}: {e}. \
             The database is unusable; restore this file manually."
        )))
    })?;
    fsync_file(staged)?;
    install(staged, Path::new(original.path()))?;

    let conn = original.connect_rusqlite()?;
    verify_key(&conn)?;
    info!("Rolled back to the pre-operation backup");
    Ok(())
}

/// Uniquely named scratch files beside the live database, so that a crash can
/// never leave a file whose name a later run would mistake for its own.
struct Workspace {
    dir: PathBuf,
    file_name: String,
    candidate: PathBuf,
    scratch: PathBuf,
    rollback: PathBuf,
    /// Where an enable puts its pre-operation backup: inside the scratch
    /// directory, which startup clears. That copy is plaintext and is deleted
    /// once the encrypted database verifies, so the only way it outlives the
    /// operation is a crash — and then it must not survive the next launch.
    pre_operation: PathBuf,
}

impl Workspace {
    fn new(db_path: &Path) -> Result<Self> {
        let dir = db_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let file_name = db_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::Database(DatabaseError::RestoreFailed(format!(
                    "Database path has no file name: {}",
                    db_path.display()
                )))
            })?
            .to_string();

        let token = Uuid::new_v4();
        Ok(Self {
            candidate: dir.join(format!("{file_name}{CANDIDATE_MARKER}{token}.new")),
            scratch: dir.join(format!("{file_name}{CANDIDATE_MARKER}{token}.src")),
            rollback: dir.join(format!("{file_name}{CANDIDATE_MARKER}{token}.rollback")),
            pre_operation: dir
                .join(super::SCRATCH_DIR_NAME)
                .join(format!("{file_name}{CANDIDATE_MARKER}{token}.pre")),
            dir,
            file_name,
        })
    }

    /// Removes candidates left behind by an interrupted run. Startup never does
    /// this: candidates are inert, and sweeping them is maintenance's job.
    fn sweep_stale_candidates(&self) {
        let prefix = format!("{}{}", self.file_name, CANDIDATE_MARKER);
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };

        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if !name.starts_with(&prefix) {
                continue;
            }
            if let Err(e) = fs::remove_file(entry.path()) {
                warn!("Failed to sweep stale maintenance file {}: {}", name, e);
            } else {
                info!("Swept stale maintenance file {}", name);
            }
        }
    }

    fn discard(&self) {
        for path in [
            &self.candidate,
            &self.scratch,
            &self.rollback,
            &self.pre_operation,
        ] {
            if let Some(path) = path.to_str() {
                let _ = remove_database_files(path);
            }
        }
    }
}

/// A backup path in the standard directory that does not overwrite an existing
/// backup. `create_backup_path` alone resolves to the second, and the rollback
/// artifact must never clobber a backup the user just took.
fn create_unused_backup_path(app_data_dir: &str) -> Result<String> {
    let backup_dir = Path::new(app_data_dir).join("backups");
    fs::create_dir_all(&backup_dir).map_err(|e| {
        error!("Failed to create backup directory: {}", e);
        Error::Database(DatabaseError::BackupFailed(e.to_string()))
    })?;

    let mut timestamp = chrono::Local::now();
    for _ in 0..60 {
        let candidate = backup_dir.join(create_backup_filename(timestamp));
        if !candidate.exists() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
        timestamp += chrono::Duration::seconds(1);
    }

    Err(Error::Database(DatabaseError::BackupFailed(
        "Could not find an unused backup filename".to_string(),
    )))
}

fn fsync_file(path: &Path) -> Result<()> {
    // Must be opened writable: on Windows `sync_all` is `FlushFileBuffers`,
    // which requires GENERIC_WRITE and fails with ACCESS_DENIED on the
    // read-only handle `File::open` produces.
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| {
            Error::Database(DatabaseError::BackupFailed(format!(
                "Failed to flush {}: {e}",
                path.display()
            )))
        })
}

/// Flushing the directory entry is what makes the rename durable. Windows has no
/// equivalent and does not need one.
fn fsync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Err(e) = fs::File::open(dir).and_then(|file| file.sync_all()) {
            warn!("Failed to flush directory {}: {}", dir.display(), e);
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Database(DatabaseError::RestoreFailed(format!(
            "Path is not valid UTF-8: {}",
            path.display()
        )))
    })
}

fn state_label(encrypted: bool) -> &'static str {
    if encrypted {
        "encrypted"
    } else {
        "plaintext"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A migrated database with one recognisable row, in the requested state.
    fn seeded_database(dir: &TempDir, key: Option<Arc<DbEncryptionKey>>) -> DbAccess {
        let db_path = dir.path().join("app.db");
        let access = DbAccess::new(db_path.to_str().unwrap(), key);
        access.prepare().unwrap();
        access.run_migrations().unwrap();
        set_setting(&access, "base_currency", "CAD");
        access
    }

    fn set_setting(access: &DbAccess, key: &str, value: &str) {
        let conn = access.connect_rusqlite().unwrap();
        conn.execute(
            "INSERT INTO app_settings (setting_key, setting_value) VALUES (?1, ?2) \
             ON CONFLICT(setting_key) DO UPDATE SET setting_value = excluded.setting_value",
            rusqlite::params![key, value],
        )
        .unwrap();
    }

    fn read_setting(access: &DbAccess, key: &str) -> Option<String> {
        let conn = access.connect_rusqlite().unwrap();
        conn.query_row(
            "SELECT setting_value FROM app_settings WHERE setting_key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn enable_encrypts_in_place_and_leaves_no_plaintext_copy() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let key = Arc::new(DbEncryptionKey::generate());

        let outcome = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Enable {
                key: Arc::clone(&key),
            },
        )
        .expect("enable");

        assert!(outcome.access.is_encrypted());
        assert_eq!(
            read_setting(&outcome.access, "base_currency").as_deref(),
            Some("CAD")
        );

        // The plaintext file must no longer open without the key.
        assert!(DbAccess::plaintext(access.path())
            .connect_rusqlite()
            .and_then(|conn| verify_key(&conn))
            .is_err());

        // Neither the replaced file nor the pre-operation backup survives.
        assert_eq!(outcome.pre_operation_backup, None);
        assert!(
            !dir.path().join("backups").exists()
                || file_names(&dir.path().join("backups")).is_empty(),
            "the plaintext pre-operation backup must be deleted"
        );
        assert!(
            file_names(&dir.path().join(super::super::SCRATCH_DIR_NAME)).is_empty(),
            "the plaintext pre-operation backup must not linger in scratch either"
        );
    }

    #[test]
    fn an_interrupted_enable_leaves_its_plaintext_copy_where_startup_sweeps_it() {
        // The pre-operation backup of an enable is a complete readable copy of
        // the database. It is deleted once the encrypted file verifies, so the
        // only way it outlives the operation is a crash in that window — and
        // then it must be somewhere startup clears, not in `backups/`, which
        // nothing ever sweeps.
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let workspace = Workspace::new(Path::new(access.path())).unwrap();

        assert!(
            workspace
                .pre_operation
                .starts_with(dir.path().join(super::super::SCRATCH_DIR_NAME)),
            "an enable's pre-operation backup must live in the scratch directory"
        );

        // Stand in for the crash: the copy exists, the deletion never ran.
        super::super::scratch_dir_beside(Path::new(access.path())).unwrap();
        backup_database_to_file(&access, workspace.pre_operation.to_str().unwrap()).unwrap();
        assert!(workspace.pre_operation.exists());

        super::super::purge_scratch_dir(Path::new(access.path()));

        assert!(
            !workspace.pre_operation.exists(),
            "startup must clear a plaintext copy an interrupted enable left behind"
        );
    }

    #[test]
    fn disable_decrypts_and_retains_encrypted_pre_operation_backup() {
        let dir = TempDir::new().unwrap();
        let key = Arc::new(DbEncryptionKey::generate());
        let access = seeded_database(&dir, Some(Arc::clone(&key)));

        let outcome = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Disable,
        )
        .expect("disable");

        assert!(!outcome.access.is_encrypted());
        assert_eq!(
            read_setting(&outcome.access, "base_currency").as_deref(),
            Some("CAD")
        );

        // The pre-operation backup inherited the encrypted source, and stays
        // openable precisely because the key is never deleted.
        let backup = outcome.pre_operation_backup.expect("retained backup");
        let backup_access = DbAccess::encrypted(&backup, key);
        let conn = backup_access.connect_rusqlite().unwrap();
        verify_key(&conn).expect("the encrypted pre-disable backup must still open");
    }

    #[test]
    fn enable_then_disable_round_trip_reuses_the_same_key() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let key = Arc::new(DbEncryptionKey::generate());

        let encrypted = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Enable {
                key: Arc::clone(&key),
            },
        )
        .unwrap()
        .access;
        let plaintext = run(
            dir.path().to_str().unwrap(),
            &encrypted,
            MaintenanceRequest::Disable,
        )
        .unwrap()
        .access;
        let re_enabled = run(
            dir.path().to_str().unwrap(),
            &plaintext,
            MaintenanceRequest::Enable {
                key: Arc::clone(&key),
            },
        )
        .unwrap()
        .access;

        assert_eq!(
            read_setting(&re_enabled, "base_currency").as_deref(),
            Some("CAD")
        );
    }

    #[test]
    fn restores_an_encrypted_backup_onto_a_device_that_has_since_disabled() {
        let dir = TempDir::new().unwrap();
        let key = Arc::new(DbEncryptionKey::generate());

        // Device was encrypted; an internal backup was taken then.
        let encrypted = seeded_database(&dir, Some(Arc::clone(&key)));
        let backup = dir.path().join("while-encrypted.db");
        backup_database_to_file(&encrypted, backup.to_str().unwrap()).unwrap();

        // The user then disables encryption. The key is retained in the keychain.
        let plaintext = run(
            dir.path().to_str().unwrap(),
            &encrypted,
            MaintenanceRequest::Disable,
        )
        .unwrap()
        .access;
        assert!(!plaintext.is_encrypted());

        // Restoring that encrypted backup must still work: the key is retained
        // precisely so backups taken before the disable stay openable.
        let outcome = run(
            dir.path().to_str().unwrap(),
            &plaintext,
            MaintenanceRequest::Restore {
                backup_path: backup,
                device_key: Some(key),
            },
        )
        .expect("an encrypted backup must restore onto a plaintext device");

        assert!(
            !outcome.access.is_encrypted(),
            "the restore lands on the device's policy, which is now plaintext"
        );
        assert_eq!(
            read_setting(&outcome.access, "base_currency").as_deref(),
            Some("CAD")
        );
    }

    #[test]
    fn restore_lands_on_the_device_policy_not_the_backups() {
        let dir = TempDir::new().unwrap();
        let key = Arc::new(DbEncryptionKey::generate());
        let access = seeded_database(&dir, Some(Arc::clone(&key)));

        // A plaintext (portable) backup carrying a different value and the
        // opposite encryption flag.
        let backup_dir = TempDir::new().unwrap();
        let backup_path = backup_dir.path().join("portable.db");
        let plaintext_source = seeded_database(&backup_dir, None);
        set_setting(&plaintext_source, "base_currency", "EUR");
        super::super::export_portable_backup(&plaintext_source, backup_path.to_str().unwrap())
            .unwrap();
        let backup_bytes_before = fs::read(&backup_path).unwrap();

        let outcome = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Restore {
                backup_path: backup_path.clone(),
                device_key: None,
            },
        )
        .expect("restore");

        assert!(
            outcome.access.is_encrypted(),
            "a plaintext backup must not flip an encrypted device to plaintext"
        );
        assert_eq!(
            read_setting(&outcome.access, "base_currency").as_deref(),
            Some("EUR")
        );
        assert_eq!(
            fs::read(&backup_path).unwrap(),
            backup_bytes_before,
            "the user's backup must never be modified or consumed"
        );
    }

    #[test]
    fn restore_reads_an_encrypted_internal_backup() {
        let dir = TempDir::new().unwrap();
        let key = Arc::new(DbEncryptionKey::generate());
        let access = seeded_database(&dir, Some(Arc::clone(&key)));

        let backup_path = dir.path().join("internal.db");
        backup_database_to_file(&access, backup_path.to_str().unwrap()).unwrap();
        set_setting(&access, "base_currency", "USD");

        let outcome = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Restore {
                backup_path,
                device_key: None,
            },
        )
        .expect("restore");

        assert!(outcome.access.is_encrypted());
        assert_eq!(
            read_setting(&outcome.access, "base_currency").as_deref(),
            Some("CAD")
        );
    }

    #[test]
    fn maintenance_aborts_untouched_while_a_connection_is_open() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let before = fs::read(access.path()).unwrap();

        let _held = access.connect_rusqlite().unwrap();
        // Force the held connection to actually take a shared lock.
        verify_key(&_held).unwrap();

        let result = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Enable {
                key: Arc::new(DbEncryptionKey::generate()),
            },
        );

        assert!(result.is_err(), "maintenance must refuse to proceed");
        assert_eq!(
            fs::read(access.path()).unwrap(),
            before,
            "app.db must be byte-identical after an aborted operation"
        );
    }

    #[test]
    fn restore_keeps_transactions_that_live_only_in_the_backups_wal() {
        // Not every backup is self-contained. A plain file copy of a live
        // database — the `.pre-restore-*` artifacts older versions wrote, or a
        // user's own copy — keeps its newest transactions in `-wal`. Staging
        // the main file alone yields a database that opens and passes its
        // integrity check at the last checkpoint, so those transactions would
        // be dropped with nothing reporting it.
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);

        // An open read transaction pins the WAL at the pre-write snapshot, so
        // the checkpointer cannot fold the next write into the main file.
        let holder = access.connect_rusqlite().unwrap();
        holder
            .execute_batch("BEGIN; SELECT count(*) FROM app_settings;")
            .unwrap();
        set_setting(&access, "base_currency", "JPY");

        let backup = dir.path().join("copied-live.db");
        let backup_path = backup.to_str().unwrap().to_string();
        fs::copy(access.path(), &backup).unwrap();
        let source_wal = format!("{}-wal", access.path());
        assert!(
            Path::new(&source_wal).exists(),
            "the write must still be in the WAL for this test to mean anything"
        );
        fs::copy(&source_wal, format!("{backup_path}-wal")).unwrap();
        drop(holder);

        let outcome = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Restore {
                backup_path: backup,
                device_key: None,
            },
        )
        .expect("restore");

        // The copied main file alone still says CAD; only the staged WAL has JPY.
        assert_eq!(
            read_setting(&outcome.access, "base_currency").as_deref(),
            Some("JPY"),
            "a restore must not silently drop the backup's WAL-resident writes"
        );
    }

    #[test]
    fn restore_rejects_a_corrupt_backup_before_touching_the_database() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let before = fs::read(access.path()).unwrap();

        let backup_path = dir.path().join("corrupt.db");
        fs::write(&backup_path, b"SQLite format 3\0 not really a database").unwrap();

        let result = run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Restore {
                backup_path,
                device_key: None,
            },
        );

        assert!(result.is_err());
        assert_eq!(fs::read(access.path()).unwrap(), before);
    }

    #[test]
    fn cipher_integrity_check_treats_zero_rows_as_success() {
        // The inversion this guards against — asserting level 2's "one row
        // saying ok" — would report every healthy encrypted database as corrupt.
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, Some(Arc::new(DbEncryptionKey::generate())));
        let conn = access.connect_rusqlite().unwrap();

        assert!(
            pragma_rows(&conn, "PRAGMA cipher_integrity_check;")
                .unwrap()
                .is_empty(),
            "a healthy encrypted database returns no rows"
        );
        cipher_integrity_check(&conn).expect("zero rows is the pass condition");
    }

    #[test]
    fn integrity_check_requires_exactly_one_ok_row() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let conn = access.connect_rusqlite().unwrap();

        assert_eq!(
            pragma_rows(&conn, "PRAGMA integrity_check;").unwrap(),
            vec!["ok".to_string()]
        );
        integrity_check(&conn).expect("one ok row is the pass condition");
    }

    #[test]
    fn rollback_reinstates_the_pre_operation_backup() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);
        let workspace = Workspace::new(Path::new(access.path())).unwrap();

        let backup = create_unused_backup_path(dir.path().to_str().unwrap()).unwrap();
        backup_database_to_file(&access, &backup).unwrap();

        // Stand in for a half-installed database that fails verification.
        remove_database_files(access.path()).unwrap();
        fs::write(access.path(), b"not a database").unwrap();

        roll_back(&workspace, &backup, &access).expect("rollback");

        assert_eq!(
            read_setting(&access, "base_currency").as_deref(),
            Some("CAD")
        );
        assert!(
            !workspace.rollback.exists(),
            "staging file must be consumed"
        );
    }

    #[test]
    fn stale_candidates_are_swept_when_maintenance_begins() {
        let dir = TempDir::new().unwrap();
        let access = seeded_database(&dir, None);

        let stale = dir
            .path()
            .join(format!("app.db{CANDIDATE_MARKER}{}.new", Uuid::new_v4()));
        fs::write(&stale, b"leftover").unwrap();

        run(
            dir.path().to_str().unwrap(),
            &access,
            MaintenanceRequest::Enable {
                key: Arc::new(DbEncryptionKey::generate()),
            },
        )
        .unwrap();

        assert!(!stale.exists(), "stale candidates must be swept");
    }
}
