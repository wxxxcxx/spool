//! Read-only private API boundaries for independent native inspection.

use super::skylight::{
    SLSCopyManagedDisplaySpaces, SLSCopyWindowsWithOptionsAndTags, SLSMainConnectionID,
};
use objc2_core_foundation::{CFArray, CFNumber, CFNumberType, CFRetained, CFType};
use std::ptr::NonNull;

pub(crate) fn spaces() -> Option<CFRetained<CFArray<CFType>>> {
    let pointer = unsafe { SLSCopyManagedDisplaySpaces(SLSMainConnectionID()) };
    NonNull::new(pointer).map(|pointer| unsafe { CFRetained::from_raw(pointer.cast()) })
}
pub(crate) fn space_windows(space: u64) -> Result<Vec<u64>, String> {
    let ids = crate::util::create_array(&[space], CFNumberType::SInt64Type)
        .map_err(|error| error.to_string())?;
    let mut set_tags = 0;
    let mut clear_tags = 0;
    let pointer = unsafe {
        SLSCopyWindowsWithOptionsAndTags(
            SLSMainConnectionID(),
            0,
            &raw const *ids,
            0x7,
            &mut set_tags,
            &mut clear_tags,
        )
    };
    let pointer = NonNull::new(pointer).ok_or("Space window inventory unavailable")?;
    let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::from_raw(pointer.cast()) };
    array
        .iter()
        .map(|value| {
            value
                .downcast_ref::<CFNumber>()
                .and_then(CFNumber::as_i64)
                .and_then(|id| u64::try_from(id).ok())
                .ok_or_else(|| "unexpected Space window ID type".into())
        })
        .collect()
}

pub(crate) fn window_id(element: accessibility_sys::AXUIElementRef) -> Result<Option<u64>, i32> {
    let mut id = 0;
    let error = unsafe { super::skylight::_AXUIElementGetWindow(element, &mut id) };
    if error != 0 {
        return Err(error);
    }
    Ok(u64::try_from(id).ok().filter(|id| *id > 0))
}

/// Independent forward membership evidence; never substitute a guessed display.
/// Source ABI: <https://github.com/koekeishiya/yabai/blob/master/src/window.c>
pub(crate) fn window_spaces(window: u64) -> Result<Vec<u64>, String> {
    let window = i32::try_from(window).map_err(|error| error.to_string())?;
    let ids = crate::util::create_array(&[window], CFNumberType::SInt32Type)
        .map_err(|error| error.to_string())?;
    let pointer = unsafe {
        super::skylight::SLSCopySpacesForWindows(
            SLSMainConnectionID(),
            0x7,
            (&raw const *ids).cast(),
        )
    };
    let pointer = NonNull::new(pointer).ok_or("window Space inventory unavailable")?;
    let array: CFRetained<CFArray<CFType>> = unsafe { CFRetained::from_raw(pointer.cast()) };
    array
        .iter()
        .map(|value| {
            value
                .downcast_ref::<CFNumber>()
                .and_then(CFNumber::as_i64)
                .and_then(|id| u64::try_from(id).ok())
                .ok_or_else(|| "unexpected Space ID type".into())
        })
        .collect()
}
