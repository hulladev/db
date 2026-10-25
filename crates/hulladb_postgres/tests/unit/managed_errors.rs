use super::{Error, Failure, IntoResponse, StatusCode};

#[tokio::test]
async fn mutation_conflict_deadline_and_unknown_commit_have_distinct_public_failures() {
    for (error, status, code) in [
        (Error::RetryExhausted, StatusCode::CONFLICT, "conflict"),
        (
            Error::TransactionDeadline,
            StatusCode::UNPROCESSABLE_ENTITY,
            "execution_limit",
        ),
        (
            Error::CommitOutcomeUnknown,
            StatusCode::SERVICE_UNAVAILABLE,
            "outcome_unknown",
        ),
    ] {
        let response = Failure::from(error).into_response();
        assert_eq!(response.status(), status);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body, serde_json::json!({"code": code}));
    }
}
