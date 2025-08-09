
use serde::{Deserialize, Serialize};
use crate::types::Secrets;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CoreConfig {
    pub site_name: String,
    pub base_url: String,
    pub timezone: String,
    pub installed: bool,
    #[serde(skip)]
    pub secrets: Option<Secrets>,
}
