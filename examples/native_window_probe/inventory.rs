//! Read-only probe of the same AX publication collector used by the daemon.
//! Usage: cargo run --example ax-window-inventory -- <pid> <expected-window-id>
#![allow(dead_code)]

#[path = "../../src/errors.rs"]
mod errors;
#[path = "../../src/util.rs"]
mod util;

mod platform {
    pub type OSStatus = i32;
    pub type WinID = i32;
}

mod manager {
    use accessibility_sys::AXUIElementRef;
    use objc2_core_foundation::{CFString, CFType};
    unsafe extern "C" {
        pub fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: &CFString,
            value: &mut *mut CFType,
        ) -> i32;
        fn _AXUIElementGetWindow(element: AXUIElementRef, id: *mut i32) -> i32;
    }
    pub fn ax_window_id(element: AXUIElementRef) -> crate::errors::Result<i32> {
        let mut id = 0;
        if unsafe { _AXUIElementGetWindow(element, &raw mut id) } == 0 && id != 0 {
            Ok(id)
        } else {
            Err(crate::errors::Error::InvalidWindow)
        }
    }
}

fn main() {
    use util::AXUIAttributes as _;
    let args = std::env::args().collect::<Vec<_>>();
    assert_eq!(args.len(), 3, "expected PID and window ID");
    let pid: i32 = args[1].parse().expect("PID");
    let expected: i32 = args[2].parse().expect("window ID");
    let app = util::AXUIWrapper::from_retained(unsafe {
        accessibility_sys::AXUIElementCreateApplication(pid)
    })
    .expect("AX application");
    let windows = app.windows().expect("published inventory");
    let ids = windows
        .iter()
        .filter_map(|window| manager::ax_window_id(window.as_ptr()).ok())
        .collect::<Vec<_>>();
    println!("published window IDs: {ids:?}");
    assert!(
        ids.contains(&expected),
        "expected window missing from production collector"
    );
}
