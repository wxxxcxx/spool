//! Native application inventory and selective top-level AX window reads.

use super::plan::Plan;
use super::value::{References, convert};
use super::{Emitter, array};
use crate::inspection::protocol::{Object, Outcome};
use accessibility_sys::{
    AXUIElementCopyActionNames, AXUIElementCopyAttributeNames, AXUIElementCopyAttributeValue,
    AXUIElementCopyParameterizedAttributeNames, AXUIElementCreateApplication, AXUIElementGetPid,
    AXUIElementIsAttributeSettable, AXUIElementRef, AXUIElementSetMessagingTimeout,
};
use objc2::MainThreadMarker;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFRetained, CFString, CFType};
use serde_json::{Value, json};
use spool_shared_types::inspection::{ReadMode, ReadRequest, Resource, Selection};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    ptr::NonNull,
    time::Duration,
};

fn pointer(element: &CFType) -> AXUIElementRef {
    NonNull::from(element).as_ptr().cast()
}
fn error(code: i32) -> Outcome {
    use accessibility_sys::{
        kAXErrorAPIDisabled as DISABLED, kAXErrorActionUnsupported as ACTION_UNSUPPORTED,
        kAXErrorAttributeUnsupported as UNSUPPORTED, kAXErrorCannotComplete as CANNOT_COMPLETE,
        kAXErrorInvalidUIElement as INVALID, kAXErrorNoValue as NO_VALUE,
        kAXErrorParameterizedAttributeUnsupported as PARAM_UNSUPPORTED,
    };
    let status = match code {
        UNSUPPORTED | PARAM_UNSUPPORTED | ACTION_UNSUPPORTED => "unsupported",
        NO_VALUE => "absent",
        DISABLED => "permission_denied",
        CANNOT_COMPLETE => "unavailable",
        INVALID => "vanished",
        _ => "failed",
    };
    Outcome::error(status, format!("AX error {code}"))
}
fn configure(element: &CFType, remaining: Duration) -> Result<(), Outcome> {
    let status = unsafe {
        AXUIElementSetMessagingTimeout(
            pointer(element),
            remaining.as_secs_f32().clamp(f32::EPSILON, 0.25),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(error(status))
    }
}
fn copy(
    element: &CFType,
    attribute: &str,
    remaining: Duration,
) -> Result<CFRetained<CFType>, Outcome> {
    configure(element, remaining)?;
    let name = CFString::from_str(attribute);
    let mut value = std::ptr::null();
    let status = unsafe {
        AXUIElementCopyAttributeValue(
            pointer(element),
            NonNull::from(&*name).as_ptr().cast(),
            &raw mut value,
        )
    };
    if status != 0 {
        return Err(error(status));
    }
    NonNull::new(value.cast_mut())
        .map(|value| unsafe { CFRetained::from_raw(value.cast()) })
        .ok_or_else(|| Outcome::error("unavailable", "AX success returned no value"))
}
fn names(element: &CFType, kind: &str, remaining: Duration) -> Result<CFRetained<CFType>, Outcome> {
    configure(element, remaining)?;
    let mut names = std::ptr::null();
    let status = unsafe {
        match kind {
            "actions" => AXUIElementCopyActionNames(pointer(element), &raw mut names),
            "parameterized-attributes" => {
                AXUIElementCopyParameterizedAttributeNames(pointer(element), &raw mut names)
            }
            _ => AXUIElementCopyAttributeNames(pointer(element), &raw mut names),
        }
    };
    if status != 0 {
        return Err(error(status));
    }
    NonNull::new(names.cast_mut())
        .map(|value| unsafe { CFRetained::from_raw(value.cast()) })
        .ok_or_else(|| Outcome::error("unavailable", "AX name inventory unavailable"))
}

fn pid(element: &CFType, remaining: Duration) -> Outcome {
    if let Err(error) = configure(element, remaining) {
        return error;
    }
    let mut pid = 0;
    let status = unsafe { AXUIElementGetPid(pointer(element), &raw mut pid) };
    if status == 0 && pid > 0 {
        Outcome::value("pid_t", json!(pid))
    } else if status != 0 {
        error(status)
    } else {
        Outcome::error("unavailable", "AX owner PID missing")
    }
}
fn id(element: &CFType, remaining: Duration) -> Outcome {
    if let Err(error) = configure(element, remaining) {
        return error;
    }
    match crate::manager::inspection::window_id(pointer(element)) {
        Ok(Some(id)) => Outcome::value("CGWindowID", json!(id)),
        Ok(None) => Outcome::error("absent", "AX window has no native window ID"),
        Err(code) => error(code),
    }
}

fn enumerate_windows<W: Write>(
    emitter: &mut Emitter<W>,
    app: &CFType,
    app_ref: &str,
    app_pid: i32,
    references: &mut References,
    published: &mut BTreeSet<String>,
) -> io::Result<Vec<(String, CFRetained<CFType>)>> {
    let mut discovered = BTreeMap::new();
    for attribute in ["AXWindows", "AXFocusedWindow", "AXMainWindow"] {
        let Some(operation) =
            emitter.inventory(app_ref, "ax", &format!("inventory.{attribute}"), &[])?
        else {
            continue;
        };
        let raw = match copy(app, attribute, emitter.remaining()) {
            Ok(raw) => raw,
            Err(outcome) => {
                let complete = attribute != "AXWindows" && outcome.complete();
                emitter.evidence(&operation, outcome.clone())?;
                emitter.finish(&operation, &outcome.status, complete)?;
                continue;
            }
        };
        let windows = if attribute == "AXWindows" {
            if let Some(array) = array(&raw) {
                array.iter().collect::<Vec<_>>()
            } else {
                emitter.evidence(
                    &operation,
                    Outcome::error("representation_unsupported", "AXWindows is not an array"),
                )?;
                emitter.finish(&operation, "representation_unsupported", false)?;
                continue;
            }
        } else {
            vec![raw]
        };
        let mut complete = true;
        // Flush an empty chunk too: this records that the platform call has
        // returned before any child identity reads begin.
        emitter.objects(&operation, Vec::new())?;
        for (index, window) in windows.into_iter().enumerate() {
            if index >= 8192 {
                complete = false;
                break;
            }
            if objc2_core_foundation::CFGetTypeID(Some(&window))
                != unsafe { accessibility_sys::AXUIElementGetTypeID() }
            {
                complete = false;
                continue;
            }
            let Some(reference) = references.reference(&window) else {
                complete = false;
                break;
            };
            if !published.insert(reference.clone()) {
                continue;
            }
            emitter.objects(&operation,vec![Object{reference:reference.clone(),source:"ax".into(),resource:Resource::Window,id:None,pid:None,data:json!({"application_pid":app_pid,"application_reference":app_ref,"inventory_operation":operation})}])?;
            discovered.insert(reference, window);
        }
        emitter.finish(
            &operation,
            if complete { "value" } else { "truncated" },
            complete,
        )?;
    }
    Ok(discovered.into_iter().collect())
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
fn window<W: Write>(
    emitter: &mut Emitter<W>,
    request: &ReadRequest,
    selection: &Selection,
    reference: &str,
    element: &CFType,
    references: &mut References,
) -> io::Result<()> {
    let owner_before = emitter.declare(reference, "ax", "identity.initial_pid", &[], false)?;
    let identity_before = emitter.declare(reference, "ax", "identity.initial_id", &[], false)?;
    emitter.read_field(&owner_before, |remaining| pid(element, remaining))?;
    let native_id = emitter.read_field(&identity_before, |remaining| id(element, remaining))?;
    if let Some(id) = native_id.value.as_ref().and_then(Value::as_u64) {
        emitter.window_ids.insert(id);
    }
    if request.resource == Resource::Window
        && matches!(request.mode,ReadMode::Inspect{id:Some(target)} if native_id.value.as_ref().and_then(Value::as_u64)!=Some(target))
    {
        return Ok(());
    }
    let owner_after = emitter.declare(reference, "ax", "identity.final_pid", &[], false)?;
    let identity_after = emitter.declare(reference, "ax", "identity.final_id", &[], false)?;
    let details =
        request.resource == Resource::Window && matches!(request.mode, ReadMode::Inspect { .. });
    let all = (details && selection.wants("ax"))
        || (request.resource == Resource::App
            && selection.paths().any(|path| path == "windows.ax"));
    let mut advertised = Vec::new();
    if all && let Some(operation) = emitter.inventory(reference, "ax", "ax.attribute_names", &[])? {
        match names(element, "attributes", emitter.remaining()) {
            Ok(raw) => {
                if let Some(values) = array(&raw) {
                    emitter.objects(&operation, Vec::new())?;
                    let mut complete = true;
                    for (index, name) in values.iter().enumerate() {
                        if index >= 1024 {
                            complete = false;
                            break;
                        }
                        let Some(name) = name.downcast_ref::<CFString>() else {
                            complete = false;
                            continue;
                        };
                        let outcome = convert(name, references);
                        if !outcome.complete() {
                            complete = false;
                            continue;
                        }
                        let Some(name) = outcome
                            .value
                            .and_then(|value| value.as_str().map(str::to_owned))
                        else {
                            complete = false;
                            continue;
                        };
                        advertised.push(name.clone());
                        emitter.objects(
                            &operation,
                            vec![Object {
                                reference: format!("attribute:{reference}:{index}"),
                                source: "ax_attribute".into(),
                                resource: Resource::Window,
                                id: None,
                                pid: None,
                                data: json!({"window_reference":reference,"name":name}),
                            }],
                        )?;
                    }
                    emitter.finish(
                        &operation,
                        if complete { "value" } else { "truncated" },
                        complete,
                    )?;
                } else {
                    emitter.evidence(
                        &operation,
                        Outcome::error(
                            "representation_unsupported",
                            "attribute names are not an array",
                        ),
                    )?;
                    emitter.finish(&operation, "representation_unsupported", false)?;
                }
            }
            Err(outcome) => {
                emitter.evidence(&operation, outcome.clone())?;
                emitter.finish(&operation, &outcome.status, false)?;
            }
        }
    }
    let mut attributes: BTreeSet<String> = if all {
        advertised.iter().cloned().collect()
    } else {
        BTreeSet::new()
    };
    if details {
        attributes.extend(
            selection.ax_attributes(&advertised.iter().map(String::as_str).collect::<Vec<_>>()),
        );
    } else {
        attributes.extend(["AXTitle", "AXMinimized"].into_iter().map(str::to_owned));
    }
    for filter in &request.filters {
        if filter.field == "title" {
            attributes.insert("AXTitle".into());
        }
        if filter.field == "minimized" {
            attributes.insert("AXMinimized".into());
        }
    }
    let mut plan = Vec::new();
    for name in attributes {
        let value = emitter.declare(reference, "ax", &format!("ax.{name}.value"), &[], false)?;
        let settable = if details || all {
            Some(emitter.declare(reference, "ax", &format!("ax.{name}.settable"), &[], false)?)
        } else {
            None
        };
        plan.push((name, value, settable));
    }
    for (name, value, settable) in plan {
        emitter.read_field(&value, |remaining| {
            copy(element, &name, remaining)
                .map_or_else(|outcome| outcome, |value| convert(&value, references))
        })?;
        if let Some(settable) = settable {
            emitter.read_field(&settable, |remaining| {
                if let Err(error) = configure(element, remaining) {
                    return error;
                }
                let name = CFString::from_str(&name);
                let mut settable = 0;
                let status = unsafe {
                    AXUIElementIsAttributeSettable(
                        pointer(element),
                        NonNull::from(&*name).as_ptr().cast(),
                        &raw mut settable,
                    )
                };
                if status == 0 {
                    Outcome::value("Boolean", json!(settable != 0))
                } else {
                    error(status)
                }
            })?;
        }
    }
    for group in ["actions", "parameterized-attributes"] {
        if details && selection.wants(group) {
            emitter.field(reference, "ax", group, &[], |remaining| {
                names(element, group, remaining)
                    .map_or_else(|outcome| outcome, |value| convert(&value, references))
            })?;
        }
    }
    emitter.read_field(&owner_after, |remaining| pid(element, remaining))?;
    emitter.read_field(&identity_after, |remaining| id(element, remaining))?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(super) fn collect<W: Write>(
    emitter: &mut Emitter<W>,
    request: &ReadRequest,
    selection: &Selection,
    plan: &Plan,
    references: &mut References,
    _main: MainThreadMarker,
) -> io::Result<()> {
    if !(plan.apps
        || plan.ax
        || plan.active
        || (request.resource == Resource::Session && selection.wants("capabilities")))
    {
        return Ok(());
    }
    let trusted = if plan.ax
        || plan.active
        || (request.resource == Resource::Session && selection.wants("capabilities"))
    {
        emitter
            .field("session", "ax", "capabilities.ax_trusted", &[], |_| {
                Outcome::value(
                    "Boolean",
                    json!(unsafe { accessibility_sys::AXIsProcessTrusted() }),
                )
            })?
            .value
            .as_ref()
            .and_then(Value::as_bool)
            == Some(true)
    } else {
        false
    };
    if !plan.apps {
        return Ok(());
    }
    let Some(inventory) = emitter.inventory("session", "appkit", "apps", &[])? else {
        return Ok(());
    };
    let apps = NSWorkspace::sharedWorkspace().runningApplications();
    emitter.objects(&inventory, Vec::new())?;
    let mut entries = Vec::new();
    let mut complete = true;
    for (index, app) in apps.iter().enumerate() {
        if index >= 4096 {
            complete = false;
            break;
        }
        let pid = app.processIdentifier();
        if pid <= 0 {
            continue;
        }
        let reference = format!("app:{index}");
        emitter.objects(
            &inventory,
            vec![Object {
                reference: reference.clone(),
                source: "appkit".into(),
                resource: Resource::App,
                id: u64::try_from(pid).ok(),
                pid: Some(pid),
                data: json!({"inventory_operation":inventory}),
            }],
        )?;
        entries.push((reference, pid, app));
    }
    emitter.finish(
        &inventory,
        if complete { "value" } else { "truncated" },
        complete,
    )?;
    let mut published = BTreeSet::new();
    for (reference, pid, app) in entries {
        let relevant = request.resource != Resource::App
            || !matches!(request.mode,ReadMode::Inspect{id:Some(target)} if u64::try_from(pid).ok()!=Some(target));
        if !relevant {
            continue;
        }
        let app_summary = request.resource == Resource::Session && selection.wants("apps")
            || matches!(request.mode, ReadMode::List)
                && matches!(request.resource, Resource::App | Resource::Window);
        let app_identity =
            app_summary || request.resource == Resource::App && selection.wants("identity");
        let app_state =
            app_summary || request.resource == Resource::App && selection.wants("state");
        if app_identity {
            emitter.field(&reference, "appkit", "identity.name", &[], |_| {
                app.localizedName().map_or_else(
                    || Outcome::error("unavailable", "application name unavailable"),
                    |name| Outcome::value("NSString", json!(name.to_string())),
                )
            })?;
        }
        if app_identity
            || request
                .filters
                .iter()
                .any(|filter| filter.field == "bundle-id")
        {
            emitter.field(&reference, "appkit", "identity.bundle_id", &[], |_| {
                app.bundleIdentifier().map_or_else(
                    || Outcome::error("absent", "application has no bundle identifier"),
                    |name| Outcome::value("NSString", json!(name.to_string())),
                )
            })?;
        }
        if app_state {
            emitter.field(&reference, "appkit", "state.hidden", &[], |_| {
                Outcome::value("Boolean", json!(app.isHidden()))
            })?;
        }
        if app_state || plan.active {
            emitter.field(&reference, "appkit", "state.frontmost", &[], |_| {
                Outcome::value("Boolean", json!(app.isActive()))
            })?;
        }
        if !trusted || !plan.ax {
            continue;
        }
        let raw = unsafe { AXUIElementCreateApplication(pid) };
        let Some(raw) = NonNull::new(raw) else {
            emitter.field(&reference, "ax", "windows", &[], |_| {
                Outcome::error("unavailable", "AX application handle unavailable")
            })?;
            continue;
        };
        let application: CFRetained<CFType> = unsafe { CFRetained::from_raw(raw.cast()) };
        for (reference, element) in enumerate_windows(
            emitter,
            &application,
            &reference,
            pid,
            references,
            &mut published,
        )? {
            window(
                emitter, request, selection, &reference, &element, references,
            )?;
        }
    }
    if !trusted
        && plan.ax
        && let Some(operation) = emitter.inventory("session", "ax", "ax.windows", &[])?
    {
        emitter.evidence(
            &operation,
            Outcome::error(
                "permission_denied",
                "Accessibility permission is unavailable; no prompt was opened",
            ),
        )?;
        emitter.finish(&operation, "permission_denied", false)?;
    }
    if plan.active {
        emitter.field("session", "appkit", "active.cursor", &[], |_| {
            let point = objc2_app_kit::NSEvent::mouseLocation();
            Outcome::value(
                "NSPoint",
                json!({"x":point.x,"y":point.y,"units":"points","coordinates":"cocoa_global_y_up"}),
            )
        })?;
        if trusted {
            let raw = unsafe { accessibility_sys::AXUIElementCreateSystemWide() };
            if let Some(raw) = NonNull::new(raw) {
                let system: CFRetained<CFType> = unsafe { CFRetained::from_raw(raw.cast()) };
                let operation =
                    emitter.declare("session", "ax", "active.application_reference", &[], false)?;
                if emitter.start(&operation)? {
                    match copy(&system, "AXFocusedApplication", emitter.remaining()) {
                        Ok(app) => {
                            emitter.evidence(
                                &operation,
                                Outcome::value(
                                    "AXUIElement",
                                    json!({"reference":references.reference(&app)}),
                                ),
                            )?;
                            emitter.finish(&operation, "value", true)?;
                            emitter.field("session", "ax", "active.pid", &[], |remaining| {
                                pid(&app, remaining)
                            })?;
                            let window = emitter.declare(
                                "session",
                                "ax",
                                "active.focused_window",
                                &[],
                                false,
                            )?;
                            if emitter.start(&window)? {
                                match copy(&app, "AXFocusedWindow", emitter.remaining()) {
                                    Ok(element) => {
                                        emitter.evidence(
                                            &window,
                                            Outcome::value(
                                                "AXUIElement",
                                                json!({"reference":references.reference(&element)}),
                                            ),
                                        )?;
                                        emitter.finish(&window, "value", true)?;
                                        emitter.field(
                                            "session",
                                            "ax",
                                            "active.window_id",
                                            &[],
                                            |remaining| id(&element, remaining),
                                        )?;
                                    }
                                    Err(outcome) => {
                                        emitter.evidence(&window, outcome.clone())?;
                                        emitter.finish(&window, &outcome.status, true)?;
                                    }
                                }
                            }
                        }
                        Err(outcome) => {
                            emitter.evidence(&operation, outcome.clone())?;
                            emitter.finish(&operation, &outcome.status, true)?;
                        }
                    }
                }
            }
        } else {
            emitter.field("session", "ax", "active.window_id", &[], |_| {
                Outcome::error("permission_denied", "AX focus unavailable")
            })?;
        }
    }
    Ok(())
}
