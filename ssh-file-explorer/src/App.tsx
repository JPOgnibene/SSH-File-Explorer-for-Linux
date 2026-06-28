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
import { save, open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
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
  has_password: boolean;
  auth_method: string;
  key_path?: string;
}

interface SshKeyInfo {
  path: string;
  name: string;
  key_type: string;
  encrypted: boolean;
}

interface Transfer {
  id: string;
  type: 'upload' | 'download' | 'copy';
  fileName: string;
  bytesTransferred: number;
  totalBytes: number;
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

  const [authMethod, setAuthMethod] = useState<"password" | "key">("password");
  const [availableKeys, setAvailableKeys] = useState<SshKeyInfo[]>([]);
  const [selectedKeyPath, setSelectedKeyPath] = useState("");
  const [keyPassphrase, setKeyPassphrase] = useState("");
  const [sudoPassword, setSudoPassword] = useState("");

  const [currentPath, setCurrentPath] = useState("/");
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);

  const navHistoryRef = useRef<string[]>(["/"]);
  const navIndexRef = useRef(0);
  const navSkipPushRef = useRef(false);

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

  const [confirmDelete, setConfirmDelete] = useState<FileEntry[] | null>(null);

  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; file: FileEntry | null } | null>(null);
  const [clipboard, setClipboard] = useState<{ path: string; name: string; is_dir: boolean }[] | null>(null);

  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set());
  const lastClickedRef = useRef<string | null>(null);

  const internalDragRef = useRef<FileEntry[] | null>(null);
  const [isDraggingInternal, setIsDraggingInternal] = useState(false);
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  const [dirWritable, setDirWritable] = useState(false);
  const [dirSudoWritable, setDirSudoWritable] = useState(false);
  const [fileWritable, setFileWritable] = useState(true);

  const [searchQuery, setSearchQuery] = useState("");
  const [searchResults, setSearchResults] = useState<{ path: string; name: string; is_dir: boolean }[]>([]);
  const [searching, setSearching] = useState(false);
  const [showSearch, setShowSearch] = useState(false);
  const [searchSelectedIndex, setSearchSelectedIndex] = useState(-1);
  const searchTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchPrefixRef = useRef("");

  const [transfers, setTransfers] = useState<Transfer[]>([]);
  const [dragOverWindow, setDragOverWindow] = useState(false);
  const [activeDrag, setActiveDrag] = useState<string | null>(null);

  const isPermissionError = (err: unknown): boolean => {
    const msg = String(err).toLowerCase();
    return msg.includes("permission denied") || msg.includes("operation not permitted");
  };


  useEffect(() => {
    loadSavedConnections();
    loadSshKeys();
  }, []);

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "f" && connected) {
        e.preventDefault();
        setShowSearch(prev => {
          if (prev) { setSearchQuery(""); setSearchResults([]); }
          return !prev;
        });
      }
    };
    const dismissContextMenu = () => setContextMenu(null);
    window.addEventListener("keydown", handleKeyDown);
    window.addEventListener("click", dismissContextMenu);
    return () => { window.removeEventListener("keydown", handleKeyDown); window.removeEventListener("click", dismissContextMenu); };
  }, [connected]);

  const listFilesRef = useRef<(path: string) => Promise<void>>(null as unknown as (path: string) => Promise<void>);

  const loadSshKeys = async () => {
    try {
      const keys: SshKeyInfo[] = await invoke("discover_ssh_keys");
      setAvailableKeys(keys);
      if (keys.length > 0) {
        setSelectedKeyPath(keys[0].path);
      }
    } catch (_) {}
  };

  useEffect(() => {
    if (!connected) return;
    const interval = setInterval(async () => {
      try {
        let entries: FileEntry[];
        try {
          entries = await invoke("list_directory", { path: currentPath });
        } catch (e) {
          if (isPermissionError(e) && sudoPassword) {
            entries = await invoke("sudo_list_directory", { path: currentPath, sudoPassword });
          } else {
            return;
          }
        }
        entries.sort((a, b) => {
          if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
          return a.name.localeCompare(b.name);
        });
        setFiles(entries);
      } catch {}
    }, 20000);
    return () => clearInterval(interval);
  }, [connected, currentPath, sudoPassword]);

  useEffect(() => {
    if (!connected) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<{ id: string; transfer_type: string; file_name: string; bytes_transferred: number; total_bytes: number }>("transfer-progress", (event) => {
      if (cancelled) return;
      const p = event.payload;
      setTransfers(prev => {
        const idx = prev.findIndex(t => t.id === p.id);
        if (idx >= 0) {
          const updated = [...prev];
          updated[idx] = { ...updated[idx], bytesTransferred: p.bytes_transferred, totalBytes: p.total_bytes };
          return updated;
        }
        return [...prev, { id: p.id, type: p.transfer_type as 'upload' | 'download' | 'copy', fileName: p.file_name, bytesTransferred: p.bytes_transferred, totalBytes: p.total_bytes }];
      });
    }).then(fn => { if (cancelled) { fn(); return; } unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, [connected]);

  const [progressPort, setProgressPort] = useState(0);

  useEffect(() => {
    invoke<number>('get_progress_port').then(port => setProgressPort(port));
  }, []);

  useEffect(() => {
    if (!progressPort) return;
    const dragTransfer = transfers.find(t => t.type === 'download' && t.id.startsWith('drag-'));
    if (!dragTransfer) return;
    const SENTINEL = 18446744073709551615;
    const interval = setInterval(async () => {
      try {
        const resp = await fetch(`http://127.0.0.1:${progressPort}/`);
        const data = await resp.json();
        if (data.total >= SENTINEL) {
          clearInterval(interval);
          setTransfers(prev => {
            const idx = prev.findIndex(t => t.id === dragTransfer.id);
            if (idx >= 0) {
              const updated = [...prev];
              updated[idx] = { ...updated[idx], bytesTransferred: 1, totalBytes: 1 };
              return updated;
            }
            return prev;
          });
          setTimeout(() => {
            setTransfers(prev => prev.filter(t => t.id !== dragTransfer.id));
          }, 1500);
          return;
        }
        if (data.total > 0 && data.bytes > 0) {
          setTransfers(prev => {
            const idx = prev.findIndex(t => t.id === dragTransfer.id);
            if (idx >= 0) {
              const updated = [...prev];
              const capped = Math.min(data.bytes, data.total);
              updated[idx] = { ...updated[idx], bytesTransferred: capped, totalBytes: data.total };
              return updated;
            }
            return prev;
          });
        }
      } catch {}
    }, 150);
    return () => clearInterval(interval);
  }, [transfers, progressPort]);

  useEffect(() => {
    if (!connected) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    listen<{ id: string }>("transfer-complete", (event) => {
      if (cancelled) return;
      const id = event.payload.id;
      setTimeout(() => {
        setTransfers(prev => prev.filter(t => t.id !== id));
      }, 1500);
    }).then(fn => { if (cancelled) { fn(); return; } unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, [connected]);

  useEffect(() => {
    if (!connected) return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    getCurrentWindow().onDragDropEvent((event) => {
      if (cancelled) return;
      const { type } = event.payload;
      if (type === 'enter' || type === 'over') {
        setDragOverWindow(true);
      } else if (type === 'leave') {
        setDragOverWindow(false);
      } else if (type === 'drop') {
        setDragOverWindow(false);
        const paths: string[] = (event.payload as { paths?: string[] }).paths || [];
        if (paths.length > 0) {
          (async () => {
            if (!dirWritable && !(sudoPassword && dirSudoWritable)) {
              setError("Permission denied: you do not have write access to this directory");
              return;
            }
            for (const localPath of paths) {
              const fileName = localPath.replace(/\\/g, "/").split("/").pop() || "file";
              const remotePath = currentPath === "/" ? `/${fileName}` : `${currentPath}/${fileName}`;
              const transferId = `ul-${Date.now()}-${Math.random().toString(36).slice(2)}`;
              const isDir: boolean = await invoke("is_local_directory", { path: localPath });
              setTransfers(prev => [...prev, { id: transferId, type: 'upload', fileName, bytesTransferred: 0, totalBytes: 0 }]);
              try {
                if (!dirWritable && sudoPassword && dirSudoWritable) {
                  if (isDir) {
                    await invoke("sudo_upload_directory", { transferId, localPath, remotePath, sudoPassword });
                  } else {
                    await invoke("sudo_upload_file", { transferId, localPath, remotePath, sudoPassword });
                  }
                } else if (!dirWritable) {
                  throw new Error("Permission denied");
                } else {
                  try {
                    if (isDir) {
                      await invoke("upload_directory", { transferId, localPath, remotePath });
                    } else {
                      await invoke("upload_file", { transferId, localPath, remotePath });
                    }
                  } catch (e) {
                    if (isPermissionError(e) && sudoPassword) {
                      if (isDir) {
                        await invoke("sudo_upload_directory", { transferId, localPath, remotePath, sudoPassword });
                      } else {
                        await invoke("sudo_upload_file", { transferId, localPath, remotePath, sudoPassword });
                      }
                    } else {
                      throw e;
                    }
                  }
                }
              } catch (e) {
                if (!String(e).includes("Transfer cancelled")) setError(String(e));
              } finally {
                setTransfers(prev => prev.filter(t => t.id !== transferId));
              }
            }
            await listFiles(currentPath);
          })();
        }
      }
    }).then(fn => { if (cancelled) { fn(); return; } unlisten = fn; });
    return () => { cancelled = true; unlisten?.(); };
  }, [connected, currentPath, dirWritable, dirSudoWritable, sudoPassword]);


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
      let entries: FileEntry[];
      try {
        entries = await invoke("list_directory", { path });
      } catch (e) {
        if (isPermissionError(e) && sudoPassword) {
          entries = await invoke("sudo_list_directory", { path, sudoPassword });
        } else {
          throw e;
        }
      }
      entries.sort((a, b) => {
        if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      setFiles(entries);
      setSelectedFiles(new Set());
      setCurrentPath(path);
      if (navSkipPushRef.current) {
        navSkipPushRef.current = false;
      } else {
        const hist = navHistoryRef.current;
        const idx = navIndexRef.current;
        navHistoryRef.current = [...hist.slice(0, idx + 1), path];
        navIndexRef.current = navHistoryRef.current.length - 1;
      }
      let writable = false;
      try {
        writable = await invoke("check_writable", { path }) as boolean;
      } catch {}
      setDirWritable(writable);
      let sudoWrite = false;
      if (!writable && sudoPassword) {
        try {
          sudoWrite = await invoke("check_sudo_writable", { path, sudoPassword }) as boolean;
        } catch {}
      }
      setDirSudoWritable(sudoWrite);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, [sudoPassword]);

  listFilesRef.current = listFiles;

  useEffect(() => {
    if (!connected) return;
    const handleMouseButton = (e: MouseEvent) => {
      if (e.button === 3 || e.button === 4) {
        e.preventDefault();
        const hist = navHistoryRef.current;
        const idx = navIndexRef.current;
        if (e.button === 3 && idx > 0) {
          navIndexRef.current = idx - 1;
          navSkipPushRef.current = true;
          listFilesRef.current?.(hist[idx - 1]);
        } else if (e.button === 4 && idx < hist.length - 1) {
          navIndexRef.current = idx + 1;
          navSkipPushRef.current = true;
          listFilesRef.current?.(hist[idx + 1]);
        }
      }
    };
    window.addEventListener("mouseup", handleMouseButton);
    return () => window.removeEventListener("mouseup", handleMouseButton);
  }, [connected]);

  const oleDragActiveRef = useRef(false);

  useEffect(() => {
    if (!isDraggingInternal) return;
    document.body.style.cursor = 'grabbing';

    const startOleDrag = async () => {
      const targets = internalDragRef.current;
      if (!targets || targets.length === 0 || oleDragActiveRef.current) return;
      oleDragActiveRef.current = true;
      setDropTarget(null);

      let result: string;
      if (targets.length > 1) {
        result = await handleMultiDragOut(targets);
      } else {
        result = await handleDragOut(targets[0]);
      }
      oleDragActiveRef.current = false;

      if (result === "reentry") {
        document.body.style.cursor = 'grabbing';
        return;
      }
      document.body.style.cursor = '';
      setIsDraggingInternal(false);
      setDropTarget(null);
      internalDragRef.current = null;
    };

    const handleMouseMove = (e: MouseEvent) => {
      if (!internalDragRef.current || oleDragActiveRef.current) return;
      const atEdge = e.clientX <= 0 || e.clientY <= 0 ||
          e.clientX >= window.innerWidth - 1 || e.clientY >= window.innerHeight - 1;
      if (atEdge) {
        startOleDrag();
        return;
      }
      const el = document.elementFromPoint(e.clientX, e.clientY);
      const row = el?.closest('tr[data-folder]');
      setDropTarget(row ? row.getAttribute('data-folder') : null);
    };

    const handleMouseUp = () => {
      if (oleDragActiveRef.current) return;
      const targets = internalDragRef.current;
      const dest = dropTarget;
      document.body.style.cursor = '';
      setIsDraggingInternal(false);
      setDropTarget(null);
      internalDragRef.current = null;
      if (dest && targets && targets.length > 0) {
        handleInternalCopy(targets, dest);
      }
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
      document.body.style.cursor = '';
    };
  }, [isDraggingInternal, dropTarget]);

  const doConnect = async (h: string, p: number, u: string, pw: string) => {
    setConnecting(true);
    setError("");
    try {
      await invoke("ssh_connect", { host: h, port: p, username: u, password: pw });
      setHost(h);
      setPort(String(p));
      setUsername(u);
      setPassword(pw);
      setSudoPassword(pw);
      setConnected(true);
      await listFiles("/");
    } catch (e) {
      setError(String(e));
    } finally {
      setConnecting(false);
    }
  };

  const doConnectKey = async (h: string, p: number, u: string, kp: string, pp: string | null) => {
    setConnecting(true);
    setError("");
    try {
      await invoke("ssh_connect_key", {
        host: h,
        port: p,
        username: u,
        keyPath: kp,
        passphrase: pp || null,
      });
      setHost(h);
      setPort(String(p));
      setUsername(u);
      setPassword("");
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
    if (authMethod === "key") {
      await doConnectKey(host, parseInt(port), username, selectedKeyPath, keyPassphrase || null);
    } else {
      await doConnect(host, parseInt(port), username, password);
    }
  };

  const handleSavedConnect = async (conn: SavedConnection) => {
    if (conn.auth_method === "key" && conn.key_path) {
      if (conn.has_password) {
        try {
          const pw: string = await invoke("get_connection_password", { id: conn.id });
          setSudoPassword(pw);
        } catch (_) {}
      }
      await doConnectKey(conn.host, conn.port, conn.username, conn.key_path, null);
    } else if (conn.has_password) {
      try {
        const pw: string = await invoke("get_connection_password", { id: conn.id });
        await doConnect(conn.host, conn.port, conn.username, pw);
      } catch (e) {
        setHost(conn.host);
        setPort(String(conn.port));
        setUsername(conn.username);
        setPassword("");
        setError(String(e));
      }
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
        password: authMethod === "password"
          ? (savePassword ? password : null)
          : (sudoPassword || null),
        authMethod,
        keyPath: authMethod === "key" ? selectedKeyPath : null,
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
    setSudoPassword("");
    setClipboard(null);
    setSelectedFiles(new Set());
    closeEditor();
  };

  const openFile = (file: FileEntry) => handleOpenFile(file);

  const saveFile = async () => {
    if (!editingFile || !editorViewRef.current) return;
    setSaving(true);
    setError("");
    try {
      const content = editorViewRef.current.state.doc.toString();
      try {
        await invoke("write_file", { path: editingFile, content });
      } catch (e) {
        if (isPermissionError(e) && sudoPassword) {
          await invoke("sudo_write_file", { path: editingFile, content, sudoPassword });
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
        if (isPermissionError(e) && sudoPassword) {
          if (isDir) {
            await invoke("sudo_create_directory", { path: fullPath, sudoPassword });
          } else {
            await invoke("sudo_create_file", { path: fullPath, sudoPassword });
          }
        } else {
          throw e;
        }
      }
      setNewFileName("");
      setShowNewFileInput(false);
      // If a nested path like "dir/file.txt" was created, navigate to the parent dir
      const slashIdx = name.indexOf("/");
      if (!isDir && slashIdx !== -1) {
        const targetDir = fullPath.substring(0, fullPath.lastIndexOf("/"));
        await listFiles(targetDir);
      } else {
        await listFiles(currentPath);
      }
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDeleteFile = async (targets: FileEntry[]) => {
    setError("");
    setConfirmDelete(null);
    for (const file of targets) {
      const filePath =
        currentPath === "/"
          ? `/${file.name}`
          : `${currentPath}/${file.name}`;
      try {
        try {
          await invoke("delete_file", { path: filePath, isDir: file.is_dir });
        } catch (e) {
          if (isPermissionError(e) && sudoPassword) {
            await invoke("sudo_delete_file", { path: filePath, isDir: file.is_dir, sudoPassword });
          } else {
            throw e;
          }
        }
        if (editingFile === filePath) {
          closeEditor();
        }
      } catch (e) {
        setError(String(e));
      }
    }
    setSelectedFiles(new Set());
    await listFiles(currentPath);
  };

  const handlePaste = async () => {
    if (!clipboard || clipboard.length === 0) return;
    for (const item of clipboard) {
      const dest = currentPath === "/" ? `/${item.name}` : `${currentPath}/${item.name}`;
      if (item.path === dest) continue;
      const transferId = `copy-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setTransfers(prev => [...prev, { id: transferId, type: 'copy', fileName: item.name, bytesTransferred: 0, totalBytes: 0 }]);
      try {
        try {
          await invoke("copy_path", { transferId, src: item.path, dest, isDir: item.is_dir });
        } catch (e) {
          if (isPermissionError(e) && sudoPassword) {
            await invoke("sudo_copy_path", { transferId, src: item.path, dest, isDir: item.is_dir, sudoPassword });
          } else {
            throw e;
          }
        }
      } catch (e) {
        setError(String(e));
      } finally {
        setTransfers(prev => prev.filter(t => t.id !== transferId));
      }
    }
    await listFiles(currentPath);
  };

  const handleInternalCopy = async (targets: FileEntry[], destFolder: string) => {
    let destDir: string;
    if (destFolder === "..") {
      destDir = currentPath.substring(0, currentPath.lastIndexOf("/")) || "/";
    } else {
      destDir = currentPath === "/" ? `/${destFolder}` : `${currentPath}/${destFolder}`;
    }
    for (const file of targets) {
      if (file.is_dir && file.name === destFolder) continue;
      const src = currentPath === "/" ? `/${file.name}` : `${currentPath}/${file.name}`;
      const dest = destDir === "/" ? `/${file.name}` : `${destDir}/${file.name}`;
      const transferId = `copy-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setTransfers(prev => [...prev, { id: transferId, type: 'copy', fileName: file.name, bytesTransferred: 0, totalBytes: 0 }]);
      try {
        try {
          await invoke("copy_path", { transferId, src, dest, isDir: file.is_dir });
        } catch (e) {
          if (isPermissionError(e) && sudoPassword) {
            await invoke("sudo_copy_path", { transferId, src, dest, isDir: file.is_dir, sudoPassword });
          } else {
            throw e;
          }
        }
      } catch (e) {
        setError(String(e));
      } finally {
        setTransfers(prev => prev.filter(t => t.id !== transferId));
      }
    }
    await listFiles(currentPath);
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

  const handleSearch = (query: string) => {
    setSearchQuery(query);
    if (searchTimerRef.current) clearTimeout(searchTimerRef.current);
    if (!query.trim()) {
      setSearchResults([]);
      setSearching(false);
      setSearchSelectedIndex(-1);
      return;
    }
    setSearching(true);
    const trimmed = query.trim();
    const slashIdx = trimmed.lastIndexOf("/");
    searchPrefixRef.current = slashIdx !== -1 ? trimmed.substring(0, slashIdx + 1) : "";
    searchTimerRef.current = setTimeout(async () => {
      try {
        let searchPath = currentPath;
        let searchTerm = trimmed;
        const slashIdx = searchTerm.lastIndexOf("/");
        if (slashIdx !== -1) {
          const dirPart = searchTerm.substring(0, slashIdx);
          searchTerm = searchTerm.substring(slashIdx + 1);
          if (dirPart.startsWith("/")) {
            searchPath = dirPart || "/";
          } else {
            searchPath = currentPath === "/" ? `/${dirPart}` : `${currentPath}/${dirPart}`;
          }
        }
        if (!searchTerm) searchTerm = "*";
        let results: { path: string; name: string; is_dir: boolean }[] = await invoke("search_files", { query: searchTerm, searchPath });
        if (results.length === 0 && sudoPassword) {
          try {
            const sudoResults: { path: string; name: string; is_dir: boolean }[] = await invoke("sudo_search_files", { query: searchTerm, searchPath, sudoPassword });
            if (sudoResults.length > 0) results = sudoResults;
          } catch {}
        }
        setSearchResults(results);
        setSearchSelectedIndex(-1);
      } catch (e) {
        setSearchResults([]);
        setSearchSelectedIndex(-1);
      } finally {
        setSearching(false);
      }
    }, 300);
  };

  const handleSearchSelect = (result: { path: string; name: string; is_dir: boolean }) => {
    setShowSearch(false);
    setSearchQuery("");
    setSearchResults([]);
    if (result.is_dir) {
      listFiles(result.path);
    } else {
      const parent = result.path.substring(0, result.path.lastIndexOf("/")) || "/";
      listFiles(parent);
      handleOpenFile({ name: result.name, is_dir: false, size: 0, modified: "", permissions: "" }, parent);
    }
  };

  const handleOpenFile = async (file: FileEntry, fromPath?: string) => {
    const dir = fromPath || currentPath;
    const filePath = dir === "/" ? `/${file.name}` : `${dir}/${file.name}`;
    if (file.is_dir) {
      listFiles(filePath);
      return;
    }
    setError("");
    try {
      let content: string;
      try {
        content = await invoke("read_file", { path: filePath });
      } catch (e) {
        if (isPermissionError(e) && sudoPassword) {
          content = await invoke("sudo_read_file", { path: filePath, sudoPassword });
        } else {
          throw e;
        }
      }
      setEditingFile(filePath);
      setEditingContent(content);
      setEditorDirty(false);
      const writable: boolean = await invoke("check_writable", { path: filePath });
      setFileWritable(writable);
    } catch (e) {
      setError(String(e));
    }
  };

  const handleDownload = async (file: FileEntry, fromPath?: string) => {
    const dir = fromPath || currentPath;
    const remotePath = dir === "/" ? `/${file.name}` : `${dir}/${file.name}`;
    try {
      let localPath: string | null;
      if (file.is_dir) {
        const selected = await open({ multiple: false, directory: true, title: `Save "${file.name}" to...` });
        if (!selected) return;
        localPath = `${String(selected)}${String(selected).endsWith("\\") || String(selected).endsWith("/") ? "" : "/"}${file.name}`;
      } else {
        localPath = await save({ defaultPath: file.name });
      }
      if (!localPath) return;
      const transferId = `dl-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setTransfers(prev => [...prev, { id: transferId, type: 'download', fileName: file.name, bytesTransferred: 0, totalBytes: 0 }]);
      try {
        if (file.is_dir) {
          await invoke("download_directory", { transferId, remotePath, localPath });
        } else {
          await invoke("download_file", { transferId, remotePath, localPath });
        }
      } finally {
        setTransfers(prev => prev.filter(t => t.id !== transferId));
      }
    } catch (e) {
      if (!String(e).includes("Transfer cancelled")) setError(String(e));
    }
  };

  const handleMultiDownload = async (targets: FileEntry[]) => {
    const selected = await open({ multiple: false, directory: true, title: `Save ${targets.length} items to...` });
    if (!selected) return;
    const destDir = String(selected);
    for (const file of targets) {
      const remotePath = currentPath === "/" ? `/${file.name}` : `${currentPath}/${file.name}`;
      const localPath = `${destDir}${destDir.endsWith("\\") || destDir.endsWith("/") ? "" : "/"}${file.name}`;
      const transferId = `dl-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setTransfers(prev => [...prev, { id: transferId, type: 'download', fileName: file.name, bytesTransferred: 0, totalBytes: 0 }]);
      try {
        if (file.is_dir) {
          await invoke("download_directory", { transferId, remotePath, localPath });
        } else {
          await invoke("download_file", { transferId, remotePath, localPath });
        }
      } catch (e) {
        if (!String(e).includes("Transfer cancelled")) setError(String(e));
      } finally {
        setTransfers(prev => prev.filter(t => t.id !== transferId));
      }
    }
  };

  const handleDragOut = async (file: FileEntry): Promise<string> => {
    const remotePath = currentPath === "/" ? `/${file.name}` : `${currentPath}/${file.name}`;
    const transferId = `drag-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    setTransfers(prev => [...prev, { id: transferId, type: 'download', fileName: file.name, bytesTransferred: 0, totalBytes: file.size }]);
    setActiveDrag(file.name);
    document.body.style.cursor = 'grabbing';
    try {
      const result: string = await invoke("start_virtual_drag", {
        transferId,
        remotePath,
        fileName: file.name,
        fileSize: file.size,
        isDir: file.is_dir,
      });
      setTransfers(prev => prev.filter(t => t.id !== transferId));
      return result;
    } catch (e) {
      if (!String(e).includes("Transfer cancelled")) setError(String(e));
      setTransfers(prev => prev.filter(t => t.id !== transferId));
      return "error";
    } finally {
      setActiveDrag(null);
      document.body.style.cursor = '';
    }
  };

  const handleMultiDragOut = async (targets: FileEntry[]): Promise<string> => {
    const transferId = `drag-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const totalSize = targets.reduce((sum, f) => sum + f.size, 0);
    const label = `${targets.length} items`;
    setTransfers(prev => [...prev, { id: transferId, type: 'download', fileName: label, bytesTransferred: 0, totalBytes: totalSize }]);
    setActiveDrag(label);
    document.body.style.cursor = 'grabbing';
    try {
      const filesArg = targets.map(f => ({
        name: f.name,
        remote_path: currentPath === "/" ? `/${f.name}` : `${currentPath}/${f.name}`,
        size: f.size,
        is_dir: f.is_dir,
      }));
      const result: string = await invoke("start_multi_drag", { transferId, files: filesArg });
      setTransfers(prev => prev.filter(t => t.id !== transferId));
      return result;
    } catch (e) {
      if (!String(e).includes("Transfer cancelled")) setError(String(e));
      setTransfers(prev => prev.filter(t => t.id !== transferId));
      return "error";
    } finally {
      setActiveDrag(null);
      document.body.style.cursor = '';
    }
  };

  const handleUpload = async () => {
    if (!dirWritable && !(sudoPassword && dirSudoWritable)) {
      setError("Permission denied: you do not have write access to this directory");
      return;
    }
    try {
      const selected = await open({ multiple: true, directory: false });
      if (!selected) return;
      const paths = Array.isArray(selected) ? selected.map(String) : [String(selected)];
      for (const localPath of paths) {
        const fileName = localPath.replace(/\\/g, "/").split("/").pop() || "uploaded_file";
        const remotePath = currentPath === "/" ? `/${fileName}` : `${currentPath}/${fileName}`;
        const transferId = `ul-${Date.now()}-${Math.random().toString(36).slice(2)}`;
        setTransfers(prev => [...prev, { id: transferId, type: 'upload', fileName, bytesTransferred: 0, totalBytes: 0 }]);
        try {
          if (!dirWritable && sudoPassword && dirSudoWritable) {
            await invoke("sudo_upload_file", { transferId, localPath, remotePath, sudoPassword });
          } else if (!dirWritable) {
            throw new Error("Permission denied");
          } else {
            try {
              await invoke("upload_file", { transferId, localPath, remotePath });
            } catch (e) {
              if (isPermissionError(e) && sudoPassword) {
                await invoke("sudo_upload_file", { transferId, localPath, remotePath, sudoPassword });
              } else {
                throw e;
              }
            }
          }
        } catch (e) {
          if (!String(e).includes("Transfer cancelled")) setError(String(e));
        } finally {
          setTransfers(prev => prev.filter(t => t.id !== transferId));
        }
      }
      await listFiles(currentPath);
    } catch (e) {
      if (!String(e).includes("Transfer cancelled")) setError(String(e));
    }
  };

  const handleUploadDirectory = async () => {
    if (!dirWritable && !(sudoPassword && dirSudoWritable)) {
      setError("Permission denied: you do not have write access to this directory");
      return;
    }
    try {
      const selected = await open({ multiple: false, directory: true });
      if (!selected) return;
      const localPath = String(selected);
      const dirName = localPath.replace(/\\/g, "/").split("/").pop() || "folder";
      const remotePath = currentPath === "/" ? `/${dirName}` : `${currentPath}/${dirName}`;
      const transferId = `ul-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setTransfers(prev => [...prev, { id: transferId, type: 'upload', fileName: dirName, bytesTransferred: 0, totalBytes: 0 }]);
      try {
        if (!dirWritable && sudoPassword && dirSudoWritable) {
          await invoke("sudo_upload_directory", { transferId, localPath, remotePath, sudoPassword });
        } else if (!dirWritable) {
          throw new Error("Permission denied");
        } else {
          try {
            await invoke("upload_directory", { transferId, localPath, remotePath });
          } catch (e) {
            if (isPermissionError(e) && sudoPassword) {
              await invoke("sudo_upload_directory", { transferId, localPath, remotePath, sudoPassword });
            } else {
              throw e;
            }
          }
        }
      } catch (e) {
        if (!String(e).includes("Transfer cancelled")) setError(String(e));
      } finally {
        setTransfers(prev => prev.filter(t => t.id !== transferId));
      }
      await listFiles(currentPath);
    } catch (e) {
      if (!String(e).includes("Transfer cancelled")) setError(String(e));
    }
  };

  const pathSegments = currentPath.split("/").filter(Boolean);

  if (!connected) {
    return (
      <div className="min-h-screen bg-zinc-950 flex items-center justify-center p-4" onContextMenu={(e) => e.preventDefault()}>
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
                    <div className="flex-1 text-left min-w-0">
                      <div className="text-sm text-white font-medium truncate">{conn.label}</div>
                      <div className="text-xs text-zinc-500 truncate">
                        {conn.username}@{conn.host}:{conn.port}
                        {conn.auth_method === "key" ? " (key)" : !conn.has_password ? " (password required)" : ""}
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

              {/* Auth method toggle */}
              <div>
                <label className="block text-xs font-medium text-zinc-400 mb-1.5">Authentication</label>
                <div className="flex rounded-lg overflow-hidden border border-zinc-800">
                  <button
                    type="button"
                    onClick={() => setAuthMethod("password")}
                    className={`flex-1 py-2 text-xs font-medium transition cursor-pointer ${
                      authMethod === "password"
                        ? "bg-emerald-600 text-white"
                        : "bg-zinc-900 text-zinc-400 hover:text-white"
                    }`}
                  >
                    Password
                  </button>
                  <button
                    type="button"
                    onClick={() => setAuthMethod("key")}
                    className={`flex-1 py-2 text-xs font-medium transition cursor-pointer ${
                      authMethod === "key"
                        ? "bg-emerald-600 text-white"
                        : "bg-zinc-900 text-zinc-400 hover:text-white"
                    }`}
                  >
                    SSH Key{availableKeys.length > 0 ? ` (${availableKeys.length} found)` : ""}
                  </button>
                </div>
              </div>

              {authMethod === "password" ? (
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
              ) : (
                <>
                  {availableKeys.length > 0 ? (
                    <div>
                      <label className="block text-xs font-medium text-zinc-400 mb-1.5">Private Key</label>
                      <select
                        value={selectedKeyPath}
                        onChange={(e) => setSelectedKeyPath(e.target.value)}
                        className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition cursor-pointer"
                      >
                        {availableKeys.map((k) => (
                          <option key={k.path} value={k.path}>
                            {k.name} ({k.key_type}){k.encrypted ? " 🔒" : ""}
                          </option>
                        ))}
                      </select>
                    </div>
                  ) : (
                    <div className="px-3 py-2 bg-amber-500/10 border border-amber-500/20 rounded-lg text-amber-400 text-sm">
                      No SSH keys found in ~/.ssh/
                    </div>
                  )}
                  {availableKeys.find((k) => k.path === selectedKeyPath)?.encrypted && (
                    <div>
                      <label className="block text-xs font-medium text-zinc-400 mb-1.5">Key Passphrase</label>
                      <input
                        type="password"
                        value={keyPassphrase}
                        onChange={(e) => setKeyPassphrase(e.target.value)}
                        placeholder="Passphrase for encrypted key"
                        className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white placeholder-zinc-600 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition"
                      />
                    </div>
                  )}
                  <div>
                    <label className="block text-xs font-medium text-zinc-400 mb-1.5">
                      Sudo Password <span className="text-zinc-600 font-normal">(optional)</span>
                    </label>
                    <input
                      type="password"
                      value={sudoPassword}
                      onChange={(e) => setSudoPassword(e.target.value)}
                      placeholder="Account password for sudo"
                      className="w-full px-3 py-2.5 bg-zinc-900 border border-zinc-800 rounded-lg text-white placeholder-zinc-600 focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500/40 transition"
                    />
                    <p className="text-xs text-zinc-500 mt-1">Not used for login — only for elevated file operations (sudo). Without this, actions on protected files will be denied.</p>
                  </div>
                </>
              )}

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
        <div className="flex items-center text-sm text-zinc-400">
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
        <div className="relative">
          <button
            onClick={() => { setShowSearch(!showSearch); if (showSearch) { setSearchQuery(""); setSearchResults([]); } }}
            className={`px-2 py-1.5 text-xs rounded-md transition cursor-pointer ${showSearch ? "text-emerald-400 bg-zinc-700" : "text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700"}`}
            title="Search files (Ctrl+F)"
          >
            <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
            </svg>
          </button>
        </div>
        <button
          onClick={() => listFiles(currentPath)}
          className="px-2 py-1.5 text-xs text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-md transition cursor-pointer"
          title="Refresh"
        >
          <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
          </svg>
        </button>
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
        <div className="relative">
          <button
            onClick={(e) => {
              const menu = e.currentTarget.nextElementSibling;
              if (menu) menu.classList.toggle("hidden");
            }}
            className="px-3 py-1.5 text-xs text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-md transition cursor-pointer"
            title="Upload files or folder"
          >
            Upload
          </button>
          <div className="hidden absolute right-0 top-full mt-1 bg-zinc-800 border border-zinc-700 rounded-md shadow-lg z-50 min-w-[120px]">
            <button
              onClick={(e) => { e.currentTarget.parentElement!.classList.add("hidden"); handleUpload(); }}
              className="w-full text-left px-3 py-1.5 text-xs text-zinc-400 hover:text-white hover:bg-zinc-700 rounded-t-md cursor-pointer"
            >
              Files
            </button>
            <button
              onClick={(e) => { e.currentTarget.parentElement!.classList.add("hidden"); handleUploadDirectory(); }}
              className="w-full text-left px-3 py-1.5 text-xs text-zinc-400 hover:text-white hover:bg-zinc-700 rounded-b-md cursor-pointer"
            >
              Folder
            </button>
          </div>
        </div>

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

      {/* Search bar */}
      {showSearch && (
        <div className="relative px-4 py-2.5 bg-zinc-900/80 border-b border-zinc-800">
          <div className="flex items-center gap-2">
            <svg className="w-4 h-4 text-zinc-500 shrink-0" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
            </svg>
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => handleSearch(e.target.value)}
              placeholder={`Search in ${currentPath}  ·  Use /path/ prefix to search elsewhere`}
              autoFocus
              className="flex-1 px-2.5 py-1.5 bg-zinc-800 border border-zinc-700 rounded-md text-sm text-white placeholder-zinc-600 focus:outline-none focus:ring-1 focus:ring-emerald-500/40 transition"
              onKeyDown={(e) => {
                if (e.key === "Escape") { setShowSearch(false); setSearchQuery(""); setSearchResults([]); setSearchSelectedIndex(-1); }
                if (e.key === "Tab" && searchResults.length > 0) {
                  e.preventDefault();
                  const nextIdx = e.shiftKey
                    ? (searchSelectedIndex <= 0 ? searchResults.length - 1 : searchSelectedIndex - 1)
                    : (searchSelectedIndex + 1) % searchResults.length;
                  setSearchSelectedIndex(nextIdx);
                  const result = searchResults[nextIdx];
                  setSearchQuery(searchPrefixRef.current + result.name + (result.is_dir ? "/" : ""));
                }
                if (e.key === "Enter" && searchSelectedIndex >= 0 && searchSelectedIndex < searchResults.length) {
                  e.preventDefault();
                  handleSearchSelect(searchResults[searchSelectedIndex]);
                }
              }}
            />
            {searching && <span className="text-xs text-zinc-500 shrink-0">Searching...</span>}
            <button
              onClick={() => { setShowSearch(false); setSearchQuery(""); setSearchResults([]); }}
              className="px-2 py-1.5 text-xs text-zinc-400 hover:text-white transition cursor-pointer"
            >
              Cancel
            </button>
          </div>
          {searchResults.length > 0 && (
            <div className="absolute left-0 right-0 top-full z-50 mx-4 mt-1 bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl max-h-64 overflow-y-auto">
              {searchResults.map((result, i) => (
                <button
                  key={i}
                  onClick={() => handleSearchSelect(result)}
                  className={`w-full flex items-center gap-2 px-3 py-2 text-left text-sm transition cursor-pointer first:rounded-t-lg last:rounded-b-lg ${i === searchSelectedIndex ? "bg-zinc-600" : "hover:bg-zinc-700"}`}
                >
                  <span className={`shrink-0 ${result.is_dir ? "text-blue-400" : "text-zinc-400"}`}>
                    {result.is_dir ? (
                      <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M3 7v10a2 2 0 002 2h14a2 2 0 002-2V9a2 2 0 00-2-2h-6l-2-2H5a2 2 0 00-2 2z" />
                      </svg>
                    ) : (
                      <svg className="w-4 h-4" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M7 21h10a2 2 0 002-2V9.414a1 1 0 00-.293-.707l-5.414-5.414A1 1 0 0012.586 3H7a2 2 0 00-2 2v14a2 2 0 002 2z" />
                      </svg>
                    )}
                  </span>
                  <span className="text-white truncate">{result.name}</span>
                  <span className="text-xs text-zinc-500 truncate ml-auto">{result.path}</span>
                </button>
              ))}
            </div>
          )}
          {searchQuery && !searching && searchResults.length === 0 && (
            <div className="absolute left-0 right-0 top-full z-50 mx-4 mt-1 bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl px-3 py-2 text-sm text-zinc-500">
              No results found
            </div>
          )}
        </div>
      )}

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
          {authMethod === "password" && (
            <label className="flex items-center gap-1.5 text-xs text-zinc-400 shrink-0 cursor-pointer">
              <input
                type="checkbox"
                checked={savePassword}
                onChange={(e) => setSavePassword(e.target.checked)}
                className="rounded border-zinc-600 accent-emerald-500"
              />
              Save password
            </label>
          )}
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
            <h3 className="text-white font-medium mb-2">
              Delete {confirmDelete.length > 1 ? `${confirmDelete.length} items` : confirmDelete[0].is_dir ? "folder" : "file"}?
            </h3>
            <p className="text-sm text-zinc-400 mb-4">
              {confirmDelete.length > 1 ? (
                <>Are you sure you want to delete <span className="text-zinc-200 font-medium">{confirmDelete.length} items</span>?</>
              ) : (
                <>
                  Are you sure you want to delete <span className="text-zinc-200 font-medium">{confirmDelete[0].name}</span>?
                  {confirmDelete[0].is_dir && " This will delete all contents inside it."}
                </>
              )}
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
        <div
          className={`overflow-auto ${editingFile ? "w-80 shrink-0 border-r border-zinc-800" : "flex-1"}`}
          onContextMenu={(e) => { if ((e.target as HTMLElement).closest("tr")) return; e.preventDefault(); setContextMenu({ x: e.clientX, y: e.clientY, file: null }); }}
        >
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
                  data-folder=".."
                  onClick={navigateUp}
                  className={`cursor-pointer transition group ${dropTarget === ".." ? "bg-blue-500/30 ring-1 ring-blue-500/50" : "hover:bg-zinc-900/50"}`}
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
                    {...(file.is_dir ? { "data-folder": file.name } : {})}
                    onClick={(e) => {
                      if (e.ctrlKey || e.metaKey) {
                        e.preventDefault();
                        setSelectedFiles(prev => {
                          const next = new Set(prev);
                          if (next.has(file.name)) next.delete(file.name); else next.add(file.name);
                          return next;
                        });
                        lastClickedRef.current = file.name;
                      } else if (e.shiftKey && lastClickedRef.current) {
                        e.preventDefault();
                        const names = files.map(f => f.name);
                        const a = names.indexOf(lastClickedRef.current);
                        const b = names.indexOf(file.name);
                        const [start, end] = a < b ? [a, b] : [b, a];
                        setSelectedFiles(new Set(names.slice(start, end + 1)));
                      } else {
                        setSelectedFiles(new Set());
                        lastClickedRef.current = file.name;
                        if (file.is_dir) openFile(file);
                      }
                    }}
                    onDoubleClick={() => {
                      if (!file.is_dir) openFile(file);
                    }}
                    onContextMenu={(e) => {
                      e.preventDefault();
                      if (!selectedFiles.has(file.name)) {
                        setSelectedFiles(new Set());
                      }
                      setContextMenu({ x: e.clientX, y: e.clientY, file });
                    }}
                    className={`border-b border-zinc-800/30 transition ${
                      dropTarget === file.name
                        ? "bg-blue-500/30 ring-1 ring-blue-500/50"
                        : selectedFiles.has(file.name)
                          ? "bg-blue-500/20 hover:bg-blue-500/30"
                          : file.is_dir
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
                      <div className="flex items-center gap-1">
                        <div
                          onMouseDown={(e) => {
                            e.stopPropagation();
                            e.preventDefault();
                            const targets = selectedFiles.size > 1 && selectedFiles.has(file.name)
                              ? files.filter(f => selectedFiles.has(f.name))
                              : [file];
                            internalDragRef.current = targets;
                            setIsDraggingInternal(true);
                          }}
                          className="opacity-0 group-hover:opacity-100 p-1 hover:bg-zinc-700 rounded transition cursor-grab"
                          title={selectedFiles.size > 1 && selectedFiles.has(file.name) ? `Drag ${selectedFiles.size} items` : `Drag ${file.name}`}
                        >
                          <svg className="w-3.5 h-3.5 text-zinc-500 hover:text-blue-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M7 16V4m0 0L3 8m4-4l4 4m6 0v12m0 0l4-4m-4 4l-4-4" />
                          </svg>
                        </div>
                        <div
                          onClick={(e) => {
                            e.stopPropagation();
                            if (selectedFiles.size > 1 && selectedFiles.has(file.name)) {
                              handleMultiDownload(files.filter(f => selectedFiles.has(f.name)));
                            } else {
                              handleDownload(file);
                            }
                          }}
                          className="opacity-0 group-hover:opacity-100 p-1 hover:bg-zinc-700 rounded transition cursor-pointer"
                          title={selectedFiles.size > 1 && selectedFiles.has(file.name) ? `Download ${selectedFiles.size} items` : `Download ${file.name}`}
                        >
                          <svg className="w-3.5 h-3.5 text-zinc-500 hover:text-emerald-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
                          </svg>
                        </div>
                        <div
                          onClick={(e) => {
                            e.stopPropagation();
                            if (selectedFiles.size > 1 && selectedFiles.has(file.name)) {
                              setConfirmDelete(files.filter(f => selectedFiles.has(f.name)));
                            } else {
                              setConfirmDelete([file]);
                            }
                          }}
                          className="opacity-0 group-hover:opacity-100 p-1 hover:bg-zinc-700 rounded transition cursor-pointer"
                          title={selectedFiles.size > 1 && selectedFiles.has(file.name) ? `Delete ${selectedFiles.size} items` : `Delete ${file.name}`}
                        >
                          <svg className="w-3.5 h-3.5 text-zinc-500 hover:text-red-400" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" />
                          </svg>
                        </div>
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

      {/* Transfer progress bars */}
      {transfers.length > 0 && (
        <div className="border-t border-zinc-800 bg-zinc-900/40">
          {transfers.map(transfer => {
            const pct = transfer.totalBytes > 0 ? Math.round(transfer.bytesTransferred / transfer.totalBytes * 100) : 0;
            return (
              <div key={transfer.id} className="flex items-center gap-3 px-4 py-1.5">
                <svg className={`w-3.5 h-3.5 shrink-0 ${transfer.type === 'upload' ? 'text-emerald-400' : transfer.type === 'copy' ? 'text-amber-400' : 'text-blue-400'}`} fill="none" stroke="currentColor" viewBox="0 0 24 24">
                  {transfer.type === 'upload' ? (
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-8l-4-4m0 0L8 8m4-4v12" />
                  ) : transfer.type === 'copy' ? (
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M8 7v8a2 2 0 002 2h6M8 7V5a2 2 0 012-2h4.586a1 1 0 01.707.293l4.414 4.414a1 1 0 01.293.707V15a2 2 0 01-2 2h-2M8 7H6a2 2 0 00-2 2v10a2 2 0 002 2h8a2 2 0 002-2v-2" />
                  ) : (
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4" />
                  )}
                </svg>
                <button
                  onClick={async () => {
                    try { await invoke("cancel_transfer", { transferId: transfer.id }); } catch {}
                    setTransfers(prev => prev.filter(t => t.id !== transfer.id));
                  }}
                  className="text-zinc-500 hover:text-red-400 transition-colors shrink-0"
                  title="Cancel transfer"
                >
                  <svg className="w-3.5 h-3.5" fill="none" stroke="currentColor" viewBox="0 0 24 24">
                    <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M6 18L18 6M6 6l12 12" />
                  </svg>
                </button>
                <span className="text-xs text-zinc-300 truncate w-36">{transfer.fileName}</span>
                <div className="flex-1 h-1.5 bg-zinc-800 rounded-full overflow-hidden">
                  <div
                    className={`h-full rounded-full transition-all duration-150 ${transfer.type === 'upload' ? 'bg-emerald-500' : transfer.type === 'copy' ? 'bg-amber-500' : 'bg-blue-500'}`}
                    style={{ width: `${pct}%` }}
                  />
                </div>
                <span className="text-xs text-zinc-500 w-10 text-right tabular-nums">{pct}%</span>
              </div>
            );
          })}
        </div>
      )}

      {/* Status bar */}
      <div className="px-4 py-2 bg-zinc-900/30 border-t border-zinc-800 text-xs text-zinc-500 flex items-center justify-between">
        <span>{selectedFiles.size > 0 ? `${selectedFiles.size} selected · ` : ""}{files.length} items</span>
        <span>{editingFile ? editingFile : currentPath}</span>
      </div>

      {/* Context menu */}
      {contextMenu && (
        <div
          ref={(el) => {
            if (!el) return;
            const rect = el.getBoundingClientRect();
            let x = contextMenu.x;
            let y = contextMenu.y;
            if (x + rect.width > window.innerWidth) x = window.innerWidth - rect.width - 4;
            if (y + rect.height > window.innerHeight) y = window.innerHeight - rect.height - 4;
            if (x < 0) x = 4;
            if (y < 0) y = 4;
            if (el.style.left !== `${x}px` || el.style.top !== `${y}px`) {
              el.style.left = `${x}px`;
              el.style.top = `${y}px`;
            }
          }}
          className="fixed z-50 bg-zinc-800 border border-zinc-700 rounded-lg shadow-xl py-1 min-w-[160px]"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          onClick={() => setContextMenu(null)}
          onContextMenu={(e) => e.preventDefault()}
        >
          <button
            onClick={() => listFiles(currentPath)}
            className="w-full text-left px-3 py-1.5 text-sm text-zinc-300 hover:bg-zinc-700 hover:text-white transition"
          >
            Refresh
          </button>
          {contextMenu.file && (
            <>
              <div className="border-t border-zinc-700 my-1" />
              <button
                onClick={() => {
                  if (selectedFiles.size > 1 && contextMenu.file && selectedFiles.has(contextMenu.file.name)) {
                    handleMultiDownload(files.filter(f => selectedFiles.has(f.name)));
                  } else if (contextMenu.file) {
                    handleDownload(contextMenu.file);
                  }
                }}
                className="w-full text-left px-3 py-1.5 text-sm text-zinc-300 hover:bg-zinc-700 hover:text-white transition"
              >
                {selectedFiles.size > 1 && contextMenu.file && selectedFiles.has(contextMenu.file.name)
                  ? `Download ${selectedFiles.size} items`
                  : "Download"}
              </button>
              <button
                onClick={() => {
                  if (contextMenu.file) {
                    if (selectedFiles.size > 1 && selectedFiles.has(contextMenu.file.name)) {
                      setClipboard(files.filter(f => selectedFiles.has(f.name)).map(f => ({
                        path: currentPath === "/" ? `/${f.name}` : `${currentPath}/${f.name}`,
                        name: f.name,
                        is_dir: f.is_dir,
                      })));
                    } else {
                      const path = currentPath === "/" ? `/${contextMenu.file.name}` : `${currentPath}/${contextMenu.file.name}`;
                      setClipboard([{ path, name: contextMenu.file.name, is_dir: contextMenu.file.is_dir }]);
                    }
                  }
                }}
                className="w-full text-left px-3 py-1.5 text-sm text-zinc-300 hover:bg-zinc-700 hover:text-white transition"
              >
                {selectedFiles.size > 1 && contextMenu.file && selectedFiles.has(contextMenu.file.name)
                  ? `Copy ${selectedFiles.size} items`
                  : "Copy"}
              </button>
            </>
          )}
          {clipboard && (
            <>
              <div className="border-t border-zinc-700 my-1" />
              <button
                onClick={handlePaste}
                className="w-full text-left px-3 py-1.5 text-sm text-zinc-300 hover:bg-zinc-700 hover:text-white transition"
              >
                Paste {clipboard.length > 1 ? `${clipboard.length} items` : `"${clipboard[0].name}"`}
              </button>
            </>
          )}
          <div className="border-t border-zinc-700 my-1" />
          <button
            onClick={() => {
              setShowNewFileInput(true);
              setNewFileName("");
            }}
            className="w-full text-left px-3 py-1.5 text-sm text-zinc-300 hover:bg-zinc-700 hover:text-white transition"
          >
            New File/Folder
          </button>
        </div>
      )}

      {/* Drag-over overlay */}
      {dragOverWindow && (
        <div className="fixed inset-0 bg-emerald-500/5 border-2 border-dashed border-emerald-500/40 flex items-center justify-center z-40 pointer-events-none">
          <div className="text-center bg-zinc-900/90 px-8 py-6 rounded-2xl border border-emerald-500/30">
            <svg className="w-10 h-10 text-emerald-400 mx-auto mb-3" fill="none" stroke="currentColor" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={1.5} d="M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-8l-4-4m0 0L8 8m4-4v12" />
            </svg>
            <p className="text-emerald-400 font-medium">Drop files to upload</p>
            <p className="text-emerald-400/50 text-xs mt-1">Uploading to {currentPath}</p>
          </div>
        </div>
      )}

      {/* Active drag-out indicator */}
      {activeDrag && (
        <div className="fixed inset-0 bg-blue-500/5 border-2 border-dashed border-blue-500/40 flex items-end justify-center z-40 pointer-events-none pb-8">
          <div className="text-center bg-zinc-900/90 px-8 py-4 rounded-2xl border border-blue-500/30 animate-pulse">
            <p className="text-blue-400 font-medium">Drag to a folder to save "{activeDrag}"</p>
            <p className="text-blue-400/50 text-xs mt-1">Drop in a folder in File Explorer to save</p>
          </div>
        </div>
      )}
    </div>
  );
}

export default App;
