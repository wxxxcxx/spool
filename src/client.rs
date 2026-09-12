//! The `spool …` CLI side of the protocol: sends requests to the running
//! daemon and renders the answer as either JSON or a compact tab-separated
//! summary. The daemon and its clients otherwise speak typed postcard values.

use spool_local_ipc::Client;
use spool_shared_types::commands::Action;
use spool_shared_types::state::{ActiveState, StateEvent};
use spool_shared_types::wire::{Request, Response, service_name};

#[cfg(test)]
use spool_shared_types::state::{Frame, SpaceKind, SpaceState, WindowState};

use crate::errors::{Error, Result};
#[cfg(test)]
use spool_shared_types::wire::QueryPayload;

/// Streams state events to stdout until interrupted.
///
/// # Errors
///
/// If the daemon cannot be reached.
pub fn subscribe(format: OutputFormat, raw: bool, timeout_ms: u64) -> Result<()> {
    use std::io::Write;

    let mut events = Client::connect_with_deadline(
        &service_name(),
        std::time::Duration::from_millis(timeout_ms),
    )?
    .subscribe(&Request::Subscribe { raw })?;
    let mut stdout = std::io::stdout();
    if format == OutputFormat::Tsv
        && writeln!(
            stdout,
            "EVENT\tDISPLAY_ID\tSPACE_ID\tWINDOW_ID\tBUNDLE_ID\tTITLE\tDETAILS"
        )
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return Ok(());
    }

    loop {
        let event = match events.recv_blocking() {
            Ok(event) => event,
            // The daemon exiting ends the subscription normally.
            Err(spool_local_ipc::Error::PeerGone) => break,
            Err(err) => return Err(Error::from(err)),
        };

        let line = render_event(&event, format)?;
        // Flush per line: callers pipe this into readers expecting immediate lines.
        if writeln!(stdout, "{line}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
    }
    Ok(())
}

/// How a CLI query or event stream is rendered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Tsv,
    Json,
}

impl OutputFormat {
    #[must_use]
    pub const fn from_json(json: bool) -> Self {
        if json { Self::Json } else { Self::Tsv }
    }
}

#[cfg(test)]
fn render(payload: &QueryPayload, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Json => payload
            .to_json()
            .map(|value| value.to_string())
            .map_err(|err| Error::Generic(err.to_string())),
        OutputFormat::Tsv => Ok(render_query_tsv(payload)),
    }
}

fn render_event(event: &StateEvent, format: OutputFormat) -> Result<String> {
    match format {
        OutputFormat::Json => event
            .to_json()
            .map(|value| value.to_string())
            .map_err(|err| Error::Generic(err.to_string())),
        OutputFormat::Tsv => Ok(render_event_tsv(event)),
    }
}

#[cfg(test)]
fn render_query_tsv(payload: &QueryPayload) -> String {
    let mut sections = Vec::new();
    match payload {
        QueryPayload::State(state) => {
            sections.push(section("ACTIVE", render_active(&state.active)));
            sections.push(section(
                "CAPABILITIES",
                format!(
                    "MOVE_WINDOWS\tFOCUS\tCREATE\tDELETE\n{}\t{}\t{}\t{}",
                    state.capabilities.move_windows,
                    state.capabilities.focus,
                    state.capabilities.create,
                    state.capabilities.delete
                ),
            ));
            sections.push(section("DISPLAYS", render_displays(&state.displays)));
            sections.push(section("SPACES", render_spaces(&state.spaces)));
            sections.push(section("WINDOWS", render_space_windows(&state.spaces)));
        }
        QueryPayload::Spaces(spaces) => {
            sections.push(render_spaces(spaces));
            if spaces.iter().any(|space| !space.windows.is_empty()) {
                sections.push(section("WINDOWS", render_space_windows(spaces)));
            }
        }
        QueryPayload::Active(active) => sections.push(render_active(active)),
        QueryPayload::OnScreen(windows) => sections.push(render_windows(windows, None)),
    }
    sections.join("\n\n")
}

#[cfg(test)]
fn section(name: &str, body: String) -> String {
    format!("{name}\n{body}")
}

#[cfg(test)]
fn render_active(active: &ActiveState) -> String {
    format!(
        "DISPLAY_ID\tSPACE_ID\tWINDOW_ID\tAPP\tBUNDLE_ID\tTITLE\n{}\t{}\t{}\t{}\t{}\t{}",
        optional(active.display_id),
        optional(active.space_id),
        optional(active.focused_window_id),
        optional_text(active.focused_app_name.as_deref()),
        optional_text(active.focused_bundle_id.as_deref()),
        optional_text(active.focused_window_title.as_deref())
    )
}

#[cfg(test)]
fn render_displays(displays: &[spool_shared_types::state::DisplayState]) -> String {
    let mut lines = vec!["DISPLAY_ID\tACTIVE\tVISIBLE_SPACE_ID".to_string()];
    lines.extend(displays.iter().map(|display| {
        format!(
            "{}\t{}\t{}",
            display.display_id,
            display.active,
            optional(display.visible_space_id)
        )
    }));
    lines.join("\n")
}

#[cfg(test)]
fn render_spaces(spaces: &[SpaceState]) -> String {
    let mut lines = vec![
        "SPACE_ID\tDISPLAY_ID\tORDINAL\tKIND\tVISIBLE\tFOCUSED\tWINDOW_COUNT\tWINDOW_IDS"
            .to_string(),
    ];
    lines.extend(spaces.iter().map(|space| {
        let window_ids = space
            .windows
            .iter()
            .map(|window| window.window_id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            space.space_id,
            space.display_id,
            space.ordinal,
            space_kind(space.kind),
            space.visible,
            space.focused,
            space.windows.len(),
            if window_ids.is_empty() {
                "-"
            } else {
                &window_ids
            }
        )
    }));
    lines.join("\n")
}

#[cfg(test)]
const fn space_kind(kind: SpaceKind) -> &'static str {
    match kind {
        SpaceKind::User => "user",
        SpaceKind::Fullscreen => "fullscreen",
    }
}

#[cfg(test)]
fn render_space_windows(spaces: &[SpaceState]) -> String {
    let mut lines = vec![window_header(true)];
    for space in spaces {
        lines.extend(
            space
                .windows
                .iter()
                .map(|window| render_window(window, Some(space.space_id))),
        );
    }
    lines.join("\n")
}

#[cfg(test)]
fn render_windows(windows: &[WindowState], space_id: Option<u64>) -> String {
    let mut lines = vec![window_header(false)];
    lines.extend(windows.iter().map(|window| render_window(window, space_id)));
    lines.join("\n")
}

#[cfg(test)]
fn window_header(include_space: bool) -> String {
    let prefix = if include_space { "SPACE_ID\t" } else { "" };
    format!(
        "{prefix}WINDOW_ID\tDISPLAY_ID\tAPP\tBUNDLE_ID\tTITLE\tX\tY\tWIDTH\tHEIGHT\tFOCUSED\tFLOATING\tVISIBLE"
    )
}

#[cfg(test)]
fn render_window(window: &WindowState, space_id: Option<u64>) -> String {
    let prefix = space_id.map_or_else(String::new, |space_id| format!("{space_id}\t"));
    let (x, y, width, height) = frame_cells(window.frame);
    format!(
        "{prefix}{}\t{}\t{}\t{}\t{}\t{x}\t{y}\t{width}\t{height}\t{}\t{}\t{}",
        window.window_id,
        optional(window.display_id),
        text_cell(&window.app_name),
        text_cell(&window.bundle_id),
        text_cell(&window.title),
        window.focused,
        window.floating,
        window.visible
    )
}

#[cfg(test)]
fn frame_cells(frame: Option<Frame>) -> (String, String, String, String) {
    frame.map_or_else(
        || ("-".into(), "-".into(), "-".into(), "-".into()),
        |frame| {
            (
                frame.x.to_string(),
                frame.y.to_string(),
                frame.width.to_string(),
                frame.height.to_string(),
            )
        },
    )
}

fn render_event_tsv(event: &StateEvent) -> String {
    let (name, display_id, space_id, window_id, bundle_id, title, details) = match event {
        StateEvent::SpaceChanged { active } => active_event_cells("space_changed", active, "-"),
        StateEvent::WindowsChanged { space_id, active } => (
            "windows_changed",
            optional(active.display_id),
            optional(*space_id),
            optional(active.focused_window_id),
            optional_text(active.focused_bundle_id.as_deref()),
            optional_text(active.focused_window_title.as_deref()),
            "-".to_string(),
        ),
        StateEvent::WindowFocused {
            window_id,
            bundle_id,
            title,
            space_id,
        } => (
            "window_focused",
            "-".to_string(),
            optional(*space_id),
            optional(*window_id),
            optional_text(bundle_id.as_deref()),
            optional_text(title.as_deref()),
            "-".to_string(),
        ),
        StateEvent::OnScreenChanged { windows, active } => active_event_cells(
            "on_screen_changed",
            active,
            &format!(
                "windows={}",
                windows
                    .iter()
                    .map(|window| window.window_id.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        ),
        StateEvent::WindowTitleChanged { window_id, title } => (
            "window_title_changed",
            "-".to_string(),
            "-".to_string(),
            window_id.to_string(),
            "-".to_string(),
            text_cell(title),
            "-".to_string(),
        ),
        StateEvent::DisplayChanged { display_id } => (
            "display_changed",
            optional(*display_id),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        ),
        StateEvent::RawEvent {
            name,
            display_id,
            space_id,
            window_id,
            details,
        } => (
            "raw_event",
            optional(*display_id),
            optional(*space_id),
            optional(*window_id),
            "-".to_string(),
            text_cell(name),
            text_cell(details),
        ),
    };
    format!("{name}\t{display_id}\t{space_id}\t{window_id}\t{bundle_id}\t{title}\t{details}")
}

fn active_event_cells<'a>(
    name: &'a str,
    active: &ActiveState,
    details: &str,
) -> (&'a str, String, String, String, String, String, String) {
    (
        name,
        optional(active.display_id),
        optional(active.space_id),
        optional(active.focused_window_id),
        optional_text(active.focused_bundle_id.as_deref()),
        optional_text(active.focused_window_title.as_deref()),
        text_cell(details),
    )
}

fn optional(value: Option<impl ToString>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

fn optional_text(value: Option<&str>) -> String {
    value.map_or_else(|| "-".to_string(), text_cell)
}

fn text_cell(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if matches!(character, '\t' | '\r' | '\n') {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Submit exactly once; a transport failure after submission is not a rejection.
pub(crate) fn checked_action(action: Action, timeout_ms: u64, json: bool) -> Result<u8> {
    use spool_shared_types::wire::{AdmissionStatus, CheckedAction};
    let request_id = uuid::Uuid::new_v4().to_string();
    let response = Client::connect_with_deadline(
        &service_name(),
        std::time::Duration::from_millis(timeout_ms),
    )
    .map_err(|error| (false, error))
    .and_then(|client| {
        client
            .call_with_status(&Request::Command(CheckedAction {
                request_id: request_id.clone(),
                action,
            }))
            .map_err(|failure| (failure.submitted, failure.error))
    });
    let (value, success) = match response {
        Ok(Response::Admission(receipt)) if receipt.request_id == request_id => {
            let success = receipt.status == AdmissionStatus::Accepted;
            (serde_json::to_value(receipt)?, success)
        }
        Ok(other) => (
            serde_json::json!({"request_id":request_id,"status":"acceptance_unknown","message":format!("unexpected response: {other:?}")}),
            false,
        ),
        Err((submitted, error)) => (
            serde_json::json!({"request_id":request_id,"status":if submitted{"acceptance_unknown"}else{"not_submitted"},"message":error.to_string()}),
            false,
        ),
    };
    if json {
        println!("{}", serde_json::to_string(&value)?);
    } else {
        println!(
            "{}{}",
            value["status"].as_str().unwrap_or("unknown"),
            value["message"]
                .as_str()
                .map_or_else(String::new, |message| format!(": {message}"))
        );
    }
    Ok(u8::from(!success))
}
pub(crate) fn inspect(
    request: spool_shared_types::inspection::ReadRequest,
) -> spool_shared_types::inspection::Report {
    use spool_shared_types::inspection::Report;
    match Client::connect_with_deadline(
        &service_name(),
        std::time::Duration::from_millis(request.timeout_ms),
    )
    .and_then(|client| client.call(&Request::Inspect(request.clone())))
    {
        Ok(Response::Inspection(report)) => *report,
        Ok(other) => Report::failure(&request, "unexpected_response", &format!("{other:?}")),
        Err(error) => Report::failure(&request, "source_unavailable", &error.to_string()),
    }
}
pub(crate) fn print_report(
    report: &spool_shared_types::inspection::Report,
    json: bool,
) -> Result<()> {
    use std::io::Write;
    let mut output = std::io::stdout().lock();
    if json {
        writeln!(output, "{}", serde_json::to_string(report)?)?;
    } else {
        writeln!(
            output,
            "{} {:?} ({:?})",
            report.resource.token(),
            report.source,
            report.status
        )?;
        write!(output, "{}", render_report_data(&report.data))?;
        for issue in &report.issues {
            writeln!(
                output,
                "{}: {} ({})",
                issue.code,
                issue.message,
                issue.target.as_deref().unwrap_or("collection")
            )?;
        }
    }
    Ok(())
}

fn render_report_data(data: &serde_json::Value) -> String {
    use std::fmt::Write;
    let Some(rows) = data.as_array() else {
        return format!("{data:#}\n");
    };
    let mut output = String::new();
    for (status, heading) in [("matched", "MATCHED"), ("unresolved", "UNRESOLVED")] {
        let selected = rows
            .iter()
            .filter(|row| row["match_status"].as_str().unwrap_or("matched") == status)
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let _ = writeln!(output, "{heading}\nID\tPID\tNAME / TITLE\tDETAILS");
        for row in selected {
            let identity = row.get("identity").unwrap_or(row);
            let cell = |value: Option<&serde_json::Value>| {
                value.filter(|value| !value.is_null()).map_or_else(
                    || "-".into(),
                    |value| {
                        value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned)
                            .replace(['\t', '\n', '\r'], " ")
                    },
                )
            };
            let id = identity.get("id").or_else(|| identity.get("pid"));
            let title = row
                .get("title")
                .or_else(|| identity.get("name"))
                .or_else(|| identity.get("title"));
            let _ = writeln!(
                output,
                "{}\t{}\t{}\t{}",
                cell(id),
                cell(identity.get("pid")),
                cell(title),
                row
            );
        }
    }
    if rows.is_empty() {
        output.push_str("No matches.\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use spool_shared_types::state::{DisplayState, QueryState, SpaceCapabilities};

    fn active() -> ActiveState {
        ActiveState {
            display_id: Some(1),
            space_id: Some(42),
            focused_window_id: Some(321),
            focused_bundle_id: Some("com.openai.chat".into()),
            focused_app_name: Some("Chat\tGPT".into()),
            focused_window_title: Some("first\nsecond".into()),
        }
    }

    fn window(window_id: i32) -> WindowState {
        WindowState {
            window_id,
            bundle_id: "com.mitchellh.ghostty".into(),
            app_name: "Ghostty".into(),
            title: "shell\tjob\nname".into(),
            focused: true,
            floating: false,
            display_id: Some(1),
            frame: Some(Frame {
                x: 10,
                y: 20,
                width: 800,
                height: 600,
            }),
            visible: true,
        }
    }

    fn space() -> SpaceState {
        SpaceState {
            space_id: 42,
            display_id: 1,
            ordinal: 0,
            kind: SpaceKind::User,
            visible: true,
            focused: true,
            windows: vec![window(321)],
        }
    }

    #[test]
    fn active_tsv_has_stable_columns_and_sanitizes_text_cells() {
        let output = render_query_tsv(&QueryPayload::Active(Box::new(active())));
        assert_eq!(
            output,
            "DISPLAY_ID\tSPACE_ID\tWINDOW_ID\tAPP\tBUNDLE_ID\tTITLE\n\
             1\t42\t321\tChat GPT\tcom.openai.chat\tfirst second"
        );
    }

    #[test]
    fn spaces_tsv_summarizes_spaces_then_lists_their_windows() {
        let output = render_query_tsv(&QueryPayload::Spaces(vec![space()]));
        assert_eq!(
            output,
            "SPACE_ID\tDISPLAY_ID\tORDINAL\tKIND\tVISIBLE\tFOCUSED\tWINDOW_COUNT\tWINDOW_IDS\n\
             42\t1\t0\tuser\ttrue\ttrue\t1\t321\n\n\
             WINDOWS\n\
             SPACE_ID\tWINDOW_ID\tDISPLAY_ID\tAPP\tBUNDLE_ID\tTITLE\tX\tY\tWIDTH\tHEIGHT\tFOCUSED\tFLOATING\tVISIBLE\n\
             42\t321\t1\tGhostty\tcom.mitchellh.ghostty\tshell job name\t10\t20\t800\t600\ttrue\tfalse\ttrue"
        );
    }

    #[test]
    fn complete_state_tsv_has_named_sections() {
        let state = QueryState {
            version: 3,
            timestamp: 1,
            active: active(),
            capabilities: SpaceCapabilities {
                move_windows: true,
                focus: true,
                create: false,
                delete: false,
            },
            displays: vec![DisplayState {
                display_id: 1,
                active: true,
                visible_space_id: Some(42),
            }],
            spaces: vec![space()],
        };

        let output = render_query_tsv(&QueryPayload::State(Box::new(state)));
        for section_name in ["ACTIVE", "CAPABILITIES", "DISPLAYS", "SPACES", "WINDOWS"] {
            assert!(
                output.lines().any(|line| line == section_name),
                "missing {section_name} section in:\n{output}"
            );
        }
    }

    #[test]
    fn on_screen_tsv_includes_geometry_and_missing_values() {
        let mut known = window(321);
        let mut unknown = window(322);
        unknown.display_id = None;
        unknown.frame = None;
        let output = render_query_tsv(&QueryPayload::OnScreen(vec![known.clone(), unknown]));
        assert!(output.contains("321\t1\tGhostty"));
        assert!(output.contains("\t10\t20\t800\t600\t"));
        assert!(output.contains("322\t-\tGhostty"));
        assert!(output.contains("\t-\t-\t-\t-\t"));

        known.title = "unchanged".into();
        assert!(!output.contains("unchanged"));
    }

    #[test]
    fn subscribe_tsv_is_one_summary_row_per_event() {
        let focused = StateEvent::WindowFocused {
            window_id: Some(321),
            bundle_id: Some("com.openai.chat".into()),
            title: Some("first\nsecond".into()),
            space_id: Some(42),
        };
        assert_eq!(
            render_event_tsv(&focused),
            "window_focused\t-\t42\t321\tcom.openai.chat\tfirst second\t-"
        );

        let visible = StateEvent::OnScreenChanged {
            windows: vec![window(321), window(322)],
            active: active(),
        };
        assert_eq!(
            render_event_tsv(&visible),
            "on_screen_changed\t1\t42\t321\tcom.openai.chat\tfirst second\twindows=321,322"
        );

        let raw = StateEvent::RawEvent {
            name: "window_moved".to_string(),
            display_id: Some(1),
            space_id: Some(42),
            window_id: Some(321),
            details: "incarnation=7".to_string(),
        };
        assert_eq!(
            render_event_tsv(&raw),
            "raw_event\t1\t42\t321\t-\twindow_moved\tincarnation=7"
        );
    }

    #[test]
    fn json_rendering_preserves_the_existing_payload_and_event_contract() {
        let payload = QueryPayload::Spaces(vec![space()]);
        assert_eq!(
            render(&payload, OutputFormat::Json).unwrap(),
            payload.to_json().unwrap().to_string()
        );

        let event = StateEvent::DisplayChanged {
            display_id: Some(1),
        };
        assert_eq!(
            render_event(&event, OutputFormat::Json).unwrap(),
            event.to_json().unwrap().to_string()
        );
    }
    #[test]
    fn inspection_table_handles_both_sources_and_partitions_unknown_matches() {
        let output = render_report_data(&serde_json::json!([
            {"id":42,"pid":100,"title":"editor\nnotes","match_status":"matched"},
            {"identity":{"id":43,"pid":200},"title":"browser","match_status":"unresolved"},
            {"pid":300,"name":"app","match_status":"matched"}
        ]));
        assert!(output.contains("42\t100\teditor notes\t"));
        assert!(output.contains("300\t300\tapp\t"));
        let (matched, unresolved) = output.split_once("UNRESOLVED").unwrap();
        assert!(!matched.contains("43\t200"));
        assert!(unresolved.contains("43\t200\tbrowser\t"));
    }
}
