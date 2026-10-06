use crate::{FidanList, FidanValue, OwnedRef};

pub fn range_length(start: i64, end: i64, inclusive: bool) -> Result<i64, String> {
    i64::try_from((i128::from(end) - i128::from(start) + i128::from(inclusive)).max(0))
        .map_err(|_| "range length cannot be represented as an integer".into())
}

pub fn range_method(
    start: i64,
    end: i64,
    inclusive: bool,
    method: &str,
    args: Vec<FidanValue>,
) -> Result<Option<FidanValue>, String> {
    Ok(match method {
        "len" | "length" | "size" | "count" => {
            Some(FidanValue::Integer(range_length(start, end, inclusive)?))
        }
        "toList" | "to_list" | "collect" => {
            // Validate representability before allocating or traversing a range.
            range_length(start, end, inclusive)?;
            let mut list = FidanList::new();
            if inclusive {
                for value in start..=end {
                    list.append(FidanValue::Integer(value));
                }
            } else {
                for value in start..end {
                    list.append(FidanValue::Integer(value));
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "contains" => Some(FidanValue::Boolean(
            matches!(args.first(), Some(FidanValue::Integer(value))
            if *value >= start && if inclusive { *value <= end } else { *value < end }),
        )),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_lengths_validate_full_integer_domain_without_overflow() {
        assert_eq!(range_length(5, 1, true), Ok(0));
        assert_eq!(range_length(i64::MAX, i64::MAX, true), Ok(1));
        assert_eq!(range_length(i64::MIN, -1, false), Ok(i64::MAX));
        assert!(range_length(i64::MIN, i64::MAX, true).is_err());
    }
}
