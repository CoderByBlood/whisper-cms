use adapt::Error as AdaptError;
use anyhow::Error as AnyError;
use pingora::protocols::raw_connect::ConnectProxyError;
use serve::Error as ServeError;
use std::{io, path::PathBuf, string::FromUtf8Error};
use tantivy::{directory::error::OpenDirectoryError, query::QueryParserError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    // Note: Server::new returns Result<Server, Box<pingora::Error>>
    #[error("Pingora error: {0}")]
    Pingora(#[from] Box<pingora::Error>),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Channel closed")]
    Channel,

    #[error("Proxy error: {0}")]
    Proxy(#[from] ConnectProxyError),

    #[error("Regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("Tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("adapt error: {0}")]
    AdaptError(#[from] AdaptError),

    #[error("sdapt error: {0}")]
    ServeError(#[from] ServeError),

    #[error("query parser error: {0}")]
    QueryParserError(#[from] QueryParserError),

    #[error("failed to open directory: {0}")]
    OpenDirectory(#[from] OpenDirectoryError),

    #[error("writer lock was poisoned")]
    WriterPoisoned,

    #[error("document with path {0:?} not found")]
    NotFound(PathBuf),

    #[error("path field missing or not a string")]
    MissingPathField,

    #[error("content field missing or not a string")]
    MissingContentField,

    #[error("IndexedJson: {0}")]
    IndexedJson(#[source] AnyError),

    #[error("No Index")]
    NoIndex(String),

    #[error("No Casm{0}")]
    NoCas(String),

    #[error("FromUtf8Error {0}")]
    FromUtf8Error(#[from] FromUtf8Error),
}
