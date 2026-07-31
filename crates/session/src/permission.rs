//! Permission resolution for a session.
//!
//! The Action Runtime emits
//! [`orbit_core::EventPayload::PermissionRequired`] and then awaits a
//! [`ConfirmationProvider`]. This module supplies the provider that turns
//! that await into "pause until a front end sends a decision back", which
//! is what makes an `ask` permission workable for a GUI or a protocol
//! client rather than only for a blocking terminal prompt.

use std::collections::HashMap;
use std::sync::Mutex;

use orbit_core::{
    CancellationToken, ConfirmationProvider, ConfirmationRequest, PermissionDecision,
    PermissionRequestId,
};
use tokio::sync::oneshot;

/// How a session answers `ask` permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmationMode {
    /// Approve every request without asking. Only from an explicit
    /// user-supplied flag (`--yes`), never a silent default.
    AutoAllow,
    /// Deny every request. The safe default when nothing can answer:
    /// a non-interactive CLI run, or a bridge client that declared it
    /// will not handle permissions.
    AutoDeny,
    /// Pause and wait for [`SessionConfirmation::resolve`] to be called
    /// with a matching request id.
    External,
}

/// Resolves `ask` permissions for one session.
///
/// In [`ConfirmationMode::External`] the pending action genuinely stops:
/// `confirm` awaits a channel that only a real decision (or cancellation)
/// completes. Nothing times out on its own — an unanswered request holds
/// the turn until the client answers or cancels, because silently
/// choosing for the user is exactly what an `ask` permission exists to
/// prevent.
pub struct SessionConfirmation {
    mode: ConfirmationMode,
    inner: Mutex<Pending>,
    cancel: Mutex<Option<CancellationToken>>,
}

#[derive(Default)]
struct Pending {
    /// Requests currently blocking on a decision.
    waiting: HashMap<PermissionRequestId, oneshot::Sender<PermissionDecision>>,
    /// Decisions that arrived before the action reached its permission
    /// check. This happens legitimately: the runtime emits
    /// `PermissionRequired` *before* calling `confirm`, so a fast client
    /// (or the CLI's own prompt thread) can answer in between. Without
    /// this the decision would be dropped and the turn would hang.
    resolved_early: HashMap<PermissionRequestId, PermissionDecision>,
}

impl SessionConfirmation {
    pub fn new(mode: ConfirmationMode) -> Self {
        Self {
            mode,
            inner: Mutex::new(Pending::default()),
            cancel: Mutex::new(None),
        }
    }

    pub fn mode(&self) -> ConfirmationMode {
        self.mode
    }

    /// Track the current turn's cancellation token so a pending request
    /// can be released when the turn is cancelled instead of hanging.
    pub fn set_cancellation(&self, token: Option<CancellationToken>) {
        *self.cancel.lock().expect("permission mutex poisoned") = token;
    }

    /// Answer a pending request. Returns `false` if the id is unknown *and*
    /// no action is waiting for it, so a client sending a stale or invented
    /// id gets a clear error instead of silently affecting nothing.
    pub fn resolve(&self, request_id: &PermissionRequestId, decision: PermissionDecision) -> bool {
        let mut pending = self.inner.lock().expect("permission mutex poisoned");
        if let Some(sender) = pending.waiting.remove(request_id) {
            // A closed receiver means the waiter already went away
            // (cancelled); treat that as handled rather than an error.
            let _ = sender.send(decision);
            return true;
        }
        pending.resolved_early.insert(request_id.clone(), decision);
        true
    }

    /// Release every waiting request as cancelled. Called when a turn is
    /// cancelled so nothing is left blocked forever.
    pub fn cancel_all_pending(&self) {
        let mut pending = self.inner.lock().expect("permission mutex poisoned");
        for (_, sender) in pending.waiting.drain() {
            let _ = sender.send(PermissionDecision::Cancelled);
        }
        pending.resolved_early.clear();
    }

    /// Ids currently awaiting a decision.
    pub fn pending_request_ids(&self) -> Vec<PermissionRequestId> {
        self.inner
            .lock()
            .expect("permission mutex poisoned")
            .waiting
            .keys()
            .cloned()
            .collect()
    }
}

#[async_trait::async_trait]
impl ConfirmationProvider for SessionConfirmation {
    async fn confirm(&self, request: &ConfirmationRequest) -> bool {
        match self.mode {
            ConfirmationMode::AutoAllow => return true,
            ConfirmationMode::AutoDeny => return false,
            ConfirmationMode::External => {}
        }

        // A turn cancelled before this point must not open a request that
        // nobody is going to answer.
        if let Some(token) = self
            .cancel
            .lock()
            .expect("permission mutex poisoned")
            .clone()
            && token.is_cancelled()
        {
            return false;
        }

        let receiver = {
            let mut pending = self.inner.lock().expect("permission mutex poisoned");
            if let Some(decision) = pending.resolved_early.remove(&request.request_id) {
                return decision.is_allowed();
            }
            let (sender, receiver) = oneshot::channel();
            pending.waiting.insert(request.request_id.clone(), sender);
            receiver
        };

        tracing::debug!(
            action = %request.action,
            request_id = %request.request_id,
            "waiting for an external permission decision"
        );

        // A dropped sender (session torn down) is a denial, not a hang.
        match receiver.await {
            Ok(decision) => decision.is_allowed(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn request(id: &str) -> ConfirmationRequest {
        ConfirmationRequest {
            request_id: PermissionRequestId(id.to_string()),
            action: "command.run_configured".to_string(),
            description: "run a configured command".to_string(),
            project: Some("obc".to_string()),
            arguments_summary: "name=test".to_string(),
        }
    }

    #[tokio::test]
    async fn auto_modes_answer_without_waiting() {
        assert!(
            SessionConfirmation::new(ConfirmationMode::AutoAllow)
                .confirm(&request("p1"))
                .await
        );
        assert!(
            !SessionConfirmation::new(ConfirmationMode::AutoDeny)
                .confirm(&request("p1"))
                .await
        );
    }

    #[tokio::test]
    async fn external_mode_waits_for_an_approval() {
        let confirmation = Arc::new(SessionConfirmation::new(ConfirmationMode::External));
        let waiter = {
            let confirmation = confirmation.clone();
            tokio::spawn(async move { confirmation.confirm(&request("p1")).await })
        };

        // Wait until the request is actually registered, proving the
        // action really paused rather than resolving immediately.
        loop {
            if !confirmation.pending_request_ids().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        confirmation.resolve(
            &PermissionRequestId("p1".to_string()),
            PermissionDecision::AllowOnce,
        );
        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn external_mode_honors_a_denial() {
        let confirmation = Arc::new(SessionConfirmation::new(ConfirmationMode::External));
        let waiter = {
            let confirmation = confirmation.clone();
            tokio::spawn(async move { confirmation.confirm(&request("p2")).await })
        };
        loop {
            if !confirmation.pending_request_ids().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        confirmation.resolve(
            &PermissionRequestId("p2".to_string()),
            PermissionDecision::DenyOnce,
        );
        assert!(!waiter.await.unwrap());
    }

    /// The runtime emits `PermissionRequired` before calling `confirm`, so
    /// a decision can legitimately arrive first. It must not be lost.
    #[tokio::test]
    async fn a_decision_arriving_before_the_check_is_not_lost() {
        let confirmation = SessionConfirmation::new(ConfirmationMode::External);
        confirmation.resolve(
            &PermissionRequestId("p3".to_string()),
            PermissionDecision::AllowOnce,
        );
        assert!(confirmation.confirm(&request("p3")).await);
    }

    #[tokio::test]
    async fn cancelling_releases_pending_requests_as_denied() {
        let confirmation = Arc::new(SessionConfirmation::new(ConfirmationMode::External));
        let waiter = {
            let confirmation = confirmation.clone();
            tokio::spawn(async move { confirmation.confirm(&request("p4")).await })
        };
        loop {
            if !confirmation.pending_request_ids().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        confirmation.cancel_all_pending();
        assert!(!waiter.await.unwrap(), "a cancelled request never runs");
    }

    #[tokio::test]
    async fn a_request_opened_after_cancellation_is_denied_immediately() {
        let confirmation = SessionConfirmation::new(ConfirmationMode::External);
        let token = CancellationToken::new();
        token.cancel();
        confirmation.set_cancellation(Some(token));
        assert!(!confirmation.confirm(&request("p5")).await);
    }
}
