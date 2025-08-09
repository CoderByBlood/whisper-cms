use askama::Template;
use askama_web::WebTemplate; // new

#[derive(Template, WebTemplate)]
#[template(path = "page/install_welcome.html")]
struct InstallWelcome;

#[derive(Template, WebTemplate)]
#[template(path = "page/install_config.html")]
struct InstallConfig;

#[derive(Template, WebTemplate)]
#[template(path = "page/install_run.html")]
struct InstallRun;

#[derive(Template, WebTemplate)]
#[template(path = "page/install_done.html")]
struct InstallDone;

pub async fn get_welcome() -> impl axum::response::IntoResponse {
    InstallWelcome
}
pub async fn get_config() -> impl axum::response::IntoResponse {
    InstallConfig
}
pub async fn get_run() -> impl axum::response::IntoResponse {
    InstallRun
}
pub async fn get_done() -> impl axum::response::IntoResponse {
    InstallDone
}
