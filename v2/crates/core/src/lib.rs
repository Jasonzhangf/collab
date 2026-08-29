use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Role {
    Master,
    Worker,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum TaskState {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Identity {
    pub id: String,
    pub session_id: String,
    pub role: Role,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Task {
    pub id: String,
    pub state: TaskState,
    pub owner: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoreState {
    pub identities: Vec<Identity>,
    pub tasks: Vec<Task>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum CoreError {
    DuplicateIdentity,
    UnknownIdentity,
    UnknownTask,
    PermissionDenied,
    InvalidTransition,
    TaskAlreadyOwned,
}

impl CoreState {
    pub fn register(&mut self, identity: Identity) -> Result<(), CoreError> {
        if self.identities.iter().any(|existing| {
            existing.id == identity.id || existing.session_id == identity.session_id
        }) {
            return Err(CoreError::DuplicateIdentity);
        }
        if self.identities.is_empty() && identity.role != Role::Master {
            return Err(CoreError::PermissionDenied);
        }
        if !self.identities.is_empty() && identity.role != Role::Worker {
            return Err(CoreError::PermissionDenied);
        }
        self.identities.push(identity);
        Ok(())
    }

    pub fn create_task(&mut self, actor: &str, id: impl Into<String>) -> Result<(), CoreError> {
        let master = self
            .identities
            .iter()
            .find(|identity| identity.id == actor)
            .ok_or(CoreError::UnknownIdentity)?;
        if master.role != Role::Master {
            return Err(CoreError::PermissionDenied);
        }
        let id = id.into();
        if self.tasks.iter().any(|task| task.id == id) {
            return Err(CoreError::TaskAlreadyOwned);
        }
        self.tasks.push(Task {
            id,
            state: TaskState::Available,
            owner: None,
        });
        Ok(())
    }

    pub fn claim(&mut self, actor: &str, task_id: &str) -> Result<(), CoreError> {
        self.identities
            .iter()
            .find(|identity| identity.id == actor)
            .ok_or(CoreError::UnknownIdentity)?;
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or(CoreError::UnknownTask)?;
        if task.state != TaskState::Available || task.owner.is_some() {
            return Err(CoreError::TaskAlreadyOwned);
        }
        task.state = TaskState::Working;
        task.owner = Some(actor.to_owned());
        Ok(())
    }

    pub fn transition(
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
        let is_owner = task.owner.as_deref() == Some(actor);
        let is_master = self
            .identities
            .iter()
            .find(|identity| identity.id == actor)
            .map(|identity| identity.role == Role::Master)
            .unwrap_or(false);
        if !is_owner && !is_master {
            return Err(CoreError::PermissionDenied);
        }
        let valid = matches!(
            (task.state, next),
            (TaskState::Available, TaskState::Working)
                | (
                    TaskState::Working,
                    TaskState::Verifying | TaskState::Blocked | TaskState::Cancelled
                )
                | (
                    TaskState::Blocked,
                    TaskState::Working | TaskState::Cancelled
                )
                | (
                    TaskState::Verifying,
                    TaskState::Reviewing | TaskState::Working
                )
                | (
                    TaskState::Reviewing,
                    TaskState::Delivered | TaskState::Working
                )
                | (TaskState::Delivered, TaskState::Merged)
                | (TaskState::Merged, TaskState::Closed)
        );
        if !valid {
            return Err(CoreError::InvalidTransition);
        }
        task.state = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master() -> Identity {
        Identity {
            id: "m".into(),
            session_id: "session-1".into(),
            role: Role::Master,
        }
    }
    fn worker() -> Identity {
        Identity {
            id: "w".into(),
            session_id: "session-2".into(),
            role: Role::Worker,
        }
    }

    #[test]
    fn identity_and_role_are_unique() {
        let mut state = CoreState::default();
        assert_eq!(state.register(worker()), Err(CoreError::PermissionDenied));
        state.register(master()).unwrap();
        state.register(worker()).unwrap();
        assert_eq!(
            state.register(Identity {
                id: "w".into(),
                session_id: "session-3".into(),
                role: Role::Worker
            }),
            Err(CoreError::DuplicateIdentity)
        );
    }

    #[test]
    fn lifecycle_requires_owner_or_master_and_closes_explicitly() {
        let mut state = CoreState::default();
        state.register(master()).unwrap();
        state.register(worker()).unwrap();
        state.create_task("m", "t").unwrap();
        state.claim("w", "t").unwrap();
        assert_eq!(
            state.transition("m", "t", TaskState::Delivered),
            Err(CoreError::InvalidTransition)
        );
        for next in [
            TaskState::Verifying,
            TaskState::Reviewing,
            TaskState::Delivered,
            TaskState::Merged,
            TaskState::Closed,
        ] {
            state.transition("w", "t", next).unwrap();
        }
        assert_eq!(state.tasks[0].state, TaskState::Closed);
    }
}
