//! Projection of retained ECS state. This module must never fetch native data.

use crate::config::Config;
use crate::ecs::layout::{Column, LayoutStrip};
use crate::ecs::native_space::{NativeMoveOwner, NativeSpace, VisibleNativeSpaceMarker};
use crate::ecs::reconcile::WindowUnavailable;
use crate::ecs::tiled_visibility::ParkedTile;
use crate::ecs::workspace::WindowSpaceReassignmentPending;
use crate::ecs::{
    ActiveDisplayMarker, ActiveWorkspaceMarker, DesiredWindowFrame, DockPosition, Floating,
    FocusedMarker, ObservedWindowFrame, PresentedWindowFrame, PreviousTiledStrip,
    WindowFrameCommitSuspended, WindowFrameMotion, WindowVisibility,
};
use crate::manager::{Application, Display, Window};
use bevy::{ecs::system::SystemParam, prelude::*};
use serde_json::{Value, json};
use spool_shared_types::inspection::{
    Collection, FieldEvidence, Issue, Match, ReadMode, ReadRequest, Report, Resource, Selection,
    Source, Status,
};

type WindowRows<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Window,
        &'static ChildOf,
        Has<Floating>,
        Has<FocusedMarker>,
        Option<&'static WindowVisibility>,
        Has<WindowUnavailable>,
        Has<ParkedTile>,
        Option<&'static PreviousTiledStrip>,
        Option<&'static crate::ecs::native_space::SpaceMoveAttempt>,
        Option<&'static crate::ecs::native_space::DeclaredSpace>,
        Option<&'static crate::ecs::floating_geometry::FloatingGeometry>,
    ),
>;
type GeometryRows<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static DesiredWindowFrame>,
        Option<&'static PresentedWindowFrame>,
        Option<&'static ObservedWindowFrame>,
        Has<WindowFrameMotion>,
        Has<NativeMoveOwner>,
        Has<WindowSpaceReassignmentPending>,
        Has<WindowFrameCommitSuspended>,
    ),
    With<Window>,
>;
type StripRows<'w, 's> = Query<
    'w,
    's,
    (
        &'static LayoutStrip,
        &'static ChildOf,
        Option<&'static NativeSpace>,
        Has<VisibleNativeSpaceMarker>,
        Has<ActiveWorkspaceMarker>,
    ),
>;

#[derive(SystemParam)]
pub(crate) struct Projection<'w, 's> {
    windows: WindowRows<'w, 's>,
    geometry: GeometryRows<'w, 's>,
    apps: Query<'w, 's, (Entity, &'static Application)>,
    strips: StripRows<'w, 's>,
    // Persistence includes retained strips even when their display parent is
    // temporarily absent; diagnostics must capture the same domain snapshot.
    all_strips: Query<'w, 's, &'static LayoutStrip>,
    displays: Query<
        'w,
        's,
        (
            Entity,
            &'static Display,
            Option<&'static DockPosition>,
            Has<ActiveDisplayMarker>,
        ),
    >,
    config: Res<'w, Config>,
    sync: Res<'w, crate::ecs::reconcile::WindowStateSync>,
    focus: Res<'w, crate::ecs::focus::FocusCoordinator>,
    persistence: ResMut<'w, crate::ecs::state::StatePersistence>,
    lifecycle: Res<'w, crate::lifecycle::Lifecycle>,
}

fn unknown() -> Value {
    json!({"status": "not_recorded"})
}
fn recorded<T: serde::Serialize>(value: Option<T>) -> Value {
    value
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or_else(unknown)
}
fn frame(frame: IRect) -> Value {
    json!({"x":frame.min.x,"y":frame.min.y,"width":i64::from(frame.max.x)-i64::from(frame.min.x),"height":i64::from(frame.max.y)-i64::from(frame.min.y),"units":"points","coordinates":"global_y_down"})
}
fn column_kind(column: &Column) -> &'static str {
    match column {
        Column::Stack(_) => "stack",
        Column::Tabs(_) => "tabs",
        Column::Fullscreen(_) => "fullscreen",
        Column::Single(_) => "single",
    }
}

struct Budget {
    started: std::time::Instant,
    duration: std::time::Duration,
    remaining: std::cell::Cell<usize>,
    exhausted: std::cell::Cell<bool>,
}
impl Budget {
    fn admit(&self) -> bool {
        if self.remaining.get() == 0 || self.started.elapsed() >= self.duration {
            self.exhausted.set(true);
            return false;
        }
        self.remaining.set(self.remaining.get() - 1);
        true
    }
}

impl Projection<'_, '_> {
    fn capture_current_intent(&mut self) -> std::io::Result<()> {
        let snapshot = crate::ecs::state::SpoolState::from_layouts(
            self.all_strips.iter(),
            |entity| {
                let (_, window, parent, ..) = self.windows.get(entity).ok()?;
                let (_, app) = self.apps.get(parent.parent()).ok()?;
                Some(crate::ecs::state::SavedWindow {
                    window_id: window.id(),
                    pid: app.pid(),
                    bundle_id: app.bundle_id().unwrap_or_default().clone(),
                })
            },
            self.windows.iter().filter_map(|row| {
                let (_, window, parent, .., geometry) = row;
                let geometry = geometry?;
                let (_, app) = self.apps.get(parent.parent()).ok()?;
                let frame = geometry.frame;
                Some(crate::ecs::state::SavedFloatingWindow {
                    window_id: window.id(),
                    pid: app.pid(),
                    bundle_id: app.bundle_id().unwrap_or_default().clone(),
                    frame: spool_shared_types::state::Frame {
                        x: frame.min.x,
                        y: frame.min.y,
                        width: frame.width(),
                        height: frame.height(),
                    },
                })
            }),
            self.windows.iter().filter_map(|row| {
                let (_, window, parent, .., declared, _) = row;
                let declared = declared?;
                let (_, app) = self.apps.get(parent.parent()).ok()?;
                Some(crate::ecs::state::SavedMembership {
                    window_id: window.id(),
                    pid: app.pid(),
                    bundle_id: app.bundle_id().unwrap_or_default().clone(),
                    space_id: declared.target?,
                })
            }),
        );
        self.persistence.capture(snapshot).map(|_| ())
    }

    fn window_rows(&self, budget: &Budget) -> Vec<Value> {
        self.windows.iter().take_while(|_| budget.admit()).map(|(entity, window, parent, floating, focused, hidden, unavailable, parked, previous, attempt, declared, floating_geometry)| {
            let app = self.apps.get(parent.parent()).ok().map(|(_, app)| app);
            let geometry = self.geometry.get(entity).ok();
            let observed = geometry.and_then(|row| row.2).map(|frame| frame.0);
            let owners = self.strips.iter().filter(|(strip, _, _, _, _)| strip.contains(entity)).collect::<Vec<_>>();
            let space_id = if owners.len() == 1 { Some(owners[0].0.id()) } else if owners.is_empty() { previous.map(|previous| previous.workspace_id) } else { None };
            let column = (owners.len() == 1).then(|| owners[0].0.index_of(entity).ok()).flatten().map(|index| index + 1);
            let display_id = space_id.and_then(|space| self.strips.iter().find(|(strip, _, _, _, _)| strip.id() == space))
                .and_then(|(_, parent, _, _, _)| self.displays.get(parent.parent()).ok()).map(|(_, display, _, _)| display.id());
            let space_visible = space_id.and_then(|id| self.strips.iter().find(|(strip, _, _, _, _)| strip.id() == id).map(|row| row.3));
            let on_screen = if hidden.is_some() || parked || unavailable { Some(false) } else {
                observed.and_then(|frame| {
                    let max_overlap = self.displays.iter().map(|(_, display, _, _)| frame.intersect(display.bounds()))
                        .filter(|rect| rect.width() > 0 && rect.height() > 0)
                        .max_by_key(|rect| i64::from(rect.width()) * i64::from(rect.height()));
                    match (space_visible, max_overlap) {
                        (Some(false), _) | (_, None) => Some(false),
                        (Some(true), Some(overlap)) => Some(overlap.width() > self.config.sliver_width()),
                        (None, _) => None,
                    }
                })
            };
            json!({
                "identity":{"id":window.id(),"pid":recorded(app.map(|app| app.pid())),"title":recorded(window.retained_title()),"bundle_id":recorded(app.and_then(|app| app.bundle_id())),"app_name":recorded(app.map(|app| app.name()))},
                "geometry":{"realization": self.sync.frame_progress(entity).map(|progress| json!({"attempts":progress.attempts,"active":progress.active,"blocked":progress.blocked,"confirmed":progress.confirmed,"checks_pending":progress.checks_pending})),"desired":recorded(geometry.and_then(|row| row.0).map(|frame_| frame(frame_.0))),"presented":recorded(geometry.and_then(|row| row.1).map(|frame_| frame(frame_.0))),"observed":recorded(observed.map(frame))},
                "layout":{"space_id":recorded(space_id),"column":column,"display_id":recorded(display_id),"previous_column":previous.map(|previous| previous.index+1)},
                "floating":recorded(floating_geometry.map(|geometry| json!({"intent":frame(geometry.frame),"unresolved":geometry.unresolved,"repairs":geometry.repairs.iter().map(|repair| json!({"from":frame(repair.from),"to":frame(repair.to),"reason":repair.reason})).collect::<Vec<_>>()}))),
                "membership":{"declared":recorded(declared.map(|declared| declared.target)),"observed":recorded(declared.map(|declared| declared.observed)),"repairs":recorded(declared.map(|declared| declared.repairs.iter().map(|repair| json!({"from":repair.from,"to":repair.to,"reason":repair.reason})).collect::<Vec<_>>())),"attempt":recorded(attempt.map(|attempt| json!({"target_space_id":attempt.target_space_id,"result":attempt.result.name(),"code":attempt.result.code()})))},
                "state":{"available":!unavailable,"floating":floating,"focused":focused,"visible":hidden.is_none()&&!parked,"minimized":matches!(hidden,Some(WindowVisibility::Minimized)),"on_screen":recorded(on_screen),"motion":geometry.is_some_and(|row|row.3),"migration":geometry.is_some_and(|row|row.4||row.5),"blockers":{"unavailable":unavailable,"native_move":geometry.is_some_and(|row|row.4),"space_reassignment":geometry.is_some_and(|row|row.5),"parked":parked,"commit_suspended":geometry.is_some_and(|row|row.6)}}
            })
        }).collect()
    }

    fn display_rows(&self, budget: &Budget) -> Vec<Value> {
        self.displays.iter().take_while(|_| budget.admit()).map(|(entity, display, dock, active)| {
            let spaces = self.strips.iter().filter(|(_, parent, _, _, _)| parent.parent()==entity).collect::<Vec<_>>();
            json!({"identity":{"id":display.id(),"name":unknown(),"uuid":unknown(),"main":unknown()},
                "geometry":{"bounds":frame(display.bounds()),"usable_frame":recorded(display.checked_actual_display_bounds(dock,&self.config).map(frame)),"scale":unknown()},
                "spaces":{"ids":spaces.iter().map(|row|row.0.id()).collect::<Vec<_>>(),"visible_id":spaces.iter().find(|row|row.3).map(|row|row.0.id())},"state":{"active":active}})
        }).collect()
    }

    fn space_rows(&self, windows: &[Value], budget: &Budget) -> Vec<Value> {
        self.strips.iter().take_while(|_| budget.admit()).map(|(strip,parent,native,visible,active)| {
            let display = self.displays.get(parent.parent()).ok().map(|row|row.1.id());
            json!({"identity":{"id":strip.id(),"display_id":recorded(display),"ordinal":recorded(native.map(|space|space.ordinal+1)),"kind":recorded(native.map(|space|space.kind))},
                "state":{"visible":visible,"active":active},"focus":{"preference_window_id":self.focus.preference_entity(strip.id()).and_then(|entity|self.windows.get(entity).ok().map(|row|row.1.id())),"selection_window_id":self.focus.navigation_entity(strip.id()).and_then(|entity|self.windows.get(entity).ok().map(|row|row.1.id()))},"windows":windows.iter().filter(|row|row["layout"]["space_id"]==strip.id()).map(window_summary).collect::<Vec<_>>()})
        }).collect()
    }

    fn app_rows(&self, windows: &[Value], budget: &Budget) -> Vec<Value> {
        self.apps.iter().take_while(|_| budget.admit()).map(|(_,app)| {
            let members=windows.iter().filter(|row|row["identity"]["pid"]==app.pid()).collect::<Vec<_>>();
            json!({"identity":{"id":app.pid(),"pid":app.pid(),"name":app.name(),"bundle_id":recorded(app.bundle_id())},
                "state":{"hidden":unknown(),"frontmost":unknown(),"window_count":members.len()},"windows":members.into_iter().map(window_summary).collect::<Vec<_>>()})
        }).collect()
    }

    fn layout_rows(&self, budget: &Budget) -> Vec<Value> {
        self.strips.iter().take_while(|_| budget.admit()).map(|(strip,parent,_,visible,active)| {
            json!({"identity":{"id":strip.id(),"space_id":strip.id(),"display_id":recorded(self.displays.get(parent.parent()).ok().map(|row|row.1.id()))},
                "state":{"visible":visible,"active":active,"accepted_revision":self.persistence.accepted_revision(),"saved_revision":self.persistence.saved_revision(),"dirty":self.persistence.is_dirty()},"columns":strip.columns().enumerate().map(|(index,column)| {
                    json!({"ordinal":index+1,"id":strip.column_state(index).map(|state| state.id.0),"width_intent":strip.column_state(index).map(|state| state.width),"intent_revision":strip.column_state(index).map(|state| state.intent_revision),"width_projection":match strip.effective_column_width(index) { Ok(value) => json!({"effective":value.slot,"requested":value.requested,"constrained":value.constrained}), Err(reason) => json!({"blocked":format!("{reason:?}")}) },"height_revision":strip.column_state(index).map(|state| state.height_revision),"height_items":strip.column_height_items(index).map(|items| items.iter().map(|item|json!({"id":item.id.0,"weight":item.weight,"intent_revision":item.intent_revision,"windows":item.members.iter().filter_map(|entity|self.windows.get(*entity).ok().map(|row|row.1.id())).collect::<Vec<_>>()})).collect::<Vec<_>>()),"height_projection":match strip.effective_stack_heights_for(index,self.displays.get(parent.parent()).ok().and_then(|row|row.1.checked_actual_display_bounds(row.2,&self.config)).map(|bounds|bounds.height()),&|entity|self.windows.get(entity).is_ok_and(|row|!row.6)) {Ok(values)=>json!(values.into_iter().map(|(id,value)|json!({"id":id.0,"requested":value.requested,"effective":value.slot,"constrained":value.constrained})).collect::<Vec<_>>()),Err(reason)=>json!({"blocked":format!("{reason:?}")})},"kind":column_kind(column),"windows":column.window_iter().map(|entity| {
                        self.windows.get(entity).ok().map_or_else(unknown, |row|json!({"id":row.1.id(),"available":!row.6,"visible":row.5.is_none()&&!row.7}))
                    }).collect::<Vec<_>>()})
                }).collect::<Vec<_>>()})
        }).collect()
    }
}

fn window_summary(row: &Value) -> Value {
    json!({"id":row["identity"]["id"],"pid":row["identity"]["pid"],"title":row["identity"]["title"],"bundle_id":row["identity"]["bundle_id"],"space_id":row["layout"]["space_id"],"display_id":row["layout"]["display_id"],"available":row["state"]["available"],"floating":row["state"]["floating"],"on_screen":row["state"]["on_screen"],"minimized":row["state"]["minimized"]})
}
fn summary(resource: Resource, row: &Value) -> Value {
    match resource {
        Resource::Window => window_summary(row),
        Resource::Space => {
            json!({"id":row["identity"]["id"],"display_id":row["identity"]["display_id"],"ordinal":row["identity"]["ordinal"],"kind":row["identity"]["kind"],"visible":row["state"]["visible"],"window_count":row["windows"].as_array().map(Vec::len)})
        }
        Resource::Display => {
            json!({"id":row["identity"]["id"],"name":row["identity"]["name"],"main":row["identity"]["main"],"bounds":row["geometry"]["bounds"],"visible_space_id":row["spaces"]["visible_id"]})
        }
        Resource::App => {
            json!({"pid":row["identity"]["pid"],"name":row["identity"]["name"],"bundle_id":row["identity"]["bundle_id"],"hidden":row["state"]["hidden"],"window_count":row["state"]["window_count"]})
        }
        _ => row.clone(),
    }
}
fn filter_value<'a>(resource: Resource, row: &'a Value, field: &str) -> &'a Value {
    match field {
        "space" => &row["layout"]["space_id"],
        "display" if resource == Resource::Window => &row["layout"]["display_id"],
        "display" => &row["identity"]["display_id"],
        "bundle-id" => &row["identity"]["bundle_id"],
        "on-screen" => &row["state"]["on_screen"],
        "visible" | "hidden" | "minimized" => &row["state"][field],
        _ => &row["identity"][field],
    }
}

fn project(row: &Value, selection: &Selection) -> Value {
    fn visit(value: &Value, path: &str, selection: &Selection) -> Option<Value> {
        if selection.wants(path) {
            return Some(value.clone());
        }
        let object = value.as_object()?;
        let selected = object
            .iter()
            .filter_map(|(key, value)| {
                let path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                visit(value, &path, selection).map(|value| (key.clone(), value))
            })
            .collect::<serde_json::Map<_, _>>();
        (!selected.is_empty()).then_some(Value::Object(selected))
    }
    let mut result = visit(row, "", selection).unwrap_or_else(|| json!({}));
    if result.get("identity").is_none() {
        result["identity"] = json!({"id":row["identity"]["id"]});
    }
    result
}

fn collect_unknowns(value: &Value, path: &str, issues: &mut Vec<Issue>) {
    if value.get("status").and_then(Value::as_str) == Some("not_recorded") {
        issues.push(issue("not_recorded", path));
    } else if let Some(object) = value.as_object() {
        for (key, value) in object {
            collect_unknowns(value, &format!("{path}.{key}"), issues);
        }
    } else if let Some(array) = value.as_array() {
        for (index, value) in array.iter().enumerate() {
            collect_unknowns(value, &format!("{path}[{index}]"), issues);
        }
    }
}
fn issue(code: &str, target: &str) -> Issue {
    Issue {
        source: "spool".into(),
        operation: "project".into(),
        target: Some(target.into()),
        code: code.into(),
        message: code.replace('_', " "),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
pub(crate) fn collect(In(request): In<ReadRequest>, mut state: Projection) -> Report {
    let started = std::time::Instant::now();
    let started_at = chrono::Utc::now().to_rfc3339();
    let failed = if let Err(error) = request.validate() {
        Some(("invalid_request", error))
    } else if request.source != Source::Spool {
        Some((
            "invalid_source",
            "daemon projections only support spool".into(),
        ))
    } else {
        state
            .lifecycle
            .rejection()
            .map(|reason| (reason, reason.into()))
    };
    if let Some((code, message)) = failed {
        return Report::failure(&request, code, &message);
    }
    let selection = match Selection::new(request.resource, request.source, &request.show) {
        Ok(selection) => selection,
        Err(error) => return Report::failure(&request, "invalid_request", &error),
    };
    // An edit and a query can run in one command batch before PostUpdate.
    // Capture now so current widths never appear beside a stale clean revision.
    if request.resource == Resource::SpaceLayout
        && let Err(error) = state.capture_current_intent()
    {
        return Report::failure(&request, "intent_capture_failed", &error.to_string());
    }
    let capture_budget = Budget {
        started,
        duration: std::time::Duration::from_millis(request.timeout_ms),
        remaining: std::cell::Cell::new(8192),
        exhausted: std::cell::Cell::new(false),
    };
    let windows = if request.resource == Resource::Display {
        Vec::new()
    } else {
        state.window_rows(&capture_budget)
    };
    let mut rows = match request.resource {
        Resource::Window => windows,
        Resource::Display => state.display_rows(&capture_budget),
        Resource::Space => state.space_rows(&windows, &capture_budget),
        Resource::App => state.app_rows(&windows, &capture_budget),
        Resource::SpaceLayout => state.layout_rows(&capture_budget),
        Resource::Session => vec![
            json!({"identity":{},"focus":state.focus.activation_diagnostics(),"active":{"display_id":state.displays.iter().find(|row|row.3).map(|row|row.1.id()),"space_id":state.strips.iter().find(|row|row.4).map(|row|row.0.id()),"window_id":state.windows.iter().find(|row|row.4).map(|row|row.1.id())},"displays":state.display_rows(&capture_budget).iter().map(|row|summary(Resource::Display,row)).collect::<Vec<_>>(),"spaces":state.space_rows(&windows, &capture_budget).iter().map(|row|summary(Resource::Space,row)).collect::<Vec<_>>(),"windows":windows.iter().map(window_summary).collect::<Vec<_>>(),"apps":state.app_rows(&windows, &capture_budget).iter().map(|row|summary(Resource::App,row)).collect::<Vec<_>>(),"capabilities":{"space_control_enabled":state.config.space_control_enabled(),"native":unknown()}}),
        ],
    };
    rows.sort_by_key(|row| row["identity"]["id"].as_u64().unwrap_or_default());
    let mut issues = Vec::new();
    if capture_budget.exhausted.get() {
        issues.push(issue("truncated", "collection"));
    }
    let mut output = Vec::new();
    let mut bytes = 0usize;
    let budget = std::time::Duration::from_millis(request.timeout_ms);
    for row in rows {
        if started.elapsed() >= budget {
            issues.push(issue("timed_out", "collection"));
            break;
        }
        if let ReadMode::Inspect { id } = request.mode {
            if let Some(id) = id {
                if row["identity"]["id"] != id {
                    continue;
                }
            } else if request.resource == Resource::SpaceLayout && row["state"]["active"] != true {
                continue;
            }
        }
        let matched = request.filters.iter().fold(Match::True, |matched, filter| {
            let value = filter_value(request.resource, &row, &filter.field);
            let evidence = if value.is_null() || value.get("status").is_some() {
                FieldEvidence::Unavailable
            } else {
                FieldEvidence::Value(value.clone())
            };
            matched.and(filter.evaluate(&[evidence]))
        });
        if matched == Match::False {
            continue;
        }
        let mut projected = if matches!(request.mode, ReadMode::List) {
            summary(request.resource, &row)
        } else {
            project(&row, &selection)
        };
        if matches!(request.mode, ReadMode::List) {
            projected["match_status"] = json!(if matched == Match::Unknown {
                "unresolved"
            } else {
                "matched"
            });
        }
        if matched == Match::Unknown {
            issues.push(issue("filter_unknown", &row["identity"]["id"].to_string()));
        }
        collect_unknowns(&projected, &row["identity"]["id"].to_string(), &mut issues);
        let size = serde_json::to_vec(&projected).map_or(usize::MAX, |bytes| bytes.len());
        bytes = bytes.saturating_add(size);
        if bytes > 2 * 1024 * 1024 {
            issues.push(issue("truncated", "collection"));
            break;
        }
        output.push(projected);
    }
    let mut status = if issues.is_empty() {
        Status::Complete
    } else {
        Status::Partial
    };
    let data = if matches!(request.mode, ReadMode::List) {
        if output.is_empty() && !issues.is_empty() {
            status = Status::Failed;
        }
        json!(output)
    } else {
        match output.len() {
            0 => {
                status = if issues.is_empty() {
                    Status::NotFound
                } else {
                    Status::Failed
                };
                Value::Null
            }
            1 => output.remove(0),
            _ => {
                status = Status::Partial;
                issues.push(issue("ambiguous_identity", "target"));
                json!({"candidates":output})
            }
        }
    };
    Report {
        schema_version: 1,
        source: Source::Spool,
        resource: request.resource,
        status,
        collection: Collection {
            operations: Vec::new(),
            requested: request,
            started_at: Some(started_at),
            finished_at: Some(chrono::Utc::now().to_rfc3339()),
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
        data,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use bevy::ecs::system::RunSystemOnce;
    use spool_shared_types::inspection::{ReadRequest, Resource, Source};

    #[test]
    fn window_detail_reports_the_last_membership_attempt() {
        let mut harness = crate::tests::TestHarness::new().with_windows(2);
        harness.pump_frames(10);
        let entity = crate::tests::find_window_entity(1, harness.world());
        harness.world().entity_mut(entity).insert((
            crate::ecs::native_space::SpaceMoveAttempt {
                target_space_id: crate::tests::TEST_WORKSPACE_ID + 1,
                result: crate::ecs::native_space::SpaceMoveResult::Refused(
                    "capability_unavailable".into(),
                ),
            },
            crate::ecs::native_space::DeclaredSpace {
                target: Some(crate::tests::TEST_WORKSPACE_ID),
                observed: Some(crate::tests::TEST_WORKSPACE_ID + 1),
                repairs: vec![crate::ecs::native_space::SpaceRepair {
                    from: Some(crate::tests::TEST_WORKSPACE_ID + 1),
                    to: Some(crate::tests::TEST_WORKSPACE_ID),
                    reason: "target_space_destroyed",
                }],
            },
        ));

        let mut request = ReadRequest::detail(Resource::Window, Source::Spool, Some(1));
        request.show = vec!["membership".into()];
        let report = harness
            .world()
            .run_system_once_with(super::collect, request)
            .unwrap();

        assert_eq!(report.data["membership"]["attempt"]["result"], "refused");
        assert_eq!(
            report.data["membership"]["attempt"]["code"],
            "capability_unavailable"
        );
        assert_eq!(
            report.data["membership"]["attempt"]["target_space_id"],
            crate::tests::TEST_WORKSPACE_ID + 1
        );
        assert_eq!(
            report.data["membership"]["declared"],
            crate::tests::TEST_WORKSPACE_ID
        );
        assert_eq!(
            report.data["membership"]["observed"],
            crate::tests::TEST_WORKSPACE_ID + 1
        );
        assert_eq!(
            report.data["membership"]["repairs"][0]["reason"],
            "target_space_destroyed"
        );
    }

    #[test]
    fn geometry_detail_reads_retained_state_and_preserves_focus() {
        let mut harness = crate::tests::TestHarness::new().with_windows(2);
        harness.pump_frames(10);
        harness.mock_state.take_focus_requests();
        let mut request = ReadRequest::detail(Resource::Window, Source::Spool, Some(1));
        request.show = vec!["geometry.desired".into()];
        let native_reads = (
            harness.mock_state.display_observation_count(),
            harness.mock_state.workspace_membership_query_count(),
        );
        let report = harness
            .world()
            .run_system_once_with(super::collect, request)
            .unwrap();
        assert_eq!(report.data["identity"]["id"], 1);
        assert!(report.data["geometry"]["desired"].is_object());
        assert!(report.data.get("state").is_none());
        assert!(report.data.get("layout").is_none());
        assert!(harness.mock_state.take_focus_requests().is_empty());
        assert_eq!(
            (
                harness.mock_state.display_observation_count(),
                harness.mock_state.workspace_membership_query_count()
            ),
            native_reads
        );
    }

    #[test]
    fn layout_inspection_after_an_immediate_edit_reports_unsaved_current_intent() {
        use crate::commands::Action;
        use crate::ecs::state::{StateFilePath, StatePersistence, periodic_state_save};
        use crate::events::Event;
        use crate::tests::{TEST_WORKSPACE_ID, TestHarness};
        use bevy::app::PreUpdate;
        use spool_shared_types::commands::{ColumnWidth, SpaceLayoutOperation};

        let mut harness = TestHarness::new().with_windows(1);
        harness.pump_frames(10);
        harness
            .world()
            .run_system_once(periodic_state_save)
            .unwrap();
        let path = harness
            .world()
            .resource::<StateFilePath>()
            .as_path()
            .to_path_buf();
        let saved_bytes = std::fs::read(&path).unwrap();
        let saved_revision = harness
            .world()
            .resource::<StatePersistence>()
            .saved_revision()
            .unwrap();
        harness
            .world()
            .write_message(Event::action_requested(Action::SpaceLayout {
                space_id: Some(TEST_WORKSPACE_ID),
                operation: SpaceLayoutOperation::SetWidth {
                    column: 1,
                    width: ColumnWidth::Points(812.0),
                },
            }));
        harness.world().run_schedule(PreUpdate);
        // Do not run Update/PostUpdate: this models the next request in the
        // same IPC batch, before the normal periodic capture system can run.
        let native_activity = (
            harness.mock_state.display_observation_count(),
            harness.mock_state.workspace_membership_query_count(),
            harness.mock_state.frame_write_attempts(0),
        );
        let mut request = ReadRequest::detail(
            Resource::SpaceLayout,
            Source::Spool,
            Some(TEST_WORKSPACE_ID),
        );
        request.show = vec!["state".into(), "columns".into()];
        let report = harness
            .world()
            .run_system_once_with(super::collect, request)
            .unwrap();
        assert_eq!(
            report.data["columns"][0]["width_intent"],
            serde_json::json!({"Absolute": 812.0})
        );
        assert_eq!(report.data["state"]["saved_revision"], saved_revision);
        assert!(report.data["state"]["accepted_revision"].as_u64().unwrap() > saved_revision);
        assert_eq!(report.data["state"]["dirty"], true);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            saved_bytes,
            "inspection must not save the new intent"
        );
        assert_eq!(
            native_activity,
            (
                harness.mock_state.display_observation_count(),
                harness.mock_state.workspace_membership_query_count(),
                harness.mock_state.frame_write_attempts(0),
            ),
            "inspection capture must not read or write native state"
        );
    }

    /// Raw diagnostics keep every retained item, but the effective projection
    /// must use the same participant policy as runtime projection. An
    /// ordered-out sibling reserves no height and cannot create a false block.
    #[test]
    fn height_diagnostics_keep_ordered_out_items_raw_without_reserving_effective_height() {
        use crate::ecs::layout::LayoutStrip;
        use crate::tests::{TEST_WORKSPACE_ID, TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(20);
        let first = find_window_entity(0, harness.world());
        let second = find_window_entity(1, harness.world());
        {
            let world = harness.world();
            let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
            strip.stack(second).unwrap();
            assert_eq!(strip.column_height_items(0).unwrap().len(), 2);
            assert!(strip.height_viewport().is_some());
        }
        harness.mock_state.update_window(1, |window| {
            window.published = false;
            window.ordered_out = true;
            window.visible = false;
        });
        harness.pump_frames(40);
        assert!(
            harness
                .world()
                .get::<super::WindowUnavailable>(second)
                .is_some_and(super::WindowUnavailable::excludes_from_layout_projection),
            "the sibling must be ordered out before the diagnostic is captured"
        );
        let viewport = {
            let world = harness.world();
            let strip = world.query::<&LayoutStrip>().single(world).unwrap();
            strip
                .height_viewport()
                .expect("a resolved display viewport")
        };
        let mut request = ReadRequest::detail(
            Resource::SpaceLayout,
            Source::Spool,
            Some(TEST_WORKSPACE_ID),
        );
        request.show = vec!["state".into(), "columns".into()];
        let report = harness
            .world()
            .run_system_once_with(super::collect, request)
            .unwrap();
        let columns = report.data["columns"].as_array().unwrap();
        let column = columns
            .iter()
            .find(|column| {
                column["height_items"]
                    .as_array()
                    .is_some_and(|items| items.len() == 2)
            })
            .expect("the stacked column keeps both raw height items");
        assert_eq!(
            column["height_items"].as_array().unwrap().len(),
            2,
            "raw diagnostics must keep the ordered-out item"
        );
        let projection = column["height_projection"]
            .as_array()
            .expect("an ordered-out sibling cannot make the projection infeasible");
        assert_eq!(
            projection.len(),
            1,
            "only the participating item reserves effective height"
        );
        assert_eq!(
            projection[0]["id"], column["height_items"][0]["id"],
            "the effective projection must identify its item"
        );
        assert_eq!(projection[0]["effective"], viewport);
        assert_eq!(
            projection[0]["constrained"], false,
            "a single participant fills the viewport without a minimum"
        );
        let _ = (first, second);
    }

    #[test]
    fn height_inspection_captures_current_raw_intent_before_the_next_schedule() {
        use crate::ecs::layout::LayoutStrip;
        use crate::ecs::state::StatePersistence;
        use crate::tests::{TEST_WORKSPACE_ID, TestHarness, find_window_entity};
        let mut harness = TestHarness::new().with_windows(2);
        harness.pump_frames(10);
        let first = find_window_entity(0, harness.world());
        let second = find_window_entity(1, harness.world());
        let previous = harness
            .world()
            .resource::<StatePersistence>()
            .accepted_revision();
        let item_id = {
            let world = harness.world();
            let mut strip = world.query::<&mut LayoutStrip>().single_mut(world).unwrap();
            strip.stack(second).unwrap();
            strip.set_height_weight(first, 3.0).unwrap();
            strip.height_state(first).unwrap().id.0
        };
        let activity = (
            harness.mock_state.display_observation_count(),
            harness.mock_state.frame_write_attempts(0),
        );
        let mut request = ReadRequest::detail(
            Resource::SpaceLayout,
            Source::Spool,
            Some(TEST_WORKSPACE_ID),
        );
        request.show = vec!["state".into(), "columns".into()];
        let report = harness
            .world()
            .run_system_once_with(super::collect, request)
            .unwrap();
        assert_eq!(report.data["columns"][0]["height_items"][0]["id"], item_id);
        assert_eq!(report.data["columns"][0]["height_items"][0]["weight"], 3.0);
        assert!(report.data["columns"][0]["height_projection"].is_array());
        assert!(report.data["state"]["accepted_revision"].as_u64().unwrap() > previous);
        assert_eq!(
            activity,
            (
                harness.mock_state.display_observation_count(),
                harness.mock_state.frame_write_attempts(0)
            )
        );
    }
}
