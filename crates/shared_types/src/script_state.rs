//! The script-owned key-value store: arbitrary named state a script can put
//! somewhere that outlives it, surviving both a Lua hot reload and a daemon
//! restart.
//!
//! Shared by the daemon and its clients: the embedded runtime writes it via
//! `spool.state.*`, a client via the same spelling over the socket. Because
//! there are two writers, a write carries what it [`Expected`] to find — that
//! is what makes `spool.state.mutate` a real read-modify-write.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::script_value::ScriptValue;

/// How large the serialised store is allowed to get. A script that writes on
/// every event has no natural stopping point, and the store is saved to disk;
/// this is the backstop that keeps a runaway loop from growing the state file
/// without bound.
pub const MAX_SERIALISED_BYTES: usize = 1024 * 1024;

/// How long a key may be. Long enough for any sane namespaced name, short
/// enough that a key built from unbounded input (a window title, say) is
/// rejected rather than stored.
pub const MAX_KEY_BYTES: usize = 512;

/// Maximum nested List/Map containers, counting empty containers as well.
/// The default JSON reader accepts 127 nested JSON containers. The saved-file
/// wrapper and state map use two, each List/Map adds its tag object and payload
/// container, and a scalar leaf can add one: 2 + 2 * 62 + 1 = 127.
pub const MAX_NESTING_DEPTH: usize = 62;

/// The store itself: names to values, in sorted order so the file it is saved
/// to is stable and diffable rather than reshuffling on every write.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ScriptState(BTreeMap<String, ScriptValue>);

impl<'de> Deserialize<'de> for ScriptState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let state = Self(BTreeMap::deserialize(deserializer)?);
        for (key, value) in &state.0 {
            Self::check_key(key).map_err(serde::de::Error::custom)?;
            Self::check_depth(value).map_err(serde::de::Error::custom)?;
        }
        state.check_size().map_err(serde::de::Error::custom)?;
        Ok(state)
    }
}

impl ScriptState {
    /// The value stored under `key`, if any.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&ScriptValue> {
        self.0.get(key)
    }

    /// Whether the store holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Applies `write`, if what it expected to find is what is there.
    ///
    /// # Errors
    ///
    /// If the key is unacceptable, the result would be too large or too deep,
    /// or a value cannot be stored as JSON. None leaves the store changed.
    pub fn apply(&mut self, write: &ScriptStateWrite) -> Result<WriteOutcome, String> {
        Self::check_key(&write.key)?;

        if let Expected::Exactly(expected) = &write.expected {
            let current = self.0.get(&write.key);
            if current != expected.as_ref() {
                return Ok(WriteOutcome::Conflict {
                    current: current.cloned(),
                });
            }
        }

        self.check_capacity(write)?;

        let changed = match &write.value {
            Some(value) => match self.0.get(&write.key) {
                Some(existing) if existing == value => false,
                _ => {
                    self.0.insert(write.key.clone(), value.clone());
                    true
                }
            },
            None => self.0.remove(&write.key).is_some(),
        };
        Ok(WriteOutcome::Applied { changed })
    }

    /// Whether `key` is one the store will accept.
    ///
    /// # Errors
    ///
    /// If the key is empty or longer than [`MAX_KEY_BYTES`].
    pub fn check_key(key: &str) -> Result<(), String> {
        if key.is_empty() {
            return Err("key must not be empty".to_string());
        }
        if key.len() > MAX_KEY_BYTES {
            return Err(format!(
                "key is {} bytes, over the {MAX_KEY_BYTES} byte limit",
                key.len()
            ));
        }
        Ok(())
    }

    /// Whether applying `write` would exceed [`MAX_NESTING_DEPTH`] or
    /// [`MAX_SERIALISED_BYTES`]. Depth is checked before cloning the value;
    /// size is checked against a trial copy, so the store stays within bounds.
    ///
    /// # Errors
    ///
    /// If the result would be too large or too deep, or cannot be stored as JSON.
    pub fn check_capacity(&self, write: &ScriptStateWrite) -> Result<(), String> {
        let Some(value) = &write.value else {
            // Removals only ever shrink it.
            return Ok(());
        };
        Self::check_depth(value)?;
        let mut trial = self.clone();
        trial.0.insert(write.key.clone(), value.clone());
        trial.check_size()
    }

    fn check_depth(value: &ScriptValue) -> Result<(), String> {
        if !value.is_within_depth(MAX_NESTING_DEPTH) {
            return Err(format!(
                "value exceeds the {MAX_NESTING_DEPTH} level persistent depth limit"
            ));
        }
        Ok(())
    }

    fn check_size(&self) -> Result<(), String> {
        // Measured as JSON because that is what the store is saved as; the
        // wire encoding is denser, so this stays the conservative bound.
        let size = serde_json::to_vec(self)
            .map_err(|err| format!("value could not be stored: {err}"))?
            .len();
        if size > MAX_SERIALISED_BYTES {
            return Err(format!(
                "store would be {size} bytes, over the {MAX_SERIALISED_BYTES} byte limit"
            ));
        }
        Ok(())
    }
}

/// One write against the store: put `value` under `key`, or take the key out
/// when it is `None`.
///
/// Every write travels as one of these rather than as a replacement map,
/// because there are two writers — a script and a client — and a map would let
/// either clobber what the other just wrote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptStateWrite {
    pub key: String,
    /// `None` removes the key. This is what makes `set(key, nil)` and a
    /// `mutate` that returns nothing mean the same thing.
    pub value: Option<ScriptValue>,
    pub expected: Expected,
}

impl ScriptStateWrite {
    /// A write that lands whatever is already there.
    #[must_use]
    pub fn set(key: String, value: ScriptValue) -> Self {
        Self {
            key,
            value: Some(value),
            expected: Expected::Anything,
        }
    }

    /// A removal that lands whatever is already there.
    #[must_use]
    pub fn remove(key: String) -> Self {
        Self {
            key,
            value: None,
            expected: Expected::Anything,
        }
    }

    /// A write that lands only if the key still holds `expected` — where `None`
    /// means the key is still absent. The read-modify-write primitive
    /// `spool.state.mutate` is built on.
    #[must_use]
    pub fn compare_and_set(
        key: String,
        expected: Option<ScriptValue>,
        value: Option<ScriptValue>,
    ) -> Self {
        Self {
            key,
            value,
            expected: Expected::Exactly(expected),
        }
    }
}

/// What a write requires to be true of the key before it lands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Expected {
    /// Land regardless — a plain `set` or `remove`.
    Anything,
    /// Land only if the key holds exactly this, `None` meaning it is absent.
    Exactly(Option<ScriptValue>),
}

/// What became of a write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WriteOutcome {
    Applied {
        /// Whether the store actually differs now. Writing the value that was
        /// already there is not a change, so nothing downstream re-reads or
        /// re-saves because of it.
        changed: bool,
    },
    /// The key no longer held what the write expected. Carries what it holds
    /// instead, so a caller can re-run its function against the current value
    /// and try again.
    Conflict { current: Option<ScriptValue> },
}

impl WriteOutcome {
    /// The documented `{"outcome": …}` JSON, for a client printing to a
    /// terminal.
    ///
    /// # Errors
    ///
    /// If serialization fails, which should not happen barring a bug in this
    /// type's `Serialize` impl.
    pub fn to_json(&self) -> serde_json::Result<serde_json::Value> {
        Ok(crate::json::flatten_tag(
            serde_json::to_value(self)?,
            "outcome",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialization_rejects_invalid_keys() {
        for key in [String::new(), "x".repeat(MAX_KEY_BYTES + 1)] {
            let entries = BTreeMap::from([(key, ScriptValue::Int(1))]);
            let json = serde_json::to_vec(&entries).expect("encodes");
            assert!(serde_json::from_slice::<ScriptState>(&json).is_err());

            let wire = postcard::to_allocvec(&entries).expect("encodes");
            assert!(postcard::from_bytes::<ScriptState>(&wire).is_err());
        }
    }

    #[test]
    fn deserialization_rejects_an_oversized_store() {
        let entries = BTreeMap::from([(
            "large".to_string(),
            ScriptValue::Str("x".repeat(MAX_SERIALISED_BYTES)),
        )]);
        let json = serde_json::to_vec(&entries).expect("encodes");
        assert!(serde_json::from_slice::<ScriptState>(&json).is_err());

        let wire = postcard::to_allocvec(&entries).expect("encodes");
        assert!(postcard::from_bytes::<ScriptState>(&wire).is_err());
    }

    #[test]
    fn deserialization_accepts_the_live_key_and_capacity_limits() {
        let key = "\u{e9}".repeat(MAX_KEY_BYTES / 2);
        let mut state = ScriptState::default();
        let write = ScriptStateWrite::set(key.clone(), ScriptValue::Str(String::new()));
        state.apply(&write).expect("valid key");
        let overhead = serde_json::to_vec(&state).expect("encodes").len();
        let write = ScriptStateWrite::set(
            key,
            ScriptValue::Str("x".repeat(MAX_SERIALISED_BYTES - overhead)),
        );
        state.apply(&write).expect("exact capacity is accepted");

        let json = serde_json::to_vec(&state).expect("encodes");
        assert_eq!(json.len(), MAX_SERIALISED_BYTES);
        assert_eq!(
            serde_json::from_slice::<ScriptState>(&json).expect("decodes"),
            state
        );
        let wire = postcard::to_allocvec(&state).expect("encodes");
        assert_eq!(
            postcard::from_bytes::<ScriptState>(&wire).expect("decodes"),
            state
        );
    }

    #[test]
    fn rejected_deep_writes_still_survive_the_binary_wire() {
        for depth in [62, 63] {
            let value = (0..depth).fold(ScriptValue::Int(1), |value, level| {
                if level % 2 == 0 {
                    ScriptValue::List(vec![value])
                } else {
                    ScriptValue::Map(BTreeMap::from([("child".to_string(), value)]))
                }
            });
            let write = ScriptStateWrite::set("deep".to_string(), value.clone());
            let wire = postcard::to_allocvec(&write).expect("write encodes");
            let decoded: ScriptStateWrite = postcard::from_bytes(&wire).expect("write decodes");
            assert_eq!(decoded, write);

            let mut state = ScriptState::default();
            assert_eq!(state.check_capacity(&decoded).is_ok(), depth == 62);
            assert_eq!(state.apply(&decoded).is_ok(), depth == 62);
            assert_eq!(state.is_empty(), depth != 62);

            let entries = BTreeMap::from([("deep".to_string(), value)]);
            let json = serde_json::to_vec(&entries).expect("fixture encodes");
            let wire = postcard::to_allocvec(&entries).expect("fixture encodes");
            assert_eq!(
                serde_json::from_slice::<ScriptState>(&json).is_ok(),
                depth == 62
            );
            assert_eq!(
                postcard::from_bytes::<ScriptState>(&wire).is_ok(),
                depth == 62
            );
        }
    }
}
