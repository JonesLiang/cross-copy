import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  ArrowDownLeft,
  ArrowLeft,
  ArrowUpRight,
  CaretDown,
  Check,
  Clipboard,
  Copy,
  Desktop,
  DownloadSimple,
  File,
  FilePlus,
  FolderOpen,
  FolderPlus,
  FileText,
  Gear,
  Keyboard,
  Lightning,
  Link,
  MouseSimple,
  PencilSimple,
  Plus,
  HardDrives,
  Info,
  FloppyDisk,
  ArrowsClockwise,
  ShieldCheck,
  Trash,
  WarningCircle,
  X
} from "@phosphor-icons/react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type {
  DisplayInfo,
  FsEntry,
  FsProperties,
  FsRequest,
  FsResponse,
  ScreenPosition,
  UiState
} from "./types";
import "./styles.css";

const crosscopy = {
  getState: () => invoke<UiState>("get_state"),
  beginPairing: () => invoke<void>("begin_pairing"),
  cancelPairing: () => invoke<void>("cancel_pairing"),
  submitPairingCode: (code: string) =>
    invoke<void>("submit_pairing_code", { code }),
  setSyncEnabled: (value: boolean) =>
    invoke<void>("set_sync_enabled", { value }),
  setLaunchAtLogin: (value: boolean) =>
    invoke<void>("set_launch_at_login", { value }),
  unpair: (peerId: string) => invoke<void>("unpair", { peerId }),
  exportDiagnostics: () => invoke<string>("export_diagnostics"),
  getUpdateEnvironment: () =>
    invoke<UpdateEnvironment>("get_update_environment"),
  logUpdateEvent: (level: string, event: string, detail: string) =>
    invoke<void>("log_update_event", { level, event, detail }),
  wakeNetwork: () => invoke<void>("wake_network"),
  openInputPermissions: () => invoke<void>("open_input_permissions"),
  setShortcuts: (copy: string, paste: string, mouse: string) =>
    invoke<void>("set_shortcuts", { copy, paste, mouse }),
  setMouseShareEnabled: (value: boolean) =>
    invoke<void>("set_mouse_share_enabled", { value }),
  setMouseExtremePerformance: (value: boolean) =>
    invoke<void>("set_mouse_extreme_performance", { value }),
  setMousePosition: (position: ScreenPosition) =>
    invoke<void>("set_mouse_position", { position }),
  setPeerScreenPosition: (peerId: string, position: ScreenPosition) =>
    invoke<void>("set_peer_screen_position", { peerId, position }),
  setPeerPermissions: (
    peerId: string,
    clipboardAllowed: boolean,
    mouseAllowed: boolean,
    filesystemAllowed: boolean
  ) =>
    invoke<void>("set_peer_permissions", {
      peerId,
      clipboardAllowed,
      mouseAllowed,
      filesystemAllowed
    }),
  filesystemRequest: (peerId: string, request: FsRequest) =>
    invoke<FsResponse>("filesystem_request", { peerId, request }),
  filesystemDownload: (peerId: string, paths: string[]) =>
    invoke<string>("filesystem_download", { peerId, paths }),
  filesystemUpload: (
    peerId: string,
    localPaths: string[],
    targetDir: string
  ) =>
    invoke<void>("filesystem_upload", { peerId, localPaths, targetDir }),
  filesystemUploadClipboard: (peerId: string, targetDir: string) =>
    invoke<void>("filesystem_upload_clipboard", { peerId, targetDir }),
  filesystemPrepareDrag: (peerId: string, paths: string[]) =>
    invoke<string[]>("filesystem_prepare_drag", { peerId, paths }),
  filesystemStartDrag: (paths: string[]) =>
    invoke<void>("filesystem_start_drag", { paths }),
  setPeerMouseDpi: (peerId: string, dpi: number) =>
    invoke<void>("set_peer_mouse_dpi", { peerId, dpi }),
  switchMouseToScreen: (screenNumber: number) =>
    invoke<void>("switch_mouse_to_screen", { screenNumber })
};

const EMPTY_STATE: UiState = {
  deviceName: "",
  displays: [],
  syncEnabled: true,
  launchAtLogin: false,
  copyShortcut: "Ctrl+Shift+C",
  pasteShortcut: "Ctrl+Shift+V",
  mouseShareEnabled: false,
  mouseExtremePerformance: false,
  mouseShortcut: "Ctrl+Shift+M",
  mousePosition: "right",
  mouseLatencyMs: null,
  mouseSessionActive: false,
  mouseListenerStarted: false,
  hasPendingClipboard: false,
  pairingCode: null,
  pairingExpiresAt: null,
  peers: [],
  activity: [],
  transfer: null
};

type PairMode = "choose" | "show" | "enter" | null;
type UpdateEnvironment = {
  ready: boolean;
  reason: string | null;
};
type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "available"; version: string }
  | { kind: "downloading"; version: string; progress: number | null }
  | { kind: "installing"; version: string }
  | { kind: "current" }
  | { kind: "error"; message: string };

function App(): React.JSX.Element {
  const [state, setState] = useState<UiState>(EMPTY_STATE);
  const [ready, setReady] = useState(false);
  const [pairMode, setPairMode] = useState<PairMode>(null);
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [diagnosticsMessage, setDiagnosticsMessage] = useState("");
  const [appVersion, setAppVersion] = useState("");
  const [updateState, setUpdateState] = useState<UpdateState>({ kind: "idle" });
  const availableUpdate = useRef<Update | null>(null);
  const [view, setView] = useState<
    "clipboard" | "filesystem" | "mouse" | "settings"
  >(
    "clipboard"
  );

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void crosscopy
      .getState()
      .then(setState)
      .catch(() => setState(EMPTY_STATE))
      .finally(() => setReady(true));
    void getVersion().then(setAppVersion).catch(() => setAppVersion(""));
    const updateTimer = window.setTimeout(() => {
      void checkForUpdate(false);
    }, 1800);
    void listen<UiState>("state", (event) => setState(event.payload))
      .then((stop) => {
        unlisten = stop;
      })
      .catch(() => undefined);
    return () => {
      window.clearTimeout(updateTimer);
      unlisten?.();
      void availableUpdate.current?.close();
    };
  }, []);

  useEffect(() => {
    if (pairMode === "show" && !state.pairingCode) setPairMode(null);
  }, [pairMode, state.pairingCode]);

  const onlineCount = state.peers.filter((peer) => peer.online).length;
  const headline = useMemo(() => {
    if (!state.syncEnabled) return "同步已暂停";
    if (onlineCount > 0) return "剪贴板已连接";
    if (state.peers.length > 0) return "等待另一台设备上线";
    return "连接你的另一台电脑";
  }, [onlineCount, state.peers.length, state.syncEnabled]);

  async function beginPairing(): Promise<void> {
    setError("");
    await crosscopy.beginPairing();
    setPairMode("show");
  }

  async function submitCode(event: React.FormEvent): Promise<void> {
    event.preventDefault();
    if (code.length !== 6) return;
    setSubmitting(true);
    setError("");
    try {
      await crosscopy.submitPairingCode(code);
      setPairMode(null);
      setCode("");
    } catch (reason) {
      setError(
        typeof reason === "string"
          ? reason
          : reason instanceof Error
            ? reason.message
            : "配对失败，请重试"
      );
    } finally {
      setSubmitting(false);
    }
  }

  function closePairing(): void {
    if (pairMode === "show") void crosscopy.cancelPairing();
    setPairMode(null);
    setCode("");
    setError("");
  }

  async function exportDiagnostics(): Promise<void> {
    setDiagnosticsMessage("正在生成");
    try {
      const path = await crosscopy.exportDiagnostics();
      setDiagnosticsMessage(`已导出到 ${path}`);
    } catch (reason) {
      setDiagnosticsMessage(
        typeof reason === "string" ? reason : "诊断日志导出失败"
      );
    }
  }

  async function checkForUpdate(showCurrent: boolean): Promise<void> {
    setUpdateState({ kind: "checking" });
    try {
      const environment = await crosscopy.getUpdateEnvironment();
      if (!environment.ready) {
        const message = environment.reason ?? "当前安装位置不支持自动更新";
        setUpdateState({ kind: "error", message });
        return;
      }
      void crosscopy.logUpdateEvent("info", "check_started", "source=ui");
      const update = await check({ timeout: 12_000 });
      if (!update) {
        void crosscopy.logUpdateEvent("info", "check_current", "available=false");
        setUpdateState(showCurrent ? { kind: "current" } : { kind: "idle" });
        return;
      }
      await availableUpdate.current?.close();
      availableUpdate.current = update;
      void crosscopy.logUpdateEvent(
        "info",
        "check_available",
        `version=${update.version}`
      );
      setUpdateState({ kind: "available", version: update.version });
    } catch (reason) {
      const message = updateErrorMessage(reason, "连接更新服务器失败");
      void crosscopy.logUpdateEvent("error", "check_failed", message);
      setUpdateState(
        showCurrent ? { kind: "error", message } : { kind: "idle" }
      );
    }
  }

  async function installUpdate(): Promise<void> {
    const initialUpdate = availableUpdate.current;
    if (!initialUpdate) {
      await checkForUpdate(true);
      return;
    }
    let update: Update = initialUpdate;

    let received = 0;
    let total: number | undefined;
    setUpdateState({
      kind: "downloading",
      version: update.version,
      progress: null
    });
    void crosscopy.logUpdateEvent(
      "info",
      "download_started",
      `version=${update.version}`
    );
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      const activeUpdate = update;
      received = 0;
      total = undefined;
      try {
        await activeUpdate.downloadAndInstall((event) => {
          if (event.event === "Started") {
            total = event.data.contentLength;
          } else if (event.event === "Progress") {
            received += event.data.chunkLength;
            const progress =
              total && total > 0
                ? Math.min(100, (received / total) * 100)
                : null;
            setUpdateState({
              kind: "downloading",
              version: activeUpdate.version,
              progress
            });
          } else {
            setUpdateState({
              kind: "installing",
              version: activeUpdate.version
            });
          }
        });
        void crosscopy.logUpdateEvent(
          "info",
          "install_completed",
          `version=${activeUpdate.version}`
        );
        try {
          await relaunch();
        } catch (reason) {
          const message = updateErrorMessage(
            reason,
            `v${activeUpdate.version} 已安装，请完全退出后从“应用程序”重新打开`
          );
          void crosscopy.logUpdateEvent("error", "relaunch_failed", message);
          setUpdateState({ kind: "error", message });
        }
        return;
      } catch (reason) {
        const message = updateErrorMessage(reason, "更新包下载或安装失败");
        void crosscopy.logUpdateEvent(
          attempt === 3 ? "error" : "warn",
          "download_install_failed",
          `attempt=${attempt} error=${message}`
        );
        if (attempt === 3) {
          setUpdateState({ kind: "error", message });
          return;
        }
        await activeUpdate.close().catch(() => undefined);
        const retryUpdate = await check({ timeout: 15_000 }).catch(() => null);
        if (!retryUpdate) {
          setUpdateState({ kind: "error", message });
          return;
        }
        update = retryUpdate;
        availableUpdate.current = retryUpdate;
        setUpdateState({
          kind: "downloading",
          version: update.version,
          progress: null
        });
        await new Promise((resolve) => window.setTimeout(resolve, attempt * 1200));
      }
    }
  }

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <Copy size={20} weight="bold" />
          </div>
          <span>CrossCopy</span>
        </div>

        <nav aria-label="主导航">
          <button
            className={`nav-item ${view === "clipboard" ? "active" : ""}`}
            type="button"
            onClick={() => setView("clipboard")}
          >
            <Clipboard size={18} />
            剪贴板
          </button>
          <button
            className={`nav-item ${view === "filesystem" ? "active" : ""}`}
            type="button"
            onClick={() => setView("filesystem")}
          >
            <HardDrives size={18} />
            文件系统
          </button>
          <button
            className={`nav-item ${view === "mouse" ? "active" : ""}`}
            type="button"
            onClick={() => setView("mouse")}
          >
            <MouseSimple size={18} />
            键鼠共享
          </button>
          <button
            className={`nav-item ${view === "settings" ? "active" : ""}`}
            type="button"
            onClick={() => setView("settings")}
          >
            <Gear size={18} />
            设置
          </button>
        </nav>

        <div className="sidebar-status">
          <div className="sidebar-device">
            <span className="sidebar-device-icon">
              <Desktop size={16} />
            </span>
            <span>
              <strong>{state.deviceName || "正在读取本机"}</strong>
              <small>CrossCopy v{appVersion || "-"}</small>
            </span>
            <i title="本机服务正在运行" />
          </div>
          <UpdateControl
            state={updateState}
            onCheck={() => void checkForUpdate(true)}
            onInstall={() => void installUpdate()}
          />
        </div>
      </aside>

      <section className="content">
        <header className="topbar">
          <div>
            <h1>
              {view === "clipboard"
                ? "剪贴板"
                : view === "filesystem"
                  ? "文件系统"
                : view === "mouse"
                  ? "键鼠共享"
                  : "设置"}
            </h1>
            <p>
              {view === "clipboard"
                ? "使用专用快捷键发送和粘贴，不影响普通剪贴板"
                : view === "filesystem"
                  ? "直接浏览和管理已授权电脑上的文件"
                : view === "mouse"
                  ? "跨越屏幕控制另一台电脑的鼠标与键盘"
                : "管理设备权限、后台启动、系统权限和诊断"}
            </p>
          </div>
          {view === "clipboard" && (
            <button
              className="primary-button"
              type="button"
              onClick={() => setPairMode("choose")}
            >
              <Plus size={17} weight="bold" />
              添加电脑
            </button>
          )}
        </header>

        {view === "settings" ? (
          <SettingsPanel
            state={state}
            diagnosticsMessage={diagnosticsMessage}
            onDiagnostics={exportDiagnostics}
          />
        ) : view === "mouse" ? (
          <MousePanel state={state} />
        ) : view === "filesystem" ? (
          <FilesystemPanel state={state} />
        ) : !ready ? (
          <LoadingState />
        ) : (
          <>
            <section className="connection-panel">
              <div
                className={`connection-visual ${
                  onlineCount > 0 && state.syncEnabled ? "is-online" : ""
                }`}
              >
                <div className="computer source">
                  <Desktop size={31} weight="light" />
                </div>
                <div className="signal-lines" aria-hidden="true">
                  <span />
                  <span />
                  <span />
                </div>
                <div className="computer target">
                  <Desktop size={31} weight="light" />
                </div>
              </div>
              <div className="connection-copy">
                <h2>{headline}</h2>
                <p>
                  {state.peers.length === 0
                    ? "首次使用只需输入一次验证码，之后会在同一局域网自动连接。"
                    : `${onlineCount} 台在线，共 ${state.peers.length} 台已配对。`}
                </p>
              </div>
              <label className="switch-row">
                <span>{state.syncEnabled ? "同步中" : "已暂停"}</span>
                <input
                  type="checkbox"
                  checked={state.syncEnabled}
                  onChange={(event) =>
                    void crosscopy.setSyncEnabled(event.target.checked)
                  }
                />
                <i aria-hidden="true" />
              </label>
            </section>

            <ShortcutSettings mode="clipboard" state={state} />

            <section className="section-block">
              <div className="section-heading">
                <h2>已配对电脑</h2>
                <span>{state.peers.length}</span>
              </div>
              {state.peers.length === 0 ? (
                <div className="empty-row">
                  <Link size={20} />
                  <span>还没有配对设备</span>
                  <button type="button" onClick={() => setPairMode("choose")}>
                    立即添加
                  </button>
                </div>
              ) : (
                <div className="peer-list">
                  {state.peers.map((peer) => (
                    <div className="peer-row" key={peer.id}>
                      <div className="peer-icon">
                        <Desktop size={20} />
                      </div>
                      <div className="peer-name">
                        <strong>{peer.name}</strong>
                        <span className={peer.online ? "online" : ""}>
                          {peer.online ? "已连接" : "离线"}
                        </span>
                      </div>
                      {!peer.online && (
                        <button
                          className="wake-button"
                          type="button"
                          onClick={() => void crosscopy.wakeNetwork()}
                        >
                          <Lightning size={15} />
                          连接
                        </button>
                      )}
                      <button
                        className="icon-button danger"
                        aria-label={`移除 ${peer.name}`}
                        type="button"
                        onClick={() => void crosscopy.unpair(peer.id)}
                      >
                        <Trash size={17} />
                      </button>
                    </div>
                  ))}
                </div>
              )}
            </section>

            <section className="section-block activity-section">
              <div className="section-heading">
                <h2>最近活动</h2>
                <span>仅保存在本机</span>
              </div>
              {state.activity.length === 0 ? (
                <div className="empty-activity">
                  <Clipboard size={26} weight="light" />
                  <p>复制内容后，传输记录会显示在这里</p>
                </div>
              ) : (
                <div className="activity-list">
                  {state.activity.map((item) => (
                    <div className="activity-row" key={item.id}>
                      <div className={`activity-icon ${item.status}`}>
                        {item.status === "error" ? (
                          <WarningCircle size={18} />
                        ) : item.direction === "sent" ? (
                          <ArrowUpRight size={18} />
                        ) : item.direction === "received" ? (
                          <ArrowDownLeft size={18} />
                        ) : (
                          <Check size={18} />
                        )}
                      </div>
                      <div>
                        <strong>{item.label}</strong>
                        <span>{item.detail}</span>
                      </div>
                      <time>{formatTime(item.createdAt)}</time>
                    </div>
                  ))}
                </div>
              )}
            </section>

          </>
        )}
      </section>

      {pairMode && (
        <PairDialog
          mode={pairMode}
          code={code}
          pairingCode={state.pairingCode}
          error={error}
          submitting={submitting}
          onClose={closePairing}
          onChoose={setPairMode}
          onBegin={beginPairing}
          onCode={setCode}
          onSubmit={submitCode}
        />
      )}
    </main>
  );
}

function UpdateControl(props: {
  state: UpdateState;
  onCheck(): void;
  onInstall(): void;
}): React.JSX.Element {
  const state = props.state;
  if (state.kind === "idle") {
    return (
      <button className="sidebar-update" type="button" onClick={props.onCheck}>
        <DownloadSimple size={15} />
        <span>
          <strong>检查更新</strong>
          <small>获取最新稳定版本</small>
        </span>
      </button>
    );
  }
  if (state.kind === "checking") {
    return (
      <div className="sidebar-update is-static">
        <Lightning size={15} />
        <span>
          <strong>正在检查</strong>
          <small>连接更新服务器…</small>
        </span>
      </div>
    );
  }
  if (state.kind === "current") {
    return (
      <button className="sidebar-update" type="button" onClick={props.onCheck}>
        <Check size={15} />
        <span>
          <strong>已是最新版本</strong>
          <small>点击重新检查</small>
        </span>
      </button>
    );
  }
  if (state.kind === "error") {
    return (
      <button
        className="sidebar-update update-error"
        type="button"
        onClick={props.onCheck}
      >
        <WarningCircle size={15} />
        <span>
          <strong>更新失败</strong>
          <small title={state.message}>{state.message}</small>
        </span>
      </button>
    );
  }
  if (state.kind === "available") {
    return (
      <button
        className="sidebar-update has-update"
        type="button"
        onClick={props.onInstall}
      >
        <DownloadSimple size={16} weight="bold" />
        <span>
          <strong>更新到 v{state.version}</strong>
          <small>下载后自动安装并重启</small>
        </span>
      </button>
    );
  }

  const label =
    state.kind === "installing"
      ? "正在安装更新…"
      : state.progress === null
        ? "正在下载更新…"
        : `正在下载 ${Math.round(state.progress)}%`;
  return (
    <div className="sidebar-update is-static is-progress">
      <DownloadSimple size={16} />
      <span>
        <strong>{label}</strong>
        <small>v{state.version}</small>
      </span>
    </div>
  );
}

function updateErrorMessage(reason: unknown, fallback: string): string {
  if (typeof reason === "string" && reason.trim()) return reason;
  if (reason instanceof Error && reason.message.trim()) return reason.message;
  try {
    const serialized = JSON.stringify(reason);
    if (serialized && serialized !== "{}") return serialized;
  } catch {
    // The updater can return non-serializable native errors.
  }
  return fallback;
}

function PairDialog(props: {
  mode: Exclude<PairMode, null>;
  code: string;
  pairingCode: string | null;
  error: string;
  submitting: boolean;
  onClose(): void;
  onChoose(mode: "show" | "enter"): void;
  onBegin(): Promise<void>;
  onCode(value: string): void;
  onSubmit(event: React.FormEvent): Promise<void>;
}): React.JSX.Element {
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={props.onClose}>
      <section
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="pair-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <button
          className="modal-close"
          type="button"
          aria-label="关闭"
          onClick={props.onClose}
        >
          <X size={18} />
        </button>

        {props.mode === "choose" && (
          <>
            <div className="modal-icon">
              <Link size={25} />
            </div>
            <h2 id="pair-title">添加另一台电脑</h2>
            <p>确保两台电脑连接到同一个局域网。</p>
            <div className="pair-choices">
              <button type="button" onClick={() => void props.onBegin()}>
                <span>
                  <strong>这台电脑显示验证码</strong>
                  <small>在另一台电脑输入</small>
                </span>
                <Copy size={20} />
              </button>
              <button type="button" onClick={() => props.onChoose("enter")}>
                <span>
                  <strong>这台电脑输入验证码</strong>
                  <small>使用另一台电脑显示的代码</small>
                </span>
                <Link size={20} />
              </button>
            </div>
          </>
        )}

        {props.mode === "show" && (
          <>
            <div className="modal-icon">
              <ShieldCheck size={26} />
            </div>
            <h2 id="pair-title">在另一台电脑输入</h2>
            <p>验证码将在 2 分钟后失效。</p>
            <div className="display-code" aria-label="配对验证码">
              {(props.pairingCode ?? "••••••").split("").map((digit, index) => (
                <span key={`${digit}-${index}`}>{digit}</span>
              ))}
            </div>
            <div className="waiting-line">
              <i />
              正在等待另一台电脑
            </div>
          </>
        )}

        {props.mode === "enter" && (
          <form onSubmit={(event) => void props.onSubmit(event)}>
            <div className="modal-icon">
              <Link size={25} />
            </div>
            <h2 id="pair-title">输入 6 位验证码</h2>
            <p>验证码显示在另一台电脑的 CrossCopy 中。</p>
            <label className="code-input">
              <span>配对验证码</span>
              <input
                autoFocus
                inputMode="numeric"
                autoComplete="one-time-code"
                maxLength={6}
                value={props.code}
                placeholder="000000"
                onChange={(event) =>
                  props.onCode(event.target.value.replace(/\D/g, ""))
                }
              />
            </label>
            {props.error && <div className="form-error">{props.error}</div>}
            <button
              className="primary-button modal-submit"
              type="submit"
              disabled={props.code.length !== 6 || props.submitting}
            >
              {props.submitting ? "正在配对" : "连接电脑"}
            </button>
          </form>
        )}
      </section>
    </div>
  );
}

const SCREEN_OFFSETS: Record<ScreenPosition, { x: number; y: number }> = {
  left: { x: -150, y: 0 },
  right: { x: 150, y: 0 },
  up: { x: 0, y: -96 },
  down: { x: 0, y: 96 }
};

function PeerDisplayGlyph(props: {
  displays: DisplayInfo[];
}): React.JSX.Element {
  const displays =
    props.displays.length > 0
      ? props.displays
      : [
          {
            id: "unknown",
            name: "屏幕",
            x: 0,
            y: 0,
            width: 16,
            height: 9,
            primary: true,
            mirroredCount: 1
          }
        ];
  const bounds = displays.reduce(
    (value, display) => ({
      left: Math.min(value.left, display.x),
      top: Math.min(value.top, display.y),
      right: Math.max(value.right, display.x + display.width),
      bottom: Math.max(value.bottom, display.y + display.height)
    }),
    {
      left: Number.POSITIVE_INFINITY,
      top: Number.POSITIVE_INFINITY,
      right: Number.NEGATIVE_INFINITY,
      bottom: Number.NEGATIVE_INFINITY
    }
  );
  const scale = Math.min(
    46 / Math.max(1, bounds.right - bounds.left),
    26 / Math.max(1, bounds.bottom - bounds.top)
  );
  const offsetX = (46 - (bounds.right - bounds.left) * scale) / 2;
  const offsetY = (26 - (bounds.bottom - bounds.top) * scale) / 2;
  return (
    <span className="peer-monitor-map">
      {displays.map((display) => (
        <i
          className={display.primary ? "primary" : ""}
          key={display.id}
          style={{
            left: offsetX + (display.x - bounds.left) * scale,
            top: offsetY + (display.y - bounds.top) * scale,
            width: Math.max(10, display.width * scale),
            height: Math.max(7, display.height * scale)
          }}
          title={display.name}
        />
      ))}
      {displays.some((display) => display.mirroredCount > 1) && (
        <b>镜像</b>
      )}
    </span>
  );
}

type RemoteEditor = {
  path: string;
  name: string;
  content: string;
  modifiedAt: number | null;
};

type FileContextMenu = {
  x: number;
  y: number;
  entry: FsEntry | null;
};

type FileNameDialogState = {
  mode: "file" | "folder" | "rename";
  entry: FsEntry | null;
};

type RemoteFileClipboard = {
  peerId: string;
  paths: string[];
};

function DeviceSelect(props: {
  value: string;
  options: Array<{ value: string; label: string; detail: string }>;
  onChange(value: string): void;
}): React.JSX.Element {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const selected =
    props.options.find((option) => option.value === props.value) ??
    props.options[0];

  useEffect(() => {
    function close(event: MouseEvent): void {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    }
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, []);

  return (
    <div className="device-select" ref={root}>
      <button
        className="device-select-trigger"
        type="button"
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((value) => !value)}
      >
        <span className="device-select-icon">
          <Desktop size={16} />
        </span>
        <span>
          <strong>{selected?.label ?? "选择电脑"}</strong>
          <small>{selected?.detail ?? "没有可用设备"}</small>
        </span>
        <CaretDown size={14} />
      </button>
      {open && (
        <div className="device-select-menu" role="listbox">
          {props.options.map((option) => (
            <button
              className={option.value === selected?.value ? "selected" : ""}
              type="button"
              role="option"
              aria-selected={option.value === selected?.value}
              key={option.value}
              onClick={() => {
                props.onChange(option.value);
                setOpen(false);
              }}
            >
              <span>
                <strong>{option.label}</strong>
                <small>{option.detail}</small>
              </span>
              {option.value === selected?.value && <Check size={15} weight="bold" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function FilesystemPanel(props: { state: UiState }): React.JSX.Element {
  const availablePeers = props.state.peers.filter(
    (peer) => peer.online && peer.filesystemAllowed
  );
  const [peerId, setPeerId] = useState(availablePeers[0]?.id ?? "");
  const [path, setPath] = useState<string | null>(null);
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [selectedPath, setSelectedPath] = useState("");
  const [loading, setLoading] = useState(false);
  const [message, setMessage] = useState("");
  const [editor, setEditor] = useState<RemoteEditor | null>(null);
  const [saving, setSaving] = useState(false);
  const [externalDragging, setExternalDragging] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [contextMenu, setContextMenu] = useState<FileContextMenu | null>(null);
  const [nameDialog, setNameDialog] = useState<FileNameDialogState | null>(null);
  const [deleteEntry, setDeleteEntry] = useState<FsEntry | null>(null);
  const [properties, setProperties] = useState<FsProperties | null>(null);
  const [propertiesLoading, setPropertiesLoading] = useState(false);
  const [remoteClipboard, setRemoteClipboard] =
    useState<RemoteFileClipboard | null>(null);
  const [preparingDragPath, setPreparingDragPath] = useState("");
  const dragCache = useRef(new Map<string, string[]>());
  const dragPreparations = useRef(new Map<string, Promise<string[]>>());

  const peer =
    availablePeers.find((candidate) => candidate.id === peerId) ??
    availablePeers[0];
  const selectedEntry = entries.find((entry) => entry.path === selectedPath);

  useEffect(() => {
    if (!peer && peerId) setPeerId("");
    if (peer && peer.id !== peerId) setPeerId(peer.id);
  }, [peer, peerId]);

  useEffect(() => {
    setPath(null);
    setSelectedPath("");
    setContextMenu(null);
    setProperties(null);
  }, [peerId]);

  useEffect(() => {
    function closeMenus(): void {
      setContextMenu(null);
    }
    document.addEventListener("mousedown", closeMenus);
    window.addEventListener("blur", closeMenus);
    window.addEventListener("resize", closeMenus);
    return () => {
      document.removeEventListener("mousedown", closeMenus);
      window.removeEventListener("blur", closeMenus);
      window.removeEventListener("resize", closeMenus);
    };
  }, []);

  useEffect(() => {
    if (!peer) {
      setEntries([]);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setMessage("");
    const request: FsRequest = path
      ? { type: "list", path }
      : { type: "roots" };
    void crosscopy
      .filesystemRequest(peer.id, request)
      .then((response) => {
        if (cancelled) return;
        if (response.type === "entries") {
          setEntries(response.entries);
        } else if (response.type === "error") {
          setMessage(response.message);
          setEntries([]);
        }
      })
      .catch((reason) => {
        if (!cancelled) setMessage(errorText(reason, "无法读取远端文件"));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [peer?.id, path]);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void getCurrentWebview()
      .onDragDropEvent((event) => {
        if (event.payload.type === "enter" || event.payload.type === "over") {
          setExternalDragging(true);
          return;
        }
        if (event.payload.type === "leave") {
          setExternalDragging(false);
          return;
        }
        setExternalDragging(false);
        const localPaths = event.payload.paths;
        if (!peer || !path || localPaths.length === 0 || uploading) {
          if (!path) setMessage("请先打开 B 上的目标文件夹，再拖入本机文件");
          return;
        }
        setUploading(true);
        setMessage(`正在上传 ${localPaths.length} 个项目到 ${peer.name}…`);
        void crosscopy
          .filesystemUpload(peer.id, localPaths, path)
          .then(async () => {
            setMessage(`已上传到 ${peer.name}`);
            await refresh();
          })
          .catch((reason) =>
            setMessage(errorText(reason, "上传到远端电脑失败"))
          )
          .finally(() => setUploading(false));
      })
      .then((stop) => {
        unlisten = stop;
      })
      .catch(() => undefined);
    return () => unlisten?.();
  }, [peer?.id, peer?.name, path, uploading]);

  useEffect(() => {
    function pasteFiles(event: ClipboardEvent): void {
      const target = event.target as HTMLElement | null;
      if (
        !peer ||
        !path ||
        uploading ||
        target?.matches("input, textarea, [contenteditable='true']")
      ) {
        return;
      }
      event.preventDefault();
      if (remoteClipboard?.peerId === peer.id) {
        void pasteRemote();
        return;
      }
      setUploading(true);
      setMessage(`正在从系统剪贴板复制到 ${peer.name}…`);
      void crosscopy
        .filesystemUploadClipboard(peer.id, path)
        .then(async () => {
          setMessage(`已粘贴到 ${peer.name}`);
          await refresh();
        })
        .catch((reason) =>
          setMessage(errorText(reason, "系统剪贴板中没有可上传的文件"))
        )
        .finally(() => setUploading(false));
    }
    window.addEventListener("paste", pasteFiles);
    return () => window.removeEventListener("paste", pasteFiles);
  }, [peer?.id, peer?.name, path, remoteClipboard, uploading]);

  useEffect(() => {
    function handleFileKeys(event: KeyboardEvent): void {
      const target = event.target as HTMLElement | null;
      if (target?.matches("input, textarea, [contenteditable='true']")) return;
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "c") {
        if (!selectedEntry || !peer) return;
        event.preventDefault();
        setRemoteClipboard({ peerId: peer.id, paths: [selectedEntry.path] });
        setMessage(`已复制“${selectedEntry.name}”，可在 ${peer.name} 的其他文件夹粘贴`);
        return;
      }
      if (event.key === "F2" && selectedEntry) {
        event.preventDefault();
        setNameDialog({ mode: "rename", entry: selectedEntry });
        return;
      }
      const deleteShortcut =
        event.key === "Delete" ||
        (navigator.userAgent.includes("Mac") &&
          event.metaKey &&
          event.key === "Backspace");
      if (deleteShortcut && selectedEntry) {
        event.preventDefault();
        setDeleteEntry(selectedEntry);
      }
    }
    window.addEventListener("keydown", handleFileKeys);
    return () => window.removeEventListener("keydown", handleFileKeys);
  }, [peer?.id, peer?.name, selectedEntry]);

  async function refresh(): Promise<void> {
    if (!peer) return;
    setLoading(true);
    setMessage("");
    try {
      const response = await crosscopy.filesystemRequest(
        peer.id,
        path ? { type: "list", path } : { type: "roots" }
      );
      if (response.type === "entries") setEntries(response.entries);
      else if (response.type === "error") setMessage(response.message);
    } catch (reason) {
      setMessage(errorText(reason, "刷新失败"));
    } finally {
      setLoading(false);
    }
  }

  async function openEntry(entry: FsEntry): Promise<void> {
    setSelectedPath(entry.path);
    if (entry.directory) {
      setPath(entry.path);
      return;
    }
    if (!peer) return;
    setMessage("正在打开远端文件…");
    try {
      const response = await crosscopy.filesystemRequest(peer.id, {
        type: "read",
        path: entry.path
      });
      if (response.type === "error") {
        setMessage(response.message);
        return;
      }
      if (response.type !== "file") return;
      const bytes = base64Bytes(response.data);
      let content: string;
      try {
        content = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
      } catch {
        setMessage("这是二进制文件，可直接拖到 Finder 或文件资源管理器后打开");
        return;
      }
      setEditor({
        path: response.path,
        name: entry.name,
        content,
        modifiedAt: response.modifiedAt
      });
      setMessage("");
    } catch (reason) {
      setMessage(errorText(reason, "无法打开远端文件"));
    }
  }

  async function saveEditor(): Promise<void> {
    if (!peer || !editor) return;
    setSaving(true);
    try {
      const response = await crosscopy.filesystemRequest(peer.id, {
        type: "write",
        path: editor.path,
        data: bytesBase64(new TextEncoder().encode(editor.content)),
        expectedModifiedAt: editor.modifiedAt
      });
      if (response.type === "error") {
        setMessage(response.message);
        return;
      }
      const modifiedAt =
        response.type === "done" ? (response.entry?.modifiedAt ?? null) : null;
      setEditor({ ...editor, modifiedAt });
      setMessage("已直接保存到远端电脑");
      await refresh();
    } catch (reason) {
      setMessage(errorText(reason, "保存失败"));
    } finally {
      setSaving(false);
    }
  }

  async function submitNameDialog(name: string): Promise<void> {
    if (!peer || !path || !nameDialog) return;
    const value = name.trim();
    if (!value || value.includes("/") || value.includes("\\")) {
      throw new Error("名称不能为空，也不能包含斜杠");
    }
    try {
      const response =
        nameDialog.mode === "rename" && nameDialog.entry
          ? await crosscopy.filesystemRequest(peer.id, {
              type: "rename",
              path: nameDialog.entry.path,
              destination: joinRemotePath(
                parentRemotePath(nameDialog.entry.path) ?? path,
                value
              )
            })
          : await crosscopy.filesystemRequest(peer.id, {
              type:
                nameDialog.mode === "folder"
                  ? "createDirectory"
                  : "createFile",
              path: joinRemotePath(path, value)
            });
      if (response.type === "error") throw new Error(response.message);
      if (nameDialog.mode === "rename") {
        setSelectedPath("");
      }
      setNameDialog(null);
      await refresh();
    } catch (reason) {
      throw new Error(errorText(reason, "操作失败"));
    }
  }

  async function confirmRemove(): Promise<void> {
    if (!peer || !deleteEntry) return;
    const current = deleteEntry;
    setDeleteEntry(null);
    try {
      const response = await crosscopy.filesystemRequest(peer.id, {
        type: "remove",
        path: current.path,
        recursive: current.directory
      });
      if (response.type === "error") throw new Error(response.message);
      setSelectedPath("");
      await refresh();
    } catch (reason) {
      setMessage(errorText(reason, "删除失败"));
    }
  }

  function copyRemote(entry: FsEntry): void {
    if (!peer) return;
    setRemoteClipboard({ peerId: peer.id, paths: [entry.path] });
    setMessage(`已复制“${entry.name}”，可在 ${peer.name} 的其他文件夹粘贴`);
  }

  async function pasteRemote(): Promise<void> {
    if (!peer || !path || remoteClipboard?.peerId !== peer.id) return;
    setContextMenu(null);
    setMessage("正在复制远端项目…");
    try {
      const response = await crosscopy.filesystemRequest(peer.id, {
        type: "paste",
        paths: remoteClipboard.paths,
        destination: path
      });
      if (response.type === "error") throw new Error(response.message);
      setMessage("已粘贴到当前文件夹");
      await refresh();
    } catch (reason) {
      setMessage(errorText(reason, "粘贴失败"));
    }
  }

  async function showProperties(entry: FsEntry): Promise<void> {
    if (!peer) return;
    setContextMenu(null);
    setPropertiesLoading(true);
    setProperties({
      entry,
      itemCount: entry.directory ? 0 : 1,
      totalSize: entry.size
    });
    try {
      const response = await crosscopy.filesystemRequest(peer.id, {
        type: "properties",
        path: entry.path
      });
      if (response.type === "error") throw new Error(response.message);
      if (response.type === "properties") setProperties(response.properties);
    } catch (reason) {
      setMessage(errorText(reason, "无法读取文件属性"));
    } finally {
      setPropertiesLoading(false);
    }
  }

  function prepareRemoteDrag(entry: FsEntry): Promise<string[]> {
    const cached = dragCache.current.get(entry.path);
    if (cached) return Promise.resolve(cached);
    const active = dragPreparations.current.get(entry.path);
    if (active) return active;
    if (!peer) return Promise.reject(new Error("设备不可用"));
    setPreparingDragPath(entry.path);
    setMessage(`正在准备“${entry.name}”，完成后将进入系统拖放`);
    const preparation = crosscopy
      .filesystemPrepareDrag(peer.id, [entry.path])
      .then((paths) => {
        dragCache.current.set(entry.path, paths);
        return paths;
      })
      .finally(() => {
        dragPreparations.current.delete(entry.path);
        setPreparingDragPath((value) => (value === entry.path ? "" : value));
      });
    dragPreparations.current.set(entry.path, preparation);
    return preparation;
  }

  async function startNativeDrag(entry: FsEntry): Promise<void> {
    try {
      const paths = await prepareRemoteDrag(entry);
      await crosscopy.filesystemStartDrag(paths);
      setMessage("");
    } catch (reason) {
      dragCache.current.delete(entry.path);
      setMessage(errorText(reason, "无法启动系统文件拖放"));
    }
  }

  function showContextMenu(
    event: React.MouseEvent,
    entry: FsEntry | null
  ): void {
    event.preventDefault();
    event.stopPropagation();
    if (entry) setSelectedPath(entry.path);
    setContextMenu({
      x: Math.min(event.clientX, window.innerWidth - 224),
      y: Math.min(event.clientY, window.innerHeight - 330),
      entry
    });
  }

  async function download(paths: string[]): Promise<void> {
    if (!peer || paths.length === 0) return;
    setMessage("正在复制到本机…");
    try {
      const destination = await crosscopy.filesystemDownload(peer.id, paths);
      setMessage(`已复制到 ${destination}`);
    } catch (reason) {
      setMessage(errorText(reason, "复制到本机失败"));
    }
  }

  if (props.state.peers.length === 0) {
    return (
      <div className="filesystem-empty">
        <HardDrives size={36} weight="light" />
        <h2>还没有可访问的电脑</h2>
        <p>先配对设备，再在“设置 → 设备权限”中开启文件权限。</p>
      </div>
    );
  }

  if (availablePeers.length === 0) {
    return (
      <div className="filesystem-empty">
        <ShieldCheck size={36} weight="light" />
        <h2>文件系统尚未授权</h2>
        <p>需要设备在线，并在设置中明确开启该设备的文件权限。</p>
      </div>
    );
  }

  return (
    <div className="filesystem-page">
      <div className="filesystem-toolbar">
        <DeviceSelect
          value={peer?.id ?? ""}
          options={availablePeers.map((candidate) => ({
            value: candidate.id,
            label: candidate.name,
            detail: `屏幕 ${candidate.screenNumber} · 已连接`
          }))}
          onChange={setPeerId}
        />
        <button type="button" disabled={!path} onClick={() => setPath(path ? parentRemotePath(path) : null)}>
          <ArrowLeft size={16} /> 返回
        </button>
        <div className="filesystem-address" title={path ?? "系统位置"}>
          {path ?? "系统位置"}
        </div>
        <button type="button" onClick={() => void refresh()}>
          <ArrowsClockwise size={16} /> 刷新
        </button>
      </div>

      <div className="filesystem-upload-hint">
        <ArrowUpRight size={14} />
        拖入可上传到 {peer?.name}，向外拖可复制到 Finder 或文件资源管理器，右键查看更多操作
      </div>

      <div
        className={`filesystem-browser ${
          externalDragging ? "external-dragging" : ""
        }`}
      >
        <div
          className="filesystem-list"
          aria-busy={loading}
          onClick={() => {
            setSelectedPath("");
            setContextMenu(null);
          }}
          onContextMenu={(event) => showContextMenu(event, null)}
        >
          <div className="filesystem-list-head">
            <span>名称</span><span>修改时间</span><span>大小</span>
          </div>
          {loading ? (
            <div className="filesystem-loading">正在读取远端目录…</div>
          ) : entries.length === 0 ? (
            <div className="filesystem-loading">这个位置是空的</div>
          ) : (
            entries.map((entry) => (
              <button
                type="button"
                key={entry.path}
                className={`filesystem-entry ${
                  selectedPath === entry.path ? "selected" : ""
                } ${preparingDragPath === entry.path ? "preparing" : ""}`}
                onClick={(event) => {
                  event.stopPropagation();
                  setSelectedPath(entry.path);
                  setContextMenu(null);
                }}
                onDoubleClick={() => void openEntry(entry)}
                onContextMenu={(event) => showContextMenu(event, entry)}
                draggable
                onDragStart={(event) => {
                  event.preventDefault();
                  void startNativeDrag(entry);
                }}
              >
                <span>
                  {entry.directory ? <FolderOpen size={18} /> : <File size={18} />}
                  <b>{entry.name}</b>
                  {entry.readonly && <small>只读</small>}
                </span>
                <time>{entry.modifiedAt ? formatDateTime(entry.modifiedAt) : "-"}</time>
                <span>{entry.directory ? "-" : formatBytes(entry.size)}</span>
              </button>
            ))
          )}
        </div>
        {externalDragging && (
          <div className="filesystem-upload-overlay">
            <ArrowUpRight size={30} weight="light" />
            <strong>{path ? `上传到 ${peer?.name}` : "请先打开目标文件夹"}</strong>
            <span>{path ?? "当前位于系统位置，不能直接写入"}</span>
          </div>
        )}
      </div>

      {message && <div className="filesystem-message">{message}</div>}

      {contextMenu && (
        <div
          className="file-context-menu"
          role="menu"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          onMouseDown={(event) => event.stopPropagation()}
        >
          {contextMenu.entry && (
            <>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setContextMenu(null);
                  void openEntry(contextMenu.entry!);
                }}
              >
                <FolderOpen size={16} />
                打开
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  copyRemote(contextMenu.entry!);
                  setContextMenu(null);
                }}
              >
                <Copy size={16} />
                复制
                <kbd>{navigator.userAgent.includes("Mac") ? "⌘C" : "Ctrl+C"}</kbd>
              </button>
            </>
          )}
          <button
            type="button"
            role="menuitem"
            disabled={!path || remoteClipboard?.peerId !== peer?.id}
            onClick={() => void pasteRemote()}
          >
            <Clipboard size={16} />
            粘贴
            <kbd>{navigator.userAgent.includes("Mac") ? "⌘V" : "Ctrl+V"}</kbd>
          </button>
          <div className="file-context-separator" />
          <button
            type="button"
            role="menuitem"
            disabled={!path}
            onClick={() => {
              setContextMenu(null);
              setNameDialog({ mode: "folder", entry: null });
            }}
          >
            <FolderPlus size={16} />
            新建文件夹
          </button>
          <button
            type="button"
            role="menuitem"
            disabled={!path}
            onClick={() => {
              setContextMenu(null);
              setNameDialog({ mode: "file", entry: null });
            }}
          >
            <FilePlus size={16} />
            新建文件
          </button>
          {contextMenu.entry && (
            <>
              <div className="file-context-separator" />
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setContextMenu(null);
                  setNameDialog({ mode: "rename", entry: contextMenu.entry });
                }}
              >
                <PencilSimple size={16} />
                重命名
                <kbd>F2</kbd>
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  setContextMenu(null);
                  void download([contextMenu.entry!.path]);
                }}
              >
                <DownloadSimple size={16} />
                下载到本机
              </button>
              <button
                className="danger"
                type="button"
                role="menuitem"
                onClick={() => {
                  setDeleteEntry(contextMenu.entry);
                  setContextMenu(null);
                }}
              >
                <Trash size={16} />
                移到废纸篓/回收站
              </button>
              <div className="file-context-separator" />
              <button
                type="button"
                role="menuitem"
                onClick={() => void showProperties(contextMenu.entry!)}
              >
                <Info size={16} />
                查看属性
              </button>
            </>
          )}
        </div>
      )}

      {nameDialog && (
        <FileNameDialog
          state={nameDialog}
          onClose={() => setNameDialog(null)}
          onSubmit={submitNameDialog}
        />
      )}

      {deleteEntry && (
        <ConfirmFileDelete
          entry={deleteEntry}
          onClose={() => setDeleteEntry(null)}
          onConfirm={confirmRemove}
        />
      )}

      {properties && (
        <FilePropertiesDialog
          properties={properties}
          loading={propertiesLoading}
          onClose={() => setProperties(null)}
        />
      )}

      {editor && (
        <div className="editor-backdrop">
          <section className="remote-editor">
            <header>
              <span><FileText size={18} /><strong>{editor.name}</strong></span>
              <button type="button" onClick={() => setEditor(null)}><X size={18} /></button>
            </header>
            <textarea
              value={editor.content}
              spellCheck={false}
              onChange={(event) => setEditor({ ...editor, content: event.target.value })}
            />
            <footer>
              <small>保存会直接写入 {peer?.name}，本机不创建文件副本</small>
              <button className="primary-button" type="button" disabled={saving} onClick={() => void saveEditor()}>
                <FloppyDisk size={16} /> {saving ? "保存中…" : "保存到远端"}
              </button>
            </footer>
          </section>
        </div>
      )}
    </div>
  );
}

function FileNameDialog(props: {
  state: FileNameDialogState;
  onClose(): void;
  onSubmit(name: string): Promise<void>;
}): React.JSX.Element {
  const initialName =
    props.state.mode === "rename"
      ? (props.state.entry?.name ?? "")
      : props.state.mode === "folder"
        ? "未命名文件夹"
        : "未命名文件";
  const [name, setName] = useState(initialName);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");
  const input = useRef<HTMLInputElement>(null);

  useEffect(() => {
    const field = input.current;
    if (!field) return;
    field.focus();
    const dot = name.lastIndexOf(".");
    field.setSelectionRange(
      0,
      props.state.mode === "rename" && dot > 0 ? dot : name.length
    );
  }, []);

  async function submit(event: React.FormEvent): Promise<void> {
    event.preventDefault();
    setSubmitting(true);
    setError("");
    try {
      await props.onSubmit(name);
    } catch (reason) {
      setError(errorText(reason, "操作失败"));
      setSubmitting(false);
    }
  }

  const title =
    props.state.mode === "rename"
      ? "重命名"
      : props.state.mode === "folder"
        ? "新建文件夹"
        : "新建文件";

  return (
    <div className="editor-backdrop" onMouseDown={props.onClose}>
      <form
        className="file-dialog file-name-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="file-name-dialog-title"
        onMouseDown={(event) => event.stopPropagation()}
        onSubmit={(event) => void submit(event)}
      >
        <header>
          <span className="file-dialog-icon">
            {props.state.mode === "folder" ? (
              <FolderPlus size={19} />
            ) : (
              <FilePlus size={19} />
            )}
          </span>
          <div>
            <h3 id="file-name-dialog-title">{title}</h3>
            <p>操作会直接发生在远端电脑。</p>
          </div>
        </header>
        <label>
          名称
          <input
            ref={input}
            value={name}
            disabled={submitting}
            onChange={(event) => setName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") props.onClose();
            }}
          />
        </label>
        {error && <p className="file-dialog-error">{error}</p>}
        <footer>
          <button type="button" disabled={submitting} onClick={props.onClose}>
            取消
          </button>
          <button className="primary-button" type="submit" disabled={submitting}>
            {submitting ? "处理中…" : title}
          </button>
        </footer>
      </form>
    </div>
  );
}

function ConfirmFileDelete(props: {
  entry: FsEntry;
  onClose(): void;
  onConfirm(): Promise<void>;
}): React.JSX.Element {
  const [submitting, setSubmitting] = useState(false);

  return (
    <div className="editor-backdrop" onMouseDown={props.onClose}>
      <section
        className="file-dialog confirm-file-dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-file-delete-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <span className="file-dialog-icon danger">
            <Trash size={19} />
          </span>
          <div>
            <h3 id="confirm-file-delete-title">移到废纸篓/回收站？</h3>
            <p>“{props.entry.name}”会移到远端电脑的系统回收位置。</p>
          </div>
        </header>
        <footer>
          <button type="button" disabled={submitting} onClick={props.onClose}>
            取消
          </button>
          <button
            className="danger-button"
            type="button"
            disabled={submitting}
            onClick={() => {
              setSubmitting(true);
              void props.onConfirm();
            }}
          >
            {submitting ? "正在移动…" : "移到废纸篓/回收站"}
          </button>
        </footer>
      </section>
    </div>
  );
}

function FilePropertiesDialog(props: {
  properties: FsProperties;
  loading: boolean;
  onClose(): void;
}): React.JSX.Element {
  const { entry, itemCount, totalSize } = props.properties;
  const type = entry.directory
    ? "文件夹"
    : entry.name.includes(".")
      ? `${entry.name.split(".").pop()?.toUpperCase()} 文件`
      : "文件";

  return (
    <div className="editor-backdrop" onMouseDown={props.onClose}>
      <section
        className="file-dialog file-properties-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="file-properties-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <span className="file-properties-icon">
            {entry.directory ? <FolderOpen size={28} /> : <File size={28} />}
          </span>
          <div>
            <h3 id="file-properties-title" title={entry.name}>
              {entry.name}
            </h3>
            <p>{type}</p>
          </div>
        </header>
        <dl>
          <div>
            <dt>大小</dt>
            <dd>
              {props.loading
                ? "正在计算…"
                : entry.directory
                  ? `${formatBytes(totalSize)}（${itemCount} 个项目）`
                  : formatBytes(totalSize)}
            </dd>
          </div>
          <div>
            <dt>位置</dt>
            <dd title={entry.path}>{entry.path}</dd>
          </div>
          <div>
            <dt>修改时间</dt>
            <dd>{entry.modifiedAt ? formatDateTime(entry.modifiedAt) : "未知"}</dd>
          </div>
          <div>
            <dt>属性</dt>
            <dd>
              {[entry.readonly && "只读", entry.hidden && "隐藏"]
                .filter(Boolean)
                .join("、") || "普通"}
            </dd>
          </div>
        </dl>
        <footer>
          <button className="primary-button" type="button" onClick={props.onClose}>
            完成
          </button>
        </footer>
      </section>
    </div>
  );
}

function MousePanel(props: { state: UiState }): React.JSX.Element {
  const [selectedPeerId, setSelectedPeerId] = useState("");
  const selectedPeer =
    props.state.peers.find((peer) => peer.id === selectedPeerId) ??
    props.state.peers[0];
  const [dragOffset, setDragOffset] = useState({ x: 0, y: 0 });
  const [dragging, setDragging] = useState(false);
  const [dragOrigin, setDragOrigin] = useState({
    peerId: "",
    pointerX: 0,
    pointerY: 0,
    offsetX: 0,
    offsetY: 0
  });
  const [message, setMessage] = useState("");

  useEffect(() => {
    if (
      selectedPeerId &&
      !props.state.peers.some((peer) => peer.id === selectedPeerId)
    ) {
      setSelectedPeerId("");
    }
  }, [props.state.peers, selectedPeerId]);

  const peerOffset = (
    peer: UiState["peers"][number]
  ): { x: number; y: number } => {
    if (dragging && peer.id === selectedPeer?.id) return dragOffset;
    const base = SCREEN_OFFSETS[peer.screenPosition];
    const sameEdge = props.state.peers
      .filter(
        (candidate) => candidate.screenPosition === peer.screenPosition
      )
      .sort((a, b) => a.screenNumber - b.screenNumber);
    const index = sameEdge.findIndex((candidate) => candidate.id === peer.id);
    const spread = (index - (sameEdge.length - 1) / 2) * 58;
    return peer.screenPosition === "left" || peer.screenPosition === "right"
      ? { x: base.x, y: base.y + spread }
      : { x: base.x + spread, y: base.y };
  };

  async function choosePosition(
    peerId: string,
    position: ScreenPosition
  ): Promise<void> {
    setMessage("正在同步屏幕位置");
    try {
      await crosscopy.setPeerScreenPosition(peerId, position);
      setMessage("逻辑屏幕位置已同步，对端会自动显示为相反方向");
    } catch (reason) {
      setMessage(typeof reason === "string" ? reason : "屏幕位置同步失败");
    }
  }

  function startDrag(
    event: React.PointerEvent<HTMLButtonElement>,
    peerId: string
  ): void {
    event.currentTarget.setPointerCapture(event.pointerId);
    setSelectedPeerId(peerId);
    const peer = props.state.peers.find((candidate) => candidate.id === peerId);
    const offset = peer ? peerOffset(peer) : { x: 0, y: 0 };
    setDragOffset(offset);
    setDragging(true);
    setDragOrigin({
      peerId,
      pointerX: event.clientX,
      pointerY: event.clientY,
      offsetX: offset.x,
      offsetY: offset.y
    });
  }

  function moveDrag(event: React.PointerEvent<HTMLButtonElement>): void {
    if (!dragging) return;
    setDragOffset({
      x: Math.max(
        -175,
        Math.min(175, dragOrigin.offsetX + event.clientX - dragOrigin.pointerX)
      ),
      y: Math.max(
        -110,
        Math.min(110, dragOrigin.offsetY + event.clientY - dragOrigin.pointerY)
      )
    });
  }

  function endDrag(event: React.PointerEvent<HTMLButtonElement>): void {
    if (!dragging) return;
    event.currentTarget.releasePointerCapture(event.pointerId);
    setDragging(false);
    const finalOffset = {
      x: Math.max(
        -175,
        Math.min(175, dragOrigin.offsetX + event.clientX - dragOrigin.pointerX)
      ),
      y: Math.max(
        -110,
        Math.min(110, dragOrigin.offsetY + event.clientY - dragOrigin.pointerY)
      )
    };
    const position: ScreenPosition =
      Math.abs(finalOffset.x) >= Math.abs(finalOffset.y)
        ? finalOffset.x < 0
          ? "left"
          : "right"
        : finalOffset.y < 0
          ? "up"
          : "down";
    if (dragOrigin.peerId) void choosePosition(dragOrigin.peerId, position);
  }

  const availablePeers = props.state.peers.filter(
    (peer) => peer.online && peer.mouseAllowed && peer.mouseShareEnabled
  );
  const localDisplays =
    props.state.displays.length > 0
      ? props.state.displays
      : [
          {
            id: "local",
            name: "本机屏幕",
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            primary: true,
            mirroredCount: 1
          }
        ];
  const localBounds = localDisplays.reduce(
    (bounds, display) => ({
      left: Math.min(bounds.left, display.x),
      top: Math.min(bounds.top, display.y),
      right: Math.max(bounds.right, display.x + display.width),
      bottom: Math.max(bounds.bottom, display.y + display.height)
    }),
    {
      left: Number.POSITIVE_INFINITY,
      top: Number.POSITIVE_INFINITY,
      right: Number.NEGATIVE_INFINITY,
      bottom: Number.NEGATIVE_INFINITY
    }
  );
  const localScale = Math.min(
    118 / Math.max(1, localBounds.right - localBounds.left),
    58 / Math.max(1, localBounds.bottom - localBounds.top)
  );
  const localMapOffset = {
    x:
      (118 - Math.max(1, localBounds.right - localBounds.left) * localScale) /
      2,
    y:
      (58 - Math.max(1, localBounds.bottom - localBounds.top) * localScale) / 2
  };
  const latencyLabel =
    props.state.mouseLatencyMs === null
      ? "完成一次鼠标穿越后显示"
      : `${props.state.mouseLatencyMs} ms（单向估算）`;

  return (
    <div className="mouse-page">
      <section className="settings-group preference-list feature-controls">
        <div className="preference-row">
          <div className="settings-intro">
            <span className="preference-icon">
              <MouseSimple size={20} />
            </span>
            <div>
              <h2>共享鼠标与键盘</h2>
              <p>
                键盘跟随当前鼠标控制目标。按 Ctrl+Alt+屏幕编号可直接切换，屏幕 1 返回本机。
              </p>
            </div>
          </div>
          <label className="login-setting">
            <input
              type="checkbox"
              checked={props.state.mouseShareEnabled}
              disabled={props.state.peers.length === 0}
              onChange={(event) =>
                void crosscopy.setMouseShareEnabled(event.target.checked)
              }
            />
            <i aria-hidden="true" />
          </label>
        </div>
        <div
          className={`preference-row performance-mode-card ${
            props.state.mouseExtremePerformance ? "enabled" : ""
          }`}
        >
          <div className="settings-intro">
            <span className="preference-icon">
              <Lightning size={19} weight="fill" />
            </span>
            <div>
              <h2>极致性能模式</h2>
              <p>
                优先保证跨屏流畅度；两端都开启效果最佳，控制期间会增加处理器、网络与电量消耗。
              </p>
            </div>
          </div>
          <label className="login-setting">
            <input
              type="checkbox"
              checked={props.state.mouseExtremePerformance}
              onChange={(event) =>
                void crosscopy.setMouseExtremePerformance(event.target.checked)
              }
            />
            <i aria-hidden="true" />
          </label>
        </div>
      </section>

      <ShortcutSettings mode="mouse" state={props.state} />

      <section className="settings-group topology-card">
        <div className="topology-heading">
          <div>
            <h2>逻辑屏幕位置</h2>
            <p>
              每台电脑是一个屏幕组；组内布局跟随系统设置，CrossCopy
              只调整电脑之间的位置。
            </p>
          </div>
          <span className={availablePeers.length > 0 ? "topology-online" : ""}>
            {props.state.peers.length === 0
              ? "尚未配对"
              : `${availablePeers.length} 台可穿越`}
          </span>
        </div>

        <div className="screen-layout" aria-label="拖动调整逻辑屏幕位置">
          <div className="local-display-group">
            <b className="screen-number">1</b>
            <div className="local-monitor-map">
              {localDisplays.map((display) => (
                <div
                  className={`local-monitor ${display.primary ? "primary" : ""}`}
                  key={display.id}
                  style={{
                    left:
                      localMapOffset.x +
                      (display.x - localBounds.left) * localScale,
                    top:
                      localMapOffset.y +
                      (display.y - localBounds.top) * localScale,
                    width: Math.max(30, display.width * localScale),
                    height: Math.max(20, display.height * localScale)
                  }}
                  title={`${display.name} · ${display.width}×${display.height}`}
                >
                  <Desktop size={14} />
                  {display.mirroredCount > 1 && (
                    <em>镜像 ×{display.mirroredCount}</em>
                  )}
                </div>
              ))}
            </div>
            <strong>{props.state.deviceName || "本机"}</strong>
            <small>
              {localDisplays.length} 个逻辑屏幕 · 系统布局
            </small>
          </div>
          {props.state.peers.map((peer) => {
            const offset = peerOffset(peer);
            return (
              <button
                className={`screen-device peer-screen ${
                  selectedPeer?.id === peer.id ? "selected" : ""
                } ${
                  dragging && selectedPeer?.id === peer.id ? "dragging" : ""
                }`}
                style={{
                  transform: `translate(-50%, -50%) translate(${offset.x}px, ${offset.y}px)`
                }}
                type="button"
                key={peer.id}
                onPointerDown={(event) => startDrag(event, peer.id)}
                onPointerMove={moveDrag}
                onPointerUp={endDrag}
                onPointerCancel={() => setDragging(false)}
              >
                <b className="screen-number">{peer.screenNumber}</b>
                <PeerDisplayGlyph displays={peer.displays} />
                <strong>{peer.name}</strong>
                <small>
                  {peer.online ? "在线" : "离线"} ·{" "}
                  {Math.max(1, peer.displays.length)} 个逻辑屏幕
                </small>
              </button>
            );
          })}
        </div>

        <div className="direction-picker" aria-label="快速选择屏幕方向">
          {(
            [
              ["left", "左侧"],
              ["right", "右侧"],
              ["up", "上方"],
              ["down", "下方"]
            ] as Array<[ScreenPosition, string]>
          ).map(([position, label]) => (
            <button
              className={
                selectedPeer?.screenPosition === position ? "active" : ""
              }
              type="button"
              key={position}
              disabled={!selectedPeer}
              onClick={() =>
                selectedPeer &&
                void choosePosition(selectedPeer.id, position)
              }
            >
              {label}
            </button>
          ))}
        </div>
        {selectedPeer && (
          <div className="selected-screen-settings">
            <div className="screen-switch-row">
              <span>
                已选择：屏幕 {selectedPeer.screenNumber} · {selectedPeer.name}
              </span>
              <button
                type="button"
                disabled={
                  !props.state.mouseShareEnabled ||
                  !selectedPeer.online ||
                  !selectedPeer.mouseAllowed ||
                  !selectedPeer.mouseShareEnabled
                }
                onClick={() =>
                  void crosscopy.switchMouseToScreen(selectedPeer.screenNumber)
                }
              >
                立即切换
                <kbd>
                  {selectedPeer.screenNumber <= 9
                    ? `Ctrl Alt ${selectedPeer.screenNumber}`
                    : `屏幕 ${selectedPeer.screenNumber}`}
                </kbd>
              </button>
            </div>
            <PeerDpiControl
              peerId={selectedPeer.id}
              peerName={selectedPeer.name}
              dpi={selectedPeer.mouseReceiveDpi}
            />
          </div>
        )}
        {message && <small className="topology-message">{message}</small>}
      </section>

      <div className="mouse-metrics">
        <section className="settings-group metric-card">
          <span>穿越延时</span>
          <strong>{latencyLabel}</strong>
          <small>通过加密 UDP 往返时间计算，不依赖两台电脑的系统时钟。</small>
        </section>
        <section className="settings-group metric-card">
          <span>当前状态</span>
          <strong>
            {!props.state.mouseShareEnabled
              ? "已关闭"
              : props.state.mouseSessionActive
                ? "正在跨屏控制"
                : availablePeers.length > 0
                  ? "等待鼠标到达屏幕边缘"
                  : "等待有权限的电脑上线并开启共享"}
          </strong>
          <small>
            {props.state.mouseShareEnabled && !props.state.mouseListenerStarted
              ? "输入监听启动失败；请检查系统辅助功能权限，然后关闭并重新开启共享。"
              : props.state.mouseListenerStarted
              ? "鼠标与键盘监听已按需启动；目标电脑的本机键盘仍可同时使用。"
              : "首次开启后才会启动输入监听，不开启时没有额外轮询。"}
          </small>
        </section>
      </div>

      <section className="mouse-support-note">
        扩展屏会作为同一电脑的固定屏幕组，镜像屏自动合并。键盘按目标电脑的系统配置处理，目标电脑自己的键盘仍可同时使用。
      </section>
    </div>
  );
}

function PeerDpiControl(props: {
  peerId: string;
  peerName: string;
  dpi: number;
}): React.JSX.Element {
  const [draft, setDraft] = useState(String(props.dpi));
  const [message, setMessage] = useState("");

  useEffect(() => {
    setDraft(String(props.dpi));
    setMessage("");
  }, [props.peerId, props.dpi]);

  async function save(): Promise<void> {
    const parsed = Number.parseInt(draft, 10);
    if (!Number.isFinite(parsed) || parsed < 100 || parsed > 2000) {
      setMessage("请输入 100-2000");
      return;
    }
    const dpi = Math.round(parsed);
    setDraft(String(dpi));
    try {
      await crosscopy.setPeerMouseDpi(props.peerId, dpi);
      setMessage("已生效");
    } catch (reason) {
      setMessage(typeof reason === "string" ? reason : "保存失败");
    }
  }

  return (
    <div className="peer-dpi-row">
      <span>
        <strong>{props.peerName} 进入本机后的逻辑 DPI</strong>
        <small>500 为默认速度，仅影响这台设备控制本机。</small>
      </span>
      <label>
        <input
          type="number"
          min="100"
          max="2000"
          step="50"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => void save()}
          onKeyDown={(event) => {
            if (event.key === "Enter") event.currentTarget.blur();
          }}
        />
        <b>DPI</b>
      </label>
      <em>{message}</em>
    </div>
  );
}

function ShortcutSettings(props: {
  mode: "clipboard" | "mouse";
  state: UiState;
}): React.JSX.Element {
  const [copy, setCopy] = useState(props.state.copyShortcut);
  const [paste, setPaste] = useState(props.state.pasteShortcut);
  const [mouse, setMouse] = useState(props.state.mouseShortcut);
  const [message, setMessage] = useState("");

  useEffect(() => {
    setCopy(props.state.copyShortcut);
    setPaste(props.state.pasteShortcut);
    setMouse(props.state.mouseShortcut);
  }, [
    props.state.copyShortcut,
    props.state.pasteShortcut,
    props.state.mouseShortcut
  ]);

  async function save(): Promise<void> {
    setMessage("正在保存");
    try {
      await crosscopy.setShortcuts(copy, paste, mouse);
      setMessage("快捷键已生效");
    } catch (reason) {
      setMessage(typeof reason === "string" ? reason : "快捷键保存失败");
    }
  }

  return (
    <section className="settings-group shortcut-card">
      <div className="settings-intro">
        <span className="preference-icon">
          <Keyboard size={19} />
        </span>
        <div>
          <h2>
            {props.mode === "clipboard" ? "剪贴板快捷键" : "键鼠共享快捷键"}
          </h2>
          <p>
            {props.mode === "clipboard"
              ? "专用组合键不会覆盖普通复制和粘贴。"
              : "用于快速开启或关闭键鼠共享。"}
          </p>
        </div>
      </div>
      <div className={`shortcut-grid ${props.mode === "mouse" ? "single" : ""}`}>
        {props.mode === "clipboard" ? (
          <>
            <ShortcutInput label="跨设备复制" value={copy} onChange={setCopy} />
            <ShortcutInput label="跨设备粘贴" value={paste} onChange={setPaste} />
          </>
        ) : (
          <ShortcutInput
            label="开启或关闭键鼠共享"
            value={mouse}
            onChange={setMouse}
          />
        )}
      </div>
      <div className="settings-actions">
        <button className="primary-button" type="button" onClick={() => void save()}>
          保存快捷键
        </button>
        {message && <span>{message}</span>}
      </div>
    </section>
  );
}

function SettingsPanel(props: {
  state: UiState;
  diagnosticsMessage: string;
  onDiagnostics(): Promise<void>;
}): React.JSX.Element {
  const isMac = navigator.userAgent.includes("Mac");

  return (
    <div className="settings-page">
      <section className="settings-group">
        <div className="settings-intro">
          <ShieldCheck size={22} />
          <div>
            <h2>设备权限</h2>
            <p>
              权限是双向的：关闭某台电脑后，双方都不能通过对应功能互相发送或接收。
            </p>
          </div>
        </div>
        <div className="device-permissions">
          {props.state.peers.length === 0 ? (
            <div className="permissions-empty">配对设备后可在这里单独授权</div>
          ) : (
            props.state.peers
              .slice()
              .sort((a, b) => a.screenNumber - b.screenNumber)
              .map((peer) => (
                <div className="permission-row" key={peer.id}>
                  <span className="permission-device">
                    <b>{peer.screenNumber}</b>
                    <span>
                      <strong>{peer.name}</strong>
                      <small>
                        {peer.direct ? "直接配对" : "由可信设备自动加入"} ·{" "}
                        {peer.online ? "在线" : "离线"}
                      </small>
                    </span>
                  </span>
                  <label>
                    <input
                      type="checkbox"
                      checked={peer.clipboardAllowed}
                      onChange={(event) =>
                        void crosscopy.setPeerPermissions(
                          peer.id,
                          event.target.checked,
                          peer.mouseAllowed,
                          peer.filesystemAllowed
                        )
                      }
                    />
                    <i aria-hidden="true" />
                    剪贴板
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={peer.mouseAllowed}
                      onChange={(event) =>
                        void crosscopy.setPeerPermissions(
                          peer.id,
                          peer.clipboardAllowed,
                          event.target.checked,
                          peer.filesystemAllowed
                        )
                      }
                    />
                    <i aria-hidden="true" />
                    鼠标
                  </label>
                  <label>
                    <input
                      type="checkbox"
                      checked={peer.filesystemAllowed}
                      onChange={(event) =>
                        void crosscopy.setPeerPermissions(
                          peer.id,
                          peer.clipboardAllowed,
                          peer.mouseAllowed,
                          event.target.checked
                        )
                      }
                    />
                    <i aria-hidden="true" />
                    文件
                  </label>
                </div>
              ))
          )}
        </div>
      </section>

      <section className="settings-group preference-list">
        <div className="preference-row">
          <span>
            <strong>开机自动启动</strong>
            <small>关闭主窗口后仍在托盘低功耗运行</small>
          </span>
          <label className="login-setting">
            <input
              type="checkbox"
              checked={props.state.launchAtLogin}
              onChange={(event) =>
                void crosscopy.setLaunchAtLogin(event.target.checked)
              }
            />
            <i aria-hidden="true" />
          </label>
        </div>
        {isMac && (
          <div className="preference-row">
            <span>
              <strong>辅助功能权限</strong>
              <small>用于全局快捷键以及跨设备鼠标和键盘控制</small>
            </span>
            <button
              className="secondary-button"
              type="button"
              onClick={() => void crosscopy.openInputPermissions()}
            >
              <ShieldCheck size={16} />
              打开系统设置
            </button>
          </div>
        )}
        <div className="preference-row">
          <span>
            <strong>诊断日志</strong>
            <small>不包含配对码、剪贴板内容或完整文件路径</small>
          </span>
          <div className="diagnostics-setting">
            <button
              className="secondary-button"
              type="button"
              onClick={() => void props.onDiagnostics()}
            >
              <FileText size={16} />
              导出日志
            </button>
            {props.diagnosticsMessage && <small>{props.diagnosticsMessage}</small>}
          </div>
        </div>
      </section>
    </div>
  );
}

function ShortcutInput(props: {
  label: string;
  value: string;
  onChange(value: string): void;
}): React.JSX.Element {
  function capture(event: React.KeyboardEvent<HTMLInputElement>): void {
    event.preventDefault();
    if (["Control", "Shift", "Alt", "Meta"].includes(event.key)) return;
    const modifiers = [
      event.ctrlKey ? "Ctrl" : "",
      event.metaKey ? "Command" : "",
      event.altKey ? "Alt" : "",
      event.shiftKey ? "Shift" : ""
    ].filter(Boolean);
    if (modifiers.length === 0) return;
    const key = event.key.length === 1 ? event.key.toUpperCase() : event.key;
    props.onChange([...modifiers, key].join("+"));
  }

  return (
    <label className="shortcut-input">
      <span>{props.label}</span>
      <input
        readOnly
        value={props.value}
        onKeyDown={capture}
        onFocus={(event) => event.currentTarget.select()}
        aria-label={`${props.label}，按下新的组合键`}
      />
      <small>点击后直接按下新组合键</small>
    </label>
  );
}

function TransferApp(): React.JSX.Element {
  const [state, setState] = useState<UiState>(EMPTY_STATE);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void crosscopy.getState().then(setState);
    void listen<UiState>("state", (event) => setState(event.payload)).then(
      (stop) => {
        unlisten = stop;
      }
    );
    return () => unlisten?.();
  }, []);

  const transfer = state.transfer;
  const percent =
    transfer && transfer.total > 0
      ? Math.min(100, Math.round((transfer.transferred / transfer.total) * 100))
      : transfer?.status === "done"
        ? 100
        : 0;

  return (
    <main className="transfer-window">
      <div className={`transfer-symbol ${transfer?.status ?? "working"}`}>
        {transfer?.direction === "sent" ? (
          <ArrowUpRight size={20} />
        ) : (
          <ArrowDownLeft size={20} />
        )}
      </div>
      <div className="transfer-copy">
        <div>
          <strong>{transfer?.label ?? "准备传输"}</strong>
          <span>{percent}%</span>
        </div>
        <div className="progress-track">
          <i style={{ transform: `scaleX(${percent / 100})` }} />
        </div>
        <small>
          {transfer
            ? `${formatBytes(transfer.transferred)} / ${formatBytes(transfer.total)}`
            : "正在建立连接"}
        </small>
      </div>
    </main>
  );
}

function LoadingState(): React.JSX.Element {
  return (
    <div className="loading-state" aria-label="正在加载">
      <div />
      <div />
      <div />
    </div>
  );
}

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit"
  }).format(timestamp);
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
}

function formatDateTime(timestamp: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit"
  }).format(timestamp);
}

function errorText(reason: unknown, fallback: string): string {
  return typeof reason === "string"
    ? reason
    : reason instanceof Error
      ? reason.message
      : fallback;
}

function base64Bytes(value: string): Uint8Array {
  const binary = window.atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}

function bytesBase64(bytes: Uint8Array): string {
  let binary = "";
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return window.btoa(binary);
}

function parentRemotePath(path: string): string | null {
  const windowsRoot = /^[A-Za-z]:\\$/;
  if (path === "/" || windowsRoot.test(path)) return null;
  const separator = path.includes("\\") ? "\\" : "/";
  const trimmed = path.endsWith(separator) ? path.slice(0, -1) : path;
  const index = trimmed.lastIndexOf(separator);
  if (index < 0) return null;
  if (separator === "\\" && index === 2) return `${trimmed.slice(0, 2)}\\`;
  return index === 0 ? "/" : trimmed.slice(0, index);
}

function joinRemotePath(parent: string, name: string): string {
  const separator = parent.includes("\\") ? "\\" : "/";
  return `${parent.endsWith(separator) ? parent : `${parent}${separator}`}${name}`;
}

const transferMode = new URLSearchParams(window.location.search).has("transfer");

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {transferMode ? <TransferApp /> : <App />}
  </React.StrictMode>
);
