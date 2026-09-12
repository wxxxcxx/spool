use spool_shared_types::inspection::{Resource, Selection, Source};

#[test]
fn title_filter_preserves_conflict_and_distinguishes_absent_from_inapplicable() {
    use spool_shared_types::inspection::{FieldEvidence, Filter, Match};
    let filter = Filter::new(Resource::Window, "title", vec!["Report".into()]).unwrap();
    let cg = FieldEvidence::Value(serde_json::json!("Untitled"));
    let ax = FieldEvidence::Value(serde_json::json!("Report"));
    assert_eq!(filter.evaluate(&[cg.clone(), ax.clone()]), Match::Unknown);
    assert_eq!(filter.evaluate(&[ax.clone(), cg]), Match::Unknown);
    assert_eq!(
        filter.evaluate(&[ax.clone(), FieldEvidence::NotApplicable]),
        Match::True
    );
    assert_eq!(
        filter.evaluate(&[ax, FieldEvidence::Unavailable]),
        Match::Unknown
    );
    assert_eq!(Match::False.and(Match::Unknown), Match::False);
    assert!(Filter::new(Resource::Display, "title", vec!["Report".into()]).is_err());
}

#[test]
fn ax_group_and_leaf_expand_to_the_same_reads_without_losing_unadvertised_requests() {
    let group = Selection::new(Resource::Window, Source::Native, &["ax".into()]).unwrap();
    let combined = Selection::new(
        Resource::Window,
        Source::Native,
        &["ax".into(), "ax.AXTitle".into()],
    )
    .unwrap();
    assert_eq!(
        group.ax_attributes(&["AXTitle", "AXRole"]),
        vec!["AXRole", "AXTitle"]
    );
    assert_eq!(
        combined.ax_attributes(&["AXTitle", "AXRole"]),
        vec!["AXRole", "AXTitle"]
    );
    assert_eq!(
        combined.ax_attributes(&["AXRole"]),
        vec!["AXRole", "AXTitle"]
    );
    assert!(Selection::new(Resource::Window, Source::Spool, &["ax".into()]).is_err());
}

#[test]
fn inspection_envelope_survives_binary_transport_and_partial_status() {
    use spool_shared_types::inspection::{ReadRequest, Report, Status};
    let request = ReadRequest::detail(Resource::Window, Source::Native, Some(42));
    let mut report = Report::failure(&request, "timeout", "AX did not finish");
    report.status = Status::Partial;
    report.data = serde_json::json!({"identity": {"window_id": 42}, "ax": {"AXTitle": "Report"}});
    let encoded = postcard::to_stdvec(&report).unwrap();
    let decoded: Report = postcard::from_bytes(&encoded).unwrap();
    assert_eq!(decoded.exit_code(), 3);
    assert_eq!(decoded.data["ax"]["AXTitle"], "Report");
    assert_eq!(serde_json::to_value(&decoded).unwrap()["status"], "partial");
}

#[test]
fn checked_action_and_read_envelopes_roundtrip_through_version_six_payloads() {
    use spool_shared_types::{
        commands::{Action, Operation},
        inspection::ReadRequest,
        wire::{AdmissionReceipt, CheckedAction, Request, Response},
    };
    let action = Request::Command(CheckedAction {
        request_id: "abc".into(),
        action: Action::TargetedWindow {
            window_id: 42,
            operation: Operation::Center,
        },
    });
    let decoded: Request = postcard::from_bytes(&postcard::to_stdvec(&action).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(action).unwrap()
    );
    let request = Request::Inspect(ReadRequest::detail(
        Resource::Window,
        Source::Spool,
        Some(42),
    ));
    let decoded: Request = postcard::from_bytes(&postcard::to_stdvec(&request).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(request).unwrap()
    );
    let receipt = Response::Admission(AdmissionReceipt::from_result(
        "abc".into(),
        Err("window_not_found".into()),
    ));
    let decoded: Response = postcard::from_bytes(&postcard::to_stdvec(&receipt).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(decoded).unwrap(),
        serde_json::to_value(receipt).unwrap()
    );
}
