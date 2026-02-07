//! Signal definitions and references.
//!
//! A [`Signal`] represents a named wire, register, or latch within a module.
//! [`SignalRef`] provides a way to refer to a full signal, a bit-slice, or a concatenation.

use crate::ids::{ClockDomainId, SignalId, TypeId};
use aion_common::{Ident, LogicVec};
use aion_source::Span;
use serde::{Deserialize, Serialize};

use crate::const_value::ConstValue;

/// The kind of a signal, determining its storage semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SignalKind {
    /// A combinational signal (wire/net).
    Wire,
    /// A sequential signal (flip-flop output).
    Reg,
    /// A latch output (usually a lint warning).
    Latch,
    /// A signal that backs a port.
    Port,
    /// A compile-time constant.
    Const,
}

/// A signal (wire, register, or latch) within a module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    /// The unique ID of this signal within its module.
    pub id: SignalId,
    /// The signal name.
    pub name: Ident,
    /// The type of this signal.
    pub ty: TypeId,
    /// The storage kind (wire, reg, latch, etc.).
    pub kind: SignalKind,
    /// An optional initial/reset value.
    pub init: Option<ConstValue>,
    /// The clock domain this signal belongs to, if sequential.
    pub clock_domain: Option<ClockDomainId>,
    /// The source span where this signal was declared.
    pub span: Span,
}

/// A reference to a signal or part of a signal.
///
/// Used in connections, assignments, and expressions to refer to
/// full signals, bit-slices, concatenations, or constant values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SignalRef {
    /// A reference to a full signal.
    Signal(SignalId),
    /// A bit-slice of a signal.
    Slice {
        /// The signal being sliced.
        signal: SignalId,
        /// The high bit index (inclusive).
        high: u32,
        /// The low bit index (inclusive).
        low: u32,
    },
    /// A concatenation of signal references.
    Concat(Vec<SignalRef>),
    /// A constant value.
    Const(LogicVec),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_construction() {
        let sig = Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(1),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Wire,
            init: None,
            clock_domain: None,
            span: Span::DUMMY,
        };
        assert_eq!(sig.kind, SignalKind::Wire);
        assert!(sig.init.is_none());
    }

    #[test]
    fn signal_with_init() {
        let sig = Signal {
            id: SignalId::from_raw(0),
            name: Ident::from_raw(1),
            ty: TypeId::from_raw(0),
            kind: SignalKind::Reg,
            init: Some(ConstValue::Int(0)),
            clock_domain: Some(ClockDomainId::from_raw(0)),
            span: Span::DUMMY,
        };
        assert_eq!(sig.kind, SignalKind::Reg);
        assert!(sig.init.is_some());
        assert!(sig.clock_domain.is_some());
    }

    #[test]
    fn signal_kinds_distinct() {
        let kinds = [
            SignalKind::Wire,
            SignalKind::Reg,
            SignalKind::Latch,
            SignalKind::Port,
            SignalKind::Const,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for (j, b) in kinds.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn signal_ref_full() {
        let r = SignalRef::Signal(SignalId::from_raw(5));
        assert_eq!(r, SignalRef::Signal(SignalId::from_raw(5)));
    }

    #[test]
    fn signal_ref_slice() {
        let r = SignalRef::Slice {
            signal: SignalId::from_raw(3),
            high: 7,
            low: 0,
        };
        if let SignalRef::Slice { high, low, .. } = r {
            assert_eq!(high, 7);
            assert_eq!(low, 0);
        } else {
            panic!("expected Slice variant");
        }
    }

    #[test]
    fn signal_ref_concat() {
        let r = SignalRef::Concat(vec![
            SignalRef::Signal(SignalId::from_raw(0)),
            SignalRef::Signal(SignalId::from_raw(1)),
        ]);
        if let SignalRef::Concat(refs) = r {
            assert_eq!(refs.len(), 2);
        } else {
            panic!("expected Concat variant");
        }
    }

    #[test]
    fn signal_ref_const() {
        let lv = LogicVec::all_zero(4);
        let r = SignalRef::Const(lv);
        if let SignalRef::Const(v) = r {
            assert_eq!(v.width(), 4);
        } else {
            panic!("expected Const variant");
        }
    }
}
