use serde::Deserialize;
use std::net::IpAddr;

#[derive(Debug, Deserialize)]
pub struct Settings {
    pub server: Option<Server>,
}
#[derive(Debug, Deserialize)]
pub struct Server {
    pub ip: IpAddr,
    pub port: u16,
}
