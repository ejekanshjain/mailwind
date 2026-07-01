# Mailwind

Mailwind is a local-first desktop email app for people who manage more than one inbox.

It brings Gmail, Google Workspace, and generic IMAP/SMTP accounts into one fast workspace with a unified inbox, local search, sending, replies, read state, delete actions, attachments, and desktop notifications. The app is built with Tauri, React, TypeScript, Rust, and SQLite.

> Current status: early MVP. Mailwind is usable for local development and testing, but credentials are currently stored in the local SQLite database rather than an OS keychain.

## Why Mailwind

Most email clients are either browser tabs, provider-specific apps, or heavy desktop tools. Mailwind is designed as a calm desktop command center for multiple accounts:

- One place for personal, work, domain, Gmail, and IMAP mail.
- Local-first storage for speed and privacy.
- Fast mailbox switching and SQLite FTS search.
- Correct-account sending and replying.
- No hosted backend required for the current MVP.

## Features

- Tauri desktop app for Linux, macOS, and Windows targets.
- React + TypeScript workspace UI.
- Gmail / Google Workspace OAuth connection.
- Generic IMAP + SMTP account connection.
- Unified account and folder navigation.
- Inbox, Sent, Trash, and Archive views.
- Local SQLite persistence in the app data directory.
- SQLite FTS5 search across subject, sender, recipient, and body.
- Concurrent account sync based on available CPU count.
- Message reading, read/unread changes, delete/trash behavior, and replies.
- SMTP sending for IMAP accounts and Gmail API sending for Gmail accounts.
- Attachment listing and download flow.
- Desktop notifications for new mail.
- Visible debug log panel for connection, sync, and mail-action troubleshooting.
- Linux `deb` and `rpm` bundle targets configured.

## Tech Stack

- Desktop shell: Tauri v2
- Frontend: React 19, TypeScript, Vite
- Native core: Rust
- Database: SQLite through `rusqlite`, with FTS5 enabled
- Email: Gmail API, IMAP, SMTP
- Runtime/tooling: Bun, Cargo, Tauri CLI

## Project Structure

```txt
.
|-- src/                    # React desktop UI
|   |-- App.tsx
|   |-- App.css
|   `-- main.tsx
|-- src-tauri/              # Tauri/Rust application core
|   |-- src/lib.rs          # SQLite, OAuth, IMAP, SMTP, sync, commands
|   |-- src/main.rs
|   |-- Cargo.toml
|   `-- tauri.conf.json
|-- IDEA.md                 # Original product direction
|-- package.json            # Frontend and Tauri scripts
`-- bun.lock
```

## Prerequisites

Install these before running the app:

- Bun
- Rust and Cargo
- Tauri v2 system prerequisites for your OS
- A Google Cloud OAuth client if you want to connect Gmail
- IMAP/SMTP credentials if you want to connect a generic mailbox

On Linux, Tauri also needs the usual WebKit/GTK build dependencies. Follow the official Tauri prerequisites for your distribution.

## Setup

Clone the repository:

```bash
git clone https://github.com/ejekanshjain/mailwind.git
cd mailwind
```

Install JavaScript dependencies:

```bash
bun install
```

Run the desktop app in development:

```bash
bun run tauri dev
```

Build the frontend only:

```bash
bun run build
```

Check the Rust/Tauri core:

```bash
cd src-tauri
cargo check --all-targets
```

Build desktop bundles:

```bash
bun run tauri build
```

The current Tauri config builds Linux `deb` and `rpm` packages.

## Connecting Gmail

Mailwind uses a local OAuth callback server for Gmail sign-in.

1. Open Google Cloud Console.
2. Create or select a project.
3. Enable the Gmail API.
4. Configure the OAuth consent screen.
5. Create an OAuth client for a desktop app.
6. Run Mailwind with `bun run tauri dev`.
7. Open the account setup view.
8. Paste the Google OAuth client ID and client secret.
9. Complete the browser consent flow.

The app requests Gmail modify and send scopes so it can sync, update read state, move/delete messages, and send replies.

## Connecting IMAP/SMTP

Use the IMAP setup form for providers such as custom domain email, iCloud-compatible mailboxes, or other mail hosts that expose IMAP and SMTP.

You need:

- Email address
- Display name, optional
- IMAP host and port, usually `993`
- SMTP host and port, usually `587`
- Username
- Password or provider app password

For Gmail over IMAP, make sure IMAP is enabled and use an app password when required by your Google account settings.

## Local Data

Mailwind stores synced mail locally in SQLite under the operating system app data directory. The database file is named:

```txt
mailwind.sqlite3
```

The database currently stores account tokens and IMAP/SMTP credentials. Do not commit local data files, share a development profile, or use sensitive production mailboxes until secure credential storage is added.

## Development Notes

- The frontend talks to Rust through Tauri commands.
- `src-tauri/src/lib.rs` owns account storage, Gmail OAuth, IMAP/SMTP, sync, sending, read/delete actions, attachment download, and SQLite queries.
- Mailbox navigation uses indexed SQLite queries and a small UI-side snapshot cache for snappier switching.
- Sync runs in the background and can process multiple accounts concurrently.
- Debug events are emitted through the `mailwind-debug` Tauri event and shown in the UI.

## Useful Commands

```bash
# Start Vite only
bun run dev

# Start the full desktop app
bun run tauri dev

# Build frontend assets
BUN_TMPDIR=/tmp/bun-tmp bun run build

# Preview frontend assets
bun run preview

# Check Rust targets
cd src-tauri && cargo check --all-targets

# Run Rust tests
cd src-tauri && cargo test

# Build configured desktop bundles
bun run tauri build
```

## Contributing

Contributions are welcome once the repository is public.

Good first areas:

- Provider presets for common IMAP/SMTP hosts.
- UI polish and accessibility improvements.
- Secure credential storage.
- Test coverage for sync, search, and mail actions.
- Packaging improvements for macOS, Windows, and Linux.

Please keep changes focused, run the relevant checks, and avoid committing local mail data or credentials.

## License

Mailwind is source-available under the PolyForm Shield License 1.0.0. You may view, use, study, modify, and contribute to the project for personal or professional purposes, but you may not provide a competing product or repackage Mailwind as a separate competing email app or service without separate permission from the project owner.
