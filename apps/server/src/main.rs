mod ai_environment;
mod api;
mod auth;
mod config;
mod domain_events;
mod error;
mod events;
mod features;
mod main_lib;
mod mcp;
mod models;
mod oidc;
mod scheduler;
mod secrets;

use api::{app_router, security_headers};
use axum::middleware;
use config::Config;
use main_lib::{build_state, init_tracing};
use tower_http::services::{ServeDir, ServeFile};
#[cfg(feature = "device-sync")]
use tracing::{info, warn};
#[cfg(feature = "device-sync")]
use wealthfolio_device_sync::SyncState;

#[cfg(feature = "device-sync")]
fn is_expected_startup_token_warmup_error(err: &crate::error::ApiError) -> bool {
    match err {
        crate::error::ApiError::Unauthorized(_) | crate::error::ApiError::Forbidden(_) => true,
        crate::error::ApiError::Internal(message) => {
            message.contains("No refresh token configured")
                || message.contains("Auth refresh configuration is missing")
                || message.contains("CONNECT_AUTH_URL or CONNECT_AUTH_PUBLISHABLE_KEY")
        }
        _ => false,
    }
}

#[cfg(test)]
mod cli_tests {
    use super::run_maintenance_cli;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn non_db_arguments_fall_through_to_normal_startup() {
        assert!(run_maintenance_cli(&args(&[])).is_none());
        assert!(run_maintenance_cli(&args(&["--version"])).is_none());
    }

    #[test]
    fn db_without_a_subcommand_is_an_error_not_a_server_start() {
        let result = run_maintenance_cli(&args(&["db"])).expect("must not fall through");
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Missing database command"));
    }

    #[test]
    fn db_with_an_unknown_subcommand_is_an_error() {
        let result = run_maintenance_cli(&args(&["db", "rotate"])).expect("must not fall through");
        assert!(result.unwrap_err().to_string().contains("db rotate"));
    }
}

#[cfg(all(test, feature = "device-sync"))]
mod tests {
    use super::*;
    use crate::error::ApiError;

    #[test]
    fn startup_token_warmup_treats_unauthorized_as_expected() {
        let err = ApiError::Forbidden("No refresh token configured".to_string());
        assert!(is_expected_startup_token_warmup_error(&err));
    }

    #[test]
    fn startup_token_warmup_treats_missing_config_as_expected() {
        let err = ApiError::Internal(
            "CONNECT_AUTH_URL or CONNECT_AUTH_PUBLISHABLE_KEY is not configured".to_string(),
        );
        assert!(is_expected_startup_token_warmup_error(&err));
    }

    #[test]
    fn startup_token_warmup_treats_unexpected_internal_as_warning_candidate() {
        let err = ApiError::Internal("Upstream refresh timeout".to_string());
        assert!(!is_expected_startup_token_warmup_error(&err));
    }
}

/// Offline database maintenance, run with the server stopped.
///
/// Converting the database replaces the file, which requires that nothing is
/// connected to it — so it is a command, not an API call.
fn run_maintenance_cli(args: &[String]) -> Option<anyhow::Result<()>> {
    if args.first().map(String::as_str) != Some("db") {
        return None;
    }

    // Once `db` is given, a missing or unknown subcommand is an error. Falling
    // through would silently start the server instead of converting anything.
    let encrypt = match args.get(1).map(String::as_str) {
        Some("encrypt") => true,
        Some("decrypt") => false,
        Some(other) => {
            return Some(Err(anyhow::anyhow!(
                "Unknown database command 'db {other}'. Expected 'db encrypt' or 'db decrypt'."
            )))
        }
        None => {
            return Some(Err(anyhow::anyhow!(
                "Missing database command. Expected 'db encrypt' or 'db decrypt'."
            )))
        }
    };

    init_tracing();
    Some(main_lib::run_database_maintenance(encrypt))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(result) = run_maintenance_cli(&args) {
        return result;
    }

    let config = Config::from_env();
    init_tracing();
    let state = build_state(&config).await?;

    #[cfg(feature = "device-sync")]
    #[allow(clippy::collapsible_if)]
    if features::device_sync_enabled() {
        let startup_state = state.clone();
        tokio::spawn(async move {
            match api::connect::mint_access_token(&startup_state).await {
                Ok(token) => {
                    if startup_state
                        .device_enroll_service
                        .get_sync_state(&token)
                        .await
                        .map(|sync_state| sync_state.state == SyncState::Ready)
                        .unwrap_or(false)
                    {
                        if let Err(err) = api::device_sync_engine::ensure_background_engine_started(
                            startup_state.clone(),
                        )
                        .await
                        {
                            warn!(
                                "Failed to auto-start device sync background engine: {}",
                                err
                            );
                        }
                    }
                }
                Err(err) => {
                    if is_expected_startup_token_warmup_error(&err) {
                        info!(
                            "Skipping startup device sync token warmup (expected state): {}",
                            err
                        );
                    } else {
                        warn!("Device sync token warmup failed during startup: {}", err);
                    }
                }
            }
        });
    }

    // Start background broker sync scheduler (4-hour interval)
    scheduler::start_broker_sync_scheduler(state.clone());

    // Start periodic market data sync (6h interval, 2min initial delay)
    let quote_svc = state.quote_service.clone();
    tokio::spawn(async move {
        wealthfolio_core::quotes::scheduler::run_periodic_sync(
            quote_svc,
            std::time::Duration::from_secs(120),
            std::time::Duration::from_secs(6 * 3600),
        )
        .await;
    });

    let static_dir = std::path::PathBuf::from(&config.static_dir);
    let index_file = static_dir.join("index.html");
    let static_service = ServeDir::new(static_dir).fallback(ServeFile::new(index_file));
    let router = app_router(state, &config)
        .fallback_service(static_service)
        .layer(middleware::from_fn(security_headers));
    if let Some(ref auth) = config.auth {
        tracing::info!(
            "Authentication enabled, cookie secure policy: {}",
            auth.cookie_secure
        );
    } else {
        tracing::info!("Authentication disabled");
    }
    tracing::info!("Listening on {}", config.listen_addr);
    let listener = tokio::net::TcpListener::bind(config.listen_addr).await?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
