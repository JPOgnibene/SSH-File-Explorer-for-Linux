use std::sync::Arc;
use tokio::sync::Mutex;
use russh::*;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(windows)]
mod virtual_drag;

pub(crate) struct SshState {
    pub(crate) session: Option<russh::client::Handle<ClientHandler>>,
    pub(crate) sftp: Option<russh_sftp::client::SftpSession>,
}

pub(crate) struct ClientHandler;

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
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
    host: String,
    port: u16,
    username: String,
    password: String,
    state: State<'_, SshSession>,
) -> Result<(), String> {
    let config = Arc::new(russh::client::Config::default());
    let handler = ClientHandler;

    let mut session = russh::client::connect(config, (host.as_str(), port), handler)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

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
    let handler = ClientHandler;

    let mut session = russh::client::connect(config, (host.as_str(), port), handler)
        .await
        .map_err(|e| format!("Connection failed: {}", e))?;

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
    let read_result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => output.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
    }).await;

    if read_result.is_err() {
        return Err("Sudo operation timed out".into());
    }

    let text = String::from_utf8_lossy(&output).into_owned();
    let lower = text.to_lowercase();
    if lower.contains("sorry, try again")
        || lower.contains("is not in the sudoers file")
        || lower.contains("incorrect password")
        || lower.contains("authentication failure")
    {
        return Err("Sudo authentication failed — incorrect password or insufficient privileges".into());
    }

    let mut exit_code = 0;
    let filtered: String = text.lines()
        .filter(|line| {
            if let Some(code) = line.strip_prefix("SUDO_EXIT:") {
                exit_code = code.trim().parse::<i32>().unwrap_or(1);
                return false;
            }
            !line.contains("[sudo] password for")
        })
        .collect::<Vec<_>>()
        .join("\n");

    if exit_code != 0 && !filtered.trim().is_empty() {
        return Err(filtered.trim().to_string());
    }
    if exit_code != 0 {
        return Err("Sudo command failed".into());
    }
    Ok(filtered)
}

#[tauri::command]
async fn check_writable(path: String, state: State<'_, SshSession>) -> Result<bool, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("test -w {} && echo 'y' || echo 'n'", shell_escape(&path));
    let output = exec_ssh(session, &cmd).await?;
    Ok(output.trim() == "y")
}

#[tauri::command]
async fn list_directory(path: String, state: State<'_, SshSession>) -> Result<Vec<FileEntry>, String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;

    let cmd = format!(
        "LC_ALL=C stat -c '%n|%F|%s|%Y|%a' {}/* 2>/dev/null; LC_ALL=C stat -c '%n|%F|%s|%Y|%a' {}/.[!.]* 2>/dev/null",
        shell_escape(&path),
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

    loop {
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
) -> Result<(), String> {
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
            return Err("Directory is empty or not accessible".to_string());
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

    app.run_on_main_thread(move || {
        let result = virtual_drag::start_drag(
            rt_handle,
            ssh_state,
            entries,
            total_size,
            display_name,
            app_for_drag,
            transfer_id,
        );
        let _ = tx.send(result);
    }).map_err(|e| format!("Failed to schedule drag on main thread: {}", e))?;

    rx.await.map_err(|_| "Drag thread error".to_string())?
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
        let mut local_file = tokio::fs::File::open(file_path)
            .await
            .map_err(|e| format!("Open error: {}", e))?;
        let mut remote_file = sftp
            .create(remote_file_path)
            .await
            .map_err(|e| format!("Create remote file error: {}", e))?;

        let mut buf = vec![0u8; 65536];
        loop {
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

    loop {
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

    for (file_path, temp_file_path) in &files {
        let mut local_file = tokio::fs::File::open(file_path).await
            .map_err(|e| format!("Open error: {}", e))?;
        let mut remote_file = sftp.create(temp_file_path).await
            .map_err(|e| format!("Create temp file error: {}", e))?;

        let mut buf = vec![0u8; 65536];
        loop {
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
            ssh_disconnect,
            check_writable,
            list_directory,
            read_file,
            write_file,
            create_file,
            create_directory,
            delete_file,
            sudo_read_file,
            sudo_write_file,
            sudo_create_file,
            sudo_create_directory,
            sudo_delete_file,
            search_files,
            download_file,
            start_virtual_drag,
            get_progress_port,
            is_local_directory,
            upload_file,
            upload_directory,
            sudo_upload_file,
            sudo_upload_directory,
            get_saved_connections,
            save_connection,
            get_connection_password,
            delete_connection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
