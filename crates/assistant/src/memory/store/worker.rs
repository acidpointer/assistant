//! Blocking connection ownership with bounded priority queues. Ordinary work has
//! 56 permits; critical work has seven and product finalization has one reserved
//! permit. The single active operation owns the durable finalization entitlement.
//! Queue mutation and closing share a lock, so shutdown drains exactly admitted
//! actions. Dropping a caller never removes an action. A native/SQL action is
//! not preempted; priority applies between complete commands.

use super::database::Database;
use crate::memory::{MemoryError, MemoryResult};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use tokio::sync::{Semaphore, oneshot};

type Action = Box<dyn FnOnce(&mut Database) + Send>;

#[derive(Debug, Clone, Copy)]
pub(super) enum WorkerRole {
    Writer,
    Reader,
}

#[derive(Default)]
struct Queues {
    ordinary: VecDeque<Action>,
    critical: VecDeque<Action>,
    settlement: VecDeque<Action>,
    closing: bool,
    abort: bool,
}

#[derive(Default)]
struct Queue {
    state: Mutex<Queues>,
    wake: Condvar,
}
impl Queue {
    fn take(&self) -> MemoryResult<Option<Action>> {
        let mut state = self.state.lock().map_err(|_| MemoryError::Unavailable)?;
        loop {
            if state.abort {
                return Ok(None);
            }
            if let Some(action) = state
                .settlement
                .pop_front()
                .or_else(|| state.critical.pop_front())
                .or_else(|| state.ordinary.pop_front())
            {
                return Ok(Some(action));
            }
            if state.closing {
                return Ok(None);
            }
            state = self
                .wake
                .wait(state)
                .map_err(|_| MemoryError::Unavailable)?;
        }
    }
}

pub(super) struct Worker {
    role: WorkerRole,
    queue: Arc<Queue>,
    pub(super) ordinary: Arc<Semaphore>,
    pub(super) critical: Arc<Semaphore>,
    settlement: Arc<Semaphore>,
    pub(super) blocked: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}
impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}
impl Worker {
    pub(super) fn start(
        mut database: Database,
        name: &str,
        role: WorkerRole,
    ) -> MemoryResult<(Self, thread::JoinHandle<()>)> {
        let queue = Arc::new(Queue::default());
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_queue = queue.clone();
        let worker_stopped = stopped.clone();
        let blocked = Arc::new(AtomicBool::new(false));
        let worker_blocked = blocked.clone();
        let thread = thread::Builder::new().name(name.into()).spawn(move || {
            let _termination = Termination {
                queue: worker_queue.clone(),
                stopped: worker_stopped,
                blocked: worker_blocked,
            };
            while let Ok(Some(action)) = worker_queue.take() {
                action(&mut database);
            }
        })?;
        Ok((
            Self {
                role,
                queue,
                ordinary: Arc::new(Semaphore::new(56)),
                critical: Arc::new(Semaphore::new(7)),
                settlement: Arc::new(Semaphore::new(1)),
                blocked,
                stopped,
            },
            thread,
        ))
    }

    pub(super) async fn call<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Database) -> MemoryResult<T> + Send + 'static,
    ) -> MemoryResult<T> {
        if self.blocked.load(Ordering::Acquire) {
            return Err(MemoryError::Unavailable);
        }
        self.enqueue(action, Priority::Ordinary).await
    }

    pub(super) async fn critical<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Database) -> MemoryResult<T> + Send + 'static,
    ) -> MemoryResult<T> {
        self.enqueue(action, Priority::Critical).await
    }

    pub(super) async fn settle<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Database) -> MemoryResult<T> + Send + 'static,
    ) -> MemoryResult<T> {
        self.enqueue(action, Priority::Settlement).await
    }

    async fn enqueue<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Database) -> MemoryResult<T> + Send + 'static,
        priority: Priority,
    ) -> MemoryResult<T> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(MemoryError::Unavailable);
        }
        let permits = match priority {
            Priority::Ordinary => &self.ordinary,
            Priority::Critical => &self.critical,
            Priority::Settlement => &self.settlement,
        };
        let permit = permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| MemoryError::Capacity)?;
        let (tx, rx) = oneshot::channel();
        let allowance = match self.role {
            WorkerRole::Writer => std::time::Duration::from_secs(1),
            WorkerRole::Reader => std::time::Duration::from_millis(250),
        };
        let blocked = self.blocked.clone();
        let role = self.role;
        {
            let mut state = self
                .queue
                .state
                .lock()
                .map_err(|_| MemoryError::Unavailable)?;
            if state.closing || state.abort {
                return Err(MemoryError::Unavailable);
            }
            let action: Action = Box::new(move |db| {
                let result = db.arm_deadline(allowance).and_then(|()| action(db));
                if matches!(role, WorkerRole::Writer)
                    && matches!(
                        &result,
                        Err(MemoryError::Storage(_)
                            | MemoryError::CorruptStore
                            | MemoryError::Indeterminate
                            | MemoryError::StorageFull)
                    )
                {
                    blocked.store(true, Ordering::Release);
                }
                let _receiver_gone = tx.send(result);
                drop(permit);
            });
            match priority {
                Priority::Ordinary => state.ordinary.push_back(action),
                Priority::Critical => state.critical.push_back(action),
                Priority::Settlement => state.settlement.push_back(action),
            }
            self.queue.wake.notify_one();
        }
        rx.await.map_err(|_| match self.role {
            WorkerRole::Writer => MemoryError::Indeterminate,
            WorkerRole::Reader => MemoryError::Unavailable,
        })?
    }

    #[cfg(test)]
    pub(super) fn test_settlement_pending(&self) -> bool {
        self.settlement.available_permits() == 0
    }

    pub(super) fn close(&self) -> MemoryResult<()> {
        let mut state = self
            .queue
            .state
            .lock()
            .map_err(|_| MemoryError::Unavailable)?;
        state.closing = true;
        self.queue.wake.notify_one();
        Ok(())
    }

    pub(super) fn abort(&self) {
        if let Ok(mut state) = self.queue.state.lock() {
            state.abort = true;
            state.ordinary.clear();
            state.critical.clear();
            state.settlement.clear();
            self.queue.wake.notify_one();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.abort();
    }
}

/// Disconnect every pending reply even on unwinding or an unexpected worker exit.
struct Termination {
    queue: Arc<Queue>,
    stopped: Arc<AtomicBool>,
    blocked: Arc<AtomicBool>,
}
impl Drop for Termination {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.blocked.store(true, Ordering::Release);
        // Poison recovery is cleanup only; no action runs against a poisoned queue.
        let mut state = match self.queue.state.lock() {
            Ok(state) => state,
            Err(poison) => poison.into_inner(),
        };
        state.abort = true;
        state.ordinary.clear();
        state.critical.clear();
        state.settlement.clear();
        self.queue.wake.notify_all();
    }
}

#[derive(Clone, Copy)]
enum Priority {
    Ordinary,
    Critical,
    Settlement,
}
