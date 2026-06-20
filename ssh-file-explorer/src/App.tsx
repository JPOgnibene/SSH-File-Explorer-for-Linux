import { useState, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

interface FileEntry {
  name: string;
  is_dir: boolean;
  size: number;
  modified: string;
  permissions: string;
}

function formatSize(bytes: number): string {
  if (bytes === 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.floor(Math.log(bytes) / Math.log(1024));
  return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
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

  const [currentPath, setCurrentPath] = useState("/");
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState(false);

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
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  const handleConnect = async (e: React.FormEvent) => {
    e.preventDefault();
    setConnecting(true);
    setError("");
    try {
      await invoke("ssh_connect", {
        host,
        port: parseInt(port),
        username,
        password,
      });
      setConnected(true);
      await listFiles("/");
    } catch (e) {
      setError(String(e));
    } finally {
      setConnecting(false);
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
  };

  const navigateTo = (entry: FileEntry) => {
    if (!entry.is_dir) return;
    const newPath =
      currentPath === "/"
        ? `/${entry.name}`
        : `${currentPath}/${entry.name}`;
    listFiles(newPath);
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
        <div className="w-full max-w-md">
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

        <button
          onClick={handleDisconnect}
          className="px-3 py-1.5 text-xs text-zinc-400 hover:text-white bg-zinc-800 hover:bg-zinc-700 rounded-md transition cursor-pointer"
        >
          Disconnect
        </button>
      </div>

      {error && (
        <div className="mx-4 mt-3 px-3 py-2 bg-red-500/10 border border-red-500/20 rounded-lg text-red-400 text-sm">
          {error}
        </div>
      )}

      {/* File list */}
      <div className="flex-1 overflow-auto">
        <table className="w-full text-sm">
          <thead>
            <tr className="text-xs text-zinc-500 uppercase tracking-wider border-b border-zinc-800/50">
              <th className="text-left py-2.5 px-4 font-medium">Name</th>
              <th className="text-right py-2.5 px-4 font-medium w-28">Size</th>
              <th className="text-left py-2.5 px-4 font-medium w-36">Modified</th>
              <th className="text-left py-2.5 px-4 font-medium w-28">Permissions</th>
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
                <td />
                <td />
                <td />
              </tr>
            )}
            {loading ? (
              <tr>
                <td colSpan={4} className="py-12 text-center text-zinc-500">
                  Loading...
                </td>
              </tr>
            ) : (
              files.map((file) => (
                <tr
                  key={file.name}
                  onClick={() => navigateTo(file)}
                  className={`border-b border-zinc-800/30 transition ${
                    file.is_dir
                      ? "hover:bg-zinc-900/50 cursor-pointer"
                      : "hover:bg-zinc-900/30"
                  } group`}
                >
                  <td className="py-2 px-4 flex items-center gap-2.5">
                    <FileIcon isDir={file.is_dir} />
                    <span className="text-zinc-300 group-hover:text-white transition truncate">
                      {file.name}
                    </span>
                  </td>
                  <td className="py-2 px-4 text-right text-zinc-500 tabular-nums">
                    {file.is_dir ? "—" : formatSize(file.size)}
                  </td>
                  <td className="py-2 px-4 text-zinc-500">{file.modified}</td>
                  <td className="py-2 px-4 text-zinc-600 font-mono text-xs">
                    {file.permissions}
                  </td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>

      {/* Status bar */}
      <div className="px-4 py-2 bg-zinc-900/30 border-t border-zinc-800 text-xs text-zinc-500 flex items-center justify-between">
        <span>{files.length} items</span>
        <span>{currentPath}</span>
      </div>
    </div>
  );
}

export default App;
