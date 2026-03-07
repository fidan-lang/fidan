//! Bootstrap method dispatch for `FidanValue::Range`.
//!
//! Provides a small set of built-in methods on lazy integer ranges so that
//! for-loop iteration (`0..100`) works without materialising a full `Vec`.

use fidan_runtime::FidanValue;

/// Dispatch `range.method(args)` for a lazy integer range.
///
/// Returns `Some(value)` when a method is handled, `None` when no handler
/// exists (caller should raise "method not found").
pub fn dispatch(
    start: i64,
    end: i64,
    inclusive: bool,
    method: &str,
    _args: Vec<FidanValue>,
) -> Option<FidanValue> {
    match method {
        "len" | "length" | "count" => {
            let n = if inclusive {
                (end - start + 1).max(0)
            } else {
                (end - start).max(0)
            };
            Some(FidanValue::Integer(n))
        }
        "to_list" | "collect" => {
            use fidan_runtime::{FidanList, OwnedRef};
            let mut list = FidanList::new();
            if inclusive {
                for n in start..=end {
                    list.append(FidanValue::Integer(n));
                }
            } else {
                for n in start..end {
                    list.append(FidanValue::Integer(n));
                }
            }
            Some(FidanValue::List(OwnedRef::new(list)))
        }
        "contains" => {
            let v = _args.into_iter().next()?;
            if let FidanValue::Integer(n) = v {
                let hit = if inclusive {
                    n >= start && n <= end
                } else {
                    n >= start && n < end
                };
                Some(FidanValue::Boolean(hit))
            } else {
                Some(FidanValue::Boolean(false))
            }
        }
        _ => None,
    }
}
