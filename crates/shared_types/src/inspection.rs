//! Resource inspection contracts, independent of an ECS world or native handles.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Spool,
    Native,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    Window,
    Space,
    Display,
    App,
    Session,
    SpaceLayout,
}

impl std::str::FromStr for Source {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "spool" => Ok(Self::Spool),
            "native" => Ok(Self::Native),
            _ => Err("expected spool or native".into()),
        }
    }
}

impl Resource {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Space => "space",
            Self::Display => "display",
            Self::App => "app",
            Self::Session => "session",
            Self::SpaceLayout => "space_layout",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadMode {
    List,
    Inspect { id: Option<u64> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadRequest {
    pub resource: Resource,
    pub source: Source,
    pub mode: ReadMode,
    pub show: Vec<String>,
    pub filters: Vec<Filter>,
    pub timeout_ms: u64,
}

impl ReadRequest {
    #[must_use]
    pub fn detail(resource: Resource, source: Source, id: Option<u64>) -> Self {
        Self {
            resource,
            source,
            mode: ReadMode::Inspect { id },
            show: Vec::new(),
            filters: Vec::new(),
            timeout_ms: 5_000,
        }
    }

    /// Validates untrusted wire requests as well as locally parsed CLI requests.
    ///
    /// # Errors
    /// Invalid identity, selection, timeout or filter fails before any read.
    pub fn validate(&self) -> Result<(), String> {
        if self.timeout_ms == 0 {
            return Err("timeout must be positive".into());
        }
        match self.mode {
            ReadMode::List => {
                if !self.show.is_empty() {
                    return Err("--show is only supported by inspect".into());
                }
                if matches!(self.resource, Resource::Session | Resource::SpaceLayout) {
                    return Err("this resource has no list operation".into());
                }
            }
            ReadMode::Inspect { id } => {
                if !self.filters.is_empty() {
                    return Err("filters are only supported by list".into());
                }
                if self.resource == Resource::Session && id.is_some() {
                    return Err("session is a singleton".into());
                }
                if !matches!(self.resource, Resource::Session | Resource::SpaceLayout)
                    && id.is_none()
                {
                    return Err("inspect requires an ID".into());
                }
                if let Some(id) = id
                    && (id == 0
                        || (matches!(self.resource, Resource::Window | Resource::App)
                            && id > i32::MAX as u64)
                        || (self.resource == Resource::Display && id > u64::from(u32::MAX)))
                {
                    return Err("ID is outside this resource's range".into());
                }
            }
        }
        Selection::new(self.resource, self.source, &self.show)?;
        for filter in &self.filters {
            Filter::new(self.resource, &filter.field, filter.values.clone())?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Complete,
    Partial,
    Failed,
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub source: String,
    pub operation: String,
    pub target: Option<String>,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionOperation {
    pub id: String,
    pub target: String,
    pub source: String,
    pub scope: String,
    pub dependencies: Vec<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub start_offset_ms: Option<u64>,
    pub finish_offset_ms: Option<u64>,
    pub status: String,
    pub enumeration_complete: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Collection {
    pub operations: Vec<CollectionOperation>,
    pub requested: ReadRequest,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    pub source: Source,
    pub resource: Resource,
    pub status: Status,
    pub collection: Collection,
    #[serde(with = "crate::json::value")]
    pub data: serde_json::Value,
    pub issues: Vec<Issue>,
}

impl Report {
    #[must_use]
    pub fn failure(request: &ReadRequest, code: &str, message: &str) -> Self {
        Self {
            schema_version: 1,
            source: request.source,
            resource: request.resource,
            status: Status::Failed,
            collection: Collection {
                operations: Vec::new(),
                requested: request.clone(),
                started_at: None,
                finished_at: None,
                duration_ms: 0,
            },
            data: serde_json::Value::Null,
            issues: vec![Issue {
                source: format!("{:?}", request.source).to_lowercase(),
                operation: "request".into(),
                target: None,
                code: code.into(),
                message: message.into(),
            }],
        }
    }

    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self.status {
            Status::Complete => 0,
            Status::Failed | Status::NotFound => 1,
            Status::Partial => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    True,
    False,
    Unknown,
}

impl Match {
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::False, _) | (_, Self::False) => Self::False,
            (Self::True, Self::True) => Self::True,
            _ => Self::Unknown,
        }
    }
}

/// One predetermined source's contribution to a predicate. Definitive absence
/// of a required value is Unavailable; `NotApplicable` concerns source coverage.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldEvidence {
    Value(serde_json::Value),
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub field: String,
    pub values: Vec<String>,
}

impl Filter {
    /// Builds a resource-supported predicate; repeated values are OR.
    ///
    /// # Errors
    /// Unsupported fields and malformed values are rejected before collection.
    pub fn new(resource: Resource, field: &str, values: Vec<String>) -> Result<Self, String> {
        let allowed: &[&str] = match resource {
            Resource::Window => &[
                "pid",
                "bundle-id",
                "space",
                "display",
                "title",
                "on-screen",
                "minimized",
            ],
            Resource::Space => &["display", "kind", "visible"],
            Resource::App => &["pid", "bundle-id", "name", "hidden"],
            Resource::Display => &["name", "main"],
            Resource::Session | Resource::SpaceLayout => &[],
        };
        if !allowed.contains(&field) || values.is_empty() {
            return Err(format!(
                "unsupported or empty filter '{field}' for {resource:?}"
            ));
        }
        for value in &values {
            let valid = match field {
                "pid" | "space" | "display" => value.parse::<u64>().is_ok_and(|n| n > 0),
                "on-screen" | "minimized" | "visible" | "hidden" | "main" => {
                    matches!(value.as_str(), "true" | "false")
                }
                "kind" => matches!(value.as_str(), "user" | "fullscreen"),
                _ => true,
            };
            if !valid {
                return Err(format!("invalid {field} filter value '{value}'"));
            }
        }
        Ok(Self {
            field: field.into(),
            values,
        })
    }

    /// Applies a predicate only after its complete, fixed source set is known.
    #[must_use]
    pub fn evaluate(&self, sources: &[FieldEvidence]) -> Match {
        let mut result = None;
        for source in sources {
            let value = match source {
                FieldEvidence::Value(value) => value,
                FieldEvidence::NotApplicable => continue,
                FieldEvidence::Unavailable => return Match::Unknown,
            };
            let Some(matched) = self.matches_value(value) else {
                return Match::Unknown;
            };
            if result.is_some_and(|previous| previous != matched) {
                return Match::Unknown;
            }
            result = Some(matched);
        }
        match result {
            Some(true) => Match::True,
            Some(false) => Match::False,
            None => Match::Unknown,
        }
    }

    fn matches_value(&self, value: &serde_json::Value) -> Option<bool> {
        if let serde_json::Value::Array(values) = value {
            if !matches!(self.field.as_str(), "space" | "display") {
                return None;
            }
            let mut matched = false;
            for value in values {
                matched |= self.matches_value(value)?;
            }
            return Some(matched);
        }
        match self.field.as_str() {
            "title" | "name" => value
                .as_str()
                .map(|s| self.values.iter().any(|needle| s.contains(needle))),
            "pid" | "space" | "display" => value
                .as_u64()
                .map(|n| self.values.iter().any(|v| v.parse::<u64>() == Ok(n))),
            "on-screen" | "minimized" | "visible" | "hidden" | "main" => value
                .as_bool()
                .map(|b| self.values.iter().any(|v| (v == "true") == b)),
            _ => value.as_str().map(|s| self.values.iter().any(|v| v == s)),
        }
    }
}

/// A validated selection. Dynamic AX groups are expanded after discovery, so an
/// explicitly requested unadvertised attribute is never lost to prefix deduplication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    paths: BTreeSet<String>,
}

impl Selection {
    /// Validates selection against one resource/source, applying explicit defaults.
    ///
    /// # Errors
    /// Unsupported groups or paths are argument errors, not source fallback.
    pub fn new(resource: Resource, source: Source, paths: &[String]) -> Result<Self, String> {
        if resource == Resource::SpaceLayout && source == Source::Native {
            return Err("Space layout has no native source".into());
        }
        let paths = if paths.is_empty() {
            defaults(resource, source)
                .iter()
                .map(|s| (*s).into())
                .collect()
        } else {
            paths.iter().cloned().collect::<BTreeSet<_>>()
        };
        for path in &paths {
            if !valid_path(resource, source, path) {
                return Err(format!(
                    "unsupported selection '{path}' for {source:?} {resource:?}"
                ));
            }
        }
        Ok(Self { paths })
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }

    #[must_use]
    pub fn wants(&self, path: &str) -> bool {
        self.paths.iter().any(|selected| {
            selected == path
                || path
                    .strip_prefix(selected)
                    .is_some_and(|tail| tail.starts_with('.'))
        })
    }

    /// Exact leaf reads after AX's advertised attribute inventory is available.
    #[must_use]
    pub fn ax_attributes(&self, advertised: &[&str]) -> Vec<String> {
        let mut attributes = BTreeSet::new();
        if self.paths.contains("ax") {
            attributes.extend(advertised.iter().map(|s| (*s).to_owned()));
        }
        attributes.extend(
            self.paths
                .iter()
                .filter_map(|p| p.strip_prefix("ax.").map(str::to_owned)),
        );
        attributes.into_iter().collect()
    }
}

fn defaults(resource: Resource, source: Source) -> &'static [&'static str] {
    match (resource, source) {
        (Resource::Window, Source::Native) => &[
            "identity",
            "ax.AXRole",
            "ax.AXSubrole",
            "ax.AXTitle",
            "ax.AXPosition",
            "ax.AXSize",
            "ax.AXMinimized",
            "ax.AXFullScreen",
            "cg",
            "spaces",
        ],
        (Resource::Window, Source::Spool) => &["identity", "geometry", "layout", "state"],
        (Resource::Space | Resource::App, _) => &["identity", "state", "windows"],
        (Resource::Display, _) => &["identity", "geometry", "spaces"],
        (Resource::Session, _) => &[
            "active",
            "displays",
            "spaces",
            "windows",
            "apps",
            "capabilities",
        ],
        (Resource::SpaceLayout, _) => &["identity", "columns", "state"],
    }
}

fn valid_path(resource: Resource, source: Source, path: &str) -> bool {
    if resource == Resource::Window && source == Source::Native {
        return matches!(
            path,
            "identity" | "ax" | "cg" | "spaces" | "actions" | "parameterized-attributes"
        ) || path
            .strip_prefix("ax.")
            .is_some_and(|name| name.starts_with("AX") && name.len() > 2 && !name.contains('.'))
            || path.strip_prefix("cg.").is_some_and(|name| {
                name.starts_with("kCGWindow") && name.len() > 9 && !name.contains('.')
            });
    }
    if resource == Resource::App && source == Source::Native && path == "windows.ax" {
        return true;
    }
    if defaults(resource, source).contains(&path) {
        return true;
    }
    match resource {
        Resource::Window => matches!(
            path,
            "geometry.desired"
                | "geometry.presented"
                | "geometry.observed"
                | "layout.space_id"
                | "layout.column"
                | "state.floating"
                | "state.visible"
                | "state.available"
                | "state.motion"
                | "state.migration"
                | "state.blockers"
        ),
        Resource::Display => matches!(
            path,
            "geometry.bounds" | "geometry.usable_frame" | "geometry.scale"
        ),
        _ => false,
    }
}
