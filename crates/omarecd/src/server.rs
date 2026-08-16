use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use omarec_protocol::{
    JsonLineConnection, PROTOCOL_VERSION, Request, RequestEnvelope, Response, ResponseEnvelope,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::time::interval;
use tracing::{debug, info, warn};

use crate::app::App;
use crate::runtime::{self, RuntimeError, ensure_private_dir, lstat, reject_symlink, verify_peer};

pub async fn run(socket_path: &Path, app: App) -> Result<(), ServerError> {
    prepare_socket(socket_path).await?;
    let listener = UnixListener::bind(socket_path).map_err(|source| ServerError::Bind {
        path: socket_path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o600)).map_err(|source| {
        ServerError::Permissions {
            path: socket_path.to_path_buf(),
            source,
        }
    })?;

    info!(socket = %socket_path.display(), "omarecd listening");
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result.map_err(ServerError::Accept)?;
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(error) = serve_connection(stream, app).await {
                        debug!(%error, "client connection closed with error");
                    }
                });
            }
            result = tokio::signal::ctrl_c() => {
                result.map_err(ServerError::Signal)?;
                app.request_shutdown();
                info!("shutdown requested");
                break;
            }
        }
    }
    drop(listener);
    match fs::remove_file(socket_path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => warn!(%error, path = %socket_path.display(), "failed to remove socket"),
    }
    Ok(())
}

async fn serve_connection(stream: UnixStream, app: App) -> Result<(), ServerError> {
    if let Err(error) = verify_peer(&stream) {
        let mut connection = JsonLineConnection::from_stream(stream);
        let (code, message) = match error {
            RuntimeError::PeerMismatch { peer, daemon } => (
                omarec_protocol::ERROR_UNAUTHORIZED.to_owned(),
                format!("client UID {peer} does not match daemon UID {daemon}"),
            ),
            other => (
                omarec_protocol::ERROR_UNAUTHORIZED.to_owned(),
                other.to_string(),
            ),
        };
        let _ = connection
            .send(&ResponseEnvelope::error(uuid::Uuid::nil(), code, message))
            .await;
        return Ok(());
    }
    let mut connection = JsonLineConnection::from_stream(stream);
    let Some(envelope) = connection
        .receive::<RequestEnvelope>()
        .await
        .map_err(ServerError::Transport)?
    else {
        return Ok(());
    };

    if envelope.protocol != PROTOCOL_VERSION {
        connection
            .send(&ResponseEnvelope::new(
                envelope.request_id,
                Response::Error {
                    code: "unsupported_protocol".to_owned(),
                    message: format!(
                        "client protocol {} is unsupported; daemon protocol is {PROTOCOL_VERSION}",
                        envelope.protocol
                    ),
                    retryable: false,
                    details: None,
                },
            ))
            .await
            .map_err(ServerError::Transport)?;
        return Ok(());
    }

    if matches!(envelope.request, Request::Watch) {
        return serve_watch(connection, envelope.request_id, app).await;
    }

    let response = app.respond(envelope.request_id, envelope.request).await;
    connection
        .send(&response)
        .await
        .map_err(ServerError::Transport)?;
    Ok(())
}

async fn serve_watch(
    mut connection: JsonLineConnection,
    request_id: uuid::Uuid,
    app: App,
) -> Result<(), ServerError> {
    let (mut subscription, snapshot) = app.watch_setup().await;
    let watermark = match &snapshot.event {
        omarec_protocol::Event::Snapshot { watermark, .. } => *watermark,
        _ => snapshot.sequence,
    };
    connection
        .send(&ResponseEnvelope::new(request_id, Response::Acknowledged))
        .await
        .map_err(ServerError::Transport)?;
    connection
        .send(&snapshot)
        .await
        .map_err(ServerError::Transport)?;
    let mut heartbeat = interval(Duration::from_secs(15));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            event = subscription.recv() => {
                match event {
                    Ok(event) if event.sequence <= watermark => {}
                    Ok(event) => connection.send(&event).await.map_err(ServerError::Transport)?,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        connection
                            .send(&app.lag_event(count))
                            .await
                            .map_err(ServerError::Transport)?;
                        connection
                            .send(&app.snapshot_event().await)
                            .await
                            .map_err(ServerError::Transport)?;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
            _ = heartbeat.tick() => {
                connection
                    .send(&app.heartbeat_event())
                    .await
                    .map_err(ServerError::Transport)?;
            }
        }
    }
}

async fn prepare_socket(path: &Path) -> Result<(), ServerError> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent).map_err(ServerError::Runtime)?;
    }
    match lstat(path) {
        Err(RuntimeError::Inspect { source, .. }) if source.kind() == ErrorKind::NotFound => {
            return Ok(());
        }
        Err(error) => return Err(ServerError::Runtime(error)),
        Ok(metadata) => {
            reject_symlink(path, &metadata).map_err(ServerError::Runtime)?;
        }
    }
    match UnixStream::connect(path).await {
        Ok(_) => Err(ServerError::AlreadyRunning(path.to_path_buf())),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionRefused | ErrorKind::NotFound
            ) =>
        {
            fs::remove_file(path).map_err(|source| ServerError::RemoveStale {
                path: path.to_path_buf(),
                source,
            })?;
            Ok(())
        }
        Err(source) => Err(ServerError::Inspect {
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("another omarecd is already listening on {0}")]
    AlreadyRunning(PathBuf),
    #[error("failed to inspect existing socket {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to remove stale socket {path}: {source}")]
    RemoveStale {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to bind {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to set permissions on {path}: {source}")]
    Permissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to accept a client: {0}")]
    Accept(std::io::Error),
    #[error("failed to install or receive shutdown signal: {0}")]
    Signal(std::io::Error),
    #[error(transparent)]
    Runtime(#[from] runtime::RuntimeError),
    #[error("protocol transport failed: {0}")]
    Transport(omarec_protocol::TransportError),
}
