use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    AttemptGranted,
    LeaseRecovered,
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
    pub from: Option<String>,
    pub to: String,
    pub event: NotificationEvent,
    pub notice: Option<ResourceNotice>,
    pub subject: String,
    pub subscription_id: Option<String>,
    pub wake_attempt_count: u8,
    pub last_wake_attempt_ms: Option<u64>,
    #[serde(default)]
    pub wake_in_flight: bool,
    #[serde(default)]
    pub wake_lease_expires_at_ms: Option<u64>,
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
    #[serde(default)]
    pub migration: Option<MigrationRecord>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    Frozen,
    Verified,
    Resumed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MigrationRecord {
    pub source_sha256: String,
    pub identity_count: usize,
    pub task_count: usize,
    pub continuity_sha256: String,
    pub phase: MigrationPhase,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LegacyMigrationPlan {
    pub source_sha256: String,
    pub identity_count: usize,
    pub task_count: usize,
    pub issues: Vec<String>,
    pub identities: Vec<Identity>,
    pub tasks: Vec<Task>,
    pub continuity_sha256: Option<String>,
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
    InvalidMessageId,
    UnknownMessage,
    DuplicateMessage,
    WakeExhausted,
    JournalGap,
    DuplicateCommand,
    WakeAttemptInFlight,
    InvalidWakeCompletion,
    InvalidWakeRecovery,
    MigrationBlocked,
    InvalidMigrationState,
    UnknownSubscription,
}

const MAX_MESSAGE_ID_LEN: usize = 128;

fn validate_message_id(message_id: &str) -> Result<(), CoreError> {
    let bytes = message_id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE_ID_LEN {
        return Err(CoreError::InvalidMessageId);
    }
    if !bytes[0].is_ascii_alphanumeric()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(CoreError::InvalidMessageId);
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CoreCommand {
    Register {
        identity: Identity,
    },
    RegisterTask {
        actor: String,
        task_id: String,
        feature_id: String,
        resource_id: String,
    },
    TransitionTask {
        actor: String,
        task_id: String,
        state: TaskState,
    },
    WaitTask {
        actor: String,
        task_id: String,
        blocking_task_id: String,
        deadline_ms: u64,
        now_ms: u64,
    },
    Subscribe {
        owner: String,
        subscription_id: String,
        event: NotificationEvent,
        subject: Option<String>,
        expires_at_ms: u64,
        now_ms: u64,
    },
    SendResourceNotice {
        message_id: String,
        from: String,
        to: String,
        notice: ResourceNotice,
        subject: String,
    },
    BeginWakeAttempt {
        message_id: String,
        agent_state: AgentState,
        now_ms: u64,
    },
    CompleteWakeAttempt {
        message_id: String,
        attempt: u8,
        succeeded: bool,
    },
    RecoverWakeAttempt {
        message_id: String,
        now_ms: u64,
    },
    ApplyMigration {
        plan: LegacyMigrationPlan,
    },
    VerifyMigration,
    ResumeMigration,
    Unsubscribe {
        owner: String,
        subscription_id: String,
    },
    ExpireSubscriptions {
        now_ms: u64,
    },
    PublishSubscriptionEvent {
        message_id: String,
        subscription_id: String,
        event: NotificationEvent,
        subject: String,
        now_ms: u64,
    },
    ExpireWaits {
        now_ms: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JournalEntry {
    pub sequence: u64,
    pub command_id: String,
    pub command: CoreCommand,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyBetaState {
    identities: Vec<LegacyIdentity>,
    tasks: Vec<LegacyTask>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyIdentity {
    id: String,
    session_id: String,
    role: serde_json::Value,
}

#[derive(Clone, Copy, Deserialize)]
enum LegacyTaskState {
    Available,
    Working,
    Verifying,
    Reviewing,
    Delivered,
    Merged,
    Closed,
    Blocked,
    Cancelled,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTask {
    id: String,
    state: LegacyTaskState,
    owner: Option<String>,
}

pub fn plan_legacy_beta_migration(raw: &str) -> Result<LegacyMigrationPlan, String> {
    let legacy: LegacyBetaState = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let source_sha256 = Sha256::digest(raw.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let identities: Vec<Identity> = legacy
        .identities
        .iter()
        .map(|identity| {
            if identity.role.is_null() {
                return Err(format!("identity {} has no legacy role", identity.id));
            }
            Ok(Identity {
                id: identity.id.clone(),
                session_id: identity.session_id.clone(),
                pane: String::new(),
                kind: IdentityKind::Peer,
            })
        })
        .collect::<Result<_, _>>()?;
    let mut issues = Vec::new();
    let mut tasks = Vec::new();
    for task in &legacy.tasks {
        let Some(owner) = task.owner.as_ref() else {
            issues.push(format!("task {} has no owner", task.id));
            continue;
        };
        if !identities.iter().any(|identity| identity.id == *owner) {
            issues.push(format!("task {} owner {} is missing", task.id, owner));
            continue;
        }
        if matches!(task.state, LegacyTaskState::Available) {
            issues.push(format!("task {} is unowned available work", task.id));
            continue;
        }
        let state = match task.state {
            LegacyTaskState::Working => TaskState::Working,
            LegacyTaskState::Verifying => TaskState::Verifying,
            LegacyTaskState::Reviewing => TaskState::Reviewed,
            LegacyTaskState::Delivered => TaskState::Delivered,
            LegacyTaskState::Merged => TaskState::Merged,
            LegacyTaskState::Closed => TaskState::Closed,
            LegacyTaskState::Blocked => TaskState::Blocked,
            LegacyTaskState::Cancelled => TaskState::Cancelled,
            LegacyTaskState::Available => unreachable!(),
        };
        tasks.push(Task {
            id: task.id.clone(),
            owner: owner.clone(),
            feature_id: "legacy-v2-beta".into(),
            resource_id: format!("legacy-task:{}", task.id),
            state,
            wait: None,
        });
    }
    let mut plan = LegacyMigrationPlan {
        source_sha256,
        identity_count: identities.len(),
        task_count: legacy.tasks.len(),
        issues,
        identities,
        tasks,
        continuity_sha256: None,
    };
    if plan.issues.is_empty() {
        let preview = CoreState {
            identities: plan.identities.clone(),
            tasks: plan.tasks.clone(),
            ..CoreState::default()
        };
        plan.continuity_sha256 = Some(
            preview
                .continuity_sha256()
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(plan)
}

impl CoreState {
    pub fn apply(&mut self, command: &CoreCommand) -> Result<Option<WakeDisposition>, CoreError> {
        if self.migration.as_ref().is_some_and(|migration| {
            matches!(
                migration.phase,
                MigrationPhase::Frozen | MigrationPhase::Verified
            )
        }) && !matches!(
            command,
            CoreCommand::Register { .. }
                | CoreCommand::VerifyMigration
                | CoreCommand::ResumeMigration
        ) {
            return Err(CoreError::InvalidMigrationState);
        }
        match command {
            CoreCommand::Register { identity } => self.register(identity.clone()).map(|_| None),
            CoreCommand::RegisterTask {
                actor,
                task_id,
                feature_id,
                resource_id,
            } => self
                .register_task(actor, task_id, feature_id, resource_id)
                .map(|_| None),
            CoreCommand::TransitionTask {
                actor,
                task_id,
                state,
            } => self.transition_task(actor, task_id, *state).map(|_| None),
            CoreCommand::WaitTask {
                actor,
                task_id,
                blocking_task_id,
                deadline_ms,
                now_ms,
            } => self
                .wait_task(actor, task_id, blocking_task_id, *deadline_ms, *now_ms)
                .map(|_| None),
            CoreCommand::Subscribe {
                owner,
                subscription_id,
                event,
                subject,
                expires_at_ms,
                now_ms,
            } => self
                .subscribe(
                    owner,
                    subscription_id,
                    *event,
                    subject.clone(),
                    *expires_at_ms,
                    *now_ms,
                )
                .map(|_| None),
            CoreCommand::SendResourceNotice {
                message_id,
                from,
                to,
                notice,
                subject,
            } => self
                .send_resource_notice(message_id, from, to, *notice, subject)
                .map(|_| None),
            CoreCommand::BeginWakeAttempt {
                message_id,
                agent_state,
                now_ms,
            } => self
                .begin_wake_attempt(message_id, *agent_state, *now_ms)
                .map(Some),
            CoreCommand::CompleteWakeAttempt {
                message_id,
                attempt,
                succeeded,
            } => self
                .complete_wake_attempt(message_id, *attempt, *succeeded)
                .map(Some),
            CoreCommand::RecoverWakeAttempt { message_id, now_ms } => {
                self.recover_wake_attempt(message_id, *now_ms).map(Some)
            }
            CoreCommand::ApplyMigration { plan } => self.apply_migration(plan).map(|_| None),
            CoreCommand::VerifyMigration => self.verify_migration().map(|_| None),
            CoreCommand::ResumeMigration => self.resume_migration().map(|_| None),
            CoreCommand::Unsubscribe {
                owner,
                subscription_id,
            } => self.unsubscribe(owner, subscription_id).map(|_| None),
            CoreCommand::ExpireSubscriptions { now_ms } => {
                self.expire_subscriptions(*now_ms);
                Ok(None)
            }
            CoreCommand::PublishSubscriptionEvent {
                message_id,
                subscription_id,
                event,
                subject,
                now_ms,
            } => self
                .publish_subscription_event(message_id, subscription_id, *event, subject, *now_ms)
                .map(|_| None),
            CoreCommand::ExpireWaits { now_ms } => {
                self.expire_waits(*now_ms);
                Ok(None)
            }
        }
    }

    pub fn replay(entries: &[JournalEntry]) -> Result<Self, CoreError> {
        let mut state = Self::default();
        let mut command_ids = std::collections::BTreeSet::new();
        for entry in entries {
            if entry.sequence != state.sequence + 1 {
                return Err(CoreError::JournalGap);
            }
            if !command_ids.insert(entry.command_id.as_str()) {
                return Err(CoreError::DuplicateCommand);
            }
            state.apply(&entry.command)?;
            state.sequence = entry.sequence;
        }
        Ok(state)
    }

    pub fn snapshot_sha256(&self) -> Result<String, serde_json::Error> {
        let raw = serde_json::to_vec(self)?;
        Ok(Sha256::digest(raw)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    pub fn continuity_sha256(&self) -> Result<String, serde_json::Error> {
        let stable_identities: Vec<_> = self
            .identities
            .iter()
            .map(|identity| (&identity.id, &identity.session_id, identity.kind))
            .collect();
        let raw = serde_json::to_vec(&(
            stable_identities,
            &self.tasks,
            &self.subscriptions,
            &self.messages,
        ))?;
        Ok(Sha256::digest(raw)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    fn apply_migration(&mut self, plan: &LegacyMigrationPlan) -> Result<(), CoreError> {
        if self.sequence != 0
            || !self.identities.is_empty()
            || !self.tasks.is_empty()
            || self.migration.is_some()
            || !plan.issues.is_empty()
        {
            return Err(CoreError::MigrationBlocked);
        }
        self.identities = plan.identities.clone();
        self.tasks = plan.tasks.clone();
        let continuity_sha256 = self
            .continuity_sha256()
            .map_err(|_| CoreError::MigrationBlocked)?;
        if plan.continuity_sha256.as_deref() != Some(&continuity_sha256) {
            return Err(CoreError::MigrationBlocked);
        }
        self.migration = Some(MigrationRecord {
            source_sha256: plan.source_sha256.clone(),
            identity_count: plan.identity_count,
            task_count: plan.task_count,
            continuity_sha256,
            phase: MigrationPhase::Frozen,
        });
        Ok(())
    }

    fn verify_migration(&mut self) -> Result<(), CoreError> {
        let continuity = self
            .continuity_sha256()
            .map_err(|_| CoreError::InvalidMigrationState)?;
        let migration = self
            .migration
            .as_mut()
            .ok_or(CoreError::InvalidMigrationState)?;
        if migration.phase != MigrationPhase::Frozen
            || migration.identity_count != self.identities.len()
            || migration.task_count != self.tasks.len()
            || migration.continuity_sha256 != continuity
        {
            return Err(CoreError::InvalidMigrationState);
        }
        migration.phase = MigrationPhase::Verified;
        Ok(())
    }

    fn resume_migration(&mut self) -> Result<(), CoreError> {
        let migration = self
            .migration
            .as_mut()
            .ok_or(CoreError::InvalidMigrationState)?;
        if migration.phase != MigrationPhase::Verified {
            return Err(CoreError::InvalidMigrationState);
        }
        migration.phase = MigrationPhase::Resumed;
        Ok(())
    }

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

    pub fn unsubscribe(&mut self, owner: &str, subscription_id: &str) -> Result<(), CoreError> {
        let subscription = self
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == subscription_id)
            .ok_or(CoreError::UnknownSubscription)?;
        if subscription.owner != owner || subscription.state != SubscriptionState::Armed {
            return Err(CoreError::PermissionDenied);
        }
        subscription.state = SubscriptionState::Cancelled;
        Ok(())
    }

    pub fn expire_subscriptions(&mut self, now_ms: u64) {
        for subscription in &mut self.subscriptions {
            if subscription.state == SubscriptionState::Armed
                && subscription.expires_at_ms <= now_ms
            {
                subscription.state = SubscriptionState::Expired;
            }
        }
    }

    pub fn publish_subscription_event(
        &mut self,
        message_id: &str,
        subscription_id: &str,
        event: NotificationEvent,
        subject: &str,
        now_ms: u64,
    ) -> Result<(), CoreError> {
        validate_message_id(message_id)?;
        if event == NotificationEvent::DirectMessage
            || subject.is_empty()
            || self.messages.iter().any(|message| message.id == message_id)
        {
            return Err(CoreError::InvalidSubscription);
        }
        let subscription = self
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == subscription_id)
            .ok_or(CoreError::UnknownSubscription)?;
        if subscription.state != SubscriptionState::Armed
            || subscription.event != event
            || subscription.subject.as_deref() != Some(subject)
        {
            return Err(CoreError::InvalidSubscription);
        }
        if subscription.expires_at_ms <= now_ms {
            subscription.state = SubscriptionState::Expired;
            return Err(CoreError::InvalidSubscription);
        }
        self.messages.push(Message {
            id: message_id.to_owned(),
            from: None,
            to: subscription.owner.clone(),
            event,
            notice: None,
            subject: subject.to_owned(),
            subscription_id: Some(subscription.id.clone()),
            wake_attempt_count: 0,
            last_wake_attempt_ms: None,
            wake_in_flight: false,
            wake_lease_expires_at_ms: None,
            delivered: false,
        });
        Ok(())
    }

    pub fn expire_waits(&mut self, now_ms: u64) {
        for task in &mut self.tasks {
            if task.state == TaskState::Waiting
                && task
                    .wait
                    .as_ref()
                    .is_some_and(|wait| wait.deadline_ms <= now_ms)
            {
                task.state = TaskState::Blocked;
                task.wait = None;
            }
        }
    }

    pub fn send_resource_notice(
        &mut self,
        id: impl Into<String>,
        from: &str,
        to: &str,
        notice: ResourceNotice,
        subject: &str,
    ) -> Result<(), CoreError> {
        let id = id.into();
        validate_message_id(&id)?;
        if !self.has_identity(from) || !self.has_identity(to) || subject.is_empty() {
            return Err(CoreError::UnknownIdentity);
        }
        if self.messages.iter().any(|message| message.id == id) {
            return Err(CoreError::DuplicateMessage);
        }
        let subscription = self.subscriptions.iter().find(|subscription| {
            subscription.owner == to
                && subscription.state == SubscriptionState::Armed
                && (subscription.event == NotificationEvent::DirectMessage
                    || (notice == ResourceNotice::Released
                        && subscription.event == NotificationEvent::ResourceReleased
                        && subscription.subject.as_deref() == Some(subject)))
        });
        self.messages.push(Message {
            id,
            from: Some(from.to_owned()),
            to: to.to_owned(),
            event: if notice == ResourceNotice::Released {
                NotificationEvent::ResourceReleased
            } else {
                NotificationEvent::DirectMessage
            },
            notice: Some(notice),
            subject: subject.to_owned(),
            subscription_id: subscription.map(|subscription| subscription.id.clone()),
            wake_attempt_count: 0,
            last_wake_attempt_ms: None,
            wake_in_flight: false,
            wake_lease_expires_at_ms: None,
            delivered: false,
        });
        Ok(())
    }

    pub fn begin_wake_attempt(
        &mut self,
        message_id: &str,
        agent_state: AgentState,
        now_ms: u64,
    ) -> Result<WakeDisposition, CoreError> {
        validate_message_id(message_id)?;
        if !matches!(agent_state, AgentState::Waiting) {
            return Ok(WakeDisposition::Skipped);
        }
        let message = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(CoreError::UnknownMessage)?;
        let Some(subscription_id) = message.subscription_id.as_ref() else {
            return Ok(WakeDisposition::Skipped);
        };
        if message.wake_attempt_count >= 3 {
            return Err(CoreError::WakeExhausted);
        }
        if message.wake_in_flight {
            return Err(CoreError::WakeAttemptInFlight);
        }
        let subscription = self
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == *subscription_id)
            .ok_or(CoreError::InvalidSubscription)?;
        if subscription.state != SubscriptionState::Armed || subscription.expires_at_ms <= now_ms {
            subscription.state = SubscriptionState::Expired;
            return Err(CoreError::InvalidSubscription);
        }
        message.wake_attempt_count += 1;
        message.last_wake_attempt_ms = Some(now_ms);
        message.wake_in_flight = true;
        message.wake_lease_expires_at_ms = Some(now_ms.saturating_add(60_000));
        Ok(WakeDisposition::AttemptGranted)
    }

    pub fn complete_wake_attempt(
        &mut self,
        message_id: &str,
        attempt: u8,
        succeeded: bool,
    ) -> Result<WakeDisposition, CoreError> {
        validate_message_id(message_id)?;
        let message = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(CoreError::UnknownMessage)?;
        if !message.wake_in_flight || message.wake_attempt_count != attempt {
            return Err(CoreError::InvalidWakeCompletion);
        }
        let subscription_id = message
            .subscription_id
            .as_ref()
            .ok_or(CoreError::InvalidSubscription)?;
        let subscription = self
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == *subscription_id)
            .ok_or(CoreError::InvalidSubscription)?;
        message.wake_in_flight = false;
        message.wake_lease_expires_at_ms = None;
        if succeeded {
            message.delivered = true;
            subscription.state = SubscriptionState::Consumed;
            return Ok(WakeDisposition::Delivered);
        }
        if attempt == 3 {
            subscription.state = SubscriptionState::Exhausted;
            Ok(WakeDisposition::Exhausted)
        } else {
            Ok(WakeDisposition::RetryPending)
        }
    }

    pub fn recover_wake_attempt(
        &mut self,
        message_id: &str,
        now_ms: u64,
    ) -> Result<WakeDisposition, CoreError> {
        validate_message_id(message_id)?;
        let message = self
            .messages
            .iter_mut()
            .find(|message| message.id == message_id)
            .ok_or(CoreError::UnknownMessage)?;
        if !message.wake_in_flight
            || message
                .wake_lease_expires_at_ms
                .is_none_or(|expires_at| expires_at > now_ms)
        {
            return Err(CoreError::InvalidWakeRecovery);
        }
        let subscription_id = message
            .subscription_id
            .as_ref()
            .ok_or(CoreError::InvalidSubscription)?;
        let subscription = self
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == *subscription_id)
            .ok_or(CoreError::InvalidSubscription)?;
        message.wake_in_flight = false;
        message.wake_lease_expires_at_ms = None;
        if message.wake_attempt_count >= 3 {
            subscription.state = SubscriptionState::Exhausted;
            Ok(WakeDisposition::Exhausted)
        } else {
            Ok(WakeDisposition::LeaseRecovered)
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
            state.begin_wake_attempt("message", AgentState::Unknown, 110),
            Ok(WakeDisposition::Skipped)
        );
        for (attempt, now) in [(1, 111), (2, 112)] {
            assert_eq!(
                state.begin_wake_attempt("message", AgentState::Waiting, now),
                Ok(WakeDisposition::AttemptGranted)
            );
            assert_eq!(
                state.complete_wake_attempt("message", attempt, false),
                Ok(WakeDisposition::RetryPending)
            );
        }
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 113),
            Ok(WakeDisposition::AttemptGranted)
        );
        assert_eq!(
            state.complete_wake_attempt("message", 3, false),
            Ok(WakeDisposition::Exhausted)
        );
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 114),
            Err(CoreError::WakeExhausted)
        );
        assert_eq!(state.messages[0].wake_attempt_count, 3);
    }

    #[test]
    fn resource_notice_is_durable_without_wake_subscription() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .send_resource_notice("message", "a", "b", ResourceNotice::Released, "resource")
            .unwrap();
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].wake_attempt_count, 0);
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 110),
            Ok(WakeDisposition::Skipped)
        );
        assert_eq!(state.messages[0].wake_attempt_count, 0);
    }

    #[test]
    fn message_ids_are_canonical_before_any_message_state_mutation() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .subscribe(
                "b",
                "direct",
                NotificationEvent::DirectMessage,
                None,
                200,
                100,
            )
            .unwrap();
        state
            .subscribe(
                "b",
                "deadline",
                NotificationEvent::Deadline,
                Some("timer".into()),
                200,
                100,
            )
            .unwrap();

        for invalid in [
            "",
            "-leading",
            "contains space",
            "contains/slash",
            "contains\nnewline",
            "contains\rcarriage-return",
            "contains\0nul",
            "contains\ttab",
            "contains\u{1b}escape",
            "contains\u{7f}delete",
            "non-ascii-é",
        ] {
            let before = state.clone();
            assert_eq!(
                state.send_resource_notice(invalid, "a", "b", ResourceNotice::Occupied, "resource"),
                Err(CoreError::InvalidMessageId),
                "invalid id: {invalid:?}"
            );
            assert_eq!(state, before, "invalid id mutated state: {invalid:?}");
        }

        let too_long = format!("a{}", "b".repeat(128));
        let before = state.clone();
        assert_eq!(
            state.send_resource_notice(too_long, "a", "b", ResourceNotice::Occupied, "resource"),
            Err(CoreError::InvalidMessageId)
        );
        assert_eq!(state, before);

        let before = state.clone();
        assert_eq!(
            state.begin_wake_attempt("bad\nSECOND_COMMAND", AgentState::Unknown, 110),
            Err(CoreError::InvalidMessageId)
        );
        assert_eq!(
            state.complete_wake_attempt("bad\nSECOND_COMMAND", 1, false),
            Err(CoreError::InvalidMessageId)
        );
        assert_eq!(
            state.recover_wake_attempt("bad\nSECOND_COMMAND", 110),
            Err(CoreError::InvalidMessageId)
        );
        assert_eq!(state, before);

        let before = state.clone();
        assert_eq!(
            state.publish_subscription_event(
                "bad\nSECOND_COMMAND",
                "deadline",
                NotificationEvent::Deadline,
                "timer",
                110,
            ),
            Err(CoreError::InvalidMessageId)
        );
        assert_eq!(state, before);

        state
            .send_resource_notice(
                "m1788111351758-4:retry_1.ok",
                "a",
                "b",
                ResourceNotice::Occupied,
                "resource",
            )
            .unwrap();
        assert_eq!(state.messages.len(), 1);
    }

    #[test]
    fn expired_wake_lease_recovers_without_retry_or_implicit_delivery() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .subscribe(
                "b",
                "sub",
                NotificationEvent::DirectMessage,
                None,
                200_000,
                100,
            )
            .unwrap();
        state
            .send_resource_notice("message", "a", "b", ResourceNotice::Occupied, "resource")
            .unwrap();
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 100_000),
            Ok(WakeDisposition::AttemptGranted)
        );
        assert_eq!(state.messages[0].wake_attempt_count, 1);
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 100_001),
            Err(CoreError::WakeAttemptInFlight)
        );
        assert_eq!(
            state.recover_wake_attempt("message", 160_000),
            Ok(WakeDisposition::LeaseRecovered)
        );
        assert_eq!(state.messages[0].wake_attempt_count, 1);
        assert!(!state.messages[0].wake_in_flight);
        assert!(!state.messages[0].delivered);
        assert_eq!(
            state.recover_wake_attempt("message", 160_001),
            Err(CoreError::InvalidWakeRecovery)
        );
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 160_001),
            Ok(WakeDisposition::AttemptGranted)
        );
    }

    #[test]
    fn recovering_third_expired_lease_exhausts_without_resetting_cap() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .subscribe(
                "b",
                "sub",
                NotificationEvent::DirectMessage,
                None,
                1_000_000,
                1,
            )
            .unwrap();
        state
            .send_resource_notice("message", "a", "b", ResourceNotice::Occupied, "resource")
            .unwrap();
        for (attempt, now) in [(1, 1_000), (2, 61_000)] {
            assert_eq!(
                state.begin_wake_attempt("message", AgentState::Waiting, now),
                Ok(WakeDisposition::AttemptGranted)
            );
            assert_eq!(
                state.complete_wake_attempt("message", attempt, false),
                Ok(WakeDisposition::RetryPending)
            );
        }
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 121_000),
            Ok(WakeDisposition::AttemptGranted)
        );
        assert_eq!(
            state.recover_wake_attempt("message", 181_000),
            Ok(WakeDisposition::Exhausted)
        );
        assert_eq!(state.messages[0].wake_attempt_count, 3);
        assert_eq!(
            state.begin_wake_attempt("message", AgentState::Waiting, 181_001),
            Err(CoreError::WakeExhausted)
        );
    }

    #[test]
    fn journal_replay_is_contiguous_idempotent_and_deterministic() {
        let entries = vec![
            JournalEntry {
                sequence: 1,
                command_id: "command-1".into(),
                command: CoreCommand::Register {
                    identity: peer("a", "session-a"),
                },
            },
            JournalEntry {
                sequence: 2,
                command_id: "command-2".into(),
                command: CoreCommand::RegisterTask {
                    actor: "a".into(),
                    task_id: "task".into(),
                    feature_id: "feature".into(),
                    resource_id: "resource".into(),
                },
            },
        ];
        let first = CoreState::replay(&entries).unwrap();
        let second = CoreState::replay(&entries).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.snapshot_sha256().unwrap(),
            second.snapshot_sha256().unwrap()
        );
        let mut gap = entries.clone();
        gap[1].sequence = 3;
        assert_eq!(CoreState::replay(&gap), Err(CoreError::JournalGap));
        let mut duplicate = entries;
        duplicate[1].command_id = "command-1".into();
        assert_eq!(
            CoreState::replay(&duplicate),
            Err(CoreError::DuplicateCommand)
        );
    }

    #[test]
    fn legacy_beta_migration_freezes_verifies_and_resumes_without_roles() {
        let raw = r#"{"identities":[{"id":"a","session_id":"session-a","role":"Master"}],"tasks":[{"id":"task","state":"Reviewing","owner":"a"}]}"#;
        let plan = plan_legacy_beta_migration(raw).unwrap();
        assert!(plan.issues.is_empty());
        assert!(!serde_json::to_string(&plan).unwrap().contains("role"));
        let mut state = CoreState::default();
        state.apply(&CoreCommand::ApplyMigration { plan }).unwrap();
        assert_eq!(
            state.migration.as_ref().unwrap().phase,
            MigrationPhase::Frozen
        );
        assert_eq!(state.tasks[0].state, TaskState::Reviewed);
        assert_eq!(
            state.apply(&CoreCommand::TransitionTask {
                actor: "a".into(),
                task_id: "task".into(),
                state: TaskState::Delivered
            }),
            Err(CoreError::InvalidMigrationState)
        );
        state
            .apply(&CoreCommand::Register {
                identity: peer("a", "session-a"),
            })
            .unwrap();
        state.apply(&CoreCommand::VerifyMigration).unwrap();
        state.apply(&CoreCommand::ResumeMigration).unwrap();
        state
            .apply(&CoreCommand::TransitionTask {
                actor: "a".into(),
                task_id: "task".into(),
                state: TaskState::Delivered,
            })
            .unwrap();
    }

    #[test]
    fn legacy_available_task_blocks_migration_without_inventing_owner() {
        let raw = r#"{"identities":[{"id":"a","session_id":"session-a","role":"Master"}],"tasks":[{"id":"task","state":"Available","owner":null}]}"#;
        let plan = plan_legacy_beta_migration(raw).unwrap();
        assert_eq!(plan.issues, vec!["task task has no owner"]);
        assert_eq!(plan.continuity_sha256, None);
        let mut state = CoreState::default();
        assert_eq!(
            state.apply(&CoreCommand::ApplyMigration { plan }),
            Err(CoreError::MigrationBlocked)
        );
        assert_eq!(state, CoreState::default());
    }

    #[test]
    fn exact_subscription_and_wait_deadlines_terminate_without_retry_loop() {
        let mut state = CoreState::default();
        state.register(peer("a", "session-a")).unwrap();
        state.register(peer("b", "session-b")).unwrap();
        state
            .register_task("a", "task-a", "feature", "resource")
            .unwrap();
        state
            .register_task("b", "task-b", "feature", "resource")
            .unwrap();
        state.wait_task("a", "task-a", "task-b", 150, 100).unwrap();
        state.expire_waits(150);
        assert_eq!(state.tasks[0].state, TaskState::Blocked);
        assert!(state.tasks[0].wait.is_none());
        assert!(state.messages.is_empty());

        state
            .subscribe(
                "a",
                "deadline",
                NotificationEvent::Deadline,
                Some("timer".into()),
                200,
                100,
            )
            .unwrap();
        assert_eq!(
            state.publish_subscription_event(
                "wrong",
                "deadline",
                NotificationEvent::Deadline,
                "other",
                110
            ),
            Err(CoreError::InvalidSubscription)
        );
        state
            .publish_subscription_event(
                "deadline-message",
                "deadline",
                NotificationEvent::Deadline,
                "timer",
                110,
            )
            .unwrap();
        assert_eq!(
            state.begin_wake_attempt("deadline-message", AgentState::Waiting, 111),
            Ok(WakeDisposition::AttemptGranted)
        );
        assert_eq!(
            state.complete_wake_attempt("deadline-message", 1, true),
            Ok(WakeDisposition::Delivered)
        );
        assert_eq!(
            state
                .subscriptions
                .iter()
                .find(|subscription| subscription.id == "deadline")
                .unwrap()
                .state,
            SubscriptionState::Consumed
        );

        state
            .subscribe(
                "a",
                "async",
                NotificationEvent::AsyncResult,
                Some("operation".into()),
                120,
                100,
            )
            .unwrap();
        state.expire_subscriptions(120);
        assert_eq!(
            state
                .subscriptions
                .iter()
                .find(|subscription| subscription.id == "async")
                .unwrap()
                .state,
            SubscriptionState::Expired
        );
        state
            .subscribe(
                "a",
                "cancel",
                NotificationEvent::ResourceReleased,
                Some("resource".into()),
                200,
                100,
            )
            .unwrap();
        state.unsubscribe("a", "cancel").unwrap();
        assert_eq!(
            state
                .subscriptions
                .iter()
                .find(|subscription| subscription.id == "cancel")
                .unwrap()
                .state,
            SubscriptionState::Cancelled
        );
    }
}
