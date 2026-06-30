# Mailwind

**Mailwind** is a desktop-first email application that brings multiple email accounts into one unified, fast, and calm workspace.

The goal is to help users connect all their email accounts — Gmail, Google Workspace, iCloud, Outlook, work emails, and custom domain emails — and manage everything from one consolidated view without constantly switching accounts or inboxes.

## Vision

Mailwind aims to become a modern local-first email command center for people who manage multiple inboxes.

Instead of opening different email apps, switching accounts, or checking multiple tabs, users can see all their emails, sent messages, trash, categories, and conversations in one clean desktop app.

## Problem

Many users today manage more than one email account:

- Personal Gmail
- Work email
- Google Workspace domain email
- iCloud email
- Outlook or Microsoft 365
- Custom domain emails
- Shared inboxes like `support@`, `sales@`, or `billing@`

This creates several problems:

- Constant account switching
- Missed important emails
- Scattered sent and trash folders
- No unified search across accounts
- Different providers organize emails differently
- Managing work, personal, promotions, and social emails becomes messy
- Users lose time moving between inboxes instead of getting work done

## Solution

Mailwind provides one unified desktop workspace for all connected email accounts.

Users can connect multiple email providers and view their emails in consolidated sections such as:

- Unified Inbox
- Primary
- Social
- Promotions
- Updates
- Sent
- Trash
- Archive
- Drafts
- Account-specific views

Mailwind normalizes emails from different providers into one clean local database and presents them in a simple, fast, and organized interface.

## Core Idea

Mailwind is not just another email client.

It is designed as:

> One calm place for every email account.

The app focuses on speed, clarity, privacy, and productivity.

## Target Users

Mailwind is useful for:

- Founders
- Freelancers
- Agency owners
- Startup teams
- Developers
- Operators
- Consultants
- People with multiple personal and work inboxes
- Google Workspace/domain email users
- Users who want a Thunderbird-like local app with a modern UI

## Key Features

### 1. Multi-Account Connection

Users can connect multiple email accounts from different providers:

- Gmail
- Google Workspace
- iCloud
- Outlook
- Microsoft 365
- Custom domain emails
- IMAP/SMTP providers

### 2. Unified Inbox

All incoming emails from connected accounts appear in one combined inbox.

Each email clearly shows which account it belongs to, so users always know where it came from.

### 3. Unified Categories

Emails are grouped into clean categories:

- Primary
- Social
- Promotions
- Updates
- Forums
- Newsletters
- Work
- Personal

For Gmail and Google Workspace, provider categories can be used where available. For other providers, Mailwind can apply its own local classification rules.

### 4. Unified Sent, Trash, Archive, and Drafts

Users can view sent emails, trash, archive, and drafts across all accounts in one place.

This removes the need to open each provider separately just to find an old email.

### 5. Correct Account Sending

When replying to an email, Mailwind automatically uses the correct account or alias.

Users can also manually choose the sender identity, such as:

- `personal@gmail.com`
- `founder@company.com`
- `support@company.com`
- `sales@company.com`

### 6. Local-First Storage

Mailwind stores synced email data locally on the user’s device.

This enables:

- Fast search
- Offline access
- Better privacy
- Reduced dependency on a backend
- Desktop-first performance

### 7. Fast Local Search

Users can search across all connected accounts from one search bar.

Search can include:

- Sender
- Subject
- Body
- Attachments metadata
- Account
- Category
- Date range

### 8. Desktop-First Experience

Mailwind will first launch as a desktop app for:

- macOS
- Windows
- Linux

Mobile apps can be added later.

## Technical Direction

Mailwind will be built using a desktop-first Tauri v2 architecture.

### Frontend

- React
- TypeScript
- Vite
- Tailwind CSS
- shadcn/ui
- Zustand or Jotai for local UI state
- TanStack Query for async state where useful

### Desktop Shell

- Tauri v2

### Backend / Native Core

- Rust
- Tokio
- SQLx
- SQLite
- SQLite FTS5 for local search

### Email Integrations

Initial provider strategy:

| Provider                | Preferred Integration |
| ----------------------- | --------------------- |
| Gmail                   | Gmail API             |
| Google Workspace        | Gmail API             |
| iCloud                  | IMAP + SMTP           |
| Custom domain email     | IMAP + SMTP           |
| Outlook / Microsoft 365 | Microsoft Graph       |
| Generic providers       | IMAP + SMTP           |

## High-Level Architecture

```txt
Mailwind Desktop App

React UI
  ├── Inbox
  ├── Message list
  ├── Thread view
  ├── Composer
  ├── Search
  ├── Settings
  └── Account management

Tauri IPC Bridge

Rust Core
  ├── Account manager
  ├── Provider adapters
  ├── Sync engine
  ├── Email parser
  ├── Search engine
  ├── Local classifier
  ├── Outbox queue
  └── SQLite storage

Local Device
  ├── Email database
  ├── Search index
  ├── Attachment cache
  └── Secure token storage
```

## Local Data Model

Core entities:

```txt
accounts
identities
folders
labels
threads
messages
message_bodies
attachments
categories
sync_cursors
outbox
contacts
rules
settings
```

## MVP Scope

The first version should focus on a strong desktop experience.

### MVP Features

- Tauri desktop app
- React UI
- Local SQLite database
- Gmail / Google Workspace account connection
- Unified inbox
- Account badges
- Message list
- Thread detail view
- Basic search
- Sent and trash views
- Basic email sending/replying
- Local sync engine
- Simple category grouping

## Future Features

After the MVP, Mailwind can add:

- IMAP/SMTP support
- iCloud support
- Outlook/Microsoft 365 support
- Advanced search filters
- Smart categories
- Local rules engine
- AI summaries
- AI reply assistance
- Snooze
- Follow-up reminders
- Unsubscribe manager
- Attachment search
- Calendar integration
- Contact timeline
- Mobile apps
- Optional cloud sync
- Optional push notification backend

## Product Positioning

Mailwind should not be positioned as just another email client.

It should be positioned as:

> A modern desktop workspace for people with too many inboxes.

Possible tagline:

> Every inbox. One smooth flow.

## Core Principles

Mailwind should be:

- Fast
- Clean
- Local-first
- Privacy-conscious
- Cross-account by default
- Easy to use
- Powerful for advanced users
- Calm instead of noisy
- Built for productivity

## Long-Term Vision

Mailwind can become a complete communication workspace where users manage all email-related work from one place.

Long term, it can support:

- Emails
- Contacts
- Tasks
- Follow-ups
- AI summaries
- Workflows
- Shared inboxes
- Team collaboration
- Customer timelines
- Business inbox management

The goal is to make email feel less fragmented and more organized.

## Final Concept

Mailwind is a local-first desktop email app that connects all user email accounts and creates one unified workspace for reading, searching, sending, organizing, and acting on email.

It starts as a desktop app and can later expand to mobile.

The first version should focus on being fast, reliable, beautiful, and useful for people who manage multiple inboxes every day.
