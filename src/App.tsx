import {
  type ClipboardEvent,
  type CSSProperties,
  type KeyboardEvent,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import {
  Archive,
  Bold,
  Bug,
  CheckCircle2,
  ChevronLeft,
  ChevronRight,
  Download,
  EyeOff,
  Heading2,
  Italic,
  Inbox,
  Link,
  List,
  ListOrdered,
  Mail,
  Palette,
  Paperclip,
  PenLine,
  RefreshCw,
  RemoveFormatting,
  Search,
  Send,
  Settings,
  SlidersHorizontal,
  TextQuote,
  Trash2,
  Type,
  Underline,
  Unlink,
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
  message_header_id: string;
  in_reply_to: string;
  references_header: string;
  normalized_subject: string;
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
  thread_count: number;
  thread_unread_count: number;
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

type DownloadedAttachment = {
  path: string;
};

type ReadFilter = "all" | "read" | "unread";
type AppView = "mail" | "settings";

type ComposerState = {
  account_id: string;
  to: string;
  subject: string;
  body: string;
  body_mime: string;
  reply_to_message_id: number | null;
};

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

const emptyComposer: ComposerState = {
  account_id: "",
  to: "",
  subject: "",
  body: "",
  body_mime: "text/html",
  reply_to_message_id: null,
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

function applyNativeTheme(dark: boolean) {
  const theme = dark ? "dark" : "light";
  document.documentElement.style.colorScheme = theme;
  document.documentElement.classList.toggle("dark", dark);
  void getCurrentWindow().setTheme(theme).catch(() => {
    // Browser-only Vite previews do not have the Tauri window/app APIs.
  });
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
  const [composer, setComposer] = useState<ComposerState>(emptyComposer);
  const [threadMessages, setThreadMessages] = useState<MailMessage[]>([]);
  const [notificationsEnabled, setNotificationsEnabled] = useState(
    () => window.localStorage.getItem("mailwind-notifications") === "enabled",
  );
  const [editingAccountId, setEditingAccountId] = useState<number | null>(null);
  const [accountEditForm, setAccountEditForm] = useState<ImapSettingsForm | null>(null);
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const messageRowRefs = useRef<Record<number, HTMLButtonElement | null>>({});
  const openEmailLinkRef = useRef<(rawUrl: string) => void>(() => undefined);
  const leftAltDownRef = useRef(false);
  const composerBodyRef = useRef<HTMLDivElement | null>(null);
  const threadRequestRef = useRef(0);
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
  const composerPlainText = useMemo(
    () => htmlToPlainText(composer.body),
    [composer.body],
  );

  useEffect(() => {
    void load();
  }, [folder, accountId, page, readFilter]);

  useEffect(() => {
    const editor = composerBodyRef.current;
    if (!editor || editor.innerHTML === composer.body) return;
    editor.innerHTML = composer.body;
  }, [composer.body, showComposer]);

  useEffect(() => {
    if (themeOverridden) {
      window.localStorage.setItem(themeOverrideStorageKey, "true");
      window.localStorage.setItem(themeStorageKey, darkMode ? "dark" : "light");
    }
    applyNativeTheme(darkMode);
  }, [darkMode, themeOverridden]);

  useEffect(() => {
    if (!window.matchMedia) return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (event: MediaQueryListEvent) => {
      if (!themeOverridden) {
        setDarkMode(event.matches);
        applyNativeTheme(event.matches);
      }
    };
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [themeOverridden]);

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
      if (isSettingsShortcut(event)) {
        event.preventDefault();
        event.stopPropagation();
        openSettings();
        return;
      }
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
    const onEmailLinkMessage = (event: MessageEvent) => {
      const href = emailLinkMessageHref(event.data);
      if (!href) return;
      openEmailLinkRef.current(href);
    };
    window.addEventListener("message", onEmailLinkMessage);
    return () => window.removeEventListener("message", onEmailLinkMessage);
  }, []);

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

  function syncComposerBody() {
    const html = composerBodyRef.current?.innerHTML ?? "";
    setComposer((current) => ({
      ...current,
      body: normalizeComposerHtml(html),
      body_mime: "text/html",
    }));
  }

  function runEditorCommand(command: string, value?: string) {
    const editor = composerBodyRef.current;
    if (!editor) return;
    editor.focus();
    document.execCommand(command, false, value);
    syncComposerBody();
  }

  function formatComposer(command: string, value?: string) {
    runEditorCommand(command, value);
  }

  function addComposerLink() {
    const href = window.prompt("URL");
    const safeHref = href ? safeExternalUrl(href) : "";
    if (!safeHref) {
      if (href) setStatus("Blocked unsafe link");
      return;
    }
    runEditorCommand("createLink", safeHref);
  }

  function pasteIntoComposer(event: ClipboardEvent<HTMLDivElement>) {
    event.preventDefault();
    const html = event.clipboardData.getData("text/html");
    const text = event.clipboardData.getData("text/plain");
    const safeHtml = html
      ? sanitizeComposerHtml(html)
      : plainTextToHtml(text);
    document.execCommand("insertHTML", false, safeHtml);
    syncComposerBody();
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
      void loadThreadMessages(freshSelected.id);
    } else if (selectedId && !selectedMessage) {
      setSelectedId(null);
      setThreadMessages([]);
    }
  }

  async function loadThreadMessages(messageId: number) {
    const requestId = threadRequestRef.current + 1;
    threadRequestRef.current = requestId;
    try {
      const messages = await invoke<MailMessage[]>("list_thread_messages", {
        input: { message_id: messageId, folder },
      });
      if (threadRequestRef.current === requestId) {
        setThreadMessages(messages);
      }
    } catch (error) {
      if (threadRequestRef.current !== requestId) return;
      setThreadMessages([]);
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
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
    setThreadMessages([]);
    await loadThreadMessages(message.id);
    setShowComposer(false);
    setView("mail");
    if (isThreadUnread(message)) {
      await markThreadRead(message, true, false);
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

  function openSettings() {
    setView("settings");
    clearCurrentMessage();
  }

  function clearCurrentMessage() {
    threadRequestRef.current += 1;
    setSelectedId(null);
    setSelectedMessage(null);
    setThreadMessages([]);
  }

  function changeReadFilter(nextFilter: ReadFilter) {
    setReadFilter(nextFilter);
    clearCurrentMessage();
    setPage(0);
    setView("mail");
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
        clearCurrentMessage();
        setPage((value) => Math.max(0, value - 1));
        window.setTimeout(() => messageListRef.current?.focus(), 0);
      }
      return;
    }

    if (event.key === "ArrowRight") {
      if (page + 1 < pageCount) {
        clearCurrentMessage();
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

  async function markThreadRead(message: MailMessage, isRead: boolean, reloadAfter = true) {
    setSnapshot((current) => ({
      ...current,
      messages: current.messages.map((item) =>
        item.account_id === message.account_id && item.thread_id === message.thread_id
          ? withThreadReadState(item, isRead)
          : item,
      ),
    }));
    if (selectedMessage?.account_id === message.account_id && selectedMessage.thread_id === message.thread_id) {
      setSelectedMessage(withThreadReadState(selectedMessage, isRead));
    }
    setThreadMessages((items) =>
      items.map((item) =>
        item.account_id === message.account_id && item.thread_id === message.thread_id
          ? withThreadReadState(item, isRead)
          : item,
      ),
    );
    try {
      await invoke<void>("mark_thread_read", {
        input: { message_id: message.id, is_read: isRead, folder },
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
    const htmlBody = sanitizeComposerHtml(composer.body);
    const plainBody = htmlToPlainText(htmlBody);
    if (!plainBody.trim()) {
      setStatus("Write a message before sending");
      pushDebug("Send blocked: empty message body");
      composerBodyRef.current?.focus();
      return;
    }
    setStatus("Sending");
    pushDebug("Sending message");
    try {
      await invoke<MailMessage>("send_message", {
        input: {
          account_id: account.id,
          to: composer.to,
          subject: composer.subject,
          body: htmlBody,
          body_mime: composer.body_mime,
          reply_to_message_id: composer.reply_to_message_id,
        },
      });
      setComposer(emptyComposer);
      setSelectedId(null);
      setSelectedMessage(null);
      setThreadMessages([]);
      setFolder("Sent");
      setStatus("Sent");
      pushDebug("Sent");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  function openCompose() {
    setSelectedId(null);
    setSelectedMessage(null);
    setThreadMessages([]);
    setComposer({
      account_id: String(accountId ?? snapshot.accounts[0]?.id ?? ""),
      to: "",
      subject: "",
      body: "",
      body_mime: "text/html",
      reply_to_message_id: null,
    });
    setShowComposer(true);
    setView("mail");
  }

  function openReply() {
    const replyTarget = latestThreadMessage(threadMessages, selected);
    if (!replyTarget) {
      openCompose();
      return;
    }
    setComposer({
      account_id: String(replyTarget.account_id),
      to: replyAddress(replyTarget),
      subject: replySubject(replyTarget.subject),
      body: "",
      body_mime: "text/html",
      reply_to_message_id: replyTarget.id,
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

  openEmailLinkRef.current = (rawUrl: string) => {
    void openEmailLink(rawUrl);
  };


  async function archiveSelected() {
    if (!selected || selected.folder === "Archive" || selected.folder === "Trash") return;
    setStatus("Archiving");
    pushDebug("Archiving message");
    try {
      await invoke<void>("archive_message", { input: { message_id: selected.id } });
      clearCurrentMessage();
      setStatus("Archived");
      pushDebug("Archived");
      await load();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
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
    setStatus(permanent ? "Deleting" : "Moving to Trash");
    pushDebug(permanent ? "Deleting message" : "Moving message to Trash");
    try {
      await invoke<void>("delete_message", { input: { message_id: selected.id } });
      clearCurrentMessage();
      setStatus(permanent ? "Deleted" : "Moved to Trash");
      pushDebug(permanent ? "Deleted" : "Moved to Trash");
      await load();
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
    setStatus(`Downloading ${attachment.filename}`);
    pushDebug(`Downloading attachment path=${savePath}`);
    try {
      const downloaded = await invoke<DownloadedAttachment>("download_attachment", {
        input: { attachment_id: attachment.id, save_path: savePath },
      });
      setStatus("Downloaded attachment");
      pushDebug(`Downloaded attachment path=${downloaded.path}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
  }

  function selectFolder(nextFolder: string) {
    setFolder(nextFolder);
    clearCurrentMessage();
    setView("mail");
    setPage(0);
  }

  function selectAccount(nextAccountId: number | null) {
    setAccountId(nextAccountId);
    clearCurrentMessage();
    setView("mail");
    setPage(0);
  }

  async function removeAccount(account: Account) {
    const ok = window.confirm(`Remove ${account.email} and its local messages from Mailwind?`);
    if (!ok) return;
    await run("Removing account", async () => {
      await invoke<void>("remove_account", { input: { account_id: account.id } });
      if (accountId === account.id) setAccountId(null);
      clearCurrentMessage();
    });
  }

  async function resetLocalDatabase() {
    const ok = window.confirm(
      "Delete and recreate the complete local Mailwind database? This removes connected accounts, synced mail, attachments, settings, and local credentials from this app.",
    );
    if (!ok) return;

    setStatus("Resetting local database");
    pushDebug("Resetting local database");
    try {
      await invoke<void>("reset_database");
      const data = await invoke<Snapshot>("mailbox_snapshot", {
        filter: {
          folder: "Inbox",
          account_id: null,
          query: null,
          read_filter: "all",
          page: 0,
          page_size: pageSize,
        },
      });
      setFolder("Inbox");
      setAccountId(null);
      setQuery("");
      setReadFilter("all");
      setPage(0);
      clearCurrentMessage();
      setSnapshot(data);
      setComposer(emptyComposer);
      setEditingAccountId(null);
      setAccountEditForm(null);
      setStatus("Database reset");
      pushDebug("Database reset");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStatus(message);
      pushDebug(`Error: ${message}`);
    }
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
  const displayedThread = selected
    ? threadMessages.length
      ? threadMessages
      : [selected]
    : [];
  const selectedThreadUnread = selected ? isThreadUnread(selected) : false;
  const canArchiveSelected = Boolean(selected && selected.folder !== "Archive" && selected.folder !== "Trash");
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
          <button className="command" disabled={!canArchiveSelected} onClick={archiveSelected} title="Archive" aria-label="Archive email">
            <Archive size={18} /> <span className="command-label">Archive</span>
          </button>
          <button className="command" disabled={!selected} onClick={deleteSelected} title={selected?.folder === "Trash" ? "Delete forever" : "Delete"} aria-label={selected?.folder === "Trash" ? "Delete forever" : "Delete email"}>
            <Trash2 size={18} /> <span className="command-label">Delete</span> <span className="shortcut">D</span>
          </button>
          <button className="command" disabled={!selected} onClick={() => selected && void markThreadRead(selected, selectedThreadUnread)} title={selectedThreadUnread ? "Mark read" : "Mark unread"} aria-label={selectedThreadUnread ? "Mark thread read" : "Mark thread unread"}>
            {selectedThreadUnread ? <CheckCircle2 size={18} /> : <EyeOff size={18} />} <span className="command-label">{selectedThreadUnread ? "Read" : "Unread"}</span> <span className="shortcut">R</span>
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
            onClick={openSettings}
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

          <section className="settings-section danger-section">
            <h3>Local database</h3>
            <p>
              Delete all local accounts, synced messages, attachments, credentials, and settings,
              then recreate a fresh empty database.
            </p>
            <button className="danger-button" onClick={() => void resetLocalDatabase()} type="button">
              Delete and recreate database
            </button>
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
          <button
            className={readFilter === "unread" ? "active" : undefined}
            onClick={() => changeReadFilter(readFilter === "unread" ? "all" : "unread")}
            title="Unread filter"
          >
            <SlidersHorizontal size={16} />
          </button>
        </div>

        <div className="filterbar" role="tablist" aria-label="Message filters">
          {(["all", "unread", "read"] as ReadFilter[]).map((filter) => (
            <button
              className={readFilter === filter ? "filter active" : "filter"}
              key={filter}
              onClick={() => changeReadFilter(filter)}
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
                isThreadUnread(message) ? "unread" : "read",
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
                    {isThreadUnread(message) ? <i aria-hidden="true" className="unread-dot" /> : null}
                    <span className="sender-text">{shortName(message.from_addr || message.to_addr || message.account_email)}</span>
                  </span>
                  {message.thread_count > 1 ? <small className="thread-count">{message.thread_count}</small> : null}
                  {messageCategory(message) ? <small className="message-tag">{messageCategory(message)}</small> : null}
                </div>
                <strong className={isThreadUnread(message) ? "unread-subject" : undefined}>{message.subject}</strong>
                <p>{message.snippet || "(no preview)"}</p>
                <div className="row-meta">
                  <small className="row-meta-email">{message.account_email}</small>
                  <div className="row-meta-right">
                    {message.thread_unread_count > 0 ? <small className="thread-unread-count">{message.thread_unread_count} unread</small> : null}
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
                  {selected.thread_count > 1 ? <em>{selected.thread_count} messages</em> : null}
                </div>
                <div className="reader-actions">
                  <button onClick={() => selected && void markThreadRead(selected, selectedThreadUnread)} title={selectedThreadUnread ? "Mark read" : "Mark unread"}>
                    {selectedThreadUnread ? <CheckCircle2 size={18} /> : <EyeOff size={18} />}
                  </button>
                  <button disabled={!canArchiveSelected} onClick={archiveSelected} title="Archive">
                    <Archive size={18} />
                  </button>
                  <button disabled={!selected} onClick={deleteSelected} title={selected.folder === "Trash" ? "Delete forever" : "Delete"}>
                    <Trash2 size={18} />
                  </button>
                  <button className="reply-action" onClick={openReply} title="Reply">
                    <PenLine size={18} />
                  </button>
                </div>
              </div>
            </div>
            <div className="thread-stack">
              {displayedThread.map((message, index) => (
                <details
                  className={[
                    "thread-message",
                    message.is_read ? "read" : "unread",
                    index === displayedThread.length - 1 ? "latest" : "",
                  ].join(" ")}
                  key={message.id}
                  open={index === displayedThread.length - 1 || displayedThread.length <= 2}
                >
                  <summary className="thread-message-head">
                    <div className="reader-contact-left">
                      <span className="avatar large" style={{ backgroundColor: colorForInitials(initialsFor(message.from_addr || message.account_email)), color: "#fff" }}>{initialsFor(message.from_addr || message.account_email)}</span>
                      <div className="reader-contact-info">
                        <p className="reader-contact-primary">
                          <strong>{shortName(message.from_addr || message.account_email)}</strong>
                          {" "}
                          <span>&lt;{extractEmailAddress(message.from_addr) || message.account_email}&gt;</span>
                        </p>
                        <p className="reader-contact-secondary">
                          to {message.to_addr || message.account_email}
                        </p>
                      </div>
                    </div>
                    <div className="thread-message-meta">
                      <time>{formatFullDate(message.date_ts)}</time>
                      {message.folder !== selected.folder ? <span>{message.folder}</span> : null}
                    </div>
                  </summary>
                  <div className="thread-message-body">
                    {message.attachments.length ? (
                      <div className="attachments thread-attachments">
                        {message.attachments.map((attachment) => (
                          <button key={attachment.id} onClick={() => downloadAttachment(attachment)}>
                            <Download size={16} />
                            <strong>{attachment.filename}</strong>
                            <span>{formatBytes(attachment.size)}</span>
                          </button>
                        ))}
                      </div>
                    ) : null}
                    {isHtmlMessage(message) ? (
                      <iframe
                        className="html-body thread-html-body"
                        referrerPolicy="no-referrer"
                        sandbox="allow-popups allow-same-origin"
                        title={`Email HTML body from ${shortName(message.from_addr || message.account_email)}`}
                        srcDoc={sanitizeHtml(message.body)}
                        onLoad={(event) => resizeEmailFrame(event.currentTarget)}
                      />
                    ) : (
                      <pre>{message.body || message.snippet}</pre>
                    )}
                  </div>
                </details>
              ))}
            </div>
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
            <div className="composer-editor-shell">
              <div className="composer-toolbar" aria-label="Message formatting">
                <button
                  aria-label="Bold"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("bold");
                  }}
                  title="Bold"
                  type="button"
                >
                  <Bold size={16} />
                </button>
                <button
                  aria-label="Italic"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("italic");
                  }}
                  title="Italic"
                  type="button"
                >
                  <Italic size={16} />
                </button>
                <button
                  aria-label="Underline"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("underline");
                  }}
                  title="Underline"
                  type="button"
                >
                  <Underline size={16} />
                </button>
                <span aria-hidden="true" className="toolbar-divider" />
                <button
                  aria-label="Heading"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("formatBlock", "h2");
                  }}
                  title="Heading"
                  type="button"
                >
                  <Heading2 size={16} />
                </button>
                <button
                  aria-label="Paragraph"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("formatBlock", "p");
                  }}
                  title="Paragraph"
                  type="button"
                >
                  <Type size={16} />
                </button>
                <button
                  aria-label="Quote"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("formatBlock", "blockquote");
                  }}
                  title="Quote"
                  type="button"
                >
                  <TextQuote size={16} />
                </button>
                <span aria-hidden="true" className="toolbar-divider" />
                <button
                  aria-label="Bulleted list"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("insertUnorderedList");
                  }}
                  title="Bulleted list"
                  type="button"
                >
                  <List size={16} />
                </button>
                <button
                  aria-label="Numbered list"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("insertOrderedList");
                  }}
                  title="Numbered list"
                  type="button"
                >
                  <ListOrdered size={16} />
                </button>
                <span aria-hidden="true" className="toolbar-divider" />
                <button
                  aria-label="Add link"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    addComposerLink();
                  }}
                  title="Add link"
                  type="button"
                >
                  <Link size={16} />
                </button>
                <button
                  aria-label="Remove link"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("unlink");
                  }}
                  title="Remove link"
                  type="button"
                >
                  <Unlink size={16} />
                </button>
                <button
                  aria-label="Clear formatting"
                  onMouseDown={(event) => {
                    event.preventDefault();
                    formatComposer("removeFormat");
                  }}
                  title="Clear formatting"
                  type="button"
                >
                  <RemoveFormatting size={16} />
                </button>
              </div>
              <div
                aria-label="Message body"
                className="composer-editor"
                contentEditable
                data-placeholder={selected ? `Reply from ${sendingAccount?.email ?? "selected account"}` : "Write a new message"}
                onInput={syncComposerBody}
                onPaste={pasteIntoComposer}
                ref={composerBodyRef}
                role="textbox"
                spellCheck
                suppressContentEditableWarning
              />
            </div>
            <div className="composer-actions">
              <span>{composerPlainText.trim().length ? `${composerPlainText.trim().length} chars` : ""}</span>
              <button className="primary" disabled={!composerPlainText.trim()} type="submit">
                Send
              </button>
            </div>
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

function formatFullDate(timestamp: number) {
  return new Date(timestamp * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    year: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function isThreadUnread(message: MailMessage) {
  return message.thread_unread_count > 0 || !message.is_read;
}

function withThreadReadState(message: MailMessage, isRead: boolean): MailMessage {
  return {
    ...message,
    is_read: isRead,
    thread_unread_count: isRead ? 0 : Math.max(1, message.thread_count || 1),
  };
}

function latestThreadMessage(messages: MailMessage[], fallback: MailMessage | null) {
  if (!messages.length) return fallback;
  return messages.reduce((latest, message) =>
    message.date_ts >= latest.date_ts ? message : latest,
  );
}

function resizeEmailFrame(frame: HTMLIFrameElement) {
  const applyHeight = () => {
    try {
      const doc = frame.contentDocument;
      const height = Math.max(
        doc?.documentElement.scrollHeight ?? 0,
        doc?.body.scrollHeight ?? 0,
      );
      if (height > 0) {
        frame.style.height = `${Math.min(Math.max(height + 2, 180), 6000)}px`;
      }
    } catch {
      frame.style.height = "560px";
    }
  };
  applyHeight();
  window.setTimeout(applyHeight, 100);
  window.setTimeout(applyHeight, 600);
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
  doc.querySelectorAll("script, object, embed, iframe, form, link, meta").forEach((node) => node.remove());
  doc.querySelectorAll("*").forEach((node) => {
    for (const attr of Array.from(node.attributes)) {
      const name = attr.name.toLowerCase();
      const rawValue = attr.value.trim();
      const attrValue = rawValue.toLowerCase();
      if (name.startsWith("on") || attrValue.startsWith("javascript:")) {
        node.removeAttribute(attr.name);
        continue;
      }
      if (["srcset", "ping", "action", "formaction"].includes(name)) {
        node.removeAttribute(attr.name);
        continue;
      }
      if (["src", "poster", "background"].includes(name)) {
        const safeUrl = safeEmbeddedUrl(rawValue);
        if (safeUrl) {
          node.setAttribute(attr.name, safeUrl);
        } else {
          node.removeAttribute(attr.name);
          if (node.tagName.toLowerCase() === "img" && !node.getAttribute("alt")) {
            node.setAttribute("alt", "Remote image blocked");
          }
        }
        continue;
      }
      if (name === "style" && (/url\s*\(/i.test(rawValue) || /@import/i.test(rawValue))) {
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
  return `<!doctype html><html><head><base target="_blank"><style>html{background:#fff;overflow:hidden}body{box-sizing:border-box;max-width:100%;margin:0;padding:18px 34px 30px;color:#1f2937;font:14px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;overflow-wrap:anywhere;word-break:normal}img,video{max-width:100%;height:auto}table{max-width:100%;border-collapse:collapse}td,th{max-width:100%;overflow-wrap:anywhere}pre{white-space:pre-wrap;overflow-wrap:anywhere}a{overflow-wrap:anywhere}blockquote{margin-left:0;padding-left:14px;border-left:3px solid #e5e7eb;color:#4b5563}@media(max-width:700px){body{padding:16px 18px 24px}}</style></head><body>${doc.body.innerHTML}</body></html>`;
}

function normalizeComposerHtml(value: string) {
  const trimmed = value.trim();
  if (!trimmed || trimmed === "<br>" || trimmed === "<div><br></div>" || trimmed === "<p><br></p>") {
    return "";
  }
  return trimmed;
}

function sanitizeComposerHtml(value: string) {
  const parser = new DOMParser();
  const doc = parser.parseFromString(value, "text/html");
  doc.querySelectorAll("script, object, embed, iframe, form, link, meta, style").forEach((node) => node.remove());
  doc.querySelectorAll("*").forEach((node) => {
    const tag = node.tagName.toLowerCase();
    if (!allowedComposerTags.has(tag)) {
      node.replaceWith(...Array.from(node.childNodes));
      return;
    }
    for (const attr of Array.from(node.attributes)) {
      const name = attr.name.toLowerCase();
      const rawValue = attr.value.trim();
      if (tag === "a" && name === "href") {
        const safeHref = safeExternalUrl(rawValue);
        if (safeHref) {
          node.setAttribute("href", safeHref);
          node.setAttribute("target", "_blank");
          node.setAttribute("rel", "noreferrer noopener");
        } else {
          node.removeAttribute(attr.name);
        }
        continue;
      }
      if (name === "style") {
        const safeStyle = safeComposerStyle(rawValue);
        if (safeStyle) {
          node.setAttribute("style", safeStyle);
        } else {
          node.removeAttribute(attr.name);
        }
        continue;
      }
      node.removeAttribute(attr.name);
    }
  });
  return normalizeComposerHtml(doc.body.innerHTML);
}

const allowedComposerTags = new Set([
  "a",
  "b",
  "blockquote",
  "br",
  "div",
  "em",
  "h1",
  "h2",
  "h3",
  "i",
  "li",
  "ol",
  "p",
  "span",
  "strong",
  "u",
  "ul",
]);

function safeComposerStyle(value: string) {
  const allowed = value
    .split(";")
    .map((part) => part.trim())
    .filter((part) => /^text-align\s*:\s*(left|right|center)$/i.test(part));
  return allowed.join("; ");
}

function plainTextToHtml(value: string) {
  return value
    .split(/\r?\n/)
    .map((line) => line.trim() ? `<p>${escapeHtml(line)}</p>` : "<p><br></p>")
    .join("");
}

function htmlToPlainText(value: string) {
  const parser = new DOMParser();
  const doc = parser.parseFromString(value, "text/html");
  return (doc.body.textContent ?? "").replace(/\u00a0/g, " ").trim();
}

function escapeHtml(value: string) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function safeEmbeddedUrl(value: string) {
  const candidate = value.trim();
  if (/^data:image\//i.test(candidate) || /^cid:/i.test(candidate)) {
    return candidate;
  }
  return "";
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

function emailLinkMessageHref(value: unknown) {
  if (!value || typeof value !== "object") return "";
  const payload = value as { type?: unknown; href?: unknown };
  if (payload.type !== "mailwind-open-email-link" || typeof payload.href !== "string") {
    return "";
  }
  return payload.href;
}

function isSettingsShortcut(event: globalThis.KeyboardEvent) {
  const modifier = event.metaKey || event.ctrlKey;
  return modifier && !event.altKey && !event.shiftKey && (event.key === "," || event.code === "Comma");
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
