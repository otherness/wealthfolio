//! The database runtime: a stable managed handle around a takeable
//! [`ServiceContext`].
//!
//! Replacing the database file requires that *nothing* is connected to it, and
//! the pool is `Arc`-cloned into every repository and service that
//! `ServiceContext` owns. Tauri state cannot be removed safely —
//! `Manager::unmanage` exists but is deprecated, and its own documentation warns
//! it leaves dangling references, prescribing instead exactly the shape used
//! here: a `Mutex` plus `Option::take`.
//!
//! So the *managed* value is this stable handle and the *contents* are what
//! maintenance takes. Repository constructors are untouched: they keep taking
//! `Arc<DbPool>`, and every one of those clones dies with the context.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use log::{error, info, warn};
use tauri::async_runtime::JoinHandle;
use tauri::AppHandle;
use wealthfolio_ai::ProviderApiError;
use wealthfolio_core::errors::{DatabaseError, Error, Result as CoreResult};
use wealthfolio_core::events::DomainEvent;
use wealthfolio_core::secrets::SecretStore;
use wealthfolio_storage_sqlite::db::{
    self,
    maintenance::{self, MaintenanceOutcome, MaintenanceRequest},
    DbAccess, DbEncryptionKey, EncryptionPolicy, KeyProvider, WriteHandle, WriterTask,
};

use crate::context::{initialize_context, ServiceContext};
use crate::secret_store::shared_secret_store;

/// Keychain entry holding this device's database key.
const DATABASE_KEY_SECRET: &str = "database_encryption_key";

/// How long teardown waits for in-flight commands to release the context before
/// giving up. Long enough for an ordinary query, short enough that a stuck one
/// still fails promptly.
const OWNERSHIP_WAIT: Duration = Duration::from_secs(3);
const OWNERSHIP_POLL: Duration = Duration::from_millis(50);

/// Marks that this device's database is meant to be encrypted.
///
/// Read only when the database file is missing, to decide how to create the
/// replacement. Nothing infers the *current* state from it: that is always
/// resolved by probing the file.
fn encryption_marker(db_path: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("{db_path}.encrypted"))
}

// ─────────────────────────────────────────────────────────────────────────────
// Unavailability
// ─────────────────────────────────────────────────────────────────────────────

/// Why a command cannot reach the database right now.
///
/// Commands surface this through `?`; the `From` impls below cover every error
/// type the command layer uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseUnavailable {
    /// The database file is being replaced. New work is rejected until it is.
    Maintenance,
    /// Startup has not finished, or it failed.
    NotInitialized,
}

impl std::fmt::Display for DatabaseUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Maintenance => f.write_str(
                "The database is being updated. Please wait for the operation to finish.",
            ),
            Self::NotInitialized => f.write_str("The database is not available."),
        }
    }
}

impl std::error::Error for DatabaseUnavailable {}

impl From<DatabaseUnavailable> for String {
    fn from(value: DatabaseUnavailable) -> Self {
        value.to_string()
    }
}

impl From<DatabaseUnavailable> for ProviderApiError {
    fn from(value: DatabaseUnavailable) -> Self {
        ProviderApiError::ProviderError {
            message: value.to_string(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Key provider
// ─────────────────────────────────────────────────────────────────────────────

/// Stores the database key in the OS keychain, alongside the app's other
/// secrets (`Security.framework` on macOS/iOS, Credential Manager on Windows,
/// the Secret Service on Linux).
pub struct KeychainKeyProvider {
    store: Arc<dyn SecretStore>,
}

impl KeychainKeyProvider {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
}

impl KeyProvider for KeychainKeyProvider {
    fn existing(&self) -> CoreResult<Option<DbEncryptionKey>> {
        match self.store.get_secret(DATABASE_KEY_SECRET)? {
            Some(value) if !value.trim().is_empty() => DbEncryptionKey::from_hex(&value).map(Some),
            _ => Ok(None),
        }
    }

    fn create(&self) -> CoreResult<DbEncryptionKey> {
        // Never replace a key that already exists. Internal and pre-operation
        // backups inherit the source database's encryption, so overwriting the
        // key would orphan every encrypted backup it opens. Replacing a key is
        // rotation, which Phase 1 does not do.
        if let Some(existing) = self.existing()? {
            return Ok(existing);
        }

        let key = DbEncryptionKey::generate();
        self.store.set_secret(DATABASE_KEY_SECRET, key.as_hex())?;

        // Read the key back before anything is encrypted with it. A backend
        // that accepts the write without persisting it — a session-only Secret
        // Service collection, a keyring shim — reports success here and returns
        // nothing after the restart, by which point the database is encrypted
        // and its plaintext pre-operation backup has been deleted. Failing now
        // costs the user a message; failing later costs them the database.
        match self.existing() {
            Ok(Some(stored)) if stored.as_hex() == key.as_hex() => {
                info!("Generated and stored a new database encryption key");
                Ok(key)
            }
            Ok(_) => Err(Error::Database(DatabaseError::Encryption(
                "The database key was not stored: this device's key store accepted it but did \
                 not keep it. Encryption was not enabled and the database is untouched."
                    .to_string(),
            ))),
            Err(e) => Err(Error::Database(DatabaseError::Encryption(format!(
                "The database key could not be read back after storing it ({e}). Encryption was \
                 not enabled and the database is untouched."
            )))),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Runtime
// ─────────────────────────────────────────────────────────────────────────────

/// Everything that holds a database handle, so that all of it can be released
/// together.
struct Live {
    /// Which database the rest of this struct is serving. It belongs here, not
    /// in a field of its own: the two are set and cleared together, and a
    /// location that outlived the services would describe a database nothing is
    /// connected to.
    access: DbAccess,
    context: Arc<ServiceContext>,
    writer: WriteHandle,
    writer_task: WriterTask,
    /// Background tasks holding context or service clones. Each must be
    /// cancellable and joinable, or sole ownership can never be proven.
    workers: Vec<JoinHandle<()>>,
}

pub struct DatabaseRuntime {
    app_data_dir: String,
    key_provider: Arc<dyn KeyProvider>,
    live: Mutex<Option<Live>>,
    /// Set for the duration of a maintenance operation. Rejects new database
    /// work and serialises maintenance with itself.
    maintenance: AtomicBool,
}

impl DatabaseRuntime {
    pub fn new(app_data_dir: String) -> Self {
        Self {
            app_data_dir,
            key_provider: Arc::new(KeychainKeyProvider::new(shared_secret_store())),
            live: Mutex::new(None),
            maintenance: AtomicBool::new(false),
        }
    }

    pub fn app_data_dir(&self) -> &str {
        &self.app_data_dir
    }

    /// The live services.
    ///
    /// Commands call this at the top of their body; the returned clone keeps the
    /// context alive for the duration of the call, which is why maintenance
    /// aborts rather than proceeding while a command is in flight.
    pub fn context(&self) -> std::result::Result<Arc<ServiceContext>, DatabaseUnavailable> {
        if self.maintenance.load(Ordering::SeqCst) {
            return Err(DatabaseUnavailable::Maintenance);
        }
        self.live
            .lock()
            .unwrap()
            .as_ref()
            .map(|live| Arc::clone(&live.context))
            .ok_or(DatabaseUnavailable::NotInitialized)
    }

    /// The context if the runtime is up, for callers that must degrade rather
    /// than fail (event listeners, shutdown hooks).
    pub fn try_context(&self) -> Option<Arc<ServiceContext>> {
        self.context().ok()
    }

    /// Where the database is and how it is encrypted, for commands that open the
    /// file directly rather than going through a service — backups, mainly.
    ///
    /// Gated exactly like [`DatabaseRuntime::context`]: a backup started while
    /// the file is being replaced would pass its own open, then keep reading the
    /// outgoing inode after the rename and silently save stale data.
    pub fn access(&self) -> std::result::Result<DbAccess, DatabaseUnavailable> {
        if self.maintenance.load(Ordering::SeqCst) {
            return Err(DatabaseUnavailable::Maintenance);
        }
        self.current_access()
            .ok_or(DatabaseUnavailable::NotInitialized)
    }

    /// The database location regardless of the gate, for status reads and for
    /// maintenance itself, which runs *inside* the gate.
    ///
    /// `None` once the runtime is down: there is then no database this process
    /// is serving, and answering with the last one it served would be a guess.
    fn current_access(&self) -> Option<DbAccess> {
        self.live
            .lock()
            .unwrap()
            .as_ref()
            .map(|live| live.access.clone())
    }

    /// Whether the database file is currently encrypted. This is the truth the
    /// UI should show — the `app_settings` flag only records intent.
    pub fn is_encrypted(&self) -> bool {
        self.current_access()
            .map(|access| access.is_encrypted())
            .unwrap_or(false)
    }

    /// Opens the database and brings the runtime up.
    ///
    /// This is the whole of startup's database logic: probe `app.db`, build the
    /// services, start the workers. It never looks for candidate files or
    /// pending markers, because maintenance always runs to completion before the
    /// restart that follows it.
    pub async fn initialize(
        &self,
        handle: &AppHandle,
    ) -> std::result::Result<Arc<ServiceContext>, String> {
        let db_path = db::get_db_path(&self.app_data_dir);

        // Startup owns the database, so it is the one place allowed to clear the
        // scratch directory of snapshots a crash left behind.
        db::purge_scratch_dir(std::path::Path::new(&db_path));

        // Desktop and iOS are opt-in: a database that does not exist yet is
        // created plaintext, and only the explicit enable path mints a key.
        // Unless the user already opted in — a database that has gone missing
        // must not come back plaintext underneath them. The policy applies to
        // nothing else: an existing file is always resolved by probing.
        let policy = if encryption_marker(&db_path).exists() {
            EncryptionPolicy::Encrypted
        } else {
            EncryptionPolicy::Plaintext
        };

        let access = db::bootstrap(&db_path, self.key_provider.as_ref(), policy)
            .map_err(|e| e.to_string())?;

        self.install(handle, access).await
    }

    async fn install(
        &self,
        handle: &AppHandle,
        access: DbAccess,
    ) -> std::result::Result<Arc<ServiceContext>, String> {
        let init = initialize_context(&self.app_data_dir, &access)
            .await
            .map_err(|e| e.to_string())?;

        let context = Arc::new(init.context);
        let workers = start_workers(
            handle,
            &context,
            init.event_receiver,
            init.sync_outbox_wake_receiver,
        );

        self.record_encryption_state(&access);
        *self.live.lock().unwrap() = Some(Live {
            access,
            context: Arc::clone(&context),
            writer: init.writer,
            writer_task: init.writer_task,
            workers,
        });

        Ok(context)
    }

    /// Records on disk whether the database this runtime just opened is
    /// encrypted, so that [`DatabaseRuntime::initialize`] can honour the user's
    /// choice when the database file itself is gone.
    ///
    /// The retained key cannot stand in for this. It is deliberately kept after
    /// encryption is disabled, so its presence says nothing about what the user
    /// asked for — a key beside a plaintext database is a normal state.
    ///
    /// Best effort: a marker that cannot be written must not fail an operation
    /// that has otherwise succeeded. It is also rewritten from the observed
    /// state on every open, so a marker that disagrees with the file corrects
    /// itself rather than compounding.
    fn record_encryption_state(&self, access: &DbAccess) {
        let marker = encryption_marker(&db::get_db_path(&self.app_data_dir));
        let result = if access.is_encrypted() {
            std::fs::write(&marker, b"")
        } else {
            match std::fs::remove_file(&marker) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        };

        if let Err(e) = result {
            warn!(
                "Failed to record the database encryption state at {}: {}",
                marker.display(),
                e
            );
        }
    }

    /// Runs a maintenance operation: tear the runtime down, replace the database
    /// file, then bring the runtime back up.
    ///
    /// The runtime is rebuilt on both the success and the failure path. A
    /// rollback that leaves no live context would strand the app with no
    /// database at all, which is worse than the failure it recovered from.
    pub async fn run_maintenance(
        &self,
        handle: &AppHandle,
        request: MaintenanceRequest,
    ) -> std::result::Result<MaintenanceOutcome, String> {
        if self
            .maintenance
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("Database maintenance is already in progress.".to_string());
        }
        // Clear the gate on every exit, including a panic: leaving it set would
        // reject database work for the rest of the process's life.
        let _gate = MaintenanceGate(&self.maintenance);

        self.run_maintenance_inner(handle, request).await
    }

    async fn run_maintenance_inner(
        &self,
        handle: &AppHandle,
        request: MaintenanceRequest,
    ) -> std::result::Result<MaintenanceOutcome, String> {
        let access = self
            .current_access()
            .ok_or_else(|| DatabaseUnavailable::NotInitialized.to_string())?;

        let outcome = match self.teardown(handle).await {
            // Two whole-database copies and an integrity scan: seconds to
            // minutes on a large portfolio, and every byte of it blocking file
            // and SQL I/O. On the async runtime it would stall the worker it
            // landed on, including the event that tells the UI what is
            // happening and the fast rejection other commands are waiting for.
            Ok(()) => {
                let app_data_dir = self.app_data_dir.clone();
                let source = access.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    maintenance::run(&app_data_dir, &source, request)
                })
                .await
                .map_err(|e| format!("Database maintenance task failed: {e}"))?
                .map_err(|e: Error| e.to_string())
            }
            Err(e) => Err(e),
        };

        let next_access = match &outcome {
            Ok(outcome) => outcome.access.clone(),
            Err(_) => access,
        };
        if let Err(rebuild_error) = self.install(handle, next_access).await {
            error!("Failed to rebuild the database runtime: {}", rebuild_error);
            if let Err(e) = outcome {
                error!("Maintenance had already failed: {}", e);
            }
            return Err(format!(
                "The database could not be reopened after maintenance: {rebuild_error}. \
                 Restart the application."
            ));
        }

        outcome
    }

    /// Releases every handle on the database, in the order that makes the
    /// ownership proof meaningful.
    async fn teardown(&self, handle: &AppHandle) -> std::result::Result<(), String> {
        let Some(live) = self.live.lock().unwrap().take() else {
            // Nothing was installed, so no worker can be starting a server right
            // now. Still stop one, in case an earlier teardown left it running.
            crate::mcp::stop_server(handle).await;
            return Ok(());
        };
        let Live {
            access: _,
            context,
            writer,
            writer_task,
            workers,
        } = live;

        // Stop the device-sync engine gracefully before its task is aborted, so
        // an in-flight cycle is not cut mid-request.
        #[cfg(feature = "device-sync")]
        context
            .device_sync_runtime()
            .ensure_background_stopped()
            .await;

        // Aborting drops each task's future, which is what releases the context
        // and service clones it captured. Awaiting each handle is what makes the
        // next step correct: once this loop ends, no task is still starting.
        for worker in workers {
            worker.abort();
            let _ = worker.await;
        }

        // The MCP server holds service clones — and therefore pool clones — that
        // do not travel through the context, so it must be stopped explicitly.
        //
        // *After* the workers, never before: one of them is the task that starts
        // this server. Stopping first leaves that task free to finish starting
        // one afterwards, and the server it records is not reachable from
        // `context`, so the ownership proof below would not see it either.
        crate::mcp::stop_server(handle).await;

        // The write actor takes a pooled connection at spawn and holds it for the
        // life of its task, and its pool handle is an independent clone that the
        // context does not own — so dropping the context would not release it.
        // Stopping it *before* the ownership check is not optional: doing it
        // after would mean the check can never pass.
        writer.shutdown().await;
        writer_task.join().await;

        // A command that entered before the gate was set still holds a clone and
        // may be awaiting a slow query. Give it a bounded moment to finish
        // rather than failing an operation the user just confirmed.
        let deadline = Instant::now() + OWNERSHIP_WAIT;
        while Arc::strong_count(&context) > 1 && Instant::now() < deadline {
            tokio::time::sleep(OWNERSHIP_POLL).await;
        }

        // Proof of sole ownership. `None` means a worker or an in-flight command
        // still holds a clone; abort having touched nothing rather than replace
        // the file underneath it.
        drop(Arc::into_inner(context).ok_or_else(|| {
            "Database maintenance aborted: another part of the app is still using the \
             database. The database file was not replaced, but an operation that was \
             running may need to be retried. Wait for it to finish and try again."
                .to_string()
        })?);

        Ok(())
    }

    /// Reinstates a backup.
    ///
    /// Supplies the device's retained key so that a backup taken while
    /// encryption was on still opens after it has been turned off — the reason
    /// the key is never deleted.
    pub async fn restore(
        &self,
        handle: &AppHandle,
        backup_path: std::path::PathBuf,
    ) -> std::result::Result<MaintenanceOutcome, String> {
        let device_key = self
            .key_provider
            .existing()
            .map_err(|e: Error| format!("Failed to read the database key: {e}"))?
            .map(Arc::new);

        self.run_maintenance(
            handle,
            MaintenanceRequest::Restore {
                backup_path,
                device_key,
            },
        )
        .await
    }

    /// Enables at-rest encryption, minting the device key if it does not exist.
    ///
    /// The key is stored *before* the candidate is built. If any later step
    /// fails the key is kept: the database is still plaintext, detection falls
    /// through to the unkeyed open, and the operation is safe to retry.
    pub async fn enable_encryption(
        &self,
        handle: &AppHandle,
    ) -> std::result::Result<MaintenanceOutcome, String> {
        let access = self
            .current_access()
            .ok_or_else(|| DatabaseUnavailable::NotInitialized.to_string())?;
        if access.is_encrypted() {
            return Err("The database is already encrypted.".to_string());
        }

        let key = self
            .key_provider
            .create()
            .map_err(|e: Error| format!("Failed to prepare the database key: {e}"))?;

        self.run_maintenance(handle, MaintenanceRequest::Enable { key: Arc::new(key) })
            .await
    }

    /// Disables at-rest encryption. The key stays in the keychain permanently,
    /// so encrypted backups taken before this point remain openable and a later
    /// re-enable reuses the same key.
    pub async fn disable_encryption(
        &self,
        handle: &AppHandle,
    ) -> std::result::Result<MaintenanceOutcome, String> {
        let access = self
            .current_access()
            .ok_or_else(|| DatabaseUnavailable::NotInitialized.to_string())?;
        if !access.is_encrypted() {
            return Err("The database is not encrypted.".to_string());
        }

        self.run_maintenance(handle, MaintenanceRequest::Disable)
            .await
    }
}

/// Clears the maintenance gate when it goes out of scope.
struct MaintenanceGate<'a>(&'a AtomicBool);

impl Drop for MaintenanceGate<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Background workers
// ─────────────────────────────────────────────────────────────────────────────

/// Spawns every long-lived task that holds a context or service clone, and
/// returns their handles so maintenance can stop them.
///
/// Anything spawned here without its handle being returned would keep a pool
/// clone alive and make the zero-connection proof fail.
fn start_workers(
    handle: &AppHandle,
    context: &Arc<ServiceContext>,
    event_receiver: tokio::sync::mpsc::UnboundedReceiver<DomainEvent>,
    #[allow(unused_variables)] sync_outbox_wake_receiver: tokio::sync::mpsc::Receiver<()>,
) -> Vec<JoinHandle<()>> {
    let mut workers = Vec::new();

    #[cfg(feature = "device-sync")]
    workers.push(crate::start_sync_outbox_wake_worker(
        sync_outbox_wake_receiver,
        Arc::clone(context),
    ));

    workers.push(
        crate::domain_events::TauriDomainEventSink::start_queue_worker(
            event_receiver,
            handle.clone(),
            Arc::clone(context),
        ),
    );

    {
        let startup_handle = handle.clone();
        let startup_context = Arc::clone(context);
        workers.push(tauri::async_runtime::spawn(async move {
            crate::scheduler::run_startup_sync(&startup_handle, &startup_context).await;
        }));
    }

    #[cfg(desktop)]
    {
        let mcp_handle = handle.clone();
        let mcp_context = Arc::clone(context);
        workers.push(tauri::async_runtime::spawn(async move {
            crate::mcp::start_if_enabled(&mcp_handle, &mcp_context).await;
        }));

        // Periodic market data sync (6h interval, 2min initial delay).
        let periodic_quote_service = Arc::clone(&context.quote_service);
        workers.push(tauri::async_runtime::spawn(async move {
            wealthfolio_core::quotes::scheduler::run_periodic_sync(
                periodic_quote_service,
                std::time::Duration::from_secs(120),
                std::time::Duration::from_secs(6 * 3600),
            )
            .await;
        }));
    }

    // Background device sync engine (self-skips when the device is not READY).
    #[cfg(feature = "device-sync")]
    {
        let device_sync_context = Arc::clone(context);
        workers.push(tauri::async_runtime::spawn(async move {
            if let Err(err) =
                crate::commands::device_sync::ensure_background_engine_started(device_sync_context)
                    .await
            {
                warn!("Failed to start background device sync engine: {}", err);
            }
        }));
    }

    workers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn runtime() -> DatabaseRuntime {
        DatabaseRuntime::new("/tmp/wealthfolio-test".to_string())
    }

    /// A key store that accepts writes and keeps them, like a working keychain.
    #[derive(Default)]
    struct FakeSecretStore {
        stored: StdMutex<Option<String>>,
        /// Drop every write instead of keeping it, like a keyring backend
        /// writing to a collection that does not survive the session.
        forgetful: bool,
    }

    impl SecretStore for FakeSecretStore {
        fn set_secret(&self, _service: &str, secret: &str) -> CoreResult<()> {
            if !self.forgetful {
                *self.stored.lock().unwrap() = Some(secret.to_string());
            }
            Ok(())
        }

        fn get_secret(&self, _service: &str) -> CoreResult<Option<String>> {
            Ok(self.stored.lock().unwrap().clone())
        }

        fn delete_secret(&self, _service: &str) -> CoreResult<()> {
            *self.stored.lock().unwrap() = None;
            Ok(())
        }
    }

    #[test]
    fn a_key_store_that_forgets_the_key_is_caught_before_anything_is_encrypted() {
        // The failure this prevents is unrecoverable: the database would be
        // encrypted with a key that is gone after the restart, and enabling
        // deletes the plaintext pre-operation backup on its way out.
        let provider = KeychainKeyProvider::new(Arc::new(FakeSecretStore {
            forgetful: true,
            ..Default::default()
        }));

        let error = provider.create().expect_err("a lost key must not be used");

        assert!(
            error.to_string().contains("did not keep it"),
            "the user must be told the key store is the problem: {error}"
        );
    }

    #[test]
    fn a_working_key_store_mints_a_key_once_and_reuses_it() {
        let provider = KeychainKeyProvider::new(Arc::new(FakeSecretStore::default()));

        let first = provider.create().expect("mint");
        let second = provider.create().expect("reuse");

        assert_eq!(first.as_hex(), second.as_hex());
        assert_eq!(
            provider
                .existing()
                .unwrap()
                .map(|key| key.as_hex().to_string()),
            Some(first.as_hex().to_string())
        );
    }

    #[test]
    fn commands_are_rejected_before_the_database_is_open() {
        assert_eq!(
            runtime().context().err(),
            Some(DatabaseUnavailable::NotInitialized)
        );
    }

    #[test]
    fn a_runtime_that_is_down_describes_no_database() {
        // The location used to live in a second `Option` under its own lock, so
        // a failed rebuild left it answering with the outgoing database while no
        // runtime was serving it. Holding both in `Live` makes that
        // unrepresentable: they are set and cleared in one move.
        let runtime = runtime();

        assert!(runtime.current_access().is_none());
        assert!(!runtime.is_encrypted());
        assert_eq!(
            runtime.access().err(),
            Some(DatabaseUnavailable::NotInitialized)
        );
        assert_eq!(
            runtime.context().err(),
            Some(DatabaseUnavailable::NotInitialized)
        );
    }

    #[test]
    fn maintenance_mode_rejects_new_database_work() {
        let runtime = runtime();
        runtime.maintenance.store(true, Ordering::SeqCst);

        assert_eq!(
            runtime.context().err(),
            Some(DatabaseUnavailable::Maintenance)
        );
        assert!(runtime.try_context().is_none());
    }

    #[test]
    fn maintenance_mode_also_rejects_direct_file_access() {
        // Backups open the database file themselves, so they must be gated too:
        // one started mid-replacement would read the outgoing inode.
        let runtime = runtime();
        runtime.maintenance.store(true, Ordering::SeqCst);

        assert_eq!(
            runtime.access().err(),
            Some(DatabaseUnavailable::Maintenance)
        );
    }

    #[test]
    fn the_gate_clears_itself_even_when_the_scope_unwinds() {
        let runtime = runtime();
        runtime.maintenance.store(true, Ordering::SeqCst);

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _gate = MaintenanceGate(&runtime.maintenance);
            panic!("maintenance blew up");
        }));

        assert!(unwound.is_err());
        assert!(!runtime.maintenance.load(Ordering::SeqCst));
    }
}
