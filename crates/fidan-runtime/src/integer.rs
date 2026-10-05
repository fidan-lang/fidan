//! Checked signed 64-bit arithmetic shared by interpreted and native execution.

use crate::stdlib::StdlibRuntimeError;
use fidan_diagnostics::diag_code;

fn overflow() -> StdlibRuntimeError {
    StdlibRuntimeError::new(diag_code!("R2003"), "arithmetic overflow")
}

pub fn add(a: i64, b: i64) -> Result<i64, StdlibRuntimeError> {
    a.checked_add(b).ok_or_else(overflow)
}

pub fn sub(a: i64, b: i64) -> Result<i64, StdlibRuntimeError> {
    a.checked_sub(b).ok_or_else(overflow)
}

pub fn mul(a: i64, b: i64) -> Result<i64, StdlibRuntimeError> {
    a.checked_mul(b).ok_or_else(overflow)
}

pub fn div(a: i64, b: i64) -> Result<i64, StdlibRuntimeError> {
    if b == 0 {
        return Err(StdlibRuntimeError::new(
            diag_code!("R2001"),
            "division by zero",
        ));
    }
    a.checked_div(b).ok_or_else(overflow)
}

pub fn rem(a: i64, b: i64) -> Result<i64, StdlibRuntimeError> {
    if b == 0 {
        return Err(StdlibRuntimeError::new(
            diag_code!("R2001"),
            "modulo by zero",
        ));
    }
    a.checked_rem(b).ok_or_else(overflow)
}

pub fn neg(a: i64) -> Result<i64, StdlibRuntimeError> {
    a.checked_neg().ok_or_else(overflow)
}

pub fn abs(a: i64) -> Result<i64, StdlibRuntimeError> {
    a.checked_abs().ok_or_else(overflow)
}

pub fn pow(mut base: i64, exponent: i64) -> Result<i64, StdlibRuntimeError> {
    // Integer ** integer has an integer result. Preserve the exact reciprocal
    // powers of unit bases; other reciprocal results require a float operand.
    if exponent < 0 {
        if base == 1 {
            return Ok(1);
        }
        if base == -1 {
            return Ok(if exponent & 1 == 0 { 1 } else { -1 });
        }
        return Err(StdlibRuntimeError::new(
            diag_code!("R2003"),
            "negative integer exponent requires a float operand",
        ));
    }
    let mut exponent = exponent as u64;
    let mut result = 1;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = mul(result, base)?;
        }
        exponent >>= 1;
        if exponent != 0 {
            base = mul(base, base)?;
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_boundaries_and_full_width_exponents() {
        for result in [
            add(i64::MAX, 1),
            sub(i64::MIN, 1),
            mul(i64::MAX, 2),
            neg(i64::MIN),
            abs(i64::MIN),
            div(i64::MIN, -1),
            rem(i64::MIN, -1),
            pow(2, 63),
            pow(2, -1),
        ] {
            assert_eq!(result.unwrap_err().code, diag_code!("R2003"));
        }
        for result in [div(1, 0), rem(1, 0)] {
            assert_eq!(result.unwrap_err().code, diag_code!("R2001"));
        }
        assert_eq!(pow(-2, 63).unwrap(), i64::MIN);
        assert_eq!(pow(-1, i64::MAX).unwrap(), -1);
        assert_eq!(pow(1, i64::MAX).unwrap(), 1);
        assert_eq!(pow(0, i64::MAX).unwrap(), 0);
        assert_eq!(pow(0, 0).unwrap(), 1);
        assert_eq!(pow(1, i64::MIN).unwrap(), 1);
        assert_eq!(pow(-1, i64::MIN).unwrap(), 1);
        assert_eq!(pow(-1, -1).unwrap(), -1);
        assert!(pow(0, -1).is_err());
        assert!(pow(2, 4_294_967_296).is_err());
    }
}
