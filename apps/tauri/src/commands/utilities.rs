use crate::database::DatabaseRuntime;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tauri::async_runtime::spawn_blocking;
use tauri::Manager;
use tauri::{AppHandle, Emitter, State};
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_shell::ShellExt;
use uuid::Uuid;
use wealthfolio_core::{
    activities::Sort,
    exports::{
        export_file_name, format_holding_list_records, format_records, ExportDataType,
        ExportFileFormat,
    },
    portfolio::holdings::HoldingListItem,
    portfolios::AccountScope,
};
use wealthfolio_storage_sqlite::db;

use crate::commands::portfolio::holdings_account_ids;
use crate::context::ServiceContext;
#[cfg(desktop)]
use crate::updater::{check_for_update, install_update};

const PENDING_EXPORTS_DIR: &str = "pending-exports";
const PENDING_EXPORT_TTL: Duration = Duration::from_secs(60 * 60);
const EXPORT_ACTIVITY_PAGE_SIZE: i64 = 9_007_199_254_740_991;

/// Normalize file path by removing file:// URI prefix if present (iOS/Android compatibility)
fn normalize_file_path(path: &str) -> String {
    if path.starts_with("file://") {
        path.strip_prefix("file://").unwrap_or(path).to_string()
    } else {
        path.to_string()
    }
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn file_extension(file_name: &str) -> Option<&str> {
    Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())
}

fn pending_export_filename(file_name: &str) -> Result<String, String> {
    let name = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Export filename is invalid".to_string())?;

    if name.is_empty() || name.contains('/') || name.contains('\\') {
        return Err("Export filename is invalid".to_string());
    }

    Ok(name.to_string())
}

fn prepare_pending_export_path(
    app_data_dir_path: &Path,
    filename: &str,
) -> Result<(String, PathBuf), String> {
    cleanup_stale_pending_exports(app_data_dir_path);

    let export_id = Uuid::new_v4().to_string();
    let export_dir = app_data_dir_path.join(PENDING_EXPORTS_DIR).join(&export_id);
    fs::create_dir_all(&export_dir)
        .map_err(|e| format!("Failed to create pending export directory: {}", e))?;

    Ok((
        format!("{}/{}/{}", PENDING_EXPORTS_DIR, export_id, filename),
        export_dir.join(filename),
    ))
}

fn cleanup_stale_pending_exports(app_data_dir_path: &Path) {
    let pending_exports_dir = app_data_dir_path.join(PENDING_EXPORTS_DIR);
    let Ok(entries) = fs::read_dir(&pending_exports_dir) else {
        return;
    };

    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified) else {
            continue;
        };

        if age <= PENDING_EXPORT_TTL {
            continue;
        }

        let result = if metadata.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };

        if let Err(error) = result {
            log::warn!(
                "Failed to clean stale pending export {}: {}",
                path.display(),
                error
            );
        }
    }
}

fn is_allowed_external_url(url: &str) -> bool {
    if url.is_empty() || url.chars().any(char::is_whitespace) || url.chars().any(char::is_control) {
        return false;
    }

    let lowercase_url = url.to_ascii_lowercase();
    ["https://", "http://", "mailto:", "tel:"]
        .iter()
        .any(|scheme| lowercase_url.starts_with(scheme) && lowercase_url.len() > scheme.len())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    version: String,
    db_path: String,
    logs_dir: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingExport {
    relative_path: String,
    filename: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataExportResult {
    status: DataExportStatus,
    relative_path: Option<String>,
    filename: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DataExportStatus {
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    Saved,
    #[cfg(any(target_os = "ios", target_os = "android"))]
    Pending,
    Empty,
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    Canceled,
}

impl DataExportResult {
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    fn saved(filename: String) -> Self {
        Self {
            status: DataExportStatus::Saved,
            relative_path: None,
            filename: Some(filename),
        }
    }

    #[cfg(any(target_os = "ios", target_os = "android"))]
    fn pending(pending_export: PendingExport) -> Self {
        Self {
            status: DataExportStatus::Pending,
            relative_path: Some(pending_export.relative_path),
            filename: Some(pending_export.filename),
        }
    }

    fn empty() -> Self {
        Self {
            status: DataExportStatus::Empty,
            relative_path: None,
            filename: None,
        }
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    fn canceled() -> Self {
        Self {
            status: DataExportStatus::Canceled,
            relative_path: None,
            filename: None,
        }
    }
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
fn save_content_with_dialog(
    app_handle: &AppHandle,
    file_name: &str,
    content: &[u8],
) -> Result<bool, String> {
    let mut dialog = app_handle.dialog().file().set_file_name(file_name);

    if let Some(extension) = file_extension(file_name) {
        let filter_name = extension.to_ascii_uppercase();
        dialog = dialog.add_filter(filter_name, &[extension]);
    }

    let Some(file_path) = dialog.blocking_save_file() else {
        return Ok(false);
    };

    let path = file_path
        .into_path()
        .map_err(|e| format!("Failed to resolve selected file path: {}", e))?;

    fs::write(&path, content)
        .map_err(|e| format!("Failed to save export to {}: {}", path.display(), e))?;

    Ok(true)
}

fn write_pending_export_content(
    app_handle: &AppHandle,
    file_name: &str,
    content: &[u8],
) -> Result<PendingExport, String> {
    let filename = pending_export_filename(file_name)?;
    let app_data_dir_path = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    let (relative_path, export_path) = prepare_pending_export_path(&app_data_dir_path, &filename)?;
    fs::write(&export_path, content).map_err(|e| {
        format!(
            "Failed to write pending export {}: {}",
            export_path.display(),
            e
        )
    })?;

    Ok(PendingExport {
        relative_path,
        filename,
    })
}

async fn build_data_export_content(
    state: &ServiceContext,
    data_type: ExportDataType,
    format: ExportFileFormat,
) -> Result<Option<Vec<u8>>, String> {
    match data_type {
        ExportDataType::Accounts => {
            let records = state
                .account_service()
                .get_non_archived_accounts()
                .map_err(|e| format!("Failed to load accounts for export: {}", e))?;
            format_records(&records, format).map_err(|e| e.to_string())
        }
        ExportDataType::Activities => {
            let records = state
                .activity_service()
                .search_activities(
                    0,
                    EXPORT_ACTIVITY_PAGE_SIZE,
                    None,
                    None,
                    None,
                    Some(Sort {
                        id: "date".to_string(),
                        desc: true,
                    }),
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .map_err(|e| format!("Failed to load activities for export: {}", e))?
                .data;
            format_records(&records, format).map_err(|e| e.to_string())
        }
        ExportDataType::Holdings => {
            let base_currency = state.get_base_currency();
            let resolved = state
                .portfolio_service()
                .resolve_account_scope(&AccountScope::All, &base_currency)
                .map_err(|e| format!("Failed to resolve portfolio scope: {}", e))?;
            let account_ids = holdings_account_ids(state, &resolved.account_ids)?;
            if account_ids.is_empty() {
                return Ok(None);
            }

            let holdings = if account_ids.len() == 1 {
                state
                    .holdings_service()
                    .get_holdings(&account_ids[0], &base_currency)
                    .await
                    .map_err(|e| format!("Failed to load holdings for export: {}", e))?
            } else {
                state
                    .holdings_service()
                    .get_holdings_for_accounts(&account_ids, &base_currency, &resolved.scope_id)
                    .await
                    .map_err(|e| format!("Failed to load holdings for export: {}", e))?
            };
            let records = holdings
                .into_iter()
                .map(HoldingListItem::from)
                .collect::<Vec<_>>();
            format_holding_list_records(&records, format).map_err(|e| e.to_string())
        }
        ExportDataType::Goals => {
            let records = state
                .goal_service()
                .get_goals()
                .map_err(|e| format!("Failed to load goals for export: {}", e))?;
            format_records(&records, format).map_err(|e| e.to_string())
        }
        ExportDataType::PortfolioHistory => {
            let base_currency = state.get_base_currency();
            let resolved = state
                .portfolio_service()
                .resolve_account_scope(&AccountScope::All, &base_currency)
                .map_err(|e| format!("Failed to resolve portfolio scope: {}", e))?;
            let records = state
                .valuation_service()
                .get_historical_valuations_for_accounts(
                    &resolved.scope_id,
                    &resolved.account_ids,
                    &resolved.base_currency,
                    None,
                    None,
                )
                .map_err(|e| format!("Failed to load portfolio history for export: {}", e))?;
            format_records(&records, format).map_err(|e| e.to_string())
        }
    }
}

#[tauri::command]
pub async fn save_text_file_with_dialog(
    app_handle: AppHandle,
    file_name: String,
    content: String,
) -> Result<bool, String> {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let _ = (&app_handle, &file_name, &content);
        return Err("Direct file saving is only supported on desktop".to_string());
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    save_content_with_dialog(&app_handle, &file_name, content.as_bytes())
}

#[tauri::command]
pub async fn save_file_with_dialog(
    app_handle: AppHandle,
    file_name: String,
    content_base64: String,
) -> Result<bool, String> {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let _ = (&app_handle, &file_name, &content_base64);
        return Err("Direct file saving is only supported on desktop".to_string());
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        let content = BASE64_STANDARD
            .decode(content_base64)
            .map_err(|e| format!("Failed to decode export content: {}", e))?;

        save_content_with_dialog(&app_handle, &file_name, &content)
    }
}

#[tauri::command]
pub async fn write_pending_export_text_file(
    app_handle: AppHandle,
    file_name: String,
    content: String,
) -> Result<PendingExport, String> {
    write_pending_export_content(&app_handle, &file_name, content.as_bytes())
}

#[tauri::command]
pub async fn write_pending_export_file(
    app_handle: AppHandle,
    file_name: String,
    content_base64: String,
) -> Result<PendingExport, String> {
    let content = BASE64_STANDARD
        .decode(content_base64)
        .map_err(|e| format!("Failed to decode export content: {}", e))?;

    write_pending_export_content(&app_handle, &file_name, &content)
}

#[tauri::command]
pub async fn export_data_file(
    app_handle: AppHandle,
    state: State<'_, DatabaseRuntime>,
    data_type: String,
    format: String,
) -> Result<DataExportResult, String> {
    let state = state.context()?;
    let data_type = ExportDataType::parse(&data_type).map_err(|e| e.to_string())?;
    let format = ExportFileFormat::parse(&format).map_err(|e| e.to_string())?;
    let Some(content) = build_data_export_content(state.as_ref(), data_type, format).await? else {
        return Ok(DataExportResult::empty());
    };

    let filename = export_file_name(data_type, format, chrono::Local::now().date_naive());

    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let pending_export = write_pending_export_content(&app_handle, &filename, &content)?;
        Ok(DataExportResult::pending(pending_export))
    }

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        if save_content_with_dialog(&app_handle, &filename, &content)? {
            Ok(DataExportResult::saved(filename))
        } else {
            Ok(DataExportResult::canceled())
        }
    }
}

#[tauri::command]
pub async fn open_external_url(app_handle: AppHandle, url: String) -> Result<(), String> {
    let url = url.trim();
    if !is_allowed_external_url(url) {
        return Err("Unsupported external URL".to_string());
    }

    open_external_link(&app_handle, url)
}

#[allow(deprecated)]
fn open_external_link(app_handle: &AppHandle, url: &str) -> Result<(), String> {
    app_handle
        .shell()
        .open(url, None)
        .map_err(|e| format!("Failed to open external link: {}", e))
}

#[tauri::command]
pub async fn get_app_info(app_handle: AppHandle) -> Result<AppInfo, String> {
    let version = app_handle.package_info().version.to_string();

    let app_data_dir_path = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?
        .to_path_buf();

    let app_data_dir = app_data_dir_path
        .to_str()
        .ok_or_else(|| "Failed to convert app data dir path to string".to_string())?
        .to_string();

    let db_path = db::get_db_path(&app_data_dir);
    let logs_dir = app_handle
        .path()
        .app_log_dir()
        .map_err(|e| format!("Failed to get app log dir: {}", e))?
        .to_str()
        .ok_or_else(|| "Failed to convert app log dir path to string".to_string())?
        .to_string();

    Ok(AppInfo {
        version,
        db_path,
        logs_dir,
    })
}

/// Check for updates and return update info if available.
#[tauri::command]
pub async fn check_for_updates(app_handle: AppHandle) -> Result<Option<serde_json::Value>, String> {
    #[cfg(desktop)]
    {
        let result = check_for_update(app_handle).await?;
        Ok(result.map(|info| serde_json::to_value(info).unwrap()))
    }
    #[cfg(not(desktop))]
    {
        Ok(None)
    }
}

/// Download and install an available update. Emits progress events and restarts the app.
#[tauri::command]
pub async fn install_app_update(app_handle: AppHandle) -> Result<(), String> {
    #[cfg(desktop)]
    install_update(app_handle).await?;
    Ok(())
}

/// Internal backup, listed in the app's backup manager and used by restore.
///
/// A faithful copy: it inherits the live database's encryption, so on an
/// encrypted device only this device's retained key opens it.
#[tauri::command]
pub async fn backup_database(runtime: State<'_, DatabaseRuntime>) -> Result<String, String> {
    let access = runtime.access()?;
    let app_data_dir = runtime.app_data_dir().to_string();

    // Copying the whole database is blocking work, and on an encrypted database
    // every page is decrypted and re-encrypted on the way out.
    let backup_path = spawn_blocking(move || db::backup_database(&access, &app_data_dir))
        .await
        .map_err(|e| format!("Backup task failed: {e}"))?
        .map_err(|e| e.to_string())?;

    Ok(Path::new(&backup_path)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Failed to get backup filename".to_string())?
        .to_string())
}

/// User-facing export for the mobile share sheet.
///
/// Explicitly decrypted and portable, so the file restores on any machine. The
/// UI must state that the exported file is unencrypted.
#[tauri::command]
pub async fn backup_database_to_pending_export(
    app_handle: AppHandle,
    runtime: State<'_, DatabaseRuntime>,
) -> Result<PendingExport, String> {
    let access = runtime.access()?;

    let app_data_dir_path = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;

    let filename = format!(
        "wealthfolio_backup_{}.db",
        chrono::Local::now().format("%Y%m%d_%H%M%S_%3f")
    );
    let (relative_path, backup_path) = prepare_pending_export_path(&app_data_dir_path, &filename)?;
    let backup_path_str = backup_path
        .to_str()
        .ok_or_else(|| "Failed to convert backup export path to string".to_string())?
        .to_string();

    spawn_blocking(move || db::export_portable_backup(&access, &backup_path_str))
        .await
        .map_err(|e| format!("Backup export task failed: {e}"))?
        .map_err(|e| format!("Failed to create backup export: {}", e))?;

    Ok(PendingExport {
        relative_path,
        filename,
    })
}

/// User-facing export to a directory the user chose. Decrypted and portable,
/// like the pending export above.
#[tauri::command]
pub async fn backup_database_to_path(
    runtime: State<'_, DatabaseRuntime>,
    backup_dir: String,
) -> Result<String, String> {
    let access = runtime.access()?;

    // Normalize the backup directory path (remove file:// prefix if present on iOS/Android)
    let normalized_backup_dir = normalize_file_path(&backup_dir);

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_filename = format!("wealthfolio_backup_{}.db", timestamp);
    let backup_path = Path::new(&normalized_backup_dir).join(&backup_filename);
    let backup_path_str = backup_path.to_string_lossy().to_string();

    let target = backup_path_str.clone();
    spawn_blocking(move || db::export_portable_backup(&access, &target))
        .await
        .map_err(|e| format!("Backup task failed: {e}"))?
        .map_err(|e| format!("Failed to backup database: {}", e))?;

    Ok(backup_path_str)
}

/// Replaces the live database with a backup, through the maintenance
/// coordinator.
///
/// The restored database lands on *this device's* encryption state, whatever
/// the backup's own was, and the user's backup file is never modified or
/// consumed.
#[tauri::command]
pub async fn restore_database(
    app_handle: AppHandle,
    runtime: State<'_, DatabaseRuntime>,
    backup_file_path: String,
) -> Result<(), String> {
    // Normalize the backup file path (remove file:// prefix if present on iOS/Android)
    let backup_path = PathBuf::from(normalize_file_path(&backup_file_path));

    runtime.restore(&app_handle, backup_path).await?;

    finish_database_maintenance(&app_handle, "database-restored")
}

/// Announces a completed maintenance operation and continues safely.
///
/// Desktop restarts unconditionally: continuing on services built over the
/// replaced file is never correct, and a restart the user can decline is not a
/// guarantee. iOS has no restart API, so it relies on the runtime having been
/// rebuilt over the new file, and tells the frontend to refetch everything.
pub(crate) fn finish_database_maintenance(
    app_handle: &AppHandle,
    event: &str,
) -> Result<(), String> {
    app_handle
        .emit(event, ())
        .map_err(|e| format!("Failed to emit {} event: {}", event, e))?;

    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    app_handle.restart();

    #[cfg(any(target_os = "ios", target_os = "android"))]
    Ok(())
}
