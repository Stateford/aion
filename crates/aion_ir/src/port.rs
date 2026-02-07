//! Port definitions for module interfaces.
//!
//! A [`Port`] represents one signal in a module's external interface,
//! with a direction and backing signal within the module.

use crate::ids::{PortId, SignalId, TypeId};
use aion_common::Ident;
use aion_source::Span;
use serde::{Deserialize, Serialize};

/// The direction of a port on a module boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PortDirection {
    /// An input port (data flows into the module).
    Input,
    /// An output port (data flows out of the module).
    Output,
    /// A bidirectional port (data flows both ways).
    InOut,
}

/// A port in a module's external interface.
///
/// Each port is backed by a [`SignalId`] inside the module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Port {
    /// The unique ID of this port.
    pub id: PortId,
    /// The port name.
    pub name: Ident,
    /// The direction of data flow.
    pub direction: PortDirection,
    /// The type of the port.
    pub ty: TypeId,
    /// The signal within the module that backs this port.
    pub signal: SignalId,
    /// The source span where this port was declared.
    pub span: Span,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_port(dir: PortDirection) -> Port {
        Port {
            id: PortId::from_raw(0),
            name: Ident::from_raw(1),
            direction: dir,
            ty: TypeId::from_raw(0),
            signal: SignalId::from_raw(0),
            span: Span::DUMMY,
        }
    }

    #[test]
    fn port_construction() {
        let p = dummy_port(PortDirection::Input);
        assert_eq!(p.direction, PortDirection::Input);
        assert_eq!(p.id.as_raw(), 0);
    }

    #[test]
    fn port_directions_distinct() {
        assert_ne!(PortDirection::Input, PortDirection::Output);
        assert_ne!(PortDirection::Output, PortDirection::InOut);
        assert_ne!(PortDirection::Input, PortDirection::InOut);
    }

    #[test]
    fn port_serde_roundtrip() {
        let p = dummy_port(PortDirection::Output);
        let json = serde_json::to_string(&p).unwrap();
        let restored: Port = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.direction, PortDirection::Output);
        assert_eq!(restored.id, p.id);
    }
}
