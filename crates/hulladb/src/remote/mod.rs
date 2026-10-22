//! Named-function HTTP/SSE runtime with a durable SQLite mutation journal.
mod storage;
mod wire;

use crate::{
    ClientFailure, Error, FunctionFuture, MutationContract, MutationIntent, RawQueryState,
    RawSubscription, Receipt, Runtime, Snapshot, runtime::Validator,
};
pub use http::{ClientOptions, Endpoint};
use hulladb_client::http::{
    self,
    generic::{HttpTransport, SseFrame},
};
use hulladb_protocol::ApiErrorCode;
use serde_json::{Value as Json, json};
use std::{
    future::Future,
    sync::{Arc, Mutex as SyncMutex, MutexGuard},
    time::Duration,
};
pub use storage::RecoveryStore;
use tokio::sync::{Mutex, Semaphore, watch};
use uuid::Uuid;
pub use wire::Session;

/// Credential-free cache/journal identity. Use the same origin normalization as transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition(String);
/// A durable acknowledgement paired with its original operation and input.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletedMutation {
    pub intent: MutationIntent,
    pub receipt: Receipt,
}
impl Partition {
    pub fn new(endpoint: &Endpoint, session: Session) -> Self {
        Self(
            json!([
                endpoint.origin(),
                session.scope,
                session.principal,
                session.authorization_scope
            ])
            .to_string(),
        )
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub transport: ClientOptions,
    pub max_pending_mutations: usize,
    /// Includes the initial submission; subsequent attempts only look up its outcome.
    pub recovery_attempts: usize,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            transport: ClientOptions::default(),
            max_pending_mutations: 1024,
            recovery_attempts: 3,
        }
    }
}
struct Life {
    retired: SyncMutex<bool>,
    signal: watch::Sender<bool>,
}
impl Life {
    fn active(&self) -> Result<MutexGuard<'_, bool>, Error> {
        let guard = self.retired.lock().map_err(|_| Error::CorruptData)?;
        if *guard {
            return Err(Error::Client(ClientFailure::Closed));
        }
        Ok(guard)
    }
    fn stop(&self) {
        if let Ok(mut guard) = self.retired.lock() {
            *guard = true;
            self.signal.send_replace(true);
        }
    }
}
/// One immutable authenticated identity. Retire it before replacing credentials/claims.
/// Dropping a call cancels transport, but its write-ahead intent stays recoverable.
pub struct RemoteRuntime {
    transport: HttpTransport,
    store: RecoveryStore,
    partition: Partition,
    session: Session,
    source: Uuid,
    options: Options,
    life: Arc<Life>,
    writes: Mutex<()>,
    ticket: Mutex<(wire::Ticket, tokio::time::Instant)>,
    streams: Arc<Semaphore>,
}
impl RemoteRuntime {
    pub async fn connect(
        endpoint: Endpoint,
        bearer: &str,
        session: Session,
        store: RecoveryStore,
        options: Options,
    ) -> Result<Self, Error> {
        if options.max_pending_mutations == 0
            || options.recovery_attempts == 0
            || options.transport.max_subscriptions > Semaphore::MAX_PERMITS
        {
            return Err(Error::InvalidInput(
                "remote budgets must be positive".into(),
            ));
        }
        let partition = Partition::new(&endpoint, session);
        let transport = HttpTransport::new(endpoint, bearer, options.transport)
            .map_err(map_error)?
            .with_identity(
                session.scope,
                session.principal,
                session.authorization_scope,
            );
        let ticket_started = tokio::time::Instant::now();
        let handshake = async {
            let bytes = transport
                .json("GET", "/functions/session", vec![])
                .await
                .map_err(map_error)?;
            let head: wire::Connection =
                serde_json::from_slice(&bytes).map_err(|_| wire::protocol())?;
            head.head.check(session, None)?;
            head.ticket.check()?;
            Ok::<_, Error>(head)
        }
        .await;
        let head = match handshake {
            Ok(head) => head,
            Err(error) => {
                if invalidates(&error) {
                    store.clear_results(&partition).await?;
                }
                return Err(error);
            }
        };
        if let Err(error) = store.bind(&partition, head.head.source).await {
            store.clear_results(&partition).await?;
            return Err(error);
        }
        let (signal, _) = watch::channel(false);
        Ok(Self {
            transport,
            store,
            partition,
            session,
            source: head.head.source,
            options,
            life: Arc::new(Life {
                retired: SyncMutex::new(false),
                signal,
            }),
            writes: Mutex::new(()),
            ticket: Mutex::new((head.ticket, ticket_started)),
            streams: Arc::new(Semaphore::new(options.transport.max_subscriptions)),
        })
    }
    pub fn source(&self) -> Uuid {
        self.source
    }
    pub fn partition(&self) -> &Partition {
        &self.partition
    }
    pub async fn pending(&self) -> Result<Vec<MutationIntent>, Error> {
        self.store.pending(&self.partition).await
    }
    /// Abort active work and clear this identity's result cache; preserve unresolved writes.
    pub async fn retire(&self) -> Result<(), Error> {
        self.life.stop();
        self.store.clear_results(&self.partition).await
    }
    async fn guarded<T>(&self, work: impl Future<Output = Result<T, Error>>) -> Result<T, Error> {
        let mut retired = self.life.signal.subscribe();
        drop(self.life.active()?);
        tokio::select! { biased;
            _=retired.changed()=>Err(Error::Client(ClientFailure::Closed)),
            result=work=>{ drop(self.life.active()?); result }
        }
    }
    async fn handle_failure(&self, error: &Error) -> Result<(), Error> {
        if invalidates(error) {
            self.retire().await?;
        }
        Ok(())
    }
    fn operation(&self, name: &str, input: Json) -> Result<Vec<u8>, Error> {
        self.encode(json!({"name":name,"input":input,"expected_source":self.source}))
    }
    fn mutation_payload(&self, intent: &MutationIntent, ticket: Uuid) -> Result<Vec<u8>, Error> {
        self.encode(json!({"request_id":intent.request_id,"ticket":ticket,"name":intent.operation,"input":intent.input,"expected_source":intent.source}))
    }
    fn encode(&self, value: Json) -> Result<Vec<u8>, Error> {
        let bytes = serde_json::to_vec(&value).map_err(|_| wire::protocol())?;
        if bytes.len() > self.options.transport.max_response_bytes {
            return Err(Error::Client(ClientFailure::Budget));
        }
        Ok(bytes)
    }
    async fn json(&self, method: &str, path: &str, bytes: Vec<u8>) -> Result<Json, Error> {
        let bytes = self
            .guarded(async {
                self.transport
                    .json(method, path, bytes)
                    .await
                    .map_err(map_error)
            })
            .await?;
        serde_json::from_slice(&bytes).map_err(|_| wire::protocol())
    }
    async fn query_inner(
        &self,
        name: &str,
        input: Json,
        validate: Validator,
    ) -> Result<Snapshot, Error> {
        let key = query_key(name, &input);
        let result = async {
            let raw = self
                .json("POST", "/functions/query", self.operation(name, input)?)
                .await?;
            let snapshot = wire::snapshot(raw, self.session, self.source)?;
            validate(&snapshot.result).map_err(|_| wire::protocol())?;
            self.store
                .put_result(&self.partition, key, snapshot.clone(), self.life.clone())
                .await?;
            drop(self.life.active()?);
            Ok(snapshot)
        }
        .await;
        if let Err(error) = &result {
            self.handle_failure(error).await?;
        }
        result
    }
    async fn resolve(
        &self,
        intent: &MutationIntent,
        recovering: bool,
        validate: impl Fn(&str, &Json) -> Result<(), Error>,
    ) -> Result<Receipt, Error> {
        if intent.source != self.source {
            return Err(Error::SourceMismatch);
        }
        let ticket = self
            .store
            .ticket(&self.partition, intent.request_id)
            .await?;
        let payload = self.mutation_payload(intent, ticket)?;
        for attempt in 0..self.options.recovery_attempts {
            let result = async {
                let mut raw = Json::Null;
                if recovering || attempt > 0 {
                    raw = self
                        .json(
                            "GET",
                            &format!(
                                "/functions/receipts/{}?source={}&ticket={}",
                                intent.request_id, intent.source, ticket
                            ),
                            vec![],
                        )
                        .await?;
                }
                if raw.is_null() && !recovering && attempt == 0 {
                    raw = self
                        .json("POST", "/functions/mutate", payload.clone())
                        .await?;
                }
                if raw.is_null() {
                    return Err(Error::CommitOutcomeUnknown);
                }
                let receipt = wire::receipt(raw, self.session, self.source, intent.request_id)?;
                validate(&intent.operation, &receipt.result).map_err(|_| wire::protocol())?;
                self.store
                    .acknowledge(
                        &self.partition,
                        intent.clone(),
                        Some(receipt.clone()),
                        self.life.clone(),
                    )
                    .await?;
                // Local durable delivery precedes best-effort result compaction.
                let _ = self
                    .json(
                        "POST",
                        &format!("/functions/receipts/{}/ack", intent.request_id),
                        self.encode(json!({"source":intent.source,"ticket":ticket}))?,
                    )
                    .await;
                drop(self.life.active()?);
                Ok(receipt)
            }
            .await;
            match result {
                Ok(receipt) => return Ok(receipt),
                Err(error) => {
                    if !transient(&error) {
                        // Terminal lookup responses resolve the journal entry. Authentication
                        // denial can block delivery of an existing outcome and stays pending.
                        if terminal_outcome(&error)
                            || (!recovering && attempt == 0 && definite_rejection(&error))
                        {
                            self.store
                                .acknowledge(
                                    &self.partition,
                                    intent.clone(),
                                    None,
                                    self.life.clone(),
                                )
                                .await?;
                        }
                        self.handle_failure(&error).await?;
                        return Err(error);
                    }
                    if attempt + 1 < self.options.recovery_attempts {
                        self.guarded(async {
                            tokio::time::sleep(self.options.transport.reconnect_delay).await;
                            Ok(())
                        })
                        .await?;
                    }
                }
            }
        }
        Err(Error::CommitOutcomeUnknown)
    }
    /// Validate stored inputs before lookup and outputs before acknowledging them.
    /// Unknown contracts or denied recovery leave the original intents untouched.
    pub async fn recover_pending(
        &self,
        contracts: &[MutationContract],
    ) -> Result<Vec<Receipt>, Error> {
        let _writes = self.guarded(async { Ok(self.writes.lock().await) }).await?;
        let mut receipts = vec![];
        for intent in self.pending().await? {
            let contract = contracts
                .iter()
                .find(|contract| contract.name == intent.operation)
                .ok_or_else(|| {
                    Error::InvalidInput(format!(
                        "unknown stored mutation contract: {}",
                        intent.operation
                    ))
                })?;
            (contract.input)(&intent.input)?;
            receipts.push(
                self.resolve(&intent, true, |_, value| (contract.output)(value))
                    .await?,
            );
        }
        Ok(receipts)
    }
    async fn new_ticket(&self) -> Result<Uuid, Error> {
        let mut current = self.ticket.lock().await;
        if current.1.elapsed() >= Duration::from_millis(current.0.renew_after_ms) {
            let started = tokio::time::Instant::now();
            let raw = self.json("GET", "/functions/session", vec![]).await?;
            let connection: wire::Connection =
                serde_json::from_value(raw).map_err(|_| wire::protocol())?;
            connection.head.check(self.session, Some(self.source))?;
            connection.ticket.check()?;
            *current = (connection.ticket, started);
        }
        if current.1.elapsed() >= Duration::from_millis(current.0.expires_in_ms) {
            return Err(Error::InvalidTicket);
        }
        Ok(current.0.id)
    }
    async fn mutation_inner(
        &self,
        name: &str,
        input: Json,
        validate: Validator,
    ) -> Result<Receipt, Error> {
        // Reject oversized inputs before journaling; durable intent precedes any network send.
        let _writes = self.guarded(async { Ok(self.writes.lock().await) }).await?;
        let ticket = match self.new_ticket().await {
            Ok(ticket) => ticket,
            Err(error) => {
                self.handle_failure(&error).await?;
                return Err(error);
            }
        };
        let intent = MutationIntent {
            request_id: Uuid::now_v7(),
            operation: name.into(),
            input,
            source: self.source,
        };
        self.mutation_payload(&intent, ticket)?;
        self.store
            .put_intent(
                &self.partition,
                intent.clone(),
                ticket,
                self.options.max_pending_mutations,
                self.life.clone(),
            )
            .await?;
        self.resolve(&intent, false, |_, value| validate(value))
            .await
    }
    async fn subscribe_inner(
        self: Arc<Self>,
        name: String,
        input: Json,
        validate: Validator,
    ) -> Result<RawSubscription, Error> {
        drop(self.life.active()?);
        let permit = self
            .streams
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Client(ClientFailure::Capacity))?;
        let payload = self.encode(json!({"expected_source":self.source,"queries":[{"key":"query","name":name,"input":input}]}))?;
        let key = query_key(&name, &input);
        let cached = self
            .store
            .result(&self.partition, key.clone(), self.session, self.source)
            .await?;
        if let Some(snapshot) = &cached
            && validate(&snapshot.result).is_err()
        {
            self.retire().await?;
            return Err(wire::protocol());
        }
        let initial = match cached {
            Some(snapshot) => RawQueryState::Ready {
                snapshot,
                connected: false,
            },
            None => RawQueryState::Loading,
        };
        let (sender, updates) = watch::channel(initial);
        let worker = tokio::spawn(async move {
            let _permit = permit;
            loop {
                let result = self.stream_once(&payload, &key, validate, &sender).await;
                match result {
                    Ok(()) => return,
                    Err(error) if transient(&error) => {
                        {
                            let previous = sender.borrow().clone();
                            if let RawQueryState::Ready { snapshot, .. } = previous {
                                sender.send_replace(RawQueryState::Ready {
                                    snapshot,
                                    connected: false,
                                });
                            }
                        }
                        if let Err(error) = self
                            .guarded(async {
                                tokio::time::sleep(self.options.transport.reconnect_delay).await;
                                Ok(())
                            })
                            .await
                        {
                            sender.send_replace(RawQueryState::Error(Arc::new(error)));
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = self.handle_failure(&error).await;
                        sender.send_replace(RawQueryState::Error(Arc::new(error)));
                        return;
                    }
                }
            }
        });
        Ok(RawSubscription::new(updates, worker))
    }
    async fn stream_once(
        &self,
        payload: &[u8],
        key: &str,
        validate: Validator,
        sender: &watch::Sender<RawQueryState>,
    ) -> Result<(), Error> {
        let mut stream = self
            .guarded(async {
                self.transport
                    .stream("/functions/subscriptions", payload.to_vec())
                    .await
                    .map_err(map_error)
            })
            .await?;
        let mut cursor: Option<(Uuid, i64)> = None;
        let mut revision = None;
        loop {
            match self
                .guarded(async { stream.next().await.map_err(map_error) })
                .await?
            {
                SseFrame::Heartbeat => {}
                SseFrame::Event { name, id, data } if name == "round" => {
                    let delivery: wire::Delivery =
                        serde_json::from_str(&data).map_err(|_| wire::protocol())?;
                    let sequence = wire::counter(&delivery.sequence)?;
                    if sequence == 0
                        || id.as_deref()
                            != Some(format!("{}:{sequence}", delivery.subscription).as_str())
                    {
                        return Err(wire::protocol());
                    }
                    if let Some((subscription, last)) = cursor {
                        if subscription != delivery.subscription
                            || last.checked_add(1) != Some(sequence)
                        {
                            return Err(wire::protocol());
                        }
                    } else if sequence != 1 {
                        return Err(wire::protocol());
                    }
                    cursor = Some((delivery.subscription, sequence));
                    let (received_revision, member) =
                        delivery.round.check(self.session, self.source)?;
                    if revision.is_some_and(|prior| received_revision < prior) {
                        return Err(wire::protocol());
                    }
                    revision = Some(received_revision);
                    let snapshot = match member {
                        Ok(snapshot) => snapshot,
                        Err(code) => {
                            let error = map_error(code);
                            if matches!(error, Error::Forbidden) {
                                self.store
                                    .remove_result(&self.partition, key, self.life.clone())
                                    .await?;
                            }
                            if matches!(
                                error,
                                Error::Client(ClientFailure::Unauthorized) | Error::SourceMismatch
                            ) {
                                return Err(error);
                            }
                            let _active = self.life.active()?;
                            sender.send_replace(RawQueryState::Error(Arc::new(error)));
                            continue;
                        }
                    };
                    validate(&snapshot.result).map_err(|_| wire::protocol())?;
                    let received_revision = snapshot.revision.sequence;
                    let snapshot = self
                        .store
                        .put_result(&self.partition, key.into(), snapshot, self.life.clone())
                        .await?;
                    // An older in-flight response cannot certify a newer durable
                    // cache entry after restart/reconnect. Wait for live coverage.
                    if snapshot.revision.sequence > received_revision {
                        continue;
                    }
                    let _active = self.life.active()?;
                    sender.send_replace(RawQueryState::Ready {
                        snapshot,
                        connected: true,
                    });
                }
                SseFrame::Event { name, data, .. } if name == "error" => {
                    let error: hulladb_protocol::ApiError =
                        serde_json::from_str(&data).map_err(|_| wire::protocol())?;
                    if !error.valid() {
                        return Err(wire::protocol());
                    }
                    return Err(map_error(match error.application {
                        Some(error) => http::Error::Application(error),
                        None => http::Error::Api(error.code),
                    }));
                }
                _ => return Err(wire::protocol()),
            }
        }
    }
}
impl Drop for RemoteRuntime {
    fn drop(&mut self) {
        self.life.stop();
    }
}
impl Runtime for RemoteRuntime {
    fn query<'a>(&'a self, operation: &'a str, input: Json) -> FunctionFuture<'a, Snapshot> {
        self.query_checked(operation, input, |_| Ok(()))
    }
    fn mutation<'a>(&'a self, operation: &'a str, input: Json) -> FunctionFuture<'a, Receipt> {
        self.mutation_checked(operation, input, |_| Ok(()))
    }
    fn query_checked<'a>(
        &'a self,
        operation: &'a str,
        input: Json,
        validate: Validator,
    ) -> FunctionFuture<'a, Snapshot> {
        Box::pin(self.query_inner(operation, input, validate))
    }
    fn mutation_checked<'a>(
        &'a self,
        operation: &'a str,
        input: Json,
        validate: Validator,
    ) -> FunctionFuture<'a, Receipt> {
        Box::pin(self.mutation_inner(operation, input, validate))
    }
    fn subscribe(
        self: Arc<Self>,
        operation: String,
        input: Json,
    ) -> FunctionFuture<'static, RawSubscription> {
        self.subscribe_checked(operation, input, |_| Ok(()))
    }
    fn subscribe_checked(
        self: Arc<Self>,
        operation: String,
        input: Json,
        validate: Validator,
    ) -> FunctionFuture<'static, RawSubscription> {
        Box::pin(self.subscribe_inner(operation, input, validate))
    }
    fn retirement(&self) -> watch::Receiver<bool> {
        self.life.signal.subscribe()
    }
    fn poll_interval(&self) -> Duration {
        self.options.transport.reconnect_delay
    }
}
fn query_key(name: &str, input: &Json) -> String {
    json!([name, input]).to_string()
}
fn invalidates(error: &Error) -> bool {
    matches!(
        error,
        Error::Forbidden
            | Error::SourceMismatch
            | Error::Client(
                ClientFailure::Unauthorized | ClientFailure::Protocol | ClientFailure::Budget
            )
    )
}
fn transient(error: &Error) -> bool {
    matches!(
        error,
        Error::CommitOutcomeUnknown
            | Error::Client(
                ClientFailure::Network | ClientFailure::Timeout | ClientFailure::Capacity
            )
    )
}
fn terminal_outcome(error: &Error) -> bool {
    matches!(
        error,
        Error::Application(_)
            | Error::TerminalFailure(_)
            | Error::TransactionDeadline
            | Error::QuotaExceeded
            | Error::ReceiptExpired
            | Error::InvalidTicket
            | Error::InvalidInput(_)
            | Error::Conflict
            | Error::NotFound
    )
}
fn definite_rejection(error: &Error) -> bool {
    matches!(
        error,
        Error::Forbidden
            | Error::NotFound
            | Error::Conflict
            | Error::RequestIdReused
            | Error::InvalidInput(_)
            | Error::QuotaExceeded
            | Error::Application(_)
            | Error::TerminalFailure(_)
            | Error::TransactionDeadline
            | Error::InvalidTicket
    )
}
fn map_error(error: http::Error) -> Error {
    use http::Error as E;
    match error {
        E::Application(error) => Error::Application(error),
        E::Api(code) => match code {
            ApiErrorCode::Unauthorized => Error::Client(ClientFailure::Unauthorized),
            ApiErrorCode::Forbidden => Error::Forbidden,
            ApiErrorCode::NotFound => Error::NotFound,
            ApiErrorCode::Conflict => Error::Conflict,
            ApiErrorCode::RequestIdReused => Error::RequestIdReused,
            ApiErrorCode::ReceiptExpired => Error::ReceiptExpired,
            ApiErrorCode::TicketExpired => Error::InvalidTicket,
            ApiErrorCode::QuotaExceeded => Error::QuotaExceeded,
            ApiErrorCode::ExecutionLimit => Error::TransactionDeadline,
            ApiErrorCode::ExecutionFailed => Error::TerminalFailure("host execution failed".into()),
            ApiErrorCode::InvalidRequest => {
                Error::InvalidInput("host rejected function input".into())
            }
            ApiErrorCode::ResetRequired => Error::SourceMismatch,
            ApiErrorCode::Capacity => Error::Client(ClientFailure::Capacity),
            ApiErrorCode::Unavailable => Error::Client(ClientFailure::Network),
            ApiErrorCode::OutcomeUnknown => Error::CommitOutcomeUnknown,
        },
        E::Network => Error::Client(ClientFailure::Network),
        E::Timeout => Error::Client(ClientFailure::Timeout),
        E::Tls => Error::Client(ClientFailure::Tls),
        E::Budget => Error::Client(ClientFailure::Budget),
        E::InvalidEndpoint | E::InvalidOptions => Error::InvalidInput(error.to_string()),
        E::Protocol => wire::protocol(),
    }
}
