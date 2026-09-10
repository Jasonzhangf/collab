use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MasterWakeSignal {
    GoalDue { revision: u64 },
    WorkerIdle { worker_id: String },
    MasterIdle { worker_id: String },
    WorkerUnresponsive { worker_id: String },
    WorkerRecovered { worker_id: String },
    WorkerWorking { worker_id: String },
    TaskBlocked { task_id: String },
    TaskFreed { task_id: String },
    SubagentStatus { subagent_id: String },
    SubagentWorking { subagent_id: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MasterWakeAccumulator {
    pub generation: u64,
    pub goal_due: bool,
    #[serde(default)]
    pub idle_workers: Vec<String>,
    #[serde(default)]
    pub unresponsive_workers: Vec<String>,
    #[serde(default)]
    pub blocked_or_timed_out_tasks: Vec<String>,
    #[serde(default)]
    pub completed_or_freed_tasks: Vec<String>,
    #[serde(default)]
    pub highest_bug_revision: Option<u64>,
    #[serde(default)]
    pub active_goal_revision: Option<u64>,
    pub ready_authorized_work: bool,
    pub scheduling_decision_revision: u64,
    pub first_pending_ms: i64,
    pub last_updated_ms: i64,
    #[serde(default)]
    pub wake_hold: Option<WakeHold>,
    #[serde(default = "default_delivery_state")]
    pub delivery_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WakeHold {
    pub reason: String,
    pub held_by: String,
    pub held_ms: i64,
    pub until_ms: i64,
}

fn default_delivery_state() -> String {
    "clean".into()
}

fn add_unique(items: &mut Vec<String>, value: String) -> bool {
    if items.contains(&value) {
        return false;
    }
    items.push(value);
    items.sort();
    true
}

pub fn accumulate_master_wake(
    accumulator: &mut MasterWakeAccumulator,
    signal: &MasterWakeSignal,
    created_ms: i64,
) {
    let changed = match signal {
        MasterWakeSignal::GoalDue { revision } => {
            let changed = !accumulator.goal_due
                || accumulator
                    .active_goal_revision
                    .is_none_or(|current| *revision > current);
            accumulator.goal_due = true;
            if changed {
                accumulator.active_goal_revision = Some(*revision);
            }
            changed
        }
        MasterWakeSignal::WorkerIdle { worker_id } | MasterWakeSignal::MasterIdle { worker_id } => {
            let added = add_unique(&mut accumulator.idle_workers, worker_id.clone());
            let recovered = accumulator
                .unresponsive_workers
                .iter()
                .position(|id| id == worker_id)
                .map(|index| accumulator.unresponsive_workers.remove(index))
                .is_some();
            added || recovered
        }
        MasterWakeSignal::WorkerUnresponsive { worker_id } => {
            add_unique(&mut accumulator.unresponsive_workers, worker_id.clone())
        }
        MasterWakeSignal::WorkerRecovered { worker_id }
        | MasterWakeSignal::WorkerWorking { worker_id } => {
            let idle_removed = accumulator
                .idle_workers
                .iter()
                .position(|id| id == worker_id);
            if let Some(index) = idle_removed {
                accumulator.idle_workers.remove(index);
            }
            let unresponsive_removed = accumulator
                .unresponsive_workers
                .iter()
                .position(|id| id == worker_id);
            if let Some(index) = unresponsive_removed {
                accumulator.unresponsive_workers.remove(index);
            }
            idle_removed.is_some() || unresponsive_removed.is_some()
        }
        MasterWakeSignal::TaskBlocked { task_id } => {
            add_unique(&mut accumulator.blocked_or_timed_out_tasks, task_id.clone())
        }
        MasterWakeSignal::TaskFreed { task_id } => {
            add_unique(&mut accumulator.completed_or_freed_tasks, task_id.clone())
        }
        MasterWakeSignal::SubagentStatus { subagent_id } => add_unique(
            &mut accumulator.idle_workers,
            format!("subagent:{subagent_id}"),
        ),
        MasterWakeSignal::SubagentWorking { subagent_id } => {
            let id = format!("subagent:{subagent_id}");
            accumulator
                .idle_workers
                .iter()
                .position(|existing| existing == &id)
                .map(|index| accumulator.idle_workers.remove(index))
                .is_some()
        }
    };
    if changed {
        if accumulator.generation == 0 {
            accumulator.generation = 1;
            accumulator.first_pending_ms = created_ms;
        }
        accumulator.last_updated_ms = created_ms;
        accumulator.delivery_state = "pending".into();
    }
}

pub fn mark_master_wake_delivered(accumulator: &mut MasterWakeAccumulator) {
    accumulator.delivery_state = "notified_unconsumed".into();
}
