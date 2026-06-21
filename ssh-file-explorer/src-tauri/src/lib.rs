use std::sync::Arc;
use tokio::sync::Mutex;
use russh::*;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};

struct SshState {
    session: Option<russh::client::Handle<ClientHandler>>,
}

struct ClientHandler;

impl russh::client::Handler for ClientHandler {
    type Error = russh::Error;

    fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
        async { Ok(true) }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct SavedConnection {
    id: String,
    label: String,
    host: String,
    port: u16,
    username: String,
    password: Option<String>,
}

#[derive(Serialize)]
struct FileEntry {
    name: String,
    is_dir: bool,
    size: u64,
    modified: String,
    permissions: String,
}

type SshSession = Arc<Mutex<SshState>>;

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
    read_connections(&app)
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
) -> Result<(), String> {
    let mut connections = read_connections(&app)?;

    let existing = connections.iter_mut().find(|c| {
        c.id == id || (c.host == host && c.port == port && c.username == username)
    });

    let conn = SavedConnection { id: id.clone(), label, host, port, username, password };

    if let Some(entry) = existing {
        *entry = conn;
    } else {
        connections.push(conn);
    }

    write_connections(&app, &connections)
}

#[tauri::command]
async fn delete_connection(app: AppHandle, id: String) -> Result<(), String> {
    let mut connections = read_connections(&app)?;
    connections.retain(|c| c.id != id);
    write_connections(&app, &connections)
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

    let mut s = state.lock().await;
    s.session = Some(session);
    Ok(())
}

#[tauri::command]
async fn ssh_disconnect(state: State<'_, SshSession>) -> Result<(), String> {
    let mut s = state.lock().await;
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
    let cmd = format!("cat {}", shell_escape(&path));
    exec_ssh(session, &cmd).await
}

#[tauri::command]
async fn write_file(path: String, content: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;

    let channel = session.channel_open_session().await.map_err(|e| format!("Channel error: {}", e))?;
    let cmd = format!("cat > {}", shell_escape(&path));
    channel.exec(true, cmd.as_bytes()).await.map_err(|e| format!("Exec error: {}", e))?;

    let mut stream = channel.into_stream();
    use tokio::io::AsyncWriteExt;
    stream.write_all(content.as_bytes()).await.map_err(|e| format!("Write error: {}", e))?;
    stream.shutdown().await.map_err(|e| format!("Close error: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn create_file(path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let escaped = shell_escape(&path);
    let cmd = format!("mkdir -p \"$(dirname {})\" && touch {}", escaped, escaped);
    exec_ssh(session, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn create_directory(path: String, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = format!("mkdir -p {}", shell_escape(&path));
    exec_ssh(session, &cmd).await?;
    Ok(())
}

#[tauri::command]
async fn delete_file(path: String, is_dir: bool, state: State<'_, SshSession>) -> Result<(), String> {
    let s = state.lock().await;
    let session = s.session.as_ref().ok_or("Not connected")?;
    let cmd = if is_dir {
        format!("rm -rf {}", shell_escape(&path))
    } else {
        format!("rm -f {}", shell_escape(&path))
    };
    exec_ssh(session, &cmd).await?;
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::new(Mutex::new(SshState { session: None })) as SshSession)
        .invoke_handler(tauri::generate_handler![
            ssh_connect,
            ssh_disconnect,
            list_directory,
            read_file,
            write_file,
            create_file,
            create_directory,
            delete_file,
            get_saved_connections,
            save_connection,
            delete_connection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
