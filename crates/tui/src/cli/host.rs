//! `nexus host` — the headless host daemon, tunnel and sleep guard.

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

use nexus_core::config;
use nexus_core::host::{HostConfig, HostServer};

use super::HostOptions;
use super::out;

// --- host daemon ---

#[derive(Clone, Copy)]
pub(super) enum HostServiceAction {
    Install,
    Uninstall,
}

enum HostTunnel {
    Quick {
        port: u16,
    },
    Named {
        config_path: PathBuf,
        tunnel_id: String,
    },
}

async fn start_host_tunnel(spec: &HostTunnel) -> Result<nexus_core::host::process::Tunnel> {
    match spec {
        HostTunnel::Quick { port } => nexus_core::host::process::Tunnel::quick(*port).await,
        HostTunnel::Named {
            config_path,
            tunnel_id,
        } => nexus_core::host::process::Tunnel::named(config_path, tunnel_id),
    }
}

async fn wait_host_tunnel(tunnel: &mut Option<nexus_core::host::process::Tunnel>) {
    if let Some(tunnel) = tunnel.as_mut() {
        let _ = tunnel.wait().await;
    }
}

/// Run the headless app actor, HTTP/SSE API, optional tunnel, and sleep guard.
#[allow(clippy::too_many_lines)]
pub(super) async fn host(options: HostOptions) -> Result<()> {
    let HostOptions {
        port,
        tunnel: quick_tunnel,
        no_sleep_guard,
        setup,
        service_action,
    } = options;
    if matches!(service_action, Some(HostServiceAction::Install)) {
        let path = nexus_core::host::process::install_service(port, quick_tunnel)?;
        out(format!("installed host service: {}", path.display()));
        print_service_activation_hint();
        return Ok(());
    }
    if matches!(service_action, Some(HostServiceAction::Uninstall)) {
        let removed = nexus_core::host::process::uninstall_service()?;
        out(if removed {
            "removed host service"
        } else {
            "host service was not installed"
        });
        print_service_deactivation_hint();
        return Ok(());
    }
    let saved_named = config::load_named_tunnel()?;
    let named = if setup {
        Some(setup_named_tunnel(port).await?)
    } else if quick_tunnel {
        saved_named
            .as_ref()
            .filter(|tunnel| tunnel.credentials_path.is_file() && tunnel.config_path.is_file())
            .map(|tunnel| reuse_named_tunnel(tunnel, port))
            .transpose()?
    } else {
        None
    };
    // Providers first: `load_all_providers` only scaffolds the commented
    // credential template when no config exists yet, and `ensure_host_token`
    // creates the file. Minting the token first would leave a first-ever
    // `nexus host` run with a config holding nothing but `host_token`.
    let saved = config::load_all_providers()?;
    let token = config::ensure_host_token()?;
    let mut app = nexus_core::boot(saved).await?;
    app.refresh_codex_in_background();
    app.init();
    let web_dir = std::env::var_os("NEXUS_WEB_DIR").map_or_else(
        || std::path::PathBuf::from("web/dist"),
        std::path::PathBuf::from,
    );
    let mut server = HostServer::bind(
        app,
        HostConfig::new(port, token.clone()).with_web_dir(web_dir),
    )
    .await?;
    let local_url = format!("http://{}", server.local_addr());

    let tunnel_spec = if let Some(named) = &named {
        Some(HostTunnel::Named {
            config_path: named.config_path.clone(),
            tunnel_id: named.tunnel_id.clone(),
        })
    } else if quick_tunnel {
        nexus_core::host::process::require_cloudflared()?;
        Some(HostTunnel::Quick {
            port: server.local_addr().port(),
        })
    } else {
        // No sidecar: the server remains reachable on loopback and all
        // shutdown cleanup is still centralized below.
        None
    };
    let mut tunnel = match tunnel_spec.as_ref() {
        Some(spec) => Some(start_host_tunnel(spec).await?),
        None => None,
    };
    let public_host = named
        .as_ref()
        .map(|result| format!("https://{}", result.hostname))
        .or_else(|| {
            tunnel
                .as_ref()
                .and_then(|sidecar| sidecar.public_url().map(str::to_string))
        });
    if let Some(base) = &public_host {
        // Browser navigations cannot send Authorization. App links therefore
        // carry the same token as a query parameter; `/v1/*` remains header-only.
        server.set_public_base(Some(base.clone())).await?;
        out(format!("public host: {base}"));
        let healthy = nexus_core::host::process::health_check(base).await;
        out(if healthy {
            "public host health check: reachable"
        } else {
            "warning: public host health check failed; tunnel may still be starting"
        });
    } else if quick_tunnel || setup {
        out("tunnel started, but no public URL was discovered");
    }
    let enrollment_host = public_host.as_deref().unwrap_or(&local_url);
    let enrollment = format!("nexus://host={enrollment_host}&token={token}");
    out(format!("host listening at {local_url}"));
    // Detached studio/service logs must not contain the bearer capability.
    if std::io::stdout().is_terminal() {
        out(format!("enrollment: {enrollment}"));
        match qrcode::QrCode::new(enrollment.as_bytes()) {
            Ok(code) => {
                let qr = code
                    .render::<qrcode::render::unicode::Dense1x2>()
                    .quiet_zone(true)
                    .build();
                out(qr);
            }
            Err(error) => out(format!("could not render enrollment QR: {error}")),
        }
    }

    let mut sleep_guard = if no_sleep_guard {
        None
    } else {
        match nexus_core::host::process::sleep_guard() {
            Ok(guard) => guard,
            Err(error) => {
                out(format!("sleep guard unavailable: {error:#}"));
                None
            }
        }
    };
    if nexus_core::host::process::on_battery() == Some(true) {
        out("warning: hosting while on battery power");
    }
    let mut restart_delay = 1u64;
    loop {
        let exited = if tunnel.is_some() {
            tokio::select! {
                result = tokio::signal::ctrl_c() => { let _ = result; false },
                () = wait_host_tunnel(&mut tunnel) => true,
            }
        } else {
            tokio::signal::ctrl_c()
                .await
                .context("waiting for shutdown")?;
            false
        };
        if !exited {
            break;
        }
        out("tunnel exited; restarting with backoff…");
        tunnel = None;
        let Some(spec) = tunnel_spec.as_ref() else {
            break;
        };
        tokio::time::sleep(std::time::Duration::from_secs(restart_delay)).await;
        match start_host_tunnel(spec).await {
            Ok(next) => {
                if let Some(base) = next.public_url().map(str::to_string) {
                    let _ = server.set_public_base(Some(base.clone())).await;
                    out(format!("new public host: {base}"));
                    restart_delay = 1;
                } else if matches!(spec, HostTunnel::Quick { .. }) {
                    // A replacement quick tunnel gets a *new* hostname, so a
                    // restart with no discovered URL means the advertised one
                    // is dead with nothing to replace it: stop handing it out.
                    // Keep backing off too — spawning succeeds even when
                    // cloudflared exits immediately, and resetting the delay
                    // here would respawn it once a second forever.
                    let _ = server.set_public_base(None).await;
                    out("tunnel restarted without a public URL; app links fall back to loopback");
                    restart_delay = (restart_delay * 2).min(30);
                } else {
                    // Named tunnels keep the hostname chosen during setup.
                    restart_delay = 1;
                }
                tunnel = Some(next);
            }
            Err(error) => {
                out(format!("tunnel restart failed: {error:#}"));
                restart_delay = (restart_delay * 2).min(30);
            }
        }
    }
    out("shutting down host…");
    let _ = server.set_public_base(None).await;
    if let Some(sidecar) = tunnel.as_mut() {
        sidecar.stop().await;
    }
    if let Some(guard) = sleep_guard.as_mut() {
        guard.stop().await;
    }
    server.shutdown().await;
    Ok(())
}

fn print_service_activation_hint() {
    match nexus_core::host::process::service_manager() {
        Some(nexus_core::host::process::ServiceManager::Systemd) => {
            out(
                "activate with: systemctl --user daemon-reload && systemctl --user enable --now nexus-host.service",
            );
        }
        Some(nexus_core::host::process::ServiceManager::Launchd) => {
            out(
                "activate with: launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/chat.nexus.host.plist",
            );
        }
        None => {}
    }
}

fn print_service_deactivation_hint() {
    match nexus_core::host::process::service_manager() {
        Some(nexus_core::host::process::ServiceManager::Systemd) => {
            out("if running, stop with: systemctl --user disable --now nexus-host.service");
        }
        Some(nexus_core::host::process::ServiceManager::Launchd) => {
            out("if running, stop with: launchctl bootout gui/$(id -u)/chat.nexus.host");
        }
        None => {}
    }
}

fn reuse_named_tunnel(
    saved: &config::NamedTunnelConfig,
    port: u16,
) -> Result<nexus_core::host::cloudflare::SetupResult> {
    nexus_core::host::cloudflare::write_named_config(
        &saved.config_path,
        &saved.credentials_path,
        &saved.tunnel_id,
        &saved.hostname,
        port,
    )?;
    Ok(nexus_core::host::cloudflare::SetupResult {
        tunnel_id: saved.tunnel_id.clone(),
        hostname: saved.hostname.clone(),
        credentials_path: saved.credentials_path.clone(),
        config_path: saved.config_path.clone(),
    })
}

/// Provision a named tunnel interactively when account/zone IDs were not
/// supplied through `CF_ACCOUNT_ID`/`CF_ZONE_ID`. Existing local tunnel
/// configuration is reused without contacting Cloudflare.
async fn setup_named_tunnel(port: u16) -> Result<nexus_core::host::cloudflare::SetupResult> {
    use nexus_core::host::cloudflare::{
        SetupOptions, list_accounts, list_zones, provision_named_tunnel,
    };
    if let Some(saved) = config::load_named_tunnel()?
        && saved.credentials_path.is_file()
        && saved.config_path.is_file()
    {
        return reuse_named_tunnel(&saved, port);
    }
    // An env override skips both the prompt *and* its listing call — an
    // empty value is treated as unset rather than sent to the API as an id.
    let env_id = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let account_id = if let Some(id) = env_id("CF_ACCOUNT_ID") {
        id
    } else {
        let accounts = list_accounts().await?;
        if accounts.is_empty() {
            bail!("Cloudflare API returned no accounts");
        }
        out("Cloudflare accounts:");
        for (index, account) in accounts.iter().enumerate() {
            out(format!(
                "  {}. {} ({})",
                index + 1,
                account.name,
                account.id
            ));
        }
        accounts[choose_index("account number", accounts.len())? - 1]
            .id
            .clone()
    };
    let (zone_id, zone_name) = if let Some(id) = env_id("CF_ZONE_ID") {
        (id, None)
    } else {
        let zones = list_zones().await?;
        if zones.is_empty() {
            bail!("Cloudflare API returned no DNS zones");
        }
        out("Cloudflare zones:");
        for (index, zone) in zones.iter().enumerate() {
            out(format!("  {}. {} ({})", index + 1, zone.name, zone.id));
        }
        let zone = &zones[choose_index("zone number", zones.len())? - 1];
        (zone.id.clone(), Some(zone.name.clone()))
    };
    let hostname = env_id("CF_HOSTNAME").unwrap_or_else(|| {
        let suffix = zone_name.as_deref().unwrap_or("example.com");
        prompt("hostname", &format!("nexus.{suffix}"))
    });
    let tunnel_name = env_id("CF_TUNNEL_NAME").unwrap_or_else(|| prompt("tunnel name", "nexus"));
    out(format!("creating Cloudflare tunnel {tunnel_name:?}…"));
    let result = provision_named_tunnel(&SetupOptions {
        account_id,
        zone_id,
        hostname,
        tunnel_name,
        port,
    })
    .await?;
    config::save_named_tunnel(&config::NamedTunnelConfig {
        tunnel_id: result.tunnel_id.clone(),
        hostname: result.hostname.clone(),
        credentials_path: result.credentials_path.clone(),
        config_path: result.config_path.clone(),
    })?;
    Ok(result)
}

fn choose_index(label: &str, count: usize) -> Result<usize> {
    let value = prompt(label, "1");
    let index = value
        .trim()
        .parse::<usize>()
        .with_context(|| format!("{label} must be a number"))?;
    if !(1..=count).contains(&index) {
        bail!("{label} must be between 1 and {count}");
    }
    Ok(index)
}

fn prompt(label: &str, default: &str) -> String {
    print!("{label} [{default}]: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return default.to_string();
    }
    let line = line.trim();
    if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    }
}
