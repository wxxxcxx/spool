//! Bounded CF value conversion; AX elements become local references, never trees.

use crate::inspection::protocol::Outcome;
use objc2_core_foundation::{
    CFArray, CFBoolean, CFCopyTypeIDDescription, CFData, CFDictionary, CFEqual, CFGetTypeID,
    CFNull, CFNumber, CFRange, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize, Type,
};
use serde_json::{Value, json};
use std::ptr::NonNull;

#[derive(Default)]
pub(super) struct References {
    elements: Vec<CFRetained<CFType>>,
}
impl References {
    pub(super) fn reference(&mut self, element: &CFType) -> Option<String> {
        if let Some(index) = self
            .elements
            .iter()
            .position(|known| CFEqual(Some(known), Some(element)))
        {
            return Some(format!("ax:{}", index + 1));
        }
        if self.elements.len() >= 8192 {
            return None;
        }
        self.elements.push(element.retain());
        Some(format!("ax:{}", self.elements.len()))
    }
}

struct Budget {
    remaining: usize,
    truncated: bool,
    unsupported: bool,
}
impl Budget {
    fn string(&mut self, value: &CFString) -> String {
        let length = usize::try_from(value.length()).unwrap_or(usize::MAX);
        let count = length.min(self.remaining / 3);
        let mut units = vec![0u16; count];
        if count > 0 {
            unsafe {
                value.characters(
                    CFRange {
                        location: 0,
                        length: isize::try_from(count).unwrap_or(isize::MAX),
                    },
                    units.as_mut_ptr(),
                );
            }
        }
        self.truncated |= count < length;
        self.remaining = self.remaining.saturating_sub(count * 3);
        if let Ok(value) = String::from_utf16(&units) {
            value
        } else {
            self.unsupported = true;
            String::from_utf16_lossy(&units)
        }
    }
}

pub(super) fn convert(value: &CFType, references: &mut References) -> Outcome {
    let mut budget = Budget {
        remaining: 4096,
        truncated: false,
        unsupported: false,
    };
    let native_type = CFCopyTypeIDDescription(CFGetTypeID(Some(value)))
        .map_or_else(|| "unknown CFType".into(), |name| name.to_string());
    let represented = convert_inner(value, references, &mut budget, 0);
    Outcome {
        status: if budget.truncated {
            "truncated"
        } else if budget.unsupported {
            "representation_unsupported"
        } else {
            "value"
        }
        .into(),
        native_type: Some(native_type),
        value: Some(represented),
        message: if budget.truncated {
            Some("bounded native value representation".into())
        } else if budget.unsupported {
            Some("native content could not be fully represented".into())
        } else {
            None
        },
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "exhaustive command/source branches share one admission or capture boundary"
)]
fn convert_inner(
    value: &CFType,
    references: &mut References,
    budget: &mut Budget,
    depth: usize,
) -> Value {
    if depth > 6 || budget.remaining < 16 {
        budget.truncated = true;
        return json!({"truncated":true});
    }
    budget.remaining = budget.remaining.saturating_sub(16);
    if value.downcast_ref::<CFNull>().is_some() {
        return Value::Null;
    }
    if let Some(value) = value.downcast_ref::<CFBoolean>() {
        return json!(value.as_bool());
    }
    if let Some(value) = value.downcast_ref::<CFString>() {
        return json!(budget.string(value));
    }
    if let Some(value) = value.downcast_ref::<CFNumber>() {
        let represented = if value.is_float_type() {
            value
                .as_f64()
                .filter(|value| value.is_finite())
                .map(|value| json!(value))
        } else {
            value.as_i64().map(|value| json!(value))
        };
        return represented.unwrap_or_else(|| {
            budget.unsupported = true;
            json!({"unrepresentable_number":true})
        });
    }
    if let Some(array) = value.downcast_ref::<CFArray>() {
        // AX and WindowServer collections contain CF objects. The runtime
        // container type was checked above; no AX element children are read.
        let array = unsafe { &*NonNull::from(array).as_ptr().cast::<CFArray<CFType>>() };
        let mut output = Vec::new();
        for (index, value) in array.iter().enumerate() {
            if index >= 128 || budget.remaining < 16 {
                budget.truncated = true;
                break;
            }
            output.push(convert_inner(&value, references, budget, depth + 1));
        }
        return json!(output);
    }
    if let Some(dictionary) = value.downcast_ref::<CFDictionary>() {
        // Native AX/CG dictionaries use CF object keys and values.
        let dictionary = unsafe {
            &*NonNull::from(dictionary)
                .as_ptr()
                .cast::<CFDictionary<CFType, CFType>>()
        };
        let (keys, values) = dictionary.to_vecs();
        let mut output = serde_json::Map::new();
        for (index, (key, value)) in keys.into_iter().zip(values).enumerate() {
            if index >= 128 || budget.remaining < 32 {
                budget.truncated = true;
                break;
            }
            if let Some(key) = key.downcast_ref::<CFString>() {
                let key = budget.string(key);
                output.insert(key, convert_inner(&value, references, budget, depth + 1));
            } else {
                budget.unsupported = true;
            }
        }
        return Value::Object(output);
    }
    if let Some(data) = value.downcast_ref::<CFData>() {
        let length = usize::try_from(data.length()).unwrap_or(usize::MAX);
        let count = length.min(budget.remaining / 2);
        let mut bytes = vec![0u8; count];
        if count > 0 {
            unsafe {
                data.bytes(
                    CFRange {
                        location: 0,
                        length: isize::try_from(count).unwrap_or(isize::MAX),
                    },
                    bytes.as_mut_ptr(),
                );
            }
        }
        budget.remaining = budget.remaining.saturating_sub(count * 2);
        budget.truncated |= count < length;
        return json!({"encoding":"hex","length":length,"data":crate::inspection::hex(&bytes)});
    }
    let type_id = CFGetTypeID(Some(value));
    if type_id == unsafe { accessibility_sys::AXUIElementGetTypeID() } {
        return references.reference(value).map_or_else(
            || {
                budget.truncated = true;
                json!({"type":"AXUIElement","truncated":true})
            },
            |reference| json!({"type":"AXUIElement","reference":reference}),
        );
    }
    if type_id == unsafe { accessibility_sys::AXValueGetTypeID() } {
        use accessibility_sys::{
            AXValueGetType, AXValueGetValue, kAXValueTypeAXError as AX_ERROR,
            kAXValueTypeCFRange as AX_RANGE, kAXValueTypeCGPoint as AX_POINT,
            kAXValueTypeCGRect as AX_RECT, kAXValueTypeCGSize as AX_SIZE,
        };
        let pointer = NonNull::from(value).as_ptr().cast();
        let kind = unsafe { AXValueGetType(pointer) };
        macro_rules! extract {
            ($value:expr,$render:expr)=>{{
                let mut native=$value;
                if unsafe{AXValueGetValue(pointer,kind,(&raw mut native).cast())}{$render(native)}else{budget.unsupported=true;json!({"AXValueType":kind})}
            }};
        }
        return match kind {
            AX_POINT => extract!(CGPoint::new(0.0, 0.0), |point: CGPoint| {
                budget.unsupported |= !point.x.is_finite() || !point.y.is_finite();
                json!({"x":point.x,"y":point.y,"units":"points","coordinates":"global_y_down"})
            }),
            AX_SIZE => extract!(CGSize::new(0.0, 0.0), |size: CGSize| {
                budget.unsupported |= !size.width.is_finite() || !size.height.is_finite();
                json!({"width":size.width,"height":size.height,"units":"points"})
            }),
            AX_RECT => extract!(
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0)),
                |rect: CGRect| {
                    budget.unsupported |= !rect.origin.x.is_finite()
                        || !rect.origin.y.is_finite()
                        || !rect.size.width.is_finite()
                        || !rect.size.height.is_finite();
                    json!({"x":rect.origin.x,"y":rect.origin.y,"width":rect.size.width,"height":rect.size.height,"units":"points","coordinates":"global_y_down"})
                }
            ),
            AX_RANGE => extract!(
                CFRange {
                    location: 0,
                    length: 0
                },
                |range: CFRange| json!({"location":range.location,"length":range.length})
            ),
            AX_ERROR => extract!(0i32, |error: i32| json!({"AXError":error})),
            _ => {
                budget.unsupported = true;
                json!({"AXValueType":kind})
            }
        };
    }
    budget.unsupported = true;
    json!({"native_type_id":type_id})
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_foundation::{CFBoolean, CFNumber, CFString};
    use serde_json::json;

    #[test]
    fn native_scalars_preserve_false_empty_and_integer_precision() {
        let mut references = References::default();
        assert_eq!(
            convert(CFBoolean::new(false), &mut references).value,
            Some(json!(false))
        );
        assert_eq!(
            convert(&CFString::from_str(""), &mut references).value,
            Some(json!(""))
        );
        assert_eq!(
            convert(&CFNumber::new_i64(9_007_199_254_740_993), &mut references).value,
            Some(json!(9_007_199_254_740_993_i64))
        );
    }
}
