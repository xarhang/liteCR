//! HTTP health adapter. Domain and application code remain transport independent.

#![forbid(unsafe_code)]

pub mod access;
pub mod appearance;
pub mod country_profiles;
pub mod integrations;
pub mod localization;
pub mod matching;
pub mod migration;
pub mod registry;
pub mod review;

use std::sync::Arc;

use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use litecr_application::ReadinessProbe;
use serde::Serialize;

#[derive(Clone)]
struct AppState {
    readiness: Arc<dyn ReadinessProbe>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

pub fn router(readiness: Arc<dyn ReadinessProbe>) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .with_state(AppState { readiness })
}

async fn live() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

async fn ready(State(state): State<AppState>) -> (StatusCode, Json<HealthResponse>) {
    match state.readiness.snapshot().await {
        Ok(snapshot) if snapshot.is_ready() => {
            (StatusCode::OK, Json(HealthResponse { status: "ready" }))
        }
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(HealthResponse {
                status: "not_ready",
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use litecr_application::{ReadinessError, ReadinessSnapshot};
    use litecr_domain::{SCHEMA_VERSION, SchemaVersion};
    use tower::ServiceExt;

    struct StaticProbe(ReadinessSnapshot);

    #[async_trait]
    impl ReadinessProbe for StaticProbe {
        async fn snapshot(&self) -> Result<ReadinessSnapshot, ReadinessError> {
            Ok(self.0)
        }
    }

    #[tokio::test]
    async fn health_routes_are_non_sensitive_and_readiness_tracks_schema() {
        let ready_router = router(Arc::new(StaticProbe(ReadinessSnapshot {
            database_reachable: true,
            schema_version: Some(SchemaVersion::new(SCHEMA_VERSION)),
        })));
        let ready_response = ready_router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health/ready")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ready_response.status(), StatusCode::OK);

        let live_response = ready_router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health/live")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(live_response.status(), StatusCode::OK);

        let incompatible_router = router(Arc::new(StaticProbe(ReadinessSnapshot {
            database_reachable: true,
            schema_version: Some(SchemaVersion::new(SCHEMA_VERSION + 1)),
        })));
        let response = incompatible_router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/health/ready")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
