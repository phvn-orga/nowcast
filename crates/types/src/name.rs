use std::fmt;

/// Why a string is not a valid name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    /// A character outside `[a-z0-9-]`, at this byte offset.
    InvalidChar {
        index: usize,
        found: char,
    },
}

impl fmt::Display for NameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("a name cannot be empty"),
            Self::InvalidChar { index, found } => write!(
                f,
                "invalid character {found:?} at byte {index}, a name only allows [a-z0-9-]"
            ),
        }
    }
}

impl std::error::Error for NameError {}

fn validate(s: &str) -> Result<(), NameError> {
    if s.is_empty() {
        return Err(NameError::Empty);
    }
    match s
        .char_indices()
        .find(|&(_, c)| !matches!(c, 'a'..='z' | '0'..='9' | '-'))
    {
        Some((index, found)) => Err(NameError::InvalidChar { index, found }),
        None => Ok(()),
    }
}

macro_rules! name_type {
    ($(#[$doc:meta])* $ty:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $ty(String);

        impl $ty {
            pub fn new(name: impl Into<String>) -> Result<Self, NameError> {
                let name = name.into();
                validate(&name)?;
                Ok(Self(name))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $ty {
            type Error = NameError;

            fn try_from(name: String) -> Result<Self, NameError> {
                Self::new(name)
            }
        }

        impl TryFrom<&str> for $ty {
            type Error = NameError;

            fn try_from(name: &str) -> Result<Self, NameError> {
                Self::new(name)
            }
        }

        impl std::str::FromStr for $ty {
            type Err = NameError;

            fn from_str(name: &str) -> Result<Self, NameError> {
                Self::new(name)
            }
        }

        impl AsRef<str> for $ty {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// The unique name of a brick, `[a-z0-9-]+`.
    BrickName
);

name_type!(
    /// The name of a client kind, `[a-z0-9-]+`, equal to its declaration file name.
    ClientKindName
);
