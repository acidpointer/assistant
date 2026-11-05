//! HTTP request and error contracts. Only literal text is accepted; core/native
//! error strings never enter responses. These DTOs can evolve separately from
//! application execution types and future transport encodings.

use assistant::application::{ApplicationError, Submission};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SubmitRequest {
    pub(crate) client_id: String,
    pub(crate) client_request_id: String,
    pub(crate) input: Input,
}
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Input {
    Text { text: String },
}
impl From<SubmitRequest> for Submission {
    fn from(request: SubmitRequest) -> Self {
        let Input::Text { text } = request.input;
        Self {
            client_id: request.client_id,
            request_id: request.client_request_id,
            text,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Problem {
    pub(crate) status: StatusCode,
    pub(crate) code: &'static str,
}
impl Problem {
    pub(crate) fn new(status: StatusCode, code: &'static str) -> Self {
        Self { status, code }
    }
}
impl From<ApplicationError> for Problem {
    fn from(error: ApplicationError) -> Self {
        let (status, code) = match error {
            ApplicationError::InvalidInput => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input"),
            ApplicationError::Busy => (StatusCode::CONFLICT, "chat_busy"),
            ApplicationError::Conflict => (StatusCode::CONFLICT, "request_conflict"),
            ApplicationError::Capacity => (StatusCode::TOO_MANY_REQUESTS, "capacity_exceeded"),
            ApplicationError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApplicationError::Closed => (StatusCode::SERVICE_UNAVAILABLE, "closed"),
            ApplicationError::StorageUnavailable => {
                (StatusCode::SERVICE_UNAVAILABLE, "memory_unavailable")
            }
            ApplicationError::Gone => (StatusCode::GONE, "forgotten"),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        };
        Self::new(status, code)
    }
}
impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(json!({"type":format!("urn:assistant:problem:{}", self.code),
            "title":self.status.canonical_reason().unwrap_or("Request failed"), "status":self.status.as_u16(), "code":self.code}))).into_response();
        response.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}
