//! Browser boundary tests independent of a model. Exercise the same authority
//! parser and middleware used by the live server, including legacy clients that
//! omit Origin. Static asset tests verify safe single-file CSP templating.
use super::*;
use axum::body::Body;
use tower::ServiceExt;

#[test]
fn authority_uses_actual_destination_and_exact_port() {
    let local = "127.0.0.1:8080".parse().expect("address");
    assert!(valid_authority("127.0.0.1:8080", local));
    assert!(valid_authority("localhost:8080", local));
    for host in [
        "evil.example:8080",
        "localhost:8081",
        "127.0.0.2:8080",
        "127.0.0.1",
        "user@localhost:8080",
    ] {
        assert!(!valid_authority(host, local), "{host}");
    }
    assert!(valid_authority(
        "[::1]:8080",
        "[::1]:8080".parse().expect("IPv6")
    ));
    assert!(!valid_authority(
        "localhost:8080",
        "192.0.2.1:8080".parse().expect("LAN")
    ));
}

#[tokio::test]
async fn embedded_asset_uses_unique_nonces_and_has_no_runtime_dependencies() {
    let first = index().await.expect("first page");
    let second = index().await.expect("second page");
    let first_policy = first
        .headers()
        .get("content-security-policy")
        .expect("CSP")
        .clone();
    assert_ne!(
        first_policy,
        second
            .headers()
            .get("content-security-policy")
            .expect("CSP")
    );
    let body = axum::body::to_bytes(first.into_body(), 128 * 1024)
        .await
        .expect("HTML");
    let html = String::from_utf8(body.to_vec()).expect("UTF-8 HTML");
    assert!(!html.contains("__CSP_NONCE__"));
    assert!(!html.contains(" src="));
    assert!(!html.contains("innerHTML"));
    assert!(html.contains("<style nonce="));
    assert!(html.contains("<script nonce="));
}

#[tokio::test]
async fn malformed_json_and_body_limits_have_structured_errors() {
    // Extraction must remain bounded even if a caller omits Content-Length.
    let app = Router::new()
        .route(
            "/",
            post(|request: Request| async {
                let result = Json::<SubmitRequest>::from_request(request, &()).await;
                match result {
                    Ok(_) => StatusCode::NO_CONTENT.into_response(),
                    Err(error) => Problem::new(error.status(), "invalid_input").into_response(),
                }
            }),
        )
        .layer(DefaultBodyLimit::max(128));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/")
                .header("content-type", "application/json")
                .body(Body::from("x".repeat(256)))
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response.headers()["content-type"],
        "application/problem+json"
    );
}
