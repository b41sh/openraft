use std::collections::BTreeMap;
use std::collections::BTreeSet;

use maplit::btreemap;

use crate::Node;
use crate::NodeId;

/// Convert into a map of `Node`.
///
/// This is used as a user input acceptor when building a Membership, to convert various input types
/// into a map of `Node`.
pub trait IntoNodes<NID, N>
where
    N: Node,
    NID: NodeId,
{
    fn has_nodes(&self) -> bool;
    fn node_ids(&self) -> Vec<NID>;
    fn into_nodes(self) -> BTreeMap<NID, N>;
}

impl<NID, N> IntoNodes<NID, N> for ()
where
    N: Node,
    NID: NodeId,
{
    fn has_nodes(&self) -> bool {
        false
    }

    fn node_ids(&self) -> Vec<NID> {
        vec![]
    }

    fn into_nodes(self) -> BTreeMap<NID, N> {
        btreemap! {}
    }
}

impl<NID, N> IntoNodes<NID, N> for BTreeSet<NID>
where
    N: Node,
    NID: NodeId,
{
    fn has_nodes(&self) -> bool {
        false
    }

    fn node_ids(&self) -> Vec<NID> {
        self.iter().copied().collect()
    }

    fn into_nodes(self) -> BTreeMap<NID, N> {
        self.into_iter().map(|node_id| (node_id, N::default())).collect()
    }
}

impl<NID, N> IntoNodes<NID, N> for Option<BTreeSet<NID>>
where
    N: Node,
    NID: NodeId,
{
    fn has_nodes(&self) -> bool {
        true
    }

    fn node_ids(&self) -> Vec<NID> {
        match self {
            None => {
                vec![]
            }
            Some(bs) => bs.iter().copied().collect(),
        }
    }

    fn into_nodes(self) -> BTreeMap<NID, N> {
        match self {
            None => BTreeMap::new(),
            Some(s) => s.into_iter().map(|node_id| (node_id, N::default())).collect(),
        }
    }
}

impl<NID, N> IntoNodes<NID, N> for BTreeMap<NID, N>
where
    N: Node,
    NID: NodeId,
{
    fn has_nodes(&self) -> bool {
        true
    }

    fn node_ids(&self) -> Vec<NID> {
        self.keys().copied().collect()
    }

    fn into_nodes(self) -> BTreeMap<NID, N> {
        self
    }
}
