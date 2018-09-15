//! A barebones executor for futures that can run in a zero-allocation environment.
//! The current implementation runs futures to completion the moment they're run.
//! This behavior, while in the theme of the name, is likely to change.
//! There are also plans to provide an executor that depends solely on the `alloc` crate,
//! As well as versions to use in multithreaded contexts such as multiproccessing operating systems.

#![no_std]
#![feature(futures_api, pin, const_fn, nll, extern_prelude)]
#![warn(missing_docs, missing_debug_implementations)]

extern crate futures;
extern crate spin;

#[cfg(test)]
#[macro_use]
extern crate std;

use futures::prelude::*;

use core::marker::Unpin;
use core::pin::PinMut;
use core::sync::atomic;
use future::FutureObj;
use task::{Context, LocalWaker, Spawn, SpawnObjError};

pub mod cached;
mod wake;

use wake::DumbWake;

/// The primary component of the crate.
/// Can be used simply with `let result = DumbExec::new().run(future)`
/// or can be shared for multiple tasks.
/// Warning: runs tasks sequentially with no true asynchronicity!
#[derive(Debug, Default)]
pub struct DumbExec {
    _opaque: (),
}

struct ChildExec<'a> {
    parent: &'a DumbExec,
}

impl DumbExec {
    /// Allocates a new `DumbExec` for use.
    pub fn new() -> DumbExec {
        DumbExec { _opaque: () }
    }

    /// Runs the provided future on this executor. Blocks until the future is complete and returns its result.
    pub fn run<F: Future + Unpin>(&self, future: F) -> F::Output {
        let mut future = future;
        let mut future_pin = PinMut::new(&mut future);
        let local_waker = unsafe { LocalWaker::new(DumbWake::get()) };
        let mut child = ChildExec { parent: &self };
        let mut ctx = Context::new(&local_waker, &mut child);
        loop {
            match Future::poll(future_pin.reborrow(), &mut ctx) {
                Poll::Pending => atomic::spin_loop_hint(),
                Poll::Ready(v) => return v,
            }
        }
    }
}

impl Spawn for DumbExec {
    #[inline]
    fn spawn_obj(&mut self, future: FutureObj<'static, ()>) -> Result<(), SpawnObjError> {
        self.run(future);
        Ok(())
    }
}

impl<'a> Spawn for ChildExec<'a> {
    #[inline]
    fn spawn_obj(&mut self, future: FutureObj<'static, ()>) -> Result<(), SpawnObjError> {
        self.parent.run(future);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::DumbExec;
    use futures::prelude::*;

    #[test]
    fn it_works() {
        let exec = DumbExec::new();
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
