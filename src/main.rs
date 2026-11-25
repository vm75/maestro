mod api;
#[path = "platforms/asus_ui.rs"]
mod asus_ui;
mod auth;
mod backup;
mod ban_attack;
mod certificate_http;
mod command;
mod config;
mod config_yaml;
pub mod core;
mod models;
mod modules;
mod nginx_ingress;
mod nginx_mutation;
mod nginx_render;
pub mod platforms;
mod restore;
mod router_layout;
mod state;
mod storage;
mod system;
#[path = "platforms/toolbox.rs"]
mod toolbox;
#[path = "platforms/upgrade.rs"]
mod upgrade;
mod util;

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

// Stable crate-root aliases while domain sources move beneath modules/.
use modules::{adguard, firewall, nginx, nginx_sites};

use crate::{
    asus_ui::{install_menu_entry, render_ui_file},
    config::{AppConfig, Platform},
    firewall::FirewallManager,
    state::AppState,
    storage::Stores,
};

#[derive(Debug, Parser)]
#[command(name = "maestro", version, about)]
struct Cli {
    #[arg(long, default_value = "/opt/etc/maestro/maestro.yaml")]
    config: PathBuf,

    #[arg(long, global = true)]
    test_mode: bool,

    /// Require a native Linux configuration (never reinterpret router YAML).
    #[arg(long, global = true, conflicts_with = "test_mode")]
    linux_mode: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Read one runtime configuration scalar for local diagnostic scripts.
    ConfigValue {
        key: String,
    },
    /// Create a private Linux YAML authority and bootstrap token as its owner.
    #[cfg(feature = "linux")]
    InitLinuxConfig,
    Serve,
    CheckConfig,
    /// Migrate persistent router YAML state to the current layout while stopped.
    MigrateRouterLayout,
    /// Install the bundled nginx configuration, backing up a different nginx.conf.
    InitNginx,
    /// Download or update the official dehydrated executable.
    #[command(alias = "init-dehydrated")]
    UpdateDehydrated,
    RenderUi,
    MountUi,
    UnmountUi,
    /// Back up the installed binary, configuration, state, nginx and certificates.
    Backup {
        /// Override /tmp on routers or the configuration directory on Linux.
        #[arg(long)]
        output_dir: Option<PathBuf>,
    },
    /// Restore a backup archive; stop services before restoring original locations.
    Restore {
        archive: PathBuf,
        /// Restore beneath this existing directory instead of /.
        #[arg(long, default_value = "/")]
        root: PathBuf,
        /// Map an external archived router root to a destination (SOURCE=DESTINATION).
        #[arg(long = "map")]
        mappings: Vec<String>,
    },
    /// Download the newest stable Entware IPK and optionally install it (router only).
    Upgrade {
        /// Verify and stage the IPK in /tmp without invoking opkg.
        #[arg(long)]
        download_only: bool,
        /// Skip creating the pre-upgrade backup.
        #[arg(long)]
        no_backup: bool,
    },
    /// List or install published Toolbox Entware release bundles (router only).
    Toolbox {
        #[command(subcommand)]
        command: toolbox::ToolboxCommand,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "maestro=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();
    // Recovery must remain available when the installed configuration is corrupt.
    if let Some(Command::Restore {
        archive,
        root,
        mappings,
    }) = &cli.command
    {
        if mappings.is_empty() {
            return restore::run(archive, root, cli.test_mode);
        }
        return restore::run_with_mappings(archive, root, cli.test_mode, mappings);
    }
    #[cfg(feature = "linux")]
    if matches!(cli.command, Some(Command::InitLinuxConfig)) {
        crate::core::linux_config::initialize(&cli.config)?;
        println!(
            "private Linux configuration ready: {}",
            cli.config.display()
        );
        return Ok(());
    }
    let mut config = if cli.test_mode && !cli.config.exists() {
        AppConfig::default()
    } else {
        AppConfig::load(&cli.config)?
    };
    anyhow::ensure!(
        !cli.linux_mode || config.platform == Platform::Linux,
        "--linux-mode requires a native Linux YAML authority"
    );
    if cli.test_mode {
        config.apply_test_mode(&cli.config)?;
    }
    config.validate()?;
    #[cfg(not(feature = "linux"))]
    if config.platform == Platform::Linux {
        anyhow::bail!(
            "Linux mode requires a Linux-feature build; the router profile excludes Linux management"
        );
    }
    #[cfg(feature = "linux")]
    if config.platform == Platform::Linux && !config.test_mode && unsafe { libc::geteuid() } == 0 {
        anyhow::bail!(
            "Linux Maestro must run as a non-root workload account; privileged host changes require optional maestro-admin"
        );
    }

    if config.platform == Platform::Asuswrt
        && !config.test_mode
        && matches!(
            &cli.command,
            None | Some(Command::Serve | Command::InitNginx | Command::CheckConfig)
        )
    {
        router_layout::require_current(&config)?;
    }

    match cli.command.unwrap_or(Command::Serve) {
        Command::MigrateRouterLayout => {
            anyhow::ensure!(
                config.platform == Platform::Asuswrt && !config.test_mode,
                "router layout migration requires a real router YAML configuration"
            );
            router_layout::migrate(&cli.config, &config)?;
            println!("router configuration layout is current");
            Ok(())
        }
        #[cfg(feature = "linux")]
        Command::InitLinuxConfig => unreachable!("initialization precedes configuration loading"),
        Command::Restore { .. } => unreachable!("restore is handled before configuration loading"),
        Command::Backup { output_dir } => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(backup::run(&config, &cli.config, output_dir.as_deref())),
        Command::Upgrade {
            download_only,
            no_backup,
        } => {
            upgrade::require_router(config.platform, config.test_mode)?;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(upgrade::run(&config, &cli.config, download_only, no_backup))
        }
        Command::Toolbox { command } => {
            toolbox::require_router(config.platform, config.test_mode)?;
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(toolbox::run(&config, &cli.config, command))
        }
        Command::ConfigValue { key } => {
            let mut value = serde_json::to_value(&config)?;
            for part in key.split('.') {
                value = value
                    .get(part)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("unknown configuration key"))?;
            }
            match value {
                serde_json::Value::String(value) => println!("{value}"),
                serde_json::Value::Bool(value) => println!("{value}"),
                serde_json::Value::Number(value) => println!("{value}"),
                _ => anyhow::bail!("configuration key is not a scalar"),
            }
            Ok(())
        }
        Command::CheckConfig => {
            validate_nginx_inputs(&config)?;
            println!("configuration is valid: {}", cli.config.display());
            Ok(())
        }
        Command::InitNginx => {
            let created = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(nginx_render::bootstrap(&config))?;
            println!(
                "{}",
                if created {
                    "installed Maestro nginx configuration"
                } else {
                    "Maestro nginx configuration is already current"
                }
            );
            Ok(())
        }
        Command::UpdateDehydrated => {
            let update = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(state::download_latest_dehydrated(&config))?;
            println!(
                "{}",
                if update.simulated {
                    format!(
                        "simulated download of dehydrated to {}",
                        update.path.display()
                    )
                } else {
                    format!(
                        "downloaded dehydrated ({} bytes) to {}",
                        update.bytes,
                        update.path.display()
                    )
                }
            );
            Ok(())
        }
        Command::RenderUi => {
            let path = render_ui_file(&config)?;
            println!("rendered {}", path.display());
            Ok(())
        }
        Command::MountUi => {
            if config.platform == Platform::Linux {
                anyhow::bail!("mount-ui requires Asuswrt; use the standalone UI on Linux");
            }
            if !config.test_mode && config.asus_ui.enabled {
                install_menu_entry(&config)?;
            }
            render_ui_file(&config)?;
            Ok(())
        }
        Command::UnmountUi => Ok(()),
        Command::Serve => {
            if config.server.daemonize && !config.test_mode {
                if let Some(parent) = config.paths.log_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let log_file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&config.paths.log_file)
                    .with_context(|| {
                        format!(
                            "failed to open log file {}",
                            config.paths.log_file.display()
                        )
                    })?;

                daemonize::Daemonize::new()
                    .stdout(log_file.try_clone()?)
                    .stderr(log_file)
                    .working_directory("/")
                    .start()
                    .context("failed to daemonize")?;
            }

            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(serve(config))
        }
    }
}

fn validate_nginx_inputs(config: &AppConfig) -> Result<()> {
    let global = nginx_render::load_global(config)?;
    let sites = nginx_sites::load(config)?;
    crate::nginx_ingress::validate_tree(&global, &sites)?;
    Ok(())
}

async fn serve(config: AppConfig) -> Result<()> {
    validate_nginx_inputs(&config)?;
    config.ensure_directories()?;
    let stores = Stores::load(&config).await?;
    let firewall = FirewallManager::new(config.clone(), stores.clone()).await?;
    let state = AppState::new(config.clone(), stores, firewall.clone());

    if config.asus_ui.enabled && !config.test_mode && config.platform == Platform::Asuswrt {
        if let Err(error) = install_menu_entry(&config) {
            warn!(%error, "unable to update ASUS menu tree; continuing with rendered UI");
        }
        if let Err(error) = render_ui_file(&config) {
            warn!(%error, "unable to render ASUS UI page; continuing with standalone listener");
        }
    } else if config.test_mode || config.platform == Platform::Asuswrt {
        // Preserve standalone ASUS rendering in simulated and asuswrt modes.
        render_ui_file(&config)?;
    }

    firewall.start().await;
    spawn_firewall_reconcile_signal(state.clone());
    spawn_reconfigure_signal();
    if let Err(error) = crate::certificate_http::recover_startup(&state).await {
        warn!(%error, "nginx ingress startup reconciliation failed; inspect certificate HTTP access status");
    }
    state.start_certificate_renewal_loop();
    crate::certificate_http::start_reconciliation(state.clone());

    if let Some(adguard_cfg) = crate::api::adguard::enabled_config(&state).await {
        let config_clone = state.config.clone();
        tokio::spawn(async move {
            if let Ok(client) = crate::adguard::AdGuardClient::new(&adguard_cfg) {
                if let Err(error) = client.deduplicate_rewrites().await {
                    warn!(%error, "unable to deduplicate AdGuard rewrites");
                }
                if let Ok(objects) = crate::nginx::list_objects(&config_clone, true) {
                    for obj in objects {
                        if obj.enabled {
                            let full_domain = match obj.kind {
                                crate::models::NginxObjectKind::Domain => obj.domain.clone(),
                                crate::models::NginxObjectKind::Subdomain => {
                                    format!("{}.{}", obj.name, obj.domain)
                                }
                                crate::models::NginxObjectKind::Subfolder => String::new(),
                            };
                            if !full_domain.is_empty() {
                                if let Err(error) = client
                                    .ensure_rewrite(&full_domain, &adguard_cfg.lan_ip)
                                    .await
                                {
                                    warn!(domain = %full_domain, %error, "unable to reconcile AdGuard rewrite");
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    let app = api::app(state.clone())?.layer(TraceLayer::new_for_http());

    let bind_ip: IpAddr = config
        .server
        .bind
        .parse()
        .context("invalid server bind address")?;
    let address = SocketAddr::new(bind_ip, config.server.port);
    let listener = TcpListener::bind(address).await?;
    info!(%address, version = env!("CARGO_PKG_VERSION"), test_mode = config.test_mode, "maestro listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal(state))
    .await?;

    Ok(())
}

#[cfg(unix)]
fn spawn_firewall_reconcile_signal(state: AppState) {
    tokio::spawn(async move {
        let mut signal =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
                Ok(signal) => signal,
                Err(error) => {
                    warn!(%error, "unable to install firewall reconciliation signal");
                    return;
                }
            };
        while signal.recv().await.is_some() {
            if let Err(error) = state.firewall.reconcile().await {
                warn!(%error, "signal-triggered firewall reconciliation failed");
            }
            if let Err(error) = crate::certificate_http::reconcile(&state).await {
                warn!(%error, "signal-triggered HTTP WAN reconciliation failed");
            }
        }
    });
}

#[cfg(not(unix))]
fn spawn_firewall_reconcile_signal(_state: AppState) {}

#[cfg(unix)]
fn spawn_reconfigure_signal() {
    tokio::spawn(async move {
        let mut signal = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
        {
            Ok(signal) => signal,
            Err(error) => {
                warn!(%error, "unable to install reconfigure signal");
                return;
            }
        };

        while signal.recv().await.is_some() {
            // rc.func's reconfigure action uses SIGHUP for a reload. Keep the
            // process alive so service refresh does not become an accidental
            // restart; runtime configuration remains owned by the process.
            info!("reconfigure signal received; keeping service running");
        }
    });
}

#[cfg(not(unix))]
fn spawn_reconfigure_signal() {}

async fn shutdown_signal(state: AppState) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    if let Err(error) = state.stop_certificates().await {
        warn!(%error, "certificate HTTP cleanup failed during shutdown");
    }
}

#[cfg(test)]
mod cli_tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn backup_and_upgrade_flags() {
        let cli = Cli::try_parse_from(["maestro", "upgrade"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Upgrade {
                no_backup: false,
                ..
            })
        ));
        let cli =
            Cli::try_parse_from(["maestro", "upgrade", "--no-backup", "--download-only"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Upgrade {
                no_backup: true,
                download_only: true
            })
        ));
        let cli = Cli::try_parse_from([
            "maestro",
            "--config",
            "/lab/maestro.yaml",
            "--linux-mode",
            "backup",
            "--output-dir",
            "/lab/backups",
        ])
        .unwrap();
        assert!(cli.linux_mode);
        assert!(
            matches!(cli.command, Some(Command::Backup { output_dir: Some(path) }) if path == Path::new("/lab/backups"))
        );
        let cli = Cli::try_parse_from([
            "maestro",
            "restore",
            "/tmp/backup.tar.gz",
            "--root",
            "/tmp/recovery",
        ])
        .unwrap();
        assert!(
            matches!(cli.command, Some(Command::Restore { archive, root, .. }) if archive == Path::new("/tmp/backup.tar.gz") && root == Path::new("/tmp/recovery"))
        );

        let cli = Cli::try_parse_from(["maestro", "toolbox", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Toolbox {
                command: toolbox::ToolboxCommand::List { all: false },
            })
        ));
        let cli = Cli::try_parse_from(["maestro", "toolbox", "list", "--all"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Toolbox {
                command: toolbox::ToolboxCommand::List { all: true },
            })
        ));
        for expected in [
            "btop",
            "vaultwarden-1.0.0-2",
            "toolbox-nginx-entware-1.30.8-99_aarch64",
        ] {
            let cli = Cli::try_parse_from(["maestro", "toolbox", "install", expected]).unwrap();
            assert!(matches!(
                cli.command,
                Some(Command::Toolbox {
                    command: toolbox::ToolboxCommand::Install { selector, .. },
                }) if selector == expected
            ));
        }
        let cli = Cli::try_parse_from([
            "maestro",
            "toolbox",
            "install",
            "nginx-1.30.8-99",
            "--download-only",
            "--allow-downgrade",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Toolbox {
                command: toolbox::ToolboxCommand::Install {
                    download_only: true,
                    no_backup: false,
                    allow_downgrade: true,
                    ..
                },
            })
        ));
        assert!(Cli::try_parse_from(["maestro", "toolbox", "install"]).is_err());
    }
}
