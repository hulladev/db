use super::HostState;
use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, RawQuery, Request, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream;
use hulladb_postgres::managed::{Context, Error, QueryRound, Receipt, RoundQuery, Snapshot};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, sync::Arc};
use tokio::{
    sync::{OwnedSemaphorePermit, watch},
    time::{Instant, Interval, MissedTickBehavior, sleep_until, timeout},
};
use uuid::Uuid;

#[derive(Clone)]
struct Identity<I> {
    headers: HeaderMap,
    context: Context<I>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    name: String,
    #[serde(default)]
    input: Value,
    expected_source: Uuid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Subscription {
    expected_source: Uuid,
    queries: Vec<Member>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    key: String,
    name: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    page: Option<hulladb_postgres::managed::PageMetadata>,
}
impl Subscription {
    fn captured(&self) -> Vec<RoundQuery> {
        self.queries
            .iter()
            .map(|q| RoundQuery {
                key: q.key.clone(),
                operation: q.name.clone(),
                input: q.input.clone(),
                page: q.page.clone(),
            })
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation {
    ticket: Uuid,
    request_id: Uuid,
    name: String,
    #[serde(default)]
    input: Value,
    expected_source: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmation {
    source: Uuid,
    ticket: Uuid,
}

pub(super) fn router<I: Clone + Send + Sync + 'static>(state: Arc<HostState<I>>) -> Router {
    Router::new()
        .route("/functions/session", get(session::<I>))
        .route("/functions/query", post(query::<I>))
        .route("/functions/mutate", post(mutate::<I>))
        .route("/functions/receipts/{request_id}", get(receipt::<I>))
        .route(
            "/functions/receipts/{request_id}/ack",
            post(acknowledge::<I>),
        )
        .route("/functions/subscriptions", post(subscribe::<I>))
        .layer(DefaultBodyLimit::max(state.options.budgets.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate::<I>,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), observe::<I>))
        .with_state(state)
}

#[derive(Clone, Copy)]
struct PublicFailure(&'static str);
async fn observe<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mut record = state.metrics.request();
    let response = next.run(request).await;
    record.complete(
        response.status(),
        response.extensions().get::<PublicFailure>().map(|f| f.0),
    );
    response
}

#[derive(Debug)]
struct Failure {
    status: StatusCode,
    code: &'static str,
    application: Option<hulladb_postgres::managed::ApplicationError>,
}
impl Failure {
    fn terminal(code: &str) -> Self {
        let (status, code) = match code {
            "invalid_request" => (StatusCode::BAD_REQUEST, "invalid_request"),
            "forbidden" => (StatusCode::FORBIDDEN, "forbidden"),
            "not_found" => (StatusCode::NOT_FOUND, "not_found"),
            "conflict" => (StatusCode::CONFLICT, "conflict"),
            "request_id_reused" => (StatusCode::CONFLICT, "request_id_reused"),
            "quota_exceeded" => (StatusCode::UNPROCESSABLE_ENTITY, "quota_exceeded"),
            "execution_limit" => (StatusCode::UNPROCESSABLE_ENTITY, "execution_limit"),
            "row_expectation_failed" => {
                (StatusCode::UNPROCESSABLE_ENTITY, "row_expectation_failed")
            }
            _ => (StatusCode::UNPROCESSABLE_ENTITY, "execution_failed"),
        };
        Self {
            status,
            code,
            application: None,
        }
    }
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "unavailable",
            application: None,
        }
    }
    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            application: None,
        }
    }
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            application: None,
        }
    }
    fn reset() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "reset_required",
            application: None,
        }
    }
    fn capacity() -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "capacity",
            application: None,
        }
    }
    fn unknown() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "outcome_unknown",
            application: None,
        }
    }
}
impl Failure {
    fn payload(&self) -> Value {
        match &self.application {
            Some(error) => json!({"code": self.code, "application": error}),
            None => json!({"code": self.code}),
        }
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(self.payload())).into_response();
        response.extensions_mut().insert(PublicFailure(self.code));
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        response
    }
}
impl From<super::pool::PoolError> for Failure {
    fn from(error: super::pool::PoolError) -> Self {
        match error {
            super::pool::PoolError::Closed | super::pool::PoolError::Budget => Self::unavailable(),
            super::pool::PoolError::Database(error) => error.into(),
        }
    }
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        match error {
            Error::Forbidden => Self {
                status: StatusCode::FORBIDDEN,
                code: "forbidden",
                application: None,
            },
            Error::NotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "not_found",
                application: None,
            },
            Error::Conflict | Error::ReferenceViolation | Error::RetryExhausted => Self {
                status: StatusCode::CONFLICT,
                code: "conflict",
                application: None,
            },
            Error::RequestIdReused => Self {
                status: StatusCode::CONFLICT,
                code: "request_id_reused",
                application: None,
            },
            Error::InvalidTicket => Self {
                status: StatusCode::GONE,
                code: "ticket_expired",
                application: None,
            },
            Error::Application(error) if error.valid() => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "execution_failed",
                application: Some(error),
            },
            Error::TerminalFailure(code) => Self::terminal(&code),
            Error::RowExpectationFailed => Self::terminal("row_expectation_failed"),
            Error::ReceiptExpired => Self {
                status: StatusCode::GONE,
                code: "receipt_expired",
                application: None,
            },
            Error::InvalidInput(_)
            | Error::Validation(_)
            | Error::WrongFunctionKind
            | Error::UnknownFunction(_) => Self::invalid(),
            Error::SourceMismatch => Self::reset(),
            Error::QuotaExceeded => Self::capacity(),
            Error::CommitOutcomeUnknown => Self::unknown(),
            Error::TransactionDeadline => Self {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                code: "execution_limit",
                application: None,
            },
            _ => Self::unavailable(),
        }
    }
}
fn auth_error(error: crate::AuthenticationError) -> Failure {
    match error {
        crate::AuthenticationError::InvalidCredentials => Failure::unauthorized(),
        crate::AuthenticationError::Unavailable => Failure::unavailable(),
    }
}
fn same_context<I>(a: &Context<I>, b: &Context<I>) -> bool {
    a.scope == b.scope
        && a.principal == b.principal
        && a.authorization_scope == b.authorization_scope
}
async fn authenticate<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let origin = request.headers().get(header::ORIGIN).cloned();
    if let Some(origin) = &origin
        && !state
            .options
            .cors_origins
            .iter()
            .any(|allowed| origin.as_bytes() == allowed.as_bytes())
    {
        return Failure::unauthorized().into_response();
    }
    let mut response = if request.method() == Method::OPTIONS && origin.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        let Ok(_permit) = state.requests.clone().try_acquire_owned() else {
            return Failure::capacity().into_response();
        };
        let mutation = request.uri().path() == "/functions/mutate";
        let result = timeout(state.options.budgets.operation_timeout, async {
            if *state.stopped.borrow() {
                return Err(Failure::unavailable());
            }
            let headers = request.headers().clone();
            let context = state
                .auth
                .authenticate(&headers)
                .await
                .map_err(auth_error)?;
            // Native/browser owners pin the authenticated identity before sending
            // a durable intent. Remapped credentials must never execute that intent.
            for (name, expected) in [
                ("x-hulladb-scope", context.scope),
                ("x-hulladb-principal", context.principal),
                ("x-hulladb-authorization-scope", context.authorization_scope),
            ] {
                if request.headers().contains_key(name) {
                    let mut values = request.headers().get_all(name).iter();
                    let value = values.next().and_then(|value| value.to_str().ok());
                    if values.next().is_some() || value != Some(expected.to_string().as_str()) {
                        return Err(Failure::unauthorized());
                    }
                }
            }
            let _quota = state
                .quotas
                .as_ref()
                .map(|gate| {
                    gate.acquire((context.scope, context.principal), false)
                        .ok_or_else(Failure::capacity)
                })
                .transpose()?;
            request
                .extensions_mut()
                .insert(Identity { headers, context });
            Ok::<_, Failure>(next.run(request).await)
        })
        .await;
        match result {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => error.into_response(),
            Err(_) => {
                super::metrics::increment(&state.metrics.operation_deadlines, 1);
                if mutation {
                    Failure::unknown()
                } else {
                    Failure::unavailable()
                }
                .into_response()
            }
        }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if let Some(origin) = origin {
        response
            .headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response
            .headers_mut()
            .insert(header::VARY, "Origin".parse().unwrap());
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            "true".parse().unwrap(),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            "GET, POST, OPTIONS".parse().unwrap(),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            "Authorization, Content-Type, Last-Event-ID, X-Hulladb-Scope, X-Hulladb-Principal, X-Hulladb-Authorization-Scope"
                .parse()
                .unwrap(),
        );
    }
    response
}
pub(super) fn snapshot_json(snapshot: &Snapshot) -> Value {
    let mut value = json!({"source": snapshot.source, "scope": snapshot.revision.scope, "principal": snapshot.principal,
        "authorization_scope": snapshot.authorization_scope, "revision": snapshot.revision.sequence.to_string(), "result": snapshot.result});
    if let Some(page) = &snapshot.page {
        value["page"] = json!(page);
    }
    value
}
pub(super) fn round_json(round: QueryRound) -> Value {
    let results: Vec<_> = round
        .results
        .into_iter()
        .map(|member| match member.result {
            Ok(result) => {
                let mut value = json!({"key": member.key, "result": result});
                if let Some(page) = member.page {
                    value["page"] = json!(page);
                }
                value
            }
            Err(error) => {
                let failure = match error {
                    Error::Database(_) => Failure::terminal("execution_failed"),
                    error => Failure::from(error),
                };
                json!({"key": member.key, "error": failure.payload()})
            }
        })
        .collect();
    json!({"source": round.source, "scope": round.revision.scope, "principal": round.principal,
        "authorization_scope": round.authorization_scope, "revision": round.revision.sequence.to_string(), "results": results})
}
fn receipt_json(receipt: &Receipt) -> Value {
    json!({"request_id": receipt.request_id, "source": receipt.source, "scope": receipt.revision.scope,
        "revision": receipt.revision.sequence.to_string(), "result": receipt.result})
}
fn bounded<I>(value: Value, state: &HostState<I>, mutation: bool) -> Result<Json<Value>, Failure> {
    if serde_json::to_vec(&value)
        .map_err(|_| Failure::unavailable())?
        .len()
        > state.options.budgets.max_body_bytes
    {
        return Err(if mutation {
            Failure::unknown()
        } else {
            Failure::unavailable()
        });
    }
    Ok(Json(value))
}
async fn session<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    Extension(identity): Extension<Identity<I>>,
) -> Result<Json<Value>, Failure> {
    let mut store = state.pool.get().await?;
    let (source, revision) = store.managed_head(&identity.context).await?;
    let ticket = store.issue_ticket(&identity.context).await?;
    bounded(
        json!({"source": source, "scope": revision.scope, "principal": identity.context.principal, "authorization_scope": identity.context.authorization_scope, "revision": revision.sequence.to_string(), "ticket": {"id": ticket.id, "expires_in_ms": ticket.expires_in_ms, "renew_after_ms": ticket.renew_after_ms}}),
        &state,
        false,
    )
}
async fn query<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    Extension(identity): Extension<Identity<I>>,
    body: Result<Json<Operation>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let Json(operation) = body.map_err(|_| Failure::invalid())?;
    let snapshot = state
        .pool
        .get()
        .await?
        .query(
            &state.registry,
            &identity.context,
            &operation.name,
            operation.input,
        )
        .await?;
    if snapshot.source != operation.expected_source {
        return Err(Failure::reset());
    }
    bounded(snapshot_json(&snapshot), &state, false)
}
async fn mutate<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    Extension(identity): Extension<Identity<I>>,
    body: Result<Json<Mutation>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let Json(operation) = body.map_err(|_| Failure::invalid())?;
    let receipt = state
        .pool
        .get()
        .await?
        .mutate_with_ticket(
            &state.registry,
            &identity.context,
            operation.ticket,
            operation.request_id,
            operation.expected_source,
            &operation.name,
            operation.input,
        )
        .await?;
    bounded(receipt_json(&receipt), &state, true)
}
async fn receipt<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    Extension(identity): Extension<Identity<I>>,
    path: Result<Path<Uuid>, PathRejection>,
    RawQuery(raw): RawQuery,
) -> Result<Json<Value>, Failure> {
    let Path(request_id) = path.map_err(|_| Failure::invalid())?;
    let confirmation = confirmation_query(raw.as_deref())?;
    let receipt = state
        .pool
        .get()
        .await?
        .receipt_with_ticket(
            &identity.context,
            confirmation.ticket,
            request_id,
            confirmation.source,
        )
        .await?;
    bounded(
        receipt.as_ref().map(receipt_json).unwrap_or(Value::Null),
        &state,
        false,
    )
}

// UUID-only transport fields: reject duplicates and unknown fields instead of
// silently selecting one of several credentials/request identities.
fn confirmation_query(raw: Option<&str>) -> Result<Confirmation, Failure> {
    let mut source = None;
    let mut ticket = None;
    for pair in raw.ok_or_else(Failure::invalid)?.split('&') {
        let (key, value) = pair.split_once('=').ok_or_else(Failure::invalid)?;
        let slot = match key {
            "source" => &mut source,
            "ticket" => &mut ticket,
            _ => return Err(Failure::invalid()),
        };
        if slot.is_some() {
            return Err(Failure::invalid());
        }
        *slot = Some(value.parse().map_err(|_| Failure::invalid())?);
    }
    Ok(Confirmation {
        source: source.ok_or_else(Failure::invalid)?,
        ticket: ticket.ok_or_else(Failure::invalid)?,
    })
}

async fn acknowledge<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    Extension(identity): Extension<Identity<I>>,
    path: Result<Path<Uuid>, PathRejection>,
    body: Result<Json<Confirmation>, JsonRejection>,
) -> Result<Json<Value>, Failure> {
    let Path(request_id) = path.map_err(|_| Failure::invalid())?;
    let Json(confirmation) = body.map_err(|_| Failure::invalid())?;
    state
        .pool
        .get()
        .await?
        .acknowledge_ticket(
            &identity.context,
            confirmation.ticket,
            request_id,
            confirmation.source,
        )
        .await?;
    Ok(Json(Value::Null))
}

struct Stream<I> {
    state: Arc<HostState<I>>,
    identity: Identity<I>,
    operation: Subscription,
    subscription: Uuid,
    sequence: i64,
    initial: Option<Arc<Value>>,
    previous: Option<Arc<Value>>,
    group: Arc<super::subscriptions::Group<I>>,
    next_poll: Instant,
    credentials: Interval,
    stopped: watch::Receiver<bool>,
    terminal: bool,
    _permit: OwnedSemaphorePermit,
    _quota: Option<super::quotas::Permit>,
}
async fn subscribe<I: Clone + Send + Sync + 'static>(
    State(state): State<Arc<HostState<I>>>,
    Extension(identity): Extension<Identity<I>>,
    body: Result<Json<Subscription>, JsonRejection>,
) -> Result<Response, Failure> {
    let Json(operation) = body.map_err(|_| Failure::invalid())?;
    let mut keys = std::collections::HashSet::new();
    if operation.queries.is_empty()
        || operation.queries.len() > 128
        || operation.queries.iter().any(|q| {
            q.key.is_empty()
                || !keys.insert(&q.key)
                || q.page
                    .as_ref()
                    .is_some_and(|page| page.range.is_empty() || page.range.len() > 65_536)
        })
    {
        return Err(Failure::invalid());
    }
    let permit = state
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::capacity())?;
    let quota = state
        .quotas
        .as_ref()
        .map(|gate| {
            gate.acquire((identity.context.scope, identity.context.principal), true)
                .ok_or_else(Failure::capacity)
        })
        .transpose()?;
    let group = state.subscriptions.acquire(
        &identity.context,
        operation.expected_source,
        &operation.captured(),
    );
    let (initial, next_poll) = group
        .refresh(
            &state,
            &identity.context,
            operation.expected_source,
            &operation.captured(),
            true,
        )
        .await?;
    if initial["source"] != operation.expected_source.to_string() {
        return Err(Failure::reset());
    }
    let current = state
        .auth
        .authenticate(&identity.headers)
        .await
        .map_err(auth_error)?;
    if !same_context(&current, &identity.context) {
        return Err(Failure::unauthorized());
    }
    let cadence = state
        .options
        .budgets
        .poll_interval
        .min(state.options.budgets.credential_check_interval);
    let mut credentials = tokio::time::interval_at(
        Instant::now() + state.options.budgets.credential_check_interval,
        state.options.budgets.credential_check_interval,
    );
    credentials.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let stream = Stream {
        stopped: state.stopped.subscribe(),
        state,
        identity,
        operation,
        subscription: Uuid::now_v7(),
        sequence: 0,
        initial: Some(initial),
        previous: None,
        group,
        next_poll,
        credentials,
        terminal: false,
        _permit: permit,
        _quota: quota,
    };
    let events = stream::unfold(stream, |mut stream| async move {
        loop {
            if stream.terminal || *stream.stopped.borrow() {
                return None;
            }
            let initial = stream.initial.take();
            let refresh = if initial.is_some() {
                Instant::now() >= stream.next_poll
            } else {
                tokio::select! {
                    _ = stream.stopped.changed() => return None,
                    _ = sleep_until(stream.next_poll) => true,
                    _ = stream.credentials.tick() => false,
                }
            };
            let state = &stream.state;
            let result = match timeout(state.options.budgets.operation_timeout, async {
                let current = state
                    .auth
                    .authenticate(&stream.identity.headers)
                    .await
                    .map_err(auth_error)?;
                if !same_context(&current, &stream.identity.context) {
                    return Err(Failure::unauthorized());
                }
                let snapshot = if let Some(initial) = initial.filter(|_| !refresh) {
                    Some(initial)
                } else if refresh {
                    let (snapshot, next_poll) = stream
                        .group
                        .refresh(
                            state,
                            &current,
                            stream.operation.expected_source,
                            &stream.operation.captured(),
                            false,
                        )
                        .await?;
                    stream.next_poll = next_poll;
                    Some(snapshot)
                } else {
                    None
                };
                // Every connection checks its own credential at delivery, even
                // when another connection computed the shared query result.
                if snapshot.is_some() {
                    let current = state
                        .auth
                        .authenticate(&stream.identity.headers)
                        .await
                        .map_err(auth_error)?;
                    if !same_context(&current, &stream.identity.context) {
                        return Err(Failure::unauthorized());
                    }
                }
                Ok::<_, Failure>(snapshot)
            })
            .await
            {
                Ok(Ok(Some(snapshot))) => Ok(snapshot),
                Ok(Ok(None)) => continue,
                Ok(Err(error)) => Err(error),
                Err(_) => {
                    super::metrics::increment(&state.metrics.operation_deadlines, 1);
                    Err(Failure::unavailable())
                }
            };
            if *stream.stopped.borrow() {
                return None;
            }
            let event = match result {
                Ok(snapshot)
                    if snapshot["source"] == stream.operation.expected_source.to_string() =>
                {
                    if stream.previous.as_ref().is_some_and(|previous| {
                        Arc::ptr_eq(previous, &snapshot) || previous.as_ref() == snapshot.as_ref()
                    }) {
                        continue;
                    }
                    let value = snapshot.as_ref();
                    stream.sequence = match stream.sequence.checked_add(1) {
                        Some(value) => value,
                        None => {
                            stream.terminal = true;
                            super::metrics::increment(&state.metrics.stream_failures, 1);
                            return Some((
                                Ok::<_, Infallible>(
                                    Event::default()
                                        .event("error")
                                        .data("{\"code\":\"reset_required\"}"),
                                ),
                                stream,
                            ));
                        }
                    };
                    let frame = json!({"subscription": stream.subscription, "sequence": stream.sequence.to_string(), "round": value});
                    let encoded = frame.to_string();
                    let event_id = format!("{}:{}", stream.subscription, stream.sequence);
                    let frame_bytes =
                        encoded.len() + event_id.len() + "event: round\nid: \ndata: \n\n".len();
                    if frame_bytes > stream.state.options.budgets.max_body_bytes {
                        stream.terminal = true;
                        Event::default()
                            .event("error")
                            .data("{\"code\":\"unavailable\"}")
                    } else {
                        super::metrics::increment(&stream.state.metrics.snapshots_delivered, 1);
                        super::metrics::increment(
                            &stream.state.metrics.snapshot_bytes,
                            encoded.len() as u64,
                        );
                        stream.previous = Some(snapshot);
                        Event::default().event("round").id(event_id).data(encoded)
                    }
                }
                Ok(_) => {
                    stream.terminal = true;
                    Event::default()
                        .event("error")
                        .data("{\"code\":\"reset_required\"}")
                }
                Err(error) => {
                    stream.terminal = true;
                    Event::default()
                        .event("error")
                        .data(json!({"code": error.code}).to_string())
                }
            };
            if stream.terminal {
                super::metrics::increment(&stream.state.metrics.stream_failures, 1);
            }
            return Some((Ok::<_, Infallible>(event), stream));
        }
    });
    // Reconnects receive a new subscription and an authoritative replacement;
    // Last-Event-ID never asserts retained history or local coverage.
    Ok(Sse::new(events)
        .keep_alive(KeepAlive::default().interval(cadence))
        .into_response())
}

#[cfg(test)]
#[path = "../../tests/unit/managed_errors.rs"]
mod error_tests;
