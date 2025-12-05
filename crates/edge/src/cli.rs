// crates/edge/src/cli.rs

use crate::fs::index::{set_cas_index, ContentMgr};
use crate::{
    fs::{
        ext::{self, DiscoveredPlugin, DiscoveredTheme, ThemeBinding},
        filter::{self, DEFAULT_CONTENT_EXTS},
    },
    proxy::{EdgeError, EdgeRuntime},
};
use adapt::runtime::bootstrap::{bootstrap_all, RuntimeHandles};
use chrono::Utc;
use clap::{builder::ValueHint, Parser, Subcommand};
use domain::{
    doc::Document,
    setting::{ContentSettings, ExtensionSettings, Settings},
};
use serve::indexer::scan_and_process_docs;
use serve::{indexer::FolderScanConfig, render::http::RequestContext};
use std::{path::PathBuf, process::ExitCode};
use tokio::task::LocalSet;
use tracing::{debug, error, info};

pub type Result<T> = std::result::Result<T, EdgeError>;

/// WhisperCMS CLI — Edge Layer
#[tokio::main(flavor = "multi_thread")]
#[tracing::instrument(skip_all)]
pub async fn start() -> ExitCode {
    let local = LocalSet::new();

    local
        .run_until(async {
            // Everything in here can safely call spawn_local,
            // including bootstrap_all → PluginRuntimeClient::spawn.
            let cli = Cli::parse();

            let result = match cli.command {
                Commands::Start(start) => do_start(start).await,
            };

            result.map_or_else(
                |e| {
                    error!("Failed to start WhisperCMS Edge: {}", e);
                    ExitCode::FAILURE
                },
                |_| {
                    info!("WhisperCMS Edge started successfully");
                    ExitCode::SUCCESS
                },
            )
        })
        .await
}

#[tracing::instrument(skip_all)]
async fn do_start(start: StartCmd) -> Result<()> {
    // parse settings file -> does the settings file exist?  If yes, parse it
    let then = Utc::now();
    let process = StartProcess::<CommandIssued>::parse_settings_file(start)?;
    info!(
        "Settings parsed in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    // inject dependencies -> adapt, serve, and domain have dependencies so inject
    let then = Utc::now();
    let process = process.inject_dependencies().await?;
    info!(
        "Dependencies injected in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    // scan for content -> does the content directory exist?  If yes, scan it
    let then = Utc::now();
    let process = process.scan_content_directory().await?;
    info!(
        "Content scanned in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    // scan for extensions -> does the extensions directory exist?  If yes, scan it
    let then = Utc::now();
    let process = process.scan_extensions_directory()?;
    info!(
        "Extensions scanned in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    // register routes and middleware in Actix
    let then = Utc::now();
    let process = process.register_routes_and_middleware().await?;
    info!(
        "Routes registered in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    // start servers (Actix web server/Pingora edge controller)
    let then = Utc::now();
    let process = process.start_servers().await?;
    info!(
        "Servers started in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    while let Ok(()) = process.is_running().await {
        info!("Restarting the server");
    }

    Ok(())
}

#[derive(Parser, Debug)]
#[command(name = "whispercms", version, about = "WhisperCMS command-line tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// Unified request passed into Tower pipeline (placeholder for future use)
pub struct CliReq {
    pub cmd: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start WhisperCMS using the specified directory
    Start(StartCmd),
}

#[derive(Parser, Debug)]
pub struct StartCmd {
    /// Target directory (or set WHISPERCMS_DIR)
    ///
    /// Must exist, be a directory, and be readable & writable.
    #[arg(
        value_name = "DIR",
        env = "WHISPERCMS_DIR",
        required = true,
        value_hint = ValueHint::DirPath,
        value_parser = dir_must_exist
    )]
    pub dir: PathBuf,
}

fn dir_must_exist(s: &str) -> std::result::Result<PathBuf, String> {
    let p = PathBuf::from(s);
    if !p.exists() {
        return Err(format!("Not found: {}", p.display()));
    }
    if !p.is_dir() {
        return Err(format!("Not a directory: {}", p.display()));
    }
    Ok(p)
}

// ─────────────────────────────────────────────────────────────────────────────
// Start process state machine
// ─────────────────────────────────────────────────────────────────────────────

trait ProcessState {}

struct CommandIssued;

struct SettingsLoaded {
    command: StartCmd,
    settings: Settings,
    content_settings: ContentSettings,
}

struct ContentLoaded {
    command: StartCmd,
    settings: Settings,
    content_settings: ContentSettings,
    documents: Vec<Document>,
}

struct ExtensionsLoaded {
    command: StartCmd,
    settings: Settings,
    content_settings: ContentSettings,
    documents: Vec<Document>,
    extensions: (Vec<DiscoveredPlugin>, Vec<DiscoveredTheme>),
}

struct RouterCreated {
    command: StartCmd,
    settings: Settings,
    content_settings: ContentSettings,
    documents: Vec<Document>,
    extensions: (Vec<DiscoveredPlugin>, Vec<DiscoveredTheme>),
    handles: RuntimeHandles,
    theme_bindings: Vec<ThemeBinding>,
}

struct ServerStarted {
    _command: StartCmd,
    _settings: Settings,
    _content_settings: ContentSettings,
    _documents: Vec<Document>,
    _extensions: (Vec<DiscoveredPlugin>, Vec<DiscoveredTheme>),
    _handles: RuntimeHandles,
    _theme_bindings: Vec<ThemeBinding>,
    _runtime: EdgeRuntime,
}

impl ProcessState for CommandIssued {}
impl ProcessState for SettingsLoaded {}
impl ProcessState for ContentLoaded {}
impl ProcessState for ExtensionsLoaded {}
impl ProcessState for RouterCreated {}
impl ProcessState for ServerStarted {}

struct StartProcess<S: ProcessState> {
    state: S,
}

impl StartProcess<CommandIssued> {
    /// Load settings from `<dir>/settings.toml`.
    ///
    /// `dir` is the directory that contains `settings.toml`.
    #[tracing::instrument(skip_all)]
    fn parse_settings_file(command: StartCmd) -> Result<StartProcess<SettingsLoaded>> {
        let dir = command.dir.clone();
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
        let text = std::fs::read_to_string(&path).map_err(|err| {
            EdgeError::Config(format!("Failed reading {}: {}", path.display(), err))
        })?;

        // Deserialize
        let settings: Settings = toml::from_str(&text).map_err(|err| {
            EdgeError::Config(format!(
                "Invalid settings.toml at {}: {}",
                path.display(),
                err
            ))
        })?;

        let content_settings = match settings.content.clone() {
            Some(settings) => settings,
            None => ContentSettings {
                dir: PathBuf::from("./content/"),
                index_dir: None,
                extensions: vec![],
            },
        };

        Ok(StartProcess::<SettingsLoaded>::new(
            command,
            settings,
            content_settings,
        ))
    }
}

impl StartProcess<SettingsLoaded> {
    fn new(command: StartCmd, settings: Settings, content_settings: ContentSettings) -> Self {
        Self {
            state: SettingsLoaded {
                command,
                settings,
                content_settings,
            },
        }
    }

    #[tracing::instrument(skip_all)]
    async fn inject_dependencies(self) -> Result<Self> {
        let dir = self.state.command.dir.clone();

        let index_dir = match self.state.content_settings.index_dir.clone() {
            Some(d) => dir.join(d),
            None => dir.join("./content_index/"),
        };

        let index_dir = index_dir.join(
            regex::Regex::new(r"[^A-Za-z0-9]")
                .unwrap()
                .replace_all(Utc::now().to_rfc3339().as_str(), "_")
                .to_string(),
        );

        // inject the dependencies
        set_cas_index(index_dir.clone()).await?;

        Ok(Self {
            state: SettingsLoaded {
                command: self.state.command,
                settings: self.state.settings,
                content_settings: ContentSettings {
                    dir: self.state.content_settings.dir,
                    extensions: self.state.content_settings.extensions,
                    index_dir: Some(index_dir),
                },
            },
        })
    }

    #[tracing::instrument(skip_all)]
    async fn scan_content_directory(self) -> Result<StartProcess<ContentLoaded>> {
        let dir = self.state.command.dir.clone();
        let mut cfg = FolderScanConfig::default();
        let content_settings = self.state.content_settings.clone();

        cfg.file_re = Some(filter::build_filename_regex(
            match content_settings.extensions.len() {
                0 => DEFAULT_CONTENT_EXTS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect(),
                _ => content_settings.extensions.clone(),
            },
        )?);

        // This now calls the serve-level pipeline
        let root = dir.join(&content_settings.dir);
        let (docs, errs) = scan_and_process_docs(&root, cfg, ContentMgr::new(root.clone())).await?;

        debug!(
            "Document and Error Counts: ({}, {})",
            docs.len(),
            errs.len()
        );
        Ok(self.done(docs))
    }

    #[tracing::instrument(skip_all)]
    fn done(self, docs: Vec<Document>) -> StartProcess<ContentLoaded> {
        StartProcess {
            state: ContentLoaded {
                command: self.state.command,
                settings: self.state.settings,
                content_settings: self.state.content_settings,
                documents: docs,
            },
        }
    }
}

impl StartProcess<ContentLoaded> {
    #[tracing::instrument(skip_all)]
    fn scan_extensions_directory(self) -> Result<StartProcess<ExtensionsLoaded>> {
        let dir = self.state.command.dir.clone();
        let ext_settings = match &self.state.settings.ext {
            Some(ext) => ext,
            None => &ExtensionSettings {
                dir: PathBuf::from("./extensions/"),
            },
        };

        let ext_dir = dir.join(&ext_settings.dir);

        let plugins = ext::discover_plugins(ext_dir.join("plugins/"))?;
        let themes = ext::discover_themes(ext_dir.join("themes/"))?;

        Ok(self.done(plugins, themes))
    }

    #[tracing::instrument(skip_all)]
    fn done(
        self,
        plugins: Vec<DiscoveredPlugin>,
        themes: Vec<DiscoveredTheme>,
    ) -> StartProcess<ExtensionsLoaded> {
        StartProcess {
            state: ExtensionsLoaded {
                command: self.state.command,
                settings: self.state.settings,
                content_settings: self.state.content_settings,
                documents: self.state.documents,
                extensions: (plugins, themes),
            },
        }
    }
}

impl StartProcess<ExtensionsLoaded> {
    #[tracing::instrument(skip_all)]
    async fn register_routes_and_middleware(self) -> Result<StartProcess<RouterCreated>> {
        let (plugins, themes) = self.state.extensions.clone();
        let plugin_cfgs = plugins.iter().map(|p| (&p.spec).into()).collect();
        let theme_cfgs = themes.iter().map(|t| (&t.spec).into()).collect();

        // Build ThemeBinding values from DiscoveredTheme so we have template_root.
        let theme_bnds: Vec<ThemeBinding> = themes.iter().map(ThemeBinding::from).collect();

        let handles = bootstrap_all(plugin_cfgs, theme_cfgs)?;

        info!("Initializing themes...");
        handles
            .theme_client
            .init_all(RequestContext::builder().build())
            .await?;
        info!("Initializing plugins...");
        handles
            .plugin_client
            .init_all(RequestContext::builder().build())
            .await?;

        info!("Plugins and themes initialized successfully");

        Ok(self.done(handles, theme_bnds))
    }

    #[tracing::instrument(skip_all)]
    fn done(
        self,
        handles: RuntimeHandles,
        theme_bindings: Vec<ThemeBinding>,
    ) -> StartProcess<RouterCreated> {
        StartProcess {
            state: RouterCreated {
                command: self.state.command,
                settings: self.state.settings,
                content_settings: self.state.content_settings,
                documents: self.state.documents,
                extensions: self.state.extensions,
                handles,
                theme_bindings,
            },
        }
    }
}

impl StartProcess<RouterCreated> {
    #[tracing::instrument(skip_all)]
    async fn start_servers(self) -> Result<StartProcess<ServerStarted>> {
        let handles = self.state.handles.clone();
        let theme_bindings = self.state.theme_bindings.clone();
        let settings = self.state.settings.clone();
        let root = self.state.command.dir.clone();

        let runtime =
            EdgeRuntime::start(root, settings, handles.clone(), theme_bindings.clone()).await?;

        Ok(self.done(runtime))
    }

    #[tracing::instrument(skip_all)]
    fn done(self, runtime: EdgeRuntime) -> StartProcess<ServerStarted> {
        StartProcess {
            state: ServerStarted {
                _command: self.state.command,
                _settings: self.state.settings,
                _content_settings: self.state.content_settings,
                _documents: self.state.documents,
                _extensions: self.state.extensions,
                _handles: self.state.handles,
                _theme_bindings: self.state.theme_bindings,
                _runtime: runtime,
            },
        }
    }
}

impl StartProcess<ServerStarted> {
    #[tracing::instrument(skip_all)]
    async fn is_running(&self) -> Result<()> {
        Ok(futures::future::pending::<()>().await)
    }
}
