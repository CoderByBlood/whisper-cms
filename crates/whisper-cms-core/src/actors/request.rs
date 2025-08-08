pub mod handler;

use std::net::AddrParseError;

use axum::{response::{IntoResponse, Response}, Json};
use hyper::StatusCode;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde_json::json;
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::{
    actors::{config::{Config, ConfigArgs, ConfigEnvelope, ConfigReply, ValidatedPassword}, request::handler::RequestManager},
    CliArgs,
};

#[derive(Debug, Error)]
pub enum RequestError {
    #[error("Could not complete request due to: {0}")]
    Internal(&'static str),

    #[error("Could not complete request due to: {0}")]
    Transformation(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serde error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Template error: {0}")]
    Template(#[from] askama::Error),

    #[error("Address Parse error: {0}")]
    AddrParse(#[from] AddrParseError),
}


impl IntoResponse for RequestError {
    fn into_response(self) -> Response {
        let status = match self {
            RequestError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RequestError::Serde(_) => StatusCode::BAD_REQUEST,
            //RequestError::SerdeJson(_) => StatusCode::BAD_REQUEST,
            RequestError::Template(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RequestError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RequestError::Transformation(_) => StatusCode::INTERNAL_SERVER_ERROR,
            RequestError::AddrParse(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };

        let body = Json(json!({
            "error": self.to_string()
        }));

        (status, body).into_response()
    }
}

#[derive(Debug)]
pub struct RequestArgs {
    pub args: CliArgs,
}

#[derive(Debug)]
pub enum RequestEnvelope {
    Start {
        reply: RpcReplyPort<Result<RequestReply, RequestError>>,
    },
}

#[derive(Debug)]
pub enum RequestReply {
    Start,
}

#[derive(Debug)]
pub struct RequestState {
    pub checkpoint: Checkpoint,
    pub config: ActorRef<ConfigEnvelope>,
    pub manager: RequestManager,
}


#[derive(Debug)]
pub enum Checkpoint {
    Deferred,
    Configured,
    Provisioned,
    Installed,
}

#[derive(Debug)]
pub struct Request;

impl Actor for Request {
    type Msg = RequestEnvelope;
    type State = RequestState;
    type Arguments = RequestArgs;

    #[tracing::instrument(skip_all)]
    async fn pre_start(
        &self,
        _me: ActorRef<Self::Msg>,
        args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("with args: {args:?}");

        let cli = args.args.clone();
        let (actor, _) = Actor::spawn(None, Config, ConfigArgs { args: args.args }).await?;
        let password = ValidatedPassword::build(cli.password, cli.salt)?;
        let manager = RequestManager::new(password, cli.address, cli.port);
        let (tx, rx) = oneshot::channel();
        let envelope = ConfigEnvelope::Load {
            reply: RpcReplyPort::from(tx),
        };

        actor.send_message(envelope)?;

        match rx.await {
            Ok(Ok(ConfigReply::Load(map))) => {
                debug!("Got config: {:?}", map);
                Ok(RequestState { checkpoint: Checkpoint::Configured, config: actor, manager })
            }
            Ok(Err(e)) => {
                debug!("Got config error: {:?}", e);
                Ok(RequestState { checkpoint: Checkpoint::Deferred, config: actor, manager })
            }
            Err(e) => {
                error!("Got receive error: {:?}", e);
                Err(Box::new(e))
            }
        }
    }

    #[tracing::instrument(skip_all)]
    async fn handle(
        &self,
        _me: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("{msg:?} with state {state:?}");

        match msg {
            RequestEnvelope::Start { reply } => {
                match state.manager.boot(&state.checkpoint).await {
                    Ok(()) => Ok(reply.send(Ok(RequestReply::Start))?),
                    Err(e) => Ok(reply.send(Err(e))?),
                }
            }
        }
    }
}
