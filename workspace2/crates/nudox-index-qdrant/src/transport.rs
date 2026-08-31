//! Synchronous HTTP transport and exact bounded retry policy.

use serde::Serialize;

use super::{
    contract::{QdrantError, RequestPhase, RetryPolicy},
    limits::MAX_RESPONSE_BYTES,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Method {
    Get,
    Put,
    Post,
    Delete,
}

#[derive(Clone, Debug)]
pub(super) struct ResponseEnvelope {
    pub(super) status: u16,
    pub(super) attempts: u8,
    pub(super) body: String,
}

pub(super) fn request_json<T: Serialize>(
    agent: &ureq::Agent,
    retry: RetryPolicy,
    phase: RequestPhase,
    method: Method,
    url: &str,
    body: T,
) -> Result<ResponseEnvelope, QdrantError> {
    let encoded =
        serde_json::to_vec(&body).map_err(|source| QdrantError::Encode { phase, source })?;
    let attempts = retry.attempts.get();
    let mut last_transport = None;
    for attempt in 1..=attempts {
        let result = match method {
            Method::Get => agent.get(url).call(),
            Method::Put => agent
                .put(url)
                .content_type("application/json")
                .send(encoded.as_slice()),
            Method::Post => agent
                .post(url)
                .content_type("application/json")
                .send(encoded.as_slice()),
            Method::Delete => agent.delete(url).call(),
        };
        match result {
            Ok(mut response) => {
                let status = response.status().as_u16();
                match response
                    .body_mut()
                    .with_config()
                    .limit(MAX_RESPONSE_BYTES)
                    .read_to_string()
                {
                    Ok(body) => {
                        if is_success(status) || !retryable_status(status) || attempt == attempts {
                            return Ok(ResponseEnvelope {
                                status,
                                attempts: attempt,
                                body,
                            });
                        }
                    }
                    Err(source) => {
                        if attempt == attempts {
                            return Err(QdrantError::Transport {
                                phase,
                                attempts,
                                source,
                            });
                        }
                        last_transport = Some(source);
                    }
                }
            }
            Err(source) => {
                if attempt == attempts {
                    return Err(QdrantError::Transport {
                        phase,
                        attempts,
                        source,
                    });
                }
                last_transport = Some(source);
            }
        }
    }
    match last_transport {
        Some(source) => Err(QdrantError::Transport {
            phase,
            attempts,
            source,
        }),
        None => Err(QdrantError::MalformedResponse {
            phase,
            cause: super::contract::MalformedResponseCause::RetryExhaustionWithoutResponse,
        }),
    }
}

pub(super) fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

fn retryable_status(status: u16) -> bool {
    status == 408 || status == 425 || status == 429 || status >= 500
}

pub(super) fn status_error(phase: RequestPhase, response: ResponseEnvelope) -> QdrantError {
    QdrantError::HttpStatus {
        phase,
        attempts: response.attempts,
        status: response.status,
        body: response.body,
    }
}
