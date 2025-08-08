pub mod handler;

use std::net::AddrParseError;

use axum::{
    response::{IntoResponse, Response},
    Json,
};
use hyper::StatusCode;
use ractor::{Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde_json::json;
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{debug, error};

use argon2::{
    password_hash::{PasswordHash, PasswordVerifier, SaltString},
    Argon2, PasswordHasher,
};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use validator::{Validate, ValidationError, ValidationErrors};
use zeroize::Zeroize;

use super::{
    config::{Config, ConfigArgs, ConfigEnvelope, ConfigReply},
    request::handler::RequestManager,
};

use crate::CliArgs;

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
                Ok(RequestState {
                    checkpoint: Checkpoint::Configured,
                    config: actor,
                    manager,
                })
            }
            Ok(Err(e)) => {
                debug!("Got config error: {:?}", e);
                Ok(RequestState {
                    checkpoint: Checkpoint::Deferred,
                    config: actor,
                    manager,
                })
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
            RequestEnvelope::Start { reply } => match state.manager.boot(&state.checkpoint).await {
                Ok(()) => Ok(reply.send(Ok(RequestReply::Start))?),
                Err(e) => Ok(reply.send(Err(e))?),
            },
        }
    }
}
/// Securely validated and hashed password
#[derive(Zeroize, Clone)]
#[zeroize(drop)]
pub struct ValidatedPassword {
    raw: SecretString,
    hashed: SecretString,
}

impl ValidatedPassword {
    /// Create a new validated and hashed password
    #[tracing::instrument(skip_all)]
    pub fn build(raw: String, salt: String) -> Result<Self, ValidationErrors> {
        let input = RawPasswordInput {
            password: raw,
            salt: salt.clone(),
        };

        input.validate()?;

        let salt = SaltString::encode_b64(salt.as_bytes()).map_err(|e| {
            let mut errors = ValidationErrors::new();
            let err = ValidationError::new("encode")
                .with_message(format!("Base64 encoding failed: {}", e).into());
            errors.add("salt", err);
            errors
        })?;

        let hash = Argon2::default()
            .hash_password(input.password.as_bytes(), &salt)
            .map_err(|e| {
                let mut errors = ValidationErrors::new();
                let err = ValidationError::new("hash")
                    .with_message(format!("Password hashing failed: {}", e).into());
                errors.add("password", err);
                errors
            })?
            .to_string();

        Ok(Self {
            raw: SecretString::new(input.password.into_boxed_str()),
            hashed: SecretString::new(hash.into_boxed_str()),
        })
    }

    /// Verify a raw password against the stored Argon2 hash
    #[tracing::instrument(skip_all)]
    pub fn verify(&self, attempt: &str) -> bool {
        match PasswordHash::new(self.hashed.expose_secret()) {
            Ok(parsed) => Argon2::default()
                .verify_password(attempt.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => false,
        }
    }

    /// Constant-time equality (not usually needed for Argon2 but useful for testing)
    #[tracing::instrument(skip_all)]
    pub fn eq_secure(&self, other: &ValidatedPassword) -> bool {
        self.hashed
            .expose_secret()
            .as_bytes()
            .ct_eq(other.hashed.expose_secret().as_bytes())
            .into()
    }

    /// For internal use only — e.g., storing to a DB
    pub fn as_hashed(&self) -> &str {
        self.hashed.expose_secret()
    }
}
/// Intermediate struct for validation only
#[derive(Validate)]
struct RawPasswordInput {
    #[validate(length(min = 8))]
    #[validate(custom(function = "validate_password_strength"))]
    password: String,

    #[validate(length(min = 16))]
    salt: String,
}

/// Prevent secret leakage through `Debug`
impl core::fmt::Debug for RawPasswordInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "RawPasswordInput(**REDACTED**)")
    }
}

/// Password strength validation rule
#[tracing::instrument(skip_all)]
fn validate_password_strength(p: &str) -> Result<(), ValidationError> {
    let symbols = "!@#$%^&*()_+-=";
    let has_upper = p.chars().any(|c| c.is_uppercase());
    let has_digit = p.chars().any(|c| c.is_ascii_digit());
    let has_symbol = p.chars().any(|c| symbols.contains(c));
    let mut errors = Vec::<String>::new();

    if !has_upper {
        errors.push("password must include at least one uppercase letter".into());
    }

    if !has_digit {
        errors.push("password must include at least one digit".into());
    }

    if !has_symbol {
        errors.push(format!(
            "password must include at least one special character: {symbols}"
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        let error = ValidationError::new("strength").with_message(format!("{:?}", errors).into());
        Err(error)
    }
}

/// Prevent secret leakage through `Debug`
impl core::fmt::Debug for ValidatedPassword {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ValidatedPassword(**REDACTED**)")
    }
}

/// Prevent accidental serialization
impl Serialize for ValidatedPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str("**REDACTED**")
    }
}

impl<'de> Deserialize<'de> for ValidatedPassword {
    fn deserialize<D>(_deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Err(serde::de::Error::custom(
            "Deserialization of ValidatedPassword is not allowed",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_validated_password_build_verify() {
        let password = String::from("StrongPass1$");
        let salt = "longsufficientlysalt".into();

        let validated = ValidatedPassword::build(password.clone(), salt).unwrap();
        assert!(validated.verify(&password));
        assert!(!validated.verify("wrongpass"));
    }

    #[tokio::test]
    async fn test_validated_password_eq_secure_false_for_different_hashes() {
        let pw1 =
            ValidatedPassword::build("StrongPass1$".into(), "salt123456789012".into()).unwrap();
        let pw2 =
            ValidatedPassword::build("StrongPass1$".into(), "salt999999999999".into()).unwrap();
        assert!(!pw1.eq_secure(&pw2)); // different salts → different hashes
    }

    #[tokio::test]
    async fn test_validation_fails_on_weak_password() {
        let weak_password = "password".into(); // No uppercase, digit, or symbol
        let salt = "longsufficientlysalt".into();

        let result = ValidatedPassword::build(weak_password, salt);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validated_password_debug_does_not_expose_secret() {
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let debug_str = format!("{:?}", password);
        assert!(debug_str.contains("**REDACTED**"));
    }

    #[tokio::test]
    async fn test_validated_password_serde_protected() {
        let password =
            ValidatedPassword::build("StrongPass1$".into(), "longsufficientlysalt".into()).unwrap();
        let serialized = serde_json::to_string(&password).unwrap();
        assert_eq!(serialized, "\"**REDACTED**\"");

        let json = "\"irrelevant\"";
        let result: Result<ValidatedPassword, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
