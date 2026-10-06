//! Bracket access shared by interpreted and native execution.
use crate::{FidanString, FidanValue, stdlib::StdlibRuntimeError};
use fidan_diagnostics::diag_code;

fn bounds(target: &FidanValue, index: i64) -> StdlibRuntimeError {
    StdlibRuntimeError::new(
        diag_code!("R2002"),
        format!(
            "{} index {index} out of range",
            target.type_name().to_lowercase()
        ),
    )
}

pub fn get(target: &FidanValue, index: &FidanValue) -> Result<FidanValue, StdlibRuntimeError> {
    if let FidanValue::Dict(dict) = target {
        return dict
            .borrow()
            .get(index)
            .map(|value| value.cloned().unwrap_or(FidanValue::Nothing))
            .map_err(|error| {
                StdlibRuntimeError::new(diag_code!("R0001"), format!("type error: {error}"))
            });
    }
    let FidanValue::Integer(index) = index else {
        return Err(StdlibRuntimeError::new(
            diag_code!("R0001"),
            format!(
                "type error: cannot index `{}` with `{}`",
                target.type_name(),
                index.type_name()
            ),
        ));
    };
    let normalize = |len: usize| {
        let offset = if *index < 0 {
            len as i128 + i128::from(*index)
        } else {
            i128::from(*index)
        };
        usize::try_from(offset).unwrap_or(usize::MAX)
    };
    let value = match target {
        FidanValue::List(list) => {
            let list = list.borrow();
            list.get(normalize(list.len())).cloned()
        }
        FidanValue::Tuple(items) => items.get(normalize(items.len())).cloned(),
        FidanValue::String(text) => text
            .as_str()
            .chars()
            .nth(normalize(text.char_len()))
            .map(|ch| FidanValue::String(FidanString::new(&ch.to_string()))),
        FidanValue::HashSet(set) => set.borrow().value_at_sorted_index(*index),
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            let len = (i128::from(*end) - i128::from(*start) + i128::from(*inclusive)).max(0);
            let offset = if *index < 0 {
                len + i128::from(*index)
            } else {
                i128::from(*index)
            };
            (offset >= 0 && offset < len)
                .then(|| FidanValue::Integer((i128::from(*start) + offset) as i64))
        }
        _ => {
            return Err(StdlibRuntimeError::new(
                diag_code!("R0001"),
                format!(
                    "type error: cannot index `{}` with `Integer`",
                    target.type_name()
                ),
            ));
        }
    };
    value.ok_or_else(|| bounds(target, *index))
}

pub fn set(
    target: &FidanValue,
    index: &FidanValue,
    value: FidanValue,
) -> Result<(), StdlibRuntimeError> {
    match (target, index) {
        (FidanValue::List(list), FidanValue::Integer(index)) => {
            let mut list = list.borrow_mut();
            let offset = if *index < 0 {
                list.len() as i128 + i128::from(*index)
            } else {
                i128::from(*index)
            };
            if offset < 0 || offset >= list.len() as i128 {
                return Err(bounds(target, *index));
            }
            list.set_at(offset as usize, value);
            Ok(())
        }
        (FidanValue::Dict(dict), key) => dict
            .borrow_mut()
            .insert(key.clone(), value)
            .map(|_| ())
            .map_err(|error| {
                StdlibRuntimeError::new(diag_code!("R0001"), format!("type error: {error}"))
            }),
        _ => Err(StdlibRuntimeError::new(
            diag_code!("R0001"),
            format!(
                "type error: cannot index-set `{}` with `{}`",
                target.type_name(),
                index.type_name()
            ),
        )),
    }
}
