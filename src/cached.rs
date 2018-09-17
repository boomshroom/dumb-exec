//! Provides a smarter executor that can store futures in progress.
//! Storage is provided by the user as a preinitialized slice of `Task`s.
//! `Task::init` is provided to facilitate this initialization by giving
//! an infinite iterator of empty `Task`s.

use core::cell::{Cell, RefCell, Ref};
use core::iter;
use core::pin::PinMut;
use core::ops::Deref;
use core::sync::atomic::{self, AtomicBool, Ordering};
use futures::future::{Future, FutureObj, LocalFutureObj};
use futures::task::{Context, LocalWaker, Poll, Spawn, SpawnObjError, UnsafeWake, Waker};

/// An executor capable of running multiple `Future`s concurrently.
/// It operates on a fixed size buffer provided by the user.
/// Atempting to exceed this buffer will block and cause the queued
/// tasks to execute.
#[derive(Debug)]
pub struct CachedExec<T: AsRef<[Task]>> {
    storage: T,
    next: Cell<usize>,
    count: Cell<usize>,
}

#[derive(Debug)]
struct TaskInner {
    ready: Flag,
    task: RefCell<LocalFutureObj<'static, ()>>,
}

#[derive(Debug)]
struct Flag(AtomicBool);

/// An opaque Task type to store incomplete futures.
/// A slice or slice-like type of these must be provided
/// to `CachedExec`.
#[derive(Default, Debug)]
pub struct Task(RefCell<Option<TaskInner>>);

impl Task {
    /// Initialize an empty `Task`.
    pub const fn new() -> Task {
        Task(RefCell::new(None))
    }

    /// Provides an iterator of empty `Tasks`
    /// # Example
    /// ```
    /// let buffer = unsafe {
    ///     let mut buffer : [Task; 256] = core::mem::uninitialized();
    ///     buffer.iter_mut().zip(Task::init()).for_each(|(cell, empty)| core::ptr::write(cell as *mut _, empty));
    ///     buffer
    /// };
    /// ```
    /// Note that this unnessisary for arrays smaller than 32,
    /// as they can be initiallized with `Default::default()`.
    /// This can also be used with one of the various
    /// array initializer crates, or, if `alloc` is used,
    /// `collect`ed into a `Vec`.
    pub fn init() -> impl Iterator<Item = Task> {
        iter::repeat_with(Task::new)
    }
}

impl<T: AsRef<[Task]>> CachedExec<T> {
    /// Allocates a new `CachedExec` for use.
    /// Takes a pre-allocated storage to hold futures.
    pub fn new(cache: T) -> CachedExec<T> {
        CachedExec {
            storage: cache,
            next: Cell::new(0),
            count: Cell::new(0),
        }
    }

    /// Runs all stored futures. Blocks until all futures are complete.
    pub fn run(&self) {
        for (_, cell) in self.task_iter() {
            if self.count.get() == 0 {
                return;
            }
            match cell.0.try_borrow() {
                Err(_) => continue,
                Ok(task) => {
                    let task = match task.as_ref() {
                        Some(t) => t,
                        None => continue,
                    };
                    if self.run_once(task) {
                        drop(task);
                        cell.0.replace(None);
                        self.count.update(|v| v-1);
                    }
                }
            }
            atomic::spin_loop_hint();
        }
    }

    #[inline]
    fn task_iter(&self) -> impl Iterator<Item = (usize, &Task)> {
        self.storage
            .as_ref()
            .iter()
            .enumerate()
            .cycle()
            .skip(self.next.get())
    }

    #[inline]
    fn set_next(&self, current: usize) {
        self.next.set(current + 1 % self.storage.as_ref().len())
    }

    #[inline]
    fn get_inner(&self, idx: usize) -> Ref<TaskInner> {
        Ref::map(self.storage.as_ref()[idx].0.borrow(), |t| t.as_ref().unwrap())
    }

    fn spawn_raw(&self, future: LocalFutureObj<'static, ()>) -> Ref<TaskInner> {
        let new_task = Some(TaskInner {
            ready: Flag::true_(),
            task: RefCell::new(future),
        });

        for (idx, cell) in self.task_iter().take(self.storage.as_ref().len()) {
            match cell.0.try_borrow() {
                Err(_) => continue,
                Ok(task) => if task.is_none() {
                    drop(task);
                    cell.0.replace(new_task);
                    self.set_next(idx);
                    return self.get_inner(idx);
                },
            }
        }

        for (idx, cell) in self.task_iter() {
            match cell.0.try_borrow() {
                Err(_) => continue,
                Ok(task) => {
                    let task = task.as_ref().unwrap();
                    if self.run_once(task) {
                        drop(cell);
                        cell.0.replace(new_task);
                        
                        self.set_next(idx);
                        return self.get_inner(idx);
                    }
                }
            }
            atomic::spin_loop_hint();
        }
        unreachable!(); // Iterator in infinite.
    }

    fn run_once(&self, task: &TaskInner) -> bool {
        if task.ready.compare_and_swap(true, false, Ordering::Acquire) {
            let mut inner = match task.task.try_borrow_mut() {
                Err(_) => return false,
                Ok(inner) => inner,
            };
            let future = PinMut::new(&mut *inner);
            let mut child = self;
            let waker = unsafe { LocalWaker::new((&task.ready as &UnsafeWake).into()) };
            let mut ctx = Context::new(&waker, &mut child);
            match Future::poll(future, &mut ctx) {
                Poll::Pending => false,
                Poll::Ready(()) => true,
            }
        } else {
            false
        }
    }
}

impl<T: AsRef<[Task]>> Spawn for CachedExec<T> {
    #[inline]
    fn spawn_obj(&mut self, future: FutureObj<'static, ()>) -> Result<(), SpawnObjError> {
        self.spawn_raw(future.into());
        Ok(())
    }
}

impl<'a, T: AsRef<[Task]>> Spawn for &'a CachedExec<T> {
    fn spawn_obj(&mut self, future: FutureObj<'static, ()>) -> Result<(), SpawnObjError> {
        self.spawn_raw(future.into());
        Ok(())
    }
}

impl Flag {
    const fn true_() -> Flag { Flag (AtomicBool::new(true)) }
}

impl Deref for Flag {
    type Target = AtomicBool;
    fn deref(&self) -> &AtomicBool { &self.0 }
}

unsafe impl UnsafeWake for Flag {
    // Tasks are allocated by the caller,
    // so all a clone can do is copy the pointer.
    unsafe fn clone_raw(&self) -> Waker {
        Waker::new((self as &UnsafeWake).into())
    }

    unsafe fn drop_raw(&self) {}

    unsafe fn wake(&self) {
        self.store(true, Ordering::Release)
    }
}

/*
#[cfg(test)]
mod tests {
    use super::{CachedExec, Task};
    use futures::prelude::*;

    #[test]
    fn it_works() {
        let exec = CachedExec::new([Task::new(), Task::new()]);
        assert_eq!(exec.run(future::ready(4)), 4);

        assert_eq!(exec.run(future::lazy(|_| 4)), 4);

        assert_eq!(
            exec.run(
                future::lazy(|_| 2)
                    .join(future::lazy(|_| 2))
                    .map(|(x, y)| x + y)
            ),
            4
        );
    }
}
*/
