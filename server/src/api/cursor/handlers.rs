//! Implements Cursor HTTP endpoints outside the Agent Run stream.
use axum::{
    body::{to_bytes, Body, Bytes},
    extract::{DefaultBodyLimit, Extension, State},
    http::{header, HeaderMap, HeaderValue, Request, Response, StatusCode},
    routing::{get, post},
    Router,
};
use tower_http::decompression::RequestDecompressionLayer;

use crate::{
    api::cursor::{
        bidi,
        proxy::{self, CursorProxy},
        run_sse,
    },
    cursor::{
        protocol::{
            connect,
            proto::{agent::v1 as agent, aiserver::v1 as ai},
        },
        services::{
            account, analytics, commit_message, compatibility, entitlement::FreeEntitlementCache,
            knowledge, model_catalog, server_config, tab,
        },
        transport::{TransportParent, TransportRegistry},
    },
    Result,
};

pub fn router(
    registry: TransportRegistry,
    clients: crate::network::NetworkClients,
) -> Result<Router> {
    let proxy = CursorProxy::cursor(clients);
    let knowledge = knowledge::KnowledgeService::managed()?;
    Ok(router_with_proxy(registry, proxy, knowledge))
}

fn router_with_proxy(
    registry: TransportRegistry,
    proxy: CursorProxy,
    knowledge_service: knowledge::KnowledgeService,
) -> Router {
    let web_cache = registry.web_cache().router();
    let free_entitlements = FreeEntitlementCache::default();
    Router::new()
        .route("/__byok-api__/healthz", get(health))
        .route("/agent.v1.AgentService/RunSSE", post(run_sse_handler))
        .route("/aiserver.v1.BidiService/BidiAppend", post(bidi_handler))
        .route(
            "/aiserver.v1.AiService/AvailableDocs",
            post(compatibility::available_docs),
        )
        .route(
            "/aiserver.v1.DashboardService/GetEffectiveUserPlugins",
            post(compatibility::effective_user_plugins),
        )
        .route(
            "/aiserver.v1.DashboardService/GetUserPrivacyMode",
            post(compatibility::user_privacy_mode),
        )
        .route(
            "/agent.v1.AgentService/UpdateConversationMetadata",
            post(compatibility::update_conversation_metadata),
        )
        .route(
            "/aiserver.v1.AiService/GetServerConfig",
            post(server_config::get),
        )
        .route(
            "/aiserver.v1.ServerConfigService/GetServerConfig",
            post(server_config::get),
        )
        .route(
            "/aiserver.v1.AiService/AvailableModels",
            post(model_catalog::available_models),
        )
        .route(
            "/agent.v1.AgentService/GetUsableModels",
            post(model_catalog::usable_models),
        )
        .route(
            "/aiserver.v1.AiService/GetUsableModels",
            post(model_catalog::usable_models),
        )
        .route(
            "/aiserver.v1.AiService/WriteGitCommitMessage",
            post(commit_message::write_git_commit_message),
        )
        .route(
            "/aiserver.v1.NetworkService/IsConnected",
            post(is_connected),
        )
        .route(
            "/agent.v1.AgentService/GetDefaultModelForCli",
            post(model_catalog::default_model_for_cli),
        )
        .route(
            "/aiserver.v1.AiService/GetDefaultModelForCli",
            post(model_catalog::default_model_for_cli),
        )
        .route(
            "/aiserver.v1.AiService/GetDefaultModel",
            post(model_catalog::default_model),
        )
        .route(
            "/aiserver.v1.AiService/GetDefaultModelNudgeData",
            post(model_catalog::default_model_nudge),
        )
        .route(
            "/aiserver.v1.AuthService/GetEmail",
            post(account::get_email),
        )
        .route(
            "/aiserver.v1.AuthService/GetUserMeta",
            post(account::get_user_meta),
        )
        .route("/aiserver.v1.DashboardService/GetMe", post(account::get_me))
        .route(
            "/aiserver.v1.DashboardService/GetTeams",
            post(account::get_teams),
        )
        .route(
            "/aiserver.v1.DashboardService/GetUserProfile",
            post(account::get_user_profile),
        )
        .route(
            "/aiserver.v1.DashboardService/GetCurrentPeriodUsage",
            post(account::current_period_usage),
        )
        .route(
            "/aiserver.v1.DashboardService/GetUsageLimitStatusAndActiveGrants",
            post(account::usage_limit_status),
        )
        .route(
            "/aiserver.v1.AiService/KnowledgeBaseAdd",
            post(knowledge::add),
        )
        .route(
            "/aiserver.v1.AiService/KnowledgeBaseList",
            post(knowledge::list),
        )
        .route(
            "/aiserver.v1.AiService/KnowledgeBaseUpdate",
            post(knowledge::update),
        )
        .route(
            "/aiserver.v1.AiService/KnowledgeBaseRemove",
            post(knowledge::remove),
        )
        .route(
            analytics::BOOTSTRAP_STATSIG_PATH,
            post(analytics::bootstrap_statsig),
        )
        .route("/auth/full_stripe_profile", get(account::stripe_profile))
        .route("/auth/stripe_profile", get(account::stripe_profile))
        .merge(tab::router())
        .route_layer(DefaultBodyLimit::disable())
        .route_layer(RequestDecompressionLayer::new())
        .fallback(proxy::forward)
        .method_not_allowed_fallback(proxy::forward)
        .layer(Extension(proxy))
        .layer(Extension(knowledge_service))
        .layer(Extension(free_entitlements))
        .with_state(registry)
        .merge(web_cache)
}

async fn health() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// `NetworkService/IsConnected` probe. Cursor's always-local extension checks
/// connectivity roughly 10s after any slow request starts; a 404/error here is
/// treated as "network disconnected" and aborts in-flight work (e.g. commit
/// message generation) even while the model is still streaming. Always answer
/// connected with an empty `IsConnectedResponse` so local BYOK generation is
/// never cancelled by this probe.
async fn is_connected() -> Result<Response<Body>> {
    let payload = connect::encode_message(&ai::IsConnectedResponse {})?;
    let mut response = Response::new(Body::from(payload));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    Ok(response)
}

async fn run_sse_handler(
    State(registry): State<TransportRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (parts, body) = buffered(request).await?;
    let request: agent::BidiRequestId = connect::decode_unary(&body)?;
    let route = registry.wait_route(&request.request_id).await;
    let trace = registry.trace(&request.request_id);
    trace.resume();
    trace.request(
        "run_sse_request",
        body.clone(),
        serde_json::json!({"request_id": request.request_id}),
    );
    match route {
        crate::cursor::transport::TransportRoute::Local => {
            run_sse::stream(&registry, &request.request_id).await
        }
        crate::cursor::transport::TransportRoute::Upstream(generation) => {
            let response = proxy::forward(
                Extension(proxy),
                Request::from_parts(parts, Body::from(body)),
            )
            .await?;
            Ok(run_sse::upstream(
                registry,
                request.request_id,
                generation,
                response,
                Some(trace),
            )
            .await)
        }
    }
}

async fn bidi_handler(
    State(registry): State<TransportRegistry>,
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    let (parts, body) = buffered(request).await?;
    let request: ai::BidiAppendRequest = connect::decode_unary(&body)?;
    let decoded = bidi::decode(&request)?;
    let first_model = decoded.model_id().map(str::to_owned);
    let conversation_id = decoded.conversation_id().map(str::to_owned);
    let trace_metadata = decoded.trace_metadata();
    let trace = registry.trace(&decoded.request_id);
    let local = if let Some(model_id) = decoded.model_id() {
        // 本地模型命名空间只在本地有意义；未知或已删除的 ID 也不能泄漏给官方上游。
        if is_local_model_id(model_id) {
            tracing::info!(
                request_id = decoded.request_id,
                model_id,
                "routing Cursor Run to BYOK provider"
            );
            true
        } else {
            tracing::info!(
                request_id = decoded.request_id,
                model_id,
                "routing Cursor Run to Cursor upstream"
            );
            false
        }
    } else if registry.local(&decoded.request_id).await.is_some() {
        true
    } else if registry.upstream(&decoded.request_id).await {
        false
    } else {
        // A small control append can overtake a large Run upload. Wait for the
        // model-bearing request to choose the route without claiming it here.
        match wait_for_bidi_route(&registry, &decoded, std::time::Duration::from_secs(60)).await {
            Ok(local) => local,
            Err(error) => {
                trace.resume();
                trace.request(
                    "bidi_request",
                    body.clone(),
                    trace_outcome(
                        trace_metadata,
                        false,
                        "missing_transport",
                        Some(error.to_string()),
                    ),
                );
                return Err(error);
            }
        }
    };
    if first_model.is_some() {
        trace.begin(
            conversation_id.as_deref(),
            if local {
                "local_byok"
            } else {
                "cursor_official"
            },
            first_model.as_deref(),
        );
    } else {
        trace.resume();
    }
    if !local {
        if first_model.is_some() {
            registry.mark_upstream(&decoded.request_id).await;
        }
        trace.request(
            "bidi_request",
            body.clone(),
            trace_outcome(trace_metadata, true, "upstream", None),
        );
        return proxy::forward(
            Extension(proxy),
            Request::from_parts(parts, Body::from(body)),
        )
        .await;
    }
    let parent = match parent_headers(&parts.headers) {
        Ok(parent) => parent,
        Err(error) => {
            trace.request(
                "bidi_request",
                body,
                trace_outcome(
                    trace_metadata,
                    false,
                    "invalid_parent",
                    Some(error.to_string()),
                ),
            );
            return Err(error);
        }
    };
    match bidi::append(&registry, decoded, parent).await {
        Ok(_) => trace.request(
            "bidi_request",
            body,
            trace_outcome(trace_metadata, true, "local", None),
        ),
        Err(error) => {
            trace.request(
                "bidi_request",
                body,
                trace_outcome(
                    trace_metadata,
                    false,
                    "command_rejected",
                    Some(error.to_string()),
                ),
            );
            return Err(error);
        }
    }
    let mut response = Response::new(axum::body::Body::empty());
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/proto"),
    );
    Ok(response)
}

async fn wait_for_bidi_route(
    registry: &TransportRegistry,
    decoded: &bidi::DecodedAppend,
    timeout: std::time::Duration,
) -> Result<bool> {
    if matches!(
        decoded.message.message,
        None | Some(agent::agent_client_message::Message::RunRequest(_))
    ) {
        return Err(crate::Error::Protocol(
            "first BidiAppend message must select a model".into(),
        ));
    }
    let route = tokio::time::timeout(timeout, registry.wait_route(&decoded.request_id))
        .await
        .map_err(|_| {
            crate::Error::Protocol(
                "timed out waiting for initial Cursor Run model selection".into(),
            )
        })?;
    Ok(matches!(
        route,
        crate::cursor::transport::TransportRoute::Local
    ))
}

fn is_local_model_id(model_id: &str) -> bool {
    model_id.starts_with(crate::model::BUILTIN_MODEL_ID_PREFIX)
        || model_id.starts_with(crate::plugin::ADAPTER_ID_PREFIX)
}

fn trace_outcome(
    mut metadata: serde_json::Value,
    accepted: bool,
    route_outcome: &str,
    error: Option<String>,
) -> serde_json::Value {
    if let Some(metadata) = metadata.as_object_mut() {
        metadata.insert("accepted".into(), accepted.into());
        metadata.insert("route_outcome".into(), route_outcome.into());
        if let Some(error) = error {
            metadata.insert("error".into(), error.into());
        }
    }
    metadata
}

async fn buffered(request: Request<Body>) -> Result<(axum::http::request::Parts, Bytes)> {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|error| crate::Error::Protocol(format!("cannot read request body: {error}")))?;
    Ok((parts, body))
}

fn parent_headers(headers: &HeaderMap) -> Result<Option<TransportParent>> {
    let request_id = header_text(headers, "x-parent-request-id")?;
    let tool_call_id = header_text(headers, "x-parent-agent-tool-call-id")?;
    match (request_id, tool_call_id) {
        (None, None) => Ok(None),
        (Some(request_id), Some(tool_call_id)) => Ok(Some(TransportParent {
            request_id: request_id.into(),
            tool_call_id: tool_call_id.into(),
        })),
        _ => Err(crate::Error::Protocol(
            "Cursor subagent request must include both parent headers".into(),
        )),
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>> {
    headers
        .get(name)
        .map(|value| value.to_str())
        .transpose()
        .map_err(|error| crate::Error::Protocol(format!("invalid {name} header: {error}")))
}

#[cfg(test)]
mod tests {
    use super::is_local_model_id;

    #[test]
    fn stale_local_ids_never_route_to_cursor_upstream() {
        assert!(is_local_model_id("byok:missing-model"));
        assert!(is_local_model_id("plugin:missing/provider/model"));
        assert!(!is_local_model_id("claude-4.5-sonnet"));
    }

    use super::*;
    use crate::{
        cursor::prompting::{PromptAssets, PromptCompiler},
        model::ModelInvocation,
        provider::{Provider, ProviderStream},
        store::Store,
    };
    use prost::Message;
    use std::{sync::Arc, time::Duration};

    struct UnusedProvider;
    impl Provider for UnusedProvider {
        fn stream(
            &self,
            _: ModelInvocation,
            _: tokio_util::sync::CancellationToken,
        ) -> ProviderStream {
            panic!("routing a control message must not invoke a provider")
        }
    }

    async fn registry() -> TransportRegistry {
        TransportRegistry::new(
            Store::connect("sqlite::memory:").await.unwrap(),
            Arc::new(UnusedProvider),
            PromptCompiler::new(PromptAssets::embedded().unwrap()),
        )
    }

    fn heartbeat_body(id: &str, seqno: i64) -> Vec<u8> {
        let message = agent::AgentClientMessage {
            message: Some(agent::agent_client_message::Message::ClientHeartbeat(
                Default::default(),
            )),
        };
        ai::BidiAppendRequest {
            request_id: Some(ai::BidiRequestId {
                request_id: id.into(),
            }),
            append_seqno: seqno,
            data: hex::encode(message.encode_to_vec()),
            ..Default::default()
        }
        .encode_to_vec()
    }

    fn request(body: Vec<u8>) -> Request<Body> {
        Request::post("/aiserver.v1.BidiService/BidiAppend")
            .header(header::CONTENT_TYPE, "application/proto")
            .body(Body::from(body))
            .unwrap()
    }

    #[tokio::test]
    async fn early_controls_wait_for_local_route_without_claiming_it() {
        let registry = registry().await;
        let proxy = CursorProxy::cursor(crate::network::NetworkClients::new(
            registry.store().clone(),
        ));
        let first = bidi_handler(
            State(registry.clone()),
            Extension(proxy.clone()),
            request(heartbeat_body("early", 1)),
        );
        let second = bidi_handler(
            State(registry.clone()),
            Extension(proxy),
            request(heartbeat_body("early", 2)),
        );
        tokio::pin!(first, second);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut first)
                .await
                .is_err(),
            "control must await Run routing, not return missing-model error"
        );
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut second)
            .await
            .is_err());
        assert!(registry.local("early").await.is_none());
        assert!(!registry.upstream("early").await);
        registry.get_or_create("early").await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), &mut first)
                .await
                .unwrap()
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), &mut second)
                .await
                .unwrap()
                .unwrap()
                .status(),
            StatusCode::OK
        );
        registry.shutdown().await;
    }
    #[tokio::test]
    async fn early_control_waits_for_official_route_and_forwards_original_request() {
        let registry = registry().await;
        let body = heartbeat_body("official-early", 1);
        let expected = body.clone();
        let router = Router::new().route(
            "/aiserver.v1.BidiService/BidiAppend",
            post(move |request: Request<Body>| {
                let expected = expected.clone();
                async move {
                    assert_eq!(
                        request.headers()[header::AUTHORIZATION],
                        "Bearer test-token"
                    );
                    assert_eq!(
                        to_bytes(request.into_body(), 4096).await.unwrap().as_ref(),
                        expected.as_slice()
                    );
                    (StatusCode::ACCEPTED, "official-response")
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let proxy = CursorProxy::with_test_upstream(
            crate::network::NetworkClients::new(registry.store().clone()),
            format!("http://{address}"),
        );
        let mut request = request(body);
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-token"),
        );
        let pending = bidi_handler(State(registry.clone()), Extension(proxy), request);
        tokio::pin!(pending);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut pending)
                .await
                .is_err()
        );
        assert!(registry.local("official-early").await.is_none());
        registry.mark_upstream("official-early").await;
        let response = tokio::time::timeout(Duration::from_secs(2), &mut pending)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            to_bytes(response.into_body(), 4096).await.unwrap().as_ref(),
            b"official-response"
        );
        assert!(registry.local("official-early").await.is_none());
        task.abort();
        registry.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_control_timeout_leaves_no_route_and_invalid_first_messages_fail() {
        let registry = registry().await;
        let decoded = bidi::decode(
            &ai::BidiAppendRequest::decode(heartbeat_body("timeout", 1).as_slice()).unwrap(),
        )
        .unwrap();
        let error = wait_for_bidi_route(&registry, &decoded, Duration::from_millis(1))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(registry.local("timeout").await.is_none());
        assert!(!registry.upstream("timeout").await);
        for message in [
            None,
            Some(agent::agent_client_message::Message::RunRequest(
                Default::default(),
            )),
        ] {
            let decoded = bidi::DecodedAppend {
                request_id: "invalid".into(),
                seqno: 0,
                message: agent::AgentClientMessage { message },
            };
            let error = tokio::time::timeout(
                Duration::from_millis(20),
                wait_for_bidi_route(&registry, &decoded, Duration::from_secs(60)),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert!(error.to_string().contains("must select a model"));
        }
    }

    #[tokio::test]
    async fn existing_local_route_accepts_model_less_run_continuation() {
        let registry = registry().await;
        registry.get_or_create("continuation").await.unwrap();
        let message = agent::AgentClientMessage {
            message: Some(agent::agent_client_message::Message::RunRequest(
                Default::default(),
            )),
        };
        let body = ai::BidiAppendRequest {
            request_id: Some(ai::BidiRequestId {
                request_id: "continuation".into(),
            }),
            append_seqno: 1,
            data: hex::encode(message.encode_to_vec()),
            ..Default::default()
        }
        .encode_to_vec();
        let proxy = CursorProxy::cursor(crate::network::NetworkClients::new(
            registry.store().clone(),
        ));
        let response = bidi_handler(State(registry.clone()), Extension(proxy), request(body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        registry.shutdown().await;
    }
}
