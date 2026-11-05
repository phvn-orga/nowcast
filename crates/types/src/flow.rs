use std::fmt;

use crate::{BrickName, NameError};

/// Maximum length of a brick-relative flow id, in bytes.
pub const FLOW_ID_MAX_LEN: usize = 256;

/// A flow id as a brick knows it: an opaque UTF-8 string of at most
/// [`FLOW_ID_MAX_LEN`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowId(String);

/// A flow id longer than [`FLOW_ID_MAX_LEN`] bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowIdError {
    pub len: usize,
}

impl fmt::Display for FlowIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a flow id is {} bytes, the limit is {FLOW_ID_MAX_LEN}",
            self.len
        )
    }
}

impl std::error::Error for FlowIdError {}

impl FlowId {
    pub fn new(id: impl Into<String>) -> Result<Self, FlowIdError> {
        let id = id.into();
        if id.len() > FLOW_ID_MAX_LEN {
            return Err(FlowIdError { len: id.len() });
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FlowId {
    type Error = FlowIdError;

    fn try_from(id: String) -> Result<Self, FlowIdError> {
        Self::new(id)
    }
}

impl TryFrom<&str> for FlowId {
    type Error = FlowIdError;

    fn try_from(id: &str) -> Result<Self, FlowIdError> {
        Self::new(id)
    }
}

impl AsRef<str> for FlowId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for FlowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A flow id as clients see it: `<brick>/<flow id>`, so that two bricks never
/// share a flow.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QualifiedFlowId {
    brick: BrickName,
    flow: FlowId,
}

/// Why a string is not a qualified flow id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualifiedFlowIdError {
    /// No `/` separates the brick name from the flow id.
    MissingSeparator,
    Brick(NameError),
    Flow(FlowIdError),
}

impl fmt::Display for QualifiedFlowIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSeparator => f.write_str("a flow id must read <brick>/<flow>"),
            Self::Brick(e) => write!(f, "invalid brick name in flow id: {e}"),
            Self::Flow(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for QualifiedFlowIdError {}

impl QualifiedFlowId {
    pub fn new(brick: BrickName, flow: FlowId) -> Self {
        Self { brick, flow }
    }

    /// Reads `<brick>/<flow id>`. A brick name holds no `/`, so the first one
    /// is the separator and the flow id may contain more.
    pub fn parse(s: &str) -> Result<Self, QualifiedFlowIdError> {
        let (brick, flow) = s
            .split_once('/')
            .ok_or(QualifiedFlowIdError::MissingSeparator)?;
        Ok(Self {
            brick: BrickName::new(brick).map_err(QualifiedFlowIdError::Brick)?,
            flow: FlowId::new(flow).map_err(QualifiedFlowIdError::Flow)?,
        })
    }

    pub fn brick(&self) -> &BrickName {
        &self.brick
    }

    pub fn flow(&self) -> &FlowId {
        &self.flow
    }
}

impl std::str::FromStr for QualifiedFlowId {
    type Err = QualifiedFlowIdError;

    fn from_str(s: &str) -> Result<Self, QualifiedFlowIdError> {
        Self::parse(s)
    }
}

impl fmt::Display for QualifiedFlowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.brick, self.flow)
    }
}
