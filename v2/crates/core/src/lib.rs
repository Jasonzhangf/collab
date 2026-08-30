use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind {
    Peer,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Working,
    Blocked,
    Waiting,
    Verifying,
    Reviewed,
    Delivered,
    Rework,
    Merged,
    Closed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NotificationEvent {
    DirectMessage,
    ResourceReleased,
    Deadline,
    AsyncResult,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceNotice {
    Occupied,
    Released,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Absent,
    Unknown,
    Working,
    Waiting,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDisposition {
    Skipped,
    RetryPending,
    Delivered,
    Exhausted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    Armed,
    Consumed,
    Expired,
    Cancelled,
    Exhausted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Identity {
    pub id: String,
    pub session_id: String,
    pub pane: String,
    #[serde(default = "peer_kind")]
    pub kind: IdentityKind,
}

fn peer_kind() -> IdentityKind {
    IdentityKind::Peer
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WaitEdge {
    pub waiting_for: String,
    pub responsible_actor: String,
    pub reason: String,
    pub deadline_ms: u64,
    pub resume_on: Vec<String>,
    pub escalation: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Task {
    pub id: String,
    pub owner: String,
    pub feature_id: String,
    pub resource_id: String,
    pub state: TaskState,
    pub wait: Option<WaitEdge>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NotificationSubscription {
    pub id: String,
    pub owner: String,
    pub event: NotificationEvent,
    pub subject: Option<String>,
    pub expires_at_ms: u64,
    pub state: SubscriptionState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    pub notice: ResourceNotice,
    pub subject: String,
    pub subscription_id: String,
    pub wake_attempt_count: u8,
    pub last_wake_attempt_ms: Option<u64>,
    pub delivered: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoreState {
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub identities: Vec<Identity>,
    #[serde(default)]
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub subscriptions: Vec<NotificationSubscription>,
    #[serde(default)]
    pub messages: Vec<Message>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreError {
    DuplicateIdentity,
    UnknownIdentity,
    UnknownTask,
    PermissionDenied,
    InvalidTransition,
    TaskAlreadyOwned,
    InvalidWait,
    WaitCycle,
    InvalidSubscription,
    UnknownMessage,
    WakeExhausted,
}

impl CoreState {
    fn has_identity(&self, actor: &str) -> bool {
        self.identities.iter().any(|identity| identity.id == actor)
    }

    pub fn register(&mut self, identity: Identity) -> Result<(), CoreError> {
        if let Some(existing) = self.identities.iter_mut().find(|existing| {
            existing.id == identity.id && existing.session_id == identity.session_id
        }) {
            existing.pane = identity.pane;
            existing.kind = IdentityKind::Peer;
            return Ok(());
        }
        if self.identities.iter().any(|existing| {
            existing.id == identity.id || existing.session_id == identity.session_id
        }) {
            return Err(CoreError::DuplicateIdentity);
        }
        self.identities.push(Identity {
            kind: IdentityKind::Peer,
            ..identity
        });
        Ok(())
    }

    pub fn register_task(
        &mut self,
        actor: &str,
        id: impl Into<String>,
        feature_id: impl Into<String>,
        resource_id: impl Into<String>,
    ) -> Result<(), CoreError> {
        if !self.has_identity(actor) {
            return Err(CoreError::UnknownIdentity);
        }
        let id = id.into();
        if self.tasks.iter().any(|task| task.id == id) {
            return Err(CoreError::TaskAlreadyOwned);
        }
        self.tasks.push(Task {
            id,
            owner: actor.to_owned(),
            feature_id: feature_id.into(),
            resource_id: resource_id.into(),
            state: TaskState::Working,
            wait: None,
        });
        Ok(())
    }

    pub fn transition_task(
        &mut self,
        actor: &str,
        task_id: &str,
        next: TaskState,
    ) -> Result<(), CoreError> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or(CoreError::UnknownTask)?;
        if task.owner != actor {
            return Err(CoreError::PermissionDenied);
        }
        let valid = matches!(
            (task.state, next),
            (
                TaskState::Working,
                TaskState::Verifying | TaskState::Blocked | TaskState::Cancelled
            ) | (
                TaskState::Blocked,
                TaskState::Working | TaskState::Cancelled
            ) | (
                TaskState::Waiting,
                TaskState::Blocked | TaskState::Working | TaskState::Cancelled
            ) | (
                TaskState::Verifying,
                TaskState::Reviewed | TaskState::Working | TaskState::Cancelled
            ) | (
                TaskState::Reviewed,
                TaskState::Delivered | TaskState::Rework | TaskState::Cancelled
            ) | (TaskState::Delivered, TaskState::Merged | TaskState::Rework)
                | (TaskState::Rework, TaskState::Working | TaskState::Cancelled)
                | (TaskState::Merged, TaskState::Closed)
        );
        if !valid {
            return Err(CoreError::InvalidTransition);
        }
        task.state = next;
        task.wait = None;
        if matches!(
            next,
            TaskState::Rework | TaskState::Closed | TaskState::Cancelled
        ) {
            self.release_waiters(task_id);
        }
        Ok(())
    }

    fn release_waiters(&mut self, blocker_id: &str) {
        for task in &mut self.tasks {
            if task
                .wait
                .as_ref()
                .is_some_and(|wait| wait.waiting_for == blocker_id)
            {
                task.wait = None;
                task.state = TaskState::Blocked;
            }
        }
    }

    fn would_cycle(&self, waiter_id: &str, blocker_id: &str) -> bool {
        let mut current = blocker_id;
        for _ in 0..=self.tasks.len() {
            if current == waiter_id {
                return true;
            }
            let Some(next) = self
                .tasks
                .iter()
                .find(|task| task.id == current)
                .and_then(|task| task.wait.as_ref())
                .map(|wait| wait.waiting_for.as_str())
            else {
                return false;
            };
            current = next;
        }
        true
    }

    pub fn wait_task(
        &mut self,
        actor: &str,
        task_id: &str,
        blocker_id: &str,
        deadline_ms: u64,
        now_ms: u64,
    ) -> Result<(), CoreError> {
        if task_id == blocker_id || deadline_ms <= now_ms || self.would_cycle(task_id, blocker_id) {
            return Err(if self.would_cycle(task_id, blocker_id) {
                CoreError::WaitCycle
            } else {
                CoreError::InvalidWait
            });
        }
        let waiter = self
            .tasks
            .iter()
            .find(|task| task.id == task_id)
            .ok_or(CoreError::UnknownTask)?;
        let blocker = self
            .tasks
            .iter()
            .find(|task| task.id == blocker_id)
            .ok_or(CoreError::UnknownTask)?;
        if waiter.owner != actor {
            return Err(CoreError::PermissionDenied);
        }
        if waiter.resource_id != blocker.resource_id
            || !matches!(waiter.state, TaskState::Working | TaskState::Blocked)
            || matches!(
                blocker.state,
                TaskState::Delivered | TaskState::Merged | TaskState::Closed | TaskState::Cancelled
            )
        {
            return Err(CoreError::InvalidWait);
        }
        let responsible_actor = blocker.owner.clone();
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .unwrap();
        task.state = TaskState::Waiting;
        task.wait = Some(WaitEdge {
            waiting_for: blocker_id.to_owned(),
            responsible_actor,
            reason: "resource_conflict".into(),
            deadline_ms,
            resume_on: vec![
                "resource_released".into(),
                "blocker_rework".into(),
                "blocker_cancelled".into(),
            ],
            escalation: "owners_recheck_durable_truth".into(),
        });
        Ok(())
    }

    pub fn subscribe(
        &mut self,
        owner: &str,
        id: impl Into<String>,
        event: NotificationEvent,
        subject: Option<String>,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<(), CoreError> {
        let id = id.into();
        if !self.has_identity(owner)
            || expires_at_ms <= now_ms
            || self
                .subscriptions
                .iter()
                .any(|subscription| subscription.id == id)
        {
            return Err(CoreError::InvalidSubscription);
        }
        if !matches!(event, NotificationEvent::DirectMessage)
            && subject.as_deref().is_none_or(str::is_empty)
        {
            return Err(CoreError::InvalidSubscription);
        }
        self.subscriptions.push(NotificationSubscription {
            id,
            owner: owner.to_owned(),
            event,
            subject,
            expires_at_ms,
            state: SubscriptionState::Armed,
        });
        Ok(())
    }

    pub fn send_resource_notice(
        &mut self,
        id: impl Into<String>,
        from: &str,
        to: &str,
        notice: ResourceNotice,
        subject: &str,
    ) -> Result<(), CoreError> {
        if !self.has_identity(from) || !self.has_identity(to) || subject.is_empty() {
            return Err(CoreError::UnknownIdentity);
        }
        let subscription = self
            .subscriptions
            .iter()
            .find(|subscription| {
                subscription.owner == to
                    && subscription.state == SubscriptionState::Armed
                    && subscription.event == NotificationEvent::DirectMessage
            })
            .ok_or(CoreError::InvalidSubscription)?;
        self.messages.push(Message {
            id: id.into(),
            from: from.to_owned(),
            to: to.to_owned(),
            notice,
            subject: subject.to_owned(),
            subscription_id: subscription.id.clone(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: None,
            delivered: false,
        });
        Ok(())
    }

    pub fn record_wake_attempt(
        &mut self,
        message_id: &str,
        agent_state: AgentState,
        succeeded: bool,
        now_ms: u64,
    ) -> Result<WakeDisposition, CoreError> {
        if !matches!(agent_state, AgentState::Waiting) {
            return Ok(WakeDisposition::Skipped);
        }
        let message = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(CoreError::UnknownMessage)?;
        if message.wake_attempt_count >= 3 {
            return Err(CoreError::WakeExhausted);
        }
        let subscription = self
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == message.subscription_id)
            .ok_or(CoreError::InvalidSubscription)?;
        if subscription.state != SubscriptionState::Armed || subscription.expires_at_ms <= now_ms {
            subscription.state = SubscriptionState::Expired;
            return Err(CoreError::InvalidSubscription);
        }
        message.wake_attempt_count += 1;
        message.last_wake_attempt_ms = Some(now_ms);
        if succeeded {
            message.delivered = true;
            subscription.state = SubscriptionState::Consumed;
            return Ok(WakeDisposition::Delivered);
        }
        if message.wake_attempt_count == 3 {
            subscription.state = SubscriptionState::Exhausted;
            Ok(WakeDisposition::Exhausted)
        } else {
            Ok(WakeDisposition::RetryPending)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, session_id: &str) -> Identity {
        Identity {
            id: id.into(),
            session_id: session_id.into(),
            pane: format!("%{id}"),
            kind: IdentityKind::Peer,
        }
    }

    #[test]
    fn registrations_are_equal_peers_and_same_session_rebinds() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        assert_eq!(state.identities[0].kind, IdentityKind::Peer);
        assert_eq!(state.identities[1].kind, IdentityKind::Peer);
        let rebound = Identity {
            pane: "%a-new".into(),
            ..peer("a", "session-a")
        };
        state.register(rebound).unwrap();
        assert_eq!(state.identities[0].pane, "%a-new");
        assert_eq!(
            state.register(peer("a", "session-other")),
            Err(CoreError::DuplicateIdentity)
        );
    }

    #[test]
    fn owner_registers_and_completes_task_without_dispatch_or_claim() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .register_task("a", "task", "feature", "resource")
            .unwrap();
        assert_eq!(
            state.transition_task("b", "task", TaskState::Cancelled),
            Err(CoreError::PermissionDenied)
        );
        for next in [
            TaskState::Verifying,
            TaskState::Reviewed,
            TaskState::Delivered,
            TaskState::Merged,
            TaskState::Closed,
        ] {
            state.transition_task("a", "task", next).unwrap();
        }
        assert_eq!(state.tasks[0].state, TaskState::Closed);
    }

    #[test]
    fn wait_graph_rejects_cycles_and_release_clears_wait() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .register_task("a", "task-a", "feature", "resource")
            .unwrap();
        state
            .register_task("b", "task-b", "feature", "resource")
            .unwrap();
        state.wait_task("a", "task-a", "task-b", 200, 100).unwrap();
        assert_eq!(state.tasks[0].state, TaskState::Waiting);
        assert_eq!(
            state.wait_task("b", "task-b", "task-a", 200, 100),
            Err(CoreError::WaitCycle)
        );
        state
            .transition_task("b", "task-b", TaskState::Cancelled)
            .unwrap();
        assert_eq!(state.tasks[0].state, TaskState::Blocked);
        assert!(state.tasks[0].wait.is_none());
    }

    #[test]
    fn subscription_is_exact_one_shot_and_wake_attempts_stop_at_three() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .subscribe("b", "sub", NotificationEvent::DirectMessage, None, 200, 100)
            .unwrap();
        state
            .send_resource_notice("message", "a", "b", ResourceNotice::Occupied, "resource")
            .unwrap();
        assert_eq!(
            state.record_wake_attempt("message", AgentState::Unknown, false, 110),
            Ok(WakeDisposition::Skipped)
        );
        for now in [111, 112] {
            assert_eq!(
                state.record_wake_attempt("message", AgentState::Waiting, false, now),
                Ok(WakeDisposition::RetryPending)
            );
        }
        assert_eq!(
            state.record_wake_attempt("message", AgentState::Waiting, false, 113),
            Ok(WakeDisposition::Exhausted)
        );
        assert_eq!(
            state.record_wake_attempt("message", AgentState::Waiting, false, 114),
            Err(CoreError::WakeExhausted)
        );
        assert_eq!(state.messages[0].wake_attempt_count, 3);
    }
}
