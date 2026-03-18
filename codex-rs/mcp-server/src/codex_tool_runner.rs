//! Asynchronous worker that executes a **Codex** tool-call inside a spawned
//! Tokio task. Separated from `message_processor.rs` to keep that file small
//! and to make future feature-growth easier to manage.

use std::collections::HashMap;
use std::sync::Arc;

use crate::busy_conversations::BusyConversations;
use crate::exec_approval::handle_exec_approval_request;
use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotificationMeta;
use crate::patch_approval::handle_patch_approval_request;
use crate::tool_response_format::ToolResponseFormat;
use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::ThreadManager;
use codex_core::config::Config as CodexConfig;
use codex_protocol::ThreadId;
use codex_protocol::protocol::AgentMessageEvent;
use codex_protocol::protocol::ApplyPatchApprovalRequestEvent;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ExecApprovalRequestEvent;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::Submission;
use codex_protocol::protocol::TurnCompleteEvent;
use codex_protocol::user_input::UserInput;
use rmcp::model::CallToolResult;
use rmcp::model::Content;
use rmcp::model::RequestId;
use serde_json::json;
use tokio::sync::Mutex;

/// To adhere to MCP `tools/call` response format, include the Codex
/// `threadId` in the `structured_content` field of the response.
/// Some MCP clients ignore `content` when `structuredContent` is present, so
/// mirror the text there as well.
pub(crate) fn create_call_tool_result_with_thread_id(
    thread_id: ThreadId,
    text: String,
    is_error: Option<bool>,
    tool_response_format: ToolResponseFormat,
) -> CallToolResult {
    let content = if tool_response_format.includes_text_content() {
        vec![Content::text(text.clone())]
    } else {
        Vec::new()
    };
    let structured_content = if tool_response_format.includes_structured_content() {
        Some(json!({
            "threadId": thread_id,
            "content": text,
        }))
    } else {
        None
    };
    CallToolResult {
        content,
        is_error,
        structured_content,
        meta: None,
    }
}

/// Run a complete Codex session and stream events back to the client.
///
/// On completion (success or error) the function sends the appropriate
/// `tools/call` response so the LLM can continue the conversation.
pub async fn run_codex_tool_session(
    id: RequestId,
    initial_prompt: String,
    config: CodexConfig,
    outgoing: Arc<OutgoingMessageSender>,
    thread_manager: Arc<ThreadManager>,
    busy_conversations: BusyConversations,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    tool_response_format: ToolResponseFormat,
) {
    let NewThread {
        thread_id,
        thread,
        session_configured,
    } = match thread_manager.start_thread(config).await {
        Ok(res) => res,
        Err(e) => {
            let result = CallToolResult {
                content: vec![Content::text(format!("Failed to start Codex session: {e}"))],
                is_error: Some(true),
                structured_content: None,
                meta: None,
            };
            outgoing.send_response(id.clone(), result).await;
            return;
        }
    };

    if let Err(owner) = busy_conversations.try_acquire(thread_id, &id).await {
        let owner_text = owner.to_string();
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            format!(
                "Conversation busy for conversation_id: {thread_id} (in-flight request: {owner_text})"
            ),
            Some(true),
            tool_response_format,
        );
        outgoing.send_response(id.clone(), result).await;
        thread_manager.remove_thread(&thread_id).await;
        return;
    }

    let session_configured_event = Event {
        id: "".to_string(),
        msg: EventMsg::SessionConfigured(session_configured.clone()),
    };
    outgoing
        .send_event_as_notification(
            &session_configured_event,
            Some(OutgoingNotificationMeta {
                request_id: Some(id.clone()),
                thread_id: Some(thread_id),
            }),
        )
        .await;

    let sub_id = id.to_string();
    running_requests_id_to_codex_uuid
        .lock()
        .await
        .insert(id.clone(), thread_id);
    let submission = Submission {
        id: sub_id,
        op: Op::UserInput {
            items: vec![UserInput::Text {
                text: initial_prompt,
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        },
        trace: None,
    };

    if let Err(e) = thread.submit_with_id(submission).await {
        tracing::error!("Failed to submit initial prompt: {e}");
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            format!("Failed to submit initial prompt: {e}"),
            Some(true),
            tool_response_format,
        );
        outgoing.send_response(id.clone(), result).await;
        running_requests_id_to_codex_uuid.lock().await.remove(&id);
        busy_conversations.release(thread_id, &id).await;
        return;
    }

    run_codex_tool_session_inner(
        thread_id,
        thread,
        outgoing,
        id,
        busy_conversations,
        running_requests_id_to_codex_uuid,
        tool_response_format,
    )
    .await;
}

pub async fn run_codex_tool_session_reply(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    prompt: String,
    busy_conversations: BusyConversations,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    tool_response_format: ToolResponseFormat,
) {
    running_requests_id_to_codex_uuid
        .lock()
        .await
        .insert(request_id.clone(), thread_id);
    if let Err(e) = thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: prompt,
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
        })
        .await
    {
        tracing::error!("Failed to submit user input: {e}");
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            format!("Failed to submit user input: {e}"),
            Some(true),
            tool_response_format,
        );
        outgoing.send_response(request_id.clone(), result).await;
        running_requests_id_to_codex_uuid
            .lock()
            .await
            .remove(&request_id);
        busy_conversations.release(thread_id, &request_id).await;
        return;
    }

    run_codex_tool_session_inner(
        thread_id,
        thread,
        outgoing,
        request_id,
        busy_conversations,
        running_requests_id_to_codex_uuid,
        tool_response_format,
    )
    .await;
}

async fn run_codex_tool_session_inner(
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    busy_conversations: BusyConversations,
    running_requests_id_to_codex_uuid: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    tool_response_format: ToolResponseFormat,
) {
    let request_id_str = request_id.to_string();

    loop {
        match thread.next_event().await {
            Ok(event) => {
                outgoing
                    .send_event_as_notification(
                        &event,
                        Some(OutgoingNotificationMeta {
                            request_id: Some(request_id.clone()),
                            thread_id: Some(thread_id),
                        }),
                    )
                    .await;

                match event.msg {
                    EventMsg::ExecApprovalRequest(ev) => {
                        let approval_id = ev.effective_approval_id();
                        let ExecApprovalRequestEvent {
                            turn_id: _,
                            command,
                            cwd,
                            call_id,
                            approval_id: _,
                            reason: _,
                            proposed_execpolicy_amendment: _,
                            proposed_network_policy_amendments: _,
                            parsed_cmd,
                            network_approval_context: _,
                            additional_permissions: _,
                            skill_metadata: _,
                            available_decisions: _,
                        } = ev;
                        handle_exec_approval_request(
                            command,
                            cwd,
                            outgoing.clone(),
                            thread.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            call_id,
                            approval_id,
                            parsed_cmd,
                            thread_id,
                        )
                        .await;
                        continue;
                    }
                    EventMsg::PlanDelta(_) => {
                        continue;
                    }
                    EventMsg::Error(err_event) => {
                        let result = create_call_tool_result_with_thread_id(
                            thread_id,
                            err_event.message,
                            Some(true),
                            tool_response_format,
                        );
                        outgoing.send_response(request_id.clone(), result).await;
                        break;
                    }
                    EventMsg::Warning(_) => {
                        continue;
                    }
                    EventMsg::GuardianAssessment(_) => {
                        continue;
                    }
                    EventMsg::ElicitationRequest(_) => {
                        continue;
                    }
                    EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                        call_id,
                        turn_id: _,
                        reason,
                        grant_root,
                        changes,
                    }) => {
                        handle_patch_approval_request(
                            call_id,
                            reason,
                            grant_root,
                            changes,
                            outgoing.clone(),
                            thread.clone(),
                            request_id.clone(),
                            request_id_str.clone(),
                            event.id.clone(),
                            thread_id,
                        )
                        .await;
                        continue;
                    }
                    EventMsg::TurnComplete(TurnCompleteEvent {
                        last_agent_message, ..
                    }) => {
                        let text = last_agent_message.unwrap_or_default();
                        let result = create_call_tool_result_with_thread_id(
                            thread_id,
                            text,
                            None,
                            tool_response_format,
                        );
                        outgoing.send_response(request_id.clone(), result).await;
                        running_requests_id_to_codex_uuid
                            .lock()
                            .await
                            .remove(&request_id);
                        break;
                    }
                    EventMsg::SessionConfigured(_) => {
                        tracing::error!("unexpected SessionConfigured event");
                    }
                    EventMsg::ThreadNameUpdated(_) => {}
                    EventMsg::AgentMessageDelta(_) => {}
                    EventMsg::AgentReasoningDelta(_) => {}
                    EventMsg::McpStartupUpdate(_) | EventMsg::McpStartupComplete(_) => {}
                    EventMsg::AgentMessage(AgentMessageEvent { .. }) => {}
                    EventMsg::AgentReasoningRawContent(_)
                    | EventMsg::AgentReasoningRawContentDelta(_)
                    | EventMsg::TurnStarted(_)
                    | EventMsg::TokenCount(_)
                    | EventMsg::AgentReasoning(_)
                    | EventMsg::AgentReasoningSectionBreak(_)
                    | EventMsg::McpToolCallBegin(_)
                    | EventMsg::McpToolCallEnd(_)
                    | EventMsg::McpListToolsResponse(_)
                    | EventMsg::ListCustomPromptsResponse(_)
                    | EventMsg::ListSkillsResponse(_)
                    | EventMsg::ExecCommandBegin(_)
                    | EventMsg::TerminalInteraction(_)
                    | EventMsg::ExecCommandOutputDelta(_)
                    | EventMsg::ExecCommandEnd(_)
                    | EventMsg::BackgroundEvent(_)
                    | EventMsg::StreamError(_)
                    | EventMsg::PatchApplyBegin(_)
                    | EventMsg::PatchApplyEnd(_)
                    | EventMsg::TurnDiff(_)
                    | EventMsg::WebSearchBegin(_)
                    | EventMsg::WebSearchEnd(_)
                    | EventMsg::GetHistoryEntryResponse(_)
                    | EventMsg::PlanUpdate(_)
                    | EventMsg::TurnAborted(_)
                    | EventMsg::UserMessage(_)
                    | EventMsg::ShutdownComplete
                    | EventMsg::ViewImageToolCall(_)
                    | EventMsg::ImageGenerationBegin(_)
                    | EventMsg::ImageGenerationEnd(_)
                    | EventMsg::RawResponseItem(_)
                    | EventMsg::EnteredReviewMode(_)
                    | EventMsg::ItemStarted(_)
                    | EventMsg::ItemCompleted(_)
                    | EventMsg::HookStarted(_)
                    | EventMsg::HookCompleted(_)
                    | EventMsg::AgentMessageContentDelta(_)
                    | EventMsg::ReasoningContentDelta(_)
                    | EventMsg::ReasoningRawContentDelta(_)
                    | EventMsg::SkillsUpdateAvailable
                    | EventMsg::UndoStarted(_)
                    | EventMsg::UndoCompleted(_)
                    | EventMsg::ExitedReviewMode(_)
                    | EventMsg::RequestUserInput(_)
                    | EventMsg::RequestPermissions(_)
                    | EventMsg::DynamicToolCallRequest(_)
                    | EventMsg::DynamicToolCallResponse(_)
                    | EventMsg::ContextCompacted(_)
                    | EventMsg::ModelReroute(_)
                    | EventMsg::ThreadRolledBack(_)
                    | EventMsg::CollabAgentSpawnBegin(_)
                    | EventMsg::CollabAgentSpawnEnd(_)
                    | EventMsg::CollabAgentInteractionBegin(_)
                    | EventMsg::CollabAgentInteractionEnd(_)
                    | EventMsg::CollabWaitingBegin(_)
                    | EventMsg::CollabWaitingEnd(_)
                    | EventMsg::CollabCloseBegin(_)
                    | EventMsg::CollabCloseEnd(_)
                    | EventMsg::CollabResumeBegin(_)
                    | EventMsg::CollabResumeEnd(_)
                    | EventMsg::RealtimeConversationStarted(_)
                    | EventMsg::RealtimeConversationRealtime(_)
                    | EventMsg::RealtimeConversationClosed(_)
                    | EventMsg::DeprecationNotice(_) => {}
                }
            }
            Err(e) => {
                let result = create_call_tool_result_with_thread_id(
                    thread_id,
                    format!("Codex runtime error: {e}"),
                    Some(true),
                    tool_response_format,
                );
                outgoing.send_response(request_id.clone(), result).await;
                break;
            }
        }
    }

    busy_conversations.release(thread_id, &request_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn dual_format_includes_text_and_structured_content() {
        let thread_id = ThreadId::new();
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            "done".to_string(),
            None,
            ToolResponseFormat::Dual,
        );
        assert_eq!(result.content, vec![Content::text("done")]);
        assert_eq!(
            result.structured_content,
            Some(json!({
                "threadId": thread_id,
                "content": "done",
            }))
        );
    }

    #[test]
    fn structured_only_format_omits_text_content() {
        let thread_id = ThreadId::new();
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            "done".to_string(),
            None,
            ToolResponseFormat::StructuredOnly,
        );
        assert_eq!(result.content, Vec::<Content>::new());
        assert_eq!(
            result.structured_content,
            Some(json!({
                "threadId": thread_id,
                "content": "done",
            }))
        );
    }

    #[test]
    fn content_only_format_omits_structured_content() {
        let thread_id = ThreadId::new();
        let result = create_call_tool_result_with_thread_id(
            thread_id,
            "done".to_string(),
            None,
            ToolResponseFormat::ContentOnly,
        );
        assert_eq!(result.content, vec![Content::text("done")]);
        assert_eq!(result.structured_content, None);
    }
}
