//! Exposes the local control API.
mod calls;
mod harness;
mod models;
mod overview;
mod plugins;
mod service;
mod settings;

use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{header, Request, Response, StatusCode},
    middleware::{self, Next},
    routing::{any, get, post, put},
    Router,
};
use tower_http::services::ServeDir;
use url::Url;

pub use service::{
    CallDetail, CallSummary, ControlService, DiscoveredModels, LegacyModelImportPreview,
    LegacyModelImportResult, ModelConnectivityResult, ModelDiscoveryInput, ObservabilitySettings,
};

pub fn web_router(service: ControlService, assets: impl AsRef<std::path::Path>) -> Router {
    Router::new()
        .nest_service(
            "/__byok-api__",
            ServeDir::new(assets).append_index_html_on_directories(true),
        )
        .merge(api_router(service))
}

pub fn proxy_web_router(service: ControlService, target: Url) -> Router {
    frontend_proxy_router(target).merge(api_router(service))
}

fn frontend_proxy_router(target: Url) -> Router {
    let state = FrontendProxy {
        client: reqwest::Client::new(),
        target: target.as_str().trim_end_matches('/').to_string(),
    };
    Router::new()
        .route("/__byok-api__/", any(proxy_frontend))
        .route("/__byok-api__/{*path}", any(proxy_frontend))
        .with_state(state)
}

#[derive(Clone)]
struct FrontendProxy {
    client: reqwest::Client,
    target: String,
}

async fn proxy_frontend(
    State(proxy): State<FrontendProxy>,
    request: Request<Body>,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/__byok-api__/");
    let mut upstream = proxy
        .client
        .request(parts.method, format!("{}{path}", proxy.target));
    for (name, value) in &parts.headers {
        if name != header::HOST && name != header::CONNECTION {
            upstream = upstream.header(name, value);
        }
    }
    let body = match to_bytes(body, 64 * 1024 * 1024).await {
        Ok(body) => body,
        Err(error) => return proxy_error(error),
    };
    let upstream = match upstream.body(body).send().await {
        Ok(response) => response,
        Err(error) => return proxy_error(error),
    };
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let body = match upstream.bytes().await {
        Ok(body) => body,
        Err(error) => return proxy_error(error),
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    for (name, value) in &headers {
        if name != header::CONNECTION
            && name != header::TRANSFER_ENCODING
            && name != header::CONTENT_LENGTH
        {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

fn proxy_error(error: impl std::fmt::Display) -> Response<Body> {
    tracing::warn!(%error, "frontend development proxy failed");
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Body::from("frontend development server is unavailable"))
        .expect("static proxy error response")
}

pub fn api_router(service: ControlService) -> Router {
    local_control_router(
        Router::new()
        .route(
            "/__byok-api__/api/models",
            get(models::list)
                .post(models::create)
                .put(models::update_many),
        )
        .route("/__byok-api__/api/models/discover", post(models::discover))
        .route(
            "/__byok-api__/api/models/import-v0049",
            get(models::preview_v0049).post(models::import_v0049),
        )
        .route("/__byok-api__/api/models/order", put(models::reorder))
        .route("/__byok-api__/api/overview", get(overview::get))
        .route(
            "/__byok-api__/api/models/{model_hash}",
            put(models::update).delete(models::remove),
        )
        .route(
            "/__byok-api__/api/models/{model_hash}/test/{test_id}",
            post(models::test).delete(models::cancel),
        )
        .route("/__byok-api__/api/llm-calls", get(calls::list))
        .route("/__byok-api__/api/llm-calls/{call_id}", get(calls::detail))
        .route("/__byok-api__/api/plugins", get(plugins::list))
        .route(
            "/__byok-api__/api/plugins/runtime",
            get(plugins::runtime_status)
                .post(plugins::initialize_runtime)
                .delete(plugins::cancel_runtime_initialization),
        )
        .route(
            "/__byok-api__/api/plugins/oauth/{session_id}/poll",
            post(plugins::oauth_poll),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}",
            axum::routing::delete(plugins::remove),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/resources/{resource_type}/add/{method_id}/begin",
            post(plugins::oauth_begin),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/resources/{resource_type}/import",
            post(plugins::import),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/resources/{resource_type}/export",
            get(plugins::export_resources),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/resources/{resource_type}/{resource_id}",
            axum::routing::delete(plugins::delete_resource),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/resources/{resource_type}/{resource_id}/actions/{action_id}",
            post(plugins::action),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/resources/{resource_type}/{resource_id}/refresh",
            post(plugins::refresh_resource),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/providers/{provider_id}/models/sync",
            post(plugins::sync_models),
        )
        .route(
            "/__byok-api__/api/plugins/{plugin_id}/providers/{provider_id}/models/enabled",
            put(plugins::set_model_enabled),
        )
        .route(
            "/__byok-api__/api/settings/observability",
            get(settings::get).put(settings::update),
        )
        .route(
            "/__byok-api__/api/settings/ports",
            get(settings::get_ports).put(settings::update_ports),
        )
        .route(
            "/__byok-api__/api/settings/storage/statistics",
            get(settings::get_storage).delete(settings::clear_storage),
        )
        .route(
            "/__byok-api__/api/settings/proxy",
            get(settings::get_proxy).put(settings::update_proxy),
        )
        .route(
            "/__byok-api__/api/settings/tab",
            get(settings::get_tab).put(settings::update_tab),
        )
        .route(
            "/__byok-api__/api/settings/desktop",
            get(settings::get_desktop).put(settings::update_desktop),
        )
        .route(
            "/__byok-api__/api/settings/commit",
            get(settings::get_commit).put(settings::update_commit),
        )
        .route(
            "/__byok-api__/api/harness/cursor/status",
            get(harness::status),
        )
        .route(
            "/__byok-api__/api/harness/cursor/ca/initialize",
            post(harness::initialize_ca),
        )
        .route(
            "/__byok-api__/api/harness/cursor/enabled",
            put(harness::set_enabled),
        )
        .with_state(service),
    )
}

/// Applies the localhost-only boundary shared by every management API router.
pub fn local_control_router(router: Router) -> Router {
    router.layer(middleware::from_fn(require_local_control_host))
}

async fn require_local_control_host(request: Request<Body>, next: Next) -> Response<Body> {
    let authority = match request.headers().get(header::HOST) {
        Some(host) => host.to_str().ok(),
        None => request
            .uri()
            .authority()
            .map(|authority| authority.as_str()),
    };
    if authority.is_some_and(is_local_control_authority) {
        return next.run(request).await;
    }

    Response::builder()
        .status(StatusCode::FORBIDDEN)
        .body(Body::from("control API is only available on localhost"))
        .expect("static forbidden response")
}

fn is_local_control_authority(value: &str) -> bool {
    let Ok(authority) = value.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    let host = authority.host().trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "[::1]" | "::1")
}

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{header, Request, StatusCode},
        routing::get,
        Router,
    };
    use tower::ServiceExt;

    use super::{is_local_control_authority, local_control_router};

    #[test]
    fn control_api_accepts_only_loopback_authorities() {
        for authority in [
            "localhost",
            "LOCALHOST:3000",
            "localhost.:3000",
            "127.0.0.1",
            "127.0.0.1:3000",
            "[::1]",
            "[::1]:3000",
        ] {
            assert!(is_local_control_authority(authority), "{authority}");
        }

        for authority in [
            "localhost.evil",
            "127.0.0.2:3000",
            "192.168.1.10:3000",
            "example.com:3000",
            "",
            "not a host",
        ] {
            assert!(!is_local_control_authority(authority), "{authority}");
        }
    }

    #[tokio::test]
    async fn control_api_host_guard_rejects_non_loopback_requests() {
        let router = local_control_router(
            Router::new().route("/", get(|| async { StatusCode::NO_CONTENT })),
        );

        for (host, expected) in [
            (Some("127.0.0.1:3000"), StatusCode::NO_CONTENT),
            (Some("localhost.evil:3000"), StatusCode::FORBIDDEN),
            (Some("192.168.1.10:3000"), StatusCode::FORBIDDEN),
            (None, StatusCode::FORBIDDEN),
        ] {
            let mut request = Request::builder().uri("/");
            if let Some(host) = host {
                request = request.header(header::HOST, host);
            }
            let response = router
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), expected, "host: {host:?}");
        }
    }
}
