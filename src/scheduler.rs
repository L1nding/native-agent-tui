use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Draft,
    Queued,
    Ready,
    Running,
    WaitingChildren,
    WaitingApproval,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
    Paused,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerCommand {
    Pause(TaskId),
    Resume(TaskId),
    Cancel(TaskId),
    Retry(TaskId),
    Reprioritize { task_id: TaskId, priority: i32 },
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub id: TaskId,
    pub state: TaskState,
    pub dependencies: BTreeSet<TaskId>,
    pub priority: i32,
}

#[derive(Debug, Default)]
pub struct Scheduler {
    tasks: BTreeMap<TaskId, TaskRecord>,
}

impl Scheduler {
    pub fn add_task(
        &mut self,
        id: TaskId,
        dependencies: impl IntoIterator<Item = TaskId>,
        priority: i32,
    ) -> bool {
        if self.tasks.contains_key(&id) {
            return false;
        }
        let dependencies: BTreeSet<_> = dependencies.into_iter().collect();
        let state = if dependencies.is_empty() {
            TaskState::Ready
        } else {
            TaskState::Queued
        };
        self.tasks.insert(
            id,
            TaskRecord {
                id,
                state,
                dependencies,
                priority,
            },
        );
        true
    }

    pub fn task(&self, id: TaskId) -> Option<&TaskRecord> {
        self.tasks.get(&id)
    }

    pub fn ready_tasks(&self) -> Vec<TaskId> {
        let mut ready: Vec<_> = self
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Ready)
            .map(|task| (task.priority, task.id))
            .collect();
        ready.sort_by(|left, right| right.cmp(left));
        ready.into_iter().map(|(_, id)| id).collect()
    }

    pub fn start(&mut self, id: TaskId) -> bool {
        let Some(task) = self.tasks.get_mut(&id) else {
            return false;
        };
        if task.state != TaskState::Ready {
            return false;
        }
        task.state = TaskState::Running;
        true
    }

    pub fn complete(&mut self, id: TaskId, success: bool) -> bool {
        let Some(task) = self.tasks.get_mut(&id) else {
            return false;
        };
        if task.state != TaskState::Running {
            return false;
        }
        task.state = if success {
            TaskState::Succeeded
        } else {
            TaskState::Failed
        };
        self.refresh_ready_tasks();
        true
    }

    fn refresh_ready_tasks(&mut self) {
        let succeeded: BTreeSet<_> = self
            .tasks
            .values()
            .filter(|task| task.state == TaskState::Succeeded)
            .map(|task| task.id)
            .collect();
        for task in self.tasks.values_mut() {
            if task.state == TaskState::Queued
                && task
                    .dependencies
                    .iter()
                    .all(|dependency| succeeded.contains(dependency))
            {
                task.state = TaskState::Ready;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Scheduler, TaskId, TaskState};

    #[test]
    fn dependencies_move_tasks_into_the_ready_queue() {
        let mut scheduler = Scheduler::default();
        assert!(scheduler.add_task(TaskId(1), [], 1));
        assert!(scheduler.add_task(TaskId(2), [TaskId(1)], 2));
        assert_eq!(scheduler.ready_tasks(), vec![TaskId(1)]);
        assert!(scheduler.start(TaskId(1)));
        assert!(scheduler.complete(TaskId(1), true));
        assert_eq!(scheduler.task(TaskId(2)).unwrap().state, TaskState::Ready);
    }
}
