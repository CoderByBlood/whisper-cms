use crate::{
    fs::{
        doc::scan_and_process_docs,
        ext::{self, DiscoveredPlugin, DiscoveredTheme},
        filter::{self, DEFAULT_CONTENT_EXTS},
        scan::FolderScanConfig,
    },
    proxy::{EdgeError, EdgeRuntime},
    router::build_app_router,
};
use adapt::{
    cmd::{Commands, StartCmd},
    runtime::bootstrap::bootstrap_all,
};
use axum::Router;
use chrono::Utc;
use clap::Parser;
use domain::{
    doc::Document,
    setting::{ContentSettings, ExtensionSettings, Settings},
};
use serve::context::RequestContext;
use std::{marker::PhantomData, path::PathBuf, process::ExitCode};
use tokio::task::LocalSet;
use tracing::{debug, error, info};

pub type Result<T> = std::result::Result<T, EdgeError>;

/// WhisperCMS CLI — Edge Layer
#[derive(Parser, Debug)]
#[command(name = "whispercms", version, about = "WhisperCMS command-line tool")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

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

struct CommandIssued;
struct SettingsLoaded;
struct ContentLoaded;
struct ExtensionsLoaded;
struct RouterCreated;
struct ServerStarted;

struct StartProcess<State> {
    command: StartCmd,
    settings: Settings,
    content_settings: Option<ContentSettings>,
    documents: Option<Vec<Document>>,
    extensions: Option<(Vec<DiscoveredPlugin>, Vec<DiscoveredTheme>)>,
    router: Option<Router>,
    runtime: Option<EdgeRuntime>,
    _state: PhantomData<State>,
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

        Ok(StartProcess::<SettingsLoaded>::new(command, settings))
    }
}

impl StartProcess<SettingsLoaded> {
    fn new(command: StartCmd, settings: Settings) -> Self {
        Self {
            command,
            settings,
            content_settings: None,
            documents: None,
            extensions: None,
            router: None,
            runtime: None,
            _state: PhantomData,
        }
    }

    #[tracing::instrument(skip_all)]
    async fn scan_content_directory(self) -> Result<StartProcess<ContentLoaded>> {
        let dir = self.command.dir.clone();
        let content_settings = match self.settings.content.clone() {
            Some(settings) => settings,
            None => ContentSettings {
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
        debug!(
            "Document and Error Counts: ({}, {})",
            docs.len(),
            errs.len()
        );
        Ok(self.done(content_settings, docs))
    }

    #[tracing::instrument(skip_all)]
    fn done(self, cnt_sets: ContentSettings, docs: Vec<Document>) -> StartProcess<ContentLoaded> {
        StartProcess {
            command: self.command,
            settings: self.settings,
            documents: Some(docs),
            content_settings: Some(cnt_sets),
            extensions: self.extensions,
            router: self.router,
            runtime: self.runtime,
            _state: PhantomData,
        }
    }
}

impl StartProcess<ContentLoaded> {
    #[tracing::instrument(skip_all)]
    fn scan_extensions_directory(self) -> Result<StartProcess<ExtensionsLoaded>> {
        let dir = self.command.dir.clone();
        let ext_settings = match &self.settings.ext {
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
            command: self.command,
            settings: self.settings,
            documents: self.documents,
            content_settings: self.content_settings,
            extensions: Some((plugins, themes)),
            router: self.router,
            runtime: self.runtime,
            _state: PhantomData,
        }
    }
}

impl StartProcess<ExtensionsLoaded> {
    #[tracing::instrument(skip_all)]
    async fn register_routes_and_middleware(self) -> Result<StartProcess<RouterCreated>> {
        let (plugins, themes) = self.extensions.as_ref().unwrap();
        let plugin_cfgs = plugins.iter().map(|p| (&p.spec).into()).collect();
        let theme_cfgs = themes.iter().map(|t| (&t.spec).into()).collect();
        let theme_bnds = themes.iter().map(|t| (&t.spec).into()).collect();
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

        let router = build_app_router(
            self.content_settings.as_ref().unwrap().dir.clone(),
            handles,
            theme_bnds,
        );
        Ok(self.done(router))
    }

    #[tracing::instrument(skip_all)]
    fn done(self, router: Router) -> StartProcess<RouterCreated> {
        StartProcess {
            command: self.command,
            settings: self.settings,
            documents: self.documents,
            content_settings: self.content_settings,
            extensions: self.extensions,
            router: Some(router),
            runtime: self.runtime,
            _state: PhantomData,
        }
    }
}

impl StartProcess<RouterCreated> {
    #[tracing::instrument(skip_all)]
    async fn start_servers(mut self) -> Result<StartProcess<ServerStarted>> {
        let router = self.router.take().unwrap();
        let settings = self.settings.clone();
        let make_router = move || router;

        Ok(self.done(EdgeRuntime::start(settings, make_router).await?))
    }

    #[tracing::instrument(skip_all)]
    fn done(self, rt: EdgeRuntime) -> StartProcess<ServerStarted> {
        StartProcess {
            command: self.command,
            settings: self.settings,
            documents: self.documents,
            content_settings: self.content_settings,
            extensions: self.extensions,
            router: self.router,
            runtime: Some(rt),
            _state: PhantomData,
        }
    }
}

impl StartProcess<ServerStarted> {
    #[tracing::instrument(skip_all)]
    async fn is_running(&self) -> Result<()> {
        Ok(futures::future::pending::<()>().await)
    }
}

#[tracing::instrument(skip_all)]
async fn do_start(start: StartCmd) -> Result<()> {
    // TODO: Refector out into funtion that registers all injected dependencies
    use crate::db::resolver::{edge_build_request_context, edge_resolve};
    use serve::resolver::{set_build_request_context_fn, set_resolver_fn}; // your edge impls

    set_resolver_fn(edge_resolve).map_err(|e| EdgeError::Other(e.to_string()))?;
    set_build_request_context_fn(edge_build_request_context)
        .map_err(|e| EdgeError::Other(e.to_string()))?;
    // parse settings file -> does the settings file exist?  If yes, parse it
    let then = Utc::now();
    let process = StartProcess::<CommandIssued>::parse_settings_file(start)?;
    info!(
        "Settings parsed in {} milliseconds",
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

    // register routes and middleware in Axum
    let then = Utc::now();
    let process = process.register_routes_and_middleware().await?;
    info!(
        "Routes registered in {} milliseconds",
        Utc::now().timestamp_millis() - then.timestamp_millis()
    );

    // start servers (Axum web server/Pingora edge controller)
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
