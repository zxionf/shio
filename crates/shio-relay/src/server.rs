//! HTTP 服务器与健康检查。

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;

use crate::config::Config;
use crate::db::Db;
use crate::error::{RelayError, Result};

/// 服务器共享状态。
#[derive(Clone)]
pub struct AppState {
    /// 配置。
    pub config: Arc<Config>,
    /// sqlite 连接。
    pub db: Db,
}

/// 构造路由。
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// 打开数据库并启动 HTTP 服务。
pub async fn serve(config: Config) -> Result<()> {
    let db = Db::open(&config.data)?;
    let listen = config.listen.clone();
    let data = config.data.clone();
    let state = AppState {
        config: Arc::new(config),
        db,
    };
    let app = router(state);
    let listener = TcpListener::bind(&listen)
        .await
        .map_err(|err| RelayError::Config(format!("bind {listen}: {err}")))?;
    let addr = listener.local_addr()?;
    tracing::info!(%addr, data = %data.display(), "relay listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(RelayError::Io)?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "failed to install Ctrl-C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(err) => {
                tracing::error!(%err, "failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            data: dir.path().join("relay.db"),
            ..Config::default()
        };
        let db = Db::open(&config.data).unwrap();
        let app = router(AppState {
            config: Arc::new(config),
            db,
        });
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ok");
    }
}
