mod handler;

use std::{
    net::{AddrParseError, IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    body::Body,
    extract::State,
    http::Request,
    response::{IntoResponse, Response},
    Json, Router,
};
use hyper::StatusCode;
use serde_json::json;
use thiserror::Error;
use tokio::sync::RwLock;
use tower::ServiceExt;
use tracing::{debug, info};

use crate::{
    request::handler::{
        BootingHandler, ConfiguringHandler, InstallingHandler, NoopHandler, RequestHandler,
        ServingHandler,
    },
    startup::{Checkpoint, Process},
};

pub struct Manager {
    startup: Process,
    state: Arc<ManagerState>,
}

impl Manager {
    pub fn build(startup: Process) -> Result<Manager, ManagerError> {
        // Start in Booting state
        let initial_handler = Box::new(BootingHandler);
        let state = Arc::new(ManagerState {
            //phase: ManagerPhase::Booting,
            handler: RwLock::new(initial_handler),
        });
        Ok(Manager { startup, state })
    }

    pub async fn boot(&mut self, address: String, port: u16) -> Result<(), ManagerError> {
        self.state.transition_to(ManagerPhase::Booting).await?;

        match self.startup.execute() {
            Ok(_) => self.state.transition_to(ManagerPhase::Serving).await?,
            Err(_e) => {
                // Todo: How to get the error message to the client
                match self.startup.checkpoint() {
                    Checkpoint::Connected => {
                        self.state.transition_to(ManagerPhase::Installing).await?
                    }
                    Checkpoint::Validated => {
                        self.state.transition_to(ManagerPhase::Installing).await?
                    }
                    Checkpoint::Ready => self.state.transition_to(ManagerPhase::Serving).await?,
                    _ => self.state.transition_to(ManagerPhase::Configuring).await?,
                }
            }
        }

        // Fallback router
        let router = Router::new()
            .fallback(Self::dispatch_request)
            .with_state(self.state.clone());

        let ip: IpAddr = address.parse()?;
        let addr = SocketAddr::new(ip, port);

        info!("Listening on http://{}", addr);

        // Use hyper 1.6.0 compatible server setup
        let listener = tokio::net::TcpListener::bind(addr).await?;
        debug!("listener: {:?}", &listener);
        axum::serve(listener, router.into_make_service()).await?;
        Ok(())
    }

    #[tracing::instrument(skip_all)]
    async fn dispatch_request(
        State(mgr_state): State<Arc<ManagerState>>,
        req: Request<Body>,
    ) -> impl IntoResponse {
        let handler = mgr_state.handler.read().await;
        let router = handler.router().await;
        router.oneshot(req).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagerPhase {
    Booting,
    Configuring,
    Installing,
    Serving,
}

pub struct ManagerState {
    //pub phase: ManagerPhase,
    pub handler: RwLock<Box<dyn RequestHandler>>,
}

impl ManagerState {
    #[tracing::instrument(skip_all)]
    pub async fn transition_to(&self, next: ManagerPhase) -> Result<(), ManagerError> {
        let mut handler_guard = self.handler.write().await;
        let mut old_handler = std::mem::replace(&mut *handler_guard, Box::new(NoopHandler));
        old_handler.on_exit().await?;

        let mut new_handler: Box<dyn RequestHandler> = match next {
            ManagerPhase::Booting => Box::new(BootingHandler),
            ManagerPhase::Configuring => Box::new(ConfiguringHandler),
            ManagerPhase::Installing => Box::new(InstallingHandler),
            ManagerPhase::Serving => Box::new(ServingHandler),
        };

        new_handler.on_enter().await?;
        *handler_guard = new_handler;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serde JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),

    #[error("Template error: {0}")]
    Template(#[from] askama::Error),

    #[error("Could not parse IP address error: {0}")]
    IpParse(#[from] AddrParseError),

    //#[error("Unhandled internal application error")]
    //Internal,
}

impl IntoResponse for ManagerError {
    fn into_response(self) -> Response {
        let status = match self {
            ManagerError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ManagerError::IpParse(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ManagerError::SerdeJson(_) => StatusCode::BAD_REQUEST,
            ManagerError::Template(_) => StatusCode::INTERNAL_SERVER_ERROR,
            //ManagerError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = Json(json!({
            "error": self.to_string()
        }));

        (status, body).into_response()
    }
}
