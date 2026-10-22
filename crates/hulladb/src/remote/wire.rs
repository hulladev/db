use crate::{ClientFailure, Codec, Error, Receipt, Revision, Snapshot};
use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    #[serde(deserialize_with = "uuid")]
    pub scope: Uuid,
    #[serde(deserialize_with = "uuid")]
    pub principal: Uuid,
    #[serde(deserialize_with = "uuid")]
    pub authorization_scope: Uuid,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    #[serde(flatten)]
    pub head: Head,
    pub ticket: Ticket,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    #[serde(deserialize_with = "uuid")]
    pub id: Uuid,
    pub expires_in_ms: u64,
    pub renew_after_ms: u64,
}
impl Ticket {
    pub fn check(&self) -> Result<(), Error> {
        if self.expires_in_ms == 0
            || self.renew_after_ms == 0
            || self.renew_after_ms >= self.expires_in_ms
        {
            return Err(protocol());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
pub struct Head {
    #[serde(deserialize_with = "uuid")]
    pub source: Uuid,
    #[serde(deserialize_with = "uuid")]
    pub scope: Uuid,
    #[serde(deserialize_with = "uuid")]
    pub principal: Uuid,
    #[serde(deserialize_with = "uuid")]
    pub authorization_scope: Uuid,
    pub revision: String,
}
impl Head {
    pub fn check(&self, session: Session, source: Option<Uuid>) -> Result<i64, Error> {
        if session
            != (Session {
                scope: self.scope,
                principal: self.principal,
                authorization_scope: self.authorization_scope,
            })
        {
            return Err(Error::Client(ClientFailure::Unauthorized));
        }
        if source.is_some_and(|source| source != self.source) {
            return Err(Error::SourceMismatch);
        }
        counter(&self.revision)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSnapshot {
    #[serde(deserialize_with = "uuid")]
    source: Uuid,
    #[serde(deserialize_with = "uuid")]
    scope: Uuid,
    #[serde(deserialize_with = "uuid")]
    principal: Uuid,
    #[serde(deserialize_with = "uuid")]
    authorization_scope: Uuid,
    revision: String,
    result: Json,
    #[serde(default)]
    page: Option<hulladb_protocol::functions::PageMetadata>,
}
pub fn snapshot(value: Json, session: Session, source: Uuid) -> Result<Snapshot, Error> {
    let value: WireSnapshot = serde_json::from_value(value).map_err(|_| protocol())?;
    let head = Head {
        source: value.source,
        scope: value.scope,
        principal: value.principal,
        authorization_scope: value.authorization_scope,
        revision: value.revision,
    };
    Ok(Snapshot {
        source,
        revision: Revision {
            scope: session.scope,
            sequence: head.check(session, Some(source))?,
        },
        principal: session.principal,
        authorization_scope: session.authorization_scope,
        result: value.result,
        page: checked_page(value.page)?,
        tables: BTreeSet::new(),
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReceipt {
    #[serde(deserialize_with = "uuid")]
    request_id: Uuid,
    #[serde(deserialize_with = "uuid")]
    source: Uuid,
    #[serde(deserialize_with = "uuid")]
    scope: Uuid,
    revision: String,
    result: Json,
}
pub fn receipt(
    value: Json,
    session: Session,
    source: Uuid,
    request: Uuid,
) -> Result<Receipt, Error> {
    let value: WireReceipt = serde_json::from_value(value).map_err(|_| protocol())?;
    if value.request_id != request || value.scope != session.scope {
        return Err(protocol());
    }
    if value.source != source {
        return Err(Error::SourceMismatch);
    }
    Ok(Receipt {
        request_id: request,
        source,
        revision: Revision {
            scope: session.scope,
            sequence: counter(&value.revision)?,
        },
        result: value.result,
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    #[serde(deserialize_with = "uuid")]
    pub subscription: Uuid,
    pub sequence: String,
    pub round: WireRound,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireRound {
    #[serde(deserialize_with = "uuid")]
    source: Uuid,
    #[serde(deserialize_with = "uuid")]
    scope: Uuid,
    #[serde(deserialize_with = "uuid")]
    principal: Uuid,
    #[serde(deserialize_with = "uuid")]
    authorization_scope: Uuid,
    revision: String,
    results: Vec<Member>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum Member {
    Result(MemberResult),
    Error(MemberError),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberResult {
    key: String,
    result: Json,
    #[serde(default)]
    page: Option<hulladb_protocol::functions::PageMetadata>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberError {
    key: String,
    error: hulladb_protocol::ApiError,
}
impl WireRound {
    pub fn check(
        self,
        session: Session,
        source: Uuid,
    ) -> Result<(i64, Result<Snapshot, hulladb_client::http::Error>), Error> {
        let head = Head {
            source: self.source,
            scope: self.scope,
            principal: self.principal,
            authorization_scope: self.authorization_scope,
            revision: self.revision,
        };
        let revision = head.check(session, Some(source))?;
        let mut results = self.results.into_iter();
        let member = results.next().ok_or_else(protocol)?;
        if results.next().is_some() {
            return Err(protocol());
        }
        let result = match member {
            Member::Result(value) if value.key == "query" => Ok(Snapshot {
                source,
                revision: Revision {
                    scope: session.scope,
                    sequence: revision,
                },
                principal: session.principal,
                authorization_scope: session.authorization_scope,
                result: value.result,
                page: checked_page(value.page)?,
                tables: BTreeSet::new(),
            }),
            Member::Error(value) if value.key == "query" => {
                if !value.error.valid() {
                    return Err(protocol());
                }
                Err(match value.error.application {
                    Some(error) => hulladb_client::http::Error::Application(error),
                    None => hulladb_client::http::Error::Api(value.error.code),
                })
            }
            _ => return Err(protocol()),
        };
        Ok((revision, result))
    }
}
fn checked_page(
    page: Option<hulladb_protocol::functions::PageMetadata>,
) -> Result<Option<hulladb_protocol::functions::PageMetadata>, Error> {
    if page
        .as_ref()
        .is_some_and(|page| page.range.is_empty() || page.range.len() > 65_536)
    {
        return Err(protocol());
    }
    Ok(page)
}

pub fn counter(text: &str) -> Result<i64, Error> {
    let value = i64::decode(&Json::String(text.into())).map_err(|_| protocol())?;
    if value < 0 {
        Err(protocol())
    } else {
        Ok(value)
    }
}
pub fn protocol() -> Error {
    Error::Client(ClientFailure::Protocol)
}

fn uuid<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(deserializer)?;
    <Uuid as Codec>::decode(&Json::String(text)).map_err(serde::de::Error::custom)
}
