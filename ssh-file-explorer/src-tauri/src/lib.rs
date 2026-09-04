use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;
use russh::*;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(target_os = "windows")]
#[allow(deprecated)]
use raw_window_handle::HasRawWindowHandle;
#[cfg(target_os = "windows")]
use raw_window_handle::RawWindowHandle;

static CANCELLED_TRANSFERS: std::sync::LazyLock<std::sync::Mutex<HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

fn is_transfer_cancelled(id: &str) -> bool {
    CANCELLED_TRANSFERS.lock().unwrap().contains(id)
}

#[cfg(windows)]
mod virtual_drag;

pub(crate) struct SshState {
    pub(crate) session: Option<russh::client::Handle<ClientHandler>>,
    pub(crate) sftp: Option<russh_sftp::client::SftpSession>,
}

pub(crate) struct ClientHandler {
    captured_key: Arc<std::sync::Mutex<Option<CapturedHostKey>>>,
}

#[derive(Clone)]
struct CapturedHostKey {
    algorithm: String,
    fingerprint: String,
}

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        // Record the presented host key so the connect command can verify it
        // against the known-hosts store *before* any credentials are sent. We
        // accept at the transport layer (Ok(true)); the caller tears the session
        // down without authenticating if the key is unknown or has changed.
        let captured = CapturedHostKey {
            algorithm: server_public_key.algorithm().to_string(),
            fingerprint: server_public_key.fingerprint(Default::default()).to_string(),
        };
        if let Ok(mut slot) = self.captured_key.lock() {
            *slot = Some(captured);
        }
        async { Ok(true) }
    }
}

const KEYRING_SERVICE: &str = "ssh-file-explorer";

#[derive(Serialize, Deserialize, Clone)]
struct SavedConnection {
    id: String,
    label: String,
    host: String,
    port: u16,
    username: String,
    #[serde(default)]
    has_password: bool,
    #[serde(skip_serializing, default)]
    password: Option<String>,
    #[serde(default = "default_auth_method")]
    auth_method: String,
    #[serde(default)]
    key_path: Option<String>,
}

fn default_auth_method() -> String {
    "password".to_string()
}

#[derive(Serialize)]
struct FileEntry {
    name: String,
    is_dir: bool,
    size: u64,
    modified: String,
    permissions: String,
}

#[derive(Clone, Serialize)]
struct TransferProgress {
    id: String,
    transfer_type: String,
    file_name: String,
    bytes_transferred: u64,
    total_bytes: u64,
}

pub(crate) type SshSession = Arc<Mutex<SshState>>;

fn connections_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get config dir: {}", e))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create config dir: {}", e))?;
    Ok(dir.join("connections.json"))
}

fn read_connections(app: &AppHandle) -> Result<Vec<SavedConnection>, String> {
    let path = connections_path(app)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path).map_err(|e| format!("Failed to read connections: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to parse connections: {}", e))
}

fn write_connections(app: &AppHandle, connections: &[SavedConnection]) -> Result<(), String> {
    let path = connections_path(app)?;
    let data = serde_json::to_string_pretty(connections).map_err(|e| format!("Failed to serialize: {}", e))?;
    std::fs::write(&path, data).map_err(|e| format!("Failed to write connections: {}", e))
}

#[derive(Serialize, Deserialize, Clone)]
struct KnownHost {
    algorithm: String,
    fingerprint: String,
}

fn known_hosts_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Failed to get config dir: {}", e))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create config dir: {}", e))?;
    Ok(dir.join("known_hosts.json"))
}

fn read_known_hosts(app: &AppHandle) -> Result<std::collections::HashMap<String, KnownHost>, String> {
    let path = known_hosts_path(app)?;
    if !path.exists() {
        return Ok(std::collections::HashMap::new());
    }
    let data = std::fs::read_to_string(&path).map_err(|e| format!("Failed to read known_hosts: {}", e))?;
    serde_json::from_str(&data).map_err(|e| format!("Failed to parse known_hosts: {}", e))
}

fn write_known_hosts(app: &AppHandle, hosts: &std::collections::HashMap<String, KnownHost>) -> Result<(), String> {
    let path = known_hosts_path(app)?;
    let data = serde_json::to_string_pretty(hosts).map_err(|e| format!("Failed to serialize: {}", e))?;
    std::fs::write(&path, data).map_err(|e| format!("Failed to write known_hosts: {}", e))
}

/// Verify a captured host key against the known-hosts store. Returns a machine-
/// readable error the frontend parses: `HOST_KEY_UNKNOWN|algo|fp` for a host we
/// have never trusted, or `HOST_KEY_MISMATCH|algo|new_fp|old_fp` when the key
/// differs from the one we stored (a possible man-in-the-middle).
fn verify_host_key(app: &AppHandle, host: &str, port: u16, captured: &CapturedHostKey) -> Result<(), String> {
    let hosts = read_known_hosts(app)?;
    let key = format!("{}:{}", host, port);
    match hosts.get(&key) {
        None => Err(format!("HOST_KEY_UNKNOWN|{}|{}", captured.algorithm, captured.fingerprint)),
        Some(existing) if existing.fingerprint == captured.fingerprint => Ok(()),
        Some(existing) => Err(format!(
            "HOST_KEY_MISMATCH|{}|{}|{}",
            captured.algorithm, captured.fingerprint, existing.fingerprint
        )),
    }
}

#[tauri::command]
async fn trust_host_key(
    app: AppHandle,
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
) -> Result<(), String> {
    let mut hosts = read_known_hosts(&app)?;
    hosts.insert(format!("{}:{}", host, port), KnownHost { algorithm, fingerprint });
    write_known_hosts(&app, &hosts)
}

#[tauri::command]
async fn get_saved_connections(app: AppHandle) -> Result<Vec<SavedConnection>, String> {
    let mut connections = read_connections(&app)?;
    for conn in &mut connections {
        conn.has_password = keyring::Entry::new(KEYRING_SERVICE, &conn.id)
            .and_then(|e| e.get_password())
            .is_ok();
    }
    Ok(connections)
}

#[tauri::command]
async fn save_connection(
    app: AppHandle,
    id: String,
    label: String,
    host: String,
    port: u16,
    username: String,
    password: Option<String>,
    auth_method: Option<String>,
    key_path: Option<String>,
) -> Result<(), String> {
    let mut connections = read_connections(&app)?;

    let resolved_id = if let Some(existing) = connections.iter().find(|c| {
        c.id == id || (c.host == host && c.port == port && c.username == username)
    }) {
        existing.id.clone()
    } else {
        id.clone()
    };

    if let Some(pw) = &password {
        keyring::Entry::new(KEYRING_SERVICE, &resolved_id)
            .and_then(|e| e.set_password(pw))
            .map_err(|e| format!("Failed to store password in keychain: {}", e))?;
    } else {
        let _ = keyring::Entry::new(KEYRING_SERVICE, &resolved_id)
            .and_then(|e| e.delete_credential());
    }

    let conn = SavedConnection {
        id: resolved_id.clone(),
        label,
        host,
        port,
        username,
        has_password: password.is_some(),
        password: None,
        auth_method: auth_method.unwrap_or_else(|| "password".to_string()),
        key_path,
    };

    if let Some(entry) = connections.iter_mut().find(|c| c.id == resolved_id) {
        *entry = conn;
    } else {
        connections.push(conn);
    }

    write_connections(&app, &connections)
}

#[tauri::command]
async fn get_connection_password(id: String) -> Result<String, String> {
    keyring::Entry::new(KEYRING_SERVICE, &id)
        .and_then(|e| e.get_password())
        .map_err(|e| format!("Failed to retrieve password: {}", e))
}

#[tauri::command]
async fn delete_connection(app: AppHandle, id: String) -> Result<(), String> {
    let _ = keyring::Entry::new(KEYRING_SERVICE, &id)
        .and_then(|e| e.delete_credential());
    let mut connections = read_connections(&app)?;
    connections.retain(|c| c.id != id);
    write_connections(&app, &connections)
}

#[derive(Serialize)]
struct SshKeyInfo {
    path: String,
    name: String,
    key_type: String,
    encrypted: bool,
}

#[tauri::command]
async fn discover_ssh_keys() -> Result<Vec<SshKeyInfo>, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let ssh_dir = home.join(".ssh");

    if !ssh_dir.exists() {
        return Ok(Vec::new());
    }

    let mut keys = Vec::new();

    let entries = std::fs::read_dir(&ssh_dir).map_err(|e| format!("Failed to read .ssh dir: {}", e))?;
    for entry in entries.flatten() {
        let key_path = entry.path();
        if !key_path.is_file() {
            continue;
        }

        let name = key_path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if name.ends_with(".pub") || name == "known_hosts" || name == "authorized_keys" || name == "config" {
            continue;
        }

        let content = match std::fs::read_to_string(&key_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !content.contains("PRIVATE KEY") {
            continue;
        }

        let encrypted = content.contains("ENCRYPTED");
        let key_type = if content.contains("ED25519") {
            "ED25519"
        } else if content.contains("ECDSA") {
            "ECDSA"
        } else if content.contains("DSA PRIVATE") && !content.contains("ECDSA") {
            "DSA"
        } else if content.contains("RSA") {
            "RSA"
        } else if content.contains("OPENSSH PRIVATE KEY") {
            "OpenSSH"
        } else {
            "Unknown"
        }.to_string();

        keys.push(SshKeyInfo {
            path: key_path.to_string_lossy().into_owned(),
            name,
            key_type,
            encrypted,
        });
    }

    Ok(keys)
}

#[tauri::command]
async fn ssh_connect(
    app: AppHandle,
    host: String,
    port: u16,
    username: String,
    password: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let config = Arc::new(russh::client::Config::default());
    let captured_key = Arc::new(std::sync::Mutex::new(None));
    let handler = ClientHandler { captured_key: captured_key.clone() };

    let session = russh::client::connect(config, (host.as_str(), port), handler)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    // Verify the host key before authenticating. On an unknown or changed key
    // this returns early and the session is dropped, so no credentials are sent.
    let captured = captured_key
        .lock()
        .unwrap()
        .clone()
        .ok_or("Server did not present a host key")?;
    verify_host_key(&app, &host, port, &captured)?;

    let mut session = session;
    let auth_ok = session
        .authenticate_password(&username, &password)
        .await
        .map_err(|e| format!("Auth error: {}", e))?;

    if !auth_ok.success() {
        return Err("Authentication failed: invalid credentials".into());
    }

    let sftp_channel = session.channel_open_session().await.map_err(|e| format!("SFTP channel error: {}", e))?;
    sftp_channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
    let sftp = russh_sftp::client::SftpSession::new(sftp_channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?;

    let mut s = state.lock().await;
    s.session = Some(session);
    s.sftp = Some(sftp);
    Ok(())
}

#[tauri::command]
async fn ssh_connect_key(
    app: AppHandle,
    host: String,
    port: u16,
    username: String,
    key_path: String,
    passphrase: Option<String>,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let key_data = std::fs::read_to_string(&key_path)
        .map_err(|e| format!("Failed to read key file: {}", e))?;

    let key_pair = russh::keys::decode_secret_key(&key_data, passphrase.as_deref())
        .map_err(|e| format!("Failed to decode key: {}", e))?;

    let config = Arc::new(russh::client::Config::default());
    let captured_key = Arc::new(std::sync::Mutex::new(None));
    let handler = ClientHandler { captured_key: captured_key.clone() };

    let session = russh::client::connect(config, (host.as_str(), port), handler)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

    // Verify the host key before authenticating (see ssh_connect).
    let captured = captured_key
        .lock()
        .unwrap()
        .clone()
        .ok_or("Server did not present a host key")?;
    verify_host_key(&app, &host, port, &captured)?;

    let mut session = session;
    let key_with_alg = russh::keys::PrivateKeyWithHashAlg::new(
        Arc::new(key_pair),
        None,
    );

    let auth_ok = session
        .authenticate_publickey(&username, key_with_alg)
        .await
        .map_err(|e| format!("Auth error: {}", e))?;

    if !auth_ok.success() {
        return Err("Authentication failed: key not accepted by server".into());
    }

    let sftp_channel = session.channel_open_session().await.map_err(|e| format!("SFTP channel error: {}", e))?;
    sftp_channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
    let sftp = russh_sftp::client::SftpSession::new(sftp_channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?;

    let mut s = state.lock().await;
    s.session = Some(session);
    s.sftp = Some(sftp);
    Ok(())
}

#[tauri::command]
async fn ssh_disconnect(state: State<'_, SshSession>) -> Result<(), String> {
    let mut s = state.lock().await;
    s.sftp.take();
    if let Some(session) = s.session.take() {
        let _ = session
            .disconnect(Disconnect::ByApplication, "User disconnected", "")
            .await;
    }
    Ok(())
}

async fn exec_ssh(session: &russh::client::Handle<ClientHandler>, cmd: &str) -> Result<String, String> {
    let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
    channel.exec(true, cmd.as_bytes()).await.map_err(|e| format!("Exec error: {}", e))?;

    let mut output = Vec::new();
    let mut stream = channel.into_stream();

    use tokio::io::AsyncReadExt;
    let mut buf = vec![0u8; 65536];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }

    Ok(String::from_utf8_lossy(&output).into_owned())
}

async fn sudo_exec_ssh(session: &russh::client::Handle<ClientHandler>, password: &str, cmd: &str) -> Result<String, String> {
    let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
    let escaped_cmd = cmd.replace("'", "'\"'\"'");
    let escaped_pw = password.replace("'", "'\\''");
    let sudo_cmd = format!(
        "printf '%s\\n' '{}' | sudo -S sh -c '{}' 2>&1; echo \"SUDO_EXIT:$?\"",
        escaped_pw,
        escaped_cmd
    );
    channel.exec(true, sudo_cmd.as_bytes()).await.map_err(|e| format!("Exec error: {}", e))?;

    let mut stream = channel.into_stream();
    use tokio::io::AsyncReadExt;
    let mut output = Vec::new();
    let mut buf = vec![0u8; 65536];
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    }).await;

    if read_result.is_err() {
        return Err("Permission Denied: Sudo Required".into());
    }

    let text = String::from_utf8_lossy(&output).into_owned();
    let lower = text.to_lowercase();
    if lower.contains("sorry, try again")
        || lower.contains("is not in the sudoers file")
        || lower.contains("incorrect password")
        || lower.contains("authentication failure")
    {
        return Err("Permission Denied: Sudo Required".into());
    }

    let mut exit_code = 0;
    let filtered: String = text.lines()
        .filter_map(|line| {
            if let Some(code) = line.strip_prefix("SUDO_EXIT:") {
                exit_code = code.trim().parse::<i32>().unwrap_or(1);
                return None;
            }
            if let Some(idx) = line.find("[sudo] password for") {
                let after = &line[idx..];
                if let Some(colon_pos) = after.find(": ") {
                    let data_start = idx + colon_pos + 2;
                    let remaining = &line[data_start..];
                    if remaining.trim().is_empty() {
                        return None;
                    }
                    return Some(remaining.to_string());
                }
                return None;
            }
            Some(line.to_string())
        })
        .collect::<Vec<_>>()
        .join("\n");

    if exit_code != 0 && !filtered.trim().is_empty() {
        return Err(filtered.trim().to_string());
    }
    if exit_code != 0 {
        return Err("Permission Denied: Sudo Required".into());
    }
    Ok(filtered)
}

#[tauri::command]
async fn check_writable(path: String, state: State<'_, SshSession>) -> Result<bool, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let probe = format!("{}/.sftp_write_probe", path.trim_end_matches('/'));
    let cmd = format!(
        "touch {} 2>/dev/null && rm -f {} && echo 'y' || echo 'n'",
        shell_escape(&probe),
        shell_escape(&probe)
    );
    let output = exec_ssh(session, &cmd).await?;
    Ok(output.trim().ends_with("y"))
}

#[tauri::command]
async fn check_sudo_writable(path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<bool, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let probe = format!("{}/.sftp_sudo_probe", path.trim_end_matches('/'));
    let escaped_pw = sudo_password.replace("'", "'\\''");
    let cmd = format!(
        "printf '%s\\n' '{}' | sudo -S sh -c 'touch {} && rm -f {}' 2>/dev/null && echo 'y' || echo 'n'",
        escaped_pw,
        shell_escape(&probe),
        shell_escape(&probe)
    );
    let output = exec_ssh(session, &cmd).await?;
    Ok(output.trim().ends_with("y"))
}

#[tauri::command]
async fn list_directory(path: String, state: State<'_, SshSession>) -> Result<Vec<FileEntry>, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;

    let cmd = format!(
        "LC_ALL=C find {} -maxdepth 1 -mindepth 1 -exec stat -c '%n|%F|%s|%Y|%a' {{}} + 2>/dev/null; true",
        shell_escape(&path)
    );

    let text = exec_ssh(session, &cmd).await?;
    let mut entries = Vec::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(5, '|').collect();
        if parts.len() != 5 {
            continue;
        }

        let full_path = parts[0];
        let name = full_path
            .rsplit('/')
            .next()
            .unwrap_or(full_path)
            .to_string();

        if name == "." || name == ".." {
            continue;
        }

        let is_dir = parts[1] == "directory";
        let size: u64 = parts[2].parse().unwrap_or(0);

        let timestamp: i64 = parts[3].parse().unwrap_or(0);
        let modified = format_timestamp(timestamp);

        let mode = parts[4].to_string();
        let permissions = format_permissions(&mode, is_dir);

        entries.push(FileEntry {
            name,
            is_dir,
            size,
            modified,
            permissions,
        });
    }

    if entries.is_empty() {
        let check = format!("ls {} 2>&1", shell_escape(&path));
        let check_output = exec_ssh(session, &check).await.unwrap_or_default();
        if check_output.contains("Permission denied") {
            return Err("Permission Denied: Sudo Required".to_string());
        }
    }

    Ok(entries)
}

#[tauri::command]
async fn sudo_list_directory(path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<Vec<FileEntry>, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;

    let cmd = format!(
        "LC_ALL=C find {} -maxdepth 1 -mindepth 1 -exec stat -c '%n|%F|%s|%Y|%a' {{}} + 2>/dev/null; true",
        shell_escape(&path)
    );

    let text = sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    let mut entries = Vec::new();

    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(5, '|').collect();
        if parts.len() != 5 {
            continue;
        }

        let full_path = parts[0];
        let name = full_path
            .rsplit('/')
            .next()
            .unwrap_or(full_path)
            .to_string();

        if name == "." || name == ".." {
            continue;
        }

        let is_dir = parts[1] == "directory";
        let size: u64 = parts[2].parse().unwrap_or(0);

        let timestamp: i64 = parts[3].parse().unwrap_or(0);
        let modified = format_timestamp(timestamp);

        let mode = parts[4].to_string();
        let permissions = format_permissions(&mode, is_dir);

        entries.push(FileEntry {
            name,
            is_dir,
            size,
            modified,
            permissions,
        });
    }

    Ok(entries)
}

#[tauri::command]
async fn read_file(path: String, state: State<'_, SshSession>) -> Result<String, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("cat {} 2>&1", shell_escape(&path));
    let output = exec_ssh(session, &cmd).await?;
    if output.starts_with("cat:") && (output.contains("Permission denied") || output.contains("No such file")) {
        return Err(output.trim().to_string());
    }
    Ok(output)
}

#[tauri::command]
async fn write_file(path: String, content: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;

    let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
    let cmd = format!("sh -c 'cat > {}' 2>&1", shell_escape(&path));
    channel.exec(true, cmd.as_bytes()).await.map_err(|e| format!("Exec error: {}", e))?;

    let mut stream = channel.into_stream();
    use tokio::io::{AsyncWriteExt, AsyncReadExt};
    stream.write_all(content.as_bytes()).await.map_err(|e| format!("Write error: {}", e))?;
    stream.shutdown().await.map_err(|e| format!("Close error: {}", e))?;

    let mut err_output = Vec::new();
    let mut buf = vec![0u8; 4096];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => err_output.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    let err_text = String::from_utf8_lossy(&err_output).trim().to_string();
    if !err_text.is_empty() {
        return Err(err_text);
    }

    Ok(())
}

#[tauri::command]
async fn create_file(path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let parent = match path.rfind('/') {
        Some(pos) if pos > 0 => &path[..pos],
        _ => "/",
    };
    let cmd = format!("mkdir -p {} 2>&1 && touch {} 2>&1", shell_escape(parent), shell_escape(&path));
    let output = exec_ssh(session, &cmd).await?;
    if !output.trim().is_empty() {
        return Err(output.trim().to_string());
    }
    Ok(())
}

#[tauri::command]
async fn create_directory(path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("mkdir -p {} 2>&1", shell_escape(&path));
    let output = exec_ssh(session, &cmd).await?;
    if !output.trim().is_empty() {
        return Err(output.trim().to_string());
    }
    Ok(())
}

#[tauri::command]
async fn delete_file(path: String, is_dir: bool, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = if is_dir {
        format!("rm -rf {} 2>&1", shell_escape(&path))
    } else {
        format!("rm -f {} 2>&1", shell_escape(&path))
    };
    let output = exec_ssh(session, &cmd).await?;
    if !output.trim().is_empty() {
        return Err(output.trim().to_string());
    }
    Ok(())
}

#[tauri::command]
async fn rename_file(old_path: String, new_path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("mv {} {} 2>&1", shell_escape(&old_path), shell_escape(&new_path));
    let output = exec_ssh(session, &cmd).await?;
    if !output.trim().is_empty() {
        return Err(output.trim().to_string());
    }
    Ok(())
}

#[tauri::command]
async fn sudo_rename_file(old_path: String, new_path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("mv {} {}", shell_escape(&old_path), shell_escape(&new_path));
    sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn sudo_read_file(path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<String, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("cat {}", shell_escape(&path));
    sudo_exec_ssh(session, &sudo_password, &cmd).await
}

#[tauri::command]
async fn sudo_write_file(path: String, content: String, sudo_password: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let escaped_content = content.replace("'", "'\\''");
    let cmd = format!("printf '%s' '{}' > {}", escaped_content, shell_escape(&path));
    sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn sudo_create_file(path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let parent = match path.rfind('/') {
        Some(pos) if pos > 0 => &path[..pos],
        _ => "/",
    };
    let cmd = format!("mkdir -p {} && touch {}", shell_escape(parent), shell_escape(&path));
    sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn sudo_create_directory(path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("mkdir -p {}", shell_escape(&path));
    sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn sudo_delete_file(path: String, is_dir: bool, sudo_password: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = if is_dir {
        format!("rm -rf {}", shell_escape(&path))
    } else {
        format!("rm -f {}", shell_escape(&path))
    };
    sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn copy_path(
    app: AppHandle,
    transfer_id: String,
    src: String,
    dest: String,
    is_dir: bool,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let file_name = src.rsplit('/').next().unwrap_or(&src).to_string();

    let size_cmd = if is_dir {
        format!("du -sb {} 2>/dev/null | cut -f1", shell_escape(&src))
    } else {
        format!("stat -c '%s' {} 2>/dev/null", shell_escape(&src))
    };
    let total_bytes: u64 = exec_ssh(session, &size_cmd).await
        .unwrap_or_default().trim().parse().unwrap_or(0);

    let _ = app.emit("transfer-progress", TransferProgress {
        id: transfer_id.clone(), transfer_type: "copy".into(),
        file_name: file_name.clone(), bytes_transferred: 0, total_bytes,
    });

    let marker = format!("/tmp/.cp_done_{}", transfer_id);
    let cp_cmd = if is_dir {
        format!("(cp -r {} {} ; echo $? > {}) >/dev/null 2>&1 &", shell_escape(&src), shell_escape(&dest), shell_escape(&marker))
    } else {
        format!("(cp {} {} ; echo $? > {}) >/dev/null 2>&1 &", shell_escape(&src), shell_escape(&dest), shell_escape(&marker))
    };
    exec_ssh(session, &cp_cmd).await?;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let check = exec_ssh(session, &format!("cat {} 2>/dev/null", shell_escape(&marker))).await.unwrap_or_default();
        if !check.trim().is_empty() {
            let _ = exec_ssh(session, &format!("rm -f {}", shell_escape(&marker))).await;
            let exit_code: i32 = check.trim().parse().unwrap_or(-1);
            if exit_code != 0 {
                return Err("Permission Denied: Sudo Required".to_string());
            }
            break;
        }
        let poll_cmd = if is_dir {
            format!("du -sb {} 2>/dev/null | cut -f1", shell_escape(&dest))
        } else {
            format!("stat -c '%s' {} 2>/dev/null", shell_escape(&dest))
        };
        if let Ok(out) = exec_ssh(session, &poll_cmd).await {
            let current: u64 = out.trim().parse().unwrap_or(0);
            let _ = app.emit("transfer-progress", TransferProgress {
                id: transfer_id.clone(), transfer_type: "copy".into(),
                file_name: file_name.clone(), bytes_transferred: current, total_bytes,
            });
        }
    }

    let _ = app.emit("transfer-progress", TransferProgress {
        id: transfer_id.clone(), transfer_type: "copy".into(),
        file_name, bytes_transferred: total_bytes, total_bytes,
    });
    Ok(())
}

#[tauri::command]
async fn sudo_copy_path(
    app: AppHandle,
    transfer_id: String,
    src: String,
    dest: String,
    is_dir: bool,
    sudo_password: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let file_name = src.rsplit('/').next().unwrap_or(&src).to_string();

    let size_cmd = if is_dir {
        format!("du -sb {}", shell_escape(&src))
    } else {
        format!("stat -c '%s' {}", shell_escape(&src))
    };
    let size_out = sudo_exec_ssh(session, &sudo_password, &size_cmd).await.unwrap_or_default();
    let total_bytes: u64 = size_out.split_whitespace().next()
        .and_then(|s| s.parse().ok()).unwrap_or(0);

    let _ = app.emit("transfer-progress", TransferProgress {
        id: transfer_id.clone(), transfer_type: "copy".into(),
        file_name: file_name.clone(), bytes_transferred: 0, total_bytes,
    });

    let marker = format!("/tmp/.cp_done_{}", transfer_id);
    let cp_cmd = if is_dir {
        format!("cp -r {} {}", shell_escape(&src), shell_escape(&dest))
    } else {
        format!("cp {} {}", shell_escape(&src), shell_escape(&dest))
    };
    let bg_cmd = format!("({} ; echo $? > {}) >/dev/null 2>&1 &", cp_cmd, shell_escape(&marker));
    sudo_exec_ssh(session, &sudo_password, &bg_cmd).await?;

    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let check = exec_ssh(session, &format!("cat {} 2>/dev/null", shell_escape(&marker))).await.unwrap_or_default();
        if !check.trim().is_empty() {
            let _ = exec_ssh(session, &format!("rm -f {}", shell_escape(&marker))).await;
            let exit_code: i32 = check.trim().parse().unwrap_or(-1);
            if exit_code != 0 {
                return Err("Permission Denied: Sudo Required".to_string());
            }
            break;
        }
        let poll_cmd = if is_dir {
            format!("du -sb {} 2>/dev/null | cut -f1", shell_escape(&dest))
        } else {
            format!("stat -c '%s' {} 2>/dev/null", shell_escape(&dest))
        };
        if let Ok(out) = exec_ssh(session, &poll_cmd).await {
            let current: u64 = out.split_whitespace().next()
                .and_then(|s| s.parse().ok()).unwrap_or(0);
            let _ = app.emit("transfer-progress", TransferProgress {
                id: transfer_id.clone(), transfer_type: "copy".into(),
                file_name: file_name.clone(), bytes_transferred: current, total_bytes,
            });
        }
    }

    let _ = app.emit("transfer-progress", TransferProgress {
        id: transfer_id.clone(), transfer_type: "copy".into(),
        file_name, bytes_transferred: total_bytes, total_bytes,
    });
    Ok(())
}

#[derive(Serialize)]
struct SearchResult {
    path: String,
    name: String,
    is_dir: bool,
}

#[tauri::command]
async fn search_files(query: String, search_path: String, state: State<'_, SshSession>) -> Result<Vec<SearchResult>, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let escaped_query = query.replace("'", "'\\''");
    let cmd = format!(
        "find {} -maxdepth 1 -iname '*{}*' -not -name '.*' -printf '%y|%p\\n' 2>/dev/null | head -50",
        shell_escape(&search_path),
        escaped_query
    );
    let output = exec_ssh(session, &cmd).await?;
    let mut results = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (dtype, path) = match line.split_once('|') {
            Some(pair) => pair,
            None => continue,
        };
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        results.push(SearchResult {
            path: path.to_string(),
            name,
            is_dir: dtype == "d",
        });
    }
    Ok(results)
}

#[tauri::command]
async fn sudo_search_files(query: String, search_path: String, sudo_password: String, state: State<'_, SshSession>) -> Result<Vec<SearchResult>, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let escaped_query = query.replace("'", "'\\''");
    let cmd = format!(
        "find {} -maxdepth 1 -iname '*{}*' -not -name '.*' -printf '%y|%p\\n' 2>/dev/null | head -50; true",
        shell_escape(&search_path),
        escaped_query
    );
    let output = sudo_exec_ssh(session, &sudo_password, &cmd).await?;
    let mut results = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (dtype, path) = match line.split_once('|') {
            Some(pair) => pair,
            None => continue,
        };
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        results.push(SearchResult {
            path: path.to_string(),
            name,
            is_dir: dtype == "d",
        });
    }
    Ok(results)
}

#[tauri::command]
async fn download_file(app: AppHandle, transfer_id: String, remote_path: String, local_path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let file_name = remote_path.rsplit('/').next().unwrap_or(&remote_path).to_string();

    let (total_bytes, sftp) = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;

        let total_bytes: u64 = {
            let cmd = format!("stat -c '%s' {}", shell_escape(&remote_path));
            exec_ssh(session, &cmd).await.ok()
                .and_then(|o| o.trim().parse().ok())
                .unwrap_or(0)
        };

        let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
        let sftp = russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?;

        (total_bytes, sftp)
    };

    let mut remote_file = sftp.open(&remote_path).await.map_err(|e| format!("Failed to open remote file: {}", e))?;
    let mut local_file = tokio::fs::File::create(&local_path).await.map_err(|e| format!("Failed to create local file: {}", e))?;

    let mut bytes_transferred = 0u64;
    let mut buf = vec![0u8; 65536];

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    loop {
        if is_transfer_cancelled(&transfer_id) {
            CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
            drop(local_file);
            let _ = tokio::fs::remove_file(&local_path).await;
            return Err("Transfer cancelled".to_string());
        }
        let n = remote_file.read(&mut buf).await.map_err(|e| format!("Read error: {}", e))?;
        if n == 0 { break; }
        local_file.write_all(&buf[..n]).await.map_err(|e| format!("Write error: {}", e))?;
        bytes_transferred += n as u64;
        let _ = app.emit("transfer-progress", TransferProgress {
            id: transfer_id.clone(),
            transfer_type: "download".to_string(),
            file_name: file_name.clone(),
            bytes_transferred,
            total_bytes,
        });
    }

    Ok(())
}

#[tauri::command]
async fn download_directory(
    app: AppHandle,
    transfer_id: String,
    remote_path: String,
    local_path: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let dir_name = remote_path.rsplit('/').next().unwrap_or(&remote_path).to_string();

    let (file_list, total_bytes) = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let cmd = format!(
            "find {} -type f -printf '%s|%P\\n' 2>/dev/null",
            shell_escape(&remote_path)
        );
        let output = exec_ssh(session, &cmd).await?;

        let mut files = Vec::new();
        let mut total = 0u64;
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            if parts.len() < 2 { continue; }
            let size: u64 = parts[0].parse().unwrap_or(0);
            let rel_path = parts[1].to_string();
            total += size;
            files.push(rel_path);
        }

        if files.is_empty() {
            let check = format!("ls {} 2>&1", shell_escape(&remote_path));
            let check_output = exec_ssh(session, &check).await.unwrap_or_default();
            if check_output.contains("Permission denied") {
                return Err("Permission Denied: Sudo Required".to_string());
            }
        }

        (files, total)
    };

    tokio::fs::create_dir_all(&local_path).await
        .map_err(|e| format!("Failed to create local directory: {}", e))?;

    let sftp = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
        russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?
    };

    let mut bytes_transferred = 0u64;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    for rel_path in &file_list {
        if is_transfer_cancelled(&transfer_id) {
            CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
            let _ = tokio::fs::remove_dir_all(&local_path).await;
            return Err("Transfer cancelled".to_string());
        }

        let remote_file_path = format!("{}/{}", remote_path, rel_path);
        let local_file_path = std::path::Path::new(&local_path).join(rel_path.replace('/', std::path::MAIN_SEPARATOR_STR));

        if let Some(parent) = local_file_path.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| format!("Failed to create local directory: {}", e))?;
        }

        let mut remote_file = sftp.open(&remote_file_path).await
            .map_err(|e| format!("Failed to open remote file: {}", e))?;
        let mut local_file = tokio::fs::File::create(&local_file_path).await
            .map_err(|e| format!("Failed to create local file: {}", e))?;

        let mut buf = vec![0u8; 65536];
        loop {
            if is_transfer_cancelled(&transfer_id) {
                CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
                drop(local_file);
                let _ = tokio::fs::remove_dir_all(&local_path).await;
                return Err("Transfer cancelled".to_string());
            }
            let n = remote_file.read(&mut buf).await.map_err(|e| format!("Read error: {}", e))?;
            if n == 0 { break; }
            local_file.write_all(&buf[..n]).await.map_err(|e| format!("Write error: {}", e))?;
            bytes_transferred += n as u64;
            let _ = app.emit("transfer-progress", TransferProgress {
                id: transfer_id.clone(),
                transfer_type: "download".to_string(),
                file_name: dir_name.clone(),
                bytes_transferred,
                total_bytes,
            });
        }
    }

    Ok(())
}

#[tauri::command]
async fn upload_file(app: AppHandle, transfer_id: String, local_path: String, remote_path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let file_name = local_path.replace('\\', "/");
    let file_name = file_name.rsplit('/').next().unwrap_or(&local_path).to_string();

    let total_bytes = tokio::fs::metadata(&local_path).await
        .map(|m| m.len())
        .unwrap_or(0);

    let sftp = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
        russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?
    };

    let mut local_file = tokio::fs::File::open(&local_path).await.map_err(|e| format!("Failed to open local file: {}", e))?;
    let mut remote_file = sftp.create(&remote_path).await.map_err(|e| format!("Failed to create remote file: {}", e))?;

    let mut bytes_transferred = 0u64;
    let mut buf = vec![0u8; 65536];

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut cancelled = false;
    loop {
        if is_transfer_cancelled(&transfer_id) {
            cancelled = true;
            break;
        }
        let n = local_file.read(&mut buf).await.map_err(|e| format!("Read error: {}", e))?;
        if n == 0 { break; }
        remote_file.write_all(&buf[..n]).await.map_err(|e| format!("Write error: {}", e))?;
        bytes_transferred += n as u64;
        let _ = app.emit("transfer-progress", TransferProgress {
            id: transfer_id.clone(),
            transfer_type: "upload".to_string(),
            file_name: file_name.clone(),
            bytes_transferred,
            total_bytes,
        });
    }

    remote_file.shutdown().await.map_err(|e| format!("Close error: {}", e))?;

    if cancelled {
        CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
        let s = state.lock().await;
        if let Some(session) = s.session.as_ref() {
            let _ = exec_ssh(session, &format!("rm -f {}", shell_escape(&remote_path))).await;
        }
        return Err("Transfer cancelled".to_string());
    }

    Ok(())
}

#[cfg(windows)]
#[tauri::command]
async fn start_virtual_drag(
    app: AppHandle,
    transfer_id: String,
    remote_path: String,
    file_name: String,
    file_size: u64,
    is_dir: bool,
    state: State<'_, SshSession>,
) -> Result<String, String> {
    let rt_handle = tokio::runtime::Handle::current();
    let ssh_state = state.inner().clone();

    let (entries, total_size) = if is_dir {
        let output = {
            let s = state.lock().await;
            let session = s.session.as_ref().ok_or("Not connected")?;
            let cmd = format!(
                "find {} -printf '%y|%s|%P\\n' 2>/dev/null",
                shell_escape(&remote_path)
            );
            exec_ssh(session, &cmd).await?
        };

        let mut entries = Vec::new();
        let mut total = 0u64;

        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() { continue; }
            let parts: Vec<&str> = line.splitn(3, '|').collect();
            if parts.len() < 3 { continue; }

            let ftype = parts[0];
            let size: u64 = parts[1].parse().unwrap_or(0);
            let rel = parts[2];

            let is_entry_dir = ftype == "d";
            let relative = if rel.is_empty() {
                file_name.clone()
            } else {
                format!("{}/{}", file_name, rel)
            };
            let full_remote = if rel.is_empty() {
                remote_path.clone()
            } else {
                format!("{}/{}", remote_path, rel)
            };

            if !is_entry_dir {
                total += size;
            }

            entries.push(virtual_drag::VirtualFileEntry {
                relative_path: relative,
                remote_path: full_remote,
                is_dir: is_entry_dir,
                file_size: if is_entry_dir { 0 } else { size },
            });
        }

        if entries.is_empty() {
            return Err("Permission Denied: Sudo Required".to_string());
        }

        let has_files = entries.iter().any(|e| !e.is_dir);
        let has_children = entries.len() > 1;
        if !has_files && !has_children {
            let s = state.lock().await;
            let session = s.session.as_ref().ok_or("Not connected")?;
            let check = format!("ls {} 2>&1", shell_escape(&remote_path));
            let check_output = exec_ssh(session, &check).await.unwrap_or_default();
            if check_output.contains("Permission denied") {
                return Err("Permission Denied: Sudo Required".to_string());
            }
        }

        (entries, total)
    } else {
        (
            vec![virtual_drag::VirtualFileEntry {
                relative_path: file_name.clone(),
                remote_path: remote_path.clone(),
                is_dir: false,
                file_size,
            }],
            file_size,
        )
    };

    let display_name = file_name.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let app_for_drag = app.clone();
    let hwnd = get_app_hwnd(&app)?;

    app.run_on_main_thread(move || {
        let result = virtual_drag::start_drag(
            rt_handle,
            ssh_state,
            entries,
            total_size,
            display_name,
            app_for_drag,
            transfer_id,
            hwnd,
        );
        let _ = tx.send(result);
    }).map_err(|e| format!("Failed to schedule drag on main thread: {}", e))?;

    rx.await.map_err(|_| "Drag thread error".to_string())?
}

#[cfg(target_os = "windows")]
fn get_app_hwnd(app: &AppHandle) -> Result<windows::Win32::Foundation::HWND, String> {
    let window = app.get_webview_window("main").ok_or("No main window")?;
    #[allow(deprecated)]
    let handle = window.raw_window_handle().map_err(|e| format!("{}", e))?;
    match handle {
        RawWindowHandle::Win32(h) => {
            Ok(windows::Win32::Foundation::HWND(isize::from(h.hwnd)))
        }
        _ => Err("Not a Win32 window".to_string()),
    }
}

#[derive(serde::Deserialize)]
// On non-Windows the fields are only used for deserialization; the drag code
// that reads them is Windows-only, so silence the dead-code lint there.
#[cfg_attr(not(windows), allow(dead_code))]
struct DragFileInfo {
    name: String,
    remote_path: String,
    size: u64,
    is_dir: bool,
}

#[cfg(windows)]
#[tauri::command]
async fn start_multi_drag(
    app: AppHandle,
    transfer_id: String,
    files: Vec<DragFileInfo>,
    state: State<'_, SshSession>,
) -> Result<String, String> {
    if files.is_empty() { return Ok("done".to_string()); }

    let rt_handle = tokio::runtime::Handle::current();
    let ssh_state = state.inner().clone();

    let mut all_entries = Vec::new();
    let mut total_size = 0u64;

    for file in &files {
        if file.is_dir {
            let output = {
                let s = state.lock().await;
                let session = s.session.as_ref().ok_or("Not connected")?;
                let cmd = format!(
                    "find {} -printf '%y|%s|%P\\n' 2>/dev/null",
                    shell_escape(&file.remote_path)
                );
                exec_ssh(session, &cmd).await?
            };
            for line in output.lines() {
                let line = line.trim();
                if line.is_empty() { continue; }
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() < 3 { continue; }
                let ftype = parts[0];
                let size: u64 = parts[1].parse().unwrap_or(0);
                let rel = parts[2];
                let is_entry_dir = ftype == "d";
                let relative = if rel.is_empty() {
                    file.name.clone()
                } else {
                    format!("{}/{}", file.name, rel)
                };
                let full_remote = if rel.is_empty() {
                    file.remote_path.clone()
                } else {
                    format!("{}/{}", file.remote_path, rel)
                };
                if !is_entry_dir { total_size += size; }
                all_entries.push(virtual_drag::VirtualFileEntry {
                    relative_path: relative,
                    remote_path: full_remote,
                    is_dir: is_entry_dir,
                    file_size: if is_entry_dir { 0 } else { size },
                });
            }
        } else {
            total_size += file.size;
            all_entries.push(virtual_drag::VirtualFileEntry {
                relative_path: file.name.clone(),
                remote_path: file.remote_path.clone(),
                is_dir: false,
                file_size: file.size,
            });
        }
    }

    if all_entries.is_empty() {
        return Err("No files to drag".to_string());
    }

    let display_name = if files.len() == 1 { files[0].name.clone() } else { format!("{} items", files.len()) };
    let (tx, rx) = tokio::sync::oneshot::channel();
    let app_for_drag = app.clone();
    let hwnd = get_app_hwnd(&app)?;

    app.run_on_main_thread(move || {
        let result = virtual_drag::start_drag(
            rt_handle,
            ssh_state,
            all_entries,
            total_size,
            display_name,
            app_for_drag,
            transfer_id,
            hwnd,
        );
        let _ = tx.send(result);
    }).map_err(|e| format!("Failed to schedule drag on main thread: {}", e))?;

    rx.await.map_err(|_| "Drag thread error".to_string())?
}

// Non-Windows platforms have no OLE virtual-file drag source. These stubs keep
// the Tauri command surface identical across platforms so `generate_handler!`
// compiles; the Linux/macOS frontend routes "drag out" through a save-folder
// dialog + download instead of invoking these (see handleDragOut in App.tsx).
#[cfg(not(windows))]
#[tauri::command]
async fn start_virtual_drag(
    _app: AppHandle,
    _transfer_id: String,
    _remote_path: String,
    _file_name: String,
    _file_size: u64,
    _is_dir: bool,
    _state: State<'_, SshSession>,
) -> Result<String, String> {
    Err("Drag-to-desktop is not supported on this platform".to_string())
}

#[cfg(not(windows))]
#[tauri::command]
async fn start_multi_drag(
    _app: AppHandle,
    _transfer_id: String,
    _files: Vec<DragFileInfo>,
    _state: State<'_, SshSession>,
) -> Result<String, String> {
    Err("Drag-to-desktop is not supported on this platform".to_string())
}

#[tauri::command]
async fn is_local_directory(path: String) -> Result<bool, String> {
    tokio::fs::metadata(&path)
        .await
        .map(|m| m.is_dir())
        .map_err(|e| format!("Failed to check path: {}", e))
}

#[tauri::command]
async fn upload_directory(
    app: AppHandle,
    transfer_id: String,
    local_path: String,
    remote_path: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let local_base = std::path::Path::new(&local_path);

    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut dirs: Vec<String> = vec![remote_path.clone()];
    let mut total_bytes = 0u64;

    let mut stack = vec![local_base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| format!("Read dir error: {}", e))?;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let meta = entry
                .metadata()
                .await
                .map_err(|e| format!("Metadata error: {}", e))?;
            let rel = entry
                .path()
                .strip_prefix(local_base)
                .unwrap()
                .to_path_buf();
            let remote_entry = format!(
                "{}/{}",
                remote_path,
                rel.to_string_lossy().replace('\\', "/")
            );

            if meta.is_dir() {
                dirs.push(remote_entry);
                stack.push(entry.path());
            } else {
                total_bytes += meta.len();
                files.push((entry.path(), remote_entry));
            }
        }
    }

    {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let mkdir_args = dirs
            .iter()
            .map(|d| shell_escape(d))
            .collect::<Vec<_>>()
            .join(" ");
        let cmd = format!("mkdir -p {} 2>&1; echo \"EXIT:$?\"", mkdir_args);
        let output = exec_ssh(session, &cmd).await?;
        let mut exit_code = 0;
        let filtered: String = output.lines().filter(|line| {
            if let Some(code) = line.strip_prefix("EXIT:") {
                exit_code = code.trim().parse::<i32>().unwrap_or(1);
                return false;
            }
            true
        }).collect::<Vec<_>>().join("\n");
        if exit_code != 0 {
            return Err(filtered.trim().to_string());
        }
    }

    let sftp = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| format!("Channel error: {}", e))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| format!("SFTP subsystem error: {}", e))?;
        russh_sftp::client::SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| format!("SFTP init error: {}", e))?
    };

    let dir_name = local_base
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut bytes_transferred = 0u64;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    for (file_path, remote_file_path) in &files {
        if is_transfer_cancelled(&transfer_id) {
            CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
            let s = state.lock().await;
            if let Some(session) = s.session.as_ref() {
                let _ = exec_ssh(session, &format!("rm -rf {}", shell_escape(&remote_path))).await;
            }
            return Err("Transfer cancelled".to_string());
        }

        let mut local_file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| format!("Open error: {}", e))?;
        let mut remote_file = sftp
            .create(remote_file_path)
            .await
            .map_err(|e| format!("Create remote file error: {}", e))?;

        let mut buf = vec![0u8; 65536];
        loop {
            if is_transfer_cancelled(&transfer_id) {
                CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
                drop(remote_file);
                let s = state.lock().await;
                if let Some(session) = s.session.as_ref() {
                    let _ = exec_ssh(session, &format!("rm -rf {}", shell_escape(&remote_path))).await;
                }
                return Err("Transfer cancelled".to_string());
            }
            let n = local_file
                .read(&mut buf)
                .await
                .map_err(|e| format!("Read error: {}", e))?;
            if n == 0 {
                break;
            }
            remote_file
                .write_all(&buf[..n])
                .await
                .map_err(|e| format!("Write error: {}", e))?;
            bytes_transferred += n as u64;
            let _ = app.emit(
                "transfer-progress",
                TransferProgress {
                    id: transfer_id.clone(),
                    transfer_type: "upload".to_string(),
                    file_name: dir_name.clone(),
                    bytes_transferred,
                    total_bytes,
                },
            );
        }
        remote_file
            .shutdown()
            .await
            .map_err(|e| format!("Close error: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
async fn sudo_upload_file(
    app: AppHandle,
    transfer_id: String,
    local_path: String,
    remote_path: String,
    sudo_password: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let file_name = local_path.replace('\\', "/");
    let file_name = file_name.rsplit('/').next().unwrap_or(&local_path).to_string();

    let total_bytes = tokio::fs::metadata(&local_path).await
        .map(|m| m.len())
        .unwrap_or(0);

    let temp_path = format!("/tmp/.ssh-explorer-{}", transfer_id);

    let sftp = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
        russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?
    };

    let mut local_file = tokio::fs::File::open(&local_path).await.map_err(|e| format!("Failed to open local file: {}", e))?;
    let mut remote_file = sftp.create(&temp_path).await.map_err(|e| format!("Failed to create temp file: {}", e))?;

    let mut bytes_transferred = 0u64;
    let mut buf = vec![0u8; 65536];

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut cancelled = false;
    loop {
        if is_transfer_cancelled(&transfer_id) {
            cancelled = true;
            break;
        }
        let n = local_file.read(&mut buf).await.map_err(|e| format!("Read error: {}", e))?;
        if n == 0 { break; }
        remote_file.write_all(&buf[..n]).await.map_err(|e| format!("Write error: {}", e))?;
        bytes_transferred += n as u64;
        let _ = app.emit("transfer-progress", TransferProgress {
            id: transfer_id.clone(),
            transfer_type: "upload".to_string(),
            file_name: file_name.clone(),
            bytes_transferred,
            total_bytes,
        });
    }

    remote_file.shutdown().await.map_err(|e| format!("Close error: {}", e))?;

    if cancelled {
        CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
        let s = state.lock().await;
        if let Some(session) = s.session.as_ref() {
            let _ = exec_ssh(session, &format!("rm -f {}", shell_escape(&temp_path))).await;
        }
        return Err("Transfer cancelled".to_string());
    }

    let result = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let parent = match remote_path.rfind('/') {
            Some(pos) if pos > 0 => &remote_path[..pos],
            _ => "/",
        };
        let cmd = format!(
            "mkdir -p {} && mv {} {}",
            shell_escape(parent),
            shell_escape(&temp_path),
            shell_escape(&remote_path),
        );
        sudo_exec_ssh(session, &sudo_password, &cmd).await
    };

    if result.is_err() {
        let s = state.lock().await;
        if let Some(session) = s.session.as_ref() {
            let _ = exec_ssh(session, &format!("rm -f {}", shell_escape(&temp_path))).await;
        }
    }

    result?;
    Ok(())
}

#[tauri::command]
async fn sudo_upload_directory(
    app: AppHandle,
    transfer_id: String,
    local_path: String,
    remote_path: String,
    sudo_password: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let local_base = std::path::Path::new(&local_path);
    let temp_base = format!("/tmp/.ssh-explorer-{}", transfer_id);

    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut target_dirs: Vec<String> = vec![remote_path.clone()];
    let mut temp_dirs: Vec<String> = vec![temp_base.clone()];
    let mut total_bytes = 0u64;

    let mut stack = vec![local_base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut rd = tokio::fs::read_dir(&dir).await
            .map_err(|e| format!("Read dir error: {}", e))?;
        while let Ok(Some(entry)) = rd.next_entry().await {
            let meta = entry.metadata().await
                .map_err(|e| format!("Metadata error: {}", e))?;
            let rel = entry.path().strip_prefix(local_base).unwrap().to_path_buf();
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let remote_entry = format!("{}/{}", remote_path, rel_str);
            let temp_entry = format!("{}/{}", temp_base, rel_str);

            if meta.is_dir() {
                target_dirs.push(remote_entry);
                temp_dirs.push(temp_entry);
                stack.push(entry.path());
            } else {
                total_bytes += meta.len();
                files.push((entry.path(), temp_entry));
            }
        }
    }

    // Create temp directory structure (no sudo needed — it's in /tmp)
    {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let mkdir_args = temp_dirs.iter().map(|d| shell_escape(d)).collect::<Vec<_>>().join(" ");
        let cmd = format!("mkdir -p {}", mkdir_args);
        exec_ssh(session, &cmd).await?;
    }

    // Upload files to temp via SFTP
    let sftp = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
        channel.request_subsystem(true, "sftp").await.map_err(|e| format!("SFTP subsystem error: {}", e))?;
        russh_sftp::client::SftpSession::new(channel.into_stream()).await.map_err(|e| format!("SFTP init error: {}", e))?
    };

    let dir_name = local_base.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mut bytes_transferred = 0u64;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut cancelled = false;
    for (file_path, temp_file_path) in &files {
        if is_transfer_cancelled(&transfer_id) {
            cancelled = true;
            break;
        }

        let mut local_file = tokio::fs::File::open(file_path).await
            .map_err(|e| format!("Open error: {}", e))?;
        let mut remote_file = sftp.create(temp_file_path).await
            .map_err(|e| format!("Create temp file error: {}", e))?;

        let mut buf = vec![0u8; 65536];
        loop {
            if is_transfer_cancelled(&transfer_id) {
                cancelled = true;
                break;
            }
            let n = local_file.read(&mut buf).await.map_err(|e| format!("Read error: {}", e))?;
            if n == 0 { break; }
            remote_file.write_all(&buf[..n]).await.map_err(|e| format!("Write error: {}", e))?;
            bytes_transferred += n as u64;
            let _ = app.emit("transfer-progress", TransferProgress {
                id: transfer_id.clone(),
                transfer_type: "upload".to_string(),
                file_name: dir_name.clone(),
                bytes_transferred,
                total_bytes,
            });
        }
        remote_file.shutdown().await.map_err(|e| format!("Close error: {}", e))?;
        if cancelled { break; }
    }

    if cancelled {
        CANCELLED_TRANSFERS.lock().unwrap().remove(&transfer_id);
        let s = state.lock().await;
        if let Some(session) = s.session.as_ref() {
            let _ = exec_ssh(session, &format!("rm -rf {}", shell_escape(&temp_base))).await;
        }
        return Err("Transfer cancelled".to_string());
    }

    // Create target dirs and move files with sudo
    let result = {
        let s = state.lock().await;
        let session = s.session.as_ref().ok_or("Not connected")?;
        let mkdir_args = target_dirs.iter().map(|d| shell_escape(d)).collect::<Vec<_>>().join(" ");
        let cmd = format!("mkdir -p {} && cp -rT {} {}", mkdir_args, shell_escape(&temp_base), shell_escape(&remote_path));
        sudo_exec_ssh(session, &sudo_password, &cmd).await
    };

    // Clean up temp regardless of success/failure
    {
        let s = state.lock().await;
        if let Some(session) = s.session.as_ref() {
            let _ = exec_ssh(session, &format!("rm -rf {}", shell_escape(&temp_base))).await;
        }
    }

    result?;
    Ok(())
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn format_timestamp(ts: i64) -> String {
    let secs_per_day: i64 = 86400;
    let days = ts / secs_per_day;
    let day_secs = ts % secs_per_day;
    let hours = day_secs / 3600;
    let mins = (day_secs % 3600) / 60;

    let (year, month, day) = days_to_date(days + 719468);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", year, month, day, hours, mins)
}

fn days_to_date(days: i64) -> (i64, i64, i64) {
    let era = if days >= 0 { days } else { days - 146096 } / 146097;
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn format_permissions(mode: &str, is_dir: bool) -> String {
    let prefix = if is_dir { "d" } else { "-" };
    let mode_int = u32::from_str_radix(mode, 8).unwrap_or(0);

    let mut perms = String::with_capacity(10);
    perms.push_str(prefix);

    for shift in (0..9).rev() {
        let bit = (mode_int >> shift) & 1;
        let c = match shift % 3 {
            2 => if bit == 1 { 'r' } else { '-' },
            1 => if bit == 1 { 'w' } else { '-' },
            0 => if bit == 1 { 'x' } else { '-' },
            _ => '-',
        };
        perms.push(c);
    }

    perms
}

#[tauri::command]
fn cancel_transfer(transfer_id: String) {
    CANCELLED_TRANSFERS.lock().unwrap().insert(transfer_id);
}

#[tauri::command]
fn get_progress_port() -> u16 {
    #[cfg(windows)]
    { virtual_drag::progress_port() }
    #[cfg(not(windows))]
    { 0 }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    virtual_drag::init_ole_main_thread();
    #[cfg(windows)]
    virtual_drag::start_progress_server();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(Mutex::new(SshState { session: None, sftp: None })) as SshSession)
        .invoke_handler(tauri::generate_handler![
            discover_ssh_keys,
            ssh_connect,
            ssh_connect_key,
            trust_host_key,
            ssh_disconnect,
            check_writable,
            check_sudo_writable,
            list_directory,
            sudo_list_directory,
            read_file,
            write_file,
            create_file,
            create_directory,
            delete_file,
            rename_file,
            sudo_rename_file,
            sudo_read_file,
            sudo_write_file,
            sudo_create_file,
            sudo_create_directory,
            sudo_delete_file,
            copy_path,
            sudo_copy_path,
            search_files,
            sudo_search_files,
            download_file,
            download_directory,
            start_virtual_drag,
            start_multi_drag,
            get_progress_port,
            is_local_directory,
            upload_file,
            upload_directory,
            sudo_upload_file,
            sudo_upload_directory,
            cancel_transfer,
            get_saved_connections,
            save_connection,
            get_connection_password,
            delete_connection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
