//! Bounded incremental native evidence and operation accounting.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use spool_shared_types::inspection::{Issue, Resource};
use std::{
    collections::BTreeMap,
    io::{self, Write},
};

pub(crate) const VERSION: u32 = 1;
pub(crate) const MAX_FRAME: usize = 64 * 1024;
pub(crate) const MAX_BUFFERED: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Frame {
    pub version: u32,
    pub capture_id: String,
    pub sequence: u64,
    pub message: Message,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum Message {
    WorkDeclared(Work),
    OperationStarted {
        id: String,
        at: String,
        offset_ms: u64,
    },
    EvidenceResult {
        id: String,
        outcome: Outcome,
        at: String,
        offset_ms: u64,
    },
    InventoryChunk {
        id: String,
        objects: Vec<Object>,
    },
    OperationFinished {
        id: String,
        at: String,
        offset_ms: u64,
        outcome: String,
        enumeration_complete: bool,
    },
    CollectionFinished {
        at: String,
        offset_ms: u64,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Work {
    pub id: String,
    pub target: String,
    pub source: String,
    pub scope: String,
    pub dependencies: Vec<String>,
    pub inventory: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Object {
    pub reference: String,
    pub source: String,
    pub resource: Resource,
    pub id: Option<u64>,
    pub pid: Option<i32>,
    pub data: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Outcome {
    pub status: String,
    pub native_type: Option<String>,
    pub value: Option<Value>,
    pub message: Option<String>,
}
impl Outcome {
    pub(crate) fn value(native_type: &str, value: Value) -> Self {
        Self {
            status: "value".into(),
            native_type: Some(native_type.into()),
            value: Some(value),
            message: None,
        }
    }
    pub(crate) fn error(status: &str, message: impl Into<String>) -> Self {
        Self {
            status: status.into(),
            native_type: None,
            value: None,
            message: Some(message.into()),
        }
    }
    pub(crate) fn complete(&self) -> bool {
        matches!(
            self.status.as_str(),
            "value" | "unsupported" | "absent" | "not_applicable" | "not_needed"
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Progress {
    pub ended: Option<(String, u64, String)>,
    pub work: Work,
    pub started: Option<(String, u64)>,
    pub result: Option<Outcome>,
    pub finished: bool,
    pub enumeration_complete: bool,
}

pub(crate) struct Ledger {
    capture_id: String,
    sequence: u64,
    last_offset: u64,
    active: Option<String>,
    pub objects: BTreeMap<String, Object>,
    pub operations: BTreeMap<String, Progress>,
    pub issues: Vec<Issue>,
    pub finished: bool,
}
impl Ledger {
    pub(crate) fn new(capture_id: String) -> Self {
        let session = Object {
            reference: "session".into(),
            source: "native".into(),
            resource: Resource::Session,
            id: None,
            pid: None,
            data: json!({}),
        };
        Self {
            capture_id,
            sequence: 0,
            last_offset: 0,
            active: None,
            objects: [("session".into(), session)].into(),
            operations: BTreeMap::new(),
            issues: Vec::new(),
            finished: false,
        }
    }
    #[allow(
        clippy::too_many_lines,
        reason = "exhaustive command/source branches share one admission or capture boundary"
    )]
    pub(crate) fn accept(&mut self, frame: Frame) -> Result<(), String> {
        if frame.version != VERSION
            || frame.capture_id != self.capture_id
            || frame.sequence != self.sequence
            || self.finished
        {
            return Err("invalid capture version, identity, sequence or trailing frame".into());
        }
        self.sequence = self.sequence.checked_add(1).ok_or("sequence overflow")?;
        let offset = match &frame.message {
            Message::OperationStarted { offset_ms, .. }
            | Message::EvidenceResult { offset_ms, .. }
            | Message::OperationFinished { offset_ms, .. }
            | Message::CollectionFinished { offset_ms, .. } => Some(*offset_ms),
            _ => None,
        };
        if let Some(offset) = offset {
            if offset < self.last_offset {
                return Err("non-monotonic capture offset".into());
            }
            self.last_offset = offset;
        }
        match frame.message {
            Message::WorkDeclared(work) => {
                if self.operations.contains_key(&work.id)
                    || work
                        .dependencies
                        .iter()
                        .any(|id| !self.operations.contains_key(id))
                    || !self.objects.contains_key(&work.target)
                {
                    return Err("duplicate operation or unknown dependency/target".into());
                }
                self.operations.insert(
                    work.id.clone(),
                    Progress {
                        ended: None,
                        work,
                        started: None,
                        result: None,
                        finished: false,
                        enumeration_complete: false,
                    },
                );
            }
            Message::OperationStarted { id, at, offset_ms } => {
                if self.active.is_some() {
                    return Err("overlapping platform calls".into());
                }
                let operation = self.operations.get_mut(&id).ok_or("unknown operation")?;
                if operation.started.is_some() || operation.finished {
                    return Err("invalid start transition".into());
                }
                operation.started = Some((at, offset_ms));
                self.active = Some(id);
            }
            Message::EvidenceResult {
                id,
                outcome,
                at,
                offset_ms,
            } => {
                let operation = self.operations.get_mut(&id).ok_or("unknown operation")?;
                if operation.started.is_none()
                    || operation.result.is_some()
                    || operation.finished
                    || self.active.as_deref() != Some(&id)
                {
                    return Err("invalid evidence transition".into());
                }
                if !outcome.complete() {
                    self.issues.push(work_issue(
                        &operation.work,
                        &outcome.status,
                        outcome
                            .message
                            .as_deref()
                            .unwrap_or("native read incomplete"),
                    ));
                }
                let mut value =
                    serde_json::to_value(&outcome).map_err(|error| error.to_string())?;
                value["operation_id"] = json!(id);
                value["source"] = json!(operation.work.source);
                value["started_at"] = json!(operation.started.as_ref().map(|entry| &entry.0));
                value["start_offset_ms"] = json!(operation.started.as_ref().map(|entry| entry.1));
                value["finished_at"] = json!(at);
                value["end_offset_ms"] = json!(offset_ms);
                self.objects
                    .get_mut(&operation.work.target)
                    .ok_or("unknown evidence target")?
                    .data[&operation.work.scope] = value;
                operation.result = Some(outcome);
                self.active = None;
            }
            Message::InventoryChunk { id, objects } => {
                let operation = self.operations.get(&id).ok_or("unknown inventory")?;
                if !operation.work.inventory
                    || operation.started.is_none()
                    || operation.finished
                    || self.active.as_ref().is_some_and(|active| active != &id)
                {
                    return Err("invalid inventory transition".into());
                }
                // First chunk proves the inventory call returned. Its discovery
                // parent may stay open while dependent leaf operations run.
                self.active = None;
                for object in objects {
                    if !object.data.is_object() || self.objects.contains_key(&object.reference) {
                        return Err("duplicate or malformed source object".into());
                    }
                    self.objects.insert(object.reference.clone(), object);
                }
            }
            Message::OperationFinished {
                id,
                at,
                offset_ms,
                outcome,
                enumeration_complete,
            } => {
                let operation = self.operations.get_mut(&id).ok_or("unknown operation")?;
                if operation.finished
                    || (operation.started.is_none()
                        && !matches!(
                            outcome.as_str(),
                            "not_needed" | "budget_skipped" | "interrupted"
                        ))
                {
                    return Err("invalid finish transition".into());
                }
                if !operation.work.inventory && operation.result.is_none() && outcome == "value" {
                    return Err("value finish without evidence".into());
                }
                if operation.work.inventory && !enumeration_complete && outcome != "not_needed" {
                    self.issues.push(work_issue(
                        &operation.work,
                        "unknown_remainder",
                        "inventory did not enumerate all objects",
                    ));
                }
                if operation.result.is_none()
                    && !matches!(
                        outcome.as_str(),
                        "value" | "unsupported" | "absent" | "not_applicable" | "not_needed"
                    )
                {
                    self.issues.push(work_issue(
                        &operation.work,
                        &outcome,
                        "required work did not produce evidence",
                    ));
                    if let Some(object) = self.objects.get_mut(&operation.work.target) {
                        object.data[&operation.work.scope] = json!({"status":outcome,"operation_id":id,"source":operation.work.source});
                    }
                }
                operation.ended = Some((at, offset_ms, outcome));
                operation.finished = true;
                operation.enumeration_complete = enumeration_complete;
                if self.active.as_ref() == Some(&id) {
                    self.active = None;
                }
            }
            Message::CollectionFinished { .. } => {
                if self.active.is_some()
                    || self
                        .operations
                        .values()
                        .any(|operation| !operation.finished)
                {
                    return Err("collection finished with open work".into());
                }
                self.finished = true;
            }
        }
        Ok(())
    }

    pub(crate) fn interrupt(&mut self, reason: &str) {
        for operation in self.operations.values() {
            if operation.work.inventory && !operation.enumeration_complete {
                self.issues.push(work_issue(
                    &operation.work,
                    "unknown_remainder",
                    "inventory enumeration did not finish",
                ));
            }
            if operation.finished {
                continue;
            }
            let code = if operation.result.is_some() {
                "missing_finish"
            } else if operation.started.is_some() {
                reason
            } else if reason == "timed_out" {
                "budget_skipped"
            } else {
                "interrupted"
            };
            self.issues.push(work_issue(
                &operation.work,
                code,
                "required operation did not finish",
            ));
            if operation.result.is_none()
                && let Some(object) = self.objects.get_mut(&operation.work.target)
            {
                object.data[&operation.work.scope] = json!({"status":code,"operation_id":operation.work.id,"source":operation.work.source});
            }
        }
        if !self.finished {
            self.issues.push(Issue {
                source: "helper".into(),
                operation: "collection".into(),
                target: None,
                code: "missing_final_marker".into(),
                message: reason.into(),
            });
        }
    }
    #[cfg(test)]
    pub(crate) fn complete(&self) -> bool {
        self.finished
            && self.issues.is_empty()
            && self.operations.values().all(|operation| {
                operation.finished && (!operation.work.inventory || operation.enumeration_complete)
            })
    }
}

fn work_issue(work: &Work, code: &str, message: &str) -> Issue {
    Issue {
        source: work.source.clone(),
        operation: work.id.clone(),
        target: Some(work.target.clone()),
        code: code.into(),
        message: message.into(),
    }
}

pub(crate) fn write_frame(writer: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let bytes = serde_json::to_vec(frame)?;
    if bytes.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "helper frame exceeds limit",
        ));
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "frame length overflow"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completed_evidence_survives_missing_finish_and_unknown_remainder() {
        let mut ledger = Ledger::new("capture".into());
        for (sequence, message) in [
            Message::WorkDeclared(Work {
                id: "inventory".into(),
                target: "session".into(),
                source: "ax".into(),
                scope: "windows".into(),
                dependencies: vec![],
                inventory: true,
            }),
            Message::OperationStarted {
                id: "inventory".into(),
                at: "start".into(),
                offset_ms: 0,
            },
            Message::InventoryChunk {
                id: "inventory".into(),
                objects: vec![Object {
                    reference: "ax:1".into(),
                    source: "ax".into(),
                    resource: spool_shared_types::inspection::Resource::Window,
                    id: Some(42),
                    pid: Some(100),
                    data: json!({}),
                }],
            },
            Message::WorkDeclared(Work {
                id: "title".into(),
                target: "ax:1".into(),
                source: "ax".into(),
                scope: "ax.AXTitle.value".into(),
                dependencies: vec!["inventory".into()],
                inventory: false,
            }),
            Message::OperationStarted {
                id: "title".into(),
                at: "read".into(),
                offset_ms: 1,
            },
            Message::EvidenceResult {
                id: "title".into(),
                outcome: Outcome::value("CFString", json!("Saved title")),
                at: "end".into(),
                offset_ms: 2,
            },
        ]
        .into_iter()
        .enumerate()
        {
            ledger
                .accept(Frame {
                    version: VERSION,
                    capture_id: "capture".into(),
                    sequence: sequence as u64,
                    message,
                })
                .unwrap();
        }
        ledger.interrupt("timed_out");
        assert_eq!(
            ledger.objects["ax:1"].data["ax.AXTitle.value"]["value"],
            "Saved title"
        );
        assert!(!ledger.complete());
        assert!(
            ledger
                .issues
                .iter()
                .any(|issue| issue.code == "unknown_remainder")
        );
        assert!(
            ledger
                .issues
                .iter()
                .any(|issue| issue.code == "missing_finish")
        );
    }
}
