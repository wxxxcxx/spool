//! What a handler can ask the main thread for, and the cache in front of it.
//!
//! Every read here is a round trip: the worker sends a request carrying a
//! reply channel and *awaits* it, while the main thread answers from the ECS
//! in `serve_lua_queries`. Awaiting rather than blocking lets handlers
//! overlap — a handler parked on a read is not holding the interpreter.
//!
//! Two invariants:
//!
//! * **The caches belong to an explicit input batch.** Handlers from one
//!   worker message share reads; a later message gets a new snapshot even
//!   while earlier handlers are suspended. The current batch is installed
//!   only while polling its handler, never across a suspension point.
//! * **No borrow may be held across an await.** These are `RefCell`s on a
//!   single thread, so a borrow spanning a suspension point is not a wait —
//!   it is a `BorrowMutError` panic the moment another dispatch touches the
//!   same cell. Every read below takes what it needs, drops the borrow,
//!   *then* awaits.

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_channel::{Sender, bounded};

use super::worker::{Shared, StoreRequest, WorldRequest};
use crate::ecs::state::SpoolQueryState;
use spool_shared_types::script_state::{ScriptState, ScriptStateWrite, WriteOutcome};
use spool_shared_types::windowset::WindowSet;

/// What the worker reported when the main thread has already gone away. Surfaces
/// inside the handler as an ordinary error, so the script unwinds normally
/// instead of the task hanging on a reply that can never come.
const SHUTTING_DOWN: &str = "the window manager is shutting down";

/// What a script is told when it reaches for the world from outside a handler.
const NO_DISPATCH: &str = "only available inside a spool.on handler or a spool.bind callback";

/// The main thread, as a handler sees it: ask, and await the answer.
///
/// Cheap to clone — it is a channel sender and a shared stamp — because every
/// dispatch task needs its own handle.
#[derive(Clone)]
pub(super) struct WorldAccess {
    /// Two channels rather than one, because two *systems* serve them: see
    /// [`WorldRequest`] for what that buys.
    world: Sender<WorldRequest>,
    store: Sender<StoreRequest>,
    revision: Arc<AtomicU64>,
}

/// Sends one request down `channel` and waits for its reply.
///
/// Either half failing means the main thread has dropped its end, which is how a
/// handler parked here is woken at shutdown rather than waiting for a reply that
/// is never coming.
async fn ask<R, T>(channel: &Sender<R>, request: impl FnOnce(Sender<T>) -> R) -> Result<T, String> {
    let (reply, answer) = bounded(1);
    channel
        .send(request(reply))
        .await
        .map_err(|_| SHUTTING_DOWN.to_string())?;
    answer.recv().await.map_err(|_| SHUTTING_DOWN.to_string())
}

impl WorldAccess {
    pub(super) fn new(
        world: Sender<WorldRequest>,
        store: Sender<StoreRequest>,
        revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            world,
            store,
            revision,
        }
    }

    async fn state(&self) -> Shared<SpoolQueryState> {
        ask(&self.world, |reply| WorldRequest::State { reply })
            .await
            .unwrap_or_else(Err)
    }

    async fn window_set(&self) -> Shared<WindowSet> {
        ask(&self.world, |reply| WorldRequest::WindowSet { reply })
            .await
            .unwrap_or_else(Err)
    }

    async fn script_state(&self) -> Result<ScriptState, String> {
        ask(&self.store, |reply| StoreRequest::Read { reply })
            .await
            .unwrap_or_else(Err)
    }

    async fn write_script_state(&self, write: &ScriptStateWrite) -> Result<WriteOutcome, String> {
        ask(&self.store, |reply| StoreRequest::Write {
            write: write.clone(),
            reply,
        })
        .await
        .unwrap_or_else(Err)
    }
}

/// One read of the world that several dispatches may want at once.
///
/// The first caller makes the round trip; anyone who asks while it's still
/// out queues behind it and is handed the same answer when it lands, rather
/// than each sending a request of its own.
struct SharedRead<T> {
    cached: RefCell<Option<Arc<T>>>,
    /// `Some` while a read is out, holding whoever is waiting on it.
    waiting: RefCell<Option<Vec<Sender<Shared<T>>>>>,
}

impl<T> SharedRead<T> {
    fn new() -> Self {
        Self {
            cached: RefCell::new(None),
            waiting: RefCell::new(None),
        }
    }

    /// The value, reading it through `read` if this is the first ask.
    async fn get<F>(&self, read: impl FnOnce() -> F) -> Shared<T>
    where
        F: Future<Output = Shared<T>>,
    {
        // Every borrow here is taken, used, and dropped before an `await`: a
        // `RefCell` borrow spanning a suspension point panics rather than waits.
        let cached = self.cached.borrow().clone();
        if let Some(cached) = cached {
            return Ok(cached);
        }

        let joined = {
            let mut waiting = self.waiting.borrow_mut();
            if let Some(queue) = waiting.as_mut() {
                let (tell, told) = bounded(1);
                queue.push(tell);
                Some(told)
            } else {
                // Nobody is reading, so this caller does it.
                *waiting = Some(Vec::new());
                None
            }
        };
        if let Some(told) = joined {
            return told
                .recv()
                .await
                .unwrap_or_else(|_| Err(SHUTTING_DOWN.to_string()));
        }

        let answer = read().await;
        if let Ok(value) = &answer {
            *self.cached.borrow_mut() = Some(Arc::clone(value));
        }
        // Taken, not borrowed, across the sends: a woken waiter may ask again
        // before this returns.
        let queued = self.waiting.borrow_mut().take().unwrap_or_default();
        for waiter in queued {
            let _ = waiter.try_send(answer.clone());
        }
        answer
    }
}

/// Point-in-time reads shared only by handlers from the same input message.
pub(super) struct DispatchBatch {
    state: SharedRead<SpoolQueryState>,
    window_set: SharedRead<WindowSet>,
}

impl DispatchBatch {
    pub(super) fn new() -> Rc<Self> {
        Rc::new(Self {
            state: SharedRead::new(),
            window_set: SharedRead::new(),
        })
    }
}

/// World access shared by interpreters, with a poll-scoped batch for API calls.
pub(super) struct DispatchWorld {
    access: WorldAccess,
    current: RefCell<Option<Rc<DispatchBatch>>>,
    /// Unlike the other two caches, this survives the batch — the store only
    /// changes on a write, tracked by the revision stamp.
    script_state: RefCell<Option<(u64, ScriptState)>>,
}

impl DispatchWorld {
    pub(super) fn new(access: WorldAccess) -> Rc<Self> {
        Rc::new(Self {
            access,
            current: RefCell::new(None),
            script_state: RefCell::new(None),
        })
    }

    /// Installs task-local access for each poll, restoring it before another
    /// handler or a reload can execute on the same interpreter thread.
    pub(super) async fn with_batch<T>(
        &self,
        batch: &Rc<DispatchBatch>,
        future: impl Future<Output = T>,
    ) -> T {
        let mut future = pin!(future);
        poll_fn(|cx| {
            let previous = self.current.replace(Some(Rc::clone(batch)));
            let _restore = scopeguard::guard(previous, |previous| {
                self.current.replace(previous);
            });
            future.as_mut().poll(cx)
        })
        .await
    }

    fn batch(&self, call: &str) -> Result<Rc<DispatchBatch>, String> {
        self.current
            .borrow()
            .clone()
            .ok_or_else(|| format!("{call} is {NO_DISPATCH}"))
    }

    /// The query documents, read once per batch however many handlers ask.
    pub(super) async fn query_state(&self) -> Result<Arc<SpoolQueryState>, String> {
        let batch = self.batch("spool.query")?;
        batch.state.get(|| self.access.state()).await
    }

    /// The layout tree, read once per batch. Handlers each transform their own
    /// copy, so this hands out the shared read and they clone from it.
    pub(super) async fn layout(&self) -> Result<Arc<WindowSet>, String> {
        let batch = self.batch("the window set")?;
        batch.window_set.get(|| self.access.window_set()).await
    }

    /// The script state store, re-read whenever the revision says the cached
    /// copy is stale. The stamp is taken *before* the read, so a write landing
    /// mid-read leaves the copy marked older than it is — re-read needlessly
    /// next time, which is the harmless direction to be wrong in.
    pub(super) async fn script_state(&self) -> Result<ScriptState, String> {
        self.batch("spool.state")?;
        let revision = self.access.revision.load(Ordering::Acquire);
        let cached = self.script_state.borrow().clone();
        match cached {
            Some((stamp, state)) if stamp == revision => Ok(state),
            _ => {
                let fresh = self.access.script_state().await?;
                *self.script_state.borrow_mut() = Some((revision, fresh.clone()));
                Ok(fresh)
            }
        }
    }

    /// Applies one write and reports what became of it. Unlike a command, this
    /// waits for the result: `spool.state.mutate` needs to know whether it
    /// was overtaken while it's still there to retry.
    pub(super) async fn write_script_state(
        &self,
        write: &ScriptStateWrite,
    ) -> Result<WriteOutcome, String> {
        self.batch("spool.state")?;
        self.access.write_script_state(write).await
    }
}

#[cfg(test)]
mod tests {
    use async_channel::unbounded;
    use futures_lite::future::{block_on, poll_once};

    use super::*;

    #[test]
    fn a_suspended_or_cancelled_dispatch_does_not_expose_top_level_world_access() {
        let (world_tx, requests) = unbounded();
        let (store_tx, _store_rx) = unbounded();
        let world = DispatchWorld::new(WorldAccess::new(
            world_tx,
            store_tx,
            Arc::new(AtomicU64::new(0)),
        ));
        let batch = DispatchBatch::new();
        let mut pending = Box::pin(world.with_batch(&batch, world.query_state()));
        assert!(block_on(poll_once(pending.as_mut())).is_none());
        assert_eq!(requests.len(), 1);

        assert!(
            block_on(world.query_state())
                .unwrap_err()
                .contains(NO_DISPATCH)
        );
        assert!(
            block_on(world.script_state())
                .unwrap_err()
                .contains(NO_DISPATCH)
        );
        drop(pending);
        assert!(block_on(world.layout()).unwrap_err().contains(NO_DISPATCH));
        assert_eq!(requests.len(), 1, "top-level calls cannot enqueue reads");
    }
}
