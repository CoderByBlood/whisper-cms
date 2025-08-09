use axum::{extract::{Form, State}, response::{IntoResponse, Redirect}};
use crate::state::AppState;
use super::plan::InstallForm;
use std::sync::RwLockWriteGuard;

pub async fn post_config(
    State(app): State<AppState>,
    Form(form): Form<InstallForm>,
) -> impl IntoResponse {
    match form.validate_into_plan() {
        Ok(plan) => {
            // store the plan in memory (single-node); coordinator will read this next
            let mut slot: RwLockWriteGuard<'_, Option<_>> = app.plan.write().unwrap();
            *slot = Some(plan);
            Redirect::to("/install/run").into_response()
        }
        Err(errs) => {
            // minimal error display for now; we can render a template later
            let body = format!(
                "Configuration errors:\n{}",
                errs.into_iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n")
            );
            (axum::http::StatusCode::BAD_REQUEST, body).into_response()
        }
    }
}