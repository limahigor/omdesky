use axum::{
    Json,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use omdesky_application::ports::PortError;
use omdesky_core::DomainError;
use omdesky_protocol::{ErrorCode, ErrorEnvelope};
use serde::de::DeserializeOwned;

use crate::{
    authorize::AuthorizationError, pairing::PairingError, replay::FreshnessError,
    session::SessionError,
};

#[derive(Debug)]
pub struct ApiError {
    code: ErrorCode,
    message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        self.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code.http_status())
            .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        (status, Json(ErrorEnvelope::new(self.code, self.message))).into_response()
    }
}

impl From<PortError> for ApiError {
    fn from(error: PortError) -> Self {
        tracing::debug!(
            code = error.code,
            detail = %error.message,
            retryable = error.retryable,
            "request.port_error"
        );

        let code = ErrorCode::from_code(error.code).unwrap_or(ErrorCode::Internal);

        Self::new(code, error.user_message())
    }
}

impl From<DomainError> for ApiError {
    fn from(error: DomainError) -> Self {
        tracing::debug!(detail = %error, "request.validation_failed");

        Self::new(
            ErrorCode::InvalidCommand,
            "The requested action is not valid.",
        )
    }
}

impl From<SessionError> for ApiError {
    fn from(error: SessionError) -> Self {
        Self::new(error.code(), error.message())
    }
}

impl From<PairingError> for ApiError {
    fn from(error: PairingError) -> Self {
        Self::new(error.code(), error.message())
    }
}

impl From<FreshnessError> for ApiError {
    fn from(error: FreshnessError) -> Self {
        Self::new(error.code(), error.message())
    }
}

impl From<AuthorizationError> for ApiError {
    fn from(error: AuthorizationError) -> Self {
        match error {
            AuthorizationError::RateLimited => Self::new(
                ErrorCode::RateLimited,
                "Too many requests. Please slow down.",
            ),
            AuthorizationError::Unavailable => Self::new(
                ErrorCode::IdentityUnavailable,
                "This device could not be identified right now. Please try again.",
            ),
            AuthorizationError::Forbidden => Self::new(
                ErrorCode::CapabilityDenied,
                "This device is not allowed to perform that action.",
            ),
            AuthorizationError::AccessStoreFailed => Self::new(
                ErrorCode::AccessStoreFailed,
                "The allowed devices list could not be read.",
            ),
            AuthorizationError::OutsideTailnet | AuthorizationError::Unauthorized => Self::new(
                ErrorCode::Unauthorized,
                "This device is not allowed to control Omdesky.",
            ),
        }
    }
}

pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(body_rejected(&rejection)),
        }
    }
}

fn body_rejected(rejection: &JsonRejection) -> ApiError {
    tracing::debug!(detail = %rejection.body_text(), "request.body_rejected");

    ApiError::new(ErrorCode::InvalidCommand, "The request body is not valid.")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn envelope(error: ApiError) -> (StatusCode, ErrorEnvelope) {
        let response = error.into_response();
        let status = response.status();

        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("readable body");

        (
            status,
            serde_json::from_slice(&body).expect("error envelope"),
        )
    }

    #[tokio::test]
    async fn test_status_and_retryability_follow_the_error_code() {
        let (status, envelope) = envelope(ApiError::new(ErrorCode::RateLimited, "slow")).await;

        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(envelope.error.code, "RATE_LIMITED");
        assert!(envelope.error.retryable);
    }

    #[tokio::test]
    async fn test_port_error_response_hides_internal_details() {
        let error = PortError::new(
            "HYPRLAND_UNAVAILABLE",
            "socket /run/user/1000/hypr/private is missing",
            true,
        );

        let (status, envelope) = envelope(ApiError::from(error)).await;

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(envelope.error.code, "HYPRLAND_UNAVAILABLE");
        assert!(!envelope.error.message.contains("/run/user"));
    }

    #[tokio::test]
    async fn test_a_port_error_outside_the_wire_vocabulary_becomes_internal() {
        let error = PortError::new("COMMAND_TIMEOUT", "hyprctl took too long", true);

        let (status, envelope) = envelope(ApiError::from(error)).await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(envelope.error.code, "INTERNAL_ERROR");
        assert!(!envelope.error.message.contains("hyprctl"));
    }

    #[tokio::test]
    async fn test_an_unlisted_source_is_unauthorized() {
        let (status, envelope) = envelope(AuthorizationError::OutsideTailnet.into()).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(envelope.error.code, "UNAUTHORIZED");
    }
}
