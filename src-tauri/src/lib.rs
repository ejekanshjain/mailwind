use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use lettre::{
    message::Mailbox, transport::smtp::authentication::Credentials, Message as SmtpMessage,
    SmtpTransport, Transport,
};
use mailparse::DispositionType;
use mailparse::MailHeaderMap;
use reqwest::blocking::Client;
use rusqlite::{params, types::Value as SqlValue, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State};
use uuid::Uuid;

type AppResult<T> = Result<T, String>;

struct AppState {
    db: Mutex<Connection>,
    syncing: Mutex<bool>,
    polling: Mutex<bool>,
    idle_accounts: Mutex<HashSet<i64>>,
}

#[derive(Debug, Clone)]
struct StoredAccount {
    id: i64,
    provider: String,
    email: String,
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_expires_at: Option<i64>,
    client_id: Option<String>,
    client_secret: Option<String>,
    imap_host: Option<String>,
    imap_port: Option<i64>,
    smtp_host: Option<String>,
    smtp_port: Option<i64>,
    username: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Serialize)]
struct AccountSummary {
    id: i64,
    provider: String,
    email: String,
    display_name: Option<String>,
    imap_host: Option<String>,
    imap_port: Option<i64>,
    smtp_host: Option<String>,
    smtp_port: Option<i64>,
    username: Option<String>,
}

#[derive(Debug, Serialize)]
struct FolderSummary {
    name: String,
    count: i64,
    unread_count: i64,
}

#[derive(Debug, Serialize)]
struct StoredMessage {
    id: i64,
    account_id: i64,
    provider_message_id: String,
    thread_id: String,
    folder: String,
    subject: String,
    from_addr: String,
    to_addr: String,
    cc_addr: String,
    date_ts: i64,
    snippet: String,
    body: String,
    body_mime: String,
    is_read: bool,
    account_email: String,
    account_provider: String,
    attachments: Vec<AttachmentSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct AttachmentSummary {
    id: i64,
    filename: String,
    mime_type: String,
    size: i64,
}

#[derive(Debug, Serialize)]
struct MailboxSnapshot {
    accounts: Vec<AccountSummary>,
    folders: Vec<FolderSummary>,
    messages: Vec<StoredMessage>,
    page: i64,
    page_size: i64,
    total: i64,
}

#[derive(Debug, Deserialize)]
struct GmailConnectInput {
    client_id: String,
    client_secret: String,
}

#[derive(Debug, Deserialize)]
struct ImapConnectInput {
    email: String,
    display_name: Option<String>,
    imap_host: String,
    imap_port: i64,
    smtp_host: String,
    smtp_port: i64,
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
struct UpdateImapSettingsInput {
    account_id: i64,
    email: String,
    display_name: Option<String>,
    imap_host: String,
    imap_port: i64,
    smtp_host: String,
    smtp_port: i64,
    username: String,
    password: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageFilter {
    folder: Option<String>,
    account_id: Option<i64>,
    query: Option<String>,
    read_filter: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct SendInput {
    account_id: i64,
    to: String,
    subject: String,
    body: String,
    reply_to_message_id: Option<i64>,
}

#[derive(Debug, Clone)]
struct ReplyContext {
    provider_message_id: String,
    thread_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DeleteInput {
    message_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct DownloadAttachmentInput {
    attachment_id: i64,
    save_path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RemoveAccountInput {
    account_id: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct MarkReadInput {
    message_id: i64,
    is_read: bool,
}

#[derive(Debug, Clone, Serialize)]
struct MailboxChanged {
    new_messages: usize,
    unread_inbox: i64,
}

#[derive(Debug, Serialize)]
struct DownloadedAttachment {
    path: String,
}

#[derive(Debug, Clone)]
struct NewMessage {
    provider_message_id: String,
    thread_id: String,
    folder: String,
    subject: String,
    from_addr: String,
    to_addr: String,
    cc_addr: String,
    date_ts: i64,
    snippet: String,
    body: String,
    body_mime: String,
    is_read: bool,
    attachments: Vec<NewAttachment>,
}

#[derive(Debug, Clone)]
struct NewAttachment {
    provider_attachment_id: Option<String>,
    filename: String,
    mime_type: String,
    size: i64,
    data: Option<Vec<u8>>,
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn debug(app: &tauri::AppHandle, message: impl AsRef<str>) {
    let message = message.as_ref();
    eprintln!("[mailwind] {message}");
    let _ = app.emit("mailwind-debug", message.to_string());
}

fn connect_imap_client(
    host: &str,
    port: u16,
    tls: &native_tls::TlsConnector,
) -> AppResult<imap::Client<native_tls::TlsStream<TcpStream>>> {
    let timeout = Duration::from_secs(15);
    let mut last_error = None;
    for addr in (host, port).to_socket_addrs().map_err(|e| e.to_string())? {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(stream) => {
                stream
                    .set_read_timeout(Some(timeout))
                    .map_err(|e| e.to_string())?;
                stream
                    .set_write_timeout(Some(timeout))
                    .map_err(|e| e.to_string())?;
                let tls_stream = tls
                    .connect(host, stream)
                    .map_err(|e| format!("IMAP TLS handshake failed: {e}"))?;
                let mut client = imap::Client::new(tls_stream);
                client
                    .read_greeting()
                    .map_err(|e| format!("IMAP greeting failed: {e}"))?;
                return Ok(client);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(format!(
        "IMAP TCP connection failed: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "no socket addresses found".to_string())
    ))
}

fn init_db(app: &tauri::AppHandle) -> Result<Connection, Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir()?;
    fs::create_dir_all(&dir)?;
    let conn = Connection::open(dir.join("mailwind.sqlite3"))?;

    conn.execute_batch(
        "
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        PRAGMA busy_timeout = 5000;

        CREATE TABLE IF NOT EXISTS accounts (
            id INTEGER PRIMARY KEY,
            provider TEXT NOT NULL,
            email TEXT NOT NULL,
            display_name TEXT,
            access_token TEXT,
            refresh_token TEXT,
            token_expires_at INTEGER,
            client_id TEXT,
            client_secret TEXT,
            imap_host TEXT,
            imap_port INTEGER,
            smtp_host TEXT,
            smtp_port INTEGER,
            username TEXT,
            password TEXT,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY,
            account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            provider_message_id TEXT NOT NULL,
            thread_id TEXT NOT NULL,
            folder TEXT NOT NULL,
            subject TEXT NOT NULL,
            from_addr TEXT NOT NULL,
            to_addr TEXT NOT NULL,
            cc_addr TEXT NOT NULL,
            date_ts INTEGER NOT NULL,
            snippet TEXT NOT NULL,
            body TEXT NOT NULL,
            body_mime TEXT NOT NULL DEFAULT 'text/plain',
            is_read INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(account_id, provider_message_id)
        );

        CREATE TABLE IF NOT EXISTS attachments (
            id INTEGER PRIMARY KEY,
            message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
            provider_attachment_id TEXT,
            filename TEXT NOT NULL,
            mime_type TEXT NOT NULL,
            size INTEGER NOT NULL,
            data BLOB,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_cursors (
            account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            folder TEXT NOT NULL,
            cursor TEXT,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(account_id, folder)
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            subject,
            from_addr,
            to_addr,
            body
        );

        CREATE INDEX IF NOT EXISTS idx_accounts_created_at
            ON accounts(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_accounts_provider_id
            ON accounts(provider, id);

        CREATE INDEX IF NOT EXISTS idx_messages_folder_date
            ON messages(folder, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_date
            ON messages(account_id, folder, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_folder_read_date
            ON messages(folder, is_read, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_read_date
            ON messages(account_id, folder, is_read, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_provider
            ON messages(account_id, folder, provider_message_id);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_thread
            ON messages(account_id, folder, thread_id);
        CREATE INDEX IF NOT EXISTS idx_messages_account_thread
            ON messages(account_id, thread_id);
        CREATE INDEX IF NOT EXISTS idx_messages_inbox_unread
            ON messages(folder, is_read)
            WHERE folder = 'Inbox' AND is_read = 0;
        CREATE INDEX IF NOT EXISTS idx_attachments_message_id
            ON attachments(message_id);
        ",
    )?;
    conn.execute_batch("PRAGMA optimize;")?;

    Ok(conn)
}

fn row_to_account(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredAccount> {
    Ok(StoredAccount {
        id: row.get(0)?,
        provider: row.get(1)?,
        email: row.get(2)?,
        access_token: row.get(4)?,
        refresh_token: row.get(5)?,
        token_expires_at: row.get(6)?,
        client_id: row.get(7)?,
        client_secret: row.get(8)?,
        imap_host: row.get(9)?,
        imap_port: row.get(10)?,
        smtp_host: row.get(11)?,
        smtp_port: row.get(12)?,
        username: row.get(13)?,
        password: row.get(14)?,
    })
}

fn get_account(conn: &Connection, id: i64) -> AppResult<StoredAccount> {
    conn.query_row(
        "SELECT id, provider, email, display_name, access_token, refresh_token, token_expires_at,
                client_id, client_secret, imap_host, imap_port, smtp_host, smtp_port, username, password
         FROM accounts WHERE id = ?",
        params![id],
        row_to_account,
    )
    .optional()
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "Account not found".to_string())
}

fn upsert_message(conn: &Connection, account_id: i64, msg: &NewMessage) -> AppResult<()> {
    prepare_imap_message_identity(conn, account_id, msg)?;
    let ts = now_ts();
    conn.execute(
        "
        INSERT INTO messages (
            account_id, provider_message_id, thread_id, folder, subject, from_addr, to_addr,
            cc_addr, date_ts, snippet, body, body_mime, is_read, created_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(account_id, provider_message_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            folder = excluded.folder,
            subject = excluded.subject,
            from_addr = excluded.from_addr,
            to_addr = excluded.to_addr,
            cc_addr = excluded.cc_addr,
            date_ts = excluded.date_ts,
            snippet = excluded.snippet,
            body = excluded.body,
            body_mime = excluded.body_mime,
            is_read = excluded.is_read,
            updated_at = excluded.updated_at
        ",
        params![
            account_id,
            msg.provider_message_id,
            msg.thread_id,
            msg.folder,
            msg.subject,
            msg.from_addr,
            msg.to_addr,
            msg.cc_addr,
            msg.date_ts,
            msg.snippet,
            msg.body,
            msg.body_mime,
            if msg.is_read { 1 } else { 0 },
            ts,
            ts
        ],
    )
    .map_err(|e| e.to_string())?;

    let id: i64 = conn
        .query_row(
            "SELECT id FROM messages WHERE account_id = ? AND provider_message_id = ?",
            params![account_id, msg.provider_message_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    conn.execute("DELETE FROM messages_fts WHERE rowid = ?", params![id])
        .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO messages_fts(rowid, subject, from_addr, to_addr, body) VALUES (?, ?, ?, ?, ?)",
        params![id, msg.subject, msg.from_addr, msg.to_addr, msg.body],
    )
    .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM attachments WHERE message_id = ?", params![id])
        .map_err(|e| e.to_string())?;
    for attachment in &msg.attachments {
        conn.execute(
            "
            INSERT INTO attachments (
                message_id, provider_attachment_id, filename, mime_type, size, data, created_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ",
            params![
                id,
                attachment.provider_attachment_id,
                attachment.filename,
                attachment.mime_type,
                attachment.size,
                attachment.data,
                ts
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn prepare_imap_message_identity(
    conn: &Connection,
    account_id: i64,
    msg: &NewMessage,
) -> AppResult<()> {
    let Some(message_id) = msg.provider_message_id.strip_prefix("imap-message:") else {
        return Ok(());
    };
    if message_id.trim().is_empty() {
        return Ok(());
    }

    let legacy_like = format!("%:{}", escape_sql_like(message_id));
    let exact_id = conn
        .query_row(
            "SELECT id FROM messages WHERE account_id = ? AND provider_message_id = ?",
            params![account_id, msg.provider_message_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "
            SELECT id
            FROM messages
            WHERE account_id = ?
              AND provider_message_id != ?
              AND provider_message_id NOT LIKE 'local-sent-%'
              AND provider_message_id LIKE ? ESCAPE '\\'
            ORDER BY CASE WHEN folder = ? THEN 0 ELSE 1 END, updated_at DESC, id DESC
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(
            params![account_id, msg.provider_message_id, legacy_like, msg.folder],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|e| e.to_string())?;
    let legacy_ids = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    if exact_id.is_none() {
        if let Some(id) = legacy_ids.first() {
            conn.execute(
                "UPDATE messages SET provider_message_id = ?, updated_at = ? WHERE id = ?",
                params![msg.provider_message_id, now_ts(), id],
            )
            .map_err(|e| e.to_string())?;
        }
        for id in legacy_ids.iter().skip(1) {
            delete_message_row(conn, *id)?;
        }
    } else {
        for id in legacy_ids {
            delete_message_row(conn, id)?;
        }
    }
    Ok(())
}

fn escape_sql_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn delete_message_row(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?", params![id])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM messages WHERE id = ?", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_messages_by_thread(
    conn: &Connection,
    account_id: i64,
    thread_id: &str,
) -> AppResult<usize> {
    let mut stmt = conn
        .prepare("SELECT id FROM messages WHERE account_id = ? AND thread_id = ?")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, thread_id], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    let ids = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    let count = ids.len();
    for id in ids {
        delete_message_row(conn, id)?;
    }
    Ok(count)
}

fn existing_provider_ids(
    conn: &Connection,
    account_id: i64,
    folder: &str,
) -> AppResult<HashSet<String>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT provider_message_id
            FROM messages
            WHERE account_id = ? AND folder = ? AND provider_message_id NOT LIKE 'local-sent-%'
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, folder], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|e| e.to_string())
}

fn existing_thread_ids(
    conn: &Connection,
    account_id: i64,
    folder: &str,
) -> AppResult<HashSet<String>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT thread_id
            FROM messages
            WHERE account_id = ? AND folder = ? AND provider_message_id NOT LIKE 'local-sent-%'
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, folder], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|e| e.to_string())
}

fn delete_missing_provider_ids(
    conn: &Connection,
    account_id: i64,
    folder: &str,
    upstream_ids: &HashSet<String>,
) -> AppResult<usize> {
    let mut stmt = conn
        .prepare(
            "
            SELECT id, provider_message_id
            FROM messages
            WHERE account_id = ? AND folder = ? AND provider_message_id NOT LIKE 'local-sent-%'
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, folder], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let local_rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    let mut removed = 0;
    for (id, provider_id) in local_rows {
        if !upstream_ids.contains(&provider_id) {
            delete_message_row(conn, id)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn delete_missing_thread_ids(
    conn: &Connection,
    account_id: i64,
    folder: &str,
    upstream_ids: &HashSet<String>,
) -> AppResult<usize> {
    let mut stmt = conn
        .prepare(
            "
            SELECT id, thread_id
            FROM messages
            WHERE account_id = ? AND folder = ? AND provider_message_id NOT LIKE 'local-sent-%'
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, folder], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let local_rows = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    let mut removed = 0;
    for (id, thread_id) in local_rows {
        if !upstream_ids.contains(&thread_id) {
            delete_message_row(conn, id)?;
            removed += 1;
        }
    }
    Ok(removed)
}

#[tauri::command]
fn list_accounts(state: State<'_, AppState>) -> AppResult<Vec<AccountSummary>> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "
            SELECT id, provider, email, display_name, imap_host, imap_port,
                   smtp_host, smtp_port, username
            FROM accounts
            ORDER BY created_at DESC
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok(AccountSummary {
                id: row.get(0)?,
                provider: row.get(1)?,
                email: row.get(2)?,
                display_name: row.get(3)?,
                imap_host: row.get(4)?,
                imap_port: row.get(5)?,
                smtp_host: row.get(6)?,
                smtp_port: row.get(7)?,
                username: row.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn mailbox_snapshot(
    state: State<'_, AppState>,
    filter: MessageFilter,
) -> AppResult<MailboxSnapshot> {
    let page = filter.page.unwrap_or(0).max(0);
    let page_size = filter.page_size.unwrap_or(50).clamp(10, 100);
    let total = count_messages(state.clone(), &filter)?;
    Ok(MailboxSnapshot {
        accounts: list_accounts(state.clone())?,
        folders: list_folders(state.clone(), filter.account_id)?,
        messages: list_messages(state, filter)?,
        page,
        page_size,
        total,
    })
}

#[tauri::command]
fn list_folders(
    state: State<'_, AppState>,
    account_id: Option<i64>,
) -> AppResult<Vec<FolderSummary>> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let mut folders = vec![
        FolderSummary {
            name: "Inbox".to_string(),
            count: 0,
            unread_count: 0,
        },
        FolderSummary {
            name: "Sent".to_string(),
            count: 0,
            unread_count: 0,
        },
        FolderSummary {
            name: "Trash".to_string(),
            count: 0,
            unread_count: 0,
        },
        FolderSummary {
            name: "Archive".to_string(),
            count: 0,
            unread_count: 0,
        },
    ];

    let mut counts = HashMap::<String, (i64, i64)>::new();
    if let Some(account_id) = account_id {
        let mut stmt = conn
            .prepare(
                "
                SELECT folder, COUNT(*), COALESCE(SUM(CASE WHEN is_read = 0 THEN 1 ELSE 0 END), 0)
                FROM messages
                WHERE account_id = ?
                GROUP BY folder
                ",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![account_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (folder, values) = row.map_err(|e| e.to_string())?;
            counts.insert(folder, values);
        }
    } else {
        let mut stmt = conn
            .prepare(
                "
                SELECT folder, COUNT(*), COALESCE(SUM(CASE WHEN is_read = 0 THEN 1 ELSE 0 END), 0)
                FROM messages
                GROUP BY folder
                ",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?),
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (folder, values) = row.map_err(|e| e.to_string())?;
            counts.insert(folder, values);
        }
    }

    for folder in &mut folders {
        if let Some((count, unread_count)) = counts.get(&folder.name) {
            folder.count = *count;
            folder.unread_count = *unread_count;
        }
    }
    Ok(folders)
}

#[tauri::command]
fn list_messages(
    state: State<'_, AppState>,
    filter: MessageFilter,
) -> AppResult<Vec<StoredMessage>> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let query = filter.query.unwrap_or_default();
    let page = filter.page.unwrap_or(0).max(0);
    let page_size = filter.page_size.unwrap_or(50).clamp(10, 100);
    let offset = page * page_size;
    let read_filter = read_filter_value(filter.read_filter.as_deref());
    let fts_query = fts_query_value(&query);
    let (where_sql, mut sql_params) = message_where_clause(
        filter.folder.as_deref(),
        filter.account_id,
        read_filter,
        fts_query,
    );

    let sql = if query.trim().is_empty() {
        format!(
            "
        SELECT m.id, m.account_id, m.provider_message_id, m.thread_id, m.folder, m.subject,
               m.from_addr, m.to_addr, m.cc_addr, m.date_ts, m.snippet, m.body, m.body_mime, m.is_read,
               a.email, a.provider
        FROM messages m
        JOIN accounts a ON a.id = m.account_id
        WHERE {where_sql}
        ORDER BY m.date_ts DESC
        LIMIT ? OFFSET ?
        "
        )
    } else {
        format!(
            "
        SELECT m.id, m.account_id, m.provider_message_id, m.thread_id, m.folder, m.subject,
               m.from_addr, m.to_addr, m.cc_addr, m.date_ts, m.snippet, m.body, m.body_mime, m.is_read,
               a.email, a.provider
        FROM messages_fts f
        JOIN messages m ON m.id = f.rowid
        JOIN accounts a ON a.id = m.account_id
        WHERE {where_sql}
        ORDER BY m.date_ts DESC
        LIMIT ? OFFSET ?
        "
        )
    };

    sql_params.push(SqlValue::Integer(page_size));
    sql_params.push(SqlValue::Integer(offset));

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(sql_params.iter()), |row| {
            let id = row.get(0)?;
            Ok(StoredMessage {
                id,
                account_id: row.get(1)?,
                provider_message_id: row.get(2)?,
                thread_id: row.get(3)?,
                folder: row.get(4)?,
                subject: row.get(5)?,
                from_addr: row.get(6)?,
                to_addr: row.get(7)?,
                cc_addr: row.get(8)?,
                date_ts: row.get(9)?,
                snippet: row.get(10)?,
                body: row.get(11)?,
                body_mime: row.get(12)?,
                is_read: row.get::<_, i64>(13)? == 1,
                account_email: row.get(14)?,
                account_provider: row.get(15)?,
                attachments: Vec::new(),
            })
        })
        .map_err(|e| e.to_string())?;

    let mut messages = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    let attachments = list_attachments_for_messages(
        &conn,
        &messages
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
    )?;
    for message in &mut messages {
        message.attachments = attachments.get(&message.id).cloned().unwrap_or_default();
    }
    Ok(messages)
}

fn count_messages(state: State<'_, AppState>, filter: &MessageFilter) -> AppResult<i64> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let query = filter.query.clone().unwrap_or_default();
    let read_filter = read_filter_value(filter.read_filter.as_deref());
    let (where_sql, sql_params) = message_where_clause(
        filter.folder.as_deref(),
        filter.account_id,
        read_filter,
        fts_query_value(&query),
    );
    let sql = if query.trim().is_empty() {
        format!(
            "
        SELECT COUNT(*)
        FROM messages m
        WHERE {where_sql}
        "
        )
    } else {
        format!(
            "
        SELECT COUNT(*)
        FROM messages_fts f
        JOIN messages m ON m.id = f.rowid
        WHERE {where_sql}
        "
        )
    };
    conn.query_row(&sql, rusqlite::params_from_iter(sql_params.iter()), |row| {
        row.get(0)
    })
    .map_err(|e| e.to_string())
}

fn fts_query_value(query: &str) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("\"{}\"", trimmed.replace('"', "\"\"")))
    }
}

fn message_where_clause(
    folder: Option<&str>,
    account_id: Option<i64>,
    read_filter: Option<i64>,
    fts_query: Option<String>,
) -> (String, Vec<SqlValue>) {
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    if let Some(fts_query) = fts_query {
        clauses.push("messages_fts MATCH ?");
        params.push(SqlValue::Text(fts_query));
    }
    if let Some(folder) = folder {
        clauses.push("m.folder = ?");
        params.push(SqlValue::Text(folder.to_string()));
    }
    if let Some(account_id) = account_id {
        clauses.push("m.account_id = ?");
        params.push(SqlValue::Integer(account_id));
    }
    if let Some(read_filter) = read_filter {
        clauses.push("m.is_read = ?");
        params.push(SqlValue::Integer(read_filter));
    }
    if clauses.is_empty() {
        ("1 = 1".to_string(), params)
    } else {
        (clauses.join(" AND "), params)
    }
}

fn read_filter_value(value: Option<&str>) -> Option<i64> {
    match value {
        Some("read") => Some(1),
        Some("unread") => Some(0),
        _ => None,
    }
}

fn list_attachments_for_messages(
    conn: &Connection,
    message_ids: &[i64],
) -> AppResult<HashMap<i64, Vec<AttachmentSummary>>> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = std::iter::repeat("?")
        .take(message_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "
        SELECT message_id, id, filename, mime_type, size
        FROM attachments
        WHERE message_id IN ({placeholders})
        ORDER BY message_id, id
        "
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(message_ids.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                AttachmentSummary {
                    id: row.get(1)?,
                    filename: row.get(2)?,
                    mime_type: row.get(3)?,
                    size: row.get(4)?,
                },
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut attachments = HashMap::<i64, Vec<AttachmentSummary>>::new();
    for row in rows {
        let (message_id, attachment) = row.map_err(|e| e.to_string())?;
        attachments.entry(message_id).or_default().push(attachment);
    }
    Ok(attachments)
}

#[tauri::command]
fn connect_imap(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: ImapConnectInput,
) -> AppResult<AccountSummary> {
    debug(
        &app,
        format!(
            "IMAP connect start email={} imap={}:{} smtp={}:{} username={}",
            input.email,
            input.imap_host,
            input.imap_port,
            input.smtp_host,
            input.smtp_port,
            input.username
        ),
    );
    debug(&app, "IMAP building TLS connector");
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    debug(&app, "IMAP opening TCP/TLS connection with 15s timeout");
    let client = connect_imap_client(input.imap_host.as_str(), input.imap_port as u16, &tls)?;
    debug(
        &app,
        "IMAP TCP/TLS ready, attempting login with 15s socket timeout",
    );
    let mut session = client.login(input.username.as_str(), input.password.as_str()).map_err(|e| {
        format!(
            "IMAP login failed or timed out after 15s. Check that IMAP is enabled and the password is valid for IMAP/app-password login. Server error: {}",
            e.0
        )
    })?;
    debug(&app, "IMAP login succeeded");
    session.logout().ok();

    debug(&app, "Saving IMAP account to SQLite");
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "
        INSERT INTO accounts (
            provider, email, display_name, imap_host, imap_port, smtp_host, smtp_port,
            username, password, created_at
        )
        VALUES ('imap', ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
        params![
            input.email,
            input.display_name,
            input.imap_host,
            input.imap_port,
            input.smtp_host,
            input.smtp_port,
            input.username,
            input.password,
            now_ts()
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = conn.last_insert_rowid();
    debug(&app, format!("IMAP account saved id={id}"));
    Ok(AccountSummary {
        id,
        provider: "imap".to_string(),
        email: input.email,
        display_name: input.display_name,
        imap_host: Some(input.imap_host),
        imap_port: Some(input.imap_port),
        smtp_host: Some(input.smtp_host),
        smtp_port: Some(input.smtp_port),
        username: Some(input.username),
    })
}

#[tauri::command]
fn connect_gmail(
    state: State<'_, AppState>,
    input: GmailConnectInput,
) -> AppResult<AccountSummary> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let oauth_state = Uuid::new_v4().to_string();
    let scope =
        "https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/gmail.send";
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        urlencoding::encode(&input.client_id),
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(scope),
        urlencoding::encode(&oauth_state),
    );

    open::that(&auth_url).map_err(|e| format!("Could not open browser: {e}"))?;

    let callback = wait_for_oauth_callback(listener, &oauth_state)?;
    let client = Client::new();
    let token: Value = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("code", callback.as_str()),
            ("client_id", input.client_id.as_str()),
            ("client_secret", input.client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    let access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Google did not return an access token".to_string())?
        .to_string();
    let refresh_token = token
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "Google did not return a refresh token. Remove prior app consent and try again."
                .to_string()
        })?
        .to_string();
    let expires_in = token
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3600);

    let profile: Value = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
        .bearer_auth(&access_token)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let email = profile
        .get("emailAddress")
        .and_then(Value::as_str)
        .unwrap_or("gmail-account")
        .to_string();

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "
        INSERT INTO accounts (
            provider, email, access_token, refresh_token, token_expires_at,
            client_id, client_secret, created_at
        )
        VALUES ('gmail', ?, ?, ?, ?, ?, ?, ?)
        ",
        params![
            email,
            access_token,
            refresh_token,
            now_ts() + expires_in - 60,
            input.client_id,
            input.client_secret,
            now_ts()
        ],
    )
    .map_err(|e| e.to_string())?;
    let id = conn.last_insert_rowid();

    Ok(AccountSummary {
        id,
        provider: "gmail".to_string(),
        email,
        display_name: None,
        imap_host: None,
        imap_port: None,
        smtp_host: None,
        smtp_port: None,
        username: None,
    })
}

fn wait_for_oauth_callback(listener: TcpListener, expected_state: &str) -> AppResult<String> {
    for _ in 0..1800 {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buffer = [0_u8; 4096];
                let n = stream.read(&mut buffer).map_err(|e| e.to_string())?;
                let request = String::from_utf8_lossy(&buffer[..n]);
                let first_line = request.lines().next().unwrap_or_default();
                let path = first_line
                    .split_whitespace()
                    .nth(1)
                    .ok_or_else(|| "Bad OAuth callback".to_string())?;
                let code = query_param(path, "code")
                    .ok_or_else(|| "OAuth callback missing code".to_string())?;
                let state = query_param(path, "state")
                    .ok_or_else(|| "OAuth callback missing state".to_string())?;
                let body = b"<html><body><h2>Mailwind connected. You can close this tab.</h2></body></html>";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).ok();
                stream.write_all(body).ok();
                if state != expected_state {
                    return Err("OAuth state mismatch".to_string());
                }
                return Ok(code);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Timed out waiting for Google OAuth callback".to_string())
}

fn query_param(path: &str, name: &str) -> Option<String> {
    let (_, query) = path.split_once('?')?;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            return Some(
                value
                    .replace("%2F", "/")
                    .replace("%3A", ":")
                    .replace("%20", " "),
            );
        }
    }
    None
}

#[tauri::command]
fn sync_all(app: tauri::AppHandle) -> AppResult<()> {
    start_sync_job(app, None)
}

#[tauri::command]
fn sync_account(app: tauri::AppHandle, account_id: i64) -> AppResult<()> {
    start_sync_job(app, Some(account_id))
}

fn start_sync_job(app: tauri::AppHandle, account_id: Option<i64>) -> AppResult<()> {
    {
        let state = app.state::<AppState>();
        let mut syncing = state.syncing.lock().map_err(|e| e.to_string())?;
        if *syncing {
            return Err("Sync already running".to_string());
        }
        *syncing = true;
    }

    debug(&app, "Sync queued in background");
    let app_for_thread = app.clone();
    thread::spawn(move || {
        let result = run_sync_job(&app_for_thread, account_id);
        if let Err(error) = result {
            debug(&app_for_thread, format!("Sync failed: {error}"));
        }
        {
            let state = app_for_thread.state::<AppState>();
            let lock_result = state.syncing.lock();
            match lock_result {
                Ok(mut syncing) => {
                    *syncing = false;
                }
                Err(error) => {
                    eprintln!("[mailwind] could not reset sync flag: {error}");
                }
            }
        }
    });
    Ok(())
}

fn start_mail_action_job<F>(app: tauri::AppHandle, label: &'static str, job: F) -> AppResult<()>
where
    F: FnOnce(&tauri::AppHandle) -> AppResult<()> + Send + 'static,
{
    debug(&app, format!("{label} queued in background"));
    thread::spawn(move || {
        debug(&app, format!("{label} started"));
        match job(&app) {
            Ok(()) => debug(&app, format!("Mail action complete: {label}")),
            Err(error) => debug(&app, format!("Mail action failed: {label}: {error}")),
        }
    });
    Ok(())
}

fn run_sync_job(app: &tauri::AppHandle, account_id: Option<i64>) -> AppResult<()> {
    let state = app.state::<AppState>();
    let accounts = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        if let Some(account_id) = account_id {
            vec![get_account(&conn, account_id)?]
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT id, provider, email, display_name, access_token, refresh_token, token_expires_at,
                            client_id, client_secret, imap_host, imap_port, smtp_host, smtp_port, username, password
                     FROM accounts ORDER BY id",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], row_to_account)
                .map_err(|e| e.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?
        }
    };

    if accounts.is_empty() {
        debug(app, "Sync complete messages_seen=0");
        return Ok(());
    }

    let worker_count = sync_worker_count(accounts.len(), account_id.is_some());
    debug(
        app,
        format!(
            "Sync workers starting accounts={} workers={} cpus={}",
            accounts.len(),
            worker_count,
            available_cpu_count()
        ),
    );

    if worker_count == 1 {
        let mut synced = 0;
        for account in accounts {
            synced += sync_one_account(app, account)?;
        }
        debug(app, format!("Sync complete messages_seen={synced}"));
        return Ok(());
    }

    let queue = Arc::new(Mutex::new(VecDeque::from(accounts)));
    let synced_total = Arc::new(Mutex::new(0usize));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut workers = Vec::new();

    for worker_index in 0..worker_count {
        let queue = Arc::clone(&queue);
        let synced_total = Arc::clone(&synced_total);
        let errors = Arc::clone(&errors);
        let worker_app = app.clone();
        workers.push(thread::spawn(move || loop {
            let account = {
                let mut queue = match queue.lock() {
                    Ok(queue) => queue,
                    Err(error) => {
                        debug(&worker_app, format!("Sync queue lock failed: {error}"));
                        break;
                    }
                };
                queue.pop_front()
            };
            let Some(account) = account else {
                break;
            };
            match sync_one_account(&worker_app, account.clone()) {
                Ok(count) => {
                    if let Ok(mut synced_total) = synced_total.lock() {
                        *synced_total += count;
                    }
                }
                Err(error) => {
                    let message = format!("{}: {error}", account.email);
                    debug(
                        &worker_app,
                        format!("Sync worker {} failed {message}", worker_index + 1),
                    );
                    if let Ok(mut errors) = errors.lock() {
                        errors.push(message);
                    }
                }
            }
        }));
    }

    for worker in workers {
        if worker.join().is_err() {
            let mut errors = errors.lock().map_err(|e| e.to_string())?;
            errors.push("sync worker panicked".to_string());
        }
    }

    let synced = *synced_total.lock().map_err(|e| e.to_string())?;
    let errors = errors.lock().map_err(|e| e.to_string())?;
    debug(app, format!("Sync complete messages_seen={synced}"));
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "{} account sync(s) failed: {}",
            errors.len(),
            errors.join("; ")
        ))
    }
}

fn sync_one_account(app: &tauri::AppHandle, account: StoredAccount) -> AppResult<usize> {
    let state = app.state::<AppState>();
    debug(
        app,
        format!(
            "Sync account id={} provider={} email={}",
            account.id, account.provider, account.email
        ),
    );
    sync_account_inner(Some(app), &state, account)
}

fn available_cpu_count() -> usize {
    thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(3)
}

fn sync_worker_count(account_count: usize, force_serial: bool) -> usize {
    if force_serial {
        return 1;
    }
    let cpu_based = (available_cpu_count() / 3).max(1);
    account_count.min(cpu_based).max(1)
}

fn start_mail_polling(app: tauri::AppHandle) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(8));
        loop {
            if let Err(error) = run_poll_tick(&app) {
                debug(&app, format!("Live poll failed: {error}"));
            }
            thread::sleep(Duration::from_secs(35));
        }
    });
}

fn start_imap_idle_manager(app: tauri::AppHandle) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(5));
        loop {
            if let Err(error) = spawn_missing_idle_watchers(&app) {
                debug(&app, format!("IMAP IDLE manager failed: {error}"));
            }
            thread::sleep(Duration::from_secs(30));
        }
    });
}

fn spawn_missing_idle_watchers(app: &tauri::AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let accounts = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, provider, email, display_name, access_token, refresh_token, token_expires_at,
                        client_id, client_secret, imap_host, imap_port, smtp_host, smtp_port, username, password
                 FROM accounts
                 WHERE provider = 'imap'
                 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], row_to_account)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    for account in accounts {
        let should_spawn = {
            let mut idle_accounts = state.idle_accounts.lock().map_err(|e| e.to_string())?;
            if idle_accounts.contains(&account.id) {
                false
            } else {
                idle_accounts.insert(account.id);
                true
            }
        };
        if should_spawn {
            let app_for_thread = app.clone();
            thread::spawn(move || run_imap_idle_watcher(app_for_thread, account));
        }
    }
    Ok(())
}

fn run_imap_idle_watcher(app: tauri::AppHandle, account: StoredAccount) {
    debug(
        &app,
        format!("IMAP IDLE watcher starting email={}", account.email),
    );
    loop {
        match idle_account_current(&app, &account) {
            Ok(true) => {}
            Ok(false) => {
                debug(
                    &app,
                    format!("IMAP IDLE watcher stopping email={}", account.email),
                );
                clear_idle_account(&app, account.id);
                break;
            }
            Err(error) => {
                debug(
                    &app,
                    format!(
                        "IMAP IDLE account check failed email={} error={error}",
                        account.email
                    ),
                );
                thread::sleep(Duration::from_secs(20));
                continue;
            }
        }
        match wait_for_imap_idle_change(&account) {
            Ok(()) => {
                debug(&app, format!("IMAP IDLE changed email={}", account.email));
                if let Err(error) = run_poll_tick(&app) {
                    debug(&app, format!("IMAP IDLE poll failed: {error}"));
                }
            }
            Err(error) => {
                debug(
                    &app,
                    format!(
                        "IMAP IDLE watcher reconnecting email={} error={error}",
                        account.email
                    ),
                );
                thread::sleep(Duration::from_secs(20));
            }
        }
    }
}

fn wait_for_imap_idle_change(account: &StoredAccount) -> AppResult<()> {
    let mut session = open_imap_session(account)?;
    session.select("INBOX").map_err(|e| e.to_string())?;
    let idle = session.idle().map_err(|e| e.to_string())?;
    idle.wait_with_timeout(Duration::from_secs(5 * 60))
        .map_err(|e| e.to_string())?;
    session.logout().ok();
    Ok(())
}

fn idle_account_current(app: &tauri::AppHandle, account: &StoredAccount) -> AppResult<bool> {
    let state = app.state::<AppState>();
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let Some(current) = conn
        .query_row(
            "SELECT id, provider, email, display_name, access_token, refresh_token, token_expires_at,
                    client_id, client_secret, imap_host, imap_port, smtp_host, smtp_port, username, password
             FROM accounts WHERE id = ?",
            params![account.id],
            row_to_account,
        )
        .optional()
        .map_err(|e| e.to_string())?
    else {
        return Ok(false);
    };
    Ok(current.provider == account.provider
        && current.email == account.email
        && current.imap_host == account.imap_host
        && current.imap_port == account.imap_port
        && current.username == account.username
        && current.password == account.password)
}

fn clear_idle_account(app: &tauri::AppHandle, account_id: i64) {
    let state = app.state::<AppState>();
    let lock_result = state.idle_accounts.lock();
    if let Ok(mut idle_accounts) = lock_result {
        idle_accounts.remove(&account_id);
    }
}

fn run_poll_tick(app: &tauri::AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    {
        let syncing = state.syncing.lock().map_err(|e| e.to_string())?;
        if *syncing {
            return Ok(());
        }
    }
    {
        let mut polling = state.polling.lock().map_err(|e| e.to_string())?;
        if *polling {
            return Ok(());
        }
        *polling = true;
    }

    let result = run_poll_job(app);
    {
        let mut polling = state.polling.lock().map_err(|e| e.to_string())?;
        *polling = false;
    }
    result
}

fn run_poll_job(app: &tauri::AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let accounts = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, provider, email, display_name, access_token, refresh_token, token_expires_at,
                        client_id, client_secret, imap_host, imap_port, smtp_host, smtp_port, username, password
                 FROM accounts ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], row_to_account)
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    let mut new_messages = 0;
    let mut changed = 0;
    for account in accounts {
        let (account_new, account_changed) = if account.provider == "gmail" {
            poll_gmail_inbox(&state, &account)?
        } else {
            poll_imap_inbox(&state, &account)?
        };
        new_messages += account_new;
        changed += account_new + account_changed;
    }

    if changed > 0 {
        let unread_inbox = inbox_unread_count(&state)?;
        let payload = MailboxChanged {
            new_messages,
            unread_inbox,
        };
        let _ = app.emit("mailwind-mailbox-changed", payload);
        debug(
            app,
            format!("Live mailbox updated new={new_messages} unread_inbox={unread_inbox}"),
        );
    }
    Ok(())
}

fn poll_gmail_inbox(
    state: &State<'_, AppState>,
    account: &StoredAccount,
) -> AppResult<(usize, usize)> {
    let access_token = gmail_access_token(state, account)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let list: Value = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults=30&labelIds=INBOX")
        .bearer_auth(&access_token)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let Some(messages) = list.get("messages").and_then(Value::as_array) else {
        return Ok((0, 0));
    };

    let existing_ids = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        existing_provider_ids(&conn, account.id, "Inbox")?
    };
    let mut new_messages = 0;
    let mut changed = 0;
    for item in messages {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let url =
            format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full");
        let raw: Value = client
            .get(url)
            .bearer_auth(&access_token)
            .send()
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .json()
            .map_err(|e| e.to_string())?;
        let message = gmail_to_message("Inbox", &raw);
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        if existing_ids.contains(id) {
            if update_message_read_by_provider(&conn, account.id, id, message.is_read)? {
                changed += 1;
            }
        } else {
            upsert_message(&conn, account.id, &message)?;
            new_messages += 1;
        }
    }
    Ok((new_messages, changed))
}

fn poll_imap_inbox(
    state: &State<'_, AppState>,
    account: &StoredAccount,
) -> AppResult<(usize, usize)> {
    let mut session = open_imap_session(account)?;
    let folder = "INBOX";
    session.select(folder).map_err(|e| e.to_string())?;
    let uids = session.uid_search("UNDELETED").map_err(|e| e.to_string())?;
    let mut selected = uids.into_iter().collect::<Vec<_>>();
    selected.sort_unstable();
    selected.reverse();
    selected.truncate(30);
    if selected.is_empty() {
        let removed = {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            delete_missing_thread_ids(&conn, account.id, "Inbox", &HashSet::new())?
        };
        session.logout().ok();
        return Ok((0, removed));
    }

    let existing_threads = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        existing_thread_ids(&conn, account.id, "Inbox")?
    };
    let set = selected
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fetches = session
        .uid_fetch(set, "(FLAGS)")
        .map_err(|e| e.to_string())?;
    let mut missing_uids = Vec::new();
    let mut changed = 0;
    for fetch in fetches.iter() {
        let uid = fetch.uid.unwrap_or(fetch.message);
        let thread_id = format!("{folder}:{uid}");
        if fetch.flags().contains(&imap::types::Flag::Deleted) {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            changed += delete_messages_by_thread(&conn, account.id, &thread_id)?;
            continue;
        }
        let is_read = fetch.flags().contains(&imap::types::Flag::Seen);
        if existing_threads.contains(&thread_id) {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            if update_message_read_by_thread(&conn, account.id, &thread_id, is_read)? {
                changed += 1;
            }
        } else {
            missing_uids.push(uid);
        }
    }

    let mut new_messages = 0;
    if !missing_uids.is_empty() {
        let set = missing_uids
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fetches = session
            .uid_fetch(set, "(BODY.PEEK[] FLAGS)")
            .map_err(|e| e.to_string())?;
        for fetch in fetches.iter() {
            let Some(body) = fetch.body() else {
                continue;
            };
            let uid = fetch.uid.unwrap_or(fetch.message);
            let message = imap_to_message(
                "Inbox",
                folder,
                uid,
                body,
                fetch.flags().contains(&imap::types::Flag::Seen),
            )?;
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            upsert_message(&conn, account.id, &message)?;
            new_messages += 1;
        }
    }
    session.logout().ok();
    Ok((new_messages, changed))
}

fn update_message_read_by_provider(
    conn: &Connection,
    account_id: i64,
    provider_message_id: &str,
    is_read: bool,
) -> AppResult<bool> {
    let current = conn
        .query_row(
            "SELECT is_read FROM messages WHERE account_id = ? AND provider_message_id = ?",
            params![account_id, provider_message_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    update_read_if_changed(
        conn,
        current,
        "provider_message_id",
        account_id,
        provider_message_id,
        is_read,
    )
}

fn update_message_read_by_thread(
    conn: &Connection,
    account_id: i64,
    thread_id: &str,
    is_read: bool,
) -> AppResult<bool> {
    let current = conn
        .query_row(
            "SELECT is_read FROM messages WHERE account_id = ? AND thread_id = ?",
            params![account_id, thread_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    update_read_if_changed(conn, current, "thread_id", account_id, thread_id, is_read)
}

fn update_read_if_changed(
    conn: &Connection,
    current: Option<i64>,
    id_column: &str,
    account_id: i64,
    id_value: &str,
    is_read: bool,
) -> AppResult<bool> {
    let next = if is_read { 1 } else { 0 };
    if current != Some(next) {
        conn.execute(
            &format!("UPDATE messages SET is_read = ?, updated_at = ? WHERE account_id = ? AND {id_column} = ?"),
            params![next, now_ts(), account_id, id_value],
        )
        .map_err(|e| e.to_string())?;
        return Ok(current.is_some());
    }
    Ok(false)
}

fn inbox_unread_count(state: &State<'_, AppState>) -> AppResult<i64> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE folder = 'Inbox' AND is_read = 0",
        [],
        |row| row.get(0),
    )
    .map_err(|e| e.to_string())
}

fn sync_account_inner(
    app: Option<&tauri::AppHandle>,
    state: &State<'_, AppState>,
    account: StoredAccount,
) -> AppResult<usize> {
    let count = if account.provider == "gmail" {
        if let Some(app) = app {
            debug(app, "Gmail sync fetch start");
        }
        sync_gmail_messages(app, state, &account)?
    } else {
        if let Some(app) = app {
            debug(app, "IMAP sync fetch start");
        }
        sync_imap_messages(app, state, &account)?
    };

    if let Some(app) = app {
        debug(
            app,
            format!(
                "Sync account complete email={} messages_seen={count}",
                account.email
            ),
        );
    }
    Ok(count)
}

fn gmail_access_token(state: &State<'_, AppState>, account: &StoredAccount) -> AppResult<String> {
    if account.token_expires_at.unwrap_or_default() > now_ts() + 30 {
        return account
            .access_token
            .clone()
            .ok_or_else(|| "Missing Gmail access token".to_string());
    }

    let refresh_token = account
        .refresh_token
        .as_deref()
        .ok_or_else(|| "Missing Gmail refresh token".to_string())?;
    let client_id = account
        .client_id
        .as_deref()
        .ok_or_else(|| "Missing Gmail client id".to_string())?;
    let client_secret = account
        .client_secret
        .as_deref()
        .ok_or_else(|| "Missing Gmail client secret".to_string())?;

    let token: Value = Client::new()
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;

    let access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "Google refresh did not return an access token".to_string())?
        .to_string();
    let expires_in = token
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3600);

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE accounts SET access_token = ?, token_expires_at = ? WHERE id = ?",
        params![access_token, now_ts() + expires_in - 60, account.id],
    )
    .map_err(|e| e.to_string())?;
    Ok(access_token)
}

fn sync_gmail_messages(
    app: Option<&tauri::AppHandle>,
    state: &State<'_, AppState>,
    account: &StoredAccount,
) -> AppResult<usize> {
    let access_token = gmail_access_token(state, account)?;
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let labels = [("Inbox", "INBOX"), ("Sent", "SENT"), ("Trash", "TRASH")];
    let mut count = 0;
    let page_size = 100;

    for (folder, label) in labels {
        let mut page_token: Option<String> = None;
        let mut folder_count = 0;
        let mut skipped_existing = 0;
        let mut upstream_ids = HashSet::new();
        let existing_ids = {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            existing_provider_ids(&conn, account.id, folder)?
        };

        loop {
            let mut list_url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults={page_size}&labelIds={label}"
            );
            if let Some(token) = page_token.as_deref() {
                list_url.push_str("&pageToken=");
                list_url.push_str(&urlencoding::encode(token));
            }
            let list: Value = client
                .get(list_url)
                .bearer_auth(&access_token)
                .send()
                .map_err(|e| e.to_string())?
                .error_for_status()
                .map_err(|e| e.to_string())?
                .json()
                .map_err(|e| e.to_string())?;

            let Some(messages) = list.get("messages").and_then(Value::as_array) else {
                break;
            };

            if messages.is_empty() {
                break;
            }

            for item in messages {
                let Some(id) = item.get("id").and_then(Value::as_str) else {
                    continue;
                };
                upstream_ids.insert(id.to_string());
                if existing_ids.contains(id) {
                    let conn = state.db.lock().map_err(|e| e.to_string())?;
                    conn.execute(
                        "UPDATE messages SET folder = ?, updated_at = ? WHERE account_id = ? AND provider_message_id = ?",
                        params![folder, now_ts(), account.id, id],
                    )
                    .map_err(|e| e.to_string())?;
                    skipped_existing += 1;
                    continue;
                }
                let url = format!(
                    "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}?format=full"
                );
                let raw: Value = client
                    .get(url)
                    .bearer_auth(&access_token)
                    .send()
                    .map_err(|e| e.to_string())?
                    .error_for_status()
                    .map_err(|e| e.to_string())?
                    .json()
                    .map_err(|e| e.to_string())?;
                let message = gmail_to_message(folder, &raw);
                let conn = state.db.lock().map_err(|e| e.to_string())?;
                upsert_message(&conn, account.id, &message)?;
                count += 1;
                folder_count += 1;
                if folder_count % 100 == 0 {
                    if let Some(app) = app {
                        debug(
                            app,
                            format!(
                                "Gmail sync progress email={} folder={} messages_seen={folder_count}",
                                account.email, folder
                            ),
                        );
                    }
                }
            }

            page_token = list
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if page_token.is_none() {
                break;
            }
        }
        let removed = {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            delete_missing_provider_ids(&conn, account.id, folder, &upstream_ids)?
        };
        if let Some(app) = app {
            debug(
                app,
                format!(
                    "Gmail sync folder complete email={} folder={} new={folder_count} existing={skipped_existing} removed={removed}",
                    account.email, folder
                ),
            );
        }
    }

    Ok(count)
}

fn gmail_to_message(folder: &str, raw: &Value) -> NewMessage {
    let payload = raw.get("payload").unwrap_or(&Value::Null);
    let subject = gmail_header(payload, "Subject").unwrap_or_else(|| "(no subject)".to_string());
    let from_addr = gmail_header(payload, "From").unwrap_or_default();
    let to_addr = gmail_header(payload, "To").unwrap_or_default();
    let cc_addr = gmail_header(payload, "Cc").unwrap_or_default();
    let date_ts = gmail_header(payload, "Date")
        .and_then(|date| DateTime::parse_from_rfc2822(&date).ok())
        .map(|date| date.timestamp())
        .or_else(|| {
            raw.get("internalDate")
                .and_then(Value::as_str)
                .and_then(|ms| ms.parse::<i64>().ok())
                .map(|ms| ms / 1000)
        })
        .unwrap_or_else(now_ts);
    let (body, body_mime) = gmail_body(payload);
    let attachments = gmail_attachments(payload);
    let snippet = raw
        .get("snippet")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    NewMessage {
        provider_message_id: raw
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        thread_id: raw
            .get("threadId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        folder: folder.to_string(),
        subject,
        from_addr,
        to_addr,
        cc_addr,
        date_ts,
        snippet,
        body,
        body_mime,
        is_read: !raw
            .get("labelIds")
            .and_then(Value::as_array)
            .map(|labels| labels.iter().any(|label| label.as_str() == Some("UNREAD")))
            .unwrap_or(false),
        attachments,
    }
}

fn gmail_header(payload: &Value, name: &str) -> Option<String> {
    payload
        .get("headers")?
        .as_array()?
        .iter()
        .find(|header| {
            header
                .get("name")
                .and_then(Value::as_str)
                .map(|h| h.eq_ignore_ascii_case(name))
                .unwrap_or(false)
        })
        .and_then(|header| header.get("value").and_then(Value::as_str))
        .map(ToString::to_string)
}

fn gmail_body(payload: &Value) -> (String, String) {
    let mime_type = payload
        .get("mimeType")
        .and_then(Value::as_str)
        .unwrap_or("text/plain")
        .to_ascii_lowercase();
    if let Some(data) = payload
        .get("body")
        .and_then(|body| body.get("data"))
        .and_then(Value::as_str)
    {
        if let Ok(bytes) = general_purpose::URL_SAFE_NO_PAD.decode(data) {
            return (String::from_utf8_lossy(&bytes).to_string(), mime_type);
        }
        if let Ok(bytes) = general_purpose::URL_SAFE.decode(data) {
            return (String::from_utf8_lossy(&bytes).to_string(), mime_type);
        }
    }

    let bodies = payload
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(gmail_body)
                .filter(|(body, _)| !body.trim().is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some((body, mime)) = bodies
        .iter()
        .find(|(_, mime)| mime.eq_ignore_ascii_case("text/html"))
    {
        return (body.clone(), mime.clone());
    }
    bodies
        .first()
        .cloned()
        .unwrap_or_else(|| (String::new(), "text/plain".to_string()))
}

fn gmail_attachments(payload: &Value) -> Vec<NewAttachment> {
    let mut attachments = Vec::new();
    collect_gmail_attachments(payload, &mut attachments);
    attachments
}

fn collect_gmail_attachments(payload: &Value, attachments: &mut Vec<NewAttachment>) {
    let filename = payload
        .get("filename")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !filename.is_empty() {
        attachments.push(NewAttachment {
            provider_attachment_id: payload
                .get("body")
                .and_then(|body| body.get("attachmentId"))
                .and_then(Value::as_str)
                .map(ToString::to_string),
            filename: filename.to_string(),
            mime_type: payload
                .get("mimeType")
                .and_then(Value::as_str)
                .unwrap_or("application/octet-stream")
                .to_string(),
            size: payload
                .get("body")
                .and_then(|body| body.get("size"))
                .and_then(Value::as_i64)
                .unwrap_or(0),
            data: None,
        });
    }

    if let Some(parts) = payload.get("parts").and_then(Value::as_array) {
        for part in parts {
            collect_gmail_attachments(part, attachments);
        }
    }
}

fn sync_imap_messages(
    app: Option<&tauri::AppHandle>,
    state: &State<'_, AppState>,
    account: &StoredAccount,
) -> AppResult<usize> {
    let host = account
        .imap_host
        .as_deref()
        .ok_or_else(|| "Missing IMAP host".to_string())?;
    let port = account.imap_port.unwrap_or(993) as u16;
    let username = account
        .username
        .as_deref()
        .ok_or_else(|| "Missing IMAP username".to_string())?;
    let password = account
        .password
        .as_deref()
        .ok_or_else(|| "Missing IMAP password".to_string())?;
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let client = connect_imap_client(host, port, &tls)?;
    let mut session = client
        .login(username, password)
        .map_err(|e| format!("IMAP login failed or timed out after 15s: {}", e.0))?;
    let folders = [
        ("Inbox", vec!["INBOX"]),
        ("Sent", vec!["Sent", "Sent Items", "[Gmail]/Sent Mail"]),
        ("Trash", vec!["Trash", "Deleted Items", "[Gmail]/Trash"]),
    ];
    let mut count = 0;
    let chunk_size = 100;

    for (role, candidates) in folders {
        for folder in candidates {
            if session.select(folder).is_err() {
                continue;
            }
            let uids = session.uid_search("UNDELETED").map_err(|e| e.to_string())?;
            let mut selected = uids.into_iter().collect::<Vec<_>>();
            selected.sort_unstable();
            selected.reverse();
            if selected.is_empty() {
                let removed = {
                    let conn = state.db.lock().map_err(|e| e.to_string())?;
                    delete_missing_thread_ids(&conn, account.id, role, &HashSet::new())?
                };
                if let Some(app) = app {
                    debug(
                        app,
                        format!(
                            "IMAP sync folder empty email={} folder={} removed={removed}",
                            account.email, role
                        ),
                    );
                }
                break;
            }
            if let Some(app) = app {
                debug(
                    app,
                    format!(
                        "IMAP sync folder selected email={} folder={} uid_count={}",
                        account.email,
                        role,
                        selected.len()
                    ),
                );
            }
            let existing_threads = {
                let conn = state.db.lock().map_err(|e| e.to_string())?;
                existing_thread_ids(&conn, account.id, role)?
            };
            let mut upstream_threads = HashSet::new();
            let mut missing_uids = Vec::new();
            let mut flag_checked = 0;

            for chunk in selected.chunks(chunk_size) {
                let set = chunk
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let fetches = session
                    .uid_fetch(set, "(FLAGS)")
                    .map_err(|e| e.to_string())?;
                for fetch in fetches.iter() {
                    let uid = fetch.uid.unwrap_or(fetch.message);
                    let thread_id = format!("{folder}:{uid}");
                    if fetch.flags().contains(&imap::types::Flag::Deleted) {
                        flag_checked += 1;
                        continue;
                    }
                    upstream_threads.insert(thread_id.clone());
                    let is_read = fetch.flags().contains(&imap::types::Flag::Seen);
                    if existing_threads.contains(&thread_id) {
                        let conn = state.db.lock().map_err(|e| e.to_string())?;
                        conn.execute(
                            "UPDATE messages SET is_read = ?, updated_at = ? WHERE account_id = ? AND thread_id = ?",
                            params![if is_read { 1 } else { 0 }, now_ts(), account.id, thread_id],
                        )
                        .map_err(|e| e.to_string())?;
                    } else {
                        missing_uids.push(uid);
                    }
                    flag_checked += 1;
                }
            }
            let removed = {
                let conn = state.db.lock().map_err(|e| e.to_string())?;
                delete_missing_thread_ids(&conn, account.id, role, &upstream_threads)?
            };
            if let Some(app) = app {
                debug(
                    app,
                    format!(
                        "IMAP sync compare email={} folder={} upstream={} new={} removed={removed}",
                        account.email,
                        role,
                        flag_checked,
                        missing_uids.len()
                    ),
                );
            }
            let mut folder_count = 0;
            let mut processed = 0;
            for chunk in missing_uids.chunks(chunk_size) {
                let set = chunk
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                let fetches = session
                    .uid_fetch(set, "(BODY.PEEK[] FLAGS)")
                    .map_err(|e| e.to_string())?;
                for fetch in fetches.iter() {
                    let Some(body) = fetch.body() else {
                        continue;
                    };
                    let uid = fetch.uid.unwrap_or(fetch.message);
                    let message = match imap_to_message(
                        role,
                        folder,
                        uid,
                        body,
                        fetch.flags().contains(&imap::types::Flag::Seen),
                    ) {
                        Ok(message) => message,
                        Err(error) => {
                            if let Some(app) = app {
                                debug(
                                    app,
                                    format!(
                                        "IMAP skipped unparsable message email={} folder={} uid={} error={}",
                                        account.email, role, uid, error
                                    ),
                                );
                            }
                            continue;
                        }
                    };
                    let conn = state.db.lock().map_err(|e| e.to_string())?;
                    upsert_message(&conn, account.id, &message)?;
                    count += 1;
                    folder_count += 1;
                }
                processed += chunk.len();
                if processed % 500 == 0 || processed >= missing_uids.len() {
                    if let Some(app) = app {
                        debug(
                            app,
                            format!(
                                "IMAP sync progress email={} folder={} fetched={processed}/{} written={folder_count}",
                                account.email,
                                role,
                                missing_uids.len()
                            ),
                        );
                    }
                }
            }
            break;
        }
    }
    session.logout().ok();
    Ok(count)
}

fn imap_to_message(
    role: &str,
    folder: &str,
    uid: u32,
    raw: &[u8],
    is_read: bool,
) -> AppResult<NewMessage> {
    let parsed = mailparse::parse_mail(raw).map_err(|e| e.to_string())?;
    let subject = parsed
        .headers
        .get_first_value("Subject")
        .unwrap_or_else(|| "(no subject)".to_string());
    let from_addr = parsed.headers.get_first_value("From").unwrap_or_default();
    let to_addr = parsed.headers.get_first_value("To").unwrap_or_default();
    let cc_addr = parsed.headers.get_first_value("Cc").unwrap_or_default();
    let provider_message_id = parsed
        .headers
        .get_first_value("Message-ID")
        .unwrap_or_else(|| format!("{folder}:{uid}"));
    let date_ts = parsed
        .headers
        .get_first_value("Date")
        .and_then(|date| DateTime::parse_from_rfc2822(&date).ok())
        .map(|date| date.timestamp())
        .unwrap_or_else(now_ts);
    let (body, body_mime) = parsed_body(&parsed);
    let attachments = parsed_attachments(&parsed);
    let snippet = snippet_for_body(&body, &body_mime);
    Ok(NewMessage {
        provider_message_id: imap_provider_message_id(folder, uid, &provider_message_id),
        thread_id: format!("{folder}:{uid}"),
        folder: role.to_string(),
        subject,
        from_addr,
        to_addr,
        cc_addr,
        date_ts,
        snippet,
        body,
        body_mime,
        is_read,
        attachments,
    })
}

fn snippet_for_body(body: &str, body_mime: &str) -> String {
    let text = if body_mime.eq_ignore_ascii_case("text/html") {
        html_to_text(body)
    } else {
        body.to_string()
    };
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(180)
        .collect()
}

fn html_to_text(value: &str) -> String {
    const DROPPED_HTML_BLOCKS: &[&str] = &["script", "style", "head", "title"];
    let cleaned = remove_html_blocks(value, DROPPED_HTML_BLOCKS);
    let mut text = String::with_capacity(cleaned.len());
    let mut in_tag = false;
    let mut in_entity = false;
    let mut entity = String::new();

    for ch in cleaned.chars() {
        if in_tag {
            if ch == '>' {
                in_tag = false;
                text.push(' ');
            }
            continue;
        }
        if in_entity {
            if ch == ';' {
                text.push_str(match entity.as_str() {
                    "nbsp" => " ",
                    "amp" => "&",
                    "lt" => "<",
                    "gt" => ">",
                    "quot" => "\"",
                    "#39" | "apos" => "'",
                    _ => " ",
                });
                in_entity = false;
                entity.clear();
            } else if entity.len() < 12 {
                entity.push(ch);
            } else {
                in_entity = false;
                entity.clear();
            }
            continue;
        }
        match ch {
            '<' => in_tag = true,
            '&' => in_entity = true,
            _ => text.push(ch),
        }
    }
    text
}

fn remove_html_blocks(value: &str, tags: &[&str]) -> String {
    let mut output = value.to_string();
    for tag in tags {
        loop {
            let lower = output.to_ascii_lowercase();
            let Some(start) = lower.find(&format!("<{tag}")) else {
                break;
            };
            let close = format!("</{tag}>");
            if let Some(relative_end) = lower[start..].find(&close) {
                let end = start + relative_end + close.len();
                output.replace_range(start..end, " ");
            } else if let Some(relative_end) = lower[start..].find('>') {
                output.replace_range(start..start + relative_end + 1, " ");
            } else {
                output.replace_range(start.., " ");
                break;
            }
        }
    }
    output
}

fn imap_provider_message_id(folder: &str, uid: u32, message_id: &str) -> String {
    let message_id = message_id.trim();
    if message_id.is_empty() {
        format!("imap-uid:{folder}:{uid}")
    } else {
        format!("imap-message:{message_id}")
    }
}

fn parsed_body(part: &mailparse::ParsedMail<'_>) -> (String, String) {
    if part.subparts.is_empty() {
        let mimetype = part.ctype.mimetype.to_ascii_lowercase();
        if part.get_content_disposition().disposition == DispositionType::Attachment {
            return (String::new(), "text/plain".to_string());
        }
        if mimetype == "text/plain" || mimetype == "text/html" {
            return (part.get_body().unwrap_or_default(), mimetype);
        }
        return (String::new(), "text/plain".to_string());
    }
    let bodies = part
        .subparts
        .iter()
        .map(parsed_body)
        .filter(|(body, _)| !body.trim().is_empty())
        .collect::<Vec<_>>();
    if let Some((body, mime)) = bodies
        .iter()
        .find(|(_, mime)| mime.eq_ignore_ascii_case("text/html"))
    {
        return (body.clone(), mime.clone());
    }
    bodies
        .first()
        .cloned()
        .unwrap_or_else(|| (String::new(), "text/plain".to_string()))
}

fn parsed_attachments(part: &mailparse::ParsedMail<'_>) -> Vec<NewAttachment> {
    let mut attachments = Vec::new();
    collect_parsed_attachments(part, &mut attachments);
    attachments
}

fn collect_parsed_attachments(
    part: &mailparse::ParsedMail<'_>,
    attachments: &mut Vec<NewAttachment>,
) {
    let disposition = part.get_content_disposition();
    let filename = disposition
        .params
        .get("filename")
        .or_else(|| part.ctype.params.get("name"))
        .cloned();
    if let Some(filename) = filename {
        if disposition.disposition == DispositionType::Attachment || !filename.is_empty() {
            let data = part.get_body_raw().unwrap_or_default();
            attachments.push(NewAttachment {
                provider_attachment_id: None,
                filename,
                mime_type: part.ctype.mimetype.clone(),
                size: data.len() as i64,
                data: Some(data),
            });
        }
    }
    for subpart in &part.subparts {
        collect_parsed_attachments(subpart, attachments);
    }
}

#[tauri::command]
fn send_message(app: tauri::AppHandle, input: SendInput) -> AppResult<()> {
    start_mail_action_job(app, "Send message", move |app| {
        run_send_message(app, input).map(|_| ())
    })
}

fn run_send_message(app: &tauri::AppHandle, input: SendInput) -> AppResult<StoredMessage> {
    let state = app.state::<AppState>();
    let account = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        get_account(&conn, input.account_id)?
    };
    let reply_context = reply_context(&state, input.reply_to_message_id)?;

    if account.provider == "gmail" {
        send_gmail(&state, &account, &input, reply_context.as_ref())?;
    } else {
        send_smtp(&account, &input, reply_context.as_ref())?;
    }

    let sent = NewMessage {
        provider_message_id: format!("local-sent-{}", Uuid::new_v4()),
        thread_id: reply_context
            .as_ref()
            .map(|reply| reply.thread_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        folder: "Sent".to_string(),
        subject: input.subject,
        from_addr: account.email.clone(),
        to_addr: input.to,
        cc_addr: String::new(),
        date_ts: Utc::now().timestamp(),
        snippet: input.body.chars().take(180).collect(),
        body: input.body,
        body_mime: "text/plain".to_string(),
        is_read: true,
        attachments: Vec::new(),
    };

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    upsert_message(&conn, account.id, &sent)?;
    let id = conn
        .query_row(
            "SELECT id FROM messages WHERE account_id = ? AND provider_message_id = ?",
            params![account.id, sent.provider_message_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    Ok(StoredMessage {
        id,
        account_id: account.id,
        provider_message_id: sent.provider_message_id,
        thread_id: sent.thread_id,
        folder: sent.folder,
        subject: sent.subject,
        from_addr: sent.from_addr,
        to_addr: sent.to_addr,
        cc_addr: sent.cc_addr,
        date_ts: sent.date_ts,
        snippet: sent.snippet,
        body: sent.body,
        body_mime: sent.body_mime,
        is_read: true,
        account_email: account.email,
        account_provider: account.provider,
        attachments: Vec::new(),
    })
}

fn send_gmail(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    input: &SendInput,
    reply_context: Option<&ReplyContext>,
) -> AppResult<()> {
    let token = gmail_access_token(state, account)?;
    let raw = raw_reply_message(account, input, reply_context);
    let encoded = general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes());
    let mut body = serde_json::Map::new();
    body.insert("raw".to_string(), Value::String(encoded));
    if let Some(reply_context) = reply_context {
        body.insert(
            "threadId".to_string(),
            Value::String(reply_context.thread_id.clone()),
        );
    }
    let body = Value::Object(body);
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?
        .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn send_smtp(
    account: &StoredAccount,
    input: &SendInput,
    reply_context: Option<&ReplyContext>,
) -> AppResult<()> {
    let host = account
        .smtp_host
        .as_deref()
        .ok_or_else(|| "Missing SMTP host".to_string())?;
    let username = account
        .username
        .as_deref()
        .ok_or_else(|| "Missing SMTP username".to_string())?;
    let password = account
        .password
        .as_deref()
        .ok_or_else(|| "Missing SMTP password".to_string())?;
    let from = account
        .email
        .parse::<Mailbox>()
        .map_err(|e| e.to_string())?;
    let to = input.to.parse::<Mailbox>().map_err(|e| e.to_string())?;
    let mut builder = SmtpMessage::builder()
        .from(from)
        .to(to)
        .subject(&input.subject);
    if let Some(message_id) =
        reply_context.and_then(|reply| reply_header_id(&reply.provider_message_id))
    {
        builder = builder
            .in_reply_to(message_id.clone())
            .references(message_id);
    }
    let email = builder
        .body(input.body.clone())
        .map_err(|e| e.to_string())?;
    let mailer = SmtpTransport::relay(host)
        .map_err(|e| e.to_string())?
        .port(account.smtp_port.unwrap_or(587) as u16)
        .timeout(Some(Duration::from_secs(30)))
        .credentials(Credentials::new(username.to_string(), password.to_string()))
        .build();
    mailer.send(&email).map_err(|e| e.to_string())?;
    Ok(())
}

fn reply_context(
    state: &State<'_, AppState>,
    reply_to_message_id: Option<i64>,
) -> AppResult<Option<ReplyContext>> {
    let Some(message_id) = reply_to_message_id else {
        return Ok(None);
    };
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT provider_message_id, thread_id FROM messages WHERE id = ?",
        rusqlite::params_from_iter([message_id]),
        |row| {
            Ok(ReplyContext {
                provider_message_id: row.get(0)?,
                thread_id: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn raw_reply_message(
    account: &StoredAccount,
    input: &SendInput,
    reply_context: Option<&ReplyContext>,
) -> String {
    let mut raw = format!(
        "From: {}\r\nTo: {}\r\nSubject: {}\r\n",
        account.email, input.to, input.subject
    );
    if let Some(message_id) =
        reply_context.and_then(|reply| reply_header_id(&reply.provider_message_id))
    {
        raw.push_str(&format!(
            "In-Reply-To: {message_id}\r\nReferences: {message_id}\r\n"
        ));
    }
    raw.push_str("Content-Type: text/plain; charset=utf-8\r\n\r\n");
    raw.push_str(&input.body);
    raw
}

fn reply_header_id(provider_message_id: &str) -> Option<String> {
    if provider_message_id.starts_with("local-sent-") {
        return None;
    }
    if let (Some(start), Some(end)) = (
        provider_message_id.find('<'),
        provider_message_id.rfind('>'),
    ) {
        if end > start {
            return provider_message_id
                .get(start..=end)
                .map(ToString::to_string);
        }
    }
    None
}

#[tauri::command]
fn delete_message(app: tauri::AppHandle, input: DeleteInput) -> AppResult<()> {
    start_mail_action_job(app, "Delete message", move |app| {
        run_delete_message(app, input)
    })
}

fn run_delete_message(app: &tauri::AppHandle, input: DeleteInput) -> AppResult<()> {
    let state = app.state::<AppState>();
    let (account, provider_message_id, thread_id, folder) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, provider_message_id, thread_id, folder): (i64, String, String, String) =
            conn.query_row(
                "SELECT account_id, provider_message_id, thread_id, folder FROM messages WHERE id = ?",
                params![input.message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| e.to_string())?;
        (
            get_account(&conn, account_id)?,
            provider_message_id,
            thread_id,
            folder,
        )
    };

    let permanent = folder == "Trash";
    debug(
        &app,
        format!(
            "Delete message id={} provider={} permanent={}",
            input.message_id, account.provider, permanent
        ),
    );

    if !provider_message_id.starts_with("local-sent-") {
        if account.provider == "gmail" {
            delete_gmail_message(&state, &account, &provider_message_id, permanent)?;
        } else {
            delete_imap_message(&account, &thread_id, permanent)?;
        }
    }

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    if permanent {
        conn.execute(
            "DELETE FROM messages_fts WHERE rowid = ?",
            params![input.message_id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute(
            "DELETE FROM messages WHERE id = ?",
            params![input.message_id],
        )
        .map_err(|e| e.to_string())?;
    } else {
        conn.execute(
            "UPDATE messages SET folder = 'Trash', updated_at = ? WHERE id = ?",
            params![now_ts(), input.message_id],
        )
        .map_err(|e| e.to_string())?;
    }
    debug(&app, "Delete complete");
    Ok(())
}

fn delete_gmail_message(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    provider_message_id: &str,
    permanent: bool,
) -> AppResult<()> {
    if provider_message_id.starts_with("local-sent-") {
        return Ok(());
    }
    let token = gmail_access_token(state, account)?;
    let client = Client::new();
    let url = if permanent {
        format!("https://gmail.googleapis.com/gmail/v1/users/me/messages/{provider_message_id}")
    } else {
        format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{provider_message_id}/trash"
        )
    };
    let request = if permanent {
        client.delete(url)
    } else {
        client.post(url)
    };
    request
        .bearer_auth(token)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_imap_message(account: &StoredAccount, thread_id: &str, permanent: bool) -> AppResult<()> {
    let (folder, uid) = parse_imap_thread_id(thread_id)?;
    let mut session = open_imap_session(account)?;
    session.select(&folder).map_err(|e| e.to_string())?;
    if permanent {
        session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .map_err(|e| e.to_string())?;
        session.uid_expunge(uid.to_string()).ok();
        session.expunge().ok();
    } else {
        let mut moved = false;
        for trash in ["Trash", "Deleted Items", "[Gmail]/Trash"] {
            if session.uid_mv(uid.to_string(), trash).is_ok() {
                moved = true;
                break;
            }
            if session.uid_copy(uid.to_string(), trash).is_ok() {
                session
                    .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
                    .map_err(|e| e.to_string())?;
                session.uid_expunge(uid.to_string()).ok();
                session.expunge().ok();
                moved = true;
                break;
            }
        }
        if !moved {
            session
                .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
                .map_err(|e| e.to_string())?;
            session.uid_expunge(uid.to_string()).ok();
            session.expunge().ok();
        }
    }
    session.logout().ok();
    Ok(())
}

fn open_imap_session(
    account: &StoredAccount,
) -> AppResult<imap::Session<native_tls::TlsStream<std::net::TcpStream>>> {
    let host = account
        .imap_host
        .as_deref()
        .ok_or_else(|| "Missing IMAP host".to_string())?;
    let port = account.imap_port.unwrap_or(993) as u16;
    let username = account
        .username
        .as_deref()
        .ok_or_else(|| "Missing IMAP username".to_string())?;
    let password = account
        .password
        .as_deref()
        .ok_or_else(|| "Missing IMAP password".to_string())?;
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let client = connect_imap_client(host, port, &tls)?;
    client
        .login(username, password)
        .map_err(|e| format!("IMAP login failed or timed out after 15s: {}", e.0))
}

fn parse_imap_thread_id(thread_id: &str) -> AppResult<(String, u32)> {
    let (folder, uid) = thread_id
        .rsplit_once(':')
        .ok_or_else(|| "Cannot identify IMAP message UID".to_string())?;
    let uid = uid
        .parse::<u32>()
        .map_err(|_| "Cannot parse IMAP message UID".to_string())?;
    Ok((folder.to_string(), uid))
}

#[tauri::command]
fn download_attachment(app: tauri::AppHandle, input: DownloadAttachmentInput) -> AppResult<()> {
    start_mail_action_job(app, "Download attachment", move |app| {
        run_download_attachment(app, input).map(|_| ())
    })
}

fn run_download_attachment(
    app: &tauri::AppHandle,
    input: DownloadAttachmentInput,
) -> AppResult<DownloadedAttachment> {
    let state = app.state::<AppState>();
    let (account, message_provider_id, attachment_provider_id, data): (
        StoredAccount,
        String,
        Option<String>,
        Option<Vec<u8>>,
    ) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, message_provider_id, attachment_provider_id, data): (
            i64,
            String,
            Option<String>,
            Option<Vec<u8>>,
        ) = conn
            .query_row(
                "
                SELECT m.account_id, m.provider_message_id, a.provider_attachment_id, a.data
                FROM attachments a
                JOIN messages m ON m.id = a.message_id
                WHERE a.id = ?
                ",
                params![input.attachment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| e.to_string())?;
        (
            get_account(&conn, account_id)?,
            message_provider_id,
            attachment_provider_id,
            data,
        )
    };

    let bytes = if let Some(data) = data {
        data
    } else if account.provider == "gmail" {
        let attachment_id =
            attachment_provider_id.ok_or_else(|| "Missing Gmail attachment id".to_string())?;
        fetch_gmail_attachment(&state, &account, &message_provider_id, &attachment_id)?
    } else {
        return Err("Attachment data was not stored locally. Sync the message again.".to_string());
    };

    let path = write_download_to_path(&input.save_path, &bytes)?;
    debug(app, format!("Downloaded attachment to {}", path.display()));
    Ok(DownloadedAttachment {
        path: path.display().to_string(),
    })
}

fn fetch_gmail_attachment(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    message_id: &str,
    attachment_id: &str,
) -> AppResult<Vec<u8>> {
    let token = gmail_access_token(state, account)?;
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages/{message_id}/attachments/{attachment_id}"
    );
    let raw: Value = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?
        .get(url)
        .bearer_auth(token)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .map_err(|e| e.to_string())?;
    let data = raw
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "Gmail attachment response missing data".to_string())?;
    general_purpose::URL_SAFE_NO_PAD
        .decode(data)
        .or_else(|_| general_purpose::URL_SAFE.decode(data))
        .map_err(|e| e.to_string())
}

fn write_download_to_path(save_path: &str, bytes: &[u8]) -> AppResult<PathBuf> {
    let path = PathBuf::from(save_path);
    if path.as_os_str().is_empty() {
        return Err("No save path selected".to_string());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

#[tauri::command]
fn mark_message_read(app: tauri::AppHandle, input: MarkReadInput) -> AppResult<()> {
    start_mail_action_job(app, "Mark message read state", move |app| {
        run_mark_message_read(app, input)
    })
}

fn run_mark_message_read(app: &tauri::AppHandle, input: MarkReadInput) -> AppResult<()> {
    let state = app.state::<AppState>();
    let (account, provider_message_id, thread_id) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, provider_message_id, thread_id): (i64, String, String) = conn
            .query_row(
                "SELECT account_id, provider_message_id, thread_id FROM messages WHERE id = ?",
                params![input.message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| e.to_string())?;
        (
            get_account(&conn, account_id)?,
            provider_message_id,
            thread_id,
        )
    };

    if !provider_message_id.starts_with("local-sent-") {
        if account.provider == "gmail" {
            mark_gmail_message_read(&state, &account, &provider_message_id, input.is_read)?;
        } else {
            mark_imap_message_read(&account, &thread_id, input.is_read)?;
        }
    }

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE messages SET is_read = ?, updated_at = ? WHERE id = ?",
        params![
            if input.is_read { 1 } else { 0 },
            now_ts(),
            input.message_id
        ],
    )
    .map_err(|e| e.to_string())?;
    debug(
        app,
        format!(
            "Marked message {} as {}",
            input.message_id,
            if input.is_read { "read" } else { "unread" }
        ),
    );
    Ok(())
}

#[tauri::command]
fn remove_account(state: State<'_, AppState>, input: RemoveAccountInput) -> AppResult<()> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM accounts WHERE id = ?",
        params![input.account_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn update_imap_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: UpdateImapSettingsInput,
) -> AppResult<()> {
    if input.email.trim().is_empty()
        || input.imap_host.trim().is_empty()
        || input.smtp_host.trim().is_empty()
        || input.username.trim().is_empty()
    {
        return Err("Email, IMAP host, SMTP host, and username are required".to_string());
    }
    if !(1..=65535).contains(&input.imap_port) || !(1..=65535).contains(&input.smtp_port) {
        return Err("Ports must be between 1 and 65535".to_string());
    }

    let existing_password = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let account = get_account(&conn, input.account_id)?;
        if account.provider != "imap" {
            return Err("Only IMAP/SMTP accounts can be edited here".to_string());
        }
        account.password.unwrap_or_default()
    };
    let password = input
        .password
        .as_deref()
        .filter(|value| !value.is_empty())
        .unwrap_or(existing_password.as_str())
        .to_string();
    if password.is_empty() {
        return Err("Password is required because no existing password is stored".to_string());
    }

    debug(
        &app,
        format!(
            "IMAP settings update test account_id={} email={} imap={}:{} smtp={}:{} username={}",
            input.account_id,
            input.email,
            input.imap_host,
            input.imap_port,
            input.smtp_host,
            input.smtp_port,
            input.username
        ),
    );
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let client = connect_imap_client(input.imap_host.as_str(), input.imap_port as u16, &tls)?;
    let mut session = client
        .login(input.username.as_str(), password.as_str())
        .map_err(|e| format!("IMAP login failed or timed out after 15s: {}", e.0))?;
    session.logout().ok();

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "
        UPDATE accounts
        SET email = ?, display_name = ?, imap_host = ?, imap_port = ?,
            smtp_host = ?, smtp_port = ?, username = ?, password = ?
        WHERE id = ? AND provider = 'imap'
        ",
        params![
            input.email.trim(),
            input
                .display_name
                .as_deref()
                .filter(|value| !value.is_empty()),
            input.imap_host.trim(),
            input.imap_port,
            input.smtp_host.trim(),
            input.smtp_port,
            input.username.trim(),
            password,
            input.account_id
        ],
    )
    .map_err(|e| e.to_string())?;
    conn.execute(
        "DELETE FROM sync_cursors WHERE account_id = ?",
        params![input.account_id],
    )
    .map_err(|e| e.to_string())?;
    debug(&app, "IMAP settings saved");
    Ok(())
}

fn mark_gmail_message_read(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    provider_message_id: &str,
    is_read: bool,
) -> AppResult<()> {
    let token = gmail_access_token(state, account)?;
    let body = if is_read {
        serde_json::json!({ "removeLabelIds": ["UNREAD"] })
    } else {
        serde_json::json!({ "addLabelIds": ["UNREAD"] })
    };
    Client::new()
        .post(format!(
            "https://gmail.googleapis.com/gmail/v1/users/me/messages/{provider_message_id}/modify"
        ))
        .bearer_auth(token)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn mark_imap_message_read(
    account: &StoredAccount,
    thread_id: &str,
    is_read: bool,
) -> AppResult<()> {
    let (folder, uid) = parse_imap_thread_id(thread_id)?;
    let mut session = open_imap_session(account)?;
    session.select(&folder).map_err(|e| e.to_string())?;
    let command = if is_read {
        "+FLAGS.SILENT (\\Seen)"
    } else {
        "-FLAGS.SILENT (\\Seen)"
    };
    session
        .uid_store(uid.to_string(), command)
        .map_err(|e| e.to_string())?;
    session.logout().ok();
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let conn = init_db(app.handle())?;
            app.manage(AppState {
                db: Mutex::new(conn),
                syncing: Mutex::new(false),
                polling: Mutex::new(false),
                idle_accounts: Mutex::new(HashSet::new()),
            });
            start_mail_polling(app.handle().clone());
            start_imap_idle_manager(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect_gmail,
            connect_imap,
            delete_message,
            download_attachment,
            list_accounts,
            list_folders,
            list_messages,
            mark_message_read,
            mailbox_snapshot,
            remove_account,
            send_message,
            sync_account,
            sync_all,
            update_imap_settings
        ])
        .run(tauri::tauri_build_context!())
        .expect("error while running tauri application");
}
