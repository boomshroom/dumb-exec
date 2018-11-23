use crate::alloc::prelude::*;
use crate::alloc::collections::{VecDeque, BTreeMap};
use crate::alloc::sync::Arc;

use core::sync::atomic::{AtomicUsize, Ordering};
use core::pin::Pin;
use core::cell::RefCell;

use futures::future::{FutureObj, LocalFutureObj, Future, FutureExt};
use futures::task::{self, Spawn, LocalSpawn, UnsafeWake, Wake, SpawnError, Poll};

use spin::Mutex;

#[derive(PartialEq,Eq,PartialOrd,Ord,Debug,Copy,Clone,Hash)]
struct TaskId(usize);

impl From<usize> for TaskId {
    fn from(v: usize) -> TaskId {
        TaskId(v)
    }
}

#[derive(Debug)]
pub struct Executor<'f> {
    ready: Mutex<VecDeque<TaskId>>,
    tasks: Mutex<BTreeMap<TaskId, FutureObj<'f, ()>>>,
    counter: AtomicUsize,
}

struct Thread<'a, 'f> {
    parent: &'a Executor<'f>,
    tasks: Mutex<VecDeque<TaskId>>,
    local_tasks: BTreeMap<TaskId, LocalFutureObj<'f, ()>>,
}

struct Waker<'a> {
    id: TaskId,
    tasks: &'a Mutex<VecDeque<TaskId>>,
}

/// A future object that preserves its send value.
/// This is important for sharing futures across threads
/// while also supporting non-[Send] futures.
enum MaybeLocalFuture<'a, T> {
    Global(FutureObj<'a, T>),
    Local(LocalFutureObj<'a, T>),
}

impl <'a, 'f> Thread<'a, 'f> {
    fn run_once(&mut self) {
        let id = match self.tasks.lock().pop_front() {
            Some(id) => id,
            None => return,
        };

        if let Some(mut task) = self.local_tasks.remove(&id) {
            let a = Arc::new(LocalWaker{id, tasks: unsafe { &*(&self.tasks as *const _)}});
            let w = task::local_waker_ref_from_nonlocal(&a);
            match task.poll_unpin(&w) {
                Poll::Ready(()) => {}
                Poll::Pending => {
                    self.local_tasks.insert(id, task);
                },
            };
        } else {
            let mut task = self.parent.tasks.lock().remove(&id).expect("Nonexistent task queued.");
            let a = Arc::new(Waker{id, tasks: unsafe { &*(&self.parents.tasks as *const _)}});
            let w = task::local_waker_ref_from_nonlocal(&a);
            match task.poll_unpin(&w) {
                Poll::Ready(()) => {}
                Poll::Pending => {
                    self.parents.tasks.lock().insert(id, task.into());
                }
            };
        };
    }

    pub fn run(&mut self) {
        loop {
            self.run_once()
        }
    }
}

impl <'f> Executor<'f> {
    fn next_id(&self) -> TaskId {
        TaskId(self.counter.fetch_add(1, Ordering::Relaxed))
    }
}

impl Spawn for Executor<'static> {
    fn spawn_obj(&mut self, future: FutureObj<'static, ()>) -> Result<(), SpawnError> {
        let id = self.next_id();
        self.tasks.try_lock().ok_or_else(SpawnError::shutdown)?.insert(id, future);
        self.ready.try_lock().ok_or_else(|| {
            self.tasks.try_lock().map(|mut t| t.remove(&id));
            SpawnError::shutdown()
        })?.push_back(id);
        Ok(())
    }
}

impl <'a> Spawn for Thread<'a, 'static> {
    fn spawn_obj(&mut self, future: FutureObj<'static, ()>) -> Result<(), SpawnError> {
        let id = self.parent.next_id();
        self.parent.tasks.try_lock().ok_or_else(SpawnError::shutdown)?.insert(id, future);
        self.parent.ready.try_lock().ok_or_else(|| {
            self.parent.tasks.try_lock().map(|mut t| t.remove(&id));
            SpawnError::shutdown()
        })?.push_back(id);
        Ok(())
    }
}

impl <'a> LocalSpawn for Thread<'a, 'static> {
    fn spawn_local_obj(&mut self, future: LocalFutureObj<'static, ()>) -> Result<(), SpawnError> {
        let id = self.parent.next_id();
        self.local_tasks.insert(id, future);
        match self.tasks.try_lock() {
            None => {
                self.local_tasks.remove(&id);
                Err(SpawnError::shutdown())
            }
            Some(mut t) => {
                t.push_back(id);
                Ok(())
            }
        }
    }
}

impl <'a, T> Future for MaybeLocalFuture<'a, T> {
    type Output = T;
    fn poll(self: Pin<&mut Self>, lw: &task::LocalWaker) -> Poll<T> {
        match *Pin::get_mut(self) {
            MaybeLocalFuture::Global(ref mut f) => f.poll_unpin(lw),
            MaybeLocalFuture::Local(ref mut f) => f.poll_unpin(lw),
        }
    }
}

impl <'a> Wake for Waker<'a> {
    fn wake(s: &Arc<Self>) {
        s.tasks.lock().push_back(s.id);
    }
}