//! Selects local handling or the configured official Cursor upstream.
use std::time::Instant;

use axum::{
    body::{to_bytes, Body, Bytes},
    extract::Extension,
    http::{header, Request, Response},
};

use crate::Result;

const CURSOR_UPSTREAM: &str = "https://api2.cursor.sh";
pub const UPSTREAM_URL_HEADER: &str = "x-server-upstream-url";

#[derive(Clone)]
pub struct CursorProxy {
    clients: crate::network::NetworkClients,
    upstream: String,
}

pub struct BufferedResponse {
    pub status: axum::http::StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Bytes,
}

impl BufferedResponse {
    pub fn into_response(self) -> Response<Body> {
        let body = self.body.clone();
        self.with_body(body)
    }

    pub fn with_body(mut self, body: Bytes) -> Response<Body> {
        self.headers.insert(
            header::CONTENT_LENGTH,
            body.len()
                .to_string()
                .parse()
                .expect("body length is always a valid header value"),
        );
        let mut response = Response::new(Body::from(body));
        *response.status_mut() = self.status;
        *response.headers_mut() = self.headers;
        response
    }
}

impl CursorProxy {
    pub fn cursor(clients: crate::network::NetworkClients) -> Self {
        Self {
            clients,
            upstream: CURSOR_UPSTREAM.into(),
        }
    }

    #[cfg(test)]
    pub(super) fn with_test_upstream(
        clients: crate::network::NetworkClients,
        upstream: String,
    ) -> Self {
        Self { clients, upstream }
    }

    async fn client(&self) -> Result<reqwest::Client> {
        self.clients.cursor_client().await
    }
}

pub async fn forward(
    Extension(proxy): Extension<CursorProxy>,
    request: Request<Body>,
) -> Result<Response<Body>> {
    forward_request(&proxy, request, None).await
}

pub(crate) async fn forward_to_service(
    proxy: &CursorProxy,
    request: Request<Body>,
    service_url: &str,
) -> Result<Response<Body>> {
    forward_request(proxy, request, Some(service_url)).await
}

async fn forward_request(
    proxy: &CursorProxy,
    request: Request<Body>,
    service_url: Option<&str>,
) -> Result<Response<Body>> {
    let started = Instant::now();
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str())
        .to_owned();
    let url = match service_url {
        Some(service_url) => format!("{}{}", service_url.trim_end_matches('/'), path),
        None => upstream_url(&parts.headers, &proxy.upstream, &path)?,
    };

    let mut headers = parts.headers;
    headers.remove(UPSTREAM_URL_HEADER);
    headers.remove(header::HOST);
    remove_hop_by_hop_headers(&mut headers);

    let client = proxy.client().await?;
    let upstream = client
        .request(parts.method.clone(), url)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await;

    let upstream = match upstream {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(
                method = %parts.method,
                path,
                elapsed_ms = started.elapsed().as_millis(),
                %error,
                "Cursor upstream request failed"
            );
            return Err(error.into());
        }
    };

    let status = upstream.status();
    let mut response_headers = upstream.headers().clone();
    remove_hop_by_hop_headers(&mut response_headers);
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;

    tracing::info!(
        method = %parts.method,
        path,
        %status,
        elapsed_ms = started.elapsed().as_millis(),
        "forwarded Cursor backend request"
    );
    Ok(response)
}

pub async fn forward_buffered(
    proxy: &CursorProxy,
    request: Request<Body>,
) -> Result<BufferedResponse> {
    let (parts, body) = request.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let url = upstream_url(&parts.headers, &proxy.upstream, path)?;
    let mut headers = parts.headers;
    headers.remove(UPSTREAM_URL_HEADER);
    headers.remove(header::HOST);
    remove_hop_by_hop_headers(&mut headers);
    headers.insert(
        "connect-accept-encoding",
        axum::http::HeaderValue::from_static("identity"),
    );
    headers.insert(
        header::ACCEPT_ENCODING,
        axum::http::HeaderValue::from_static("identity"),
    );
    let body = to_bytes(body, usize::MAX)
        .await
        .map_err(|error| crate::Error::Protocol(format!("cannot read request body: {error}")))?;
    let upstream = proxy
        .client()
        .await?
        .request(parts.method, url)
        .headers(headers)
        .body(body)
        .send()
        .await?;
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    remove_hop_by_hop_headers(&mut headers);
    let body = upstream.bytes().await?;
    Ok(BufferedResponse {
        status,
        headers,
        body,
    })
}

fn upstream_url(headers: &axum::http::HeaderMap, fallback: &str, path: &str) -> Result<String> {
    let Some(value) = headers.get(UPSTREAM_URL_HEADER) else {
        return Ok(format!("{fallback}{path}"));
    };
    let value = value
        .to_str()
        .map_err(|error| crate::Error::Protocol(format!("invalid upstream URL header: {error}")))?;
    let url = reqwest::Url::parse(value)
        .map_err(|error| crate::Error::Protocol(format!("invalid upstream URL: {error}")))?;
    let host = url.host_str().unwrap_or_default();
    if url.scheme() != "https" || !crate::local_app::proxy_host_allowed(host) {
        return Err(crate::Error::Protocol(
            "upstream URL must target a Cursor HTTPS host".into(),
        ));
    }
    Ok(url.into())
}

fn remove_hop_by_hop_headers(headers: &mut axum::http::HeaderMap) {
    let connection_headers = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for name in connection_headers {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        headers.remove(name);
    }
    headers.remove("keep-alive");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, routing::post, Router};

    #[tokio::test]
    async fn empty_local_catalog_forwards_all_default_rpcs_unchanged() {
        use crate::{
            cursor::{
                prompting::{PromptAssets, PromptCompiler},
                services::model_catalog,
                transport::TransportRegistry,
            },
            model::ModelInvocation,
            provider::{Provider, ProviderStream},
        };
        struct UnusedProvider;
        impl Provider for UnusedProvider {
            fn stream(
                &self,
                _: ModelInvocation,
                _: tokio_util::sync::CancellationToken,
            ) -> ProviderStream {
                panic!("official default RPC must not invoke a local model")
            }
        }
        let store = crate::store::Store::connect("sqlite::memory:")
            .await
            .unwrap();
        let registry = TransportRegistry::new(
            store.clone(),
            std::sync::Arc::new(UnusedProvider),
            PromptCompiler::new(PromptAssets::embedded().unwrap()),
        );
        for status in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
            let router = Router::new().fallback(post(move |request: Request<Body>| async move {
                assert_eq!(
                    request.headers()[header::AUTHORIZATION],
                    "Bearer official-test-token"
                );
                assert_eq!(request.uri().query(), Some("test=1"));
                assert_eq!(
                    to_bytes(request.into_body(), 1024).await.unwrap().as_ref(),
                    b"original-request"
                );
                (
                    status,
                    [(header::CONTENT_TYPE, "application/proto")],
                    "official-default-response",
                )
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            for name in [
                "GetDefaultModelForCli",
                "GetDefaultModel",
                "GetDefaultModelNudge",
            ] {
                let proxy = CursorProxy {
                    clients: crate::network::NetworkClients::new(store.clone()),
                    upstream: format!("http://{address}"),
                };
                let request = Request::post(format!("/aiserver.v1.AiService/{name}?test=1"))
                    .header(header::AUTHORIZATION, "Bearer official-test-token")
                    .body(Body::from("original-request"))
                    .unwrap();
                let response = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    match name {
                        "GetDefaultModelForCli" => {
                            model_catalog::default_model_for_cli(
                                axum::extract::State(registry.clone()),
                                Extension(proxy),
                                request,
                            )
                            .await
                        }
                        "GetDefaultModel" => {
                            model_catalog::default_model(
                                axum::extract::State(registry.clone()),
                                Extension(proxy),
                                request,
                            )
                            .await
                        }
                        _ => {
                            model_catalog::default_model_nudge(
                                axum::extract::State(registry.clone()),
                                Extension(proxy),
                                request,
                            )
                            .await
                        }
                    }
                })
                .await
                .unwrap()
                .unwrap();
                assert_eq!(response.status(), status);
                assert_eq!(
                    response.headers()[header::CONTENT_TYPE],
                    "application/proto"
                );
                assert_eq!(
                    to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
                    b"official-default-response"
                );
            }
            task.abort();
        }
    }

    #[tokio::test]
    async fn official_account_identity_and_auth_errors_are_forwarded_unchanged() {
        let store = crate::store::Store::connect("sqlite::memory:")
            .await
            .unwrap();
        for status in [StatusCode::OK, StatusCode::UNAUTHORIZED] {
            let router = Router::new().route(
                "/aiserver.v1.DashboardService/GetMe",
                post(move |headers: axum::http::HeaderMap| async move {
                    assert_eq!(headers[header::AUTHORIZATION], "Bearer official-test-token");
                    (
                        status,
                        [(header::CONTENT_TYPE, "application/proto")],
                        b"\x0a\x0bofficial-id".as_slice(),
                    )
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
            let upstream = CursorProxy {
                clients: crate::network::NetworkClients::new(store.clone()),
                upstream: format!("http://{address}"),
            };
            let request = Request::post("/aiserver.v1.DashboardService/GetMe")
                .header(header::AUTHORIZATION, "Bearer official-test-token")
                .body(Body::empty())
                .unwrap();
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                let response =
                    crate::cursor::services::account::get_me(Extension(upstream), request)
                        .await
                        .unwrap();
                let status = response.status();
                let content_type = response.headers()[header::CONTENT_TYPE].clone();
                let body = to_bytes(response.into_body(), 1024).await.unwrap();
                (status, content_type, body)
            })
            .await;
            task.abort();
            let (actual_status, content_type, body) = result.unwrap();
            assert_eq!(actual_status, status);
            assert_eq!(content_type, "application/proto");
            assert_eq!(body.as_ref(), b"\x0a\x0bofficial-id");
        }
    }
}
