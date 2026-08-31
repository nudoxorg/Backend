//! Synchronous HTTP transport and exact bounded retry policy.

use std::io::Read;

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

#[derive(Debug)]
enum RetryFailure {
    Transport(ureq::Error),
    ResponseRead(std::io::Error),
}

pub(super) fn request_json<T: Serialize>(
    agent: &ureq::Agent,
    retry: RetryPolicy,
    phase: RequestPhase,
    method: Method,
    url: &str,
    body: T,
) -> Result<ResponseEnvelope, QdrantError> {
    let encoded = match method {
        Method::Put | Method::Post => Some(
            serde_json::to_vec(&body).map_err(|source| QdrantError::Encode { phase, source })?,
        ),
        Method::Get | Method::Delete => None,
    };
    let attempts = retry.attempts.get();
    let mut last_transport = None;
    for attempt in 1..=attempts {
        let result = match method {
            Method::Get => agent.get(url).call(),
            Method::Put => agent
                .put(url)
                .content_type("application/json")
                .send(encoded.as_deref().unwrap_or_default()),
            Method::Post => agent
                .post(url)
                .content_type("application/json")
                .send(encoded.as_deref().unwrap_or_default()),
            Method::Delete => agent.delete(url).call(),
        };
        match result {
            Ok(mut response) => {
                let status = response.status().as_u16();
                let mut body = String::new();
                let decoded_limit = (MAX_RESPONSE_BYTES as u64).saturating_add(1);
                let mut decoded = response
                    .body_mut()
                    .with_config()
                    .limit(MAX_RESPONSE_BYTES as u64)
                    .reader()
                    .take(decoded_limit);
                match decoded.read_to_string(&mut body) {
                    Ok(_decoded_bytes) => {
                        if body.len() > MAX_RESPONSE_BYTES {
                            return Err(QdrantError::ResponseTooLarge {
                                phase,
                                maximum: MAX_RESPONSE_BYTES,
                                observed_at_least: body.len(),
                            });
                        }
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
                            return Err(QdrantError::ResponseRead {
                                phase,
                                attempts,
                                source,
                            });
                        }
                        last_transport = Some(RetryFailure::ResponseRead(source));
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
                last_transport = Some(RetryFailure::Transport(source));
            }
        }
    }
    match last_transport {
        Some(RetryFailure::Transport(source)) => Err(QdrantError::Transport {
            phase,
            attempts,
            source,
        }),
        Some(RetryFailure::ResponseRead(source)) => Err(QdrantError::ResponseRead {
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

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        num::NonZeroU8,
        thread,
    };

    use flate2::{Compression, write::GzEncoder};

    use super::*;

    #[derive(Debug, thiserror::Error)]
    enum ResponseBoundTestError {
        #[error("response-bound fixture I/O failed")]
        Io(#[from] std::io::Error),
        #[error("response-bound request failed before the expected terminal")]
        Qdrant(#[from] QdrantError),
        #[error("response-bound server thread panicked: {report}")]
        ServerPanicked { report: ServerPanicReport },
    }

    #[derive(Debug, thiserror::Error)]
    enum ServerPanicReport {
        #[error("{message}")]
        Static { message: &'static str },
        #[error("{message}")]
        Owned { message: String },
        #[error("non-text panic payload")]
        NonText,
    }

    #[test]
    fn decoded_gzip_body_is_bounded_after_decompression() -> Result<(), ResponseBoundTestError> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let server = thread::spawn(move || -> Result<(), std::io::Error> {
            let (mut stream, _) = listener.accept()?;
            let mut request = [0_u8; 1024];
            let _request_bytes = stream.read(&mut request)?;

            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            let oversized = vec![b'x'; MAX_RESPONSE_BYTES.saturating_add(1)];
            encoder.write_all(&oversized)?;
            let compressed = encoder.finish()?;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                compressed.len()
            )?;
            stream.write_all(&compressed)
        });

        let endpoint = format!("http://{address}");
        let result = request_json(
            &ureq::Agent::new_with_defaults(),
            RetryPolicy {
                attempts: NonZeroU8::MIN,
            },
            RequestPhase::ReadCollection,
            Method::Get,
            &endpoint,
            (),
        );
        let server_result = match server.join() {
            Ok(result) => result,
            Err(panic) => {
                let report = if let Some(message) = panic.downcast_ref::<&'static str>() {
                    ServerPanicReport::Static { message }
                } else if let Some(message) = panic.downcast_ref::<String>() {
                    ServerPanicReport::Owned {
                        message: message.clone(),
                    }
                } else {
                    ServerPanicReport::NonText
                };
                return Err(ResponseBoundTestError::ServerPanicked { report });
            }
        };
        server_result?;
        assert!(matches!(
            result,
            Err(QdrantError::ResponseTooLarge {
                phase: RequestPhase::ReadCollection,
                maximum: MAX_RESPONSE_BYTES,
                observed_at_least,
            }) if observed_at_least == MAX_RESPONSE_BYTES + 1
        ));
        Ok(())
    }
}
