use crate::{FidanList, FidanString, FidanValue, OwnedRef};

/// Shared character/element slicing for interpreted and native execution.
pub fn slice_value(
    target: &FidanValue,
    start: Option<&FidanValue>,
    end: Option<&FidanValue>,
    inclusive: bool,
    step: Option<&FidanValue>,
) -> Result<FidanValue, String> {
    let integer = |value: Option<&FidanValue>, component: &str| match value {
        None | Some(FidanValue::Nothing) => Ok(None),
        Some(FidanValue::Integer(n)) => Ok(Some(i128::from(*n))),
        Some(other) => Err(format!(
            "slice {component} must be an integer, got `{}`",
            other.type_name()
        )),
    };
    let step = integer(step, "step")?.unwrap_or(1);
    if step == 0 {
        return Err("slice step cannot be zero".into());
    }
    let start = integer(start, "index")?;
    let end = integer(end, "index")?;
    // Wide arithmetic covers even a range spanning the entire i64 domain,
    // and adding an extreme step must never wrap back into the slice.
    let len = match target {
        FidanValue::List(list) => list.borrow().len() as i128,
        FidanValue::String(text) => text.as_str().chars().count() as i128,
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => (i128::from(*end) - i128::from(*start) + i128::from(*inclusive)).max(0),
        other => return Err(format!("cannot slice `{}`", other.type_name())),
    };
    let forward = step > 0;
    let normalize = |index: i128| {
        let index = if index < 0 { len + index } else { index };
        index.clamp(
            if forward { 0 } else { -1 },
            if forward { len } else { len - 1 },
        )
    };
    let mut index = start
        .map(normalize)
        .unwrap_or(if forward { 0 } else { len - 1 });
    let stop = end
        .map(|end| {
            // Inclusive bounds move in the direction of traversal. Adjust before
            // clamping so out-of-range endpoints cannot add a phantom element.
            let end = if end < 0 { len + end } else { end };
            let end = end
                + if inclusive {
                    if forward { 1 } else { -1 }
                } else {
                    0
                };
            end.clamp(
                if forward { 0 } else { -1 },
                if forward { len } else { len - 1 },
            )
        })
        .unwrap_or(if forward { len } else { -1 });
    let indices = std::iter::from_fn(move || {
        if (forward && index >= stop) || (!forward && index <= stop) {
            return None;
        }
        let current = index;
        index += step;
        Some(current)
    });
    match target {
        FidanValue::String(text) => {
            let text = text.as_str();
            let result: String = if step == 1 {
                text.chars()
                    .skip(index as usize)
                    .take((stop - index).max(0) as usize)
                    .collect()
            } else {
                let chars: Vec<_> = text.chars().collect();
                indices.map(|index| chars[index as usize]).collect()
            };
            Ok(FidanValue::String(FidanString::new(&result)))
        }
        FidanValue::List(list) => {
            let list = list.borrow();
            let mut result = FidanList::new();
            for index in indices {
                result.append(
                    list.get(index as usize)
                        .expect("normalized slice index")
                        .clone(),
                );
            }
            Ok(FidanValue::List(OwnedRef::new(result)))
        }
        FidanValue::Range { start, .. } => {
            let mut result = FidanList::new();
            for index in indices {
                result.append(FidanValue::Integer((i128::from(*start) + index) as i64));
            }
            Ok(FidanValue::List(OwnedRef::new(result)))
        }
        _ => unreachable!("slice target validated above"),
    }
}
