use url::Url;

use crate::db::conn::{self, Conn};

pub async fn connect(url: &Url, token: Option<&str>) -> anyhow::Result<Conn> {
    conn::connect_with_token(url.as_str(), token).await
}