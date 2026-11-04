//! Same-origin HTTP adapter. Authority is checked against the real TCP target,
//! not forwarded headers. Mutations require JSON and a non-simple client header.
//! SSE owns a bounded subscription and connection permit; disconnect never owns
//! cancellation of inference. All initial snapshots are atomic with registration.

use crate::{
    api::{Problem, SubmitRequest},
    service::LocalAddress,
};
use assistant::{
    application::{
        AssistantHandle, INPUT_BYTES, MESSAGE_LIMIT, REQUEST_RETENTION, TRANSCRIPT_BYTES,
    },
    engine::Cancellation,
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, FromRequest, Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream;
use serde_json::json;
use std::{convert::Infallible, net::IpAddr, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

mod assets {
    //! Frozen, verified frontend bytes; no filesystem reads occur while serving.
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}
#[derive(Debug, Clone)]
struct WebState {
    assistant: AssistantHandle,
    quiesce: Cancellation,
    stop: Cancellation,
    streams: Arc<Semaphore>,
}
pub(crate) fn router(
    assistant: AssistantHandle,
    quiesce: Cancellation,
    stop: Cancellation,
) -> Router {
    let state = WebState {
        assistant,
        quiesce,
        stop,
        streams: Arc::new(Semaphore::new(4)),
    };
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(static_asset))
        .route("/api/v1/bootstrap", get(bootstrap))
        .route("/api/v1/chats/{id}", get(snapshot))
        .route("/api/v1/chats/{id}/events", get(events))
        .route("/api/v1/chats/{id}/runs", post(submit))
        .route("/api/v1/runs/{id}", get(run))
        .route("/api/v1/runs/{id}/cancel", post(cancel))
        .fallback(|| async { Problem::new(StatusCode::NOT_FOUND, "not_found") })
        .method_not_allowed_fallback(|| async {
            Problem::new(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
        })
        .layer(DefaultBodyLimit::max(128 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), policy))
        .with_state(state)
}

fn valid_authority(authority: &str, local: std::net::SocketAddr) -> bool {
    if authority.contains('@') {
        return false;
    }
    let Ok(authority) = authority.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    if authority.port_u16().unwrap_or(80) != local.port() {
        return false;
    }
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return local.ip().is_loopback();
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip == local.ip())
}
async fn policy(State(state): State<WebState>, request: Request, next: Next) -> Response {
    let failure = |code| Problem::new(StatusCode::FORBIDDEN, code).into_response();
    let local = request
        .extensions()
        .get::<ConnectInfo<LocalAddress>>()
        .and_then(|info| info.0.0);
    let host = request.headers().get("host").and_then(|v| v.to_str().ok());
    let allowed = local
        .zip(host)
        .is_some_and(|(address, host)| valid_authority(host, address));
    let mut response = if !allowed {
        failure("invalid_authority")
    } else if state.quiesce.is_cancelled() {
        Problem::new(StatusCode::SERVICE_UNAVAILABLE, "closed").into_response()
    } else {
        let mutation = request.method() != Method::GET && request.method() != Method::HEAD;
        let origin_ok = request.headers().get("origin").is_none_or(|value| {
            value
                .to_str()
                .ok()
                .and_then(|v| v.parse::<Uri>().ok())
                .is_some_and(|uri| {
                    uri.scheme_str() == Some("http")
                        && uri.authority().is_some_and(|a| Some(a.as_str()) == host)
                        && uri.path_and_query().is_none_or(|path| path.as_str() == "/")
                })
        });
        let header_ok = request
            .headers()
            .get("x-assistant-client")
            .is_some_and(|v| v == "webui");
        let json_ok = request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';')
                    .next()
                    .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
            });
        if !origin_ok {
            failure("invalid_origin")
        } else if mutation && !header_ok {
            failure("missing_client_header")
        } else if mutation && !json_ok {
            Problem::new(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required").into_response()
        } else {
            match tokio::time::timeout(Duration::from_secs(10), next.run(request)).await {
                Ok(response) => response,
                Err(_) => {
                    Problem::new(StatusCode::REQUEST_TIMEOUT, "request_timeout").into_response()
                }
            }
        }
    };
    if !response.headers().contains_key("cache-control") {
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-store"));
    }
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
        .headers_mut()
        .insert("x-frame-options", HeaderValue::from_static("DENY"));
    response
}

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; worker-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";
#[derive(Debug, Clone, Copy)]
enum AssetCaching {
    Revalidate,
    Immutable,
}
async fn index(headers: HeaderMap) -> Result<Response, Problem> {
    asset_response("index.html", &headers, AssetCaching::Revalidate)
}
async fn static_asset(Path(path): Path<String>, headers: HeaderMap) -> Result<Response, Problem> {
    asset_response(&format!("assets/{path}"), &headers, AssetCaching::Immutable)
}
fn asset_response(
    path: &str,
    headers: &HeaderMap,
    caching: AssetCaching,
) -> Result<Response, Problem> {
    let (bytes, compressed, mime) =
        assets::asset(path).ok_or_else(|| Problem::new(StatusCode::NOT_FOUND, "not_found"))?;
    let gzip = headers
        .get("accept-encoding")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|encoding| {
                let mut parts = encoding.trim().split(';');
                parts.next() == Some("gzip")
                    && parts.all(|parameter| {
                        parameter
                            .trim()
                            .strip_prefix("q=")
                            .is_none_or(|q| q.parse::<f32>().is_ok_and(|q| q > 0.0))
                    })
            })
        });
    let mut response = (
        [
            ("content-type", mime),
            ("vary", "Accept-Encoding"),
            (
                "cache-control",
                if matches!(caching, AssetCaching::Immutable) {
                    "public, max-age=31536000, immutable"
                } else {
                    "no-cache"
                },
            ),
            ("content-security-policy", CSP),
        ],
        if gzip { compressed } else { bytes },
    )
        .into_response();
    if gzip {
        response
            .headers_mut()
            .insert("content-encoding", HeaderValue::from_static("gzip"));
    }
    Ok(response)
}
async fn bootstrap(State(state): State<WebState>) -> Result<Json<serde_json::Value>, Problem> {
    let snapshot = state.assistant.snapshot().await?;
    Ok(Json(
        json!({"api_version":1, "instance_id":snapshot.instance_id, "primary_chat_id":snapshot.chat_id,
        "capabilities":{"history":false,"text":true,"cancellation":true,"metrics":true},
        "limits":{"input_bytes":INPUT_BYTES,"messages":MESSAGE_LIMIT,"transcript_bytes":TRANSCRIPT_BYTES,"deduplication_seconds":REQUEST_RETENTION.as_secs()}}),
    ))
}
async fn check_chat(state: &WebState, id: &str) -> Result<(), Problem> {
    if state.assistant.chat_id().await? != id {
        return Err(Problem::new(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok(())
}
async fn snapshot(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<Json<assistant::application::Snapshot>, Problem> {
    let snapshot = state.assistant.snapshot().await?;
    if snapshot.chat_id != id {
        return Err(Problem::new(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok(Json(snapshot))
}
async fn submit(
    State(state): State<WebState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response, Problem> {
    check_chat(&state, &id).await?;
    let Json(input) = Json::<SubmitRequest>::from_request(request, &state)
        .await
        .map_err(|error| {
            Problem::new(
                error.status(),
                if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    "body_too_large"
                } else {
                    "invalid_input"
                },
            )
        })?;
    let run = state.assistant.submit("web", input.into()).await?;
    let location = HeaderValue::from_str(&format!("/api/v1/runs/{}", run.id))
        .map_err(|_| Problem::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid_location"))?;
    Ok((StatusCode::ACCEPTED, [("location", location)], Json(run)).into_response())
}
async fn run(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, Problem> {
    Ok(Json(state.assistant.run(&id).await?))
}
async fn cancel(
    State(state): State<WebState>,
    Path(id): Path<String>,
    request: Request,
) -> Result<impl IntoResponse, Problem> {
    let Json(_): Json<serde_json::Map<String, serde_json::Value>> =
        Json::from_request(request, &state)
            .await
            .map_err(|error| Problem::new(error.status(), "invalid_input"))?;
    let run = state.assistant.cancel(&id).await?;
    Ok((
        if run.status.is_terminal() {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        },
        Json(run),
    ))
}

struct EventStream {
    initial: Option<assistant::application::Snapshot>,
    events: tokio::sync::broadcast::Receiver<Arc<assistant::application::ApplicationEvent>>,
    stop: Cancellation,
    _permit: OwnedSemaphorePermit,
}
async fn events(
    State(state): State<WebState>,
    Path(id): Path<String>,
) -> Result<Response, Problem> {
    let permit = state
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| Problem::new(StatusCode::TOO_MANY_REQUESTS, "stream_capacity"))?;
    let subscription = state.assistant.subscribe().await?;
    if subscription.snapshot.chat_id != id {
        return Err(Problem::new(StatusCode::NOT_FOUND, "not_found"));
    }
    let stream = stream::unfold(
        EventStream {
            initial: Some(subscription.snapshot),
            events: subscription.events,
            stop: state.stop,
            _permit: permit,
        },
        |mut state| async move {
            let event = if let Some(snapshot) = state.initial.take() {
                Event::default()
                    .event("snapshot")
                    .id(&snapshot.revision)
                    .json_data(&snapshot)
            } else {
                // Drain ready final updates before honoring shutdown; this is
                // bounded by the ring capacity and never waits on new inference.
                let received = tokio::select! { biased; value = state.events.recv() => value, () = state.stop.cancelled() => return None };
                // Lag closes the stream; EventSource reconnects to an atomic snapshot.
                let Ok(event) = received else { return None };
                Event::default()
                    .event("update")
                    .id(&event.id)
                    .json_data(&event.update)
            };
            match event {
                Ok(event) => Some((Ok::<_, Infallible>(event), state)),
                Err(_) => None,
            }
        },
    );
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

#[cfg(test)]
mod tests;
