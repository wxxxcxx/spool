//! Physical screen and native Space evidence, independent of daemon state.
use super::plan::Plan;
use super::{Emitter, Object, Outcome, References, array, dictionary, integer, key};
use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_graphics::{CGDisplayBounds, CGMainDisplayID};
use objc2_foundation::{NSNumber, NSString, ns_string};
use serde_json::json;
use spool_shared_types::inspection::{ReadMode, ReadRequest, Resource, Selection};
use std::{
    collections::BTreeMap,
    io::{self, Write},
};

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(super) fn collect<W: Write>(
    emitter: &mut Emitter<W>,
    request: &ReadRequest,
    selection: &Selection,
    plan: &Plan,
    _references: &mut References,
    mtm: MainThreadMarker,
) -> io::Result<()> {
    let mut display_ids = BTreeMap::new();
    if plan.displays
        && let Some(inventory) = emitter.inventory("session", "appkit", "displays", &[])?
    {
        let screens = NSScreen::screens(mtm);
        emitter.objects(&inventory, vec![])?;
        let mut entries = Vec::new();
        let mut complete = true;
        for (index, screen) in screens.iter().enumerate() {
            if index >= 256 {
                complete = false;
                break;
            }
            let description = screen.deviceDescription();
            let numbers = unsafe { description.cast_unchecked::<NSString, NSNumber>() };
            let Some(id) = numbers
                .objectForKey(ns_string!("NSScreenNumber"))
                .map(|id| id.as_u32())
            else {
                complete = false;
                continue;
            };
            let reference = format!("display:{index}");
            emitter.objects(
                &inventory,
                vec![Object {
                    reference: reference.clone(),
                    source: "appkit".into(),
                    resource: Resource::Display,
                    id: Some(u64::from(id)),
                    pid: None,
                    data: json!({"inventory_operation":inventory}),
                }],
            )?;
            entries.push((id, reference, screen));
        }
        emitter.finish(
            &inventory,
            if complete { "value" } else { "truncated" },
            complete,
        )?;
        for (id, reference, screen) in entries {
            let relevant = request.resource != Resource::Display
                || !matches!(request.mode,ReadMode::Inspect{id:Some(target)} if target!=u64::from(id));
            let identity = plan.spaces
                || relevant
                    && (matches!(request.mode, ReadMode::List) || selection.wants("identity"));
            if identity {
                let uuid = emitter.field(&reference, "cg", "identity.uuid", &[], |_| {
                    crate::manager::Display::uuid_from_id(id).map_or_else(
                        |error| Outcome::error("unavailable", error.to_string()),
                        |uuid| Outcome::value("CFString", json!(uuid.to_string())),
                    )
                })?;
                if let Some(uuid) = uuid
                    .value
                    .and_then(|value| value.as_str().map(str::to_owned))
                {
                    display_ids.insert(uuid, id);
                }
                emitter.field(&reference, "appkit", "identity.name", &[], |_| {
                    Outcome::value("NSString", json!(screen.localizedName().to_string()))
                })?;
                emitter.field(&reference, "cg", "identity.main", &[], |_| {
                    Outcome::value("Boolean", json!(id == CGMainDisplayID()))
                })?;
            }
            if relevant
                && (request.resource != Resource::Display
                    || matches!(request.mode, ReadMode::List)
                    || selection.wants("geometry.bounds"))
            {
                emitter.field(&reference,"cg","geometry.bounds",&[],|_|{let frame=CGDisplayBounds(id);Outcome::value("CGRect",json!({"x":frame.origin.x,"y":frame.origin.y,"width":frame.size.width,"height":frame.size.height,"units":"points","coordinates":"global_y_down"}))})?;
            }
            if relevant
                && request.resource == Resource::Display
                && selection.wants("geometry.usable_frame")
            {
                emitter.field(&reference,"appkit","geometry.usable_frame",&[],|_|{let frame=screen.visibleFrame();Outcome::value("NSRect",json!({"x":frame.origin.x,"y":frame.origin.y,"width":frame.size.width,"height":frame.size.height,"units":"points","coordinates":"cocoa_global_y_up"}))})?;
            }
            if relevant
                && request.resource == Resource::Display
                && selection.wants("geometry.scale")
            {
                emitter.field(&reference, "appkit", "geometry.scale", &[], |_| {
                    Outcome::value("CGFloat", json!(screen.backingScaleFactor()))
                })?;
            }
        }
    }
    if plan.spaces
        && let Some(inventory) = emitter.inventory("session", "skylight", "spaces", &[])?
    {
        let Some(raw) = crate::manager::inspection::spaces() else {
            emitter.evidence(
                &inventory,
                Outcome::error("unavailable", "native Space topology unavailable"),
            )?;
            emitter.finish(&inventory, "unavailable", false)?;
            return Ok(());
        };
        emitter.objects(&inventory, vec![])?;
        let mut entries = Vec::new();
        let mut complete = true;
        let mut count = 0usize;
        for entry in raw.iter() {
            let Some(display) = dictionary(&entry) else {
                complete = false;
                continue;
            };
            let identifier = key(display, "Display Identifier").and_then(|value| {
                value
                    .downcast_ref::<objc2_core_foundation::CFString>()
                    .map(ToString::to_string)
            });
            let display_id = identifier
                .as_ref()
                .and_then(|uuid| display_ids.get(uuid).copied());
            // "Main" is not a UUID. Do not guess an owner from an ambiguous private record.
            let current = key(display, "Current Space")
                .as_deref()
                .and_then(dictionary)
                .and_then(|value| key(value, "id64"))
                .as_deref()
                .and_then(integer)
                .and_then(|id| u64::try_from(id).ok());
            let Some(spaces) = key(display, "Spaces") else {
                complete = false;
                continue;
            };
            let Some(spaces) = array(&spaces) else {
                complete = false;
                continue;
            };
            for (ordinal, space) in spaces.iter().enumerate() {
                if count >= 8192 {
                    complete = false;
                    break;
                }
                count += 1;
                let Some(space) = dictionary(&space) else {
                    complete = false;
                    continue;
                };
                let id = key(space, "id64")
                    .as_deref()
                    .and_then(integer)
                    .and_then(|id| u64::try_from(id).ok())
                    .filter(|id| *id > 0);
                if id.is_none() {
                    complete = false;
                }
                let kind = key(space, "type").as_deref().and_then(integer);
                let reference = format!("space:{count}");
                emitter.objects(&inventory,vec![Object{reference:reference.clone(),source:"skylight".into(),resource:Resource::Space,id,pid:None,data:json!({"inventory_operation":inventory,"display_identifier":identifier})}])?;
                entries.push((reference, id, display_id, current, ordinal, kind));
            }
        }
        emitter.finish(
            &inventory,
            if complete { "value" } else { "truncated" },
            complete,
        )?;
        for (reference, id, display, current, ordinal, kind) in entries {
            for (scope, outcome) in [
                (
                    "identity.display_id",
                    display.map_or_else(
                        || Outcome::error("unavailable", "display UUID could not be resolved"),
                        |id| Outcome::value("CGDirectDisplayID", json!(id)),
                    ),
                ),
                (
                    "identity.ordinal",
                    Outcome::value("ordinal", json!(ordinal + 1)),
                ),
                (
                    "identity.kind",
                    match kind {
                        Some(0) => Outcome::value("SpaceType", json!("user")),
                        Some(4) => Outcome::value("SpaceType", json!("fullscreen")),
                        _ => Outcome::error("unavailable", "unknown native Space kind"),
                    },
                ),
                (
                    "state.visible",
                    current.zip(id).map_or_else(
                        || Outcome::error("unavailable", "current Space is unavailable"),
                        |(current, id)| Outcome::value("Boolean", json!(current == id)),
                    ),
                ),
            ] {
                if !plan.space_field(request, selection, id, scope) {
                    continue;
                }
                emitter.field(
                    &reference,
                    "skylight",
                    scope,
                    std::slice::from_ref(&inventory),
                    |_| outcome,
                )?;
            }
            if plan.membership
                && (request.resource != Resource::Space
                    || !matches!(request.mode, ReadMode::Inspect{id:Some(target)} if id != Some(target)))
            {
                emitter.field(&reference, "skylight", "windows", &[], |_| {
                    id.map_or_else(
                        || Outcome::error("unavailable", "Space ID unavailable"),
                        |id| {
                            crate::manager::inspection::space_windows(id).map_or_else(
                                |error| Outcome::error("unavailable", error),
                                |ids| {
                                    if ids.len() > 4096 {
                                        Outcome {
                                            status: "truncated".into(),
                                            native_type: Some("CGWindowID[]".into()),
                                            value: Some(json!(&ids[..4096])),
                                            message: Some("window membership limit".into()),
                                        }
                                    } else {
                                        Outcome::value("CGWindowID[]", json!(ids))
                                    }
                                },
                            )
                        },
                    )
                })?;
            }
        }
    }
    Ok(())
}
