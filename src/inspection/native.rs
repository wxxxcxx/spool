//! Pure projection of a bounded native evidence ledger. Never reads the OS.
use super::protocol::{Ledger, Object};
use serde_json::{Value, json};
use spool_shared_types::inspection::{
    Collection, FieldEvidence, Filter, Issue, Match, ReadMode, ReadRequest, Report, Resource,
    Selection, Status,
};
use std::{collections::BTreeSet, process::Command, sync::atomic::AtomicBool, time::Instant};

pub(crate) fn collect(request: ReadRequest, cancelled: &AtomicBool) -> Report {
    let start = Instant::now();
    let at = chrono::Utc::now().to_rfc3339();
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => return Report::failure(&request, "helper_unavailable", &error.to_string()),
    };
    let mut command = Command::new(executable);
    command.arg(super::WORKER_ARGUMENT);
    let ledger = super::supervisor::supervise(
        &mut command,
        uuid::Uuid::new_v4().to_string(),
        &request,
        cancelled,
    );
    let mut report = project(&ledger, &request);
    report.collection.started_at = Some(at);
    report.collection.finished_at = Some(chrono::Utc::now().to_rfc3339());
    report.collection.duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    report
}
fn field<'a>(object: &'a Object, path: &str) -> Option<&'a Value> {
    object
        .data
        .get(path)
        .filter(|field| field["status"] == "value")
        .and_then(|field| field.get("value"))
}
fn identity(object: &Object) -> (Option<u64>, Option<i32>) {
    if object.source == "ax" {
        (
            field(object, "identity.initial_id").and_then(Value::as_u64),
            field(object, "identity.initial_pid")
                .and_then(Value::as_i64)
                .and_then(|pid| i32::try_from(pid).ok()),
        )
    } else {
        (object.id, object.pid)
    }
}
fn inventory_complete(ledger: &Ledger, source: &str) -> bool {
    let work = ledger
        .operations
        .values()
        .filter(|operation| operation.work.source == source && operation.work.inventory)
        .collect::<Vec<_>>();
    !work.is_empty()
        && work
            .iter()
            .all(|operation| operation.finished && operation.enumeration_complete)
        && ledger.finished
}
fn evidence(value: Option<&Value>) -> FieldEvidence {
    value
        .cloned()
        .map_or(FieldEvidence::Unavailable, FieldEvidence::Value)
}
fn issue(target: Option<String>, code: &str, message: &str) -> Issue {
    Issue {
        source: "native".into(),
        operation: "projection".into(),
        target,
        code: code.into(),
        message: message.into(),
    }
}

struct WindowRow<'a> {
    cg: Option<&'a Object>,
    ax: Option<&'a Object>,
    association: &'static str,
}
impl WindowRow<'_> {
    fn id(&self) -> Option<u64> {
        self.cg.or(self.ax).and_then(|object| identity(object).0)
    }
    fn refs(&self) -> impl Iterator<Item = &str> {
        self.cg
            .into_iter()
            .chain(self.ax)
            .map(|object| object.reference.as_str())
    }
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive command/source branches share one admission or capture boundary"
    )]
    fn matches(&self, ledger: &Ledger, filter: &Filter) -> Match {
        let source = |object: Option<&Object>, name: &str, kind: &str| {
            object.map_or_else(
                || {
                    if inventory_complete(ledger, kind)
                        && !ledger.objects.values().any(|candidate| {
                            candidate.source == kind
                                && identity(candidate).0 == self.id()
                                && self.id().is_some()
                        })
                    {
                        FieldEvidence::NotApplicable
                    } else {
                        FieldEvidence::Unavailable
                    }
                },
                |object| evidence(field(object, name)),
            )
        };
        match filter.field.as_str() {
            "title" => filter.evaluate(&[
                source(self.cg, "cg.kCGWindowName", "cg"),
                source(self.ax, "ax.AXTitle.value", "ax"),
            ]),
            "pid" => filter.evaluate(&[
                self.cg.map_or_else(
                    || {
                        if inventory_complete(ledger, "cg") {
                            FieldEvidence::NotApplicable
                        } else {
                            FieldEvidence::Unavailable
                        }
                    },
                    |object| {
                        object.pid.map_or(FieldEvidence::Unavailable, |pid| {
                            FieldEvidence::Value(json!(pid))
                        })
                    },
                ),
                source(self.ax, "identity.initial_pid", "ax"),
            ]),
            "on-screen" => filter.evaluate(&[source(self.cg, "cg.kCGWindowIsOnscreen", "cg")]),
            "minimized" => filter.evaluate(&[source(self.ax, "ax.AXMinimized.value", "ax")]),
            "bundle-id" => {
                if self.association == "unresolved" {
                    return Match::Unknown;
                }
                let pids = self
                    .cg
                    .into_iter()
                    .chain(self.ax)
                    .filter_map(|object| identity(object).1)
                    .collect::<BTreeSet<_>>();
                if pids.len() != 1 {
                    return Match::Unknown;
                }
                let app = ledger
                    .objects
                    .values()
                    .find(|app| app.resource == Resource::App && app.pid == pids.first().copied());
                filter.evaluate(&[app.map_or(FieldEvidence::Unavailable, |app| {
                    evidence(field(app, "identity.bundle_id"))
                })])
            }
            "space" | "display" => {
                let Some(id) = self.id() else {
                    return Match::Unknown;
                };
                let Some(sets) = membership_sets(ledger, id) else {
                    return Match::Unknown;
                };
                let sources = sets
                    .into_iter()
                    .map(|ids| {
                        if filter.field == "space" {
                            return FieldEvidence::Value(json!(ids));
                        }
                        let mut displays = BTreeSet::new();
                        for id in ids {
                            let matches = ledger
                                .objects
                                .values()
                                .filter(|object| {
                                    object.resource == Resource::Space && object.id == Some(id)
                                })
                                .collect::<Vec<_>>();
                            if matches.len() != 1 {
                                return FieldEvidence::Unavailable;
                            }
                            let Some(display) =
                                field(matches[0], "identity.display_id").and_then(Value::as_u64)
                            else {
                                return FieldEvidence::Unavailable;
                            };
                            displays.insert(display);
                        }
                        FieldEvidence::Value(json!(displays))
                    })
                    .collect::<Vec<_>>();
                filter.evaluate(&sources)
            }
            _ => Match::Unknown,
        }
    }
    fn summary(&self) -> Value {
        let title = self
            .ax
            .and_then(|object| field(object, "ax.AXTitle.value"))
            .or_else(|| self.cg.and_then(|object| field(object, "cg.kCGWindowName")));
        json!({"identity":{"id":self.id(),"pid":self.cg.or(self.ax).and_then(|object|identity(object).1)},"title":title,"titles":{"cg":self.cg.and_then(|object|field(object,"cg.kCGWindowName")),"ax":self.ax.and_then(|object|field(object,"ax.AXTitle.value"))},"bounds":self.cg.and_then(|object|field(object,"cg.kCGWindowBounds")),"on_screen":self.cg.and_then(|object|field(object,"cg.kCGWindowIsOnscreen")),"minimized":self.ax.and_then(|object|field(object,"ax.AXMinimized.value")),"association":{"status":self.association,"identity_guarantee":"sampled_evidence_only","references":self.refs().collect::<Vec<_>>()}})
    }
}
fn membership_sets(ledger: &Ledger, id: u64) -> Option<[BTreeSet<u64>; 2]> {
    if !inventory_complete(ledger, "skylight") {
        return None;
    }
    let forward = field(
        ledger.objects.get(&format!("membership:{id}"))?,
        "spaces.window_to_spaces",
    )?
    .as_array()?
    .iter()
    .map(Value::as_u64)
    .collect::<Option<BTreeSet<_>>>()?;
    let mut reverse = BTreeSet::new();
    for space in ledger
        .objects
        .values()
        .filter(|object| object.resource == Resource::Space)
    {
        let windows = field(space, "windows")?
            .as_array()?
            .iter()
            .map(Value::as_u64)
            .collect::<Option<BTreeSet<_>>>()?;
        if windows.contains(&id) {
            reverse.insert(space.id?);
        }
    }
    Some([forward, reverse])
}

fn window_rows(ledger: &Ledger) -> Vec<WindowRow<'_>> {
    let cg = ledger
        .objects
        .values()
        .filter(|object| object.source == "cg" && object.resource == Resource::Window)
        .collect::<Vec<_>>();
    let ax = ledger
        .objects
        .values()
        .filter(|object| object.source == "ax" && object.resource == Resource::Window)
        .collect::<Vec<_>>();
    let mut used = BTreeSet::new();
    let mut rows = Vec::new();
    for cg in &cg {
        let key = identity(cg);
        let candidates = ax
            .iter()
            .copied()
            .filter(|ax| identity(ax) == key && key.0.is_some() && key.1.is_some())
            .collect::<Vec<_>>();
        let owner_conflict = ledger
            .objects
            .values()
            .any(|other| other.source == "cg" && other.id == key.0 && other.pid != key.1);
        let consistent = ledger
            .objects
            .values()
            .filter(|record| record.source == "cg" && identity(record).0 == key.0)
            .count()
            == 1
            && candidates.len() == 1
            && ax.iter().filter(|ax| identity(ax).0 == key.0).count() == 1
            && !owner_conflict
            && inventory_complete(ledger, "ax")
            && {
                let ax = candidates[0];
                field(ax, "identity.final_id").and_then(Value::as_u64) == key.0
                    && field(ax, "identity.final_pid").and_then(Value::as_i64)
                        == key.1.map(i64::from)
            };
        let paired = consistent.then(|| candidates[0]);
        if let Some(ax) = paired {
            used.insert(ax.reference.as_str());
        }
        rows.push(WindowRow {
            cg: Some(cg),
            ax: paired,
            association: if consistent {
                "consistent"
            } else if candidates.is_empty() && inventory_complete(ledger, "ax") {
                "cg_only"
            } else {
                "unresolved"
            },
        });
    }
    for ax in ax {
        if !used.contains(ax.reference.as_str()) {
            rows.push(WindowRow {
                cg: None,
                ax: Some(ax),
                association: if inventory_complete(ledger, "cg")
                    && !cg.iter().any(|cg| identity(cg).0 == identity(ax).0)
                {
                    "ax_only"
                } else {
                    "unresolved"
                },
            });
        }
    }
    rows.sort_by_key(|row| (row.id(), row.refs().map(str::to_owned).collect::<Vec<_>>()));
    rows
}
fn insert_path(root: &mut Value, path: &str, value: Value) {
    if let Some((head, tail)) = path.split_once('.') {
        if !root[head].is_object() {
            root[head] = json!({});
        }
        insert_path(&mut root[head], tail, value);
    } else {
        root[path] = value;
    }
}
fn object_summary(object: &Object) -> Value {
    let mut output = json!({"identity":{"id":object.id},"reference":object.reference});
    for (path, value) in object.data.as_object().into_iter().flatten() {
        if let Some(value) = value.get("value") {
            insert_path(&mut output, path, value.clone());
        }
    }
    output
}
fn generic_match(object: &Object, filter: &Filter) -> Match {
    let path = match filter.field.as_str() {
        "pid" => {
            return filter.evaluate(&[object.pid.map_or(FieldEvidence::Unavailable, |pid| {
                FieldEvidence::Value(json!(pid))
            })]);
        }
        "bundle-id" => "identity.bundle_id",
        "name" => "identity.name",
        "main" => "identity.main",
        "hidden" => "state.hidden",
        "display" => "identity.display_id",
        "kind" => "identity.kind",
        "visible" => "state.visible",
        _ => return Match::Unknown,
    };
    filter.evaluate(&[evidence(field(object, path))])
}
#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(super) fn project(ledger: &Ledger, request: &ReadRequest) -> Report {
    let mut issues = ledger.issues.clone();
    let mut kept = BTreeSet::from(["session".to_owned()]);
    let mut excluded = BTreeSet::new();
    let mut data;
    let mut found = request.resource == Resource::Session;
    let selection = Selection::new(request.resource, request.source, &request.show)
        .expect("validated native request");
    if request.resource == Resource::Window {
        let mut rows = Vec::new();
        let mut records = Vec::new();
        for row in window_rows(ledger) {
            let selected = match request.mode {
                ReadMode::List => true,
                ReadMode::Inspect { id } => row.id() == id,
            };
            if !selected {
                if row.ax.is_some_and(|ax| {
                    ax.data.get("identity.initial_id").is_none_or(|field| {
                        !matches!(field["status"].as_str(), Some("value" | "absent"))
                    })
                }) {
                    issues.push(issue(
                        row.ax.map(|ax| ax.reference.clone()),
                        "identity_unresolved",
                        "a discovered AX window could not be resolved to an ID",
                    ));
                } else {
                    excluded.extend(row.refs().map(str::to_owned));
                }
                continue;
            }
            let matched = request.filters.iter().fold(Match::True, |result, filter| {
                result.and(row.matches(ledger, filter))
            });
            if matched == Match::False {
                excluded.extend(row.refs().map(str::to_owned));
                if let Some(id) = row.id() {
                    excluded.insert(format!("membership:{id}"));
                }
                continue;
            }
            if let Some(id) = row.id() {
                kept.insert(format!("membership:{id}"));
            }
            found = true;
            kept.extend(row.refs().map(str::to_owned));
            let mut summary = row.summary();
            summary["match_status"] = json!(if matched == Match::Unknown {
                "unresolved"
            } else {
                "matched"
            });
            if matched == Match::Unknown {
                issues.push(issue(
                    row.refs().next().map(str::to_owned),
                    "filter_unknown",
                    "required filter evidence is missing or conflicting",
                ));
            }
            if matches!(request.mode, ReadMode::Inspect { .. }) {
                records.extend(row.cg.into_iter().chain(row.ax).map(|object| json!(object)));
            }
            rows.push(summary);
        }
        data = if matches!(request.mode, ReadMode::List) {
            json!(rows)
        } else {
            json!({"identity":{"id":match request.mode{ReadMode::Inspect{id}=>id,ReadMode::List=>None}},"candidates":rows,"records":records})
        };
        if matches!(request.mode, ReadMode::Inspect { .. }) && selection.wants("ax") {
            data["attribute_names"] = json!(
                ledger
                    .objects
                    .values()
                    .filter(|object| object.source == "ax_attribute"
                        && object.data["window_reference"]
                            .as_str()
                            .is_some_and(|reference| kept.contains(reference)))
                    .collect::<Vec<_>>()
            );
        }
        if selection.wants("spaces") && matches!(request.mode, ReadMode::Inspect { .. }) {
            let id = match request.mode {
                ReadMode::Inspect { id } => id,
                ReadMode::List => None,
            };
            data["window_to_spaces"] = id
                .and_then(|id| ledger.objects.get(&format!("membership:{id}")))
                .map_or(Value::Null, |record| json!(record));
            data["spaces"]=json!(ledger.objects.values().filter(|object|object.resource==Resource::Space).map(|object|{kept.insert(object.reference.clone());json!({"space_id":object.id,"membership":field(object,"windows").and_then(Value::as_array).map(|ids|ids.iter().any(|candidate|candidate.as_u64()==id)),"evidence":object.data.get("windows")})}).collect::<Vec<_>>());
        }
    } else if request.resource == Resource::Session {
        data = json!({});
        for group in selection.paths() {
            match group {
                "displays" | "spaces" | "apps" => {
                    let resource = match group {
                        "displays" => Resource::Display,
                        "spaces" => Resource::Space,
                        _ => Resource::App,
                    };
                    data[group] = json!(
                        ledger
                            .objects
                            .values()
                            .filter(|object| object.resource == resource)
                            .map(|object| {
                                kept.insert(object.reference.clone());
                                object_summary(object)
                            })
                            .collect::<Vec<_>>()
                    );
                }
                "windows" => {
                    data[group] = json!(
                        window_rows(ledger)
                            .into_iter()
                            .map(|row| {
                                kept.extend(row.refs().map(str::to_owned));
                                row.summary()
                            })
                            .collect::<Vec<_>>()
                    );
                }
                _ => {
                    let mut value = json!({});
                    for (path, evidence) in ledger.objects["session"]
                        .data
                        .as_object()
                        .into_iter()
                        .flatten()
                    {
                        if let Some(path) = path.strip_prefix(&format!("{group}.")) {
                            insert_path(&mut value, path, evidence.clone());
                        }
                    }
                    data[group] = value;
                }
            }
        }
    } else {
        let mut rows = Vec::new();
        for object in ledger
            .objects
            .values()
            .filter(|object| object.resource == request.resource)
        {
            if matches!(request.mode,ReadMode::Inspect{id} if object.id!=id) {
                excluded.insert(object.reference.clone());
                continue;
            }
            let matched = request.filters.iter().fold(Match::True, |result, filter| {
                result.and(generic_match(object, filter))
            });
            if matched == Match::False {
                excluded.insert(object.reference.clone());
                continue;
            }
            found = true;
            kept.insert(object.reference.clone());
            let mut row = object_summary(object);
            row["match_status"] = json!(if matched == Match::Unknown {
                "unresolved"
            } else {
                "matched"
            });
            if matched == Match::Unknown {
                issues.push(issue(
                    Some(object.reference.clone()),
                    "filter_unknown",
                    "required filter evidence is missing",
                ));
            }
            if matches!(request.mode, ReadMode::Inspect { .. }) {
                row =
                    json!({"identity":{"id":object.id},"reference":object.reference,"evidence":{}});
                for (path, evidence) in object.data.as_object().into_iter().flatten() {
                    if selection.paths().any(|selected| {
                        path == selected || path.starts_with(&format!("{selected}."))
                    }) {
                        insert_path(&mut row, path, evidence.clone());
                    }
                }
            }
            if request.resource == Resource::App
                && (matches!(request.mode, ReadMode::List)
                    || selection.paths().any(|path| path.starts_with("windows")))
            {
                let windows=window_rows(ledger).into_iter().filter(|window|window.cg.into_iter().chain(window.ax).any(|window|identity(window).1==object.pid)).map(|window|{kept.extend(window.refs().map(str::to_owned));if selection.paths().any(|path|path=="windows.ax"){json!({"summary":window.summary(),"records":window.cg.into_iter().chain(window.ax).collect::<Vec<_>>()})}else{window.summary()}}).collect::<Vec<_>>();
                row["windows"] = json!(windows);
            }
            if request.resource == Resource::Display
                && selection.wants("spaces")
                && matches!(request.mode, ReadMode::Inspect { .. })
            {
                row["spaces"] = json!(
                    ledger
                        .objects
                        .values()
                        .filter(|space| space.resource == Resource::Space
                            && field(space, "identity.display_id").and_then(Value::as_u64)
                                == object.id)
                        .map(|space| {
                            kept.insert(space.reference.clone());
                            object_summary(space)
                        })
                        .collect::<Vec<_>>()
                );
            }
            rows.push(row);
        }
        data = if matches!(request.mode, ReadMode::List) {
            json!(rows)
        } else if rows.len() == 1 {
            rows.remove(0)
        } else {
            json!({"candidates":rows})
        };
    }
    // Exclusion changes completeness, never erases evidence already collected.
    let blocking = issues.iter().any(|issue| {
        issue
            .target
            .as_ref()
            .is_none_or(|target| !excluded.contains(target) || kept.contains(target))
    });
    let empty = matches!(request.mode, ReadMode::Inspect { .. }) && !found;
    let status = if empty && !blocking && ledger.finished {
        Status::NotFound
    } else if empty || !found && blocking {
        Status::Failed
    } else if !blocking && ledger.finished {
        Status::Complete
    } else {
        Status::Partial
    };
    if empty {
        data = Value::Null;
    }
    Report {
        schema_version: 1,
        source: request.source,
        resource: request.resource,
        status,
        collection: Collection {
            operations: ledger
                .operations
                .values()
                .map(
                    |operation| spool_shared_types::inspection::CollectionOperation {
                        id: operation.work.id.clone(),
                        target: operation.work.target.clone(),
                        source: operation.work.source.clone(),
                        scope: operation.work.scope.clone(),
                        dependencies: operation.work.dependencies.clone(),
                        started_at: operation.started.as_ref().map(|(at, _)| at.clone()),
                        finished_at: operation.ended.as_ref().map(|(at, _, _)| at.clone()),
                        start_offset_ms: operation.started.as_ref().map(|(_, offset)| *offset),
                        finish_offset_ms: operation.ended.as_ref().map(|(_, offset, _)| *offset),
                        status: operation.ended.as_ref().map_or_else(
                            || {
                                if operation.result.is_some() {
                                    "missing_finish".into()
                                } else if operation.started.is_some() {
                                    "interrupted".into()
                                } else {
                                    "budget_skipped".into()
                                }
                            },
                            |(_, _, status)| status.clone(),
                        ),
                        enumeration_complete: operation
                            .work
                            .inventory
                            .then_some(operation.enumeration_complete),
                    },
                )
                .collect(),
            requested: request.clone(),
            started_at: None,
            finished_at: None,
            duration_ms: 0,
        },
        data,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{Progress, Work};
    use super::*;
    use spool_shared_types::inspection::Source;
    fn inventory(ledger: &mut Ledger, source: &str) {
        ledger.operations.insert(
            source.into(),
            Progress {
                ended: Some(("t".into(), 0, "value".into())),
                work: Work {
                    id: source.into(),
                    target: "session".into(),
                    source: source.into(),
                    scope: "windows".into(),
                    dependencies: vec![],
                    inventory: true,
                },
                started: Some(("t".into(), 0)),
                result: None,
                finished: true,
                enumeration_complete: true,
            },
        );
    }
    fn fixture() -> Ledger {
        let mut ledger = Ledger::new("test".into());
        ledger.finished = true;
        inventory(&mut ledger, "cg");
        inventory(&mut ledger, "ax");
        ledger.objects.insert(
            "cg:1".into(),
            Object {
                reference: "cg:1".into(),
                source: "cg".into(),
                resource: Resource::Window,
                id: Some(42),
                pid: Some(100),
                data: json!({"cg.kCGWindowName":{"status":"value","value":"alpha"}}),
            },
        );
        ledger.objects.insert("ax:1".into(),Object{reference:"ax:1".into(),source:"ax".into(),resource:Resource::Window,id:None,pid:None,data:json!({"identity.initial_pid":{"status":"value","value":100},"identity.initial_id":{"status":"value","value":42},"identity.final_pid":{"status":"value","value":100},"identity.final_id":{"status":"value","value":42},"ax.AXTitle.value":{"status":"value","value":"beta"}})});
        ledger
    }
    #[test]
    fn owner_identity_uniqueness_and_revalidation_are_all_required() {
        let mut ledger = fixture();
        assert_eq!(window_rows(&ledger).len(), 1);
        assert_eq!(window_rows(&ledger)[0].association, "consistent");
        ledger.objects.get_mut("ax:1").unwrap().data["identity.final_id"]["value"] = json!(99);
        assert_eq!(window_rows(&ledger).len(), 2);
        let mut ledger = fixture();
        ledger.objects.get_mut("cg:1").unwrap().pid = Some(200);
        assert_eq!(window_rows(&ledger).len(), 2);
        let mut ledger = fixture();
        let mut duplicate = ledger.objects["ax:1"].clone();
        duplicate.reference = "ax:2".into();
        ledger.objects.insert("ax:2".into(), duplicate);
        assert_eq!(window_rows(&ledger).len(), 3);
    }
    #[test]
    fn conflicting_filter_keeps_candidate_but_false_and_unknown_excludes_it() {
        let ledger = fixture();
        let mut request = ReadRequest {
            resource: Resource::Window,
            source: Source::Native,
            mode: ReadMode::List,
            show: vec![],
            filters: vec![Filter::new(Resource::Window, "title", vec!["alpha".into()]).unwrap()],
            timeout_ms: 5000,
        };
        let report = project(&ledger, &request);
        assert_eq!(report.status, Status::Partial);
        assert_eq!(report.data[0]["match_status"], "unresolved");
        request
            .filters
            .push(Filter::new(Resource::Window, "pid", vec!["200".into()]).unwrap());
        let report = project(&ledger, &request);
        assert_eq!(report.status, Status::Complete);
        assert_eq!(report.data, json!([]));
    }
    #[test]
    fn an_unresolved_discovered_identity_is_not_not_found() {
        let mut ledger = fixture();
        ledger.objects.get_mut("ax:1").unwrap().data["identity.initial_id"] =
            json!({"status":"unavailable"});
        let request = ReadRequest::detail(Resource::Window, Source::Native, Some(99));
        assert_eq!(project(&ledger, &request).status, Status::Failed);
    }
    #[test]
    fn unfiltered_source_disagreement_preserves_raw_values_without_downgrade() {
        let ledger = fixture();
        let report = project(
            &ledger,
            &ReadRequest::detail(Resource::Window, Source::Native, Some(42)),
        );
        assert_eq!(report.status, Status::Complete);
        assert_eq!(report.data["records"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn membership_filters_compare_both_endpoints_and_map_all_spaces() {
        let mut ledger = fixture();
        inventory(&mut ledger, "skylight");
        ledger.objects.insert(
            "membership:42".into(),
            Object {
                reference: "membership:42".into(),
                source: "skylight".into(),
                resource: Resource::Window,
                id: Some(42),
                pid: None,
                data: json!({"spaces.window_to_spaces":{"status":"value","value":[10,20]}}),
            },
        );
        for (id, display) in [(10, 1), (20, 2)] {
            ledger.objects.insert(format!("space:{id}"), Object {
                reference: format!("space:{id}"), source: "skylight".into(), resource: Resource::Space, id: Some(id), pid: None,
                data: json!({"windows":{"status":"value","value":[42]},"identity.display_id":{"status":"value","value":display}}),
            });
        }
        let display = Filter::new(Resource::Window, "display", vec!["2".into()]).unwrap();
        assert_eq!(
            window_rows(&ledger)[0].matches(&ledger, &display),
            Match::True
        );
        ledger.objects.get_mut("space:20").unwrap().data["windows"]["value"] = json!([]);
        assert_eq!(
            window_rows(&ledger)[0].matches(&ledger, &display),
            Match::Unknown
        );
        ledger.objects.get_mut("membership:42").unwrap().data["spaces.window_to_spaces"]["value"] =
            json!([10]);
        assert_eq!(
            window_rows(&ledger)[0].matches(&ledger, &display),
            Match::False
        );
    }

    #[test]
    fn excluded_row_errors_remain_visible_without_downgrading_complete() {
        let mut ledger = fixture();
        ledger.issues.push(issue(
            Some("ax:1".into()),
            "unavailable",
            "AX field unavailable",
        ));
        let request = ReadRequest {
            resource: Resource::Window,
            source: Source::Native,
            mode: ReadMode::List,
            show: vec![],
            filters: vec![Filter::new(Resource::Window, "pid", vec!["999".into()]).unwrap()],
            timeout_ms: 5000,
        };
        let report = project(&ledger, &request);
        assert_eq!(report.status, Status::Complete);
        assert_eq!(report.data, json!([]));
        assert_eq!(report.issues.len(), 1);
        assert_eq!(report.issues[0].target.as_deref(), Some("ax:1"));
    }
}
