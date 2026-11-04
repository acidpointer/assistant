//! HTTP lifecycle and bounded socket ownership. Reserved sockets accept only
//! after activation. Connection permits cover the entire TCP lifetime, including
//! incomplete requests; idle I/O expires. Graceful drain is followed by forced
//! socket closure, and the service joins its task before returning shutdown.

use crate::{WebConfig, http};
use assistant::{
    chat::{ChatError, ChatResult},
    engine::Cancellation,
    pipeline::BoxFuture,
    runtime::services::{PreparedService, RunningService, RuntimeContext, RuntimeServiceFactory},
};
use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    task::JoinHandle,
    time::{Instant, Sleep},
};

#[derive(Debug)]
pub(crate) struct WebFactory(pub(crate) WebConfig);
impl RuntimeServiceFactory for WebFactory {
    fn name(&self) -> &'static str {
        "web"
    }
    fn prepare(&self) -> BoxFuture<'_, ChatResult<Box<dyn PreparedService>>> {
        Box::pin(async {
            let listener = TcpListener::bind((self.0.host, self.0.port))
                .await
                .map_err(|error| ChatError::Extension {
                    name: "web.bind".into(),
                    message: error.to_string(),
                })?;
            Ok(Box::new(BoundWeb { listener }) as Box<dyn PreparedService>)
        })
    }
}
#[derive(Debug)]
struct BoundWeb {
    listener: TcpListener,
}
impl PreparedService for BoundWeb {
    fn activate(
        self: Box<Self>,
        context: RuntimeContext,
    ) -> BoxFuture<'static, ChatResult<Box<dyn RunningService>>> {
        Box::pin(async move {
            let address = self
                .listener
                .local_addr()
                .map_err(|error| ChatError::Extension {
                    name: "web.address".into(),
                    message: error.to_string(),
                })?;
            let quiesce = Cancellation::default();
            let stop = Cancellation::default();
            let force = Cancellation::default();
            let router = http::router(context.assistant, quiesce.clone(), stop.clone());
            let listener = BoundedListener {
                listener: self.listener,
                quiesce: quiesce.clone(),
                force: force.clone(),
                permits: Arc::new(Semaphore::new(64)),
            };
            let (completion, finished) = watch::channel(None);
            let task_stop = stop.clone();
            let task = tokio::spawn(async move {
                let shutdown = task_stop.clone();
                let serve = axum::serve(
                    listener,
                    router.into_make_service_with_connect_info::<LocalAddress>(),
                )
                .with_graceful_shutdown(async move { shutdown.cancelled().await });
                let serve = std::future::IntoFuture::into_future(serve);
                tokio::pin!(serve);
                let result = tokio::select! {
                    result = &mut serve => result,
                    () = task_stop.cancelled() => {
                        if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut serve).await {
                            result
                        } else { force.cancel(); serve.await }
                    }
                };
                let outcome = result.map_err(|error| error.to_string());
                completion.send_replace(Some(outcome));
            });
            eprintln!("WebUI ready at http://{address}");
            Ok(Box::new(WebService {
                quiesce,
                stop,
                finished,
                task: Some(task),
            }) as Box<dyn RunningService>)
        })
    }
}

#[derive(Debug)]
struct WebService {
    quiesce: Cancellation,
    stop: Cancellation,
    finished: watch::Receiver<Option<Result<(), String>>>,
    task: Option<JoinHandle<()>>,
}
impl RunningService for WebService {
    fn quiesce(&self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async {
            self.quiesce.cancel();
            Ok(())
        })
    }
    fn terminated(&self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async {
            let mut finished = self.finished.clone();
            loop {
                if let Some(result) = finished.borrow_and_update().clone() {
                    return result.map_err(|message| ChatError::Extension {
                        name: "web".into(),
                        message,
                    });
                }
                finished.changed().await.map_err(|_| ChatError::Closed)?;
            }
        })
    }
    fn shutdown(&mut self) -> BoxFuture<'_, ChatResult<()>> {
        Box::pin(async {
            self.quiesce.cancel();
            self.stop.cancel();
            if let Some(task) = self.task.take() {
                task.await
                    .map_err(|_| ChatError::Configuration("web task terminated".into()))?;
            }
            match self.finished.borrow().clone() {
                Some(Ok(())) => Ok(()),
                Some(Err(message)) => Err(ChatError::Extension {
                    name: "web".into(),
                    message,
                }),
                None => Err(ChatError::Closed),
            }
        })
    }
}
impl Drop for WebService {
    fn drop(&mut self) {
        self.quiesce.cancel();
        self.stop.cancel();
    }
}

/// Actual destination address, independent of untrusted Host/forwarded headers.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalAddress(pub(crate) Option<SocketAddr>);
impl Connected<IncomingStream<'_, BoundedListener>> for LocalAddress {
    fn connect_info(stream: IncomingStream<'_, BoundedListener>) -> Self {
        Self(stream.io().stream.local_addr().ok())
    }
}

#[derive(Debug)]
struct BoundedListener {
    listener: TcpListener,
    quiesce: Cancellation,
    force: Cancellation,
    permits: Arc<Semaphore>,
}
impl Listener for BoundedListener {
    type Io = Connection;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Connection, SocketAddr) {
        loop {
            let accepted = async {
                let permit = self.permits.clone().acquire_owned().await.ok()?;
                let (stream, address) = self.listener.accept().await.ok()?;
                Some((stream, address, permit))
            };
            let result = tokio::select! {
                () = self.quiesce.cancelled() => std::future::pending().await,
                result = accepted => result,
            };
            if let Some((stream, address, permit)) = result {
                let force = self.force.clone();
                return (
                    Connection {
                        stream,
                        _permit: permit,
                        idle: Box::pin(tokio::time::sleep(Duration::from_secs(30))),
                        force: Box::pin(async move { force.cancelled().await }),
                    },
                    address,
                );
            }
            tracing::warn!("HTTP accept failed; retrying");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}

struct Connection {
    stream: TcpStream,
    _permit: OwnedSemaphorePermit,
    idle: Pin<Box<Sleep>>,
    force: Pin<Box<dyn Future<Output = ()> + Send>>,
}
impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}
impl Connection {
    fn expired(&mut self, cx: &mut Context<'_>) -> bool {
        self.force.as_mut().poll(cx).is_ready() || self.idle.as_mut().poll(cx).is_ready()
    }
    fn touch(&mut self) {
        self.idle
            .as_mut()
            .reset(Instant::now() + Duration::from_secs(30));
    }
}
impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.expired(cx) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        let before = buf.filled().len();
        let result = Pin::new(&mut this.stream).poll_read(cx, buf);
        if buf.filled().len() > before {
            this.touch();
        }
        result
    }
}
impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.expired(cx) {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        let result = Pin::new(&mut this.stream).poll_write(cx, buf);
        if matches!(result, Poll::Ready(Ok(n)) if n > 0) {
            this.touch();
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}
