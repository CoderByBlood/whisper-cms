// crates/operator/src/steps.rs

use anyhow::{Context, Result};
use secrecy::ExposeSecret;
use serde::Serialize;
use std::{fs, io::Write, path::Path};
use time::OffsetDateTime;
use url::Url;

use crate::progress::Msg;
use types::InstallStep;

#[tracing::instrument(skip_all)]
pub async fn run_all_from(
    mut plan: types::InstallPlan,
    tx: tokio::sync::broadcast::Sender<Msg>,
    start_from: Option<InstallStep>,
) -> Result<()> {
    // New canonical order
    let order = [
        InstallStep::GenerateSecrets,
        InstallStep::WriteCoreConfigs,
        InstallStep::WriteAdminConfig,
        InstallStep::WriteDbTokens, // NEW
        InstallStep::MigrateOpsDb,  // NEW
        InstallStep::SeedBaseline,
        InstallStep::FlipInstalledTrue,
    ];

    let start_idx = start_from
        .and_then(|s| order.iter().position(|&x| x == s).map(|i| i + 1))
        .unwrap_or(0);

    // Persist resume info after each successful step (no secrets at rest)
    let persist_step = |step: InstallStep| -> Result<()> {
        let prev = infra::install::resume::load()?.unwrap_or_default();
        let st = infra::install::resume::Resume {
            last_step: Some(step_name(step).to_string()),
            started_at: prev.started_at,
            plan_fingerprint: prev.plan_fingerprint,
        };
        infra::install::resume::save(&st)
    };

    // Working domain config (only used for secrets generation)
    let mut cfg = domain::config::core::CoreConfig {
        site_name: plan.site_name.clone(),
        base_url: plan.base_url.as_str().to_string(),
        timezone: plan.timezone.clone(),
        installed: false,
        secrets: None,
    };

    for &step in &order[start_idx..] {
        let _ = tx.send(Msg::Begin(step_name(step)));

        match step {
            InstallStep::GenerateSecrets => {
                cfg.secrets = Some(domain::security::secrets::generate());
            }

            InstallStep::WriteCoreConfigs => {
                write_core_configs(&plan).await?;
            }

            InstallStep::WriteAdminConfig => {
                // Take and zeroize the password immediately after hashing
                let pw = plan
                    .admin_password
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("missing admin password"))?;
                let hash = domain::security::password::hash_password(pw.expose_secret())
                    .map_err(|e| anyhow::anyhow!("hash: {e}"))?;

                let admin = domain::config::admin::AdminConfig {
                    admin_identity: "admin".into(),
                    password_hash: hash,
                    created_at: OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| "now".into()),
                };
                infra::config::io::write_toml(infra::config::paths::admin_toml(), &admin)?;
            }

            InstallStep::WriteDbTokens => {
                write_db_tokens(&plan).await?;
            }

            InstallStep::MigrateOpsDb => {
                migrate_ops_db(&plan).await?;
            }

            InstallStep::SeedBaseline => {
                seed_baseline(&plan).await?;
            }

            InstallStep::FlipInstalledTrue => {
                // If core.toml somehow wasn't written earlier, write it once now (installed=false).
                let core_path = infra::config::paths::core_toml();
                if !core_path.exists() {
                    // minimal write using the plan (no secrets)
                    write_core_configs(&plan).await?;
                }

                // Then drop the sentinel to mark installed
                mark_installed_sentinel()?;
            }
        }

        let _ = tx.send(Msg::Success(step_name(step)));
        persist_step(step)?;
    }

    // Done: clear resume and signal success
    let _ = infra::install::resume::clear();
    let _ = tx.send(Msg::Success("Install"));
    let _ = tx.send(Msg::Done);
    Ok(())
}

#[tracing::instrument(skip_all)]
pub fn step_name(s: InstallStep) -> &'static str {
    match s {
        InstallStep::GenerateSecrets => "GenerateSecrets",
        InstallStep::WriteCoreConfigs => "WriteCoreConfigs",
        InstallStep::WriteAdminConfig => "WriteAdminConfig",
        InstallStep::WriteDbTokens => "WriteDbTokens", // NEW
        InstallStep::MigrateOpsDb => "MigrateOpsDb",   // NEW
        InstallStep::SeedBaseline => "SeedBaseline",
        InstallStep::FlipInstalledTrue => "FlipInstalledTrue",
    }
}

// Parse a stored (or user-provided) step name back into an InstallStep.
#[tracing::instrument(skip_all)]
pub fn parse_step(name: &str) -> Option<InstallStep> {
    let raw = name.trim();
    match raw {
        "GenerateSecrets" => return Some(InstallStep::GenerateSecrets),
        "WriteCoreConfigs" => return Some(InstallStep::WriteCoreConfigs),
        "WriteAdminConfig" => return Some(InstallStep::WriteAdminConfig),
        "WriteDbTokens" => return Some(InstallStep::WriteDbTokens),
        "MigrateOpsDb" => return Some(InstallStep::MigrateOpsDb),
        "SeedBaseline" => return Some(InstallStep::SeedBaseline),
        "FlipInstalledTrue" => return Some(InstallStep::FlipInstalledTrue),
        "Start" => return None,
        _ => {}
    }
    let key = raw.to_ascii_lowercase().replace([' ', '_'], "-");
    match key.as_str() {
        "generatesecrets" | "generate-secrets" => Some(InstallStep::GenerateSecrets),
        "writecoreconfigs" | "write-core-configs" | "core" | "core-configs" => {
            Some(InstallStep::WriteCoreConfigs)
        }
        "writeadminconfig" | "write-admin-config" | "admin" | "admin-config" => {
            Some(InstallStep::WriteAdminConfig)
        }
        "writedbtokens" | "write-db-tokens" | "db-tokens" => Some(InstallStep::WriteDbTokens),
        "migrateopsdb" | "migrate-ops-db" | "migrate-db" | "db-migrate" => {
            Some(InstallStep::MigrateOpsDb)
        }
        "seedbaseline" | "seed-baseline" | "seed" | "baseline" => Some(InstallStep::SeedBaseline),
        "flipinstalledtrue" | "flip-installed-true" | "finalize" | "complete" | "done" => {
            Some(InstallStep::FlipInstalledTrue)
        }
        "start" => None,
        _ => None,
    }
}

// ------------------- concrete step functions -------------------

pub async fn write_core_configs(plan: &types::InstallPlan) -> Result<()> {
    #[derive(Serialize)]
    struct Site<'a> {
        name: &'a str,
        base_url: &'a str,
        timezone: &'a str,
    }
    #[derive(Serialize)]
    struct Database<'a> {
        ops_url: &'a str,
        content_url: &'a str,
    }
    #[derive(Serialize)]
    struct CoreToml<'a> {
        site: Site<'a>,
        database: Database<'a>,
    }

    let doc = CoreToml {
        site: Site {
            name: plan.site_name.as_str(),
            base_url: plan.base_url.as_str(),
            timezone: plan.timezone.as_str(),
        },
        database: Database {
            ops_url: plan.db_ops_url.as_str(),
            content_url: plan.db_content_url.as_str(),
        },
    };

    let path = infra::config::paths::core_toml();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o750));
        }
    }

    infra::config::io::write_toml(&path, &doc)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub async fn write_db_tokens(plan: &types::InstallPlan) -> Result<()> {
    fn is_remote(u: &Url) -> bool {
        matches!(u.scheme(), "libsql" | "http" | "https")
    }

    let dir = infra::config::paths::secrets_libsql_dir();
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    }

    fn write_secret_file(path: &Path, value: &str) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let mut tmp = path.to_path_buf();
        tmp.set_extension("tmp");

        {
            let mut f =
                fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("chmod 0600 {}", tmp.display()))?;
            }
            f.write_all(value.as_bytes())
                .with_context(|| format!("write {}", tmp.display()))?;
            let _ = f.sync_all();
        }

        fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("chmod 0600 {}", path.display()))?;
        }
        Ok(())
    }

    if is_remote(&plan.db_ops_url) {
        if let Some(tok) = &plan.db_ops_token {
            let p = infra::config::paths::secrets_ops_token();
            write_secret_file(&p, tok.expose_secret())?;
        }
    }
    if is_remote(&plan.db_content_url) {
        if let Some(tok) = &plan.db_content_token {
            let p = infra::config::paths::secrets_content_token();
            write_secret_file(&p, tok.expose_secret())?;
        }
    }
    Ok(())
}

/// Connect to the **ops DB** and run file-based migrations (disk-first, embedded-fallback).
pub async fn migrate_ops_db(plan: &types::InstallPlan) -> Result<()> {
    let token = plan.db_ops_token.as_ref().map(|s| s.expose_secret());
    let conn = infra::db::conn::connect_with_token(plan.db_ops_url.as_str(), token)
        .await
        .context("connect ops db")?;
    infra::db::ops::migrate::run(&conn)
        .await
        .context("migrate ops db")?;
    Ok(())
}

/// Seed baseline records (admin/site) into the **ops DB**.
pub async fn seed_baseline(plan: &types::InstallPlan) -> Result<()> {
    let token = plan.db_ops_token.as_ref().map(|s| s.expose_secret());
    let conn = infra::db::conn::connect_with_token(plan.db_ops_url.as_str(), token)
        .await
        .context("connect ops db")?;

    let admin_cfg = infra::config::io::read_toml_opt::<_, domain::config::admin::AdminConfig>(
        &infra::config::paths::admin_toml(),
    )?
    .ok_or_else(|| anyhow::anyhow!("admin config missing"))?;

    infra::db::ops::seed::baseline(
        &conn,
        &admin_cfg,
        &plan.site_name,
        plan.base_url.as_str(),
        &plan.timezone,
    )
    .await
    .context("seed baseline")?;
    Ok(())
}

/// Mark installation complete without rewriting core.toml.
fn mark_installed_sentinel() -> Result<()> {
    let path = infra::config::paths::core_toml()
        .parent()
        .map(|p| p.join("installed"))
        .unwrap_or_else(|| std::path::PathBuf::from("config/installed"));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("create dir {}", dir.display()))?;
    }
    fs::write(&path, b"ok").with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
