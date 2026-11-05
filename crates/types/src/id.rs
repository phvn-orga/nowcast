use std::fmt;

macro_rules! opaque_string {
    ($(#[$doc:meta])* $ty:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $ty(String);

        impl $ty {
            pub fn new(id: impl Into<String>) -> Self {
                Self(id.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $ty {
            fn from(id: String) -> Self {
                Self(id)
            }
        }

        impl From<&str> for $ty {
            fn from(id: &str) -> Self {
                Self(id.to_owned())
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

opaque_string!(
    /// The session an anchor process declares, as the agent names it.
    SessionId
);

opaque_string!(
    /// A subscription, issued by the service and private to one subscriber.
    SubscriptionId
);

/// The id of an event, issued by the service and increasing within a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventId(pub u64);

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The `req` of the brick protocol: chosen by the sender of a request, echoed
/// by its reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
