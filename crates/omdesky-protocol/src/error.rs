use serde::{Deserialize, Serialize};

macro_rules! error_codes {
    ($($variant:ident => ($code:literal, $status:literal, $retryable:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum ErrorCode {
            $($variant),+
        }

        impl ErrorCode {
            pub const ALL: &[Self] = &[$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $code),+
                }
            }

            pub const fn http_status(self) -> u16 {
                match self {
                    $(Self::$variant => $status),+
                }
            }

            pub const fn retryable(self) -> bool {
                match self {
                    $(Self::$variant => $retryable),+
                }
            }

            pub fn from_code(code: &str) -> Option<Self> {
                match code {
                    $($code => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

error_codes! {
    Unauthorized => ("UNAUTHORIZED", 401, false),
    CapabilityDenied => ("CAPABILITY_DENIED", 403, false),
    CallbackAccessMissing => ("CALLBACK_ACCESS_MISSING", 403, false),
    RateLimited => ("RATE_LIMITED", 429, true),
    IdentityUnavailable => ("IDENTITY_UNAVAILABLE", 503, true),
    AccessStoreFailed => ("ACCESS_STORE_FAILED", 500, false),
    VersionIncompatible => ("VERSION_INCOMPATIBLE", 409, false),
    ControllerUnreachable => ("CONTROLLER_UNREACHABLE", 502, true),
    NoController => ("NO_CONTROLLER", 409, false),
    InvalidCommand => ("INVALID_COMMAND", 400, false),
    RequestStale => ("REQUEST_STALE", 400, false),
    RequestReplayed => ("REQUEST_REPLAYED", 409, false),
    PairingChallengeInvalid => ("PAIRING_CHALLENGE_INVALID", 403, false),
    SessionAlreadyOwned => ("SESSION_ALREADY_OWNED", 409, false),
    SessionNotOwned => ("SESSION_NOT_OWNED", 403, false),
    SessionNotFound => ("SESSION_NOT_FOUND", 404, false),
    SessionEffectsFailed => ("SESSION_EFFECTS_FAILED", 503, true),
    HyprlandUnavailable => ("HYPRLAND_UNAVAILABLE", 503, true),
    DisplayNotFound => ("DISPLAY_NOT_FOUND", 404, false),
    WorkspaceNotFound => ("WORKSPACE_NOT_FOUND", 404, false),
    WindowNotFound => ("WINDOW_NOT_FOUND", 404, false),
    SunshineNotInstalled => ("SUNSHINE_NOT_INSTALLED", 503, false),
    SunshineNotRunning => ("SUNSHINE_NOT_RUNNING", 503, true),
    SunshineApiUnavailable => ("SUNSHINE_API_UNAVAILABLE", 503, false),
    SunshinePairingFailed => ("SUNSHINE_PAIRING_FAILED", 502, false),
    Internal => ("INTERNAL_ERROR", 500, false),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ProtocolError,
}

impl ErrorEnvelope {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            error: ProtocolError {
                code: code.as_str().to_owned(),
                message: message.into(),
                retryable: code.retryable(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl ProtocolError {
    pub fn known_code(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(&self.code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_every_code_round_trips_through_its_wire_string() {
        for code in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_code(code.as_str()), Some(*code));
        }
    }

    #[test]
    fn test_wire_strings_are_unique_uppercase_tokens() {
        let mut seen = HashSet::new();

        for code in ErrorCode::ALL {
            let value = code.as_str();

            assert!(seen.insert(value), "{value} is duplicated");
            assert!(
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte == b'_'),
                "{value} is not an uppercase token"
            );
        }
    }

    #[test]
    fn test_every_code_maps_to_an_error_status() {
        for code in ErrorCode::ALL {
            assert!((400..600).contains(&code.http_status()), "{code:?}");
        }
    }

    #[test]
    fn test_an_unknown_code_is_not_recognized() {
        assert_eq!(ErrorCode::from_code("FUTURE_ERROR"), None);
    }

    #[test]
    fn test_envelope_carries_the_code_and_its_retryability() {
        let envelope = ErrorEnvelope::new(ErrorCode::RateLimited, "slow down");

        let value = serde_json::to_value(&envelope).expect("serializable envelope");

        assert_eq!(value["error"]["code"], "RATE_LIMITED");
        assert_eq!(value["error"]["retryable"], true);
        assert_eq!(value["error"]["message"], "slow down");
    }

    #[test]
    fn test_envelope_tolerates_unknown_fields() {
        let envelope: ErrorEnvelope = serde_json::from_str(
            r#"{"error":{"code":"FUTURE_ERROR","message":"m","retryable":false,"details":{}},"extra":1}"#,
        )
        .expect("tolerant envelope");

        assert_eq!(envelope.error.known_code(), None);
    }
}
