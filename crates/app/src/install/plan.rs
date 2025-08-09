use serde::Deserialize;
use types::InstallPlan;
use secrecy::SecretString;

#[derive(Debug, Deserialize)]
pub struct InstallForm {
    pub site_name: String,
    pub base_url: String,
    pub timezone: String,
    pub admin_password: String,
}

impl InstallForm {
    pub fn validate_into_plan(self) -> Result<InstallPlan, Vec<String>> {
        let mut errs = Vec::new();

        if let Err(e) = domain::validate::site::validate_site_name(&self.site_name) {
            errs.push(format!("site_name: {e}"));
        }
        if let Err(e) = domain::validate::site::validate_base_url(&self.base_url) {
            errs.push(format!("base_url: {e}"));
        }
        if let Err(e) = domain::validate::site::validate_timezone(&self.timezone) {
            errs.push(format!("timezone: {e}"));
        }
        if let Err(_) = domain::security::password::validate_policy(&self.admin_password) {
            errs.push("admin_password: too weak (min 12 chars)".into());
        }

        if !errs.is_empty() {
            return Err(errs);
        }

        let base_url = url::Url::parse(&self.base_url)
            .map_err(|e| vec![format!("base_url parse error: {e}")])?;

        Ok(InstallPlan {
            site_name: self.site_name,
            base_url,
            timezone: self.timezone,
            admin_password: Some(SecretString::from(self.admin_password)),
        })
    }
}