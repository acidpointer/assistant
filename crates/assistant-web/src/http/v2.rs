//! Local durable HTTP mapping. The core owns admission, evidence and
//! history; handlers only validate bounded wire input and map typed results.
//! Streams share the local owner handle and close during service shutdown.

use super::*;
use crate::api::OwnerJson;
use assistant::application::OwnerHandle;
use assistant::memory::{MemoryId, Revision};
use axum::{Extension, extract::Query};
use serde::Deserialize;
use tokio::sync::broadcast;

pub(super) fn routes() -> Router<WebState> {
    Router::new()
        .route("/api/v2/snapshot", get(snapshot))
        .route("/api/v2/history", get(history))
        .route("/api/v2/events", get(events))
        .route("/api/v2/runs", post(submit))
        .route("/api/v2/operations/lookup", post(lookup))
        .route("/api/v2/operations/{id}", get(operation))
        .route("/api/v2/runs/{id}/cancel", post(cancel))
        .route("/api/v2/memory/evidence/{id}", get(evidence))
        .route("/api/v2/memory/search", post(search))
        .route("/api/v2/memory/health", get(health))
        .route("/api/v2/memory/remember", post(preserve))
        .route("/api/v2/memory/notes", post(remember))
        .route("/api/v2/memory/follow", post(follow))
        .route("/api/v2/memory/connections", post(connections))
        .route("/api/v2/assistant", get(profile).put(update_profile))
        .route("/api/v2/assistant/notices", get(notices))
        .route(
            "/api/v2/assistant/notices/{id}/dismiss",
            post(dismiss_notice),
        )
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Note {
    request_id: String,
    text: assistant::memory::MemoryText,
}
async fn remember(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<Note>,
) -> Result<Json<assistant::memory::MemoryReceipt<assistant::memory::Source>>, Problem> {
    Ok(Json(owner.remember(&input.request_id, input.text).await?))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connections {
    source_id: MemoryId,
    cursor: Option<String>,
}
async fn connections(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<Connections>,
) -> Result<Json<assistant::memory::SourceGraph>, Problem> {
    Ok(Json(
        owner.connections(input.source_id, input.cursor).await?,
    ))
}
async fn profile(
    Extension(owner): Extension<OwnerHandle>,
) -> Result<Json<assistant::memory::AssistantProfile>, Problem> {
    Ok(Json(owner.assistant_profile().await?))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileEdit {
    revision: Revision,
    settings: assistant::memory::AssistantSettings,
}
async fn update_profile(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<ProfileEdit>,
) -> Result<Json<assistant::memory::AssistantProfile>, Problem> {
    Ok(Json(
        owner
            .update_assistant(input.revision, input.settings)
            .await?,
    ))
}
async fn notices(
    Extension(owner): Extension<OwnerHandle>,
) -> Result<Json<Vec<assistant::memory::AssistantNotice>>, Problem> {
    Ok(Json(owner.notices().await?))
}
async fn dismiss_notice(
    Extension(owner): Extension<OwnerHandle>,
    Path(id): Path<MemoryId>,
    OwnerJson(_): OwnerJson<serde_json::Map<String, serde_json::Value>>,
) -> Result<StatusCode, Problem> {
    owner.dismiss_notice(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Follow {
    start: Option<MemoryId>,
    depth: Option<usize>,
}
async fn follow(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<Follow>,
) -> Result<Json<assistant::memory::GraphBundle>, Problem> {
    Ok(Json(
        owner.follow(input.start, input.depth.unwrap_or(2)).await?,
    ))
}
async fn snapshot(
    Extension(owner): Extension<OwnerHandle>,
) -> Result<Json<assistant::application::Snapshot>, Problem> {
    Ok(Json(owner.snapshot().await?))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Page {
    before: Option<Revision>,
    limit: Option<usize>,
}
async fn history(
    Extension(owner): Extension<OwnerHandle>,
    page: Result<Query<Page>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<assistant::memory::HistoryPage>, Problem> {
    let Query(page) = page.map_err(|error| Problem::new(error.status(), "invalid_input"))?;
    Ok(Json(
        owner.history(page.before, page.limit.unwrap_or(16)).await?,
    ))
}
async fn submit(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<SubmitRequest>,
) -> Result<(StatusCode, Json<assistant::application::RunView>), Problem> {
    Ok((
        StatusCode::ACCEPTED,
        Json(owner.submit(input.into()).await?),
    ))
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    client_id: String,
    client_request_id: String,
}
async fn lookup(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(key): OwnerJson<Key>,
) -> Result<Json<assistant::application::RunView>, Problem> {
    Ok(Json(
        owner.lookup(&key.client_id, &key.client_request_id).await?,
    ))
}
async fn operation(
    Extension(owner): Extension<OwnerHandle>,
    Path(id): Path<String>,
) -> Result<Json<assistant::application::RunView>, Problem> {
    Ok(Json(owner.operation(&id).await?))
}
async fn cancel(
    Extension(owner): Extension<OwnerHandle>,
    Path(id): Path<String>,
    OwnerJson(_): OwnerJson<serde_json::Map<String, serde_json::Value>>,
) -> Result<Json<assistant::application::RunView>, Problem> {
    Ok(Json(owner.cancel(&id).await?))
}
async fn evidence(
    Extension(owner): Extension<OwnerHandle>,
    id: Result<Path<MemoryId>, axum::extract::rejection::PathRejection>,
) -> Result<Json<assistant::memory::SourceRecord>, Problem> {
    let Path(id) = id.map_err(|error| Problem::new(error.status(), "invalid_input"))?;
    Ok(Json(owner.evidence(id).await?))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    query: assistant::memory::MemoryText,
    limit: Option<usize>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preserve {
    request_id: String,
    source: assistant::memory::SourceRef,
}
async fn preserve(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<Preserve>,
) -> Result<Json<assistant::memory::MemoryReceipt<assistant::memory::Source>>, Problem> {
    Ok(Json(owner.preserve(&input.request_id, input.source).await?))
}
async fn search(
    Extension(owner): Extension<OwnerHandle>,
    OwnerJson(input): OwnerJson<Search>,
) -> Result<Json<assistant::memory::service::RecallBundle>, Problem> {
    Ok(Json(
        owner.search(input.query, input.limit.unwrap_or(8)).await?,
    ))
}
async fn health(
    Extension(owner): Extension<OwnerHandle>,
) -> Result<Json<assistant::memory::service::MemoryHealth>, Problem> {
    Ok(Json(owner.memory_health().await?))
}

struct Stream {
    initial: Option<assistant::application::Snapshot>,
    events: broadcast::Receiver<Arc<assistant::application::ApplicationEvent>>,
    assistant_changes: tokio::sync::watch::Receiver<u64>,
    initial_notices: bool,
    owner: OwnerHandle,
    stop: Cancellation,
    _permit: OwnedSemaphorePermit,
}
async fn events(
    State(state): State<WebState>,
    Extension(owner): Extension<OwnerHandle>,
) -> Result<Response, Problem> {
    let permit = state
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| Problem::new(StatusCode::TOO_MANY_REQUESTS, "stream_capacity"))?;
    let subscription = owner.subscribe().await?;
    let stream = stream::unfold(
        Stream {
            initial: Some(subscription.snapshot),
            events: subscription.events,
            assistant_changes: owner.assistant_changes()?,
            initial_notices: true,
            owner,
            stop: state.stop,
            _permit: permit,
        },
        |mut state| async move {
            loop {
                let event = if let Some(snapshot) = state.initial.take() {
                    Event::default()
                        .event("snapshot")
                        .id(&snapshot.revision)
                        .json_data(&snapshot)
                } else if state.initial_notices {
                    state.initial_notices = false;
                    let notices = state.owner.notices().await.ok()?;
                    Event::default().event("notices").json_data(&notices)
                } else {
                    let received = tokio::select! { biased;
                        ()=state.owner.revoked()=>return None,
                        ()=state.stop.cancelled()=>return None,
                        changed=state.assistant_changes.changed()=>{
                            if changed.is_err() {return None;}
                            state.initial_notices=true;continue;
                        },
                        event=state.events.recv()=>event,
                    };
                    let Ok(event) = received else { return None };
                    Event::default()
                        .event("update")
                        .id(&event.id)
                        .json_data(&event.update)
                };
                if state.owner.check().is_err() {
                    return None;
                }
                let Ok(event) = event else { return None };
                return Some((Ok::<_, Infallible>(event), state));
            }
        },
    );
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    Ok(response)
}
