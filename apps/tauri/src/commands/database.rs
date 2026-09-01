//! Database encryption commands.
//!
//! Enabling and disabling both replace the whole database file, so both run
//! through the maintenance coordinator: tear the runtime down, build and verify
//! a candidate, install it atomically, then continue (desktop restarts; iOS
//! carries on over the rebuilt runtime).

use serde::Serialize;
use tauri::{AppHandle, State};

use crate::commands::utilities::finish_database_maintenance;
use crate::database::DatabaseRuntime;

/// Whether encryption is available on this platform at all.
///
/// Android has no `keyring` backend — it compiles to a non-persistent in-memory
/// mock — so there is nowhere to keep the key.
const ENCRYPTION_SUPPORTED: bool = !cfg!(target_os = "android");

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseEncryptionStatus {
    /// Whether the database file is encrypted *right now*. This is the observed
    /// state of the file, not the `app_settings` flag, which records intent.
    pub enabled: bool,
    pub supported: bool,
}

#[tauri::command]
pub async fn get_database_encryption_status(
    runtime: State<'_, DatabaseRuntime>,
) -> Result<DatabaseEncryptionStatus, String> {
    Ok(DatabaseEncryptionStatus {
        enabled: runtime.is_encrypted(),
        supported: ENCRYPTION_SUPPORTED,
    })
}

/// Turns at-rest encryption on or off.
///
/// On desktop this does not return: the app restarts as soon as the new database
/// is verified, because continuing on services built over the replaced file is
/// never correct.
#[tauri::command]
pub async fn set_database_encryption_enabled(
    app_handle: AppHandle,
    runtime: State<'_, DatabaseRuntime>,
    enabled: bool,
) -> Result<(), String> {
    if !ENCRYPTION_SUPPORTED {
        return Err("Database encryption is not available on this platform.".to_string());
    }

    if enabled {
        runtime.enable_encryption(&app_handle).await?;
    } else {
        runtime.disable_encryption(&app_handle).await?;
    }

    finish_database_maintenance(&app_handle, "database-encryption-changed")
}
