use crate::{EdgeKind, NodeId};

#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EdgeEndpoint {
    source: NodeId,
    target: NodeId,
    kind: EdgeKind,
}

impl EdgeEndpoint {
    pub const fn new(source: NodeId, target: NodeId, kind: EdgeKind) -> Self {
        Self {
            source,
            target,
            kind,
        }
    }

    pub const fn source(self) -> NodeId {
        self.source
    }

    pub const fn target(self) -> NodeId {
        self.target
    }

    pub const fn kind(self) -> EdgeKind {
        self.kind
    }
}
