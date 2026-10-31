//! Restricts panic output only while invoking a synchronous builtin callback.
use std::{
    cell::Cell,
    panic::{self, AssertUnwindSafe},
    sync::Once,
};

thread_local! { static CALLBACK_DEPTH: Cell<u32> = const { Cell::new(0) }; }
static INSTALL: Once = Once::new();

pub(crate) fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, ()> {
    INSTALL.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if !CALLBACK_DEPTH
                .try_with(|depth| depth.get() != 0)
                .unwrap_or(false)
            {
                previous(info);
            }
        }));
    });
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            CALLBACK_DEPTH.with(|depth| depth.set(depth.get() - 1));
        }
    }
    CALLBACK_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let scope = Scope;
    let result = panic::catch_unwind(AssertUnwindSafe(f)).map_err(|_| ());
    drop(scope);
    result
}
