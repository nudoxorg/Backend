//! Loopback HTTP instrumentation for an existing locald owner.
use axum::{
    body::Body,
    http::{
        Method, Request, Response, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE, TRANSFER_ENCODING},
    },
};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::Semaphore,
    task::JoinSet,
};

const LIMIT: usize = 8192;
const DEADLINE: Duration = Duration::from_secs(2);
const OWNER_DEADLINE: Duration = Duration::from_millis(750);
const STATUSES: [u16; 5] = [200, 400, 404, 405, 503];
const BUCKETS: [f64; 11] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 1.5, 2.0, 5.0];

#[derive(Default)]
struct Metrics {
    counts: [u64; 5],
    buckets: [u64; 11],
    count: u64,
    sum: f64,
}
impl Metrics {
    fn record(&mut self, status: u16, elapsed: f64) {
        if let Some(index) = STATUSES.iter().position(|s| *s == status) {
            self.counts[index] += 1;
        }
        self.count += 1;
        self.sum += elapsed;
        for (index, bound) in BUCKETS.iter().enumerate() {
            if elapsed <= *bound {
                self.buckets[index] += 1;
            }
        }
    }
    fn render(&self) -> String {
        let mut out = String::from(
            "# HELP http_requests_total HTTP requests handled by the metrics service.\n# TYPE http_requests_total counter\n",
        );
        for (index, status) in STATUSES.iter().enumerate() {
            out.push_str(&format!(
                "http_requests_total{{status=\"{status}\"}} {}\n",
                self.counts[index]
            ));
        }
        out.push_str("# HELP http_request_duration_seconds HTTP handler duration in seconds.\n# TYPE http_request_duration_seconds histogram\n");
        for (index, bound) in BUCKETS.iter().enumerate() {
            out.push_str(&format!(
                "http_request_duration_seconds_bucket{{le=\"{bound}\"}} {}\n",
                self.buckets[index]
            ));
        }
        out.push_str(&format!("http_request_duration_seconds_bucket{{le=\"+Inf\"}} {}\nhttp_request_duration_seconds_sum {}\nhttp_request_duration_seconds_count {}\n", self.count, self.sum, self.count));
        out
    }
}

async fn owner_healthy(endpoint: PathBuf) -> bool {
    let context = backend_client::RequestContext::new();
    if context.read_deadline(OWNER_DEADLINE).is_err() {
        return false;
    }
    let worker_context = context.clone();
    let task = tokio::task::spawn_blocking(move || {
        backend_client::Session::connect_with_context(endpoint, worker_context)
            .and_then(|mut session| session.health())
            .is_ok()
    });
    match tokio::time::timeout(OWNER_DEADLINE + Duration::from_millis(100), task).await {
        Ok(Ok(result)) => result,
        _ => {
            context.cancel(backend_client::RequestCancellationReason::Explicit);
            false
        }
    }
}

async fn handle(stream: TcpStream, endpoint: PathBuf, metrics: Arc<Mutex<Metrics>>) {
    let service = service_fn(move |request: Request<Incoming>| {
        let endpoint = endpoint.clone();
        let metrics = metrics.clone();
        async move {
            let started = Instant::now();
            let headers = request.headers();
            let has_body = headers.contains_key(TRANSFER_ENCODING)
                || headers
                    .get(CONTENT_LENGTH)
                    .is_some_and(|value| value.as_bytes() != b"0");
            let (status, body, content_type) = if has_body {
                (
                    StatusCode::BAD_REQUEST,
                    "request body not allowed\n".to_owned(),
                    "text/plain",
                )
            } else if request.method() != Method::GET {
                (
                    StatusCode::METHOD_NOT_ALLOWED,
                    "method not allowed\n".to_owned(),
                    "text/plain",
                )
            } else {
                match request.uri().path_and_query().map(|v| v.as_str()) {
                    Some("/metrics") => (
                        StatusCode::OK,
                        metrics.lock().unwrap_or_else(|e| e.into_inner()).render(),
                        "text/plain; version=0.0.4; charset=utf-8",
                    ),
                    Some("/health") if owner_healthy(endpoint).await => (
                        StatusCode::OK,
                        "owner responsive\n".to_owned(),
                        "text/plain",
                    ),
                    Some("/health") => (
                        StatusCode::SERVICE_UNAVAILABLE,
                        "owner unavailable\n".to_owned(),
                        "text/plain",
                    ),
                    _ => (
                        StatusCode::NOT_FOUND,
                        "not found\n".to_owned(),
                        "text/plain",
                    ),
                }
            };
            metrics
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record(status.as_u16(), started.elapsed().as_secs_f64());
            let mut response = Response::new(Body::from(body));
            *response.status_mut() = status;
            response.headers_mut().insert(
                CONTENT_TYPE,
                content_type.parse().expect("static content type"),
            );
            Ok::<_, std::convert::Infallible>(response)
        }
    });
    let mut builder = http1::Builder::new();
    builder
        .keep_alive(false)
        .max_buf_size(LIMIT)
        .header_read_timeout(DEADLINE)
        .timer(TokioTimer::new());
    let _ = tokio::time::timeout(
        DEADLINE,
        builder.serve_connection(TokioIo::new(stream), service),
    )
    .await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut endpoint = None;
    let mut bind: SocketAddr = "127.0.0.1:9464".parse()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--endpoint" => {
                endpoint = Some(PathBuf::from(args.next().ok_or("missing --endpoint")?))
            }
            "--bind" => bind = args.next().ok_or("missing --bind")?.parse()?,
            "--help" | "-h" => {
                println!("Usage: backend-metrics --endpoint LOCALD_SOCKET [--bind LOOPBACK:PORT]");
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }
    let endpoint = endpoint.ok_or("--endpoint is required")?;
    if !bind.ip().is_loopback() {
        return Err("--bind must be loopback".into());
    }
    let listener = TcpListener::bind(bind).await?;
    eprintln!("backend-metrics listening on {}", listener.local_addr()?);
    let metrics = Arc::new(Mutex::new(Metrics::default()));
    let permits = Arc::new(Semaphore::new(32));
    let mut tasks = JoinSet::new();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            result = &mut shutdown => { result?; break; }
            result = listener.accept() => {
                let (stream, _) = result?;
                if let Ok(permit) = permits.clone().try_acquire_owned() {
                    let endpoint = endpoint.clone(); let metrics = metrics.clone();
                    tasks.spawn(async move { let _permit = permit; handle(stream, endpoint, metrics).await; });
                }
            }
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    while tasks.join_next().await.is_some() {}
    Ok(())
}
async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}
