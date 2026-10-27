//! Hydrates request context blobs supplied by Cursor.
use std::{sync::Arc, time::Duration};

use crate::cursor::tools::runtime::CursorToolRuntime;
use parking_lot::Mutex;
use prost::Message;
use tokio::sync::oneshot;

use crate::{
    cursor::{protocol::proto::agent::v1 as pb, transport::TransportHandle},
    store::{BlobId, Store},
    Error, Result,
};

type ContextSender = oneshot::Sender<Result<pb::RequestContext>>;

#[derive(Clone)]
pub(crate) struct RequestContextSynchronizer {
    handle: TransportHandle,
    store: Store,
    pending: Arc<Mutex<Option<(u32, ContextSender)>>>,
    ids: CursorToolRuntime,
}

impl RequestContextSynchronizer {
    pub(crate) fn new(handle: TransportHandle, store: Store, ids: CursorToolRuntime) -> Self {
        Self {
            handle,
            store,
            ids,
            pending: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) async fn refresh_if_missing(
        &self,
        references: &pb::RequestContextPartReferences,
        conversation_id: &str,
    ) -> Result<Option<pb::RequestContext>> {
        if !self.has_missing_part(references).await? {
            return Ok(None);
        }
        let context = self.load(conversation_id).await?;
        self.cache_parts(&context).await?;
        Ok(Some(context))
    }

    pub(crate) async fn get(&self, id: &BlobId) -> Result<Option<Vec<u8>>> {
        self.store.get_blob(id).await
    }

    pub(crate) async fn load(&self, conversation_id: &str) -> Result<pb::RequestContext> {
        let id = self.ids.allocate_exec_id()?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock();
            if pending.is_some() {
                return Err(Error::Protocol(
                    "Cursor request context is already being loaded".into(),
                ));
            }
            *pending = Some((id, sender));
        }
        let _pending = PendingContext {
            pending: self.pending.clone(),
            id,
        };

        tracing::info!(
            request_id = self.handle.request_id(),
            conversation_id,
            "requesting uncached Cursor context"
        );

        if let Err(error) = self.handle.emit(&pb::AgentServerMessage {
            ttft_breakdown: None,
            message: Some(pb::agent_server_message::Message::ExecServerMessage(
                pb::ExecServerMessage {
                    id,
                    message: Some(pb::exec_server_message::Message::RequestContextArgs(
                        pb::RequestContextArgs {
                            notes_session_id: Some(conversation_id.into()),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                },
            )),
        }) {
            return Err(error);
        }

        let cancellation = self.handle.disconnect_token();
        let result = tokio::select! {
            result = receiver => result.map_err(|_| Error::Protocol("request context response channel closed".into()))?,
            _ = cancellation.cancelled() => Err(Error::Cancelled),
            _ = tokio::time::sleep(Duration::from_secs(60)) => Err(Error::Protocol("request context timed out".into())),
        };
        result
    }

    pub(crate) async fn handle_client(&self, message: &pb::ExecClientMessage) -> bool {
        let Some(pb::exec_client_message::Message::RequestContextResult(result)) =
            message.message.as_ref()
        else {
            return false;
        };
        let sender = {
            let mut pending = self.pending.lock();
            if pending.as_ref().is_some_and(|(id, _)| *id == message.id) {
                pending.take().map(|(_, sender)| sender)
            } else {
                None
            }
        };
        let Some(sender) = sender else {
            tracing::warn!(
                request_id = self.handle.request_id(),
                "unexpected Cursor request context result"
            );
            return true;
        };
        use pb::request_context_result::Result as ContextResult;
        let result = match result.result.as_ref() {
            Some(ContextResult::Success(success)) => success
                .request_context
                .clone()
                .ok_or_else(|| Error::Protocol("Cursor returned empty request context".into())),
            Some(ContextResult::Error(error)) => Err(Error::Protocol(format!(
                "Cursor request context failed: {}",
                error.error
            ))),
            Some(ContextResult::Rejected(rejected)) => Err(Error::Protocol(format!(
                "Cursor rejected request context: {}",
                rejected.reason
            ))),
            None => Err(Error::Protocol(
                "Cursor returned no request context result".into(),
            )),
        };
        let _ = sender.send(result);
        true
    }

    pub(crate) async fn handle_stream_close(&self, id: u32) -> bool {
        self.pending
            .lock()
            .as_ref()
            .is_some_and(|(pending_id, _)| *pending_id == id)
    }

    pub(crate) async fn handle_throw(&self, id: u32, message: String) -> bool {
        let sender = {
            let mut pending = self.pending.lock();
            if pending
                .as_ref()
                .is_some_and(|(pending_id, _)| *pending_id == id)
            {
                pending.take().map(|(_, sender)| sender)
            } else {
                None
            }
        };
        let Some(sender) = sender else { return false };
        let _ = sender.send(Err(Error::Protocol(message)));
        true
    }

    async fn has_missing_part(&self, parts: &pb::RequestContextPartReferences) -> Result<bool> {
        for raw_id in [
            parts.rules_blob_id.as_slice(),
            parts.skills_blob_id.as_slice(),
            parts.subagents_blob_id.as_slice(),
            parts.mcps_blob_id.as_slice(),
        ] {
            if raw_id.is_empty() {
                continue;
            }
            let id = BlobId::from_bytes(raw_id)?;
            if self.store.get_blob(&id).await?.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn cache_parts(&self, context: &pb::RequestContext) -> Result<()> {
        self.cache_part(&pb::RequestContextRulesPart {
            rules: context.rules.clone(),
            non_file_rules: context.non_file_rules.clone(),
            cloud_rule: context.cloud_rule.clone(),
        })
        .await?;
        self.cache_part(&pb::RequestContextSkillsPart {
            agent_skills: context.agent_skills.clone(),
            skill_options: context.skill_options.clone(),
        })
        .await?;
        self.cache_part(&pb::RequestContextSubagentsPart {
            custom_subagents: context.custom_subagents.clone(),
        })
        .await?;
        self.cache_part(&pb::RequestContextMcpsPart {
            tools: context.tools.clone(),
            mcp_instructions: context.mcp_instructions.clone(),
            mcp_file_system_options: context.mcp_file_system_options.clone(),
            mcp_meta_tool_options: context.mcp_meta_tool_options.clone(),
        })
        .await
    }

    async fn cache_part<T: Message>(&self, part: &T) -> Result<()> {
        let data = part.encode_to_vec();
        self.store.put_blob(&data, &[]).await?;
        Ok(())
    }
}

struct PendingContext {
    pending: Arc<Mutex<Option<(u32, ContextSender)>>>,
    id: u32,
}
impl Drop for PendingContext {
    fn drop(&mut self) {
        let mut pending = self.pending.lock();
        if pending.as_ref().is_some_and(|(id, _)| *id == self.id) {
            pending.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::{services::observability::CursorTraceService, transport::OutputHub};

    async fn fixture() -> (
        RequestContextSynchronizer,
        tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>,
    ) {
        let store = Store::connect("sqlite::memory:").await.unwrap();
        let (commands, _receiver) = tokio::sync::mpsc::channel(16);
        let output = Arc::new(OutputHub::default());
        let handle = TransportHandle::new(
            "context-test".into(),
            commands,
            output,
            CursorTraceService::new(store.clone()).recorder("context-test"),
        );
        let receiver = handle.subscribe();
        (
            RequestContextSynchronizer::new(handle, store, CursorToolRuntime::default()),
            receiver,
        )
    }
    fn response(id: u32) -> pb::ExecClientMessage {
        pb::ExecClientMessage {
            id,
            message: Some(pb::exec_client_message::Message::RequestContextResult(
                pb::RequestContextResult {
                    result: Some(pb::request_context_result::Result::Success(
                        pb::RequestContextSuccess {
                            request_context: Some(pb::RequestContext {
                                cloud_rule: Some(format!("context-{id}")),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    )),
                },
            )),
            ..Default::default()
        }
    }
    async fn requested_id(
        load: &mut std::pin::Pin<
            Box<impl std::future::Future<Output = Result<pb::RequestContext>>>,
        >,
        output: &mut tokio::sync::mpsc::UnboundedReceiver<bytes::Bytes>,
    ) -> u32 {
        let frame = tokio::select! { result = load => panic!("load ended before request: {result:?}"), frame = output.recv() => frame.unwrap() };
        let (_, payload) = crate::cursor::protocol::connect::decode_frames(&frame)
            .unwrap()
            .pop()
            .unwrap();
        let server = pb::AgentServerMessage::decode(payload).unwrap();
        let Some(pb::agent_server_message::Message::ExecServerMessage(exec)) = server.message
        else {
            panic!("missing exec")
        };
        exec.id
    }
    #[tokio::test]
    async fn dropped_load_releases_pending_and_stale_results_cannot_complete_replacement() {
        for stale_first in [true, false] {
            let (sync, mut output) = fixture().await;
            let mut old = Box::pin(sync.load("old"));
            let old_id = requested_id(&mut old, &mut output).await;
            drop(old);
            if stale_first {
                assert!(sync.handle_client(&response(old_id)).await);
            }
            let mut new = Box::pin(sync.load("new"));
            let new_id = requested_id(&mut new, &mut output).await;
            assert_ne!(old_id, new_id);
            assert!(!sync.handle_throw(old_id, "stale".into()).await);
            assert!(!sync.handle_stream_close(old_id).await);
            if !stale_first {
                assert!(sync.handle_client(&response(old_id)).await);
            }
            assert!(tokio::time::timeout(Duration::from_millis(5), &mut new)
                .await
                .is_err());
            assert!(sync.handle_stream_close(new_id).await);
            assert!(sync.handle_client(&response(new_id)).await);
            assert_eq!(
                new.await.unwrap().cloud_rule.as_deref(),
                Some(format!("context-{new_id}").as_str())
            );
            assert!(sync.handle_client(&response(old_id)).await);
            let mut third = Box::pin(sync.load("third"));
            let third_id = requested_id(&mut third, &mut output).await;
            assert!(sync.handle_client(&response(old_id)).await);
            assert!(tokio::time::timeout(Duration::from_millis(5), &mut third)
                .await
                .is_err());
            assert!(sync.handle_client(&response(third_id)).await);
            assert_eq!(
                third.await.unwrap().cloud_rule.as_deref(),
                Some(format!("context-{third_id}").as_str())
            );
        }
    }
    #[tokio::test]
    async fn matching_throw_fails_only_its_context_load() {
        let (sync, mut output) = fixture().await;
        let mut load = Box::pin(sync.load("one"));
        let id = requested_id(&mut load, &mut output).await;
        assert!(sync.handle_throw(id, "context failed".into()).await);
        assert!(load
            .await
            .unwrap_err()
            .to_string()
            .contains("context failed"));
        let mut retry = Box::pin(sync.load("retry"));
        let new_id = requested_id(&mut retry, &mut output).await;
        assert_ne!(id, new_id);
        assert!(sync.handle_client(&response(new_id)).await);
        assert!(retry.await.is_ok());
    }
}
