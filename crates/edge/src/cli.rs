use adapt::cmd::{run_cli, Commands};
use clap::Parser;
use config::{Config, File};
use domain::setting::Settings;
use serve::ctx::AppCtx;
use std::process::ExitCode;

/// WhisperCMS CLI — Edge Layer
#[derive(Parser, Debug)]
#[command(name = "whispercms", version, about = "WhisperCMS command-line tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[tokio::main(flavor = "multi_thread")]
pub async fn start() -> ExitCode {
    let cli = Cli::parse();

    let ctx = match &cli.command {
        Commands::Start(start) => AppCtx::new().set_root(&start.dir),
    };

    let file = ctx.root_dir().join("whispercms-settings.toml");

    let cfg = match Config::builder()
        .add_source(File::with_name(
            file.as_os_str()
                .to_str()
                .expect("Failed to convert file path to string"),
        ))
        .build()
    {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("Error loading settings: {}", err);
            return ExitCode::FAILURE;
        }
    };

    let settings: Settings = match cfg.try_deserialize() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("Error deserializing settings: {}", err);
            return ExitCode::FAILURE;
        }
    };

    run_cli(ctx.set_settings(settings), cli.command).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::Parser;
    use std::env;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    // ── Env serialization (avoid races on WHISPERCMS_DIR) ────────────────────
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
    fn with_env_var<F: FnOnce() -> T, T>(key: &str, val: Option<&str>, f: F) -> T {
        let _g = env_lock().lock().unwrap();
        let prev = env::var_os(key);
        match val {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
        let out = f();
        match prev {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
        out
    }

    // ── Core parsing behavior ────────────────────────────────────────────────
    #[test]
    fn fails_when_missing_subcommand() {
        let err = Cli::try_parse_from(["whispercms"]).unwrap_err();
        use clap::error::ErrorKind::*;
        assert!(
            matches!(
                err.kind(),
                DisplayHelpOnMissingArgumentOrSubcommand | MissingSubcommand
            ),
            "got kind: {:?}",
            err.kind()
        );
        let msg = err.to_string();
        assert!(msg.to_lowercase().contains("usage"));
        assert!(msg.contains("whispercms"));
    }

    #[test]
    fn fails_on_unknown_subcommand() {
        let err = Cli::try_parse_from(["whispercms", "nope"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidSubcommand);
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("nope"));
        assert!(msg.contains("usage"));
    }

    #[test]
    fn displays_help_and_exits() {
        let err = Cli::try_parse_from(["whispercms", "--help"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = err.to_string();
        assert!(text.contains("whispercms"));
        assert!(text.to_lowercase().contains("usage"));
    }

    #[test]
    fn displays_version_and_exits() {
        let err = Cli::try_parse_from(["whispercms", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        let text = err.to_string();
        assert!(text.contains("whispercms"));
        assert!(!text.trim().is_empty());
    }

    // ── Start subcommand parsing (positional + env) ──────────────────────────
    #[test]
    fn start_parses_with_positional_dir_and_exposes_it() {
        let td = tempdir().expect("create temp dir");
        let cli = Cli::try_parse_from(["whispercms", "start", td.path().to_str().unwrap()])
            .expect("CLI should parse with positional dir");
        match cli.command {
            Commands::Start(ref s) => assert_eq!(s.dir, td.path()),
        }
    }

    #[test]
    fn start_parses_with_env_when_no_positional() {
        let td = tempdir().expect("create temp dir");
        with_env_var("WHISPERCMS_DIR", Some(td.path().to_str().unwrap()), || {
            let cli = Cli::try_parse_from(["whispercms", "start"])
                .expect("CLI should parse using env var");
            match cli.command {
                Commands::Start(ref s) => assert_eq!(s.dir, td.path()),
            }
        });
    }

    #[test]
    fn start_fails_when_missing_both_env_and_arg() {
        with_env_var("WHISPERCMS_DIR", None, || {
            let err = Cli::try_parse_from(["whispercms", "start"]).unwrap_err();
            use clap::error::ErrorKind::*;
            assert!(
                matches!(
                    err.kind(),
                    MissingRequiredArgument | DisplayHelpOnMissingArgumentOrSubcommand
                ),
                "unexpected error kind: {:?}",
                err.kind()
            );
        });
    }

    #[test]
    fn start_fails_when_positional_is_missing_path() {
        let missing: PathBuf = if cfg!(windows) {
            r"C:\__definitely__\__not__\__here__".into()
        } else {
            "/definitely/not/here/__whispercms__".into()
        };
        let err =
            Cli::try_parse_from(["whispercms", "start", missing.to_str().unwrap()]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().to_lowercase().contains("not found"));
    }
}
