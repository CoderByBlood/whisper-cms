use std::process::ExitCode;

use adapt::cli::{run_cli, Commands};
use clap::Parser;

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
    run_cli(cli.command).await
}
#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::Parser;

    // ─────────────────────────────────────────────────────────────────────
    // Core parsing behavior (no assumptions about subcommand variants)
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn fails_when_missing_subcommand() {
        let err = Cli::try_parse_from(["whispercms"]).unwrap_err();
        // Accept either behavior across clap minor versions
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
        // Help-on-missing should include program name and usage
        assert!(msg.to_lowercase().contains("usage"));
        assert!(msg.contains("whispercms"));
    }

    #[test]
    fn fails_on_unknown_subcommand() {
        let err = Cli::try_parse_from(["whispercms", "nope"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::InvalidSubcommand);
        let msg = err.to_string().to_lowercase();
        // Don’t rely on “subcommands” word; just ensure it mentions the bad token and shows usage
        assert!(msg.contains("nope"));
        assert!(msg.contains("usage"));
    }

    #[test]
    fn displays_help_and_exits() {
        let err = Cli::try_parse_from(["whispercms", "--help"]).unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayHelp);
        let text = err.to_string();
        // Be tolerant of formatting differences: name + usage is enough
        assert!(text.contains("whispercms"));
        assert!(text.to_lowercase().contains("usage"));
    }

    #[test]
    fn displays_version_and_exits() {
        let err = Cli::try_parse_from(["whispercms", "--version"]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::DisplayVersion);
        let text = err.to_string();
        assert!(text.contains("whispercms"));
        // Version string is injected by #[command(version)]
        assert!(!text.trim().is_empty());
    }
}
