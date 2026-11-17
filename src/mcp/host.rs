//! The HTTP host of the MCP server: one loopback port, one bearer token.
//!
//! The server lives on a thread of its own with a small runtime of its own,
//! the way the notification permission check does, so the interface thread
//! and the account workers never serve it. The token is a hex string kept in
//! the state directory with 0600 permissions; an MCP client answers with it
//! as `Authorization: Bearer <token>`.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use super::Hub;
use super::tools::ZapServer;
use crate::paths::AppDirs;

/// The state file that holds the bearer token.
pub const TOKEN_FILE: &str = "mcp-token";

/// How the MCP server is reached: the port it binds on 127.0.0.1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub port: u16,
}

/// The token file's path, for the user who copies it into an MCP client.
pub fn token_file(dirs: &AppDirs) -> std::path::PathBuf {
    dirs.state.join(TOKEN_FILE)
}

/// Reads the stored token, or makes one and stores it. A token survives
/// restarts so an MCP client configured once stays configured.
fn load_or_create_token(dirs: &AppDirs) -> String {
    let path = token_file(dirs);
    if let Ok(stored) = std::fs::read_to_string(&path) {
        let token = stored.trim();
        if token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return token.to_owned();
        }
        log::warn!("the MCP token file is unreadable, replacing it");
        let _ = std::fs::remove_file(&path);
    }
    let bytes: [u8; 32] = rand::random();
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    };
    if let Err(error) = write() {
        if path.exists()
            && let Ok(stored) = std::fs::read_to_string(&path)
            && stored.trim().len() == 64
        {
            return stored.trim().to_owned();
        }
        log::warn!("could not store the MCP token: {error}");
    }
    token
}

/// Starts the MCP server thread. It never touches the interface: when the
/// process ends, the thread ends with it.
pub fn spawn(dirs: AppDirs, hub: Arc<Hub>, options: Options) {
    let token = load_or_create_token(&dirs);
    log::info!(
        "MCP server starting on 127.0.0.1:{}; its token is at {}",
        options.port,
        token_file(&dirs).display()
    );
    let _ = std::thread::Builder::new()
        .name("zapfast-mcp".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("zapfast-mcp-runtime")
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    log::error!("could not start the MCP server runtime: {error}");
                    return;
                }
            };
            runtime.block_on(serve(&hub, &token, options.port));
            runtime.shutdown_timeout(Duration::from_secs(2));
        });
}

async fn serve(hub: &Arc<Hub>, token: &str, port: u16) {
    let service = StreamableHttpService::new(
        {
            let hub = hub.clone();
            move || Ok(ZapServer::new(hub.clone()))
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let expected = Arc::new(token.to_owned());
    let router = Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn_with_state(expected, auth));
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    match tokio::net::TcpListener::bind(address).await {
        Ok(listener) => {
            log::info!("MCP server listening on http://{address}/mcp");
            let _ = axum::serve(listener, router).await;
        }
        Err(error) => {
            log::error!("could not bind the MCP server on {address}: {error}");
        }
    }
}

/// The only gate in front of the tools: the request carries the bearer token.
/// A plain comparison is enough over the loopback, where no timing attacker
/// shares the address space with the answer.
async fn auth(State(expected): State<Arc<String>>, request: Request, next: Next) -> Response {
    let allowed = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {}", *expected));
    if allowed {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "unauthorized\n").into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs_with_token() -> (AppDirs, tempfile::TempDir, String) {
        let root = tempfile::tempdir().expect("temp dir");
        let dirs = AppDirs::under(root.path());
        dirs.ensure().expect("state dir");
        let token = load_or_create_token(&dirs);
        (dirs, root, token)
    }

    #[test]
    fn token_is_generated_once_and_read_back() {
        let (dirs, _root, token) = dirs_with_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        // A second run reads the same token instead of replacing it.
        assert_eq!(load_or_create_token(&dirs), token);
        // Its file is private to the user.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(token_file(&dirs))
                .expect("token file")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_broken_token_file_is_replaced() {
        let (dirs, _root, _token) = dirs_with_token();
        std::fs::write(token_file(&dirs), "not a token").expect("write");
        let replaced = load_or_create_token(&dirs);
        assert_eq!(replaced.len(), 64);
        assert_eq!(load_or_create_token(&dirs), replaced);
    }
}
