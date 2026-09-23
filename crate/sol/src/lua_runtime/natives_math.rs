//! the math library.
//! Split out of natives.rs, which holds the shared `call_native` dispatch
//! table and its small cross-cutting coercion helpers.

use super::util::*;
use super::*;

impl LuaRuntime {
    pub(super) fn call_native_math(
        &mut self,
        function: NativeFunction,
        args: Vec<LuaValue>,
    ) -> LuaResult<Vec<LuaValue>> {
        let required = |index: usize| {
            args.get(index).cloned().ok_or_else(|| {
                LuaError::new(format!(
                    "bad argument #{} to '{}' (value expected)",
                    index + 1,
                    function.name()
                ))
            })
        };
        match function {
            NativeFunction::MathAbs => match coerce_number(&required(0)?)? {
                Number::Integer(value) => Ok(vec![LuaValue::Integer(value.wrapping_abs())]),
                Number::Float(value) => Ok(vec![LuaValue::Float(value.abs())]),
            },
            NativeFunction::MathFloor | NativeFunction::MathCeil => {
                match coerce_number(&required(0)?)? {
                    Number::Integer(value) => Ok(vec![LuaValue::Integer(value)]),
                    Number::Float(value) => {
                        let value = if function == NativeFunction::MathFloor {
                            value.floor()
                        } else {
                            value.ceil()
                        };
                        if value.is_finite()
                            && value >= i64::MIN as f64
                            && value < -(i64::MIN as f64)
                        {
                            Ok(vec![LuaValue::Integer(value as i64)])
                        } else {
                            Ok(vec![LuaValue::Float(value)])
                        }
                    }
                }
            }
            NativeFunction::MathMin | NativeFunction::MathMax => {
                if args.is_empty() {
                    return Err(LuaError::new("value expected"));
                }
                let mut best = args[0].clone();
                let mut best_number = coerce_number(&best)?;
                for value in args.iter().skip(1) {
                    let number = coerce_number(value)?;
                    // Comparing via `f64` loses precision for large
                    // integers - e.g. `minint` and `minint + 1` both round
                    // to the same float - so use the same exact int/float
                    // comparison the `<`/`>` operators use instead.
                    let is_new_best = match compare_numbers(best_number, number) {
                        Some(ordering) if function == NativeFunction::MathMin => ordering.is_gt(),
                        Some(ordering) => ordering.is_lt(),
                        None => false,
                    };
                    if is_new_best {
                        best = value.clone();
                        best_number = number;
                    }
                }
                Ok(vec![best])
            }
            NativeFunction::MathToInteger => {
                // Real Lua's `math.tointeger` (`math_toint` in lmathlib.c,
                // via `lua_tointegerx`) applies the same string->number
                // coercion as arithmetic, so a numeral string that denotes
                // an exact integer (e.g. `"34.0"` or `minint .. ""`)
                // converts too, not just live number values.
                let value = match required(0)? {
                    value @ LuaValue::Integer(_) => Some(value),
                    value @ LuaValue::Float(_) => Some(value),
                    LuaValue::String(bytes) => match parse_lua_number(&bytes) {
                        Some(Number::Integer(value)) => Some(LuaValue::Integer(value)),
                        Some(Number::Float(value)) => Some(LuaValue::Float(value)),
                        None => None,
                    },
                    _ => None,
                };
                Ok(vec![match value {
                    Some(LuaValue::Integer(value)) => LuaValue::Integer(value),
                    // `i64::MAX as f64` rounds up to 2^63 (`i64::MAX` isn't
                    // exactly representable), so an upper bound of `<=
                    // i64::MAX as f64` would wrongly accept 2^63 itself,
                    // which is one past the largest representable integer.
                    // Real Lua's equivalent check (`lvm.c`'s
                    // `luaV_flttointeger`) uses this same strict bound.
                    Some(LuaValue::Float(value))
                        if value.is_finite()
                            && value.fract() == 0.0
                            && value >= i64::MIN as f64
                            && value < -(i64::MIN as f64) =>
                    {
                        LuaValue::Integer(value as i64)
                    }
                    _ => LuaValue::Nil,
                }])
            }
            NativeFunction::MathType => Ok(vec![match required(0)? {
                LuaValue::Integer(_) => LuaValue::String(Rc::new(b"integer".to_vec())),
                LuaValue::Float(_) => LuaValue::String(Rc::new(b"float".to_vec())),
                _ => LuaValue::Nil,
            }]),
            NativeFunction::MathSqrt
            | NativeFunction::MathSin
            | NativeFunction::MathCos
            | NativeFunction::MathTan
            | NativeFunction::MathExp
            | NativeFunction::MathLog
            | NativeFunction::MathAcos
            | NativeFunction::MathAsin
            | NativeFunction::MathDeg
            | NativeFunction::MathRad => {
                let input = required(0)?;
                let value = match coerce_number(&input).map_err(|_| {
                    LuaError::new(format!(
                        "bad argument #1 to '{}' (number expected, got {})",
                        function.name(),
                        self.error_type_label(&input)
                    ))
                })? {
                    Number::Integer(value) => value as f64,
                    Number::Float(value) => value,
                };
                let result = match function {
                    NativeFunction::MathSqrt => value.sqrt(),
                    NativeFunction::MathSin => value.sin(),
                    NativeFunction::MathCos => value.cos(),
                    NativeFunction::MathTan => value.tan(),
                    NativeFunction::MathExp => value.exp(),
                    NativeFunction::MathAcos => value.acos(),
                    NativeFunction::MathAsin => value.asin(),
                    NativeFunction::MathDeg => value.to_degrees(),
                    NativeFunction::MathRad => value.to_radians(),
                    NativeFunction::MathLog => {
                        if let Some(base) = args.get(1) {
                            let base = match coerce_number(base)? {
                                Number::Integer(value) => value as f64,
                                Number::Float(value) => value,
                            };
                            value.log(base)
                        } else {
                            value.ln()
                        }
                    }
                    _ => unreachable!(),
                };
                Ok(vec![LuaValue::Float(result)])
            }
            NativeFunction::MathAtan => {
                let y = number_as_f64(&required(0)?)?;
                let value = if let Some(x) = args.get(1) {
                    y.atan2(number_as_f64(x)?)
                } else {
                    y.atan()
                };
                Ok(vec![LuaValue::Float(value)])
            }
            NativeFunction::MathFmod => {
                let left = coerce_number(&required(0)?)?;
                let right = coerce_number(&required(1)?)?;
                match (left, right) {
                    (Number::Integer(_), Number::Integer(0)) => Err(LuaError::new("zero divisor")),
                    (Number::Integer(a), Number::Integer(b)) => {
                        Ok(vec![LuaValue::Integer(if a == i64::MIN && b == -1 {
                            0
                        } else {
                            a % b
                        })])
                    }
                    (a, b) => {
                        let (a, b) = number_float(a, b);
                        Ok(vec![LuaValue::Float(a % b)])
                    }
                }
            }
            NativeFunction::MathModf => match coerce_number(&required(0)?)? {
                Number::Integer(value) => Ok(vec![LuaValue::Integer(value), LuaValue::Float(0.0)]),
                Number::Float(value) => {
                    let integral = value.trunc();
                    let fraction = if value.is_infinite() {
                        0.0
                    } else {
                        value - integral
                    };
                    Ok(vec![LuaValue::Float(integral), LuaValue::Float(fraction)])
                }
            },
            NativeFunction::MathUlt => {
                let left = self.integer(&required(0)?)? as u64;
                let right = self.integer(&required(1)?)? as u64;
                Ok(vec![LuaValue::Bool(left < right)])
            }
            NativeFunction::MathFrexp => {
                let (fraction, exponent) = frexp(number_as_f64(&required(0)?)?);
                Ok(vec![LuaValue::Float(fraction), LuaValue::Integer(exponent)])
            }
            NativeFunction::MathLdexp => {
                let value = number_as_f64(&required(0)?)?;
                let exponent = self.integer(&required(1)?)?;
                Ok(vec![LuaValue::Float(ldexp(value, exponent))])
            }
            NativeFunction::MathRandom => {
                let random = next_random(&mut self.random_state);
                match args.as_slice() {
                    [] => Ok(vec![LuaValue::Float(
                        (random >> 11) as f64 * 2f64.powi(-53),
                    )]),
                    [upper] => {
                        let upper = self.integer(upper)?;
                        if upper == 0 {
                            Ok(vec![LuaValue::Integer(random as i64)])
                        } else if upper < 1 {
                            Err(LuaError::new("interval is empty"))
                        } else {
                            let projected = project_random(
                                random,
                                (upper as u64).wrapping_sub(1),
                                &mut self.random_state,
                            );
                            Ok(vec![LuaValue::Integer(projected.wrapping_add(1) as i64)])
                        }
                    }
                    [lower, upper] => {
                        let lower = self.integer(lower)?;
                        let upper = self.integer(upper)?;
                        if lower > upper {
                            return Err(LuaError::new("interval is empty"));
                        }
                        let projected = project_random(
                            random,
                            (upper as u64).wrapping_sub(lower as u64),
                            &mut self.random_state,
                        );
                        Ok(vec![LuaValue::Integer(
                            projected.wrapping_add(lower as u64) as i64,
                        )])
                    }
                    _ => Err(LuaError::new("wrong number of arguments")),
                }
            }
            NativeFunction::MathRandomSeed => {
                let (seed1, seed2) = if args.is_empty() {
                    (
                        next_random(&mut self.random_state),
                        next_random(&mut self.random_state),
                    )
                } else {
                    (
                        self.integer(&args[0])? as u64,
                        args.get(1)
                            .map(|value| self.integer(value))
                            .transpose()?
                            .unwrap_or(0) as u64,
                    )
                };
                if args.len() > 2 {
                    return Err(LuaError::new("wrong number of arguments"));
                }
                self.random_state = seeded_random_state(seed1, seed2);
                Ok(vec![
                    LuaValue::Integer(seed1 as i64),
                    LuaValue::Integer(seed2 as i64),
                ])
            }
            _ => unreachable!("call_native_math received a non-math NativeFunction"),
        }
    }
}
