use std::collections::HashMap;
use std::sync::Arc;

use codex_protocol::ConversationId;
use mcp_types::RequestId;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub(crate) struct BusyConversations {
    inner: Arc<Mutex<HashMap<ConversationId, RequestId>>>,
}

impl BusyConversations {
    pub(crate) async fn try_acquire(
        &self,
        conversation_id: ConversationId,
        request_id: &RequestId,
    ) -> Result<(), RequestId> {
        let mut map = self.inner.lock().await;
        if let Some(owner) = map.get(&conversation_id) {
            return Err(owner.clone());
        }
        map.insert(conversation_id, request_id.clone());
        Ok(())
    }

    pub(crate) async fn release(&self, conversation_id: ConversationId, request_id: &RequestId) {
        let mut map = self.inner.lock().await;
        if matches!(map.get(&conversation_id), Some(owner) if owner == request_id) {
            map.remove(&conversation_id);
        }
    }
}
