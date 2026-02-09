//! Route tree data structures representing physical wiring solutions.
//!
//! A [`RouteTree`] describes the physical routing of a single net from its
//! driver pin to all sink pins. It is a tree of [`RouteNode`]s, where each
//! node represents a routing resource (site pin, wire, or PIP) in the device.

use aion_arch::ids::{PipId, SiteId, WireId};
use serde::{Deserialize, Serialize};

/// A routing solution for a single net.
///
/// Stored as a tree rooted at the driver pin, branching to each sink.
/// After successful routing, every net in the PnR netlist has a `RouteTree`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteTree {
    /// The root node (driver side) of the route tree.
    pub root: RouteNode,
}

impl RouteTree {
    /// Creates a new route tree with the given root node.
    pub fn new(root: RouteNode) -> Self {
        Self { root }
    }

    /// Creates a stub route tree (direct connection with no intermediate resources).
    ///
    /// Used as a placeholder when the routing graph is not available (Phase 2).
    pub fn stub() -> Self {
        Self {
            root: RouteNode {
                resource: RouteResource::Direct,
                children: Vec::new(),
            },
        }
    }

    /// Returns the total number of routing resources used by this tree.
    pub fn resource_count(&self) -> usize {
        self.root.subtree_size()
    }

    /// Returns the depth of the routing tree (longest path from root to leaf).
    pub fn depth(&self) -> usize {
        self.root.depth()
    }

    /// Returns all wire resources used in this route tree.
    pub fn wires_used(&self) -> Vec<WireId> {
        let mut wires = Vec::new();
        self.root.collect_wires(&mut wires);
        wires
    }

    /// Returns all PIP resources used in this route tree.
    pub fn pips_used(&self) -> Vec<PipId> {
        let mut pips = Vec::new();
        self.root.collect_pips(&mut pips);
        pips
    }
}

/// A node in a route tree.
///
/// Each node represents one routing resource (site pin, wire, PIP, or direct
/// connection) and can branch to multiple children for fanout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteNode {
    /// The routing resource at this node.
    pub resource: RouteResource,
    /// Child nodes (branches) in the route tree.
    pub children: Vec<RouteNode>,
}

impl RouteNode {
    /// Returns the total number of nodes in this subtree (including self).
    pub fn subtree_size(&self) -> usize {
        1 + self
            .children
            .iter()
            .map(|c| c.subtree_size())
            .sum::<usize>()
    }

    /// Returns the depth of this subtree (longest path from self to leaf).
    pub fn depth(&self) -> usize {
        if self.children.is_empty() {
            1
        } else {
            1 + self.children.iter().map(|c| c.depth()).max().unwrap_or(0)
        }
    }

    /// Collects all wire resources in this subtree.
    fn collect_wires(&self, wires: &mut Vec<WireId>) {
        if let RouteResource::Wire(w) = self.resource {
            wires.push(w);
        }
        for child in &self.children {
            child.collect_wires(wires);
        }
    }

    /// Collects all PIP resources in this subtree.
    fn collect_pips(&self, pips: &mut Vec<PipId>) {
        if let RouteResource::Pip(p) = self.resource {
            pips.push(p);
        }
        for child in &self.children {
            child.collect_pips(pips);
        }
    }
}

/// A routing resource in the device fabric.
///
/// Each [`RouteNode`] holds one of these, identifying what physical resource
/// is used at that point in the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RouteResource {
    /// A pin on a site (source or sink of a route).
    SitePin {
        /// The site containing the pin.
        site: SiteId,
        /// The pin index within the site.
        pin: u32,
    },
    /// A routing wire in the interconnect fabric.
    Wire(WireId),
    /// A programmable interconnect point connecting two wires.
    Pip(PipId),
    /// A direct connection (no physical resource — stub for Phase 2).
    Direct,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_route_tree() {
        let rt = RouteTree::stub();
        assert_eq!(rt.resource_count(), 1);
        assert_eq!(rt.depth(), 1);
        assert!(rt.wires_used().is_empty());
        assert!(rt.pips_used().is_empty());
    }

    #[test]
    fn route_tree_with_wires() {
        let rt = RouteTree::new(RouteNode {
            resource: RouteResource::SitePin {
                site: SiteId::from_raw(0),
                pin: 0,
            },
            children: vec![RouteNode {
                resource: RouteResource::Wire(WireId::from_raw(1)),
                children: vec![RouteNode {
                    resource: RouteResource::Pip(PipId::from_raw(2)),
                    children: vec![RouteNode {
                        resource: RouteResource::SitePin {
                            site: SiteId::from_raw(1),
                            pin: 0,
                        },
                        children: vec![],
                    }],
                }],
            }],
        });
        assert_eq!(rt.resource_count(), 4);
        assert_eq!(rt.depth(), 4);
        assert_eq!(rt.wires_used().len(), 1);
        assert_eq!(rt.pips_used().len(), 1);
    }

    #[test]
    fn route_tree_fanout() {
        let rt = RouteTree::new(RouteNode {
            resource: RouteResource::SitePin {
                site: SiteId::from_raw(0),
                pin: 0,
            },
            children: vec![
                RouteNode {
                    resource: RouteResource::Wire(WireId::from_raw(1)),
                    children: vec![],
                },
                RouteNode {
                    resource: RouteResource::Wire(WireId::from_raw(2)),
                    children: vec![],
                },
                RouteNode {
                    resource: RouteResource::Wire(WireId::from_raw(3)),
                    children: vec![],
                },
            ],
        });
        assert_eq!(rt.resource_count(), 4);
        assert_eq!(rt.depth(), 2);
        assert_eq!(rt.wires_used().len(), 3);
    }

    #[test]
    fn route_resource_equality() {
        let a = RouteResource::Wire(WireId::from_raw(5));
        let b = RouteResource::Wire(WireId::from_raw(5));
        let c = RouteResource::Wire(WireId::from_raw(6));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn route_resource_variants() {
        let resources = [
            RouteResource::SitePin {
                site: SiteId::from_raw(0),
                pin: 0,
            },
            RouteResource::Wire(WireId::from_raw(0)),
            RouteResource::Pip(PipId::from_raw(0)),
            RouteResource::Direct,
        ];
        // All distinct
        for (i, a) in resources.iter().enumerate() {
            for (j, b) in resources.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn serde_roundtrip() {
        let rt = RouteTree::new(RouteNode {
            resource: RouteResource::Wire(WireId::from_raw(42)),
            children: vec![RouteNode {
                resource: RouteResource::Direct,
                children: vec![],
            }],
        });
        let json = serde_json::to_string(&rt).unwrap();
        let restored: RouteTree = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.resource_count(), 2);
    }

    #[test]
    fn empty_children_depth() {
        let node = RouteNode {
            resource: RouteResource::Direct,
            children: vec![],
        };
        assert_eq!(node.depth(), 1);
        assert_eq!(node.subtree_size(), 1);
    }
}
