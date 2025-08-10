use secrecy::ExposeSecret;
use types::InstallStep;

use crate::progress::Msg;

#[tracing::instrument(skip_all)]
pub async fn run_all_from(
    mut plan: types::InstallPlan,
    tx: tokio::sync::broadcast::Sender<Msg>,
    start_from: Option<InstallStep>,
) -> anyhow::Result<()> {
    // Ordered steps for resume logic
    let order = [
        InstallStep::GenerateSecrets,
        InstallStep::WriteCoreConfigs,
        InstallStep::WriteAdminConfig,
        InstallStep::MigrateDb,
        InstallStep::SeedBaseline,
        InstallStep::FlipInstalledTrue,
    ];

    let start_idx = start_from
        .and_then(|s| order.iter().position(|&x| x == s).map(|i| i + 1))
        .unwrap_or(0);

    // Persist resume info after each successful step (no secrets at rest)
    let persist_step = |step: InstallStep| -> anyhow::Result<()> {
        let prev = infra::install::resume::load()?.unwrap_or_default();
        let st = infra::install::resume::Resume {
            last_step: Some(step_name(step).to_string()),
            started_at: prev.started_at,
            plan_fingerprint: prev.plan_fingerprint,
        };
        infra::install::resume::save(&st)
    };

    // Working domain config (secrets in-memory until written)
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
                infra::config::io::write_toml(infra::config::paths::core_toml(), &cfg)?;
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
                    created_at: time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_else(|_| "now".into()),
                };
                infra::config::io::write_toml(infra::config::paths::admin_toml(), &admin)?;
            }

            InstallStep::MigrateDb => {
                let database_url = std::env::var("DATABASE_URL")
                    .unwrap_or_else(|_| "sqlite://data/whispercms.db".into());

                let conn = infra::db::connect(&database_url).await?;
                infra::db::migrate::run(&conn).await?;
                infra::db::seed::baseline(&conn).await?;
            }

            InstallStep::SeedBaseline => {
                let database_url = std::env::var("DATABASE_URL")
                    .unwrap_or_else(|_| "sqlite://data/whispercms.db".into());

                let conn = infra::db::connect(&database_url).await?;
                infra::db::migrate::run(&conn).await?;
                infra::db::seed::baseline(&conn).await?;
            }

            InstallStep::FlipInstalledTrue => {
                cfg.installed = true;
                infra::config::io::write_toml(infra::config::paths::core_toml(), &cfg)?;
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
        InstallStep::MigrateDb => "MigrateDb",
        InstallStep::SeedBaseline => "SeedBaseline",
        InstallStep::WriteAdminConfig => "WriteAdminConfig",
        InstallStep::FlipInstalledTrue => "FlipInstalledTrue",
    }
}

// Parse a stored (or user-provided) step name back into an InstallStep.
// Accepts the canonical CamelCase plus common aliases.
#[tracing::instrument(skip_all)]
pub fn parse_step(name: &str) -> Option<InstallStep> {
    let raw = name.trim();

    // Fast path: exact canonical names (what we write via `step_name`)
    match raw {
        "GenerateSecrets" => return Some(InstallStep::GenerateSecrets),
        "WriteCoreConfigs" => return Some(InstallStep::WriteCoreConfigs),
        "WriteAdminConfig" => return Some(InstallStep::WriteAdminConfig),
        "MigrateDb" => return Some(InstallStep::MigrateDb),
        "SeedBaseline" => return Some(InstallStep::SeedBaseline),
        "FlipInstalledTrue" => return Some(InstallStep::FlipInstalledTrue),
        "Start" => return None, // treat "Start" as no specific step (fresh run)
        _ => {}
    }

    // Flexible aliases: kebab/snake/collapsed + case-insensitive
    let key = raw.to_ascii_lowercase().replace([' ', '_'], "-"); // normalize separators

    match key.as_str() {
        "generatesecrets" | "generate-secrets" => Some(InstallStep::GenerateSecrets),

        "writecoreconfigs" | "write-core-configs" | "core" | "core-configs" => {
            Some(InstallStep::WriteCoreConfigs)
        }

        "writeadminconfig" | "write-admin-config" | "admin" | "admin-config" => {
            Some(InstallStep::WriteAdminConfig)
        }

        "migratedb" | "migrate-db" | "migrate" | "db-migrate" => Some(InstallStep::MigrateDb),

        "seedbaseline" | "seed-baseline" | "seed" | "baseline" => Some(InstallStep::SeedBaseline),

        "flipinstalledtrue" | "flip-installed-true" | "finalize" | "complete" | "done" => {
            Some(InstallStep::FlipInstalledTrue)
        }

        "start" => None,
        _ => None,
    }
}
