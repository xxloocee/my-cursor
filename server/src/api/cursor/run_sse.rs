//! Subscribes Cursor RunSSE clients to replayable Transport output.
use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode},
};
use bytes::Bytes;
use std::convert::Infallible;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::{
    cursor::{
        protocol::connect::{self, END_STREAM_FLAG},
        services::observability::CursorTraceRecorder,
        transport::{TransportHandle, TransportRegistry},
    },
    Result,
};

pub async fn stream(registry: &TransportRegistry, request_id: &str) -> Result<Response<Body>> {
    let handle = registry.get_or_create(request_id).await?;
    let receiver = handle.subscribe();
    let trace = handle.trace().cloned();
    if let Some(trace) = &trace {
        trace.response_started(StatusCode::OK.as_u16());
    }
    let body_stream = local_body_stream(receiver, handle, trace);
    let mut response = Response::new(Body::from_stream(body_stream));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("connect-protocol-version", HeaderValue::from_static("1"));
    Ok(response)
}

fn local_body_stream(
    mut receiver: mpsc::UnboundedReceiver<Bytes>,
    handle: TransportHandle,
    trace: Option<CursorTraceRecorder>,
) -> impl tokio_stream::Stream<Item = std::result::Result<Bytes, Infallible>> {
    let mut guard = LocalRunGuard::new(handle);
    async_stream::stream! {
        let mut trace = TraceStreamSink::new(trace, "byok_server");
        while let Some(chunk) = receiver.recv().await {
            let terminal = is_end_stream_frame(&chunk);
            trace.chunk(&chunk);
            if terminal {
                guard.complete();
                trace.finish(end_stream_error(&chunk));
            }
            yield Ok::<Bytes, Infallible>(chunk);
            if terminal {
                return;
            }
        }
        guard.complete();
        trace.finish(None);
    }
}

fn is_end_stream_frame(frame: &Bytes) -> bool {
    frame
        .first()
        .is_some_and(|flags| flags & END_STREAM_FLAG != 0)
}

fn end_stream_error(frame: &Bytes) -> Option<String> {
    connect::decode_frames(frame)
        .ok()?
        .into_iter()
        .find_map(|(flags, payload)| {
            if flags & END_STREAM_FLAG == 0 {
                return None;
            }
            let value = serde_json::from_slice::<serde_json::Value>(&payload).ok()?;
            let error = value.get("error")?;
            let code = error.get("code").and_then(serde_json::Value::as_str);
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .filter(|message| !message.is_empty());
            Some(match (code, message) {
                (Some(code), Some(message)) => format!("{code}: {message}"),
                (Some(code), None) => code.to_string(),
                (None, Some(message)) => message.to_string(),
                (None, None) => error.to_string(),
            })
        })
}

struct LocalRunGuard {
    handle: TransportHandle,
    completed: bool,
    acquired: bool,
}

impl LocalRunGuard {
    fn new(handle: TransportHandle) -> Self {
        let acquired = handle.acquire_http();
        Self {
            handle,
            acquired,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for LocalRunGuard {
    fn drop(&mut self) {
        if self.acquired {
            if let Some(epoch) = self.handle.release_http() {
                if !self.completed {
                    let handle = self.handle.clone();
                    tokio::spawn(async move {
                        let _ = handle.command(crate::cursor::conversation::TransportCommand::DisconnectIfUnsubscribed(epoch)).await;
                    });
                }
            }
        }
    }
}

pub async fn upstream(
    guard: UpstreamRunGuard,
    response: Response<Body>,
    trace: Option<CursorTraceRecorder>,
) -> Response<Body> {
    let (parts, body) = response.into_parts();
    if let Some(trace) = &trace {
        trace.response_started(parts.status.as_u16());
    }
    let success = parts.status.is_success();
    let stream = async_stream::stream! {
        let mut guard = guard;
        let mut terminal = ConnectCompletion::default();
        let mut trace = TraceStreamSink::new(trace, "cursor_official");
        let mut body = body.into_data_stream();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(chunk) => {
                    if success && terminal.observe(&chunk) { guard.complete = true; }
                    trace.chunk(&chunk);
                    yield Ok::<Bytes, axum::Error>(chunk);
                }
                Err(error) => {
                    trace.finish(Some(error.to_string()));
                    yield Err(error);
                    return;
                }
            }
        }
        trace.finish(None);
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

enum TraceStreamEvent {
    Chunk(Bytes),
    Finish(Option<String>),
}

struct TraceStreamSink {
    sender: Option<mpsc::UnboundedSender<TraceStreamEvent>>,
}

impl TraceStreamSink {
    fn new(trace: Option<CursorTraceRecorder>, source: &'static str) -> Self {
        let Some(trace) = trace else {
            return Self { sender: None };
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                match event {
                    TraceStreamEvent::Chunk(chunk) => {
                        trace.response_chunk(source, chunk);
                    }
                    TraceStreamEvent::Finish(error) => {
                        trace.finish(error.as_deref());
                        return;
                    }
                }
            }
            trace.finish(None);
        });
        Self {
            sender: Some(sender),
        }
    }

    fn chunk(&self, chunk: &Bytes) {
        if let Some(sender) = &self.sender {
            let _ = sender.send(TraceStreamEvent::Chunk(chunk.clone()));
        }
    }

    fn finish(&mut self, error: Option<String>) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(TraceStreamEvent::Finish(error));
        }
    }
}

impl Drop for TraceStreamSink {
    fn drop(&mut self) {
        if self.sender.is_some() {
            self.finish(Some(
                "response stream dropped before completion".to_string(),
            ));
        }
    }
}

pub(crate) struct UpstreamRunGuard {
    registry: TransportRegistry,
    request_id: String,
    generation: u64,
    complete: bool,
}

impl UpstreamRunGuard {
    pub(crate) async fn new(
        registry: TransportRegistry,
        request_id: String,
        generation: u64,
    ) -> Result<Self> {
        if !registry.begin_upstream(&request_id, generation).await {
            return Err(crate::Error::Protocol(
                "Cursor upstream route expired".into(),
            ));
        }
        Ok(Self {
            registry,
            request_id,
            generation,
            complete: false,
        })
    }
}

impl Drop for UpstreamRunGuard {
    fn drop(&mut self) {
        self.registry
            .end_upstream(self.request_id.clone(), self.generation, self.complete);
    }
}

// Observe complete Connect envelopes without buffering their payloads.
#[derive(Default)]
struct ConnectCompletion {
    header: [u8; 5],
    header_len: usize,
    remaining: u32,
    terminal: bool,
}
impl ConnectCompletion {
    fn observe(&mut self, mut chunk: &[u8]) -> bool {
        while !chunk.is_empty() {
            if self.remaining != 0 {
                let skip = chunk.len().min(self.remaining as usize);
                self.remaining -= skip as u32;
                chunk = &chunk[skip..];
                if self.remaining == 0 && self.terminal {
                    return true;
                }
            } else {
                let take = chunk.len().min(5 - self.header_len);
                self.header[self.header_len..self.header_len + take]
                    .copy_from_slice(&chunk[..take]);
                self.header_len += take;
                chunk = &chunk[take..];
                if self.header_len == 5 {
                    self.remaining = u32::from_be_bytes(self.header[1..].try_into().unwrap());
                    self.terminal = self.header[0] & END_STREAM_FLAG != 0;
                    self.header_len = 0;
                    if self.remaining == 0 && self.terminal {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cursor::{
            prompting::{PromptAssets, PromptCompiler},
            services::observability::CursorTraceService,
            transport::{OutputHub, TransportRoute},
        },
        model::ModelInvocation,
        provider::{Provider, ProviderStream},
        store::Store,
    };
    use std::sync::Arc;
    struct NoProvider;
    impl Provider for NoProvider {
        fn stream(
            &self,
            _: ModelInvocation,
            _: tokio_util::sync::CancellationToken,
        ) -> ProviderStream {
            panic!("unexpected provider request")
        }
    }
    async fn registry() -> TransportRegistry {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let assets = PromptAssets::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("prompt/cursor"),
        )
        .unwrap();
        TransportRegistry::new(store, Arc::new(NoProvider), PromptCompiler::new(assets))
    }
    async fn handle() -> (
        TransportHandle,
        mpsc::Receiver<crate::cursor::conversation::TransportCommand>,
    ) {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let (commands, receiver) = mpsc::channel(16);
        (
            TransportHandle::new(
                "http-test".into(),
                commands,
                Arc::new(OutputHub::default()),
                CursorTraceService::new(store).recorder("http-test"),
            ),
            receiver,
        )
    }
    #[tokio::test]
    async fn local_unpolled_bodies_hold_subscriptions_and_only_last_drop_disconnects() {
        let (handle, mut commands) = handle().await;
        let old = Body::from_stream(local_body_stream(handle.subscribe(), handle.clone(), None));
        let new = Body::from_stream(local_body_stream(handle.subscribe(), handle.clone(), None));
        drop(old);
        assert!(commands.try_recv().is_err());
        assert!(handle.emit_frame(Bytes::from_static(b"still active")));
        drop(new);
        let command = tokio::time::timeout(std::time::Duration::from_secs(1), commands.recv())
            .await
            .unwrap()
            .unwrap();
        let crate::cursor::conversation::TransportCommand::DisconnectIfUnsubscribed(epoch) =
            command
        else {
            panic!("wrong command")
        };
        // The actor has not consumed the queued disconnect; a reconnect invalidates it.
        let reconnect =
            Body::from_stream(local_body_stream(handle.subscribe(), handle.clone(), None));
        assert!(!handle.disconnect_if_unsubscribed(epoch));
        drop(reconnect);
        let Some(crate::cursor::conversation::TransportCommand::DisconnectIfUnsubscribed(epoch)) =
            commands.recv().await
        else {
            panic!("missing last disconnect")
        };
        assert!(handle.disconnect_if_unsubscribed(epoch));
        assert!(!handle.acquire_http());
    }
    #[tokio::test]
    async fn local_terminal_body_drop_does_not_cancel() {
        let (handle, mut commands) = handle().await;
        let body = Body::from_stream(local_body_stream(handle.subscribe(), handle.clone(), None));
        handle.emit_frame(Bytes::from_static(&[2, 0, 0, 0, 2, b'{', b'}']));
        let data = axum::body::to_bytes(body, 100).await.unwrap();
        assert_eq!(data.as_ref(), &[2, 0, 0, 0, 2, b'{', b'}']);
        assert!(commands.try_recv().is_err());
    }
    #[test]
    fn connect_completion_requires_full_envelope_across_every_chunk_boundary() {
        let data = [
            0, 0, 0, 0, 7, 2, 0, 0, 0, 2, b'{', b'}', 2, 0, 0, 0, 2, b'{', b'}',
        ];
        // A fake END_STREAM header inside an ordinary payload must be skipped.
        for split in 1..data.len() {
            let mut observer = ConnectCompletion::default();
            assert!(!observer.observe(&data[..split]));
            assert!(observer.observe(&data[split..]));
        }
        let mut observer = ConnectCompletion::default();
        for (index, byte) in data.iter().enumerate() {
            assert_eq!(observer.observe(&[*byte]), index == data.len() - 1);
        }
    }
    #[tokio::test]
    async fn official_abnormal_drop_keeps_route_and_full_terminal_removes_it() {
        let registry = registry().await;
        registry.mark_upstream("official").await;
        let TransportRoute::Upstream(generation) = registry.wait_route("official").await else {
            panic!()
        };
        let guard = UpstreamRunGuard::new(registry.clone(), "official".into(), generation)
            .await
            .unwrap();
        // Even a Body that was never polled releases its lease on drop.
        let body = upstream(guard, Response::new(Body::empty()), None).await;
        drop(body);
        tokio::task::yield_now().await;
        assert!(registry.upstream("official").await);
        let guard = UpstreamRunGuard::new(registry.clone(), "official".into(), generation)
            .await
            .unwrap();
        let chunks = vec![
            Bytes::from_static(&[2, 0]),
            Bytes::from_static(&[0, 0, 2, b'{']),
            Bytes::from_static(b"}"),
        ];
        let response = upstream(
            guard,
            Response::new(Body::from_stream(tokio_stream::iter(
                chunks.into_iter().map(Ok::<_, Infallible>),
            ))),
            None,
        )
        .await;
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 100)
                .await
                .unwrap()
                .len(),
            7
        );
        tokio::task::yield_now().await;
        assert!(!registry.upstream("official").await);
    }
    #[tokio::test]
    async fn official_eof_error_and_non_success_keep_reconnect_route() {
        let registry = registry().await;
        for kind in 0..3 {
            registry.mark_upstream("abnormal").await;
            let TransportRoute::Upstream(generation) = registry.wait_route("abnormal").await else {
                panic!()
            };
            let guard = UpstreamRunGuard::new(registry.clone(), "abnormal".into(), generation)
                .await
                .unwrap();
            let chunks: Vec<std::result::Result<Bytes, std::io::Error>> = match kind {
                0 => vec![Ok(Bytes::from_static(&[2, 0, 0, 0, 2, b'{']))], // incomplete terminal payload
                1 => vec![Err(std::io::Error::other("upstream failed"))],
                _ => vec![Ok(Bytes::from_static(&[2, 0, 0, 0, 2, b'{', b'}']))],
            };
            let mut response = Response::new(Body::from_stream(tokio_stream::iter(chunks)));
            if kind == 2 {
                *response.status_mut() = StatusCode::BAD_GATEWAY;
            }
            let response = upstream(guard, response, None).await;
            let result = axum::body::to_bytes(response.into_body(), 100).await;
            assert_eq!(result.is_err(), kind == 1);
            tokio::task::yield_now().await;
            assert_eq!(
                registry.wait_route("abnormal").await,
                TransportRoute::Upstream(generation)
            );
            // The same generation can reconnect after each abnormal outcome.
            let reconnect = UpstreamRunGuard::new(registry.clone(), "abnormal".into(), generation)
                .await
                .unwrap();
            drop(reconnect);
        }
    }
}
