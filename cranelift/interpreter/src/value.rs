//! The [DataValueExt] trait is an extension trait for [DataValue]. It provides a lot of functions
//! used by the rest of the interpreter.

use core::convert::TryFrom;
use core::fmt::{self, Display, Formatter};
use cranelift_codegen::data_value::{DataValue, DataValueCastFailure};
use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
use cranelift_codegen::ir::{types, Type};
use thiserror::Error;

pub type ValueResult<T> = Result<T, ValueError>;

pub trait DataValueExt: Sized {
    // Identity.
    fn int(n: i128, ty: Type) -> ValueResult<Self>;
    fn into_int(self) -> ValueResult<i128>;
    fn float(n: u64, ty: Type) -> ValueResult<Self>;
    fn into_float(self) -> ValueResult<f64>;
    fn is_float(&self) -> bool;
    fn is_nan(&self) -> ValueResult<bool>;
    fn bool(b: bool, vec_elem: bool, ty: Type) -> ValueResult<Self>;
    fn into_bool(self) -> ValueResult<bool>;
    fn vector(v: [u8; 16], ty: Type) -> ValueResult<Self>;
    fn into_array(&self) -> ValueResult<[u8; 16]>;
    fn convert(self, kind: ValueConversionKind) -> ValueResult<Self>;
    fn concat(self, other: Self) -> ValueResult<Self>;

    fn is_negative(&self) -> ValueResult<bool>;
    fn is_zero(&self) -> ValueResult<bool>;

    fn max(self, other: Self) -> ValueResult<Self>;
    fn min(self, other: Self) -> ValueResult<Self>;

    // Comparison.
    fn uno(&self, other: &Self) -> ValueResult<bool>;

    // Arithmetic.
    fn add(self, other: Self) -> ValueResult<Self>;
    fn sub(self, other: Self) -> ValueResult<Self>;
    fn mul(self, other: Self) -> ValueResult<Self>;
    fn div(self, other: Self) -> ValueResult<Self>;
    fn rem(self, other: Self) -> ValueResult<Self>;
    fn sqrt(self) -> ValueResult<Self>;
    fn fma(self, a: Self, b: Self) -> ValueResult<Self>;
    fn abs(self) -> ValueResult<Self>;
    fn checked_add(self, other: Self) -> ValueResult<Option<Self>>;
    fn overflowing_add(self, other: Self) -> ValueResult<(Self, bool)>;
    fn overflowing_sub(self, other: Self) -> ValueResult<(Self, bool)>;
    fn overflowing_mul(self, other: Self) -> ValueResult<(Self, bool)>;

    // Float operations
    fn neg(self) -> ValueResult<Self>;
    fn copysign(self, sign: Self) -> ValueResult<Self>;
    fn ceil(self) -> ValueResult<Self>;
    fn floor(self) -> ValueResult<Self>;
    fn trunc(self) -> ValueResult<Self>;
    fn nearest(self) -> ValueResult<Self>;

    // Saturating arithmetic.
    fn add_sat(self, other: Self) -> ValueResult<Self>;
    fn sub_sat(self, other: Self) -> ValueResult<Self>;

    // Bitwise.
    fn shl(self, other: Self) -> ValueResult<Self>;
    fn ushr(self, other: Self) -> ValueResult<Self>;
    fn ishr(self, other: Self) -> ValueResult<Self>;
    fn rotl(self, other: Self) -> ValueResult<Self>;
    fn rotr(self, other: Self) -> ValueResult<Self>;
    fn and(self, other: Self) -> ValueResult<Self>;
    fn or(self, other: Self) -> ValueResult<Self>;
    fn xor(self, other: Self) -> ValueResult<Self>;
    fn not(self) -> ValueResult<Self>;

    // Bit counting.
    fn count_ones(self) -> ValueResult<Self>;
    fn leading_ones(self) -> ValueResult<Self>;
    fn leading_zeros(self) -> ValueResult<Self>;
    fn trailing_zeros(self) -> ValueResult<Self>;
    fn reverse_bits(self) -> ValueResult<Self>;
    fn swap_bytes(self) -> ValueResult<Self>;
}

#[derive(Error, Debug, PartialEq)]
pub enum ValueError {
    #[error("unable to convert type {1} into class {0}")]
    InvalidType(ValueTypeClass, Type),
    #[error("unable to convert value into type {0}")]
    InvalidValue(Type),
    #[error("unable to convert to primitive integer")]
    InvalidInteger(#[from] std::num::TryFromIntError),
    #[error("unable to cast data value")]
    InvalidDataValueCast(#[from] DataValueCastFailure),
    #[error("performed a division by zero")]
    IntegerDivisionByZero,
    #[error("performed a operation that overflowed this integer type")]
    IntegerOverflow,
}

#[derive(Debug, PartialEq)]
pub enum ValueTypeClass {
    Integer,
    Boolean,
    Float,
    Vector,
}

impl Display for ValueTypeClass {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            ValueTypeClass::Integer => write!(f, "integer"),
            ValueTypeClass::Boolean => write!(f, "boolean"),
            ValueTypeClass::Float => write!(f, "float"),
            ValueTypeClass::Vector => write!(f, "vector"),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ValueConversionKind {
    /// Throw a [ValueError] if an exact conversion to [Type] is not possible; e.g. in `i32` to
    /// `i16`, convert `0x00001234` to `0x1234`.
    Exact(Type),
    /// Truncate the value to fit into the specified [Type]; e.g. in `i16` to `i8`, `0x1234` becomes
    /// `0x34`.
    Truncate(Type),
    ///  Similar to Truncate, but extracts from the top of the value; e.g. in a `i32` to `u8`,
    /// `0x12345678` becomes `0x12`.
    ExtractUpper(Type),
    /// Convert to a larger integer type, extending the sign bit; e.g. in `i8` to `i16`, `0xff`
    /// becomes `0xffff`.
    SignExtend(Type),
    /// Convert to a larger integer type, extending with zeroes; e.g. in `i8` to `i16`, `0xff`
    /// becomes `0x00ff`.
    ZeroExtend(Type),
    /// Convert a signed integer to its unsigned value of the same size; e.g. in `i8` to `u8`,
    /// `0xff` (`-1`) becomes `0xff` (`255`).
    ToUnsigned,
    /// Convert an unsigned integer to its signed value of the same size; e.g. in `u8` to `i8`,
    /// `0xff` (`255`) becomes `0xff` (`-1`).
    ToSigned,
    /// Convert a floating point number by rounding to the nearest possible value with ties to even.
    /// See `fdemote`, e.g.
    RoundNearestEven(Type),
    /// Converts an integer into a boolean, zero integers are converted into a
    /// `false`, while other integers are converted into `true`. Booleans are passed through.
    ToBoolean,
    /// Converts an integer into either -1 or zero.
    Mask(Type),
}

/// Helper for creating match expressions over [DataValue].
macro_rules! unary_match {
    ( $op:ident($arg1:expr); [ $( $data_value_ty:ident ),* ]; [ $( $return_value_ty:ident ),* ] ) => {
        match $arg1 {
            $( DataValue::$data_value_ty(a) => {
                Ok(DataValue::$data_value_ty($return_value_ty::try_from(a.$op()).unwrap()))
            } )*
            _ => unimplemented!()
        }
    };
    ( $op:ident($arg1:expr); [ $( $data_value_ty:ident ),* ] ) => {
        match $arg1 {
            $( DataValue::$data_value_ty(a) => { Ok(DataValue::$data_value_ty(a.$op())) } )*
            _ => unimplemented!()
        }
    };
    ( $op:tt($arg1:expr); [ $( $data_value_ty:ident ),* ] ) => {
        match $arg1 {
            $( DataValue::$data_value_ty(a) => { Ok(DataValue::$data_value_ty($op a)) } )*
            _ => unimplemented!()
        }
    };
}
macro_rules! binary_match {
    ( $op:ident($arg1:expr, $arg2:expr); [ $( $data_value_ty:ident ),* ] ) => {
        match ($arg1, $arg2) {
            $( (DataValue::$data_value_ty(a), DataValue::$data_value_ty(b)) => { Ok(DataValue::$data_value_ty(a.$op(*b))) } )*
            _ => unimplemented!()
        }
    };
    ( option $op:ident($arg1:expr, $arg2:expr); [ $( $data_value_ty:ident ),* ] ) => {
        match ($arg1, $arg2) {
            $( (DataValue::$data_value_ty(a), DataValue::$data_value_ty(b)) => { Ok(a.$op(*b).map(DataValue::$data_value_ty)) } )*
            _ => unimplemented!()
        }
    };
    ( pair $op:ident($arg1:expr, $arg2:expr); [ $( $data_value_ty:ident ),* ] ) => {
        match ($arg1, $arg2) {
            $( (DataValue::$data_value_ty(a), DataValue::$data_value_ty(b)) => {
                let (f, s) = a.$op(*b);
                Ok((DataValue::$data_value_ty(f), s))
            } )*
            _ => unimplemented!()
        }
    };
    ( $op:tt($arg1:expr, $arg2:expr); [ $( $data_value_ty:ident ),* ] ) => {
        match ($arg1, $arg2) {
            $( (DataValue::$data_value_ty(a), DataValue::$data_value_ty(b)) => { Ok(DataValue::$data_value_ty(a $op b)) } )*
            _ => unimplemented!()
        }
    };
    ( $op:tt($arg1:expr, $arg2:expr); [ $( $data_value_ty:ident ),* ]; rhs: $rhs:tt ) => {
        match ($arg1, $arg2) {
            $( (DataValue::$data_value_ty(a), DataValue::$rhs(b)) => { Ok(DataValue::$data_value_ty(a.$op(*b))) } )*
            _ => unimplemented!()
        }
    };
    ( $op:ident($arg1:expr, $arg2:expr); unsigned integers ) => {
        match ($arg1, $arg2) {
            (DataValue::I8(a), DataValue::I8(b)) => { Ok(DataValue::I8((u8::try_from(*a)?.$op(u8::try_from(*b)?) as i8))) }
            (DataValue::I16(a), DataValue::I16(b)) => { Ok(DataValue::I16((u16::try_from(*a)?.$op(u16::try_from(*b)?) as i16))) }
            (DataValue::I32(a), DataValue::I32(b)) => { Ok(DataValue::I32((u32::try_from(*a)?.$op(u32::try_from(*b)?) as i32))) }
            (DataValue::I64(a), DataValue::I64(b)) => { Ok(DataValue::I64((u64::try_from(*a)?.$op(u64::try_from(*b)?) as i64))) }
            (DataValue::I128(a), DataValue::I128(b)) => { Ok(DataValue::I128((u128::try_from(*a)?.$op(u128::try_from(*b)?) as i64))) }
            _ => { Err(ValueError::InvalidType(ValueTypeClass::Integer, if !($arg1).ty().is_int() { ($arg1).ty() } else { ($arg2).ty() })) }
        }
    };
}

macro_rules! bitop {
    ( $op:tt($arg1:expr, $arg2:expr) ) => {
        Ok(match ($arg1, $arg2) {
            (DataValue::I8(a), DataValue::I8(b)) => DataValue::I8(a $op b),
            (DataValue::I16(a), DataValue::I16(b)) => DataValue::I16(a $op b),
            (DataValue::I32(a), DataValue::I32(b)) => DataValue::I32(a $op b),
            (DataValue::I64(a), DataValue::I64(b)) => DataValue::I64(a $op b),
            (DataValue::I128(a), DataValue::I128(b)) => DataValue::I128(a $op b),
            (DataValue::F32(a), DataValue::F32(b)) => DataValue::F32(a $op b),
            (DataValue::F64(a), DataValue::F64(b)) => DataValue::F64(a $op b),
            (DataValue::V128(a), DataValue::V128(b)) => {
                let mut a2 = a.clone();
                for (a, b) in a2.iter_mut().zip(b.iter()) {
                    *a = *a $op *b;
                }
                DataValue::V128(a2)
            }
            _ => unimplemented!(),
        })
    };
}

impl DataValueExt for DataValue {
    fn int(n: i128, ty: Type) -> ValueResult<Self> {
        if ty.is_int() && !ty.is_vector() {
            DataValue::from_integer(n, ty).map_err(|_| ValueError::InvalidValue(ty))
        } else {
            Err(ValueError::InvalidType(ValueTypeClass::Integer, ty))
        }
    }

    fn into_int(self) -> ValueResult<i128> {
        match self {
            DataValue::I8(n) => Ok(n as i128),
            DataValue::I16(n) => Ok(n as i128),
            DataValue::I32(n) => Ok(n as i128),
            DataValue::I64(n) => Ok(n as i128),
            DataValue::I128(n) => Ok(n),
            DataValue::U8(n) => Ok(n as i128),
            DataValue::U16(n) => Ok(n as i128),
            DataValue::U32(n) => Ok(n as i128),
            DataValue::U64(n) => Ok(n as i128),
            DataValue::U128(n) => Ok(n as i128),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Integer, self.ty())),
        }
    }

    fn float(bits: u64, ty: Type) -> ValueResult<Self> {
        match ty {
            types::F32 => Ok(DataValue::F32(Ieee32::with_bits(u32::try_from(bits)?))),
            types::F64 => Ok(DataValue::F64(Ieee64::with_bits(bits))),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Float, ty)),
        }
    }

    fn into_float(self) -> ValueResult<f64> {
        match self {
            DataValue::F32(n) => Ok(n.as_f32() as f64),
            DataValue::F64(n) => Ok(n.as_f64()),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Float, self.ty())),
        }
    }

    fn is_float(&self) -> bool {
        match self {
            DataValue::F32(_) | DataValue::F64(_) => true,
            _ => false,
        }
    }

    fn is_nan(&self) -> ValueResult<bool> {
        match self {
            DataValue::F32(f) => Ok(f.is_nan()),
            DataValue::F64(f) => Ok(f.is_nan()),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Float, self.ty())),
        }
    }

    fn bool(b: bool, vec_elem: bool, ty: Type) -> ValueResult<Self> {
        assert!(ty.is_int());
        macro_rules! make_bool {
            ($ty:ident) => {
                Ok(DataValue::$ty(if b {
                    if vec_elem {
                        -1
                    } else {
                        1
                    }
                } else {
                    0
                }))
            };
        }

        match ty {
            types::I8 => make_bool!(I8),
            types::I16 => make_bool!(I16),
            types::I32 => make_bool!(I32),
            types::I64 => make_bool!(I64),
            types::I128 => make_bool!(I128),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Integer, ty)),
        }
    }

    fn into_bool(self) -> ValueResult<bool> {
        match self {
            DataValue::I8(b) => Ok(b != 0),
            DataValue::I16(b) => Ok(b != 0),
            DataValue::I32(b) => Ok(b != 0),
            DataValue::I64(b) => Ok(b != 0),
            DataValue::I128(b) => Ok(b != 0),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Boolean, self.ty())),
        }
    }

    fn vector(v: [u8; 16], ty: Type) -> ValueResult<Self> {
        assert!(ty.is_vector() && [8, 16].contains(&ty.bytes()));
        if ty.bytes() == 16 {
            Ok(DataValue::V128(v))
        } else if ty.bytes() == 8 {
            let v64: [u8; 8] = v[..8].try_into().unwrap();
            Ok(DataValue::V64(v64))
        } else {
            unimplemented!()
        }
    }

    fn into_array(&self) -> ValueResult<[u8; 16]> {
        match *self {
            DataValue::V128(v) => Ok(v),
            DataValue::V64(v) => {
                let mut v128 = [0; 16];
                v128[..8].clone_from_slice(&v);
                Ok(v128)
            }
            _ => Err(ValueError::InvalidType(ValueTypeClass::Vector, self.ty())),
        }
    }

    fn convert(self, kind: ValueConversionKind) -> ValueResult<Self> {
        Ok(match kind {
            ValueConversionKind::Exact(ty) => match (self, ty) {
                // TODO a lot to do here: from bmask to ireduce to bitcast...
                (val, ty) if val.ty().is_int() && ty.is_int() => {
                    DataValue::from_integer(val.into_int()?, ty)?
                }
                (DataValue::I32(n), types::F32) => DataValue::F32(f32::from_bits(n as u32).into()),
                (DataValue::I64(n), types::F64) => DataValue::F64(f64::from_bits(n as u64).into()),
                (DataValue::F32(n), types::I32) => DataValue::I32(n.bits() as i32),
                (DataValue::F64(n), types::I64) => DataValue::I64(n.bits() as i64),
                (DataValue::F32(n), types::F64) => DataValue::F64((n.as_f32() as f64).into()),
                (dv, t) if (t.is_int() || t.is_float()) && dv.ty() == t => dv,
                (dv, _) => unimplemented!("conversion: {} -> {:?}", dv.ty(), kind),
            },
            ValueConversionKind::Truncate(ty) => {
                assert!(
                    ty.is_int(),
                    "unimplemented conversion: {} -> {:?}",
                    self.ty(),
                    kind
                );

                let mask = (1 << (ty.bytes() * 8)) - 1i128;
                let truncated = self.into_int()? & mask;
                Self::from_integer(truncated, ty)?
            }
            ValueConversionKind::ExtractUpper(ty) => {
                assert!(
                    ty.is_int(),
                    "unimplemented conversion: {} -> {:?}",
                    self.ty(),
                    kind
                );

                let shift_amt = (self.ty().bytes() * 8) - (ty.bytes() * 8);
                let mask = (1 << (ty.bytes() * 8)) - 1i128;
                let shifted_mask = mask << shift_amt;

                let extracted = (self.into_int()? & shifted_mask) >> shift_amt;
                Self::from_integer(extracted, ty)?
            }
            ValueConversionKind::SignExtend(ty) => match (self, ty) {
                (DataValue::U8(n), types::I16) => DataValue::U16(n as u16),
                (DataValue::U8(n), types::I32) => DataValue::U32(n as u32),
                (DataValue::U8(n), types::I64) => DataValue::U64(n as u64),
                (DataValue::U8(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I8(n), types::I16) => DataValue::I16(n as i16),
                (DataValue::I8(n), types::I32) => DataValue::I32(n as i32),
                (DataValue::I8(n), types::I64) => DataValue::I64(n as i64),
                (DataValue::I8(n), types::I128) => DataValue::I128(n as i128),
                (DataValue::U16(n), types::I32) => DataValue::U32(n as u32),
                (DataValue::U16(n), types::I64) => DataValue::U64(n as u64),
                (DataValue::U16(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I16(n), types::I32) => DataValue::I32(n as i32),
                (DataValue::I16(n), types::I64) => DataValue::I64(n as i64),
                (DataValue::I16(n), types::I128) => DataValue::I128(n as i128),
                (DataValue::U32(n), types::I64) => DataValue::U64(n as u64),
                (DataValue::U32(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I32(n), types::I64) => DataValue::I64(n as i64),
                (DataValue::I32(n), types::I128) => DataValue::I128(n as i128),
                (DataValue::U64(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I64(n), types::I128) => DataValue::I128(n as i128),
                (dv, _) => unimplemented!("conversion: {} -> {:?}", dv.ty(), kind),
            },
            ValueConversionKind::ZeroExtend(ty) => match (self, ty) {
                (DataValue::U8(n), types::I16) => DataValue::U16(n as u16),
                (DataValue::U8(n), types::I32) => DataValue::U32(n as u32),
                (DataValue::U8(n), types::I64) => DataValue::U64(n as u64),
                (DataValue::U8(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I8(n), types::I16) => DataValue::I16(n as u8 as i16),
                (DataValue::I8(n), types::I32) => DataValue::I32(n as u8 as i32),
                (DataValue::I8(n), types::I64) => DataValue::I64(n as u8 as i64),
                (DataValue::I8(n), types::I128) => DataValue::I128(n as u8 as i128),
                (DataValue::U16(n), types::I32) => DataValue::U32(n as u32),
                (DataValue::U16(n), types::I64) => DataValue::U64(n as u64),
                (DataValue::U16(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I16(n), types::I32) => DataValue::I32(n as u16 as i32),
                (DataValue::I16(n), types::I64) => DataValue::I64(n as u16 as i64),
                (DataValue::I16(n), types::I128) => DataValue::I128(n as u16 as i128),
                (DataValue::U32(n), types::I64) => DataValue::U64(n as u64),
                (DataValue::U32(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I32(n), types::I64) => DataValue::I64(n as u32 as i64),
                (DataValue::I32(n), types::I128) => DataValue::I128(n as u32 as i128),
                (DataValue::U64(n), types::I128) => DataValue::U128(n as u128),
                (DataValue::I64(n), types::I128) => DataValue::I128(n as u64 as i128),
                (from, to) if from.ty() == to => from,
                (dv, _) => unimplemented!("conversion: {} -> {:?}", dv.ty(), kind),
            },
            ValueConversionKind::ToUnsigned => match self {
                DataValue::I8(n) => DataValue::U8(n as u8),
                DataValue::I16(n) => DataValue::U16(n as u16),
                DataValue::I32(n) => DataValue::U32(n as u32),
                DataValue::I64(n) => DataValue::U64(n as u64),
                DataValue::I128(n) => DataValue::U128(n as u128),
                DataValue::U8(_) => self,
                DataValue::U16(_) => self,
                DataValue::U32(_) => self,
                DataValue::U64(_) => self,
                DataValue::U128(_) => self,
                _ => unimplemented!("conversion: {} -> {:?}", self.ty(), kind),
            },
            ValueConversionKind::ToSigned => match self {
                DataValue::U8(n) => DataValue::I8(n as i8),
                DataValue::U16(n) => DataValue::I16(n as i16),
                DataValue::U32(n) => DataValue::I32(n as i32),
                DataValue::U64(n) => DataValue::I64(n as i64),
                DataValue::U128(n) => DataValue::I128(n as i128),
                DataValue::I8(_) => self,
                DataValue::I16(_) => self,
                DataValue::I32(_) => self,
                DataValue::I64(_) => self,
                DataValue::I128(_) => self,
                _ => unimplemented!("conversion: {} -> {:?}", self.ty(), kind),
            },
            ValueConversionKind::RoundNearestEven(ty) => match (self, ty) {
                (DataValue::F64(n), types::F32) => DataValue::F32(Ieee32::from(n.as_f64() as f32)),
                (s, _) => unimplemented!("conversion: {} -> {:?}", s.ty(), kind),
            },
            ValueConversionKind::ToBoolean => match self.ty() {
                ty if ty.is_int() => DataValue::I8(if self.into_int()? != 0 { 1 } else { 0 }),
                ty => unimplemented!("conversion: {} -> {:?}", ty, kind),
            },
            ValueConversionKind::Mask(ty) => {
                let b = self.into_bool()?;
                Self::bool(b, true, ty).unwrap()
            }
        })
    }

    fn concat(self, other: Self) -> ValueResult<Self> {
        match (self, other) {
            (DataValue::I64(lhs), DataValue::I64(rhs)) => Ok(DataValue::I128(
                (((lhs as u64) as u128) | (((rhs as u64) as u128) << 64)) as i128,
            )),
            (lhs, rhs) => unimplemented!("concat: {} -> {}", lhs.ty(), rhs.ty()),
        }
    }

    fn is_negative(&self) -> ValueResult<bool> {
        match self {
            DataValue::F32(f) => Ok(f.is_negative()),
            DataValue::F64(f) => Ok(f.is_negative()),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Float, self.ty())),
        }
    }

    fn is_zero(&self) -> ValueResult<bool> {
        match self {
            DataValue::F32(f) => Ok(f.is_zero()),
            DataValue::F64(f) => Ok(f.is_zero()),
            _ => Err(ValueError::InvalidType(ValueTypeClass::Float, self.ty())),
        }
    }

    fn max(self, other: Self) -> ValueResult<Self> {
        if self > other {
            Ok(self)
        } else {
            Ok(other)
        }
    }

    fn min(self, other: Self) -> ValueResult<Self> {
        if self < other {
            Ok(self)
        } else {
            Ok(other)
        }
    }

    fn uno(&self, other: &Self) -> ValueResult<bool> {
        Ok(self.is_nan()? || other.is_nan()?)
    }

    fn add(self, other: Self) -> ValueResult<Self> {
        if self.is_float() {
            binary_match!(+(self, other); [F32, F64])
        } else {
            binary_match!(wrapping_add(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
        }
    }

    fn sub(self, other: Self) -> ValueResult<Self> {
        if self.is_float() {
            binary_match!(-(self, other); [F32, F64])
        } else {
            binary_match!(wrapping_sub(&self, &other); [I8, I16, I32, I64, I128])
        }
    }

    fn mul(self, other: Self) -> ValueResult<Self> {
        if self.is_float() {
            binary_match!(*(self, other); [F32, F64])
        } else {
            binary_match!(wrapping_mul(&self, &other); [I8, I16, I32, I64, I128])
        }
    }

    fn div(self, other: Self) -> ValueResult<Self> {
        if self.is_float() {
            return binary_match!(/(self, other); [F32, F64]);
        }

        let denominator = other.clone().into_int()?;

        // Check if we are dividing INT_MIN / -1. This causes an integer overflow trap.
        let min = DataValueExt::int(1i128 << (self.ty().bits() - 1), self.ty())?;
        if self == min && denominator == -1 {
            return Err(ValueError::IntegerOverflow);
        }

        if denominator == 0 {
            return Err(ValueError::IntegerDivisionByZero);
        }

        binary_match!(/(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn rem(self, other: Self) -> ValueResult<Self> {
        let denominator = other.clone().into_int()?;

        // Check if we are dividing INT_MIN / -1. This causes an integer overflow trap.
        let min = DataValueExt::int(1i128 << (self.ty().bits() - 1), self.ty())?;
        if self == min && denominator == -1 {
            return Err(ValueError::IntegerOverflow);
        }

        if denominator == 0 {
            return Err(ValueError::IntegerDivisionByZero);
        }

        binary_match!(%(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn sqrt(self) -> ValueResult<Self> {
        unary_match!(sqrt(&self); [F32, F64]; [Ieee32, Ieee64])
    }

    fn fma(self, b: Self, c: Self) -> ValueResult<Self> {
        match (self, b, c) {
            (DataValue::F32(a), DataValue::F32(b), DataValue::F32(c)) => {
                // The `fma` function for `x86_64-pc-windows-gnu` is incorrect. Use `libm`'s instead.
                // See: https://github.com/bytecodealliance/wasmtime/issues/4512
                #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "gnu"))]
                let res = libm::fmaf(a.as_f32(), b.as_f32(), c.as_f32());

                #[cfg(not(all(
                    target_arch = "x86_64",
                    target_os = "windows",
                    target_env = "gnu"
                )))]
                let res = a.as_f32().mul_add(b.as_f32(), c.as_f32());

                Ok(DataValue::F32(res.into()))
            }
            (DataValue::F64(a), DataValue::F64(b), DataValue::F64(c)) => {
                #[cfg(all(target_arch = "x86_64", target_os = "windows", target_env = "gnu"))]
                let res = libm::fma(a.as_f64(), b.as_f64(), c.as_f64());

                #[cfg(not(all(
                    target_arch = "x86_64",
                    target_os = "windows",
                    target_env = "gnu"
                )))]
                let res = a.as_f64().mul_add(b.as_f64(), c.as_f64());

                Ok(DataValue::F64(res.into()))
            }
            (a, _b, _c) => Err(ValueError::InvalidType(ValueTypeClass::Float, a.ty())),
        }
    }

    fn abs(self) -> ValueResult<Self> {
        unary_match!(abs(&self); [F32, F64])
    }

    fn checked_add(self, other: Self) -> ValueResult<Option<Self>> {
        binary_match!(option checked_add(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn overflowing_add(self, other: Self) -> ValueResult<(Self, bool)> {
        binary_match!(pair overflowing_add(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn overflowing_sub(self, other: Self) -> ValueResult<(Self, bool)> {
        binary_match!(pair overflowing_sub(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn overflowing_mul(self, other: Self) -> ValueResult<(Self, bool)> {
        binary_match!(pair overflowing_mul(&self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn neg(self) -> ValueResult<Self> {
        unary_match!(neg(&self); [F32, F64])
    }

    fn copysign(self, sign: Self) -> ValueResult<Self> {
        binary_match!(copysign(&self, &sign); [F32, F64])
    }

    fn ceil(self) -> ValueResult<Self> {
        unary_match!(ceil(&self); [F32, F64])
    }

    fn floor(self) -> ValueResult<Self> {
        unary_match!(floor(&self); [F32, F64])
    }

    fn trunc(self) -> ValueResult<Self> {
        unary_match!(trunc(&self); [F32, F64])
    }

    fn nearest(self) -> ValueResult<Self> {
        unary_match!(round_ties_even(&self); [F32, F64])
    }

    fn add_sat(self, other: Self) -> ValueResult<Self> {
        binary_match!(saturating_add(self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn sub_sat(self, other: Self) -> ValueResult<Self> {
        binary_match!(saturating_sub(self, &other); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn shl(self, other: Self) -> ValueResult<Self> {
        let amt = other
            .convert(ValueConversionKind::Exact(types::I32))?
            .convert(ValueConversionKind::ToUnsigned)?;
        binary_match!(wrapping_shl(&self, &amt); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; rhs: U32)
    }

    fn ushr(self, other: Self) -> ValueResult<Self> {
        let amt = other
            .convert(ValueConversionKind::Exact(types::I32))?
            .convert(ValueConversionKind::ToUnsigned)?;
        binary_match!(wrapping_shr(&self, &amt); [U8, U16, U32, U64, U128]; rhs: U32)
    }

    fn ishr(self, other: Self) -> ValueResult<Self> {
        let amt = other
            .convert(ValueConversionKind::Exact(types::I32))?
            .convert(ValueConversionKind::ToUnsigned)?;
        binary_match!(wrapping_shr(&self, &amt); [I8, I16, I32, I64, I128]; rhs: U32)
    }

    fn rotl(self, other: Self) -> ValueResult<Self> {
        let amt = other
            .convert(ValueConversionKind::Exact(types::I32))?
            .convert(ValueConversionKind::ToUnsigned)?;
        binary_match!(rotate_left(&self, &amt); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; rhs: U32)
    }

    fn rotr(self, other: Self) -> ValueResult<Self> {
        let amt = other
            .convert(ValueConversionKind::Exact(types::I32))?
            .convert(ValueConversionKind::ToUnsigned)?;
        binary_match!(rotate_right(&self, &amt); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; rhs: U32)
    }

    fn and(self, other: Self) -> ValueResult<Self> {
        bitop!(&(self, other))
    }

    fn or(self, other: Self) -> ValueResult<Self> {
        bitop!(|(self, other))
    }

    fn xor(self, other: Self) -> ValueResult<Self> {
        bitop!(^(self, other))
    }

    fn not(self) -> ValueResult<Self> {
        Ok(match self {
            DataValue::I8(a) => DataValue::I8(!a),
            DataValue::I16(a) => DataValue::I16(!a),
            DataValue::I32(a) => DataValue::I32(!a),
            DataValue::I64(a) => DataValue::I64(!a),
            DataValue::I128(a) => DataValue::I128(!a),
            DataValue::F32(a) => DataValue::F32(!a),
            DataValue::F64(a) => DataValue::F64(!a),
            DataValue::V128(a) => {
                let mut a2 = a.clone();
                for a in a2.iter_mut() {
                    *a = !*a;
                }
                DataValue::V128(a2)
            }
            _ => unimplemented!(),
        })
    }

    fn count_ones(self) -> ValueResult<Self> {
        unary_match!(count_ones(&self); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; [i8, i16, i32, i64, i128, u8, u16, u32, u64, u128])
    }

    fn leading_ones(self) -> ValueResult<Self> {
        unary_match!(leading_ones(&self); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; [i8, i16, i32, i64, i128, u8, u16, u32, u64, u128])
    }

    fn leading_zeros(self) -> ValueResult<Self> {
        unary_match!(leading_zeros(&self); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; [i8, i16, i32, i64, i128, u8, u16, u32, u64, u128])
    }

    fn trailing_zeros(self) -> ValueResult<Self> {
        unary_match!(trailing_zeros(&self); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128]; [i8, i16, i32, i64, i128, u8, u16, u32, u64, u128])
    }

    fn reverse_bits(self) -> ValueResult<Self> {
        unary_match!(reverse_bits(&self); [I8, I16, I32, I64, I128, U8, U16, U32, U64, U128])
    }

    fn swap_bytes(self) -> ValueResult<Self> {
        unary_match!(swap_bytes(&self); [I16, I32, I64, I128, U16, U32, U64, U128])
    }
}
