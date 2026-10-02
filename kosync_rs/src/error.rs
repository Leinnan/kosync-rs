//! API error types and HTTP response conversion.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// The numeric protocol error codes understood by the `KOReader` client.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ErrorCode {
    /// Unknown internal server error.
    Internal = 2000,
    /// Missing or invalid credentials.
    Unauthorized = 2001,
    /// A user with the requested username already exists.
    UserExists = 2002,
    /// A required request field was missing or malformed.
    InvalidFields = 2003,
    /// The `document` field was not provided.
    DocumentMissing = 2004,
    /// User registration has been disabled by the operator.
    RegistrationDisabled = 2005,
}

impl ErrorCode {
    /// The HTTP status code associated with this error code.
    pub(crate) const fn status(self) -> StatusCode {
        match self {
            Self::Internal => StatusCode::BAD_GATEWAY,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::UserExists | Self::RegistrationDisabled => StatusCode::PAYMENT_REQUIRED,
            Self::InvalidFields | Self::DocumentMissing => StatusCode::FORBIDDEN,
        }
    }

    /// The human-readable message associated with this error code.
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Internal => "Unknown server error.",
            Self::Unauthorized => "Unauthorized",
            Self::UserExists => "Username is already registered.",
            Self::InvalidFields => "Invalid request",
            Self::DocumentMissing => "Field 'document' not provided.",
            Self::RegistrationDisabled => "User registration is disabled.",
        }
    }
}

/// The JSON body returned to clients for protocol errors.
#[derive(Debug, Serialize)]
struct ErrorBody {
    code: i32,
    message: &'static str,
}

/// A protocol-level error, carrying a numeric code and message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ApiError {
    code: ErrorCode,
}

impl ApiError {
    /// Construct an error from a known protocol [`ErrorCode`].
    pub(crate) const fn new(code: ErrorCode) -> Self {
        Self { code }
    }

    /// The HTTP status code for this error.
    pub(crate) fn status(self) -> StatusCode {
        self.code.status()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            code: self.code as i32,
            message: self.code.message(),
        };
        (self.status(), Json(body)).into_response()
    }
}
