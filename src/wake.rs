use core::ptr::NonNull;
use futures::task::{UnsafeWake, Waker};

pub struct DumbWake {
    _opaque: (),
}

static DUMBWAKE: DumbWake = DumbWake { _opaque: () };

impl DumbWake {
    pub fn get() -> NonNull<DumbWake> {
        NonNull::from(&DUMBWAKE)
    }
}

unsafe impl UnsafeWake for DumbWake {
    unsafe fn clone_raw(&self) -> Waker {
        Waker::new(DumbWake::get())
    }

    unsafe fn drop_raw(&self) {}

    unsafe fn wake(&self) {}
}
