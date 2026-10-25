//! Bounded JSON/SSE transport shared by named-function clients and native TLS.
use super::*;

#[derive(Clone)]
pub struct HttpTransport {
    endpoint: Endpoint,
    bearer: HeaderValue,
    options: ClientOptions,
    identity: Option<[Uuid; 3]>,
}
impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(10),
            stream_idle_timeout: Duration::from_secs(45),
            reconnect_delay: Duration::from_millis(250),
            max_response_bytes: 1024 * 1024,
            max_event_bytes: 1024 * 1024,
            max_subscriptions: 128,
        }
    }
}
impl HttpTransport {
    pub fn new(endpoint: Endpoint, bearer: &str, options: ClientOptions) -> Result<Self, Error> {
        if options.request_timeout.is_zero()
            || options.stream_idle_timeout.is_zero()
            || options.reconnect_delay.is_zero()
            || options.max_response_bytes == 0
            || options.max_event_bytes == 0
            || options.max_subscriptions == 0
        {
            return Err(Error::InvalidOptions);
        }
        Ok(Self {
            endpoint,
            bearer: credential(bearer)?,
            options,
            identity: None,
        })
    }
    pub fn with_identity(
        mut self,
        scope: Uuid,
        principal: Uuid,
        authorization_scope: Uuid,
    ) -> Self {
        self.identity = Some([scope, principal, authorization_scope]);
        self
    }
    pub async fn json(&self, method: &str, path: &str, bytes: Vec<u8>) -> Result<Vec<u8>, Error> {
        timeout(self.options.request_timeout, async {
            let (_http, response) = request(
                &self.endpoint,
                &self.bearer,
                method,
                path,
                bytes,
                false,
                self.identity,
            )
            .await?;
            let status = response.status();
            if !mime(&response, "application/json") {
                return Err(Error::Protocol);
            }
            let bytes = body_bytes(response.into_body(), self.options.max_response_bytes).await?;
            if status != StatusCode::OK {
                return Err(checked_error(status, &bytes)?);
            }
            Ok(bytes)
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
    pub async fn stream(&self, path: &str, bytes: Vec<u8>) -> Result<EventStream, Error> {
        timeout(self.options.request_timeout, async {
            let (http, response) = request(
                &self.endpoint,
                &self.bearer,
                "POST",
                path,
                bytes,
                true,
                self.identity,
            )
            .await?;
            if response.status() != StatusCode::OK {
                let status = response.status();
                let bytes =
                    body_bytes(response.into_body(), self.options.max_response_bytes).await?;
                return Err(checked_error(status, &bytes)?);
            }
            if !mime(&response, "text/event-stream") {
                return Err(Error::Protocol);
            }
            Ok(EventStream {
                _http: http,
                body: response.into_body(),
                chunk: Bytes::new(),
                decoder: SseDecoder::new(self.options.max_event_bytes),
                idle: self.options.stream_idle_timeout,
            })
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}
fn mime(response: &hyper::Response<Incoming>, expected: &str) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|mime| mime.trim() == expected)
}
fn checked_error(status: StatusCode, bytes: &[u8]) -> Result<Error, Error> {
    let failure: ApiError = serde_json::from_slice(bytes).map_err(|_| Error::Protocol)?;
    if !failure.valid() {
        return Err(Error::Protocol);
    }
    let code = failure.code;
    let expected = match code {
        ApiErrorCode::Unauthorized => 401,
        ApiErrorCode::Forbidden => 403,
        ApiErrorCode::InvalidRequest => 400,
        ApiErrorCode::NotFound => 404,
        ApiErrorCode::Conflict | ApiErrorCode::RequestIdReused | ApiErrorCode::ResetRequired => 409,
        ApiErrorCode::ReceiptExpired | ApiErrorCode::TicketExpired => 410,
        ApiErrorCode::ExecutionFailed
        | ApiErrorCode::ExecutionLimit
        | ApiErrorCode::QuotaExceeded => 422,
        ApiErrorCode::Capacity => 429,
        ApiErrorCode::Unavailable | ApiErrorCode::OutcomeUnknown => 503,
    };
    if status.as_u16() != expected
        && !(code == ApiErrorCode::InvalidRequest && [413, 415, 422].contains(&status.as_u16()))
    {
        return Err(Error::Protocol);
    }
    Ok(match failure.application {
        Some(error) => Error::Application(error),
        None => Error::Api(code),
    })
}

#[derive(Debug)]
pub enum SseFrame {
    Heartbeat,
    Event {
        name: String,
        id: Option<String>,
        data: String,
    },
}
pub struct EventStream {
    _http: Http,
    body: Incoming,
    chunk: Bytes,
    decoder: SseDecoder,
    idle: Duration,
}
impl EventStream {
    pub async fn next(&mut self) -> Result<SseFrame, Error> {
        timeout(self.idle, self.next_frame())
            .await
            .map_err(|_| Error::Timeout)?
    }
    async fn next_frame(&mut self) -> Result<SseFrame, Error> {
        loop {
            for offset in 0..self.chunk.len() {
                if let Some(event) = self.decoder.byte(self.chunk[offset])? {
                    self.chunk = self.chunk.slice(offset + 1..);
                    return Ok(event);
                }
            }
            self.chunk = Bytes::new();
            match self.body.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(bytes) = frame.into_data() {
                        self.chunk = bytes;
                    }
                }
                Some(Err(_)) => return Err(Error::Network),
                None => {
                    return Err(if self.decoder.used == 0 {
                        Error::Network
                    } else {
                        Error::Protocol
                    });
                }
            }
        }
    }
}
struct SseDecoder {
    limit: usize,
    used: usize,
    line: Vec<u8>,
    name: String,
    id: Option<String>,
    data: String,
    has_data: bool,
    cr: bool,
}
impl SseDecoder {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            used: 0,
            line: vec![],
            name: String::new(),
            id: None,
            data: String::new(),
            has_data: false,
            cr: false,
        }
    }
    fn byte(&mut self, byte: u8) -> Result<Option<SseFrame>, Error> {
        if self.cr {
            self.cr = false;
            if byte == b'\n' {
                return Ok(None);
            }
        }
        // Reserve the LF of a possible CRLF before delivering a frame at CR.
        // Bare CR delimiters are conservatively charged the same two bytes.
        self.used = self.used.saturating_add(if byte == b'\r' { 2 } else { 1 });
        if self.used > self.limit {
            return Err(Error::Budget);
        }
        if byte != b'\r' && byte != b'\n' {
            self.line.push(byte);
            return Ok(None);
        }
        self.cr = byte == b'\r';
        let line = std::str::from_utf8(&self.line).map_err(|_| Error::Protocol)?;
        if line.is_empty() {
            let frame = if self.has_data {
                SseFrame::Event {
                    name: if self.name.is_empty() {
                        "message".into()
                    } else {
                        std::mem::take(&mut self.name)
                    },
                    id: self.id.take(),
                    data: std::mem::take(&mut self.data),
                }
            } else {
                SseFrame::Heartbeat
            };
            self.used = 0;
            self.has_data = false;
            self.name.clear();
            self.id = None;
            return Ok(Some(frame));
        }
        if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.name = value.into(),
                "id" if !value.contains('\0') => self.id = Some(value.into()),
                "data" => {
                    if self.has_data {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                    self.has_data = true;
                }
                _ => {}
            }
        }
        self.line.clear();
        Ok(None)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/generic_sse.rs"]
mod tests;
