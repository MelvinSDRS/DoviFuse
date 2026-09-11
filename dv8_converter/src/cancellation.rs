use std::sync::atomic::{AtomicBool, Ordering};

static CANCELLED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn handle_signal(_: i32) {
    CANCELLED.store(true, Ordering::SeqCst);
}

pub(crate) fn install() {
    #[cfg(unix)]
    unsafe {
        unsafe extern "C" {
            fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
        }
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        signal(SIGINT, handle_signal);
        signal(SIGTERM, handle_signal);
    }
}

pub(crate) fn requested() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}
