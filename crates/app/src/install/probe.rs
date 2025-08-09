use anyhow::Result;
use domain::config::core::CoreConfig;
use infra::config::{io::read_toml_opt, paths::{core_toml, install_json}};
use infra::install::resume;
use types::{InstallState, InstallStep};

pub fn probe() -> Result<InstallState> {
    // If core.toml exists and installed=true → Complete
    if let Some(core) = read_toml_opt::<_, CoreConfig>(core_toml())? {
        if core.installed {
            return Ok(InstallState::Complete);
        }
    }

    // Partial install if we have an install.json
    if install_json().exists() {
        if let Some(r) = resume::load()? {
            if let Some(step) = r.last_step.as_deref().and_then(parse_step) {
                return Ok(InstallState::Partial { last_step: step });
            }
            return Ok(InstallState::Partial { last_step: InstallStep::GenerateSecrets });
        }
        return Ok(InstallState::Partial { last_step: InstallStep::GenerateSecrets });
    }

    Ok(InstallState::NeedsInstall)
}

fn parse_step(s: &str) -> Option<InstallStep> {
    use InstallStep::*;
    match s {
        "GenerateSecrets" => Some(GenerateSecrets),
        "WriteCoreConfigs" => Some(WriteCoreConfigs),
        "MigrateDb" => Some(MigrateDb),
        "SeedBaseline" => Some(SeedBaseline),
        "WriteAdminConfig" => Some(WriteAdminConfig),
        "FlipInstalledTrue" => Some(FlipInstalledTrue),
        _ => None,
    }
}