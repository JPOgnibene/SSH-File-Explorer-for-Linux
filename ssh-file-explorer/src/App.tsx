import { useState, useCallback, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter } from "@codemirror/view";
import { EditorState } from "@codemirror/state";
import { defaultKeymap, indentWithTab, history, historyKeymap } from "@codemirror/commands";
import { oneDark } from "@codemirror/theme-one-dark";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { json } from "@codemirror/lang-json";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { xml } from "@codemirror/lang-xml";
import { markdown } from "@codemirror/lang-markdown";
import "./App.css";

interface FileEntry {
  name: string;
  is_dir: boolean;
  size: number;
  modified: string;
  permissions: string;
}

interface SavedConnection {
  id: string;
  label: string;
  host: string;
  port: number;
  username: string;
  password: string | null;
}

function formatSize(bytes: number): string {
  if (bytes === 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

function getLanguageExtension(filename: string) {
  const ext = filename.split(".").pop()?.toLowerCase();
  switch (ext) {
    case "js": case "jsx": case "ts": case "tsx": case "mjs": case "cjs":
      return javascript({ jsx: true, typescript: ext?.includes("ts") });
    case "py": return python();
    case "json": return json();
    case "html": case "htm": return html();
    case "css": case "scss": case "less": return css();
    case "xml": case "svg": case "xsl": return xml();
    case "md": case "markdown": return markdown();
    default: return [];
  }
}

function FileIcon({ isDir }: { isDir: boolean }) {
  if (isDir) {
    return (
      <svg className="w-5 h-5 text-amber-400" fill="currentColor" viewBox="0 0 20 20">
        <path d="M2 6a2 2 0 012-2h5l2 2h5a2 2 0 012 2v6a2 2 0 01-2 2H4a2 2 0 01-2-2V6z" />
      </svg>
    );
  }
  return (
    <svg className="w-5 h-5 text-zinc-400" fill="currentColor" viewBox="0 0 20 20">
      <path fillRule="evenodd" d="M4 4a2 2 0 012-2h4.586A2 2 0 0112 2.586L15.414 6A2 2 0 0116 7.414V16a2 2 0 01-2 2H6a2 2 0 01-2-2V4z" clipRule="evenodd" />
    </svg>
  );
}

function App() {
  const [connected, setConnected] = useState(false);
  const [connecting, setConnecting] = useState(false);
  const [error, setError] = useState("");

  const [host, setHost] = useState("");
  const [port, setPort] = useState("22");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [savePassword, setSavePassword] = useState(true);

  const [currentPath, setCurrentPath] = useState("/");
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);

  const [savedConnections, setSavedConnections] = useState<SavedConnection[]>([]);
  const [showSaveForm, setShowSaveForm] = useState(false);
  const [saveLabel, setSaveLabel] = useState("");

  const [editingFile, setEditingFile] = useState<string | null>(null);
  const [editingContent, setEditingContent] = useState("");
  const [editorDirty, setEditorDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const editorRef = useRef<HTMLDivElement>(null);
  const editorViewRef = useRef<EditorView | null>(null);

  const [showNewFileInput, setShowNewFileInput] = useState(false);
  const [newFileName, setNewFileName] = useState("");

  const [confirmDelete, setConfirmDelete] = useState<FileEntry | null>(null);

  const [dirWritable, setDirWritable] = useState(true);
  const [fileWritable, setFileWritable] = useState(true);

  const isPermissionError = (err: unknown): boolean => {
    const msg = String(err).toLowerCase();
    return msg.includes("permission denied") || msg.includes("operation not permitted");
  };

  useEffect(() => {
    loadSavedConnections();
  }, []);

  useEffect(() => {
    if (!connected) return;
    const interval = setInterval(async () => {
      try {
        const entries: FileEntry[] = await invoke("list_directory", { path: currentPath });
        entries.sort((a, b) => {
          if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
          return a.name.localeCompare(b.name);
        });
        setFiles(entries);
      } catch {}
    }, 3000);
    return () => clearInterval(interval);
  }, [connected, currentPath]);

  useEffect(() => {
    if (!editingFile || !editorRef.current) return;

    if (editorViewRef.current) {
      editorViewRef.current.destroy();
    }

    const lang = getLanguageExtension(editingFile);

    const state = EditorState.create({
      doc: editingContent,
      extensions: [
        lineNumbers(),
        highlightActiveLine(),
        highlightActiveLineGutter(),
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
        oneDark,
        EditorView.theme({
          "&": { height: "100%", fontSize: "13px" },
          ".cm-scroller": { overflow: "auto" },
          ".cm-content": { fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', monospace" },
          ".cm-gutters": { fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', monospace" },
        }),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            setEditorDirty(true);
          }
        }),
        ...(Array.isArray(lang) ? lang : [lang]),
      ],
    });

    const view = new EditorView({
      state,
      parent: editorRef.current,
    });

    editorViewRef.current = view;

    return () => {
      view.destroy();
      editorViewRef.current = null;
    };
  }, [editingFile, editingContent]);

  const loadSavedConnections = async () => {
    try {
      const conns: SavedConnection[] = await invoke("get_saved_connections");
      setSavedConnections(conns);
    } catch (_) {}
  };

  const listFiles = useCallback(async (path: string) => {
    setLoading(true);
    setError("");
    try {
      const entries: FileEntry[] = await invoke("list_directory", { path });
      entries.sort((a, b) => {
        if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      setFiles(entries);
      setCurrentPath(path);
      const writable: boolean = await invoke("check_writable", { path });
      setDirWritable(writable);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const doConnect = async (h: string, p: number, u: string, pw: string) => {
    setConnecting(true);
    setError("");
    try {
      await invoke("ssh_connect", { host: h, port: p, username: u, password: pw });
      setHost(h);
      setPort(String(p));
      setUsername(u);
      setPassword(pw);
      setConnected(true);
      await listFiles("/");
    } catch (e) {
      setError(String(e));
    } finally {
      setConnecting(false);
    }
  };

  const handleConnect = async (e: React.FormEvent) => {
    e.preventDefault();
    await doConnect(host, parseInt(port), username, password);
  };

  const handleSavedConnect = async (conn: SavedConnection) => {
    if (conn.password) {
      await doConnect(conn.host, conn.port, conn.username, conn.password);
    } else {
      setHost(conn.host);
      setPort(String(conn.port));
      setUsername(conn.username);
      setPassword("");
      setError("Enter password for this connection");
    }
  };

  const handleSaveConnection = async () => {
    if (!saveLabel.trim()) return;
    const id = crypto.randomUUID();
    try {
      await invoke("save_connection", {
        id,
        label: saveLabel.trim(),
        host,
        port: parseInt(port),
        username,
        password: savePassword ? password : null,
      });
      await loadSavedConnections();
      setShowSaveForm(false);
      setSaveLabel("");
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDeleteConnection = async (e: React.MouseEvent, id: string) => {
    e.stopPropagation();
    try {
      await invoke("delete_connection", { id });
      await loadSavedConnections();
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDisconnect = async () => {
    try {
      await invoke("ssh_disconnect");
    } catch (_) {}
    setConnected(false);
    setFiles([]);
    setCurrentPath("/");
    setError("");
    closeEditor();
  };

  const openFile = async (file: FileEntry) => {
    if (file.is_dir) {
      const newPath =
        currentPath === "/"
          ? `/${file.name}`
          : `${currentPath}/${file.name}`;
      listFiles(newPath);
      return;
    }

    const filePath =
      currentPath === "/"
        ? `/${file.name}`
        : `${currentPath}/${file.name}`;

    setError("");
    try {
      let content: string;
      try {
        content = await invoke("read_file", { path: filePath });
      } catch (e) {
        if (isPermissionError(e)) {
          content = await invoke("sudo_read_file", { path: filePath, sudoPassword: password });
        } else {
          throw e;
        }
      }
      setEditingContent(content);
      setEditingFile(filePath);
      setEditorDirty(false);
      const writable: boolean = await invoke("check_writable", { path: filePath });
      setFileWritable(writable);
    } catch (e) {
      setError(String(e));
    }
  };

  const saveFile = async () => {
    if (!editingFile || !editorViewRef.current) return;
    setSaving(true);
    setError("");
    try {
      const content = editorViewRef.current.state.doc.toString();
      try {
        await invoke("write_file", { path: editingFile, content });
      } catch (e) {
        if (isPermissionError(e)) {
          await invoke("sudo_write_file", { path: editingFile, content, sudoPassword: password });
        } else {
          throw e;
        }
      }
      setEditorDirty(false);
      await listFiles(currentPath);
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const closeEditor = () => {
    setEditingFile(null);
    setEditingContent("");
    setEditorDirty(false);
    setFileWritable(true);
  };

  const handleCreateFile = async () => {
    if (!newFileName.trim()) return;
    const name = newFileName.trim();
    const fullPath =
      currentPath === "/" ? `/${name}` : `${currentPath}/${name}`;
    setError("");
    try {
      const isDir = name.endsWith("/");
      try {
        if (isDir) {
          await invoke("create_directory", { path: fullPath });
        } else {
          await invoke("create_file", { path: fullPath });
        }
      } catch (e) {
        if (isPermissionError(e)) {
          if (isDir) {
            await invoke("sudo_create_directory", { path: fullPath, sudoPassword: password });
          } else {
            await invoke("sudo_create_file", { path: fullPath, sudoPassword: password });
          }
        } else {
          throw e;
        }
      }
      setNewFileName("");
      setShowNewFileInput(false);
      await listFiles(currentPath);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDeleteFile = async (file: FileEntry) => {
    const filePath =
      currentPath === "/"
        ? `/${file.name}`
        : `${currentPath}/${file.name}`;
    setError("");
    try {
      try {
        await invoke("delete_file", { path: filePath, isDir: file.is_dir });
      } catch (e) {
        if (isPermissionError(e)) {
          await invoke("sudo_delete_file", { path: filePath, isDir: file.is_dir, sudoPassword: password });
        } else {
          throw e;
        }
      }
      setConfirmDelete(null);
      if (editingFile === filePath) {
        closeEditor();
      }
      await listFiles(currentPath);
    } catch (e) {
      setConfirmDelete(null);
      setError(String(e));
    }
  };

  const navigateUp = () => {
    if (currentPath === "/") return;
    const parent = currentPath.substring(0, currentPath.lastIndexOf("/")) || "/";
    listFiles(parent);
  };

  const navigateToSegment = (index: number) => {
    const segments = currentPath.split("/").filter(Boolean);
    const path = "/" + segments.slice(0, index + 1).join("/");
    listFiles(path);
  };

  const pathSegments = currentPath.split("/").filter(Boolean);

  if (!connected) {
    return (
      <div className="min-h-screen bg-zinc-950 flex items-center justify-center p-4">
        <div className={`flex items-stretch gap-6 w-full ${savedConnections.length > 0 ? "max-w-3xl" : "max-w-md"}`}>

          {/* Saved connections panel */}
          {savedConnections.length > 0 && (
            <div className="w-72 shrink-0 flex flex-col min-h-0">
              <p className="text-xs font-medium text-zinc-400 mb-2 shrink-0">Saved Connections</p>
              <div className="flex-1 overflow-y-auto min-h-0 max-h-[420px] space-y-1.5 pr-1">
                {savedConnections.map((conn) => (
                  <button
                    key={conn.id}
                    onClick={() => handleSavedConnect(conn)}
                    disabled={connecting}
                    className="w-full flex items-center gap-3 px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg hover:border-emerald-500/40 hover:bg-zinc-900/80 transition group disabled:opacity-50 cursor-pointer"
                  >
                    <div className="w-8 h-8 rounded-lg bg-emerald-500/10 flex items-center justify-center shrink-0">
                      <svg className="w-4 h-4 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M5 12h14M12 5l7 7-7 7" />
                      </svg>
                    </div>
                    <div className="flex-1 text-left min-w-0">
                      <div className="text-sm text-white font-medium truncate">{conn.label}</div>
                      <div className="text-xs text-zinc-500 truncate">
                        {conn.username}@{conn.host}:{conn.port}
                        {!conn.password && " (password required)"}
                      </div>
                    </div>
                    <div
                      onClick={(e) => handleDeleteConnection(e, conn.id)}
                      className="opacity-0 group-hover:opacity-100 p-1.5 hover:bg-zinc-700 rounded-md transition cursor-pointer"
                    >
                      <svg className="w-3.5 h-3.5 text-zinc-400 hover:text-red-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                      </svg>
                    </div>
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Login form */}
          <div className="flex-1 max-w-md">
            <div className="text-center mb-8">
              <div className="inline-flex items-center justify-center w-16 h-16 rounded-2xl bg-emerald-500/10 mb-4">
                <svg className="w-8 h-8 text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M5 12h14M12 5l7 7-7 7" />
                </svg>
              </div>
              <h1 className="text-2xl font-semibold text-white">SSH File Explorer</h1>
              <p className="text-zinc-500 mt-1">Connect to a remote Linux machine</p>
            </div>

            <form onSubmit={handleConnect} className="space-y-4">
              <div className="flex gap-3">
                <div className="flex-1">
                  <label className="block text-xs font-medium text-zinc-400 mb-1.5">Host</label>
                  <input
                    type="text"
                    value={host}
                    onChange={(e) => setHost(e.target.value)}
                    placeholder="192.168.1.100"
                    required
                    className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white placeholder-zinc-600 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition"
                  />
                </div>
                <div className="w-24">
                  <label className="block text-xs font-medium text-zinc-400 mb-1.5">Port</label>
                  <input
                    type="text"
                    value={port}
                    onChange={(e) => setPort(e.target.value)}
                    className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white placeholder-zinc-600 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition"
                  />
                </div>
              </div>

              <div>
                <label className="block text-xs font-medium text-zinc-400 mb-1.5">Username</label>
                <input
                  type="text"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                  placeholder="root"
                  required
                  className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white placeholder-zinc-600 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition"
                />
              </div>

              <div>
                <label className="block text-xs font-medium text-zinc-400 mb-1.5">Password</label>
                <input
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder="••••••••"
                  required
                  className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white placeholder-zinc-600 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition"
                />
              </div>

              {error && (
                <div className="px-3 py-2 bg-red-500/10 border border-red-500/20 rounded-lg text-red-400 text-sm">
                  {error}
                </div>
              )}

              <button
                type="submit"
                disabled={connecting}
                className="w-full py-2.5 bg-emerald-600 hover:bg-emerald-500 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg text-white font-medium transition cursor-pointer"
              >
                {connecting ? "Connecting..." : "Connect"}
              </button>
            </form>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-zinc-950 flex flex-col">
      {/* Top bar */}
      <div className="flex items-center gap-3 px-4 py-3 bg-zinc-900/50 border-b border-zinc-800">
        <div className="flex items-center gap-2 text-sm text-zinc-400">
          <div className="w-2 h-2 rounded-full bg-emerald-400" />
          <span className="text-zinc-300 font-medium">{username}@{host}</span>
        </div>

        {/* Breadcrumb */}
        <div className="flex-1 flex items-center gap-1 text-sm overflow-x-auto mx-4">
          <button
            onClick={() => listFiles("/")}
            className="text-zinc-400 hover:text-white transition shrink-0 cursor-pointer"
          >
            /
          </button>
          {pathSegments.map((seg, i) => (
            <span key={i} className="flex items-center gap-1 shrink-0">
              <span className="text-zinc-600">/</span>
              <button
                onClick={() => navigateToSegment(i)}
                className="text-zinc-400 hover:text-white transition cursor-pointer"
              >
                {seg}
              </button>
            </span>
          ))}
        </div>

        {/* Action buttons */}
        <button
          onClick={() => {
            setShowNewFileInput(true);
            setNewFileName("");
          }}
          className="px-3 py-1.5 text-xs text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-md transition cursor-pointer"
          title="New file or folder"
        >
          + New File/Folder
        </button>

        {!showSaveForm && (
          <button
            onClick={() => {
              setSaveLabel(`${username}@${host}`);
              setShowSaveForm(true);
            }}
            className="px-3 py-1.5 text-xs text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-md transition cursor-pointer"
          >
            Save
          </button>
        )}

        <button
          onClick={handleDisconnect}
          className="px-3 py-1.5 text-xs text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-md transition cursor-pointer"
        >
          Disconnect
        </button>
      </div>

      {/* Save connection inline form */}
      {showSaveForm && (
        <div className="flex items-center gap-2 px-4 py-2.5 bg-zinc-900/80 border-b border-zinc-800">
          <span className="text-xs text-zinc-400 shrink-0">Save as:</span>
          <input
            type="text"
            value={saveLabel}
            onChange={(e) => setSaveLabel(e.target.value)}
            placeholder="Connection name"
            autoFocus
            className="flex-1 px-2.5 py-1.5 bg-zinc-800 border border-zinc-700 rounded-md text-sm text-white placeholder-zinc-600 focus:outline-none focus:ring-1 focus:ring-emerald-500/40 transition"
            onKeyDown={(e) => {
              if (e.key === "Enter") handleSaveConnection();
              if (e.key === "Escape") setShowSaveForm(false);
            }}
          />
          <label className="flex items-center gap-1.5 text-xs text-zinc-400 shrink-0 cursor-pointer">
            <input
              type="checkbox"
              checked={savePassword}
              onChange={(e) => setSavePassword(e.target.checked)}
              className="rounded border-zinc-600 accent-emerald-500"
            />
            Save password
          </label>
          <button
            onClick={handleSaveConnection}
            className="px-3 py-1.5 text-xs text-white bg-emerald-600 hover:bg-emerald-500 rounded-md transition cursor-pointer"
          >
            Save
          </button>
          <button
            onClick={() => setShowSaveForm(false)}
            className="px-2 py-1.5 text-xs text-zinc-400 hover:text-white transition cursor-pointer"
          >
            Cancel
          </button>
        </div>
      )}

      {/* New file inline input */}
      {showNewFileInput && (
        <div className="flex items-center gap-2 px-4 py-2.5 bg-zinc-900/80 border-b border-zinc-800">
          <span className="text-xs text-zinc-400 shrink-0">New file/folder:</span>
          <input
            type="text"
            value={newFileName}
            onChange={(e) => setNewFileName(e.target.value)}
            placeholder="test.txt, testDir/, or testDir/test.txt"
            autoFocus
            className="flex-1 px-2.5 py-1.5 bg-zinc-800 border border-zinc-700 rounded-md text-sm text-white placeholder-zinc-600 focus:outline-none focus:ring-1 focus:ring-emerald-500/40 transition"
            onKeyDown={(e) => {
              if (e.key === "Enter") handleCreateFile();
              if (e.key === "Escape") setShowNewFileInput(false);
            }}
          />
          <button
            onClick={handleCreateFile}
            className="px-3 py-1.5 text-xs text-white bg-emerald-600 hover:bg-emerald-500 rounded-md transition cursor-pointer"
          >
            Create
          </button>
          <button
            onClick={() => setShowNewFileInput(false)}
            className="px-2 py-1.5 text-xs text-zinc-400 hover:text-white transition cursor-pointer"
          >
            Cancel
          </button>
        </div>
      )}

      {error && (
        <div className="mx-4 mt-3 px-3 py-2 bg-red-500/10 border border-red-500/20 rounded-lg text-red-400 text-sm">
          {error}
        </div>
      )}

      {/* Protected directory warning */}
      {!dirWritable && (
        <div className="flex items-center gap-2 px-4 py-2 bg-amber-500/10 border-b border-amber-500/20">
          <svg className="w-4 h-4 text-amber-400 shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 9v2m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" />
          </svg>
          <span className="text-xs text-amber-400">
            This directory is protected — actions will use elevated privileges (sudo)
          </span>
        </div>
      )}

      {/* Delete confirmation modal */}
      {confirmDelete && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
          <div className="bg-zinc-900 border border-zinc-700 rounded-xl p-5 max-w-sm w-full mx-4 shadow-2xl">
            <h3 className="text-white font-medium mb-2">Delete {confirmDelete.is_dir ? "folder" : "file"}?</h3>
            <p className="text-sm text-zinc-400 mb-4">
              Are you sure you want to delete <span className="text-zinc-200 font-medium">{confirmDelete.name}</span>?
              {confirmDelete.is_dir && " This will delete all contents inside it."}
            </p>
            <div className="flex justify-end gap-2">
              <button
                onClick={() => setConfirmDelete(null)}
                className="px-4 py-2 text-sm text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-lg transition cursor-pointer"
              >
                Cancel
              </button>
              <button
                onClick={() => handleDeleteFile(confirmDelete)}
                className="px-4 py-2 text-sm text-white bg-red-600 hover:bg-red-500 rounded-lg transition cursor-pointer"
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Main content area */}
      <div className="flex-1 flex min-h-0">
        {/* File list */}
        <div className={`overflow-auto ${editingFile ? "w-80 shrink-0 border-r border-zinc-800" : "flex-1"}`}>
          <table className="w-full text-sm">
            <thead>
              <tr className="text-xs text-zinc-500 uppercase tracking-wider border-b border-zinc-800/50">
                <th className="text-left py-2.5 px-4 font-medium">Name</th>
                {!editingFile && (
                  <>
                    <th className="text-right py-2.5 px-4 font-medium w-28">Size</th>
                    <th className="text-left py-2.5 px-4 font-medium w-36">Modified</th>
                    <th className="text-left py-2.5 px-4 font-medium w-28">Permissions</th>
                  </>
                )}
                <th className="w-10" />
              </tr>
            </thead>
            <tbody>
              {currentPath !== "/" && (
                <tr
                  onClick={navigateUp}
                  className="hover:bg-zinc-900/50 cursor-pointer transition group"
                >
                  <td className="py-2 px-4 flex items-center gap-2.5">
                    <svg className="w-5 h-5 text-zinc-500" fill="currentColor" viewBox="0 0 20 20">
                      <path d="M2 6a2 2 0 012-2h5l2 2h5a2 2 0 012 2v6a2 2 0 01-2 2H4a2 2 0 01-2-2V6z" />
                    </svg>
                    <span className="text-zinc-400 group-hover:text-white transition">..</span>
                  </td>
                  {!editingFile && (<><td /><td /><td /></>)}
                  <td />
                </tr>
              )}
              {loading ? (
                <tr>
                  <td colSpan={editingFile ? 2 : 5} className="py-12 text-center text-zinc-500">
                    Loading...
                  </td>
                </tr>
              ) : (
                files.map((file) => (
                  <tr
                    key={file.name}
                    onClick={() => openFile(file)}
                    className={`border-b border-zinc-800/30 transition ${
                      file.is_dir
                        ? "hover:bg-zinc-900/50 cursor-pointer"
                        : "hover:bg-zinc-900/30 cursor-pointer"
                    } group ${
                      editingFile &&
                      editingFile === (currentPath === "/" ? `/${file.name}` : `${currentPath}/${file.name}`)
                        ? "bg-zinc-800/50"
                        : ""
                    }`}
                  >
                    <td className="py-2 px-4 flex items-center gap-2.5">
                      <FileIcon isDir={file.is_dir} />
                      <span className="text-zinc-300 group-hover:text-white transition truncate">
                        {file.name}
                      </span>
                    </td>
                    {!editingFile && (
                      <>
                        <td className="py-2 px-4 text-right text-zinc-500 tabular-nums">
                          {file.is_dir ? "—" : formatSize(file.size)}
                        </td>
                        <td className="py-2 px-4 text-zinc-500">{file.modified}</td>
                        <td className="py-2 px-4 text-zinc-600 font-mono text-xs">
                          {file.permissions}
                        </td>
                      </>
                    )}
                    <td className="py-2 px-2">
                      <div
                        onClick={(e) => {
                          e.stopPropagation();
                          setConfirmDelete(file);
                        }}
                        className="opacity-0 group-hover:opacity-100 p-1 hover:bg-zinc-700 rounded transition cursor-pointer"
                        title={`Delete ${file.name}`}
                      >
                        <svg className="w-3.5 h-3.5 text-zinc-500 hover:text-red-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                        </svg>
                      </div>
                    </td>
                  </tr>
                ))
              )}
            </tbody>
          </table>
        </div>

        {/* Editor panel */}
        {editingFile && (
          <div className="flex-1 flex flex-col min-h-0">
            {/* Editor header */}
            <div className="flex items-center gap-2 px-4 py-2 bg-zinc-900/60 border-b border-zinc-800 shrink-0">
              <svg className="w-4 h-4 text-zinc-500" fill="currentColor" viewBox="0 0 20 20">
                <path fillRule="evenodd" d="M4 4a2 2 0 012-2h4.586A2 2 0 0112 2.586L15.414 6A2 2 0 0116 7.414V16a2 2 0 01-2 2H6a2 2 0 01-2-2V4z" clipRule="evenodd" />
              </svg>
              <span className="text-sm text-zinc-300 truncate flex-1">
                {editingFile.split("/").pop()}
              </span>
              {!fileWritable && (
                <span className="flex items-center gap-1 px-1.5 py-0.5 bg-amber-500/15 border border-amber-500/30 rounded text-[10px] text-amber-400 shrink-0">
                  <svg className="w-3 h-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M12 15v2m-6 4h12a2 2 0 002-2v-6a2 2 0 00-2-2H6a2 2 0 00-2 2v6a2 2 0 002 2zm10-10V7a4 4 0 00-8 0v4h8z" />
                  </svg>
                  Protected
                </span>
              )}
              {editorDirty && (
                <span className="w-2 h-2 rounded-full bg-amber-400 shrink-0" title="Unsaved changes" />
              )}
              <button
                onClick={saveFile}
                disabled={saving || !editorDirty}
                className="px-3 py-1 text-xs text-white bg-emerald-600 hover:bg-emerald-500 disabled:opacity-40 disabled:cursor-not-allowed rounded-md transition cursor-pointer"
              >
                {saving ? "Saving..." : "Save"}
              </button>
              <button
                onClick={closeEditor}
                className="p-1 text-zinc-400 hover:text-white hover:bg-zinc-700 rounded transition cursor-pointer"
                title="Close editor"
              >
                <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                </svg>
              </button>
            </div>
            {/* CodeMirror container */}
            <div ref={editorRef} className="flex-1 min-h-0 overflow-hidden" />
          </div>
        )}
      </div>

      {/* Status bar */}
      <div className="px-4 py-2 bg-zinc-900/30 border-t border-zinc-800 text-xs text-zinc-500 flex items-center justify-between">
        <span>{files.length} items</span>
        <span>{editingFile ? editingFile : currentPath}</span>
      </div>
    </div>
  );
}

export default App;
