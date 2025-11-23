use serde::Deserialize;
use std::{net::IpAddr, path::PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct CertSettings {
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EdgeSettings {
    /// IP address to bind the EdgeController listeners (HTTP + HTTPS)
    pub ip: IpAddr,

    /// Public HTTP port (redirect-only)
    pub http_port: u16,

    /// Public HTTPS port (proxy + TLS)
    pub https_port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoopbackSettings {
    /// Loopback IP for Axum backend
    pub ip: IpAddr,

    /// A/B hot-reload ports for WebServer
    pub port_a: u16,
    pub port_b: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtensionSettings {
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ContentSettings {
    pub dir: PathBuf,
    pub extensions: Vec<String>,
    pub index_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub cert: CertSettings,
    pub edge: EdgeSettings,
    pub loopback: LoopbackSettings,
    pub ext: Option<ExtensionSettings>,
    pub content: Option<ContentSettings>,
}
