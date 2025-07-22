mod handler;

use std::sync::Arc;

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

use crate::{
    request::handler::{
        BootingHandler, ConfiguringHandler, InstallingHandler, NoopHandler, RequestHandler,
        ServingHandler,
    },
    startup::{self, Startup},
};

pub struct Manager {
    startup: Startup,
    state: Arc<ManagerState>,
}

impl Manager {
    pub fn build(startup: Startup) -> Result<Manager, ManagerError> {
        // Start in Booting state
        let initial_handler = Box::new(BootingHandler);
        let state = Arc::new(ManagerState {
            phase: ManagerPhase::Booting,
            handler: RwLock::new(initial_handler),
        });
        Ok(Manager { startup, state })
    }

    pub fn boot(&self) -> Result<(), ManagerError> {
        // Case 1 - config file is missing: ConfigState::Missing
        // Case 2 - config file exists but is invalid: ConfigState::Exists -> config.validate() -> ConfigState::Invalid
        // Case 3 - config file exists and is valid: ConfigState::Exists -> config.validate() -> ConfigState::Valid
        // Case 4 - config file exists, is valid, but unable to connect to DB: ConfigState::Valid -> config.get_connection().test_connection() -> false
        // Case 5 - config file exists, is valid, connects to DB: ConfigState::Valid -> config.get_connection().test_connection() -> true
        // Case 6 - config file exists, is valid, connects to DB, but no settings: ConfigState::Valid -> ...test_connection() -> true -> SettingsState:Empty
        // Case 7 - config file exists, is valid, connects to DB, settings exists, settings are invalid: ConfigState::Valid -> ...test_connection() -> true -> SettingsState:Invalid
        // Case 8 - config file exists, is valid, connects to DB, setting exists, settings valid: ConfigState::Valid -> ...test_connection() -> true -> SettingsState:Valid
        Ok(())
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
    pub phase: ManagerPhase,
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

    #[error("Unhandled internal application error")]
    Internal,
}

impl IntoResponse for ManagerError {
    fn into_response(self) -> Response {
        let status = match self {
            ManagerError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ManagerError::SerdeJson(_) => StatusCode::BAD_REQUEST,
            ManagerError::Template(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ManagerError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = Json(json!({
            "error": self.to_string()
        }));

        (status, body).into_response()
    }
}
