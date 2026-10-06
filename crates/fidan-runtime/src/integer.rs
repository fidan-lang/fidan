//! Checked signed 64-bit arithmetic shared by interpreted and native execution.

use crate::stdlib::StdlibRuntimeError;
use fidan_diagnostics::diag_code;

fn overflow() -> StdlibRuntimeError {
    StdlibRuntimeError::new(diag_code!("R2003"), "arithmetic overflow")
}

/// Integer powers can produce a reciprocal Float for a negative exponent.
#[derive(Debug, PartialEq)]
pub enum Power {
    Integer(i64),
    Float(f64),
}

pub fn power(base: i64, exponent: i64) -> Result<Power, StdlibRuntimeError> {
    if exponent >= 0 || base == 1 || base == -1 {
        return pow(base, exponent).map(Power::Integer);
    }
    if base == 0 {
        return Err(StdlibRuntimeError::new(
            diag_code!("R2001"),
            "division by zero",
        ));
    }
    // Preserve parity even for exponents that cannot be represented exactly as f64.
    let magnitude = (base as f64).abs().powf(exponent as f64);
    Ok(Power::Float(if base < 0 && exponent & 1 != 0 {
        -magnitude
    } else {
        magnitude
    }))
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
    // Scalar ABI helper, used only when the compiler proves an integer result.
    // General language exponentiation goes through power().
    if exponent < 0 {
        if base == 1 {
            return Ok(1);
        }
        if base == -1 {
            return Ok(if exponent & 1 == 0 { 1 } else { -1 });
        }
        return Err(StdlibRuntimeError::new(
            diag_code!("R0001"),
            "integer-only ABI used for a non-integer power result",
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
    fn negative_powers_follow_the_numeric_result_contract() {
        assert_eq!(power(2, -3).unwrap(), Power::Float(0.125));
        assert_eq!(power(10, -2).unwrap(), Power::Float(0.01));
        assert_eq!(power(1, -999).unwrap(), Power::Integer(1));
        assert_eq!(power(-1, -3).unwrap(), Power::Integer(-1));
        assert_eq!(power(-1, i64::MIN).unwrap(), Power::Integer(1));
        assert_eq!(power(0, -1).unwrap_err().code, diag_code!("R2001"));
        assert_eq!(power(2, 63).unwrap_err().code, diag_code!("R2003"));
    }

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
