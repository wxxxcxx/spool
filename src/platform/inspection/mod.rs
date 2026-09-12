//! Native calls execute only on this short-lived process's main thread.

use self::value::{References, convert};
use crate::inspection::protocol::{
    Frame, MAX_FRAME, Message, Object, Outcome, VERSION, Work, write_frame,
};
use crate::inspection::supervisor::WorkerRequest;
use objc2::MainThreadMarker;
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption, kCGNullWindowID};
use serde_json::json;
use spool_shared_types::inspection::{ReadMode, ReadRequest, Resource, Selection};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    ptr::NonNull,
    time::{Duration, Instant},
};

mod ax;
mod plan;
mod topology;
mod value;

pub(super) struct Emitter<W: Write> {
    output: W,
    window_ids: BTreeSet<u64>,
    capture_id: String,
    sequence: u64,
    operation: u64,
    started: Instant,
    budget: Duration,
}
impl<W: Write> Emitter<W> {
    fn new(output: W, capture_id: String, timeout_ms: u64) -> Self {
        Self {
            output,
            window_ids: BTreeSet::new(),
            capture_id,
            sequence: 0,
            operation: 0,
            started: Instant::now(),
            budget: Duration::from_millis(timeout_ms),
        }
    }
    pub(super) fn remaining(&self) -> Duration {
        self.budget.saturating_sub(self.started.elapsed())
    }
    fn offset(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn emit(&mut self, message: Message) -> io::Result<()> {
        write_frame(
            &mut self.output,
            &Frame {
                version: VERSION,
                capture_id: self.capture_id.clone(),
                sequence: self.sequence,
                message,
            },
        )?;
        self.sequence += 1;
        Ok(())
    }
    fn declare(
        &mut self,
        target: &str,
        source: &str,
        scope: &str,
        dependencies: &[String],
        inventory: bool,
    ) -> io::Result<String> {
        self.operation += 1;
        let id = format!("operation:{}", self.operation);
        self.emit(Message::WorkDeclared(Work {
            id: id.clone(),
            target: target.into(),
            source: source.into(),
            scope: scope.into(),
            dependencies: dependencies.to_vec(),
            inventory,
        }))?;
        Ok(id)
    }
    fn start(&mut self, id: &str) -> io::Result<bool> {
        if self.remaining().is_zero() {
            self.finish(id, "budget_skipped", false)?;
            return Ok(false);
        }
        self.emit(Message::OperationStarted {
            id: id.into(),
            at: chrono::Utc::now().to_rfc3339(),
            offset_ms: self.offset(),
        })?;
        Ok(true)
    }
    fn finish(&mut self, id: &str, outcome: &str, enumeration_complete: bool) -> io::Result<()> {
        self.emit(Message::OperationFinished {
            id: id.into(),
            at: chrono::Utc::now().to_rfc3339(),
            offset_ms: self.offset(),
            outcome: outcome.into(),
            enumeration_complete,
        })
    }
    fn inventory(
        &mut self,
        target: &str,
        source: &str,
        scope: &str,
        dependencies: &[String],
    ) -> io::Result<Option<String>> {
        let id = self.declare(target, source, scope, dependencies, true)?;
        Ok(self.start(&id)?.then_some(id))
    }
    fn objects(&mut self, id: &str, objects: Vec<Object>) -> io::Result<()> {
        // Small per-object frames retain earlier objects even if a later one
        // cannot be represented or the process is interrupted mid-inventory.
        if objects.is_empty() {
            self.emit(Message::InventoryChunk {
                id: id.into(),
                objects,
            })?;
        } else {
            for object in objects {
                if object.resource == Resource::Window
                    && let Some(id) = object.id
                {
                    self.window_ids.insert(id);
                }
                self.emit(Message::InventoryChunk {
                    id: id.into(),
                    objects: vec![object],
                })?;
            }
        }
        Ok(())
    }
    fn evidence(&mut self, id: &str, outcome: Outcome) -> io::Result<()> {
        self.emit(Message::EvidenceResult {
            id: id.into(),
            outcome,
            at: chrono::Utc::now().to_rfc3339(),
            offset_ms: self.offset(),
        })
    }
    pub(super) fn field(
        &mut self,
        target: &str,
        source: &str,
        scope: &str,
        dependencies: &[String],
        read: impl FnOnce(Duration) -> Outcome,
    ) -> io::Result<Outcome> {
        let id = self.declare(target, source, scope, dependencies, false)?;
        self.read_field(&id, read)
    }
    fn read_field(
        &mut self,
        id: &str,
        read: impl FnOnce(Duration) -> Outcome,
    ) -> io::Result<Outcome> {
        if !self.start(id)? {
            return Ok(Outcome::error(
                "budget_skipped",
                "collection budget exhausted",
            ));
        }
        let outcome = read(self.remaining());
        self.evidence(id, outcome.clone())?;
        self.finish(id, &outcome.status, true)?;
        Ok(outcome)
    }
}

pub(super) fn dictionary(value: &CFType) -> Option<&CFDictionary<CFString, CFType>> {
    value
        .downcast_ref::<CFDictionary>()
        .map(|value| unsafe { &*NonNull::from(value).as_ptr().cast() })
}
pub(super) fn array(value: &CFType) -> Option<&CFArray<CFType>> {
    value
        .downcast_ref::<CFArray>()
        .map(|value| unsafe { &*NonNull::from(value).as_ptr().cast() })
}
pub(super) fn key(
    value: &CFDictionary<CFString, CFType>,
    name: &str,
) -> Option<CFRetained<CFType>> {
    value.get(&CFString::from_str(name))
}
pub(super) fn integer(value: &CFType) -> Option<i64> {
    value.downcast_ref::<CFNumber>().and_then(CFNumber::as_i64)
}

fn cg_fields(request: &ReadRequest, selection: &Selection) -> BTreeSet<String> {
    let summary = matches!(request.mode, ReadMode::List) || request.resource != Resource::Window;
    let mut fields: BTreeSet<_> = ["kCGWindowNumber", "kCGWindowOwnerPID"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if summary {
        fields.extend(
            [
                "kCGWindowName",
                "kCGWindowOwnerName",
                "kCGWindowBounds",
                "kCGWindowLayer",
                "kCGWindowAlpha",
                "kCGWindowIsOnscreen",
                "kCGWindowSharingState",
                "kCGWindowStoreType",
                "kCGWindowMemoryUsage",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    fields.extend(
        selection
            .paths()
            .filter_map(|path| path.strip_prefix("cg."))
            .map(str::to_owned),
    );
    for filter in &request.filters {
        let name = match filter.field.as_str() {
            "title" => Some("kCGWindowName"),
            "on-screen" => Some("kCGWindowIsOnscreen"),
            _ => None,
        };
        if let Some(name) = name {
            fields.insert(name.into());
        }
    }
    fields
}

fn cg_windows<W: Write>(
    emitter: &mut Emitter<W>,
    request: &ReadRequest,
    selection: &Selection,
    references: &mut References,
) -> io::Result<()> {
    let Some(inventory) = emitter.inventory("session", "cg", "cg.windows", &[])? else {
        return Ok(());
    };
    let Some(raw) = CGWindowListCopyWindowInfo(CGWindowListOption::OptionAll, kCGNullWindowID)
    else {
        emitter.evidence(
            &inventory,
            Outcome::error("unavailable", "WindowServer inventory unavailable"),
        )?;
        emitter.finish(&inventory, "unavailable", false)?;
        return Ok(());
    };
    let raw: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(raw) };
    let fields = cg_fields(request, selection);
    let mut complete = true;
    let mut entries = Vec::new();
    emitter.objects(&inventory, Vec::new())?;
    for (index, entry) in raw.iter().enumerate() {
        if index >= 8192 {
            complete = false;
            break;
        }
        let Some(dictionary) = dictionary(&entry) else {
            complete = false;
            continue;
        };
        let id = key(dictionary, "kCGWindowNumber")
            .as_deref()
            .and_then(integer)
            .and_then(|id| u64::try_from(id).ok());
        let pid = key(dictionary, "kCGWindowOwnerPID")
            .as_deref()
            .and_then(integer)
            .and_then(|pid| i32::try_from(pid).ok());
        if id.is_none_or(|id| id == 0) || pid.is_none_or(|pid| pid <= 0) {
            complete = false;
        }
        let reference = format!("cg:{}", index + 1);
        emitter.objects(&inventory,vec![Object{reference:reference.clone(),source:"cg".into(),resource:Resource::Window,id,pid,data:json!({"inventory_operation":inventory,"sampled_at":chrono::Utc::now().to_rfc3339()})}])?;
        entries.push((reference, id, entry));
    }
    emitter.finish(
        &inventory,
        if complete { "value" } else { "truncated" },
        complete,
    )?;
    for (reference, id, entry) in entries {
        let Some(dictionary) = dictionary(&entry) else {
            continue;
        };
        if request.resource == Resource::Window
            && matches!(request.mode,ReadMode::Inspect{id:Some(target)} if id!=Some(target))
        {
            continue;
        }
        let mut selected = fields.clone();
        if selection.wants("cg")
            && request.resource == Resource::Window
            && !matches!(request.mode, ReadMode::List)
        {
            let (keys, _) = dictionary.to_vecs();
            selected.extend(keys.iter().map(std::string::ToString::to_string));
        }
        for field in &selected {
            emitter.field(
                &reference,
                "cg",
                &format!("cg.{field}"),
                std::slice::from_ref(&inventory),
                |_| {
                    key(dictionary, field).map_or_else(
                        || {
                            Outcome::error(
                                "unavailable",
                                "key was not exposed; absence or redaction is unresolved",
                            )
                        },
                        |value| convert(&value, references),
                    )
                },
            )?;
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one bounded native capture owns its process and ordered sources"
)]
pub(crate) fn run() -> io::Result<()> {
    let main_thread = MainThreadMarker::new()
        .ok_or_else(|| io::Error::other("native inspection requires main thread"))?;
    let mut stdin = io::stdin().lock();
    let mut length = [0u8; 4];
    stdin.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker request exceeds limit",
        ));
    }
    let mut bytes = vec![0u8; length];
    stdin.read_exact(&mut bytes)?;
    let input: WorkerRequest = serde_json::from_slice(&bytes)?;
    if input.request.source != spool_shared_types::inspection::Source::Native {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native worker source required",
        ));
    }
    input
        .request
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let selection = Selection::new(
        input.request.resource,
        input.request.source,
        &input.request.show,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut emitter = Emitter::new(
        io::stdout().lock(),
        input.capture_id,
        input.request.timeout_ms,
    );
    let mut references = References::default();
    let plan = plan::Plan::new(&input.request, &selection);
    topology::collect(
        &mut emitter,
        &input.request,
        &selection,
        &plan,
        &mut references,
        main_thread,
    )?;
    if plan.cg {
        cg_windows(&mut emitter, &input.request, &selection, &mut references)?;
    }
    ax::collect(
        &mut emitter,
        &input.request,
        &selection,
        &plan,
        &mut references,
        main_thread,
    )?;
    if plan.membership
        && let Some(inventory) =
            emitter.inventory("session", "skylight", "window_memberships", &[])?
    {
        let ids=emitter.window_ids.iter().copied().filter(|id|input.request.resource!=Resource::Window||!matches!(input.request.mode,ReadMode::Inspect{id:Some(target)} if *id!=target)).collect::<Vec<_>>();
        emitter.objects(&inventory, Vec::new())?;
        for id in &ids {
            emitter.objects(
                &inventory,
                vec![Object {
                    reference: format!("membership:{id}"),
                    source: "skylight".into(),
                    resource: Resource::Window,
                    id: Some(*id),
                    pid: None,
                    data: json!({"inventory_operation":inventory}),
                }],
            )?;
        }
        emitter.finish(&inventory, "value", true)?;
        for id in ids {
            emitter.field(
                &format!("membership:{id}"),
                "skylight",
                "spaces.window_to_spaces",
                std::slice::from_ref(&inventory),
                |_| {
                    crate::manager::inspection::window_spaces(id).map_or_else(
                        |error| Outcome::error("unavailable", error),
                        |ids| {
                            if ids.len() > 4096 {
                                Outcome {
                                    status: "truncated".into(),
                                    native_type: Some("SpaceID[]".into()),
                                    value: Some(json!(&ids[..4096])),
                                    message: Some("membership representation limit".into()),
                                }
                            } else {
                                Outcome::value("SpaceID[]", json!(ids))
                            }
                        },
                    )
                },
            )?;
        }
    }
    if input.request.resource == Resource::Session && selection.wants("capabilities") {
        emitter.field(
            "session",
            "cg",
            "capabilities.screen_recording",
            &[],
            |_| {
                Outcome::value(
                    "Boolean",
                    json!(objc2_core_graphics::CGPreflightScreenCaptureAccess()),
                )
            },
        )?;
        emitter.field(
            "session",
            "skylight",
            "capabilities.separate_spaces",
            &[],
            |_| Outcome::value("Boolean", json!(crate::manager::check_separate_spaces())),
        )?;
    }
    emitter.emit(Message::CollectionFinished {
        at: chrono::Utc::now().to_rfc3339(),
        offset_ms: emitter.offset(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use spool_shared_types::inspection::Source;
    #[test]
    fn cg_group_does_not_invent_optional_keys_but_explicit_leaf_requests_them() {
        let mut request = ReadRequest::detail(Resource::Window, Source::Native, Some(42));
        request.show = vec!["cg".into()];
        let selection = Selection::new(request.resource, request.source, &request.show).unwrap();
        assert!(!cg_fields(&request, &selection).contains("kCGWindowMemoryUsage"));
        request.show = vec!["cg.kCGWindowMemoryUsage".into()];
        let selection = Selection::new(request.resource, request.source, &request.show).unwrap();
        assert!(cg_fields(&request, &selection).contains("kCGWindowMemoryUsage"));
    }
}
