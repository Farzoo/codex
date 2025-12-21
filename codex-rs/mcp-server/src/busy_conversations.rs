use std::collections::HashMap;
use std::sync::Arc;

use codex_protocol::ThreadId;
use rmcp::model::RequestId;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub(crate) struct BusyConversations {
    inner: Arc<Mutex<HashMap<ThreadId, RequestId>>>,
}

impl BusyConversations {
    pub(crate) async fn try_acquire(
        &self,
        thread_id: ThreadId,
        request_id: &RequestId,
    ) -> Result<(), RequestId> {
        let mut map = self.inner.lock().await;
        if let Some(owner) = map.get(&thread_id) {
            return Err(owner.clone());
        }
        map.insert(thread_id, request_id.clone());
        Ok(())
    }

    pub(crate) async fn release(&self, thread_id: ThreadId, request_id: &RequestId) {
        let mut map = self.inner.lock().await;
        if matches!(map.get(&thread_id), Some(owner) if owner == request_id) {
            map.remove(&thread_id);
        }
    }
}
