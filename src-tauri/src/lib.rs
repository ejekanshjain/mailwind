use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use keyring::v1::{Entry, Error as KeyringError};
use lettre::{
    message::{Mailbox, MultiPart},
    transport::smtp::authentication::Credentials,
    Address, Message as SmtpMessage, SmtpTransport, Transport,
};
use mailparse::DispositionType;
use mailparse::MailAddr;
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
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{Emitter, Manager, State};
use url::form_urlencoded;
use uuid::Uuid;

type AppResult<T> = Result<T, String>;
type ImapSession = imap::Session<native_tls::TlsStream<TcpStream>>;

const IMAP_FLAG_FETCH_CHUNK: usize = 100;
const IMAP_BODY_FETCH_CHUNK: usize = 25;
const KEYRING_SERVICE: &str = "com.mailwind.desktop";
const ACCOUNT_SECRET_FIELDS: [&str; 5] = [
    "access_token",
    "refresh_token",
    "client_secret",
    "password",
    "client_id",
];

struct AppState {
    db: Mutex<Connection>,
    syncing: Mutex<bool>,
    polling: Mutex<bool>,
    resetting: Mutex<bool>,
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
    message_header_id: String,
    in_reply_to: String,
    references_header: String,
    normalized_subject: String,
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
    thread_count: i64,
    thread_unread_count: i64,
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
    cc: Option<String>,
    bcc: Option<String>,
    reply_to: Option<String>,
    subject: String,
    body: String,
    body_mime: String,
    reply_to_message_id: Option<i64>,
}

#[derive(Debug, Clone)]
struct ReplyContext {
    account_id: i64,
    thread_id: String,
    message_header_id: String,
    references_header: String,
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
    folder: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ThreadInput {
    message_id: i64,
    folder: Option<String>,
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
struct ImapMovedLocation {
    folder: String,
    uid: u32,
}

#[derive(Debug, Clone)]
struct NewMessage {
    provider_message_id: String,
    thread_id: String,
    message_header_id: String,
    in_reply_to: String,
    references_header: String,
    normalized_subject: String,
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

struct OAuthCallback {
    code: String,
    state: String,
}

struct ImapRawFetch<'a> {
    role: &'a str,
    folder: &'a str,
    uid: u32,
    raw: &'a [u8],
    is_read: bool,
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

fn imap_error(error: imap::error::Error) -> String {
    format!("{error}; details={error:?}")
}

fn imap_uid_set(uids: &[u32]) -> String {
    uids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn imap_uid_summary(uids: &[u32]) -> String {
    match uids {
        [] => "-".to_string(),
        [uid] => uid.to_string(),
        many => {
            let first = many.first().copied().unwrap_or_default();
            let last = many.last().copied().unwrap_or_default();
            format!("{first}..{last} ({} uids)", many.len())
        }
    }
}

fn select_imap_folder(session: &mut ImapSession, folder: &str) -> AppResult<()> {
    session
        .select(folder)
        .map(|_| ())
        .map_err(|e| format!("IMAP select failed folder={folder}: {}", imap_error(e)))
}

fn reopen_imap_folder(account: &StoredAccount, folder: &str) -> AppResult<ImapSession> {
    let mut session = open_imap_session(account)?;
    select_imap_folder(&mut session, folder)?;
    Ok(session)
}

fn debug_imap_fetch_failure(
    app: Option<&tauri::AppHandle>,
    account: &StoredAccount,
    role: &str,
    uids: &[u32],
    query: &str,
    action: &str,
    error: &str,
) {
    if let Some(app) = app {
        debug(
            app,
            format!(
                "IMAP fetch failed email={} folder={} uids={} query={} action={} error={}",
                account.email,
                role,
                imap_uid_summary(uids),
                query,
                action,
                error
            ),
        );
    }
}

fn init_db(app: &tauri::AppHandle) -> Result<Connection, Box<dyn std::error::Error>> {
    let dir = app.path().app_data_dir()?;
    fs::create_dir_all(&dir)?;
    let conn = Connection::open(dir.join("mailwind.sqlite3"))?;
    initialize_schema(&conn).map_err(boxed_app_error)?;
    migrate_plaintext_account_secrets(&conn).map_err(boxed_app_error)?;
    recover_message_search_index(&conn).map_err(boxed_app_error)?;

    Ok(conn)
}

fn boxed_app_error(error: String) -> Box<dyn std::error::Error> {
    Box::new(std::io::Error::other(error))
}

fn initialize_schema(conn: &Connection) -> AppResult<()> {
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
            message_header_id TEXT NOT NULL DEFAULT '',
            in_reply_to TEXT NOT NULL DEFAULT '',
            references_header TEXT NOT NULL DEFAULT '',
            normalized_subject TEXT NOT NULL DEFAULT '',
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
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_thread_date
            ON messages(account_id, folder, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_folder_account_thread_date
            ON messages(folder, account_id, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_read_thread_date
            ON messages(account_id, folder, is_read, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_folder_read_account_thread_date
            ON messages(folder, is_read, account_id, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_thread
            ON messages(account_id, thread_id);
        CREATE INDEX IF NOT EXISTS idx_messages_account_thread_date
            ON messages(account_id, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_thread_folder_date
            ON messages(account_id, thread_id, folder, date_ts ASC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_subject
            ON messages(account_id, normalized_subject);
        CREATE INDEX IF NOT EXISTS idx_messages_account_message_header
            ON messages(account_id, message_header_id);
        CREATE INDEX IF NOT EXISTS idx_messages_account_in_reply_to
            ON messages(account_id, in_reply_to);
        CREATE INDEX IF NOT EXISTS idx_messages_inbox_unread
            ON messages(folder, is_read)
            WHERE folder = 'Inbox' AND is_read = 0;
        CREATE INDEX IF NOT EXISTS idx_attachments_message_id
            ON attachments(message_id);
        ",
    )
    .map_err(|e| e.to_string())?;
    dedupe_accounts_for_unique_index(conn)?;
    conn.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_accounts_provider_email_unique ON accounts(provider, email)",
        [],
    )
    .map_err(|e| e.to_string())?;
    ensure_message_metadata_columns(conn)?;
    backfill_message_metadata(conn)?;
    conn.execute_batch("PRAGMA optimize;")
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn dedupe_accounts_for_unique_index(conn: &Connection) -> AppResult<()> {
    let duplicate_ids = {
        let mut stmt = conn
            .prepare(
                "
                SELECT id
                FROM accounts
                WHERE id NOT IN (
                    SELECT MAX(id)
                    FROM accounts
                    GROUP BY provider, email
                )
                ",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for account_id in duplicate_ids {
        delete_account_secrets(account_id)?;
        conn.execute("DELETE FROM accounts WHERE id = ?", params![account_id])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn ensure_message_metadata_columns(conn: &Connection) -> AppResult<()> {
    let existing = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(messages)")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<HashSet<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    for (name, sql_type) in [
        ("message_header_id", "TEXT NOT NULL DEFAULT ''"),
        ("in_reply_to", "TEXT NOT NULL DEFAULT ''"),
        ("references_header", "TEXT NOT NULL DEFAULT ''"),
        ("normalized_subject", "TEXT NOT NULL DEFAULT ''"),
    ] {
        if !existing.contains(name) {
            conn.execute(
                &format!("ALTER TABLE messages ADD COLUMN {name} {sql_type}"),
                [],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    conn.execute_batch(
        "
        CREATE INDEX IF NOT EXISTS idx_messages_account_thread_date
            ON messages(account_id, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_thread_folder_date
            ON messages(account_id, thread_id, folder, date_ts ASC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_thread_date
            ON messages(account_id, folder, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_folder_account_thread_date
            ON messages(folder, account_id, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_folder_read_thread_date
            ON messages(account_id, folder, is_read, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_folder_read_account_thread_date
            ON messages(folder, is_read, account_id, thread_id, date_ts DESC);
        CREATE INDEX IF NOT EXISTS idx_messages_account_subject
            ON messages(account_id, normalized_subject);
        CREATE INDEX IF NOT EXISTS idx_messages_account_message_header
            ON messages(account_id, message_header_id);
        CREATE INDEX IF NOT EXISTS idx_messages_account_in_reply_to
            ON messages(account_id, in_reply_to);
        ",
    )
    .map_err(|e| e.to_string())
}

fn backfill_message_metadata(conn: &Connection) -> AppResult<()> {
    let rows = {
        let mut stmt = conn
            .prepare(
                "
                SELECT id, provider_message_id, thread_id, subject,
                       message_header_id, normalized_subject
                FROM messages
                WHERE message_header_id = ''
                   OR normalized_subject = ''
                   OR provider_message_id LIKE 'imap-uid:%'
                ",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    for (id, provider_message_id, current_thread_id, subject, header_id, normalized) in rows {
        let clean_subject = clean_header_value(&subject);
        let normalized_subject = if normalized.trim().is_empty() {
            normalize_subject_key(&clean_subject)
        } else {
            normalized
        };
        let message_header_id = if header_id.trim().is_empty() {
            message_header_id_from_provider_id(&provider_message_id).unwrap_or_default()
        } else {
            header_id
        };
        let thread_id = backfilled_conversation_id(
            &provider_message_id,
            &current_thread_id,
            &normalized_subject,
            &message_header_id,
        );
        conn.execute(
            "
            UPDATE messages
            SET thread_id = ?, message_header_id = ?, normalized_subject = ?, subject = ?
            WHERE id = ?
            ",
            params![
                thread_id,
                message_header_id,
                normalized_subject,
                clean_subject,
                id
            ],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn recover_message_search_index(conn: &Connection) -> AppResult<()> {
    let missing_rows: i64 = conn
        .query_row(
            "
            SELECT COUNT(*)
            FROM messages m
            LEFT JOIN messages_fts f ON f.rowid = m.id
            WHERE f.rowid IS NULL
            ",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let orphaned_rows: i64 = conn
        .query_row(
            "
            SELECT COUNT(*)
            FROM messages_fts f
            LEFT JOIN messages m ON m.id = f.rowid
            WHERE m.id IS NULL
            ",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if missing_rows == 0 && orphaned_rows == 0 {
        return Ok(());
    }
    conn.execute("DELETE FROM messages_fts", [])
        .map_err(|e| e.to_string())?;
    conn.execute(
        "
        INSERT INTO messages_fts(rowid, subject, from_addr, to_addr, body)
        SELECT id, subject, from_addr, to_addr, body
        FROM messages
        ",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn migrate_plaintext_account_secrets(conn: &Connection) -> AppResult<()> {
    let rows = {
        let mut stmt = conn
            .prepare(
                "
                SELECT id, access_token, refresh_token, client_secret, password, client_id
                FROM accounts
                WHERE COALESCE(access_token, '') <> ''
                   OR COALESCE(refresh_token, '') <> ''
                   OR COALESCE(client_secret, '') <> ''
                   OR COALESCE(password, '') <> ''
                   OR COALESCE(client_id, '') <> ''
                ",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };

    for (account_id, access_token, refresh_token, client_secret, password, client_id) in rows {
        for (field, value) in [
            ("access_token", access_token.as_deref()),
            ("refresh_token", refresh_token.as_deref()),
            ("client_secret", client_secret.as_deref()),
            ("password", password.as_deref()),
            ("client_id", client_id.as_deref()),
        ] {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                store_account_secret(account_id, field, value)?;
            }
        }
        conn.execute(
            "
            UPDATE accounts
            SET access_token = NULL,
                refresh_token = NULL,
                client_secret = NULL,
                password = NULL,
                client_id = NULL
            WHERE id = ?
            ",
            params![account_id],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn account_secret_entry(account_id: i64, field: &str) -> AppResult<Entry> {
    Entry::new(KEYRING_SERVICE, &format!("account:{account_id}:{field}"))
        .map_err(|e| format!("Could not open secure credential store for {field}: {e}"))
}

fn store_account_secret(account_id: i64, field: &str, value: &str) -> AppResult<()> {
    account_secret_entry(account_id, field)?
        .set_password(value)
        .map_err(|e| format!("Could not store {field} in the OS credential store: {e}"))
}

fn load_account_secret(account_id: i64, field: &str) -> AppResult<Option<String>> {
    match account_secret_entry(account_id, field)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(format!(
            "Could not read {field} from the OS credential store: {error}"
        )),
    }
}

fn delete_account_secret(account_id: i64, field: &str) -> AppResult<()> {
    match account_secret_entry(account_id, field)?.delete_credential() {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(format!(
            "Could not delete {field} from the OS credential store: {error}"
        )),
    }
}

fn delete_account_secrets(account_id: i64) -> AppResult<()> {
    for field in ACCOUNT_SECRET_FIELDS {
        delete_account_secret(account_id, field)?;
    }
    Ok(())
}

fn delete_all_account_secrets(conn: &Connection) -> AppResult<()> {
    let ids = {
        let mut stmt = conn
            .prepare("SELECT id FROM accounts")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for id in ids {
        delete_account_secrets(id)?;
    }
    Ok(())
}

fn resolve_account_secrets(mut account: StoredAccount) -> AppResult<StoredAccount> {
    account.access_token = load_account_secret(account.id, "access_token")?;
    account.refresh_token = load_account_secret(account.id, "refresh_token")?;
    account.client_id = load_account_secret(account.id, "client_id")?;
    account.client_secret = load_account_secret(account.id, "client_secret")?;
    account.password = load_account_secret(account.id, "password")?;
    Ok(account)
}

fn resolve_accounts(accounts: Vec<StoredAccount>) -> AppResult<Vec<StoredAccount>> {
    accounts.into_iter().map(resolve_account_secrets).collect()
}

fn backfilled_conversation_id(
    provider_message_id: &str,
    current_thread_id: &str,
    normalized_subject: &str,
    message_header_id: &str,
) -> String {
    if provider_message_id.starts_with("imap-uid:") {
        if !normalized_subject.is_empty() {
            return format!("subject:{normalized_subject}");
        }
        if !message_header_id.is_empty() {
            return format!("message:{}", message_header_id.to_ascii_lowercase());
        }
    }
    if !current_thread_id.trim().is_empty() {
        return current_thread_id.to_string();
    }
    if !normalized_subject.is_empty() {
        return format!("subject:{normalized_subject}");
    }
    provider_message_id.to_string()
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
    let account = conn
        .query_row(
        "SELECT id, provider, email, display_name, access_token, refresh_token, token_expires_at,
                client_id, client_secret, imap_host, imap_port, smtp_host, smtp_port, username, password
         FROM accounts WHERE id = ?",
        params![id],
        row_to_account,
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Account not found".to_string())?;
    resolve_account_secrets(account)
}

fn upsert_message(conn: &Connection, account_id: i64, msg: &NewMessage) -> AppResult<()> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    delete_matching_local_sent_copy(&tx, account_id, msg)?;
    let ts = now_ts();
    tx.execute(
        "
        INSERT INTO messages (
            account_id, provider_message_id, thread_id, message_header_id, in_reply_to,
            references_header, normalized_subject, folder, subject, from_addr, to_addr,
            cc_addr, date_ts, snippet, body, body_mime, is_read, created_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(account_id, provider_message_id) DO UPDATE SET
            thread_id = excluded.thread_id,
            message_header_id = excluded.message_header_id,
            in_reply_to = excluded.in_reply_to,
            references_header = excluded.references_header,
            normalized_subject = excluded.normalized_subject,
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
            msg.message_header_id,
            msg.in_reply_to,
            msg.references_header,
            msg.normalized_subject,
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

    let id: i64 = tx
        .query_row(
            "SELECT id FROM messages WHERE account_id = ? AND provider_message_id = ?",
            params![account_id, msg.provider_message_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;

    tx.execute("DELETE FROM messages_fts WHERE rowid = ?", params![id])
        .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO messages_fts(rowid, subject, from_addr, to_addr, body) VALUES (?, ?, ?, ?, ?)",
        params![id, msg.subject, msg.from_addr, msg.to_addr, msg.body],
    )
    .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM attachments WHERE message_id = ?", params![id])
        .map_err(|e| e.to_string())?;
    for attachment in &msg.attachments {
        tx.execute(
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
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_matching_local_sent_copy(
    conn: &Connection,
    account_id: i64,
    msg: &NewMessage,
) -> AppResult<()> {
    if msg.folder != "Sent" || is_local_sent_id(&msg.provider_message_id) {
        return Ok(());
    }
    let Some(message_id) = reply_header_id(&msg.provider_message_id) else {
        return Ok(());
    };
    let local_id = format!("local-sent:{message_id}");
    let mut stmt = conn
        .prepare(
            "
            SELECT id
            FROM messages
            WHERE account_id = ? AND folder = 'Sent' AND provider_message_id = ?
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, local_id], |row| row.get::<_, i64>(0))
        .map_err(|e| e.to_string())?;
    let ids = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(stmt);

    for id in ids {
        delete_message_row_inner(conn, id)?;
    }
    Ok(())
}

fn delete_message_row(conn: &Connection, id: i64) -> AppResult<()> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    delete_message_row_inner(&tx, id)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

fn delete_message_row_inner(conn: &Connection, id: i64) -> AppResult<()> {
    conn.execute("DELETE FROM messages_fts WHERE rowid = ?", params![id])
        .map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM messages WHERE id = ?", params![id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn is_local_sent_id(provider_message_id: &str) -> bool {
    provider_message_id.starts_with("local-sent:")
}

fn delete_messages_by_imap_location(
    conn: &Connection,
    account_id: i64,
    folder: &str,
    uid: u32,
) -> AppResult<usize> {
    let exact = imap_provider_message_id(folder, uid, None);
    let like = format!("{exact}:%");
    let mut stmt = conn
        .prepare(
            "
            SELECT id
            FROM messages
            WHERE account_id = ?
              AND (provider_message_id = ? OR provider_message_id LIKE ?)
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, exact, like], |row| row.get::<_, i64>(0))
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

fn update_message_read_by_imap_location(
    conn: &Connection,
    account_id: i64,
    folder: &str,
    uid: u32,
    is_read: bool,
) -> AppResult<bool> {
    let exact = imap_provider_message_id(folder, uid, None);
    let like = format!("{exact}:%");
    let current = conn
        .query_row(
            "
            SELECT is_read
            FROM messages
            WHERE account_id = ?
              AND (provider_message_id = ? OR provider_message_id LIKE ?)
            ",
            params![account_id, exact, like],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let next = if is_read { 1 } else { 0 };
    if current != Some(next) {
        conn.execute(
            "
            UPDATE messages
            SET is_read = ?, updated_at = ?
            WHERE account_id = ?
              AND (provider_message_id = ? OR provider_message_id LIKE ?)
            ",
            params![next, now_ts(), account_id, exact, like],
        )
        .map_err(|e| e.to_string())?;
        return Ok(current.is_some());
    }
    Ok(false)
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
            WHERE account_id = ? AND folder = ?
              AND provider_message_id NOT LIKE 'local-sent:%'
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, folder], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<HashSet<_>, _>>()
        .map_err(|e| e.to_string())
}

fn existing_imap_location_ids(
    conn: &Connection,
    account_id: i64,
    folder: &str,
) -> AppResult<HashSet<String>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT provider_message_id
            FROM messages
            WHERE account_id = ? AND folder = ?
              AND provider_message_id LIKE 'imap-uid:%'
            ",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, folder], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let provider_ids = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(provider_ids
        .iter()
        .filter_map(|provider_id| imap_location_from_provider_id(provider_id))
        .collect())
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
            WHERE account_id = ? AND folder = ?
              AND provider_message_id NOT LIKE 'local-sent:%'
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

fn delete_missing_imap_location_ids(
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
            WHERE account_id = ? AND folder = ?
              AND provider_message_id LIKE 'imap-uid:%'
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
        if let Some(location) = imap_location_from_provider_id(&provider_id) {
            if !upstream_ids.contains(&location) {
                delete_message_row(conn, id)?;
                removed += 1;
            }
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

    let filtered_sql = if query.trim().is_empty() {
        format!("SELECT m.* FROM messages m WHERE {where_sql}")
    } else {
        format!(
            "
            SELECT m.*
            FROM messages_fts
            JOIN messages m ON m.id = messages_fts.rowid
            WHERE {where_sql}
            "
        )
    };
    let sql = format!(
        "
        WITH filtered AS ({filtered_sql}),
             grouped AS (
                SELECT account_id, thread_id,
                       COUNT(*) AS thread_count,
                       COALESCE(SUM(CASE WHEN is_read = 0 THEN 1 ELSE 0 END), 0) AS thread_unread_count
                FROM filtered
                GROUP BY account_id, thread_id
             ),
             picked AS (
                SELECT (
                    SELECT id
                    FROM filtered f
                    WHERE f.account_id = grouped.account_id
                      AND f.thread_id = grouped.thread_id
                    ORDER BY date_ts DESC, id DESC
                    LIMIT 1
                ) AS id,
                thread_count,
                thread_unread_count
                FROM grouped
             )
        SELECT m.id, m.account_id, m.provider_message_id, m.thread_id,
               m.message_header_id, m.in_reply_to, m.references_header, m.normalized_subject,
               m.folder, m.subject, m.from_addr, m.to_addr, m.cc_addr, m.date_ts, m.snippet,
               m.body, m.body_mime, m.is_read, a.email, a.provider,
               picked.thread_count, picked.thread_unread_count
        FROM picked
        JOIN messages m ON m.id = picked.id
        JOIN accounts a ON a.id = m.account_id
        ORDER BY m.date_ts DESC, m.id DESC
        LIMIT ? OFFSET ?
        "
    );

    sql_params.push(SqlValue::Integer(page_size));
    sql_params.push(SqlValue::Integer(offset));

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(sql_params.iter()), |row| {
            stored_message_from_row(row, Some((20, 21)))
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

#[tauri::command]
fn list_thread_messages(
    state: State<'_, AppState>,
    input: ThreadInput,
) -> AppResult<Vec<StoredMessage>> {
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let (account_id, thread_id): (i64, String) = conn
        .query_row(
            "SELECT account_id, thread_id FROM messages WHERE id = ?",
            params![input.message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Message not found".to_string())?;
    let folder_scope = match input.folder.as_deref() {
        Some("Trash") => "AND m.folder = 'Trash'",
        Some(_) => "AND m.folder <> 'Trash'",
        std::option::Option::None => "",
    };
    let sql = format!(
        "
        WITH thread_messages AS (
            SELECT m.id, m.account_id, m.provider_message_id, m.thread_id,
                   m.message_header_id, m.in_reply_to, m.references_header, m.normalized_subject,
                   m.folder, m.subject, m.from_addr, m.to_addr, m.cc_addr, m.date_ts, m.snippet,
                   m.body, m.body_mime, m.is_read, a.email, a.provider
            FROM messages m
            JOIN accounts a ON a.id = m.account_id
            WHERE m.account_id = ? AND m.thread_id = ?
            {folder_scope}
        ),
        thread_stats AS (
            SELECT COUNT(*) AS thread_count,
                   COALESCE(SUM(CASE WHEN is_read = 0 THEN 1 ELSE 0 END), 0) AS thread_unread_count
            FROM thread_messages
        )
        SELECT tm.id, tm.account_id, tm.provider_message_id, tm.thread_id,
               tm.message_header_id, tm.in_reply_to, tm.references_header, tm.normalized_subject,
               tm.folder, tm.subject, tm.from_addr, tm.to_addr, tm.cc_addr, tm.date_ts, tm.snippet,
               tm.body, tm.body_mime, tm.is_read, tm.email, tm.provider,
               thread_stats.thread_count, thread_stats.thread_unread_count
        FROM thread_messages tm
        CROSS JOIN thread_stats
        ORDER BY tm.date_ts ASC, tm.id ASC
        "
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![account_id, thread_id], |row| {
            stored_message_from_row(row, Some((20, 21)))
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

fn stored_message_from_row(
    row: &rusqlite::Row<'_>,
    count_columns: Option<(usize, usize)>,
) -> rusqlite::Result<StoredMessage> {
    let (thread_count, thread_unread_count) = match count_columns {
        Some((count_column, unread_column)) => (row.get(count_column)?, row.get(unread_column)?),
        std::option::Option::None => (1, if row.get::<_, i64>(17)? == 0 { 1 } else { 0 }),
    };
    Ok(StoredMessage {
        id: row.get(0)?,
        account_id: row.get(1)?,
        provider_message_id: row.get(2)?,
        thread_id: row.get(3)?,
        message_header_id: row.get(4)?,
        in_reply_to: row.get(5)?,
        references_header: row.get(6)?,
        normalized_subject: row.get(7)?,
        folder: row.get(8)?,
        subject: row.get(9)?,
        from_addr: row.get(10)?,
        to_addr: row.get(11)?,
        cc_addr: row.get(12)?,
        date_ts: row.get(13)?,
        snippet: row.get(14)?,
        body: row.get(15)?,
        body_mime: row.get(16)?,
        is_read: row.get::<_, i64>(17)? == 1,
        account_email: row.get(18)?,
        account_provider: row.get(19)?,
        thread_count,
        thread_unread_count,
        attachments: Vec::new(),
    })
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
    let filtered_sql = if query.trim().is_empty() {
        format!("SELECT m.account_id, m.thread_id FROM messages m WHERE {where_sql}")
    } else {
        format!(
            "
            SELECT m.account_id, m.thread_id
            FROM messages_fts
            JOIN messages m ON m.id = messages_fts.rowid
            WHERE {where_sql}
            "
        )
    };
    let sql = format!(
        "
        WITH filtered AS ({filtered_sql})
        SELECT COUNT(*)
        FROM (
            SELECT account_id, thread_id
            FROM filtered
            GROUP BY account_id, thread_id
        )
        "
    );
    conn.query_row(&sql, rusqlite::params_from_iter(sql_params.iter()), |row| {
        row.get(0)
    })
    .map_err(|e| e.to_string())
}

fn fts_query_value(query: &str) -> Option<String> {
    let terms = parse_search_terms(query);
    if terms.is_empty() {
        return None;
    }
    Some(
        terms
            .into_iter()
            .map(|term| match term {
                SearchTerm::Word(value) => format!("{}*", fts_escape_word(&value)),
                SearchTerm::Phrase(value) => format!("\"{}\"", value.replace('"', "\"\"")),
            })
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

#[derive(Debug, PartialEq, Eq)]
enum SearchTerm {
    Word(String),
    Phrase(String),
}

fn parse_search_terms(query: &str) -> Vec<SearchTerm> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut phrase = String::new();
    let mut in_phrase = false;

    for ch in query.chars() {
        if ch == '"' {
            if in_phrase {
                let value = collapse_spaces(&phrase);
                if !value.is_empty() {
                    terms.push(SearchTerm::Phrase(value));
                }
                phrase.clear();
                in_phrase = false;
            } else {
                push_search_word(&mut terms, &mut current);
                in_phrase = true;
            }
            continue;
        }
        if in_phrase {
            phrase.push(ch);
        } else if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            push_search_word(&mut terms, &mut current);
        }
    }
    if in_phrase {
        current.push_str(&phrase);
    }
    push_search_word(&mut terms, &mut current);
    terms
}

fn push_search_word(terms: &mut Vec<SearchTerm>, current: &mut String) {
    let value = current.trim().to_string();
    current.clear();
    if !value.is_empty() {
        terms.push(SearchTerm::Word(value));
    }
}

fn fts_escape_word(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_alphanumeric() || *ch == '_')
        .collect::<String>()
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
    let placeholders = std::iter::repeat_n("?", message_ids.len())
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

fn required_input(value: &str, label: &str) -> AppResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(format!("{label} is required"))
    } else {
        Ok(trimmed.to_string())
    }
}

fn normalize_email_input(value: &str, label: &str) -> AppResult<String> {
    Ok(required_input(value, label)?.to_ascii_lowercase())
}

fn existing_account_id(conn: &Connection, provider: &str, email: &str) -> AppResult<Option<i64>> {
    conn.query_row(
        "SELECT id FROM accounts WHERE provider = ? AND email = ?",
        rusqlite::params_from_iter([provider, email]),
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn validate_port(value: i64, label: &str) -> AppResult<u16> {
    if (1..=65535).contains(&value) {
        Ok(value as u16)
    } else {
        Err(format!("{label} must be between 1 and 65535"))
    }
}

fn validate_smtp_login(host: &str, port: u16, username: &str, password: &str) -> AppResult<()> {
    let mailer = SmtpTransport::relay(host)
        .map_err(|e| format!("SMTP setup failed for {host}:{port}: {e}"))?
        .port(port)
        .timeout(Some(Duration::from_secs(30)))
        .credentials(Credentials::new(username.to_string(), password.to_string()))
        .build();
    match mailer.test_connection() {
        Ok(true) => Ok(()),
        Ok(false) => Err("SMTP server did not accept the connection".to_string()),
        Err(error) => Err(format!("SMTP login failed for {host}:{port}: {error}")),
    }
}

#[tauri::command]
fn connect_imap(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: ImapConnectInput,
) -> AppResult<AccountSummary> {
    let email = normalize_email_input(&input.email, "Email")?;
    let imap_host = required_input(&input.imap_host, "IMAP host")?;
    let smtp_host = required_input(&input.smtp_host, "SMTP host")?;
    let username = required_input(&input.username, "Username")?;
    if input.password.is_empty() {
        return Err("Password is required".to_string());
    }
    let imap_port = validate_port(input.imap_port, "IMAP port")?;
    let smtp_port = validate_port(input.smtp_port, "SMTP port")?;
    let display_name = input
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    debug(
        &app,
        format!(
            "IMAP connect start email={} imap={}:{} smtp={}:{} username={}",
            email, imap_host, imap_port, smtp_host, smtp_port, username
        ),
    );
    debug(&app, "IMAP building TLS connector");
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    debug(&app, "IMAP opening TCP/TLS connection with 15s timeout");
    let client = connect_imap_client(imap_host.as_str(), imap_port, &tls)?;
    debug(
        &app,
        "IMAP TCP/TLS ready, attempting login with 15s socket timeout",
    );
    let mut session = client.login(username.as_str(), input.password.as_str()).map_err(|e| {
        format!(
            "IMAP login failed or timed out after 15s. Check that IMAP is enabled and the password is valid for IMAP/app-password login. Server error: {}",
            e.0
        )
    })?;
    debug(&app, "IMAP login succeeded");
    session.logout().ok();
    debug(&app, "Testing SMTP login");
    validate_smtp_login(&smtp_host, smtp_port, &username, &input.password)?;

    debug(&app, "Saving IMAP account to SQLite");
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let id = if let Some(id) = existing_account_id(&conn, "imap", &email)? {
        store_account_secret(id, "password", &input.password)?;
        conn.execute(
            "
            UPDATE accounts
            SET display_name = ?, imap_host = ?, imap_port = ?,
                smtp_host = ?, smtp_port = ?, username = ?, password = NULL
            WHERE id = ? AND provider = 'imap'
            ",
            params![
                display_name,
                imap_host,
                input.imap_port,
                smtp_host,
                input.smtp_port,
                username,
                id
            ],
        )
        .map_err(|e| e.to_string())?;
        debug(&app, format!("IMAP account reconnected id={id}"));
        id
    } else {
        conn.execute(
            "
            INSERT INTO accounts (
                provider, email, display_name, imap_host, imap_port, smtp_host, smtp_port,
                username, password, created_at
            )
            VALUES ('imap', ?, ?, ?, ?, ?, ?, ?, NULL, ?)
            ",
            params![
                email,
                display_name,
                imap_host,
                input.imap_port,
                smtp_host,
                input.smtp_port,
                username,
                now_ts()
            ],
        )
        .map_err(|e| e.to_string())?;
        let id = conn.last_insert_rowid();
        if let Err(error) = store_account_secret(id, "password", &input.password) {
            let _ = conn.execute("DELETE FROM accounts WHERE id = ?", [id]);
            return Err(error);
        }
        debug(&app, format!("IMAP account saved id={id}"));
        id
    };
    Ok(AccountSummary {
        id,
        provider: "imap".to_string(),
        email,
        display_name,
        imap_host: Some(imap_host),
        imap_port: Some(input.imap_port),
        smtp_host: Some(smtp_host),
        smtp_port: Some(input.smtp_port),
        username: Some(username),
    })
}

#[tauri::command]
fn connect_gmail(
    state: State<'_, AppState>,
    input: GmailConnectInput,
) -> AppResult<AccountSummary> {
    let client_id = required_input(&input.client_id, "Google OAuth client ID")?;
    let client_secret = required_input(&input.client_secret, "Google OAuth client secret")?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let oauth_state = Uuid::new_v4().to_string();
    let scope =
        "https://www.googleapis.com/auth/gmail.modify https://www.googleapis.com/auth/gmail.send";
    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        urlencoding::encode(&client_id),
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
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
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
        .trim()
        .to_ascii_lowercase();

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let (id, created_account) = if let Some(id) = existing_account_id(&conn, "gmail", &email)? {
        (
            {
                conn.execute(
                    "
                    UPDATE accounts
                    SET token_expires_at = ?, access_token = NULL, refresh_token = NULL,
                        client_id = NULL, client_secret = NULL
                    WHERE id = ? AND provider = 'gmail'
                    ",
                    params![now_ts() + expires_in - 60, id],
                )
                .map_err(|e| e.to_string())?;
                id
            },
            false,
        )
    } else {
        conn.execute(
            "
            INSERT INTO accounts (
                provider, email, access_token, refresh_token, token_expires_at,
                client_id, client_secret, created_at
            )
            VALUES ('gmail', ?, NULL, NULL, ?, NULL, NULL, ?)
            ",
            params![email, now_ts() + expires_in - 60, now_ts()],
        )
        .map_err(|e| e.to_string())?;
        (conn.last_insert_rowid(), true)
    };
    for (field, value) in [
        ("access_token", access_token.as_str()),
        ("refresh_token", refresh_token.as_str()),
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
    ] {
        if let Err(error) = store_account_secret(id, field, value) {
            let _ = delete_account_secrets(id);
            if created_account {
                let _ = conn.execute("DELETE FROM accounts WHERE id = ?", [id]);
            }
            return Err(error);
        }
    }

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
                let callback = parse_oauth_callback_path(path)?;
                if callback.state != expected_state {
                    write_oauth_response(
                        &mut stream,
                        "400 Bad Request",
                        "Mailwind could not verify this OAuth request. Return to the app and try again.",
                    );
                    return Err("OAuth state mismatch".to_string());
                }
                write_oauth_response(
                    &mut stream,
                    "200 OK",
                    "Mailwind connected. You can close this tab.",
                );
                return Ok(callback.code);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("Timed out waiting for Google OAuth callback".to_string())
}

fn parse_oauth_callback_path(path: &str) -> AppResult<OAuthCallback> {
    Ok(OAuthCallback {
        code: query_param(path, "code").ok_or_else(|| "OAuth callback missing code".to_string())?,
        state: query_param(path, "state")
            .ok_or_else(|| "OAuth callback missing state".to_string())?,
    })
}

fn query_param(path: &str, name: &str) -> Option<String> {
    let (_, query) = path.split_once('?')?;
    let query = query.split('#').next().unwrap_or(query);
    form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn write_oauth_response(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!("<html><body><h2>{message}</h2></body></html>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).ok();
    stream.write_all(body.as_bytes()).ok();
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
        let resetting = state.resetting.lock().map_err(|e| e.to_string())?;
        if *resetting {
            return Err("Cannot sync while reset is running".to_string());
        }
        drop(resetting);
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
            resolve_accounts(
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?,
            )?
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

fn ensure_not_resetting(app: &tauri::AppHandle) -> AppResult<()> {
    let state = app.state::<AppState>();
    let resetting = state.resetting.lock().map_err(|e| e.to_string())?;
    if *resetting {
        Err("Database reset is running".to_string())
    } else {
        Ok(())
    }
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
        resolve_accounts(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
        )?
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
    let current = resolve_account_secrets(current)?;
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
        let resetting = state.resetting.lock().map_err(|e| e.to_string())?;
        if *resetting {
            return Ok(());
        }
    }
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
        resolve_accounts(
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| e.to_string())?,
        )?
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
    select_imap_folder(&mut session, folder)?;
    let uids = session
        .uid_search("UNDELETED")
        .map_err(|e| format!("IMAP poll search failed: {}", imap_error(e)))?;
    let mut selected = uids.into_iter().collect::<Vec<_>>();
    selected.sort_unstable();
    selected.reverse();
    selected.truncate(30);
    if selected.is_empty() {
        let removed = {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            delete_missing_imap_location_ids(&conn, account.id, "Inbox", &HashSet::new())?
        };
        session.logout().ok();
        return Ok((0, removed));
    }

    let existing_locations = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        existing_imap_location_ids(&conn, account.id, "Inbox")?
    };
    let mut missing_uids = Vec::new();
    let mut existing_uids = Vec::new();
    for uid in &selected {
        let location_id = imap_location_key(folder, *uid);
        if existing_locations.contains(&location_id) {
            existing_uids.push(*uid);
        } else {
            missing_uids.push(*uid);
        }
    }

    let mut changed = 0;
    if !existing_uids.is_empty() {
        let set = imap_uid_set(&existing_uids);
        match session.uid_fetch(&set, "(FLAGS)") {
            Ok(fetches) => {
                for fetch in fetches.iter() {
                    let (_, _, item_changed) =
                        apply_imap_flag_fetch(state, account, folder, fetch, None)?;
                    changed += item_changed;
                }
            }
            Err(error) => {
                debug_imap_fetch_failure(
                    None,
                    account,
                    "Inbox",
                    &existing_uids,
                    "(FLAGS)",
                    "retry-single",
                    &imap_error(error),
                );
                session.logout().ok();
                session = reopen_imap_folder(account, folder)?;
                for uid in &existing_uids {
                    match session.uid_fetch(uid.to_string(), "(FLAGS)") {
                        Ok(fetches) => {
                            for fetch in fetches.iter() {
                                let (_, _, item_changed) =
                                    apply_imap_flag_fetch(state, account, folder, fetch, None)?;
                                changed += item_changed;
                            }
                        }
                        Err(_) => {
                            session.logout().ok();
                            session = reopen_imap_folder(account, folder)?;
                        }
                    }
                }
            }
        }
    }

    let mut new_messages = 0;
    if !missing_uids.is_empty() {
        let set = imap_uid_set(&missing_uids);
        match session.uid_fetch(&set, "(BODY.PEEK[] FLAGS)") {
            Ok(fetches) => {
                for fetch in fetches.iter() {
                    if store_imap_body_fetch(None, state, account, "Inbox", folder, fetch)? {
                        new_messages += 1;
                    }
                }
            }
            Err(error) => {
                debug_imap_fetch_failure(
                    None,
                    account,
                    "Inbox",
                    &missing_uids,
                    "(BODY.PEEK[] FLAGS)",
                    "retry-single",
                    &imap_error(error),
                );
                session.logout().ok();
                session = reopen_imap_folder(account, folder)?;
                for uid in &missing_uids {
                    match session.uid_fetch(uid.to_string(), "(BODY.PEEK[] FLAGS)") {
                        Ok(fetches) => {
                            for fetch in fetches.iter() {
                                if store_imap_body_fetch(
                                    None, state, account, "Inbox", folder, fetch,
                                )? {
                                    new_messages += 1;
                                }
                            }
                        }
                        Err(_) => {
                            session.logout().ok();
                            session = reopen_imap_folder(account, folder)?;
                            if let Ok(fetches) = session.uid_fetch(
                                uid.to_string(),
                                "(BODY.PEEK[HEADER] BODY.PEEK[TEXT] FLAGS)",
                            ) {
                                for fetch in fetches.iter() {
                                    if store_imap_header_text_fetch(
                                        None, state, account, "Inbox", folder, fetch,
                                    )? {
                                        new_messages += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
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

    store_account_secret(account.id, "access_token", &access_token)?;
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE accounts SET access_token = NULL, token_expires_at = ? WHERE id = ?",
        params![now_ts() + expires_in - 60, account.id],
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
    let labels = [
        ("Inbox", Some("INBOX"), None),
        ("Sent", Some("SENT"), None),
        ("Trash", Some("TRASH"), None),
        ("Archive", None, Some("-in:inbox -in:sent -in:trash")),
    ];
    let mut count = 0;
    let page_size = 100;

    for (folder, label, query) in labels {
        let mut page_token: Option<String> = None;
        let mut folder_count = 0;
        let mut refreshed_existing = 0;
        let mut new_count = 0;
        let mut upstream_ids = HashSet::new();
        let existing_ids = {
            let conn = state.db.lock().map_err(|e| e.to_string())?;
            existing_provider_ids(&conn, account.id, folder)?
        };

        loop {
            let mut list_url = format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages?maxResults={page_size}"
            );
            if let Some(label) = label {
                list_url.push_str("&labelIds=");
                list_url.push_str(&urlencoding::encode(label));
            }
            if let Some(query) = query {
                list_url.push_str("&q=");
                list_url.push_str(&urlencoding::encode(query));
            }
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
                if existing_ids.contains(id) {
                    refreshed_existing += 1;
                } else {
                    new_count += 1;
                }
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
                    "Gmail sync folder complete email={} folder={} written={folder_count} new={new_count} refreshed={refreshed_existing} removed={removed}",
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
    let message_header_id = gmail_header(payload, "Message-ID")
        .as_deref()
        .and_then(normalize_message_id)
        .unwrap_or_default();
    let in_reply_to = normalize_message_id_header(gmail_header(payload, "In-Reply-To").as_deref());
    let references_header =
        normalize_message_id_header(gmail_header(payload, "References").as_deref());
    let normalized_subject = normalize_subject_key(&subject);
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
            .map(ToString::to_string)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                conversation_id_from_headers(
                    &message_header_id,
                    &in_reply_to,
                    &references_header,
                    &normalized_subject,
                    raw.get("id").and_then(Value::as_str).unwrap_or_default(),
                )
            }),
        message_header_id,
        in_reply_to,
        references_header,
        normalized_subject,
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
        .map(clean_header_value)
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
    let mut session = open_imap_session(account)?;
    let folders = [
        ("Inbox", vec!["INBOX"]),
        ("Sent", vec!["Sent", "Sent Items", "[Gmail]/Sent Mail"]),
        ("Trash", vec!["Trash", "Deleted Items", "[Gmail]/Trash"]),
        ("Archive", vec!["Archive", "Archives"]),
    ];
    let mut count = 0;

    for (role, candidates) in folders {
        for folder in candidates {
            if session.select(folder).is_err() {
                continue;
            }
            let uids = session
                .uid_search("UNDELETED")
                .map_err(|e| format!("IMAP search failed folder={role}: {}", imap_error(e)))?;
            let mut selected = uids.into_iter().collect::<Vec<_>>();
            selected.sort_unstable();
            selected.reverse();
            if selected.is_empty() {
                let removed = {
                    let conn = state.db.lock().map_err(|e| e.to_string())?;
                    delete_missing_imap_location_ids(&conn, account.id, role, &HashSet::new())?
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
            let existing_locations = {
                let conn = state.db.lock().map_err(|e| e.to_string())?;
                existing_imap_location_ids(&conn, account.id, role)?
            };
            let mut upstream_locations = HashSet::new();
            let mut missing_uids = Vec::new();
            let mut existing_uids = Vec::new();

            for uid in &selected {
                let location_id = imap_location_key(folder, *uid);
                upstream_locations.insert(location_id.clone());
                if existing_locations.contains(&location_id) {
                    existing_uids.push(*uid);
                } else {
                    missing_uids.push(*uid);
                }
            }

            let mut flag_checked = 0;
            let mut deleted_existing = 0;

            for chunk in existing_uids.chunks(IMAP_FLAG_FETCH_CHUNK) {
                let set = imap_uid_set(chunk);
                let fetches = match session.uid_fetch(&set, "(FLAGS)") {
                    Ok(fetches) => fetches,
                    Err(error) => {
                        let detail = imap_error(error);
                        debug_imap_fetch_failure(
                            app,
                            account,
                            role,
                            chunk,
                            "(FLAGS)",
                            "retry-single",
                            &detail,
                        );
                        session.logout().ok();
                        session = reopen_imap_folder(account, folder)?;
                        for uid in chunk {
                            match session.uid_fetch(uid.to_string(), "(FLAGS)") {
                                Ok(fetches) => {
                                    for fetch in fetches.iter() {
                                        let (checked, deleted, _) = apply_imap_flag_fetch(
                                            state,
                                            account,
                                            folder,
                                            fetch,
                                            Some(&mut upstream_locations),
                                        )?;
                                        flag_checked += checked;
                                        deleted_existing += deleted;
                                    }
                                }
                                Err(error) => {
                                    let detail = imap_error(error);
                                    debug_imap_fetch_failure(
                                        app,
                                        account,
                                        role,
                                        &[*uid],
                                        "(FLAGS)",
                                        "skip-uid",
                                        &detail,
                                    );
                                    session.logout().ok();
                                    session = reopen_imap_folder(account, folder)?;
                                }
                            }
                        }
                        continue;
                    }
                };
                for fetch in fetches.iter() {
                    let (checked, deleted, _) = apply_imap_flag_fetch(
                        state,
                        account,
                        folder,
                        fetch,
                        Some(&mut upstream_locations),
                    )?;
                    flag_checked += checked;
                    deleted_existing += deleted;
                }
            }
            let removed = {
                let conn = state.db.lock().map_err(|e| e.to_string())?;
                deleted_existing
                    + delete_missing_imap_location_ids(
                        &conn,
                        account.id,
                        role,
                        &upstream_locations,
                    )?
            };
            if let Some(app) = app {
                debug(
                    app,
                    format!(
                        "IMAP sync compare email={} folder={} upstream={} flags={} new={} removed={removed}",
                        account.email,
                        role,
                        selected.len(),
                        flag_checked,
                        missing_uids.len()
                    ),
                );
            }
            let mut folder_count = 0;
            let mut processed = 0;
            for chunk in missing_uids.chunks(IMAP_BODY_FETCH_CHUNK) {
                let set = imap_uid_set(chunk);
                let fetches = match session.uid_fetch(&set, "(BODY.PEEK[] FLAGS)") {
                    Ok(fetches) => fetches,
                    Err(error) => {
                        let detail = imap_error(error);
                        debug_imap_fetch_failure(
                            app,
                            account,
                            role,
                            chunk,
                            "(BODY.PEEK[] FLAGS)",
                            "retry-single",
                            &detail,
                        );
                        session.logout().ok();
                        session = reopen_imap_folder(account, folder)?;
                        for uid in chunk {
                            match session.uid_fetch(uid.to_string(), "(BODY.PEEK[] FLAGS)") {
                                Ok(fetches) => {
                                    for fetch in fetches.iter() {
                                        if store_imap_body_fetch(
                                            app, state, account, role, folder, fetch,
                                        )? {
                                            count += 1;
                                            folder_count += 1;
                                        }
                                    }
                                }
                                Err(error) => {
                                    let detail = imap_error(error);
                                    debug_imap_fetch_failure(
                                        app,
                                        account,
                                        role,
                                        &[*uid],
                                        "(BODY.PEEK[] FLAGS)",
                                        "retry-header-text",
                                        &detail,
                                    );
                                    session.logout().ok();
                                    session = reopen_imap_folder(account, folder)?;
                                    match session.uid_fetch(
                                        uid.to_string(),
                                        "(BODY.PEEK[HEADER] BODY.PEEK[TEXT] FLAGS)",
                                    ) {
                                        Ok(fetches) => {
                                            for fetch in fetches.iter() {
                                                if store_imap_header_text_fetch(
                                                    app, state, account, role, folder, fetch,
                                                )? {
                                                    count += 1;
                                                    folder_count += 1;
                                                }
                                            }
                                        }
                                        Err(error) => {
                                            let detail = imap_error(error);
                                            debug_imap_fetch_failure(
                                                app,
                                                account,
                                                role,
                                                &[*uid],
                                                "(BODY.PEEK[HEADER] BODY.PEEK[TEXT] FLAGS)",
                                                "skip-uid",
                                                &detail,
                                            );
                                            session.logout().ok();
                                            session = reopen_imap_folder(account, folder)?;
                                        }
                                    }
                                }
                            }
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
                        continue;
                    }
                };
                for fetch in fetches.iter() {
                    if store_imap_body_fetch(app, state, account, role, folder, fetch)? {
                        count += 1;
                        folder_count += 1;
                    }
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

fn apply_imap_flag_fetch(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    folder: &str,
    fetch: &imap::types::Fetch,
    upstream_locations: Option<&mut HashSet<String>>,
) -> AppResult<(usize, usize, usize)> {
    let uid = fetch.uid.unwrap_or(fetch.message);
    let location_id = imap_location_key(folder, uid);
    if fetch.flags().contains(&imap::types::Flag::Deleted) {
        if let Some(upstream_locations) = upstream_locations {
            upstream_locations.remove(&location_id);
        }
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let deleted = delete_messages_by_imap_location(&conn, account.id, folder, uid)?;
        return Ok((1, deleted, deleted));
    }

    let is_read = fetch.flags().contains(&imap::types::Flag::Seen);
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let changed = if update_message_read_by_imap_location(&conn, account.id, folder, uid, is_read)?
    {
        1
    } else {
        0
    };
    Ok((1, 0, changed))
}

fn store_imap_body_fetch(
    app: Option<&tauri::AppHandle>,
    state: &State<'_, AppState>,
    account: &StoredAccount,
    role: &str,
    folder: &str,
    fetch: &imap::types::Fetch,
) -> AppResult<bool> {
    let Some(body) = fetch.body() else {
        return Ok(false);
    };
    let uid = fetch.uid.unwrap_or(fetch.message);
    store_imap_raw_message(
        app,
        state,
        account,
        ImapRawFetch {
            role,
            folder,
            uid,
            raw: body,
            is_read: fetch.flags().contains(&imap::types::Flag::Seen),
        },
    )
}

fn store_imap_header_text_fetch(
    app: Option<&tauri::AppHandle>,
    state: &State<'_, AppState>,
    account: &StoredAccount,
    role: &str,
    folder: &str,
    fetch: &imap::types::Fetch,
) -> AppResult<bool> {
    let (Some(header), Some(text)) = (fetch.header(), fetch.text()) else {
        return Ok(false);
    };
    let mut raw = Vec::with_capacity(header.len() + text.len() + 2);
    raw.extend_from_slice(header);
    raw.extend_from_slice(b"\r\n");
    raw.extend_from_slice(text);
    let uid = fetch.uid.unwrap_or(fetch.message);
    store_imap_raw_message(
        app,
        state,
        account,
        ImapRawFetch {
            role,
            folder,
            uid,
            raw: &raw,
            is_read: fetch.flags().contains(&imap::types::Flag::Seen),
        },
    )
}

fn store_imap_raw_message(
    app: Option<&tauri::AppHandle>,
    state: &State<'_, AppState>,
    account: &StoredAccount,
    fetch: ImapRawFetch<'_>,
) -> AppResult<bool> {
    let message = match imap_to_message(
        fetch.role,
        fetch.folder,
        fetch.uid,
        fetch.raw,
        fetch.is_read,
    ) {
        Ok(message) => message,
        Err(error) => {
            if let Some(app) = app {
                debug(
                    app,
                    format!(
                        "IMAP skipped unparsable message email={} folder={} uid={} error={}",
                        account.email, fetch.role, fetch.uid, error
                    ),
                );
            }
            return Ok(false);
        }
    };
    let conn = state.db.lock().map_err(|e| e.to_string())?;
    upsert_message(&conn, account.id, &message)?;
    Ok(true)
}

fn imap_to_message(
    role: &str,
    folder: &str,
    uid: u32,
    raw: &[u8],
    is_read: bool,
) -> AppResult<NewMessage> {
    let parsed = mailparse::parse_mail(raw).map_err(|e| e.to_string())?;
    let subject = parsed_header(&parsed, "Subject").unwrap_or_else(|| "(no subject)".to_string());
    let from_addr = parsed_header(&parsed, "From").unwrap_or_default();
    let to_addr = parsed_header(&parsed, "To").unwrap_or_default();
    let cc_addr = parsed_header(&parsed, "Cc").unwrap_or_default();
    let message_header_id = parsed_header(&parsed, "Message-ID")
        .as_deref()
        .and_then(normalize_message_id)
        .unwrap_or_default();
    let in_reply_to = normalize_message_id_header(parsed_header(&parsed, "In-Reply-To").as_deref());
    let references_header =
        normalize_message_id_header(parsed_header(&parsed, "References").as_deref());
    let normalized_subject = normalize_subject_key(&subject);
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
        provider_message_id: imap_provider_message_id(folder, uid, nonempty(&message_header_id)),
        thread_id: conversation_id_from_headers(
            &message_header_id,
            &in_reply_to,
            &references_header,
            &normalized_subject,
            &format!("imap-location:{folder}:{uid}"),
        ),
        message_header_id,
        in_reply_to,
        references_header,
        normalized_subject,
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

fn parsed_header(parsed: &mailparse::ParsedMail<'_>, name: &str) -> Option<String> {
    parsed
        .headers
        .get_first_value(name)
        .map(|value| clean_header_value(value.as_str()))
}

fn clean_header_value(value: &str) -> String {
    let mut cleaned = value.replace(['\r', '\n', '\t'], " ");
    for pattern in [
        "?==?utf-8?q?",
        "?==?utf-8?b?",
        "=?utf-8?q?",
        "=?utf-8?b?",
        "?==?",
        "?=",
    ] {
        cleaned = replace_case_insensitive(&cleaned, pattern, " ");
    }
    collapse_spaces(&cleaned)
}

fn replace_case_insensitive(value: &str, pattern: &str, replacement: &str) -> String {
    let mut output = String::new();
    let lower_value = value.to_ascii_lowercase();
    let lower_pattern = pattern.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(relative) = lower_value
        .get(offset..)
        .and_then(|tail| tail.find(&lower_pattern))
    {
        let start = offset + relative;
        let Some(before_match) = value.get(offset..start) else {
            output.push_str(value.get(offset..).unwrap_or_default());
            return output;
        };
        output.push_str(before_match);
        output.push_str(replacement);
        offset = start + pattern.len();
    }
    output.push_str(value.get(offset..).unwrap_or_default());
    output
}

fn collapse_spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_subject_key(subject: &str) -> String {
    let mut value = clean_header_value(subject);
    loop {
        let trimmed = value.trim_start();
        let lowered = trimmed.to_ascii_lowercase();
        let prefix = ["re:", "fw:", "fwd:"]
            .iter()
            .find(|prefix| lowered.starts_with(**prefix));
        if let Some(prefix) = prefix {
            value = trimmed
                .get(prefix.len()..)
                .unwrap_or_default()
                .trim_start()
                .to_string();
        } else {
            break;
        }
    }
    clean_header_value(&value).to_ascii_lowercase()
}

fn normalize_message_id_header(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    parse_message_ids(value).join(" ")
}

fn normalize_message_id(value: &str) -> Option<String> {
    parse_message_ids(value).into_iter().next()
}

fn parse_message_ids(value: &str) -> Vec<String> {
    if let Ok(ids) = mailparse::msgidparse(value) {
        return ids.iter().map(|id| format!("<{}>", id.trim())).collect();
    }

    let mut ids = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find('<') {
        let Some(after_start) = rest.get(start + 1..) else {
            break;
        };
        let Some(end) = after_start.find('>') else {
            break;
        };
        let id = after_start.get(..end).unwrap_or_default().trim();
        if !id.is_empty() {
            ids.push(format!("<{id}>"));
        }
        let Some(next_rest) = after_start.get(end + 1..) else {
            break;
        };
        rest = next_rest;
    }
    ids
}

fn conversation_id_from_headers(
    message_header_id: &str,
    in_reply_to: &str,
    references_header: &str,
    normalized_subject: &str,
    fallback: &str,
) -> String {
    let references = parse_message_ids(references_header);
    if let Some(root) = references.first() {
        return format!("message:{}", root.to_ascii_lowercase());
    }
    let reply_ids = parse_message_ids(in_reply_to);
    if let Some(parent) = reply_ids.first() {
        return format!("message:{}", parent.to_ascii_lowercase());
    }
    if !message_header_id.trim().is_empty() {
        return format!("message:{}", message_header_id.trim().to_ascii_lowercase());
    }
    if !normalized_subject.trim().is_empty() {
        return format!("subject:{}", normalized_subject.trim());
    }
    fallback.to_string()
}

fn message_header_id_from_provider_id(provider_message_id: &str) -> Option<String> {
    if let Some(message_id) = provider_message_id.strip_prefix("local-sent:") {
        return normalize_message_id(message_id);
    }
    if !provider_message_id.starts_with("imap-uid:") {
        return None;
    }
    let (_, _, message_id) = parse_imap_provider_message_id_parts(provider_message_id)?;
    message_id.and_then(normalize_message_id)
}

fn nonempty(value: &str) -> Option<&str> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
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
    const DROPPED_HTML_BLOCKS: [&str; 4] = ["script", "style", "head", "title"];
    let cleaned = remove_html_blocks(value, DROPPED_HTML_BLOCKS.as_slice());
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

fn imap_provider_message_id(folder: &str, uid: u32, message_id: Option<&str>) -> String {
    let base = format!("imap-uid:{folder}:{uid}");
    let Some(message_id) = message_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return base;
    };
    format!("{base}:{message_id}")
}

fn imap_location_key(folder: &str, uid: u32) -> String {
    format!("{folder}:{uid}")
}

fn imap_location_from_provider_id(provider_message_id: &str) -> Option<String> {
    let (folder, uid, _) = parse_imap_provider_message_id_parts(provider_message_id)?;
    Some(imap_location_key(&folder, uid))
}

fn parse_imap_provider_message_id_parts(
    provider_message_id: &str,
) -> Option<(String, u32, Option<&str>)> {
    let rest = provider_message_id.strip_prefix("imap-uid:")?;
    let mut parts = rest.splitn(3, ':');
    let folder = parts.next()?.to_string();
    let uid = parts.next()?.parse::<u32>().ok()?;
    let message_id = parts.next().filter(|value| !value.trim().is_empty());
    Some((folder, uid, message_id))
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
fn send_message(app: tauri::AppHandle, input: SendInput) -> AppResult<StoredMessage> {
    debug(&app, "Send message started");
    let sent = run_send_message(&app, input)?;
    debug(&app, "Send message complete");
    Ok(sent)
}

fn run_send_message(app: &tauri::AppHandle, input: SendInput) -> AppResult<StoredMessage> {
    ensure_not_resetting(app)?;
    let state = app.state::<AppState>();
    let account = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        get_account(&conn, input.account_id)?
    };
    let reply_context = reply_context(&state, input.reply_to_message_id)?;
    let sent_message_id = outbound_message_id(&account.email);
    let outbound =
        build_outbound_message(&account, &input, reply_context.as_ref(), &sent_message_id)?;

    if account.provider == "gmail" {
        send_gmail(&state, &account, &outbound, reply_context.as_ref())?;
    } else {
        send_smtp(&account, &outbound)?;
        match append_imap_sent_copy(&account, &outbound.formatted()) {
            Ok(folder) => debug(
                app,
                format!(
                    "SMTP sent copy appended email={} folder={folder}",
                    account.email
                ),
            ),
            Err(error) => debug(
                app,
                format!(
                    "SMTP sent copy append failed email={} error={error}",
                    account.email
                ),
            ),
        }
    }

    let sent = NewMessage {
        provider_message_id: format!("local-sent:{sent_message_id}"),
        thread_id: reply_context
            .as_ref()
            .map(|reply| reply.thread_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        message_header_id: sent_message_id.clone(),
        in_reply_to: reply_context
            .as_ref()
            .map(|reply| reply.message_header_id.clone())
            .unwrap_or_default(),
        references_header: reply_context
            .as_ref()
            .map(reply_references_header)
            .unwrap_or_default(),
        normalized_subject: normalize_subject_key(&input.subject),
        folder: "Sent".to_string(),
        subject: clean_header_value(&input.subject),
        from_addr: account.email.clone(),
        to_addr: input.to.clone(),
        cc_addr: input.cc.clone().unwrap_or_default(),
        date_ts: Utc::now().timestamp(),
        snippet: snippet_for_body(&input.body, &input.body_mime),
        body: input.body,
        body_mime: input.body_mime,
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
        message_header_id: sent.message_header_id,
        in_reply_to: sent.in_reply_to,
        references_header: sent.references_header,
        normalized_subject: sent.normalized_subject,
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
        thread_count: 1,
        thread_unread_count: 0,
        attachments: Vec::new(),
    })
}

fn send_gmail(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    outbound: &SmtpMessage,
    reply_context: Option<&ReplyContext>,
) -> AppResult<()> {
    let token = gmail_access_token(state, account)?;
    let encoded = general_purpose::URL_SAFE_NO_PAD.encode(outbound.formatted());
    let mut body = serde_json::Map::new();
    body.insert("raw".to_string(), Value::String(encoded));
    if let Some(reply_context) = reply_context.filter(|reply| reply.account_id == account.id) {
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

fn build_outbound_message(
    account: &StoredAccount,
    input: &SendInput,
    reply_context: Option<&ReplyContext>,
    message_id: &str,
) -> AppResult<SmtpMessage> {
    let from = account
        .email
        .parse::<Mailbox>()
        .map_err(|e| e.to_string())?;
    let to_recipients = parse_mailbox_list(&input.to, "To")?;
    if to_recipients.is_empty() {
        return Err("To must include at least one recipient".to_string());
    }
    let cc_recipients = parse_optional_mailbox_list(input.cc.as_deref(), "Cc")?;
    let bcc_recipients = parse_optional_mailbox_list(input.bcc.as_deref(), "Bcc")?;
    let mut builder = SmtpMessage::builder()
        .from(from)
        .subject(&input.subject)
        .message_id(Some(message_id.to_string()));
    for recipient in to_recipients {
        builder = builder.to(recipient);
    }
    for recipient in cc_recipients {
        builder = builder.cc(recipient);
    }
    for recipient in bcc_recipients {
        builder = builder.bcc(recipient);
    }
    if let Some(reply_to) = input
        .reply_to
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let mut reply_to_recipients = parse_mailbox_list(reply_to, "Reply-To")?;
        if reply_to_recipients.len() != 1 {
            return Err("Reply-To must contain exactly one mailbox".to_string());
        }
        builder = builder.reply_to(reply_to_recipients.remove(0));
    }
    if let Some(message_id) = reply_context
        .and_then(|reply| nonempty(&reply.message_header_id))
        .map(ToString::to_string)
    {
        let references = reply_context
            .map(reply_references_header)
            .unwrap_or_default();
        builder = builder
            .in_reply_to(message_id.clone())
            .references(references);
    }
    if input.body_mime.to_ascii_lowercase().contains("html") {
        builder
            .multipart(MultiPart::alternative_plain_html(
                html_to_text(&input.body),
                input.body.clone(),
            ))
            .map_err(|e| e.to_string())
    } else {
        builder.body(input.body.clone()).map_err(|e| e.to_string())
    }
}

fn parse_optional_mailbox_list(value: Option<&str>, label: &str) -> AppResult<Vec<Mailbox>> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => parse_mailbox_list(value, label),
        std::option::Option::None => Ok(Vec::new()),
    }
}

fn parse_mailbox_list(value: &str, label: &str) -> AppResult<Vec<Mailbox>> {
    let parsed = mailparse::addrparse(value).map_err(|e| format!("{label} address error: {e}"))?;
    let mut mailboxes = Vec::new();
    for address in parsed.iter() {
        collect_mailboxes(address, &mut mailboxes)
            .map_err(|e| format!("{label} address error: {e}"))?;
    }
    Ok(mailboxes)
}

fn collect_mailboxes(address: &MailAddr, mailboxes: &mut Vec<Mailbox>) -> AppResult<()> {
    match address {
        MailAddr::Single(info) => {
            let email = info.addr.parse::<Address>().map_err(|e| e.to_string())?;
            mailboxes.push(Mailbox::new(info.display_name.clone(), email));
        }
        MailAddr::Group(group) => {
            for info in &group.addrs {
                let email = info.addr.parse::<Address>().map_err(|e| e.to_string())?;
                mailboxes.push(Mailbox::new(info.display_name.clone(), email));
            }
        }
    }
    Ok(())
}

fn send_smtp(account: &StoredAccount, outbound: &SmtpMessage) -> AppResult<()> {
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
    let mailer = SmtpTransport::relay(host)
        .map_err(|e| e.to_string())?
        .port(validate_port(
            account.smtp_port.unwrap_or(587),
            "SMTP port",
        )?)
        .timeout(Some(Duration::from_secs(30)))
        .credentials(Credentials::new(username.to_string(), password.to_string()))
        .build();
    mailer.send(outbound).map_err(|e| e.to_string())?;
    Ok(())
}

fn append_imap_sent_copy(account: &StoredAccount, raw: &[u8]) -> AppResult<String> {
    let mut session = open_imap_session(account)?;
    let flags = [imap::types::Flag::Seen];
    let mut last_error = None;
    for folder in ["Sent", "Sent Items", "[Gmail]/Sent Mail"] {
        match session.append_with_flags(folder, raw, flags.as_slice()) {
            Ok(()) => {
                session.logout().ok();
                return Ok(folder.to_string());
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
    }
    session.logout().ok();
    Err(last_error.unwrap_or_else(|| "No Sent folder accepted APPEND".to_string()))
}

fn outbound_message_id(email: &str) -> String {
    let domain = email
        .rsplit_once('@')
        .map(|(_, domain)| domain.trim())
        .filter(|domain| !domain.is_empty())
        .unwrap_or("mailwind.local");
    format!("<{}@{}>", Uuid::new_v4(), domain)
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
        "
        SELECT account_id, thread_id, message_header_id, references_header
        FROM messages
        WHERE id = ?
        ",
        rusqlite::params_from_iter([message_id]),
        |row| {
            Ok(ReplyContext {
                account_id: row.get(0)?,
                thread_id: row.get(1)?,
                message_header_id: row.get(2)?,
                references_header: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn reply_references_header(reply_context: &ReplyContext) -> String {
    let mut refs = Vec::new();
    refs.extend(parse_message_ids(&reply_context.references_header));
    if !reply_context.message_header_id.is_empty() {
        refs.push(reply_context.message_header_id.clone());
    }
    dedup_message_ids(refs).join(" ")
}

fn dedup_message_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut output = Vec::new();
    for id in ids {
        let key = id.to_ascii_lowercase();
        if seen.insert(key) {
            output.push(id);
        }
    }
    output
}

fn reply_header_id(provider_message_id: &str) -> Option<String> {
    if let Some(message_id) = provider_message_id.strip_prefix("local-sent:") {
        let message_id = message_id.trim();
        if message_id.starts_with('<') && message_id.ends_with('>') {
            return Some(message_id.to_string());
        }
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
    debug(&app, "Delete message started");
    run_delete_message(&app, input)?;
    debug(&app, "Delete message complete");
    Ok(())
}

fn run_delete_message(app: &tauri::AppHandle, input: DeleteInput) -> AppResult<()> {
    ensure_not_resetting(app)?;
    let state = app.state::<AppState>();
    let (account, provider_message_id, message_header_id, folder) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, provider_message_id, message_header_id, folder): (
            i64,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT account_id, provider_message_id, message_header_id, folder FROM messages WHERE id = ?",
                params![input.message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| e.to_string())?;
        (
            get_account(&conn, account_id)?,
            provider_message_id,
            message_header_id,
            folder,
        )
    };

    let permanent = folder == "Trash";
    debug(
        app,
        format!(
            "Delete message id={} provider={} permanent={}",
            input.message_id, account.provider, permanent
        ),
    );

    let mut moved_imap_provider_id = None;
    let mut remove_unresolved_imap_row = false;
    if !is_local_sent_id(&provider_message_id) {
        if account.provider == "gmail" {
            delete_gmail_message(&state, &account, &provider_message_id, permanent)?;
        } else {
            moved_imap_provider_id = delete_imap_message(
                &account,
                &provider_message_id,
                permanent,
                &message_header_id,
            )?;
            remove_unresolved_imap_row = !permanent && moved_imap_provider_id.is_none();
        }
    }

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    if permanent {
        delete_message_row(&conn, input.message_id)?;
    } else if let Some(provider_id) = moved_imap_provider_id {
        conn.execute(
            "
            UPDATE messages
            SET folder = 'Trash', provider_message_id = ?, updated_at = ?
            WHERE id = ?
            ",
            params![provider_id, now_ts(), input.message_id],
        )
        .map_err(|e| e.to_string())?;
    } else if remove_unresolved_imap_row {
        delete_message_row(&conn, input.message_id)?;
    } else {
        conn.execute(
            "UPDATE messages SET folder = 'Trash', updated_at = ? WHERE id = ?",
            params![now_ts(), input.message_id],
        )
        .map_err(|e| e.to_string())?;
    }
    debug(app, "Delete complete");
    Ok(())
}

fn delete_gmail_message(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    provider_message_id: &str,
    permanent: bool,
) -> AppResult<()> {
    if is_local_sent_id(provider_message_id) {
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

#[tauri::command]
fn archive_message(app: tauri::AppHandle, input: DeleteInput) -> AppResult<()> {
    ensure_not_resetting(&app)?;
    debug(&app, "Archive message started");
    let state = app.state::<AppState>();
    let (account, provider_message_id, message_header_id, folder) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, provider_message_id, message_header_id, folder): (
            i64,
            String,
            String,
            String,
        ) = conn
            .query_row(
                "SELECT account_id, provider_message_id, message_header_id, folder FROM messages WHERE id = ?",
                params![input.message_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| e.to_string())?;
        (
            get_account(&conn, account_id)?,
            provider_message_id,
            message_header_id,
            folder,
        )
    };

    if folder == "Archive" {
        debug(&app, "Archive skipped because message is already archived");
        return Ok(());
    }
    if folder == "Trash" {
        return Err("Restore the message before archiving it".to_string());
    }

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    if is_local_sent_id(&provider_message_id) {
        conn.execute(
            "UPDATE messages SET folder = 'Archive', updated_at = ? WHERE id = ?",
            params![now_ts(), input.message_id],
        )
        .map_err(|e| e.to_string())?;
        debug(&app, "Archive message complete");
        return Ok(());
    }
    drop(conn);

    if account.provider == "gmail" {
        archive_gmail_message(&state, &account, &provider_message_id)?;
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE messages SET folder = 'Archive', updated_at = ? WHERE id = ?",
            params![now_ts(), input.message_id],
        )
        .map_err(|e| e.to_string())?;
    } else {
        let Some(location) = move_imap_message_to_folder(
            &account,
            &provider_message_id,
            &["Archive", "Archives"],
            &message_header_id,
        )?
        else {
            return Err("No IMAP Archive folder accepted the message".to_string());
        };
        let provider_id =
            imap_provider_message_id(&location.folder, location.uid, nonempty(&message_header_id));
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        conn.execute(
            "
            UPDATE messages
            SET folder = 'Archive', provider_message_id = ?, updated_at = ?
            WHERE id = ?
            ",
            params![provider_id, now_ts(), input.message_id],
        )
        .map_err(|e| e.to_string())?;
    }
    debug(&app, "Archive message complete");
    Ok(())
}

fn archive_gmail_message(
    state: &State<'_, AppState>,
    account: &StoredAccount,
    provider_message_id: &str,
) -> AppResult<()> {
    let token = gmail_access_token(state, account)?;
    let body = serde_json::json!({ "removeLabelIds": ["INBOX"] });
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

fn delete_imap_message(
    account: &StoredAccount,
    provider_message_id: &str,
    permanent: bool,
    message_header_id: &str,
) -> AppResult<Option<String>> {
    let (folder, uid, provider_message_id_header) =
        parse_imap_provider_message_id(provider_message_id)?;
    let mut session = open_imap_session(account)?;
    session.select(&folder).map_err(|e| e.to_string())?;
    if permanent {
        session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
            .map_err(|e| e.to_string())?;
        session.uid_expunge(uid.to_string()).ok();
        session.expunge().ok();
        session.logout().ok();
        return Ok(None);
    }

    let lookup_message_id = nonempty(message_header_id)
        .map(ToString::to_string)
        .or_else(|| provider_message_id_header.map(ToString::to_string));
    let mut moved_folder = None;
    for trash in ["Trash", "Deleted Items", "[Gmail]/Trash"] {
        if session.uid_mv(uid.to_string(), trash).is_ok() {
            moved_folder = Some(trash.to_string());
            break;
        }
        if session.uid_copy(uid.to_string(), trash).is_ok() {
            session
                .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
                .map_err(|e| e.to_string())?;
            session.uid_expunge(uid.to_string()).ok();
            session.expunge().ok();
            moved_folder = Some(trash.to_string());
            break;
        }
    }
    if let Some(folder) = moved_folder {
        let moved_location = lookup_message_id.as_deref().and_then(|message_id| {
            find_imap_uid_by_message_id(&mut session, &folder, message_id)
                .ok()
                .flatten()
        });
        session.logout().ok();
        return Ok(moved_location
            .map(|uid| imap_provider_message_id(&folder, uid, lookup_message_id.as_deref())));
    }

    session
        .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
        .map_err(|e| e.to_string())?;
    session.uid_expunge(uid.to_string()).ok();
    session.expunge().ok();
    session.logout().ok();
    Ok(None)
}

fn move_imap_message_to_folder(
    account: &StoredAccount,
    provider_message_id: &str,
    target_folders: &[&str],
    message_header_id: &str,
) -> AppResult<Option<ImapMovedLocation>> {
    let (folder, uid, provider_message_id_header) =
        parse_imap_provider_message_id(provider_message_id)?;
    let lookup_message_id = nonempty(message_header_id)
        .map(ToString::to_string)
        .or_else(|| provider_message_id_header.map(ToString::to_string));
    let mut session = open_imap_session(account)?;
    session.select(&folder).map_err(|e| e.to_string())?;
    let mut moved_folder = None;
    for target in target_folders {
        if session.uid_mv(uid.to_string(), target).is_ok() {
            moved_folder = Some((*target).to_string());
            break;
        }
        if session.uid_copy(uid.to_string(), target).is_ok() {
            session
                .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Deleted)")
                .map_err(|e| e.to_string())?;
            session.uid_expunge(uid.to_string()).ok();
            session.expunge().ok();
            moved_folder = Some((*target).to_string());
            break;
        }
    }
    let Some(folder) = moved_folder else {
        session.logout().ok();
        return Ok(None);
    };
    let uid = lookup_message_id.as_deref().and_then(|message_id| {
        find_imap_uid_by_message_id(&mut session, &folder, message_id)
            .ok()
            .flatten()
    });
    session.logout().ok();
    Ok(uid.map(|uid| ImapMovedLocation { folder, uid }))
}

fn find_imap_uid_by_message_id(
    session: &mut ImapSession,
    folder: &str,
    message_id: &str,
) -> AppResult<Option<u32>> {
    select_imap_folder(session, folder)?;
    let query = format!("HEADER Message-ID {}", imap_search_quoted(message_id));
    let uids = session
        .uid_search(query)
        .map_err(|e| format!("IMAP search by Message-ID failed: {}", imap_error(e)))?;
    Ok(uids.into_iter().max())
}

fn imap_search_quoted(value: &str) -> String {
    let mut escaped = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\r' | '\n' => escaped.push(' '),
            _ => escaped.push(ch),
        }
    }
    escaped.push('"');
    escaped
}

fn open_imap_session(
    account: &StoredAccount,
) -> AppResult<imap::Session<native_tls::TlsStream<std::net::TcpStream>>> {
    let host = account
        .imap_host
        .as_deref()
        .ok_or_else(|| "Missing IMAP host".to_string())?;
    let port = validate_port(account.imap_port.unwrap_or(993), "IMAP port")?;
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

fn parse_imap_provider_message_id(
    provider_message_id: &str,
) -> AppResult<(String, u32, Option<&str>)> {
    parse_imap_provider_message_id_parts(provider_message_id)
        .ok_or_else(|| "Cannot identify IMAP message UID".to_string())
}

#[tauri::command]
fn download_attachment(
    app: tauri::AppHandle,
    input: DownloadAttachmentInput,
) -> AppResult<DownloadedAttachment> {
    debug(&app, "Download attachment started");
    let downloaded = run_download_attachment(&app, input)?;
    debug(&app, "Download attachment complete");
    Ok(downloaded)
}

fn run_download_attachment(
    app: &tauri::AppHandle,
    input: DownloadAttachmentInput,
) -> AppResult<DownloadedAttachment> {
    ensure_not_resetting(app)?;
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
    run_mark_message_read(&app, input)
}

#[tauri::command]
fn mark_thread_read(app: tauri::AppHandle, input: MarkReadInput) -> AppResult<()> {
    run_mark_thread_read(&app, input)
}

fn run_mark_thread_read(app: &tauri::AppHandle, input: MarkReadInput) -> AppResult<()> {
    ensure_not_resetting(app)?;
    let state = app.state::<AppState>();
    let folder_scope = match input.folder.as_deref() {
        Some("Trash") => "AND folder = 'Trash'",
        Some(_) => "AND folder <> 'Trash'",
        std::option::Option::None => "",
    };
    let (account, thread_id, messages) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, thread_id): (i64, String) = conn
            .query_row(
                "SELECT account_id, thread_id FROM messages WHERE id = ?",
                params![input.message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        let sql = format!(
            "
            SELECT id, provider_message_id
            FROM messages
            WHERE account_id = ? AND thread_id = ?
            {folder_scope}
            "
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![account_id, thread_id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| e.to_string())?;
        let messages = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        (get_account(&conn, account_id)?, thread_id, messages)
    };

    for (_, provider_message_id) in &messages {
        if is_local_sent_id(provider_message_id) {
            continue;
        }
        if account.provider == "gmail" {
            mark_gmail_message_read(&state, &account, provider_message_id, input.is_read)?;
        } else {
            mark_imap_message_read(&account, provider_message_id, input.is_read)?;
        }
    }

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    let sql = format!(
        "
        UPDATE messages
        SET is_read = ?, updated_at = ?
        WHERE account_id = ? AND thread_id = ?
        {folder_scope}
        "
    );
    conn.execute(
        &sql,
        params![
            if input.is_read { 1 } else { 0 },
            now_ts(),
            account.id,
            thread_id
        ],
    )
    .map_err(|e| e.to_string())?;
    debug(
        app,
        format!(
            "Thread read state updated messages={} is_read={}",
            messages.len(),
            input.is_read
        ),
    );
    Ok(())
}

fn run_mark_message_read(app: &tauri::AppHandle, input: MarkReadInput) -> AppResult<()> {
    ensure_not_resetting(app)?;
    let state = app.state::<AppState>();
    let (account, provider_message_id) = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        let (account_id, provider_message_id): (i64, String) = conn
            .query_row(
                "SELECT account_id, provider_message_id FROM messages WHERE id = ?",
                params![input.message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| e.to_string())?;
        (get_account(&conn, account_id)?, provider_message_id)
    };

    if !is_local_sent_id(&provider_message_id) {
        if account.provider == "gmail" {
            mark_gmail_message_read(&state, &account, &provider_message_id, input.is_read)?;
        } else {
            mark_imap_message_read(&account, &provider_message_id, input.is_read)?;
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
    delete_account_secrets(input.account_id)?;
    conn.execute(
        "DELETE FROM accounts WHERE id = ?",
        params![input.account_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn reset_database(app: tauri::AppHandle) -> AppResult<()> {
    {
        let state = app.state::<AppState>();
        let mut resetting = state.resetting.lock().map_err(|e| e.to_string())?;
        if *resetting {
            return Err("Database reset is already running".to_string());
        }
        *resetting = true;
        drop(resetting);

        let syncing = state.syncing.lock().map_err(|e| e.to_string())?;
        if *syncing {
            if let Ok(mut resetting) = state.resetting.lock() {
                *resetting = false;
            }
            return Err("Cannot reset the database while sync is running".to_string());
        }
        drop(syncing);
        let polling = state.polling.lock().map_err(|e| e.to_string())?;
        if *polling {
            if let Ok(mut resetting) = state.resetting.lock() {
                *resetting = false;
            }
            return Err("Cannot reset the database while live poll is running".to_string());
        }
    }

    let result = reset_database_inner(&app);

    let state = app.state::<AppState>();
    if let Ok(mut resetting) = state.resetting.lock() {
        *resetting = false;
    }

    result
}

fn reset_database_inner(app: &tauri::AppHandle) -> AppResult<()> {
    debug(app, "Resetting local database");
    let state = app.state::<AppState>();
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let db_path = dir.join("mailwind.sqlite3");

    {
        let mut conn = state.db.lock().map_err(|e| e.to_string())?;
        delete_all_account_secrets(&conn)?;
        let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
        let replacement = Connection::open_in_memory().map_err(|e| e.to_string())?;
        *conn = replacement;
    }

    for path in sqlite_database_files(&db_path) {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Failed to remove {}: {error}", path.display())),
        }
    }

    let new_conn = init_db(app).map_err(|e| e.to_string())?;
    {
        let mut conn = state.db.lock().map_err(|e| e.to_string())?;
        *conn = new_conn;
    }
    state
        .idle_accounts
        .lock()
        .map_err(|e| e.to_string())?
        .clear();
    debug(app, "Local database reset complete");
    Ok(())
}

fn sqlite_database_files(db_path: &Path) -> Vec<PathBuf> {
    ["", "-wal", "-shm"]
        .iter()
        .map(|suffix| {
            let mut path = db_path.as_os_str().to_os_string();
            path.push(suffix);
            PathBuf::from(path)
        })
        .collect()
}

#[tauri::command]
fn update_imap_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    input: UpdateImapSettingsInput,
) -> AppResult<()> {
    let email = normalize_email_input(&input.email, "Email")?;
    let imap_host = required_input(&input.imap_host, "IMAP host")?;
    let smtp_host = required_input(&input.smtp_host, "SMTP host")?;
    let username = required_input(&input.username, "Username")?;
    let imap_port = validate_port(input.imap_port, "IMAP port")?;
    let smtp_port = validate_port(input.smtp_port, "SMTP port")?;
    let display_name = input
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

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
    let duplicate_account_id = {
        let conn = state.db.lock().map_err(|e| e.to_string())?;
        existing_account_id(&conn, "imap", &email)?
    };
    if let Some(existing_id) = duplicate_account_id {
        if existing_id != input.account_id {
            return Err(format!("An IMAP account for {email} already exists"));
        }
    }

    debug(
        &app,
        format!(
            "IMAP settings update test account_id={} email={} imap={}:{} smtp={}:{} username={}",
            input.account_id, email, imap_host, imap_port, smtp_host, smtp_port, username
        ),
    );
    let tls = native_tls::TlsConnector::builder()
        .build()
        .map_err(|e| e.to_string())?;
    let client = connect_imap_client(imap_host.as_str(), imap_port, &tls)?;
    let mut session = client
        .login(username.as_str(), password.as_str())
        .map_err(|e| format!("IMAP login failed or timed out after 15s: {}", e.0))?;
    session.logout().ok();
    validate_smtp_login(&smtp_host, smtp_port, &username, &password)?;

    let conn = state.db.lock().map_err(|e| e.to_string())?;
    if input
        .password
        .as_deref()
        .is_some_and(|value| !value.is_empty())
    {
        store_account_secret(input.account_id, "password", &password)?;
    }
    conn.execute(
        "
        UPDATE accounts
        SET email = ?, display_name = ?, imap_host = ?, imap_port = ?,
            smtp_host = ?, smtp_port = ?, username = ?, password = NULL
        WHERE id = ? AND provider = 'imap'
        ",
        params![
            email,
            display_name,
            imap_host,
            input.imap_port,
            smtp_host,
            input.smtp_port,
            username,
            input.account_id
        ],
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
    provider_message_id: &str,
    is_read: bool,
) -> AppResult<()> {
    let (folder, uid, _) = parse_imap_provider_message_id(provider_message_id)?;
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
                resetting: Mutex::new(false),
                idle_accounts: Mutex::new(HashSet::new()),
            });
            start_mail_polling(app.handle().clone());
            start_imap_idle_manager(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            archive_message,
            connect_gmail,
            connect_imap,
            delete_message,
            download_attachment,
            list_accounts,
            list_folders,
            list_messages,
            list_thread_messages,
            mark_message_read,
            mark_thread_read,
            mailbox_snapshot,
            remove_account,
            reset_database,
            send_message,
            sync_account,
            sync_all,
            update_imap_settings
        ])
        .run(tauri::tauri_build_context!())
        .expect("error while running tauri application");
}
