import {
  type CSSProperties,
  type KeyboardEvent,
  type SyntheticEvent,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { setTheme as setAppTheme } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import {
  Archive,
  Bug,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Download,
  EyeOff,
  Inbox,
  Mail,
  Palette,
  Paperclip,
  PenLine,
  RefreshCw,
  Search,
  Send,
  Settings,
  SlidersHorizontal,
  Trash2,
} from "lucide-react";
import "./App.css";

type Account = {
  id: number;
  provider: string;
  email: string;
  display_name: string | null;
  imap_host: string | null;
  imap_port: number | null;
  smtp_host: string | null;
  smtp_port: number | null;
  username: string | null;
};

type Folder = {
  name: string;
  count: number;
  unread_count: number;
};

type MailMessage = {
  id: number;
  account_id: number;
  provider_message_id: string;
  thread_id: string;
  folder: string;
  subject: string;
  from_addr: string;
  to_addr: string;
  cc_addr: string;
  date_ts: number;
  snippet: string;
  body: string;
  body_mime: string;
  is_read: boolean;
  account_email: string;
  account_provider: string;
  attachments: Attachment[];
};

type Attachment = {
  id: number;
  filename: string;
  mime_type: string;
  size: number;
};

type Snapshot = {
  accounts: Account[];
  folders: Folder[];
  messages: MailMessage[];
  page: number;
  page_size: number;
  total: number;
};

type MailboxChanged = {
  new_messages: number;
  unread_inbox: number;
};

type ReadFilter = "all" | "read" | "unread";
type AppView = "mail" | "settings";

type ImapSettingsForm = {
  email: string;
  display_name: string;
  imap_host: string;
  imap_port: string;
  smtp_host: string;
  smtp_port: string;
  username: string;
  password: string;
};

const emptySnapshot: Snapshot = {
  accounts: [],
  folders: [],
  messages: [],
  page: 0,
  page_size: 50,
  total: 0,
};

const themeOptions = [
  { label: "Blue", value: "#2563eb" },
  { label: "Teal", value: "#0f9f8f" },
  { label: "Violet", value: "#7c3aed" },
  { label: "Rose", value: "#e11d48" },
  { label: "Amber", value: "#d97706" },
];
const themeStorageKey = "mailwind-theme";
const themeOverrideStorageKey = "mailwind-theme-override";

function systemPrefersDark() {
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ?? false;
}

function hexToRgb(value: string) {
  const clean = value.replace("#", "");
  const numeric = Number.parseInt(clean, 16);
  return `${(numeric >> 16) & 255}, ${(numeric >> 8) & 255}, ${numeric & 255}`;
}

function App() {
  const [snapshot, setSnapshot] = useState<Snapshot>(emptySnapshot);
  const [folder, setFolder] = useState("Inbox");
  const [accountId, setAccountId] = useState<number | null>(null);
  const [query, setQuery] = useState("");
  const [readFilter, setReadFilter] = useState<ReadFilter>("all");
  const [page, setPage] = useState(0);
  const [pageSize] = useState(50);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [selectedMessage, setSelectedMessage] = useState<MailMessage | null>(null);
  const [view, setView] = useState<AppView>("mail");
  const [darkMode, setDarkMode] = useState(
    () => {
      const hasOverride = window.localStorage.getItem(themeOverrideStorageKey) === "true";
      const stored = window.localStorage.getItem(themeStorageKey);
      if (hasOverride && stored === "dark") return true;
      if (hasOverride && stored === "light") return false;
      return systemPrefersDark();
    },
  );
  const [themeOverridden, setThemeOverridden] = useState(
    () => window.localStorage.getItem(themeOverrideStorageKey) === "true",
  );
  const [accentColor, setAccentColor] = useState(
    () => window.localStorage.getItem("mailwind-accent") || themeOptions[0].value,
  );
  const [status, setStatus] = useState("Ready");
  const [showDebug, setShowDebug] = useState(false);
  const [showComposer, setShowComposer] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [debugLines, setDebugLines] = useState<string[]>([]);
  const [composer, setComposer] = useState({ account_id: "", to: "", subject: "", body: "" });
  const [notificationsEnabled, setNotificationsEnabled] = useState(
    () => window.localStorage.getItem("mailwind-notifications") === "enabled",
  );
  const [editingAccountId, setEditingAccountId] = useState<number | null>(null);
  const [accountEditForm, setAccountEditForm] = useState<ImapSettingsForm | null>(null);
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const messageRowRefs = useRef<Record<number, HTMLButtonElement | null>>({});
  const leftAltDownRef = useRef(false);
  const composerBodyRef = useRef<HTMLTextAreaElement | null>(null);
  const [gmailForm, setGmailForm] = useState({ client_id: "", client_secret: "" });
  const [imapForm, setImapForm] = useState({
    email: "",
    display_name: "",
    imap_host: "",
    imap_port: "993",
    smtp_host: "",
    smtp_port: "587",
    username: "",
    password: "",
  });

  const selected = useMemo(
    () =>
      snapshot.messages.find((message) => message.id === selectedId) ??
      (selectedMessage?.id === selectedId ? selectedMessage : null),
    [snapshot.messages, selectedId, selectedMessage],
  );

  useEffect(() => {
    void load();
  }, [folder, accountId, page, readFilter]);

  useEffect(() => {
    if (themeOverridden) {
      window.localStorage.setItem(themeOverrideStorageKey, "true");
      window.localStorage.setItem(themeStorageKey, darkMode ? "dark" : "light");
    }
    document.documentElement.style.colorScheme = darkMode ? "dark" : "light";
    void setAppTheme(darkMode || systemPrefersDark() ? "dark" : "light").catch(() => {
      // Browser-only Vite previews do not have the Tauri app API.
    });
  }, [darkMode, themeOverridden]);

  useEffect(() => {
    if (!window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (event: MediaQueryListEvent) => {
      if (!themeOverridden) {
        setDarkMode(event.matches);
      }
      void setAppTheme(darkMode || event.matches ? "dark" : "light").catch(() => {
        // Browser-only Vite previews do not have the Tauri app API.
      });
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [darkMode, themeOverridden]);

  useEffect(() => {
    window.localStorage.setItem("mailwind-accent", accentColor);
  }, [accentColor]);

  useEffect(() => {
    window.localStorage.setItem(
      "mailwind-notifications",
      notificationsEnabled ? "enabled" : "disabled",
    );
  }, [notificationsEnabled]);

  useEffect(() => {
    const resetLeftAlt = () => {
      leftAltDownRef.current = false;
    };
    const onKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.code === "AltLeft") {
        leftAltDownRef.current = true;
        return;
      }
      if (
        leftAltDownRef.current &&
        event.altKey &&
        (event.key === "ArrowUp" || event.key === "ArrowDown") &&
        !isEditableTarget(event.target)
      ) {
        event.preventDefault();
        event.stopPropagation();
        selectAdjacentFolder(event.key === "ArrowUp" ? -1 : 1);
      }
    };
    const onKeyUp = (event: globalThis.KeyboardEvent) => {
      if (event.code === "AltLeft") {
        resetLeftAlt();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("blur", resetLeftAlt);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("blur", resetLeftAlt);
    };
  }, [folder, snapshot.folders]);

  useEffect(() => {
    let cleanupDebug: (() => void) | undefined;
    let cleanupMailbox: (() => void) | undefined;
    void listen<string>("mailwind-debug", (event) => {
      pushDebug(event.payload);
      if (event.payload.startsWith("Sync complete")) {
        setSyncing(false);
        setStatus("Sync complete");
        void load();
      }
      if (event.payload.startsWith("Sync failed")) {
        setSyncing(false);
        setStatus(event.payload);
      }
      if (event.payload.startsWith("Mail action complete")) {
        setStatus(event.payload.replace("Mail action complete: ", ""));
        void load();
      }
      if (event.payload.startsWith("Mail action failed")) {
        setStatus(event.payload);
      }
    }).then((unlisten) => {
      cleanupDebug = unlisten;
    });
    void listen<MailboxChanged>("mailwind-mailbox-changed", (event) => {
      const count = event.payload.new_messages;
      setStatus(count ? `${count} new email${count === 1 ? "" : "s"}` : "Mailbox updated");
      void load();
      if (count > 0) {
        void notifyNewMail(event.payload);
      }
    }).then((unlisten) => {
      cleanupMailbox = unlisten;
    });
    return () => {
      cleanupDebug?.();
      cleanupMailbox?.();
    };
  }, [folder, accountId, page, query, readFilter, notificationsEnabled]);

  function pushDebug(message: string) {
    const line = `${new Date().toLocaleTimeString()} ${message}`;
    console.info("[mailwind]", message);
    setDebugLines((lines) => [...lines.slice(-79), line]);
  }

  async function load(nextQuery = query, pageOverride = page) {
    pushDebug(
      `load mailbox folder=${folder} account=${accountId ?? "unified"} page=${pageOverride + 1} query=${nextQuery.trim() || "-"}`,
    );
    const data = await invoke<Snapshot>("mailbox_snapshot", {
      filter: {
        folder,
        account_id: accountId,
        query: nextQuery.trim() || null,
        read_filter: readFilter,
        page: pageOverride,
        page_size: pageSize,
      },
    });
    setSnapshot(data);
    const freshSelected = data.messages.find((message) => message.id === selectedId);
    if (freshSelected) {
      setSelectedMessage(freshSelected);
    } else if (selectedId && !selectedMessage) {
      setSelectedId(null);
    }
  }

  async function run<T>(label: string, action: () => Promise<T>) {
    setStatus(label);
    pushDebug(label);
    try {
      const result = await action();
      setStatus("Done");
      pushDebug("Done");
      await load();
      return result;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
      return null;
    }
  }

  async function connectGmail() {
    await run("Waiting for Google OAuth in your browser", async () => {
      await invoke<Account>("connect_gmail", { input: gmailForm });
      setGmailForm({ client_id: "", client_secret: "" });
    });
  }

  async function connectImap() {
    await run("Connecting IMAP account", async () => {
      pushDebug(
        `IMAP form email=${imapForm.email || "-"} imap=${imapForm.imap_host || "-"}:${imapForm.imap_port || "-"} smtp=${imapForm.smtp_host || "-"}:${imapForm.smtp_port || "-"} username=${imapForm.username || "-"}`,
      );
      await invoke<Account>("connect_imap", {
        input: {
          ...imapForm,
          display_name: imapForm.display_name || null,
          imap_port: Number(imapForm.imap_port),
          smtp_port: Number(imapForm.smtp_port),
        },
      });
      setImapForm({
        email: "",
        display_name: "",
        imap_host: "",
        imap_port: "993",
        smtp_host: "",
        smtp_port: "587",
        username: "",
        password: "",
      });
    });
  }

  async function syncAll() {
    setSyncing(true);
    setStatus("Syncing in background");
    pushDebug("Syncing accounts in background");
    try {
      await invoke<void>("sync_all");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setSyncing(false);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  async function search() {
    setPage(0);
    await run("Searching local mail", async () => load(query, 0));
  }

  async function selectMessage(message: MailMessage) {
    setSelectedId(message.id);
    setSelectedMessage(message);
    setShowComposer(false);
    setView("mail");
    if (!message.is_read) {
      await markRead(message, true, false);
    }
  }

  async function selectAndFocusMessage(message: MailMessage) {
    await selectMessage(message);
    window.setTimeout(() => {
      messageRowRefs.current[message.id]?.focus();
      messageRowRefs.current[message.id]?.scrollIntoView({
        block: "nearest",
        inline: "nearest",
      });
    }, 0);
  }

  function selectAdjacentFolder(direction: -1 | 1) {
    const folders = snapshot.folders.map((item) => item.name);
    if (!folders.length) return;
    const currentIndex = Math.max(0, folders.indexOf(folder));
    const nextIndex = Math.min(folders.length - 1, Math.max(0, currentIndex + direction));
    const nextFolder = folders[nextIndex];
    if (nextFolder && nextFolder !== folder) {
      selectFolder(nextFolder);
    }
  }

  function handleMessageListKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (
      leftAltDownRef.current &&
      event.altKey &&
      (event.key === "ArrowUp" || event.key === "ArrowDown")
    ) {
      return;
    }

    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(event.key)) return;

    event.preventDefault();
    event.stopPropagation();

    if (event.key === "ArrowLeft") {
      if (page > 0) {
        setSelectedId(null);
        setSelectedMessage(null);
        setPage((value) => Math.max(0, value - 1));
        window.setTimeout(() => messageListRef.current?.focus(), 0);
      }
      return;
    }

    if (event.key === "ArrowRight") {
      if (page + 1 < pageCount) {
        setSelectedId(null);
        setSelectedMessage(null);
        setPage((value) => Math.min(pageCount - 1, value + 1));
        window.setTimeout(() => messageListRef.current?.focus(), 0);
      }
      return;
    }

    if (!snapshot.messages.length) return;
    const currentIndex = selectedId
      ? snapshot.messages.findIndex((message) => message.id === selectedId)
      : -1;
    const nextIndex =
      event.key === "ArrowDown"
        ? Math.min(snapshot.messages.length - 1, currentIndex < 0 ? 0 : currentIndex + 1)
        : Math.max(0, currentIndex < 0 ? snapshot.messages.length - 1 : currentIndex - 1);
    const nextMessage = snapshot.messages[nextIndex];
    if (nextMessage) {
      void selectAndFocusMessage(nextMessage);
    }
  }

  async function markRead(message: MailMessage, isRead: boolean, reloadAfter = true) {
    setSnapshot((current) => ({
      ...current,
      messages: current.messages.map((item) =>
        item.id === message.id ? { ...item, is_read: isRead } : item,
      ),
    }));
    if (selectedMessage?.id === message.id) {
      setSelectedMessage({ ...selectedMessage, is_read: isRead });
    }
    try {
      await invoke<void>("mark_message_read", {
        input: { message_id: message.id, is_read: isRead },
      });
      if (reloadAfter) await load();
    } catch (error) {
      const errorMessage = error instanceof Error ? error.message : String(error);
      setStatus(errorMessage);
      pushDebug(`Error: ${errorMessage}`);
    }
  }

  async function send() {
    const composerAccountId = composer.account_id ? Number(composer.account_id) : null;
    const account = snapshot.accounts.find((item) => item.id === (composerAccountId ?? accountId ?? selected?.account_id));
    if (!account) {
      setStatus("Add or select an account before sending");
      pushDebug("Send blocked: no account selected");
      return;
    }
    setStatus("Sending in background");
    pushDebug("Send queued in background");
    try {
      await invoke<void>("send_message", {
        input: {
          account_id: account.id,
          to: composer.to,
          subject: composer.subject,
          body: composer.body,
          reply_to_message_id: selected?.id ?? null,
        },
      });
      setComposer({ account_id: "", to: "", subject: "", body: "" });
      setFolder("Sent");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  function openCompose() {
    setSelectedId(null);
    setSelectedMessage(null);
    setComposer({
      account_id: String(accountId ?? snapshot.accounts[0]?.id ?? ""),
      to: "",
      subject: "",
      body: "",
    });
    setShowComposer(true);
    setView("mail");
  }

  function openReply() {
    if (!selected) {
      openCompose();
      return;
    }
    setComposer({
      account_id: String(selected.account_id),
      to: replyAddress(selected),
      subject: replySubject(selected.subject),
      body: "",
    });
    setShowComposer(true);
    window.setTimeout(() => composerBodyRef.current?.focus(), 0);
  }

  async function toggleNotifications(enabled: boolean) {
    if (!enabled) {
      setNotificationsEnabled(false);
      return;
    }
    try {
      const granted = await isPermissionGranted();
      const permission = granted ? "granted" : await requestPermission();
      if (permission === "granted") {
        setNotificationsEnabled(true);
        sendNotification({
          title: "Mailwind notifications enabled",
          body: "New mail will appear here automatically.",
        });
      } else {
        setStatus("Notification permission was not granted");
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  async function notifyNewMail(payload: MailboxChanged) {
    if (!notificationsEnabled) return;
    try {
      const granted = await isPermissionGranted();
      if (!granted) return;
      sendNotification({
        title: payload.new_messages === 1 ? "New email" : `${payload.new_messages} new emails`,
        body: `${payload.unread_inbox} unread in Inbox`,
      });
    } catch (error) {
      pushDebug(`Notification error: ${error instanceof Error ? error.message : String(error)}`);
    }
  }

  async function openEmailLink(rawUrl: string) {
    const url = safeExternalUrl(rawUrl);
    if (!url) {
      setStatus("Blocked unsafe email link");
      pushDebug(`Blocked unsafe email link: ${rawUrl}`);
      return;
    }
    try {
      await openUrl(url);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(`Could not open link: ${message}`);
      pushDebug(`Open link error: ${message}`);
    }
  }

  function handleEmailFrameLoad(event: SyntheticEvent<HTMLIFrameElement>) {
    const doc = event.currentTarget.contentDocument;
    if (!doc) return;
    doc.addEventListener("click", (clickEvent) => {
      if (!(clickEvent.target instanceof Element)) return;
      const anchor = clickEvent.target.closest("a[href]");
      if (!(anchor instanceof HTMLAnchorElement)) return;
      const rawHref = anchor.getAttribute("href") ?? "";
      const url = safeExternalUrl(rawHref) ?? safeExternalUrl(anchor.href);
      if (!url) return;
      clickEvent.preventDefault();
      void openEmailLink(url);
    });
  }

  async function deleteSelected() {
    if (!selected) return;
    const permanent = selected.folder === "Trash";
    const ok = window.confirm(
      permanent
        ? "Permanently delete this email?"
        : "Move this email to Trash?",
    );
    if (!ok) return;
    setStatus(permanent ? "Deleting in background" : "Moving to Trash in background");
    pushDebug(permanent ? "Delete queued in background" : "Move to Trash queued in background");
    try {
      await invoke<void>("delete_message", { input: { message_id: selected.id } });
      setSelectedId(null);
      setSelectedMessage(null);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  async function downloadAttachment(attachment: Attachment) {
    const savePath = await save({
      defaultPath: attachment.filename || "attachment",
      title: `Save ${attachment.filename || "attachment"}`,
    });
    if (!savePath) {
      pushDebug("Download canceled");
      return;
    }
    setStatus(`Downloading ${attachment.filename} in background`);
    pushDebug(`Download queued path=${savePath}`);
    try {
      await invoke<void>("download_attachment", {
        input: { attachment_id: attachment.id, save_path: savePath },
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  function selectFolder(nextFolder: string) {
    setFolder(nextFolder);
    setSelectedId(null);
    setSelectedMessage(null);
    setView("mail");
    setPage(0);
  }

  function selectAccount(nextAccountId: number | null) {
    setAccountId(nextAccountId);
    setSelectedId(null);
    setSelectedMessage(null);
    setView("mail");
    setPage(0);
  }

  async function removeAccount(account: Account) {
    const ok = window.confirm(`Remove ${account.email} and its local messages from Mailwind?`);
    if (!ok) return;
    await run("Removing account", async () => {
      await invoke<void>("remove_account", { input: { account_id: account.id } });
      if (accountId === account.id) setAccountId(null);
      setSelectedId(null);
      setSelectedMessage(null);
    });
  }

  function editAccount(account: Account) {
    if (account.provider !== "imap") return;
    setEditingAccountId(account.id);
    setAccountEditForm({
      email: account.email,
      display_name: account.display_name ?? "",
      imap_host: account.imap_host ?? "",
      imap_port: String(account.imap_port ?? 993),
      smtp_host: account.smtp_host ?? "",
      smtp_port: String(account.smtp_port ?? 587),
      username: account.username ?? account.email,
      password: "",
    });
  }

  async function saveAccountSettings() {
    if (!editingAccountId || !accountEditForm) return;
    await run("Saving IMAP / SMTP settings", async () => {
      await invoke<void>("update_imap_settings", {
        input: {
          account_id: editingAccountId,
          ...accountEditForm,
          display_name: accountEditForm.display_name || null,
          imap_port: Number(accountEditForm.imap_port),
          smtp_port: Number(accountEditForm.smtp_port),
          password: accountEditForm.password || null,
        },
      });
      setEditingAccountId(null);
      setAccountEditForm(null);
    });
  }

  const pageCount = Math.max(1, Math.ceil(snapshot.total / pageSize));
  const currentAccount = snapshot.accounts.find((item) => item.id === (accountId ?? selected?.account_id));
  const sendingAccount = snapshot.accounts.find((item) => item.id === Number(composer.account_id)) ?? currentAccount;
  const inboxFolder = snapshot.folders.find((item) => item.name === "Inbox");
  const pageStart = snapshot.total === 0 ? 0 : page * pageSize + 1;
  const pageEnd = Math.min(snapshot.total, (page + 1) * pageSize);
  const selectedIsHtml = selected ? isHtmlMessage(selected) : false;
  const shellStyle = {
    "--accent": accentColor,
    "--accent-rgb": hexToRgb(accentColor),
  } as CSSProperties;

  return (
    <main
      className={darkMode ? "shell dark" : "shell"}
      style={shellStyle}
    >
      <header className="topbar">
        <div className="topbar-brand">
          <div className="brand">
            <div className="mark"><Send size={19} /></div>
            <h1>Mailwind</h1>
          </div>
        </div>
        <form
          className="top-search"
          onSubmit={(event) => {
            event.preventDefault();
            void search();
          }}
        >
          <Search size={18} />
          <input
            value={query}
            onChange={(event) => setQuery(event.currentTarget.value)}
            placeholder="Search mail, people, files and commands…"
            aria-label="Search mail"
          />
          <button type="submit">Search</button>
        </form>
        <div className="top-actions">
          <button className="command primary-command" onClick={openCompose} aria-label="Compose email">
            <PenLine size={18} /> <span className="command-label">Compose</span> <span className="shortcut">C</span>
          </button>
          <button className="command" disabled={syncing} onClick={syncAll} title={syncing ? "Syncing" : "Sync"} aria-label="Sync mail">
            <RefreshCw size={18} /> <span className="command-label">Sync</span> <span className="shortcut">S</span>
          </button>
          <button className="command" disabled={!selected} onClick={deleteSelected} title={selected?.folder === "Trash" ? "Delete forever" : "Delete"} aria-label={selected?.folder === "Trash" ? "Delete forever" : "Delete email"}>
            <Trash2 size={18} /> <span className="command-label">Delete</span> <span className="shortcut">D</span>
          </button>
          <button className="command" disabled={!selected} onClick={() => selected && void markRead(selected, !selected.is_read)} title={selected?.is_read ? "Mark unread" : "Mark read"} aria-label={selected?.is_read ? "Mark unread" : "Mark read"}>
            {selected?.is_read ? <EyeOff size={18} /> : <CheckCircle2 size={18} />} <span className="command-label">{selected?.is_read ? "Unread" : "Read"}</span> <span className="shortcut">R</span>
          </button>
          <button className="icon-command" onClick={() => setShowDebug((value) => !value)} title="Debug log" aria-label="Toggle debug log">
            <Bug size={18} />
          </button>
        </div>
      </header>

      <aside className="sidebar">
        <section className="sidebar-group">
          <div className="section-title">Folders</div>
          {snapshot.folders.map((item) => (
            <button
              className={item.name === folder ? "nav-item active" : "nav-item"}
              key={item.name}
              onClick={() => selectFolder(item.name)}
            >
              <span>
                {item.name === "Inbox" ? <Inbox size={17} /> : null}
                {item.name === "Sent" ? <Send size={17} /> : null}
                {item.name === "Trash" ? <Trash2 size={17} /> : null}
                {item.name === "Archive" ? <Archive size={17} /> : null}
                {item.name}
              </span>
              <strong>{folderCountLabel(item)}</strong>
            </button>
          ))}
        </section>

        <section className="sidebar-group">
          <div className="section-title">Accounts</div>
          <button
            className={accountId === null ? "nav-item active" : "nav-item"}
            onClick={() => selectAccount(null)}
          >
            <span><Mail size={17} />Unified</span>
            <strong>{snapshot.accounts.length}</strong>
          </button>
          {snapshot.accounts.map((account) => (
            <button
              className={account.id === accountId ? "nav-item active" : "nav-item"}
              key={account.id}
              onClick={() => selectAccount(account.id)}
            >
              <span><i className="account-dot" />{account.email}</span>
              <em>{account.provider === "imap" ? "IMAP" : account.provider}</em>
            </button>
          ))}
        </section>

        <div className="sidebar-bottom">
          <button className="sync-button" disabled={syncing} onClick={syncAll}>
            <RefreshCw size={17} />
            {syncing ? "Syncing…" : "Sync all"}
          </button>
          <button
            className={view === "settings" ? "settings-button active" : "settings-button"}
            onClick={() => {
              setView("settings");
              setSelectedId(null);
              setSelectedMessage(null);
            }}
            title="Settings"
          >
            <Settings size={17} /> Settings
          </button>
        </div>
      </aside>

      {view === "settings" ? (
        <section className="settings-screen">
          <div className="settings-header">
            <div>
              <h2>Settings</h2>
              <p>Theme and account management</p>
            </div>
            <button onClick={() => setView("mail")}>Back to mail</button>
          </div>

          <section className="settings-section">
            <h3>Appearance</h3>
            <label className="toggle-row">
              <span>Dark mode</span>
              <input
                type="checkbox"
                checked={darkMode}
                onChange={(event) => {
                  setThemeOverridden(true);
                  setDarkMode(event.currentTarget.checked);
                }}
              />
            </label>
            <label className="toggle-row">
              <span>Desktop notifications</span>
              <input
                type="checkbox"
                checked={notificationsEnabled}
                onChange={(event) => void toggleNotifications(event.currentTarget.checked)}
              />
            </label>
            <div className="theme-row">
              <div>
                <strong><Palette size={16} /> Accent color</strong>
                <p>Used for selected rows, buttons, and focus states.</p>
              </div>
              <div className="swatches" role="list" aria-label="Theme colors">
                {themeOptions.map((option) => (
                  <button
                    aria-label={option.label}
                    className={accentColor === option.value ? "swatch active" : "swatch"}
                    key={option.value}
                    onClick={() => setAccentColor(option.value)}
                    style={{ background: option.value }}
                    type="button"
                  />
                ))}
              </div>
            </div>
          </section>

          <section className="settings-section">
            <h3>Accounts</h3>
            <div className="account-list">
              {snapshot.accounts.map((account) => (
                <div className="account-card" key={account.id}>
                  <div>
                    <strong>{account.email}</strong>
                    <span>{account.provider}</span>
                  </div>
                  <div className="account-actions">
                    {account.provider === "imap" ? (
                      <button onClick={() => editAccount(account)}>Edit</button>
                    ) : null}
                    <button onClick={() => removeAccount(account)}>Remove</button>
                  </div>
                </div>
              ))}
              {snapshot.accounts.length === 0 ? <p>No accounts connected.</p> : null}
            </div>
            {accountEditForm ? (
              <form
                className="account-edit-form"
                onSubmit={(event) => {
                  event.preventDefault();
                  void saveAccountSettings();
                }}
              >
                <input
                  value={accountEditForm.email}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, email: event.currentTarget.value })}
                  placeholder="Email address"
                  required
                />
                <input
                  value={accountEditForm.display_name}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, display_name: event.currentTarget.value })}
                  placeholder="Display name"
                />
                <input
                  value={accountEditForm.imap_host}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, imap_host: event.currentTarget.value })}
                  placeholder="IMAP host"
                  required
                />
                <input
                  value={accountEditForm.imap_port}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, imap_port: event.currentTarget.value })}
                  placeholder="IMAP port"
                  required
                />
                <input
                  value={accountEditForm.smtp_host}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, smtp_host: event.currentTarget.value })}
                  placeholder="SMTP host"
                  required
                />
                <input
                  value={accountEditForm.smtp_port}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, smtp_port: event.currentTarget.value })}
                  placeholder="SMTP port"
                  required
                />
                <input
                  value={accountEditForm.username}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, username: event.currentTarget.value })}
                  placeholder="Username"
                  required
                />
                <input
                  value={accountEditForm.password}
                  onChange={(event) => setAccountEditForm({ ...accountEditForm, password: event.currentTarget.value })}
                  placeholder="New password or leave blank to keep current"
                  type="password"
                />
                <div className="account-edit-actions">
                  <button type="submit">Save settings</button>
                  <button
                    type="button"
                    onClick={() => {
                      setEditingAccountId(null);
                      setAccountEditForm(null);
                    }}
                  >
                    Cancel
                  </button>
                </div>
              </form>
            ) : null}
          </section>

          <section className="settings-section">
            <details open>
              <summary>Add Gmail / Google Workspace</summary>
              <div className="setup-grid">
                <input
                  value={gmailForm.client_id}
                  onChange={(event) => setGmailForm({ ...gmailForm, client_id: event.currentTarget.value })}
                  placeholder="Google OAuth client ID"
                />
                <input
                  value={gmailForm.client_secret}
                  onChange={(event) => setGmailForm({ ...gmailForm, client_secret: event.currentTarget.value })}
                  placeholder="Google OAuth client secret"
                  type="password"
                />
                <button onClick={connectGmail} type="button">
                  Connect Gmail
                </button>
              </div>
            </details>
          </section>

          <section className="settings-section">
            <details>
              <summary>Add IMAP / SMTP</summary>
              <div className="setup-grid imap-grid">
                <input
                  value={imapForm.email}
                  onChange={(event) => setImapForm({ ...imapForm, email: event.currentTarget.value })}
                  placeholder="Email address"
                />
                <input
                  value={imapForm.display_name}
                  onChange={(event) => setImapForm({ ...imapForm, display_name: event.currentTarget.value })}
                  placeholder="Display name"
                />
                <input
                  value={imapForm.imap_host}
                  onChange={(event) => setImapForm({ ...imapForm, imap_host: event.currentTarget.value })}
                  placeholder="IMAP host"
                />
                <input
                  value={imapForm.imap_port}
                  onChange={(event) => setImapForm({ ...imapForm, imap_port: event.currentTarget.value })}
                  placeholder="IMAP port"
                />
                <input
                  value={imapForm.smtp_host}
                  onChange={(event) => setImapForm({ ...imapForm, smtp_host: event.currentTarget.value })}
                  placeholder="SMTP host"
                />
                <input
                  value={imapForm.smtp_port}
                  onChange={(event) => setImapForm({ ...imapForm, smtp_port: event.currentTarget.value })}
                  placeholder="SMTP port"
                />
                <input
                  value={imapForm.username}
                  onChange={(event) => setImapForm({ ...imapForm, username: event.currentTarget.value })}
                  placeholder="Username"
                />
                <input
                  value={imapForm.password}
                  onChange={(event) => setImapForm({ ...imapForm, password: event.currentTarget.value })}
                  placeholder="Password or app password"
                  type="password"
                />
                <button onClick={connectImap} type="button">
                  Connect IMAP
                </button>
              </div>
            </details>
          </section>
        </section>
      ) : (
      <>
      <section className="list-pane">
        <div className="mail-header">
          <div>
            <h2>{folder}</h2>
            <p>{snapshot.total}</p>
          </div>
          <button onClick={() => void load()} title="Refresh current view">
            <RefreshCw size={16} />
          </button>
          <button onClick={() => setReadFilter(readFilter === "unread" ? "all" : "unread")} title="Unread filter">
            <SlidersHorizontal size={16} />
          </button>
        </div>

        <div className="filterbar" role="tablist" aria-label="Message filters">
          {(["all", "unread", "read"] as ReadFilter[]).map((filter) => (
            <button
              className={readFilter === filter ? "filter active" : "filter"}
              key={filter}
              onClick={() => {
                setReadFilter(filter);
                setSelectedId(null);
                setSelectedMessage(null);
                setPage(0);
              }}
            >
              {filter === "all" ? "All" : filter[0].toUpperCase() + filter.slice(1)}
            </button>
          ))}
        </div>

        <div
          aria-label="Email list"
          aria-activedescendant={selected ? `message-row-${selected.id}` : undefined}
          className="message-list"
          onKeyDown={handleMessageListKeyDown}
          ref={messageListRef}
          role="listbox"
          tabIndex={0}
        >
          {snapshot.messages.map((message) => (
            <button
              aria-selected={message.id === selected?.id}
              className={[
                "message-row",
                message.is_read ? "read" : "unread",
                message.id === selected?.id ? "selected" : "",
              ].join(" ")}
              id={`message-row-${message.id}`}
              key={message.id}
              onClick={() => void selectAndFocusMessage(message)}
              ref={(node) => {
                messageRowRefs.current[message.id] = node;
              }}
              role="option"
              tabIndex={-1}
            >
              <span className="avatar" style={{ backgroundColor: colorForInitials(initialsFor(message.from_addr || message.account_email)), color: "#fff" }}>{initialsFor(message.from_addr || message.account_email)}</span>
              <div className="message-row-content">
                <div className="row-top">
                  <span className="sender-wrap">
                    {!message.is_read ? <i aria-hidden="true" className="unread-dot" /> : null}
                    <span className="sender-text">{shortName(message.from_addr || message.to_addr || message.account_email)}</span>
                  </span>
                  {messageCategory(message) ? <small className="message-tag">{messageCategory(message)}</small> : null}
                </div>
                <strong className={message.is_read ? undefined : "unread-subject"}>{message.subject}</strong>
                <p>{message.snippet || "(no preview)"}</p>
                <div className="row-meta">
                  <small className="row-meta-email">{message.account_email}</small>
                  <div className="row-meta-right">
                    {message.attachments.length ? <small className="attachment-count"><Paperclip size={13} /> {message.attachments.length}</small> : null}
                    <time>{formatMessageTime(message.date_ts)}</time>
                  </div>
                </div>
              </div>
            </button>
          ))}
          {snapshot.messages.length === 0 ? (
            <div className="empty">
              <strong>No local messages yet.</strong>
              <p>Connect an account, then run sync.</p>
            </div>
          ) : null}
        </div>
        <div className="pagination">
          <button disabled={page === 0} onClick={() => setPage((value) => Math.max(0, value - 1))}>
            <ChevronLeft size={16} /> <span>Previous</span>
          </button>
          <span>
            {pageStart}-{pageEnd} of {snapshot.total}
            {inboxFolder?.unread_count ? <em>Unread: {inboxFolder.unread_count}</em> : null}
          </span>
          <button
            disabled={page + 1 >= pageCount}
            onClick={() => setPage((value) => Math.min(pageCount - 1, value + 1))}
          >
            <span>Next</span> <ChevronRight size={16} />
          </button>
        </div>
      </section>

      <section className="reader-pane">
        <div className="reader-toolbar">
          <div className="status">{status}</div>
        </div>
        {showDebug ? (
          <details className="debug-log" open>
            <summary>Debug log</summary>
            <pre>{debugLines.length ? debugLines.join("\n") : "No logs yet."}</pre>
          </details>
        ) : null}
        {selected ? (
          <article className="reader">
            <div className="reader-head">
              <div className="reader-head-top">
                <div className="reader-title-row">
                  <h2>{selected.subject}</h2>
                  <span>{selected.folder}</span>
                </div>
                <div className="reader-actions">
                  <button onClick={() => selected && void markRead(selected, !selected.is_read)} title={selected.is_read ? "Mark unread" : "Mark read"}>
                    {selected.is_read ? <EyeOff size={18} /> : <CheckCircle2 size={18} />}
                  </button>
                  <button disabled={!selected} onClick={deleteSelected} title={selected.folder === "Trash" ? "Delete forever" : "Delete"}>
                    <Trash2 size={18} />
                  </button>
                  <button className="reply-action" onClick={openReply} title="Reply">
                    <PenLine size={18} />
                  </button>
                </div>
              </div>
              <div className="reader-contact-row">
                <div className="reader-contact-left">
                  <span className="avatar large" style={{ backgroundColor: colorForInitials(initialsFor(selected.from_addr || selected.account_email)), color: "#fff" }}>{initialsFor(selected.from_addr || selected.account_email)}</span>
                  <div className="reader-contact-info">
                    <p className="reader-contact-primary">
                      <strong>{shortName(selected.from_addr || selected.account_email)}</strong>
                      {" "}
                      <span>&lt;{extractEmailAddress(selected.from_addr) || selected.account_email}&gt;</span>
                    </p>
                    <p className="reader-contact-secondary">
                      to {selected.to_addr || selected.account_email}
                    </p>
                  </div>
                </div>
                <div className="reader-contact-right">
                  <time className="reader-time-primary">{new Date(selected.date_ts * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}</time>
                  <time className="reader-time-secondary">{new Date(selected.date_ts * 1000).toLocaleDateString([], { month: 'short', day: 'numeric', year: 'numeric' })}</time>
                </div>
              </div>
            </div>
            {selected.attachments.length ? (
              <div className="attachments">
                {selected.attachments.map((attachment) => (
                  <button key={attachment.id} onClick={() => downloadAttachment(attachment)}>
                    <Download size={16} />
                    <strong>{attachment.filename}</strong>
                    <span>{formatBytes(attachment.size)}</span>
                  </button>
                ))}
              </div>
            ) : null}
            {selectedIsHtml ? (
              <iframe
                className="html-body"
                sandbox="allow-same-origin"
                title="Email HTML body"
                onLoad={handleEmailFrameLoad}
                srcDoc={sanitizeHtml(selected.body)}
              />
            ) : (
              <pre>{selected.body || selected.snippet}</pre>
            )}
          </article>
        ) : (
          <article className="reader empty-reader">
            <h2>Select a message</h2>
            <p>Choose an email from the list to read, reply, mark read or unread, delete, and download attachments.</p>
          </article>
        )}

        <details
          className="composer-panel"
          open={showComposer}
          onToggle={(event) => setShowComposer(event.currentTarget.open)}
        >
          <summary>
            <span>{selected ? "Quick reply" : "Compose"}</span>
            {selected ? <em>from {sendingAccount?.email ?? selected.account_email}</em> : null}
          </summary>
          <form
            className="composer"
            onSubmit={(event) => {
              event.preventDefault();
              void send();
            }}
          >
            <div className="composer-grid">
              <select
                value={composer.account_id}
                onChange={(event) => setComposer({ ...composer, account_id: event.currentTarget.value })}
                required
              >
                <option value="">Send as</option>
                {snapshot.accounts.map((account) => (
                  <option key={account.id} value={account.id}>
                    {account.email}
                  </option>
                ))}
              </select>
              <input
                value={composer.to}
                onChange={(event) => setComposer({ ...composer, to: event.currentTarget.value })}
                placeholder="To"
                required
              />
              <input
                value={composer.subject}
                onChange={(event) => setComposer({ ...composer, subject: event.currentTarget.value })}
                placeholder="Subject"
                required
              />
            </div>
            <textarea
              ref={composerBodyRef}
              value={composer.body}
              onChange={(event) => setComposer({ ...composer, body: event.currentTarget.value })}
              placeholder={selected ? `Reply from ${sendingAccount?.email ?? "selected account"}` : "Write a new message"}
              required
            />
            <button className="primary" type="submit">
              Send
            </button>
          </form>
        </details>

      </section>
      </>
      )}
    </main>
  );
}

function formatMessageTime(timestamp: number) {
  const date = new Date(timestamp * 1000);
  const now = new Date();
  const isToday =
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate();
  if (isToday) {
    return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  return date.toLocaleDateString([], { month: "short", day: "numeric" });
}

function formatBytes(size: number) {
  if (!size) return "unknown size";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${Math.round(size / 1024)} KB`;
  return `${(size / 1024 / 1024).toFixed(1)} MB`;
}

function shortName(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return "Unknown sender";
  const match = trimmed.match(/^"?([^"<]+)"?\s*</);
  const name = (match?.[1] || trimmed.split("@")[0] || trimmed).trim();
  return name || trimmed;
}

function replyAddress(message: MailMessage) {
  return extractEmailAddress(message.from_addr) || message.from_addr || message.account_email;
}

function folderCountLabel(folder: Folder) {
  if (folder.name === "Inbox" && folder.unread_count > 0) {
    return `${folder.unread_count}/${folder.count}`;
  }
  return String(folder.count);
}

function messageCategory(message: MailMessage) {
  if (message.folder === "Inbox") return "Inbox";
  if (message.folder === "Sent") return "Sent";
  if (message.folder === "Trash") return "Trash";
  if (message.folder === "Archive") return "Archive";
  return "";
}

function replySubject(subject: string) {
  return /^re:/i.test(subject.trim()) ? subject : `Re: ${subject || "(no subject)"}`;
}

function extractEmailAddress(value: string) {
  const bracketed = value.match(/<([^>]+)>/);
  if (bracketed?.[1]) return bracketed[1].trim();
  const email = value.match(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/i);
  return email?.[0] ?? "";
}

function initialsFor(value: string) {
  const name = shortName(value)
    .replace(/[^a-zA-Z0-9\s]/g, " ")
    .trim();
  if (!name) return "MW";
  const parts = name.split(/\s+/).filter(Boolean);
  const initials = parts.length > 1
    ? `${parts[0][0]}${parts[1][0]}`
    : name.slice(0, 2);
  return initials.toUpperCase();
}

function colorForInitials(initials: string) {
  const colors = [
    "#3b82f6", // blue
    "#ef4444", // red
    "#10b981", // green
    "#f59e0b", // yellow/amber
    "#8b5cf6", // violet
    "#ec4899", // pink
    "#06b6d4", // cyan
    "#f97316", // orange
    "#6366f1", // indigo
    "#14b8a6", // teal
  ];
  let hash = 0;
  for (let i = 0; i < initials.length; i++) {
    hash = initials.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

function sanitizeHtml(value: string) {
  const parser = new DOMParser();
  const doc = parser.parseFromString(value, "text/html");
  doc.querySelectorAll("script, object, embed, iframe, form").forEach((node) => node.remove());
  doc.querySelectorAll("*").forEach((node) => {
    for (const attr of Array.from(node.attributes)) {
      const name = attr.name.toLowerCase();
      const attrValue = attr.value.trim().toLowerCase();
      if (name.startsWith("on") || attrValue.startsWith("javascript:")) {
        node.removeAttribute(attr.name);
      }
    }
  });
  doc.querySelectorAll("a").forEach((node) => {
    const href = node.getAttribute("href") ?? "";
    const safeHref = safeExternalUrl(href);
    if (safeHref) {
      node.setAttribute("href", safeHref);
      node.setAttribute("target", "_blank");
      node.setAttribute("rel", "noreferrer noopener");
    } else {
      node.removeAttribute("href");
    }
  });
  return `<!doctype html><html><head><base target="_blank"><style>html{background:#fff}body{box-sizing:border-box;margin:0;padding:34px 44px;color:#1f2937;font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}img{max-width:100%;height:auto}table{max-width:100%}@media(max-width:700px){body{padding:22px}}</style></head><body>${doc.body.innerHTML}</body></html>`;
}

function safeExternalUrl(value: string) {
  let candidate = value.trim().replace(/[\u0000-\u001f\u007f\s]+/g, "");
  if (!candidate || candidate.startsWith("#")) return "";
  if (/^www\./i.test(candidate)) {
    candidate = `https://${candidate}`;
  }
  try {
    const url = new URL(candidate);
    if (url.protocol === "http:" || url.protocol === "https:" || url.protocol === "mailto:") {
      return url.toString();
    }
  } catch {
    return "";
  }
  return "";
}

function isEditableTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false;
  return Boolean(target.closest("input, textarea, select, [contenteditable='true']"));
}

function isHtmlMessage(message: MailMessage) {
  if (message.body_mime.toLowerCase().includes("html")) return true;
  const body = message.body.trim().slice(0, 500).toLowerCase();
  return (
    body.startsWith("<!doctype html") ||
    body.startsWith("<html") ||
    /<(body|head|table|div|p|span|style|meta|title)\b/i.test(body)
  );
}

export default App;
