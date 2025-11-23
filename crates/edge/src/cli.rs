use crate::{
    fs::{
        doc::scan_and_process_docs,
        ext::{self, DiscoveredPlugin, DiscoveredTheme},
        filter::{self, DEFAULT_CONTENT_EXTS},
        scan::FolderScanConfig,
    },
    proxy::EdgeError,
};
use adapt::cmd::{Commands, StartCmd};
use chrono::Utc;
use clap::Parser;
use domain::{
    doc::Document,
    setting::{ContentSettings, ExtensionSettings, Settings},
};
use std::{path::PathBuf, process::ExitCode};

pub type Result<T> = std::result::Result<T, EdgeError>;

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

    let result = match &cli.command {
        Commands::Start(start) => do_start(start).await,
    };

    dbg!(&result);

    result
        .map(|_| ExitCode::SUCCESS)
        .unwrap_or(ExitCode::FAILURE)
}

async fn do_start(start: &StartCmd) -> Result<()> {
    // parse settings file -> does the settings file exist?  If yes, parse it
    let then = Utc::now();
    let settings = parse_settings_file(&start.dir)?;
    dbg!(Utc::now().timestamp_millis() - then.timestamp_millis());
    //dbg!(&settings);

    // scan for content -> does the content directory exist?  If yes, scan it
    let then = Utc::now();
    let _docs = scan_content_directory(&start.dir, &settings).await?;
    dbg!(Utc::now().timestamp_millis() - then.timestamp_millis());

    // scan for extensions -> does the extensions directory exist?  If yes, scan it
    let then = Utc::now();
    let (_plugins, _extensions) = scan_extensions_directory(&start.dir, &settings)?;
    dbg!(Utc::now().timestamp_millis() - then.timestamp_millis());

    // register routes and middleware in Axum
    // start Axum (web server)
    // start Pingora (edge controller)
    Ok(())
}

/// Load settings from `<dir>/settings.toml`.
///
/// `dir` is the directory that contains `settings.toml`.
fn parse_settings_file(dir: &PathBuf) -> Result<Settings> {
    // Ensure directory exists
    if !dir.exists() {
        return Err(EdgeError::Config(format!(
            "Settings directory does not exist: {}",
            dir.display()
        )));
    }

    // Construct full path to file
    let mut path = PathBuf::from(dir);
    path.push("settings.toml");

    // Ensure file exists
    if !path.exists() {
        return Err(EdgeError::Config(format!(
            "settings.toml not found at {}",
            path.display()
        )));
    }

    // Read the file
    let text = std::fs::read_to_string(&path)
        .map_err(|err| EdgeError::Config(format!("Failed reading {}: {}", path.display(), err)))?;

    // Deserialize
    let settings: Settings = toml::from_str(&text).map_err(|err| {
        EdgeError::Config(format!(
            "Invalid settings.toml at {}: {}",
            path.display(),
            err
        ))
    })?;

    Ok(settings)
}

async fn scan_content_directory(dir: &PathBuf, settings: &Settings) -> Result<Vec<Document>> {
    let content_settings = match &settings.content {
        Some(settings) => settings,
        None => &ContentSettings {
            dir: PathBuf::from("./content/"),
            index_dir: None,
            extensions: vec![],
        },
    };

    let index_dir = match content_settings.index_dir.clone() {
        Some(d) => dir.join(d),
        None => dir.join("./content_index/"),
    };

    let index_dir = index_dir.join(
        regex::Regex::new(r"[^A-Za-z0-9]")
            .unwrap()
            .replace_all(Utc::now().to_rfc3339().as_str(), "_")
            .to_string(),
    );

    let root = dir.join(&content_settings.dir);
    let mut cfg = FolderScanConfig::default();

    cfg.file_re = Some(filter::build_filename_regex(
        match content_settings.extensions.len() {
            0 => DEFAULT_CONTENT_EXTS
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            _ => content_settings.extensions.clone(),
        },
    )?);

    let (docs, errs) = scan_and_process_docs(&root, cfg, index_dir).await?;
    dbg!(docs.len());
    dbg!(errs.len());
    dbg!(errs.first());
    Ok(docs)
}

fn scan_extensions_directory(
    dir: &PathBuf,
    settings: &Settings,
) -> Result<(Vec<DiscoveredPlugin>, Vec<DiscoveredTheme>)> {
    let ext_settings = match &settings.ext {
        Some(ext) => ext,
        None => &ExtensionSettings {
            dir: PathBuf::from("./extensions/"),
        },
    };

    let ext_dir = dir.join(&ext_settings.dir);

    let plugins = ext::discover_plugins(ext_dir.join("plugins/"))?;
    let themes = ext::discover_themes(ext_dir.join("themes/"))?;

    Ok((plugins, themes))
}

fn _register_routes_and_middleware() {
    // Implementation details
}

fn _start_axum_web_server() {
    // Implementation details
}

fn _start_pingora_edge_controller() {
    // Implementation details
}
