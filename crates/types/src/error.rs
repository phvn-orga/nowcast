use std::fmt;

/// A string that names no known error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownCode(pub String);

impl fmt::Display for UnknownCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown error code {:?}", self.0)
    }
}

impl std::error::Error for UnknownCode {}

macro_rules! error_codes {
    ($(#[$doc:meta])* $ty:ident { $($(#[$vdoc:meta])* $variant:ident => $code:literal,)+ }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $ty {
            $($(#[$vdoc])* $variant,)+
        }

        impl $ty {
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The code as the contract spells it.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $code,)+
                }
            }
        }

        impl std::str::FromStr for $ty {
            type Err = UnknownCode;

            fn from_str(code: &str) -> Result<Self, UnknownCode> {
                match code {
                    $($code => Ok(Self::$variant),)+
                    _ => Err(UnknownCode(code.to_owned())),
                }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

error_codes!(
    /// The error codes of the client API.
    ApiErrorCode {
        /// A request came before the session was declared.
        SessionRequired => "session_required",
        /// The subscription params do not match the kind's schema.
        InvalidParams => "invalid_params",
        UnknownBrick => "unknown_brick",
        UnknownKind => "unknown_kind",
        /// The flow does not exist, or the anchor is not subscribed to it.
        UnknownFlow => "unknown_flow",
        /// The brick did not answer `ready` in time.
        BrickUnavailable => "brick_unavailable",
        /// The brick answered `subscribe` with `error`.
        SubscriptionRefused => "subscription_refused",
        /// The flow's subscription kind accepts no input.
        InputNotAccepted => "input_not_accepted",
        /// The input is not JSON, or does not match the accepted type's schema.
        InvalidInput => "invalid_input",
    }
);

error_codes!(
    /// The codes the service answers a brick's `publish` with.
    PublishErrorCode {
        /// The detail is not valid JSON.
        InvalidJson => "invalid_json",
        /// The flow was never assigned, or was released.
        UnknownFlow => "unknown_flow",
        /// The type is missing from the brick's declaration file.
        UndeclaredType => "undeclared_type",
    }
);

/// An error of the client API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub code: ApiErrorCode,
    pub message: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}
