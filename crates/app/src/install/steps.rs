use crate::install::progress::Msg;
use anyhow::Result;
use secrecy::ExposeSecret;
use tokio::sync::broadcast::Sender;
use types::InstallPlan;

pub async fn run_all(plan: InstallPlan, tx: Sender<Msg>) -> Result<()> {
    // 1) Generate secrets (in-memory for now)
    tx.send(Msg::Info("generating secrets")).ok();
    let secrets = domain::security::secrets::generate();

    // 2) Write core config installed=false
    tx.send(Msg::Begin("WriteCoreConfigs")).ok();
    let mut core = domain::config::core::CoreConfig {
        site_name: plan.site_name.clone(),
        base_url: plan.base_url.as_str().to_string(),
        timezone: plan.timezone.clone(),
        installed: false,
        secrets: Some(secrets),
    };
    infra::config::io::write_toml(infra::config::paths::core_toml(), &core)?;
    tx.send(Msg::Success("WriteCoreConfigs")).ok();

    // 3) Write admin config (hash)
    tx.send(Msg::Begin("WriteAdminConfig")).ok();
    let hash =
        domain::security::password::hash_password(plan.admin_password.unwrap().expose_secret())
            .map_err(|e| anyhow::anyhow!("hash: {e}"))?;
    let admin = domain::config::admin::AdminConfig {
        admin_identity: "admin".into(),
        password_hash: hash,
        created_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| "now".into()),
    };
    infra::config::io::write_toml(infra::config::paths::admin_toml(), &admin)?;
    tx.send(Msg::Success("WriteAdminConfig")).ok();

    // 4) Migrate DB
    tx.send(Msg::Begin("MigrateDb")).ok();
    // For demo: read DATABASE_URL from env or default to a local sqlite file
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://data/whispercms.db".into());
    let pool = infra::db::pool::connect(&database_url).await?;
    infra::db::migrate::run(&pool).await?;
    tx.send(Msg::Success("MigrateDb")).ok();

    // 5) Seed baseline
    tx.send(Msg::Begin("SeedBaseline")).ok();
    infra::db::seed::baseline(&pool).await?;
    tx.send(Msg::Success("SeedBaseline")).ok();

    // 6) Flip installed=true and persist
    tx.send(Msg::Begin("FlipInstalledTrue")).ok();
    core.installed = true;
    infra::config::io::write_toml(infra::config::paths::core_toml(), &core)?;
    // Clear any resume state if you later add it
    tx.send(Msg::Success("FlipInstalledTrue")).ok();

    Ok(())
}
