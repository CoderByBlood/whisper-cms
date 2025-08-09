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
            let mut slot: RwLockWriteGuard<'_, Option<_>> = app.plan.write().unwrap();
            *slot = Some(plan);
            Redirect::to("/install/run").into_response()
        }
        Err(errs) => {
            let body = format!(
                "Configuration errors:\n{}",
                errs.into_iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n")
            );
            (axum::http::StatusCode::BAD_REQUEST, body).into_response()
        }
    }
}

pub async fn post_run(
    State(app): State<AppState>,
) -> impl IntoResponse {
    // capture plan
    let plan = {
        let plan_lock = app.plan.read().unwrap();
        match &*plan_lock {
            Some(p) => p.clone(),
            None => return (axum::http::StatusCode::BAD_REQUEST, "no plan set").into_response(),
        }
    };

    // create a fresh broadcast channel each run
    let (tx, _rx) = tokio::sync::broadcast::channel(64);
    {
        let mut cell = app.progress.write().unwrap();
        *cell = Some(tx.clone());
    }

    // Spawn the coordinator; it will emit progress into `tx`
    tokio::spawn(async move {
        use crate::install::progress::Msg::*;
        // Start
        let _ = tx.send(Begin("GenerateSecrets"));

        // Orchestrate steps
        if let Err(e) = crate::install::steps::run_all(plan.clone(), tx.clone()).await {
            let _ = tx.send(Fail("Install", format!("{e}")));
            let _ = tx.send(Done);
            return;
        }

        let _ = tx.send(Success("Install"));
        let _ = tx.send(Done);
    });

    // 204 No Content; page JS will already be listening on /install/progress
    axum::http::StatusCode::NO_CONTENT.into_response()
}