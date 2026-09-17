//! A read-only census of Accessibility and CoreGraphics call outcomes.
//!
//! Accessibility is a synchronous, per-application protocol: every read is a
//! message into another process, and an application decides what it publishes. A
//! failure and an answer of "this attribute does not exist" are therefore
//! different facts, but by the time they reach the policy both have usually been
//! mapped to `None`. This module counts them by kind, attribute and application so
//! the window-access layer can be designed from evidence instead of guesses.
//!
//! Nothing here changes behaviour: every function passes its result through
//! unchanged, and failures are only counted and summarised. The summary is one
//! `INFO` line per key every [`SUMMARY_INTERVAL`], so a normal foreground run
//! captures it without `RUST_LOG=debug`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use tracing::info;

use crate::platform::{Pid, WinID};

/// How often the accumulated census is written out.
const SUMMARY_INTERVAL: Duration = Duration::from_mins(5);

/// How many rows one summary prints, worst first.
const SUMMARY_ROWS: usize = 40;

/// Which API answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Source {
    Ax,
    Cg,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Self::Ax => "ax",
            Self::Cg => "cg",
        }
    }
}

/// What the call did. These are the shapes a caller has to answer for; they are
/// not interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    /// The attribute exists but the element has no value for it. A legitimate
    /// answer, not a failure.
    NoValue,
    /// This element does not implement the attribute at all.
    Unsupported,
    /// The target application did not answer in time or is not answering.
    Unresponsive,
    /// The element no longer names a live window.
    Invalidated,
    /// Accessibility is disabled for this process.
    NotPermitted,
    /// The request itself was malformed.
    IllegalArgument,
    /// The target process does not implement the accessibility API.
    NotImplemented,
    /// A failure the API did not otherwise classify.
    Failure,
    /// The answer arrived but could not be read (wrong type, not finite, empty).
    Malformed,
    /// A call that succeeded.
    Success,
}

impl Kind {
    /// The kind a macOS error code describes.
    ///
    /// The codes are the whole point of the census: `-25212` (no value) and
    /// `-25205` (unsupported) are answers, `-25204` (cannot complete) is a
    /// moment, and `-25202` (invalid element) is a dead handle.
    pub(crate) fn from_code(code: i32) -> Self {
        match code {
            accessibility_sys::kAXErrorNoValue => Self::NoValue,
            accessibility_sys::kAXErrorAttributeUnsupported
            | accessibility_sys::kAXErrorActionUnsupported
            | accessibility_sys::kAXErrorParameterizedAttributeUnsupported => Self::Unsupported,
            accessibility_sys::kAXErrorCannotComplete => Self::Unresponsive,
            accessibility_sys::kAXErrorInvalidUIElement
            | accessibility_sys::kAXErrorInvalidUIElementObserver => Self::Invalidated,
            accessibility_sys::kAXErrorAPIDisabled => Self::NotPermitted,
            accessibility_sys::kAXErrorIllegalArgument => Self::IllegalArgument,
            accessibility_sys::kAXErrorNotImplemented => Self::NotImplemented,
            // `kAXErrorFailure` and any code the SDK does not name.
            _ => Self::Failure,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::NoValue => "no_value",
            Self::Unsupported => "unsupported",
            Self::Unresponsive => "unresponsive",
            Self::Invalidated => "invalidated",
            Self::NotPermitted => "not_permitted",
            Self::IllegalArgument => "illegal_argument",
            Self::NotImplemented => "not_implemented",
            Self::Failure => "failure",
            Self::Malformed => "malformed",
            Self::Success => "success",
        }
    }
}

/// Who the call was about, as far as the caller knows it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Context<'a> {
    pub window: Option<WinID>,
    pub pid: Option<Pid>,
    pub bundle_id: Option<&'a str>,
}

impl Context<'_> {
    fn label(&self) -> String {
        let bundle = self.bundle_id.unwrap_or("?");
        match (self.window, self.pid) {
            (Some(window), Some(pid)) => format!("{bundle}|{pid}|{window}"),
            (None, Some(pid)) => format!("{bundle}|{pid}"),
            (Some(window), None) => format!("{bundle}|?|{window}"),
            (None, None) => bundle.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Tally {
    success: u64,
    kinds: HashMap<&'static str, u64>,
}

impl Tally {
    fn failures(&self) -> u64 {
        self.kinds.values().sum()
    }
}

#[derive(Debug)]
struct Census {
    tallies: HashMap<String, Tally>,
    last_summary: Instant,
}

fn census() -> &'static Mutex<Census> {
    static CENSUS: OnceLock<Mutex<Census>> = OnceLock::new();
    CENSUS.get_or_init(|| {
        Mutex::new(Census {
            tallies: HashMap::new(),
            last_summary: Instant::now(),
        })
    })
}

/// Counts one outcome, and writes the accumulated summary when its interval has
/// passed. Never panics on a poisoned lock: a diagnostic must not take the
/// daemon down.
///
/// Successes are counted alongside failures so the summary can state a rate, not
/// only an absolute count; a rate is what distinguishes an application that
/// never implements an attribute from one that is momentarily busy.
pub(crate) fn record(
    source: Source,
    operation: &str,
    subject: &str,
    kind: Kind,
    context: &Context,
) {
    let Ok(mut census) = census().lock() else {
        return;
    };
    let key = format!(
        "source={} operation={} subject={} app={}",
        source.name(),
        operation,
        subject,
        context.label()
    );
    let tally = census.tallies.entry(key).or_default();
    if kind == Kind::Success {
        tally.success += 1;
    } else {
        *tally.kinds.entry(kind.name()).or_default() += 1;
    }
    if census.last_summary.elapsed() < SUMMARY_INTERVAL {
        return;
    }
    census.last_summary = Instant::now();
    let mut rows = census
        .tallies
        .iter()
        .map(|(key, tally)| (key.clone(), tally.clone()))
        .collect::<Vec<_>>();
    // Worst first: most failures, then most calls, then name.
    rows.sort_by(|left, right| {
        right
            .1
            .failures()
            .cmp(&left.1.failures())
            .then_with(|| {
                (right.1.success + right.1.failures()).cmp(&(left.1.success + left.1.failures()))
            })
            .then_with(|| left.0.cmp(&right.0))
    });
    let calls: u64 = rows
        .iter()
        .map(|(_, tally)| tally.success + tally.failures())
        .sum();
    info!(
        calls,
        distinct = rows.len(),
        "ax_census summary (cumulative)"
    );
    for (key, tally) in rows.iter().take(SUMMARY_ROWS) {
        let mut kinds = tally
            .kinds
            .iter()
            .map(|(kind, count)| (*kind, *count))
            .collect::<Vec<_>>();
        kinds.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        let breakdown = kinds
            .iter()
            .map(|(kind, count)| format!("{kind}={count}"))
            .collect::<Vec<_>>()
            .join(" ");
        info!(
            "ax_census {key} success={} failures={} {breakdown}",
            tally.success,
            tally.failures()
        );
    }
    if rows.len() > SUMMARY_ROWS {
        info!(
            "ax_census remaining {} rows omitted",
            rows.len() - SUMMARY_ROWS
        );
    }
}

/// Records an AX attribute read and returns it unchanged.
pub(crate) fn ax_read<T>(
    context: &Context,
    subject: &str,
    result: Result<T, crate::errors::Error>,
) -> Result<T, crate::errors::Error> {
    match &result {
        Ok(_) => record(Source::Ax, "read", subject, Kind::Success, context),
        Err(error) => record(
            Source::Ax,
            "read",
            subject,
            error.macos_code().map_or(Kind::Failure, Kind::from_code),
            context,
        ),
    }
    result
}

/// Records a settability probe and returns it unchanged.
pub(crate) fn ax_settable(
    context: &Context,
    subject: &str,
    result: Result<bool, crate::errors::Error>,
) -> Result<bool, crate::errors::Error> {
    match &result {
        Ok(_) => record(Source::Ax, "settable", subject, Kind::Success, context),
        Err(error) => record(
            Source::Ax,
            "settable",
            subject,
            error.macos_code().map_or(Kind::Failure, Kind::from_code),
            context,
        ),
    }
    result
}

/// Records an AX write or action and returns it unchanged.
pub(crate) fn ax_write<T>(
    context: &Context,
    subject: &str,
    result: Result<T, crate::errors::Error>,
) -> Result<T, crate::errors::Error> {
    match &result {
        Ok(_) => record(Source::Ax, "write", subject, Kind::Success, context),
        Err(error) => record(
            Source::Ax,
            "write",
            subject,
            error.macos_code().map_or(Kind::Failure, Kind::from_code),
            context,
        ),
    }
    result
}

/// Records an AX observer registration or removal.
pub(crate) fn ax_observer<T>(
    context: &Context,
    subject: &str,
    result: Result<T, crate::errors::Error>,
) -> Result<T, crate::errors::Error> {
    match &result {
        Ok(_) => record(Source::Ax, "observe", subject, Kind::Success, context),
        Err(error) => record(
            Source::Ax,
            "observe",
            subject,
            error.macos_code().map_or(Kind::Failure, Kind::from_code),
            context,
        ),
    }
    result
}

/// Records a CoreGraphics outcome that is not an error: a missing field, an
/// empty window list, an unreadable value.
pub(crate) fn cg(subject: &str, kind: Kind, context: &Context) {
    record(Source::Cg, "read", subject, kind, context);
}

/// Records a successful CoreGraphics read, for the same rate.
pub(crate) fn cg_ok(subject: &str, context: &Context) {
    record(Source::Cg, "read", subject, Kind::Success, context);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_classified_into_distinct_kinds() {
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorNoValue),
            Kind::NoValue
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorAttributeUnsupported),
            Kind::Unsupported
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorActionUnsupported),
            Kind::Unsupported
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorCannotComplete),
            Kind::Unresponsive
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorInvalidUIElement),
            Kind::Invalidated
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorAPIDisabled),
            Kind::NotPermitted
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorIllegalArgument),
            Kind::IllegalArgument
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorNotImplemented),
            Kind::NotImplemented
        );
        assert_eq!(
            Kind::from_code(accessibility_sys::kAXErrorFailure),
            Kind::Failure
        );
        // An unknown code is a failure, not a silent success.
        assert_eq!(Kind::from_code(-9999), Kind::Failure);
    }

    #[test]
    fn a_context_label_never_invents_an_application() {
        assert_eq!(Context::default().label(), "?");
        assert_eq!(
            Context {
                pid: Some(7),
                ..Default::default()
            }
            .label(),
            "?|7"
        );
        assert_eq!(
            Context {
                window: Some(42),
                pid: Some(7),
                bundle_id: Some("com.example.app"),
            }
            .label(),
            "com.example.app|7|42"
        );
    }

    #[test]
    fn recording_passes_the_result_through_untouched() {
        let context = Context::default();
        let ok: Result<u8, crate::errors::Error> = ax_read(&context, "AXTitle", Ok(3));
        assert_eq!(ok.unwrap(), 3);
        let failed: Result<u8, crate::errors::Error> = ax_read(
            &context,
            "AXTitle",
            Err(crate::errors::Error::macos(
                "AXTitle",
                accessibility_sys::kAXErrorCannotComplete,
            )),
        );
        assert_eq!(
            failed.unwrap_err().macos_code(),
            Some(accessibility_sys::kAXErrorCannotComplete)
        );
        // And the census saw it, without needing a summary interval to pass.
        let tallies = census().lock().expect("census").tallies.clone();
        assert!(
            tallies
                .iter()
                .any(|(key, tally)| key.contains("subject=AXTitle")
                    && tally.kinds.get("unresponsive").copied() == Some(1)),
            "{tallies:?}"
        );
        assert!(
            tallies
                .iter()
                .any(|(key, tally)| key.contains("subject=AXTitle") && tally.success == 1),
            "a success is counted too, so the summary can state a rate: {tallies:?}"
        );
    }
}
