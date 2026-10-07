use std::{
    convert::Infallible,
    sync::{Arc, atomic::Ordering},
};

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, Path, Request, State,
        rejection::{JsonRejection, PathRejection},
    },
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use hulladb_postgres::{Error as DatabaseError, Receipt};
use hulladb_protocol::{ApiError, ApiErrorCode, CommandRequest, Session, TaskQuery, TaskResult};
use hulladb_sync::{TaskFeedError, TaskSubscription};
use tokio::{
    sync::watch,
    time::{Interval, MissedTickBehavior, timeout},
};
use uuid::Uuid;

use crate::{AuthenticationError, Coordinator, HostState};

#[derive(Clone)]
struct Identity {
    token: String,
    session: Session,
}

pub(crate) fn router(state: Arc<HostState>) -> Router {
    Router::new()
        .route("/tasks/commands", post(command))
        .route("/tasks/receipts/{request_id}", get(receipt))
        .route("/tasks/subscriptions", post(subscribe))
        .layer(DefaultBodyLimit::max(state.options.max_body_bytes))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}

#[derive(Debug)]
struct Failure {
    status: StatusCode,
    code: ApiErrorCode,
}
impl Failure {
    fn new(status: StatusCode, code: ApiErrorCode) -> Self {
        Self { status, code }
    }
    fn unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, ApiErrorCode::Unavailable)
    }
    fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, ApiErrorCode::Unauthorized)
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let mut response = (self.status, Json(ApiError { code: self.code })).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        }
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        response
    }
}
impl From<crate::pool::PoolError> for Failure {
    fn from(error: crate::pool::PoolError) -> Self {
        match error {
            crate::pool::PoolError::Closed => Self::unavailable(),
            crate::pool::PoolError::Database(error) => error.into(),
        }
    }
}
impl From<DatabaseError> for Failure {
    fn from(error: DatabaseError) -> Self {
        match error {
            DatabaseError::Forbidden => Self::new(StatusCode::FORBIDDEN, ApiErrorCode::Forbidden),
            DatabaseError::NotFound => Self::new(StatusCode::NOT_FOUND, ApiErrorCode::NotFound),
            DatabaseError::Conflict => Self::new(StatusCode::CONFLICT, ApiErrorCode::Conflict),
            DatabaseError::RequestIdReused => {
                Self::new(StatusCode::CONFLICT, ApiErrorCode::RequestIdReused)
            }
            DatabaseError::InvalidInput(_)
            | DatabaseError::InvalidQueryRevision
            | DatabaseError::QueryNotCovered => {
                Self::new(StatusCode::BAD_REQUEST, ApiErrorCode::InvalidRequest)
            }
            DatabaseError::CommitOutcomeUnknown => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ApiErrorCode::OutcomeUnknown,
            ),
            DatabaseError::Database(_) | DatabaseError::CorruptData => Self::unavailable(),
        }
    }
}
impl From<TaskFeedError> for Failure {
    fn from(error: TaskFeedError) -> Self {
        match error {
            TaskFeedError::Authorization(e) | TaskFeedError::Query(e) => e.into(),
            TaskFeedError::Limit => {
                Self::new(StatusCode::TOO_MANY_REQUESTS, ApiErrorCode::Capacity)
            }
            TaskFeedError::SourceMismatch => {
                Self::new(StatusCode::CONFLICT, ApiErrorCode::ResetRequired)
            }
            TaskFeedError::Closed => Self::unavailable(),
        }
    }
}
impl From<AuthenticationError> for Failure {
    fn from(error: AuthenticationError) -> Self {
        match error {
            AuthenticationError::InvalidCredentials => Self::unauthorized(),
            AuthenticationError::Unavailable => Self::unavailable(),
        }
    }
}
fn json_error(error: JsonRejection) -> Failure {
    Failure::new(error.status(), ApiErrorCode::InvalidRequest)
}
fn bearer(headers: &HeaderMap) -> Result<String, Failure> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next().ok_or_else(Failure::unauthorized)?;
    if values.next().is_some() {
        return Err(Failure::unauthorized());
    }
    let (scheme, token) = value
        .to_str()
        .ok()
        .and_then(|v| v.split_once(' '))
        .ok_or_else(Failure::unauthorized)?;
    if !scheme.eq_ignore_ascii_case("Bearer")
        || token.is_empty()
        || token.bytes().any(|b| b.is_ascii_whitespace())
    {
        return Err(Failure::unauthorized());
    }
    Ok(token.into())
}

async fn authenticate(
    State(state): State<Arc<HostState>>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let Ok(_admission) = state.requests.clone().try_acquire_owned() else {
        return Failure::new(StatusCode::TOO_MANY_REQUESTS, ApiErrorCode::Capacity).into_response();
    };
    let mutation = request.uri().path() == "/tasks/commands";
    let result = timeout(state.options.operation_timeout, async {
        if !state.available() {
            return Err(Failure::unavailable());
        }
        let token = bearer(request.headers())?;
        let session = state.auth.authenticate(&token).await?;
        if !state.available() {
            return Err(Failure::unavailable());
        }
        request.extensions_mut().insert(Identity { token, session });
        Ok::<_, Failure>(next.run(request).await)
    })
    .await;
    let mut response = match result {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => error.into_response(),
        Err(_) => Failure::new(
            StatusCode::SERVICE_UNAVAILABLE,
            if mutation {
                ApiErrorCode::OutcomeUnknown
            } else {
                ApiErrorCode::Unavailable
            },
        )
        .into_response(),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

async fn command(
    State(state): State<Arc<HostState>>,
    Extension(identity): Extension<Identity>,
    body: Result<Json<CommandRequest>, JsonRejection>,
) -> Result<Json<Receipt>, Failure> {
    let Json(request) = body.map_err(json_error)?;
    let mut store = state.pool.get().await?;
    let result = store
        .execute(identity.session, request.request_id, request.command)
        .await?;
    // Acknowledgement never awaits polling or subscriber fan-out.
    state.wake.notify_one();
    Ok(Json(result))
}
async fn receipt(
    State(state): State<Arc<HostState>>,
    Extension(identity): Extension<Identity>,
    path: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Option<Receipt>>, Failure> {
    let Path(request_id) =
        path.map_err(|_| Failure::new(StatusCode::BAD_REQUEST, ApiErrorCode::InvalidRequest))?;
    let mut store = state.pool.get().await?;
    Ok(Json(store.receipt(identity.session, request_id).await?))
}
async fn subscribe(
    State(state): State<Arc<HostState>>,
    Extension(identity): Extension<Identity>,
    body: Result<Json<TaskQuery>, JsonRejection>,
) -> Result<Response, Failure> {
    let Json(query) = body.map_err(json_error)?;
    // Reserve for the entire HTTP stream, including an unread prior epoch.
    let capacity = state
        .streams
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::new(StatusCode::TOO_MANY_REQUESTS, ApiErrorCode::Capacity))?;
    // Bind the stream epoch while holding the coordinator lock, so a failed poll
    // cannot install an old subscription into a new recovery lifecycle.
    let mut coordinator = state.coordinator.lock().await;
    let Coordinator { store, feed } = coordinator.as_mut().ok_or_else(Failure::unavailable)?;
    let subscription = feed.subscribe(store, identity.session, query).await?;
    let epoch = state.epoch.subscribe();
    let expected_epoch = *epoch.borrow();
    drop(coordinator);
    let mut credentials = tokio::time::interval(state.options.credential_check_interval);
    credentials.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let stream = futures_util::stream::unfold(
        Some(StreamState {
            state,
            identity,
            subscription,
            epoch,
            expected_epoch,
            credentials,
            _capacity: capacity,
        }),
        |current| async {
            let mut current = current?;
            match current.frame().await {
                Ok((event, terminal)) => Some((
                    Ok::<_, Infallible>(event),
                    if terminal { None } else { Some(current) },
                )),
                Err(failure) => {
                    let event = Event::default()
                        .event("error")
                        .json_data(ApiError { code: failure.code })
                        .expect("error enum serializes");
                    Some((Ok(event), None))
                }
            }
        },
    );
    Ok(Sse::new(stream).into_response())
}

struct StreamState {
    state: Arc<HostState>,
    identity: Identity,
    subscription: TaskSubscription,
    epoch: watch::Receiver<Uuid>,
    expected_epoch: Uuid,
    credentials: Interval,
    _capacity: tokio::sync::OwnedSemaphorePermit,
}
impl StreamState {
    async fn frame(&mut self) -> Result<(Event, bool), Failure> {
        let pending = tokio::select! {
            biased;
            _ = self.epoch.changed() => false,
            _ = self.credentials.tick() => false,
            ready = self.subscription.ready() => { ready?; true },
        };
        if !self.state.available() || *self.epoch.borrow() != self.expected_epoch {
            return Err(Failure::unavailable());
        }
        let state = self.state.clone();
        let identity = &self.identity;
        let session = timeout(
            state.options.operation_timeout,
            state.auth.authenticate(&identity.token),
        )
        .await
        .map_err(|_| Failure::unavailable())??;
        if session != identity.session {
            return Err(Failure::unauthorized());
        }
        if !pending {
            return Ok((Event::default().comment("heartbeat"), false));
        }
        let message = timeout(state.options.operation_timeout, async {
            let store = state.pool.get().await?;
            self.subscription.next(&store).await.map_err(Failure::from)
        })
        .await
        .map_err(|_| Failure::unavailable())??;
        // Credential identity and recovery state may change during a database wait.
        let session = timeout(
            state.options.operation_timeout,
            state.auth.authenticate(&identity.token),
        )
        .await
        .map_err(|_| Failure::unavailable())??;
        if session != identity.session {
            return Err(Failure::unauthorized());
        }
        if !state.available()
            || *self.epoch.borrow() != self.expected_epoch
            || state.stopping.load(Ordering::Acquire)
        {
            return Err(Failure::unavailable());
        }
        let terminal = !matches!(message.result, TaskResult::Snapshot { .. });
        let event = Event::default()
            .event("delivery")
            .json_data(message.as_ref())
            .map_err(|_| Failure::unavailable())?;
        Ok((event, terminal))
    }
}
