// Copyright 2018 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![allow(unknown_lints)]

use ty::layout::{HasDataLayout, Size};
use ty::subst::Substs;
use hir::def_id::DefId;

use super::{EvalResult, Pointer, PointerArithmetic, Allocation, AllocId, sign_extend, truncate};

/// Represents a constant value in Rust. Scalar and ScalarPair are optimizations which
/// matches the LocalValue optimizations for easy conversions between Value and ConstValue.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord, RustcEncodable, RustcDecodable, Hash)]
pub enum ConstValue<'tcx> {
    /// Never returned from the `const_eval` query, but the HIR contains these frequently in order
    /// to allow HIR creation to happen for everything before needing to be able to run constant
    /// evaluation
    Unevaluated(DefId, &'tcx Substs<'tcx>),

    /// Used only for types with layout::abi::Scalar ABI and ZSTs
    ///
    /// Not using the enum `Value` to encode that this must not be `Undef`
    Scalar(Scalar),

    /// Used only for *fat pointers* with layout::abi::ScalarPair
    ///
    /// Needed for pattern matching code related to slices and strings.
    ScalarPair(Scalar, Scalar),

    /// An allocation + offset into the allocation.
    /// Invariant: The AllocId matches the allocation.
    ByRef(AllocId, &'tcx Allocation, Size),
}

impl<'tcx> ConstValue<'tcx> {
    #[inline]
    pub fn try_to_scalar(&self) -> Option<Scalar> {
        match *self {
            ConstValue::Unevaluated(..) |
            ConstValue::ByRef(..) |
            ConstValue::ScalarPair(..) => None,
            ConstValue::Scalar(val) => Some(val),
        }
    }

    #[inline]
    pub fn try_to_bits(&self, size: Size) -> Option<u128> {
        self.try_to_scalar()?.to_bits(size).ok()
    }

    #[inline]
    pub fn try_to_ptr(&self) -> Option<Pointer> {
        self.try_to_scalar()?.to_ptr().ok()
    }

    #[inline]
    pub fn new_slice(
        val: Scalar,
        len: u64,
        cx: impl HasDataLayout
    ) -> Self {
        ConstValue::ScalarPair(val, Scalar::Bits {
            bits: len as u128,
            size: cx.data_layout().pointer_size.bytes() as u8,
        })
    }

    #[inline]
    pub fn new_dyn_trait(val: Scalar, vtable: Pointer) -> Self {
        ConstValue::ScalarPair(val, Scalar::Ptr(vtable))
    }
}

impl<'tcx> Scalar {
    #[inline]
    pub fn ptr_null(cx: impl HasDataLayout) -> Self {
        Scalar::Bits {
            bits: 0,
            size: cx.data_layout().pointer_size.bytes() as u8,
        }
    }

    #[inline]
    pub fn zst() -> Self {
        Scalar::Bits { bits: 0, size: 0 }
    }

    #[inline]
    pub fn ptr_signed_offset(self, i: i64, cx: impl HasDataLayout) -> EvalResult<'tcx, Self> {
        let layout = cx.data_layout();
        match self {
            Scalar::Bits { bits, size } => {
                assert_eq!(size as u64, layout.pointer_size.bytes());
                Ok(Scalar::Bits {
                    bits: layout.signed_offset(bits as u64, i)? as u128,
                    size,
                })
            }
            Scalar::Ptr(ptr) => ptr.signed_offset(i, layout).map(Scalar::Ptr),
        }
    }

    #[inline]
    pub fn ptr_offset(self, i: Size, cx: impl HasDataLayout) -> EvalResult<'tcx, Self> {
        let layout = cx.data_layout();
        match self {
            Scalar::Bits { bits, size } => {
                assert_eq!(size as u64, layout.pointer_size.bytes());
                Ok(Scalar::Bits {
                    bits: layout.offset(bits as u64, i.bytes())? as u128,
                    size,
                })
            }
            Scalar::Ptr(ptr) => ptr.offset(i, layout).map(Scalar::Ptr),
        }
    }

    #[inline]
    pub fn ptr_wrapping_signed_offset(self, i: i64, cx: impl HasDataLayout) -> Self {
        let layout = cx.data_layout();
        match self {
            Scalar::Bits { bits, size } => {
                assert_eq!(size as u64, layout.pointer_size.bytes());
                Scalar::Bits {
                    bits: layout.wrapping_signed_offset(bits as u64, i) as u128,
                    size,
                }
            }
            Scalar::Ptr(ptr) => Scalar::Ptr(ptr.wrapping_signed_offset(i, layout)),
        }
    }

    #[inline]
    pub fn is_null_ptr(self, cx: impl HasDataLayout) -> bool {
        match self {
            Scalar::Bits { bits, size } =>  {
                assert_eq!(size as u64, cx.data_layout().pointer_size.bytes());
                bits == 0
            },
            Scalar::Ptr(_) => false,
        }
    }

    #[inline]
    pub fn is_null(self) -> bool {
        match self {
            Scalar::Bits { bits, .. } => bits == 0,
            Scalar::Ptr(_) => false
        }
    }

    #[inline]
    pub fn from_bool(b: bool) -> Self {
        Scalar::Bits { bits: b as u128, size: 1 }
    }

    #[inline]
    pub fn from_char(c: char) -> Self {
        Scalar::Bits { bits: c as u128, size: 4 }
    }

    #[inline]
    pub fn from_uint(i: impl Into<u128>, size: Size) -> Self {
        let i = i.into();
        debug_assert_eq!(truncate(i, size), i,
                    "Unsigned value {} does not fit in {} bits", i, size.bits());
        Scalar::Bits { bits: i, size: size.bytes() as u8 }
    }

    #[inline]
    pub fn from_int(i: impl Into<i128>, size: Size) -> Self {
        let i = i.into();
        // `into` performed sign extension, we have to truncate
        let truncated = truncate(i as u128, size);
        debug_assert_eq!(sign_extend(truncated, size) as i128, i,
                    "Signed value {} does not fit in {} bits", i, size.bits());
        Scalar::Bits { bits: truncated, size: size.bytes() as u8 }
    }

    #[inline]
    pub fn from_f32(f: f32) -> Self {
        Scalar::Bits { bits: f.to_bits() as u128, size: 4 }
    }

    #[inline]
    pub fn from_f64(f: f64) -> Self {
        Scalar::Bits { bits: f.to_bits() as u128, size: 8 }
    }

    #[inline]
    pub fn to_bits(self, target_size: Size) -> EvalResult<'tcx, u128> {
        match self {
            Scalar::Bits { bits, size } => {
                assert_eq!(target_size.bytes(), size as u64);
                assert_ne!(size, 0, "to_bits cannot be used with zsts");
                Ok(bits)
            }
            Scalar::Ptr(_) => err!(ReadPointerAsBytes),
        }
    }

    #[inline]
    pub fn to_ptr(self) -> EvalResult<'tcx, Pointer> {
        match self {
            Scalar::Bits { bits: 0, .. } => err!(InvalidNullPointerUsage),
            Scalar::Bits { .. } => err!(ReadBytesAsPointer),
            Scalar::Ptr(p) => Ok(p),
        }
    }

    #[inline]
    pub fn is_bits(self) -> bool {
        match self {
            Scalar::Bits { .. } => true,
            _ => false,
        }
    }

    #[inline]
    pub fn is_ptr(self) -> bool {
        match self {
            Scalar::Ptr(_) => true,
            _ => false,
        }
    }

    pub fn to_bool(self) -> EvalResult<'tcx, bool> {
        match self {
            Scalar::Bits { bits: 0, size: 1 } => Ok(false),
            Scalar::Bits { bits: 1, size: 1 } => Ok(true),
            _ => err!(InvalidBool),
        }
    }

    pub fn to_char(self) -> EvalResult<'tcx, char> {
        let val = self.to_u32()?;
        match ::std::char::from_u32(val) {
            Some(c) => Ok(c),
            None => err!(InvalidChar(val as u128)),
        }
    }

    pub fn to_u8(self) -> EvalResult<'static, u8> {
        let sz = Size::from_bits(8);
        let b = self.to_bits(sz)?;
        assert_eq!(b as u8 as u128, b);
        Ok(b as u8)
    }

    pub fn to_u32(self) -> EvalResult<'static, u32> {
        let sz = Size::from_bits(32);
        let b = self.to_bits(sz)?;
        assert_eq!(b as u32 as u128, b);
        Ok(b as u32)
    }

    pub fn to_u64(self) -> EvalResult<'static, u64> {
        let sz = Size::from_bits(64);
        let b = self.to_bits(sz)?;
        assert_eq!(b as u64 as u128, b);
        Ok(b as u64)
    }

    pub fn to_usize(self, cx: impl HasDataLayout) -> EvalResult<'static, u64> {
        let b = self.to_bits(cx.data_layout().pointer_size)?;
        assert_eq!(b as u64 as u128, b);
        Ok(b as u64)
    }

    pub fn to_i8(self) -> EvalResult<'static, i8> {
        let sz = Size::from_bits(8);
        let b = self.to_bits(sz)?;
        let b = sign_extend(b, sz) as i128;
        assert_eq!(b as i8 as i128, b);
        Ok(b as i8)
    }

    pub fn to_i32(self) -> EvalResult<'static, i32> {
        let sz = Size::from_bits(32);
        let b = self.to_bits(sz)?;
        let b = sign_extend(b, sz) as i128;
        assert_eq!(b as i32 as i128, b);
        Ok(b as i32)
    }

    pub fn to_i64(self) -> EvalResult<'static, i64> {
        let sz = Size::from_bits(64);
        let b = self.to_bits(sz)?;
        let b = sign_extend(b, sz) as i128;
        assert_eq!(b as i64 as i128, b);
        Ok(b as i64)
    }

    pub fn to_isize(self, cx: impl HasDataLayout) -> EvalResult<'static, i64> {
        let b = self.to_bits(cx.data_layout().pointer_size)?;
        let b = sign_extend(b, cx.data_layout().pointer_size) as i128;
        assert_eq!(b as i64 as i128, b);
        Ok(b as i64)
    }

    #[inline]
    pub fn to_f32(self) -> EvalResult<'static, f32> {
        Ok(f32::from_bits(self.to_u32()?))
    }

    #[inline]
    pub fn to_f64(self) -> EvalResult<'static, f64> {
        Ok(f64::from_bits(self.to_u64()?))
    }
}

impl From<Pointer> for Scalar {
    #[inline(always)]
    fn from(ptr: Pointer) -> Self {
        Scalar::Ptr(ptr)
    }
}

/// A `Scalar` represents an immediate, primitive value existing outside of a
/// `memory::Allocation`. It is in many ways like a small chunk of a `Allocation`, up to 8 bytes in
/// size. Like a range of bytes in an `Allocation`, a `Scalar` can either represent the raw bytes
/// of a simple value or a pointer into another `Allocation`
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, RustcEncodable, RustcDecodable, Hash)]
pub enum Scalar<Id=AllocId> {
    /// The raw bytes of a simple value.
    Bits {
        /// The first `size` bytes are the value.
        /// Do not try to read less or more bytes that that. The remaining bytes must be 0.
        size: u8,
        bits: u128,
    },

    /// A pointer into an `Allocation`. An `Allocation` in the `memory` module has a list of
    /// relocations, but a `Scalar` is only large enough to contain one, so we just represent the
    /// relocation and its associated offset together as a `Pointer` here.
    Ptr(Pointer<Id>),
}
