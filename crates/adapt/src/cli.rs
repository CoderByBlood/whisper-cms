use clap::{builder::ValueHint, Parser, Subcommand};
use serve::ctx::AppCtx;
use serve::ctx::AppError;
use std::path::PathBuf;
use std::{future::Future, pin::Pin, process::ExitCode, time::Duration};
use tower::limit::ConcurrencyLimitLayer;
use tower::timeout::TimeoutLayer;
use tower::{Service, ServiceBuilder, ServiceExt};

type Result<T> = std::result::Result<T, AppError>;

/// Unified request passed into Tower pipeline
pub struct CliReq {
    pub ctx: AppCtx,
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

/// The Dispatcher — maps commands to business logic
pub struct Dispatcher;

impl Service<CliReq> for Dispatcher {
    type Response = ExitCode;
    type Error = AppError;
    type Future = Pin<Box<dyn Future<Output = Result<ExitCode>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut std::task::Context<'_>) -> std::task::Poll<Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: CliReq) -> Self::Future {
        Box::pin(async move {
            match req.cmd {
                Commands::Start(cmd) => start_command(&req.ctx, cmd).await,
            }
        })
    }
}

/// Run the dispatcher through Tower layers
pub async fn run_cli(cmd: Commands) -> ExitCode {
    let start = match &cmd {
        Commands::Start(start) => start,
    };

    let req = CliReq {
        ctx: AppCtx::new(start.dir.clone()),
        cmd,
    };

    let svc = ServiceBuilder::new()
        .layer(ConcurrencyLimitLayer::new(1))
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
        // .layer(your custom tracing / confirm layers here)
        .service(Dispatcher);

    match svc.oneshot(req).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(1)
        }
    }
}

// ---------------- Business logic layer ----------------

async fn start_command(ctx: &AppCtx, _cmd: StartCmd) -> Result<ExitCode> {
    // Delegate to the serve tier
    serve::start::start_command(ctx)
        .await
        .and(Ok(ExitCode::SUCCESS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::Parser;
    use std::sync::{Mutex, OnceLock};
    use std::{
        env,
        path::PathBuf,
        task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    };
    use tempfile::{tempdir, NamedTempFile};
    use tokio; // for #[tokio::test]

    // ───────────────────────────────────────────────────────────────
    // Env serialization (prevents parallel tests from racing on env)
    // ───────────────────────────────────────────────────────────────
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn with_env_var<F: FnOnce() -> T, T>(key: &str, val: Option<&str>, f: F) -> T {
        let _g = env_lock().lock().unwrap(); // serialize env mutations
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

    // ───────────────────────────────────────────────────────────────
    // Minimal noop waker (avoid bringing in `futures` as a dev-dep)
    // ───────────────────────────────────────────────────────────────
    fn noop_waker() -> Waker {
        fn no_op(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            raw_waker()
        }
        fn raw_waker() -> RawWaker {
            RawWaker::new(
                std::ptr::null(),
                &RawWakerVTable::new(clone, no_op, no_op, no_op),
            )
        }
        unsafe { Waker::from_raw(raw_waker()) }
    }

    // ─────────────────────────────
    // dir_must_exist (unit tests)
    // ─────────────────────────────
    #[test]
    fn dir_validator_ok_for_existing_dir() {
        let td = tempdir().expect("tmpdir");
        let p = td.path().to_str().unwrap();
        let out = dir_must_exist(p).expect("validator should accept existing dir");
        assert_eq!(out, td.path());
    }

    #[test]
    fn dir_validator_err_for_missing_path() {
        let missing: PathBuf = if cfg!(windows) {
            r"C:\__definitely__\__not__\__here__".into()
        } else {
            "/definitely/not/here/__whispercms__".into()
        };
        let err = dir_must_exist(missing.to_str().unwrap()).unwrap_err();
        assert!(err.to_lowercase().contains("not found"));
    }

    #[test]
    fn dir_validator_err_when_path_is_a_file() {
        let f = NamedTempFile::new().unwrap();
        let err = dir_must_exist(f.path().to_str().unwrap()).unwrap_err();
        assert!(err.to_lowercase().contains("not a directory"));
    }

    // ─────────────────────────────
    // StartCmd parsing (clap derive)
    // ─────────────────────────────
    #[test]
    fn startcmd_parses_with_positional_dir() {
        let td = tempdir().unwrap();
        let args = ["whispercms", td.path().to_str().unwrap()];
        let parsed = StartCmd::try_parse_from(args).expect("parse positional dir");
        assert_eq!(parsed.dir, td.path());
    }

    #[test]
    fn startcmd_parses_with_env_when_no_positional() {
        let td = tempdir().unwrap();
        with_env_var("WHISPERCMS_DIR", Some(td.path().to_str().unwrap()), || {
            let parsed = StartCmd::try_parse_from(["whispercms"]).expect("parse from env");
            assert_eq!(parsed.dir, td.path());
        });
    }

    #[test]
    fn startcmd_fails_when_missing_both_env_and_arg() {
        with_env_var("WHISPERCMS_DIR", None, || {
            let err = StartCmd::try_parse_from(["whispercms"]).unwrap_err();
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
    fn startcmd_fails_when_positional_is_file() {
        let f = NamedTempFile::new().unwrap();
        let err = StartCmd::try_parse_from(["whispercms", f.path().to_str().unwrap()]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().to_lowercase().contains("not a directory"));
    }

    #[test]
    fn startcmd_fails_when_positional_missing_path() {
        let missing: PathBuf = if cfg!(windows) {
            r"C:\__definitely__\__not__\__here__".into()
        } else {
            "/definitely/not/here/__whispercms__".into()
        };
        let err = StartCmd::try_parse_from(["whispercms", missing.to_str().unwrap()]).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        assert!(err.to_string().to_lowercase().contains("not found"));
    }

    // ─────────────────────────────
    // Dispatcher / run_cli
    // ─────────────────────────────
    #[tokio::test]
    async fn run_cli_returns_success_on_valid_dir() {
        let td = tempdir().unwrap();
        let cmd = Commands::Start(StartCmd {
            dir: td.path().to_path_buf(),
        });
        let code = run_cli(cmd).await;
        assert_eq!(code, ExitCode::SUCCESS);
    }

    #[tokio::test]
    async fn run_cli_returns_nonzero_on_bogus_dir() {
        // Construct a bogus path directly to bypass clap validation.
        let bogus: PathBuf = if cfg!(windows) {
            r"C:\__definitely__\__not__\__here__".into()
        } else {
            "/definitely/not/here/__whispercms__".into()
        };
        let cmd = Commands::Start(StartCmd { dir: bogus });
        let code = run_cli(cmd).await;
        assert_ne!(code, ExitCode::SUCCESS, "expected non-zero exit on failure");
    }

    #[test]
    fn dispatcher_ready_ok() {
        let mut d = Dispatcher;
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let ready = d.poll_ready(&mut cx);
        assert!(matches!(ready, Poll::Ready(Ok(()))));
    }
}
