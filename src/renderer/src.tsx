import React, { useEffect, useMemo, useRef, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  ArrowDownLeft,
  ArrowUpRight,
  Check,
  Clipboard,
  Copy,
  Desktop,
  DownloadSimple,
  FileText,
  Gear,
  Keyboard,
  Lightning,
  Link,
  MouseSimple,
  Plus,
  ShieldCheck,
  Trash,
  WarningCircle,
  X
} from "@phosphor-icons/react";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type { ScreenPosition, UiState } from "./types";
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
  wakeNetwork: () => invoke<void>("wake_network"),
  openInputPermissions: () => invoke<void>("open_input_permissions"),
  setShortcuts: (copy: string, paste: string, mouse: string) =>
    invoke<void>("set_shortcuts", { copy, paste, mouse }),
  setMouseShareEnabled: (value: boolean) =>
    invoke<void>("set_mouse_share_enabled", { value }),
  setMousePosition: (position: ScreenPosition) =>
    invoke<void>("set_mouse_position", { position }),
  setPeerScreenPosition: (peerId: string, position: ScreenPosition) =>
    invoke<void>("set_peer_screen_position", { peerId, position }),
  setPeerPermissions: (
    peerId: string,
    clipboardAllowed: boolean,
    mouseAllowed: boolean
  ) =>
    invoke<void>("set_peer_permissions", {
      peerId,
      clipboardAllowed,
      mouseAllowed
    }),
  switchMouseToScreen: (screenNumber: number) =>
    invoke<void>("switch_mouse_to_screen", { screenNumber })
};

const EMPTY_STATE: UiState = {
  deviceName: "",
  syncEnabled: true,
  launchAtLogin: false,
  copyShortcut: "Ctrl+Shift+C",
  pasteShortcut: "Ctrl+Shift+V",
  mouseShareEnabled: false,
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
type UpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "available"; version: string }
  | { kind: "downloading"; version: string; progress: number | null }
  | { kind: "installing"; version: string }
  | { kind: "current" }
  | { kind: "error" };

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
  const [view, setView] = useState<"clipboard" | "mouse" | "settings">(
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
      const update = await check({ timeout: 12_000 });
      if (!update) {
        setUpdateState(showCurrent ? { kind: "current" } : { kind: "idle" });
        return;
      }
      await availableUpdate.current?.close();
      availableUpdate.current = update;
      setUpdateState({ kind: "available", version: update.version });
    } catch {
      setUpdateState(showCurrent ? { kind: "error" } : { kind: "idle" });
    }
  }

  async function installUpdate(): Promise<void> {
    let update = availableUpdate.current;
    if (!update) {
      await checkForUpdate(true);
      return;
    }

    let received = 0;
    let total: number | undefined;
    setUpdateState({
      kind: "downloading",
      version: update.version,
      progress: null
    });
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      received = 0;
      total = undefined;
      try {
        await update.downloadAndInstall((event) => {
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
              version: update.version,
              progress
            });
          } else {
            setUpdateState({ kind: "installing", version: update.version });
          }
        });
        await relaunch();
        return;
      } catch {
        if (attempt === 3) {
          setUpdateState({ kind: "error" });
          return;
        }
        await update.close().catch(() => undefined);
        const retryUpdate = await check({ timeout: 15_000 }).catch(() => null);
        if (!retryUpdate) {
          setUpdateState({ kind: "error" });
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
            className={`nav-item ${view === "mouse" ? "active" : ""}`}
            type="button"
            onClick={() => setView("mouse")}
          >
            <MouseSimple size={18} />
            鼠标共享
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

        <UpdateControl
          state={updateState}
          onCheck={() => void checkForUpdate(true)}
          onInstall={() => void installUpdate()}
        />
        <div className="device-label">
          <Desktop size={17} />
          <span>
            <small>本机</small>
            {state.deviceName || "正在读取"}
          </span>
        </div>
        <div className="app-version">v{appVersion || "—"}</div>
      </aside>

      <section className="content">
        <header className="topbar">
          <div>
            <h1>
              {view === "clipboard"
                ? "剪贴板"
                : view === "mouse"
                  ? "鼠标共享"
                  : "设置"}
            </h1>
            <p>
              {view === "clipboard"
                ? "使用专用快捷键发送和粘贴，不影响普通剪贴板"
                : view === "mouse"
                  ? "把另一台电脑放到逻辑方位，鼠标即可跨越屏幕"
                : "配置快捷键、后台启动、系统权限和诊断"}
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

            <div className="footer-settings">
              <div className="diagnostics-setting">
                <button type="button" onClick={() => void exportDiagnostics()}>
                  <FileText size={17} />
                  导出诊断日志
                </button>
                {diagnosticsMessage && <small>{diagnosticsMessage}</small>}
              </div>
              <label className="login-setting">
                <span>
                  <strong>开机自动启动</strong>
                  <small>保持后台运行，电脑上线后自动连接</small>
                </span>
                <input
                  type="checkbox"
                  checked={state.launchAtLogin}
                  onChange={(event) =>
                    void crosscopy.setLaunchAtLogin(event.target.checked)
                  }
                />
                <i aria-hidden="true" />
              </label>
            </div>
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
      <button className="update-link" type="button" onClick={props.onCheck}>
        检查更新
      </button>
    );
  }
  if (state.kind === "checking") {
    return <div className="update-link is-static">正在检查更新…</div>;
  }
  if (state.kind === "current") {
    return <div className="update-link is-static">已是最新版本</div>;
  }
  if (state.kind === "error") {
    return (
      <button
        className="update-link update-error"
        type="button"
        onClick={props.onCheck}
      >
        更新检查失败，重试
      </button>
    );
  }
  if (state.kind === "available") {
    return (
      <button className="update-card" type="button" onClick={props.onInstall}>
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
    <div className="update-card is-progress">
      <DownloadSimple size={16} />
      <span>
        <strong>{label}</strong>
        <small>v{state.version}</small>
      </span>
    </div>
  );
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
  up: { x: 0, y: -82 },
  down: { x: 0, y: 82 }
};

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
        -95,
        Math.min(95, dragOrigin.offsetY + event.clientY - dragOrigin.pointerY)
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
        -95,
        Math.min(95, dragOrigin.offsetY + event.clientY - dragOrigin.pointerY)
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
  const latencyLabel =
    props.state.mouseLatencyMs === null
      ? "完成一次鼠标穿越后显示"
      : `${props.state.mouseLatencyMs} ms（单向估算）`;

  return (
    <div className="mouse-page">
      <section className="settings-group mouse-toggle-card">
        <div className="settings-intro">
          <MouseSimple size={23} />
          <div>
            <h2>共享鼠标</h2>
            <p>
              移动到对应边缘，或按 Ctrl+Shift+屏幕编号，直接把本机鼠标切换过去。
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
      </section>

      <section className="settings-group topology-card">
        <div className="topology-heading">
          <div>
            <h2>逻辑屏幕位置</h2>
            <p>拖动任意电脑到本机四周；编号 1 永远是当前电脑。</p>
          </div>
          <span className={availablePeers.length > 0 ? "topology-online" : ""}>
            {props.state.peers.length === 0
              ? "尚未配对"
              : `${availablePeers.length} 台可穿越`}
          </span>
        </div>

        <div className="screen-layout" aria-label="拖动调整逻辑屏幕位置">
          <div className="screen-device local-screen">
            <b className="screen-number">1</b>
            <Desktop size={27} />
            <strong>{props.state.deviceName || "本机"}</strong>
            <small>当前电脑</small>
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
                <Desktop size={27} />
                <strong>{peer.name}</strong>
                <small>{peer.online ? "在线" : "离线"}</small>
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
                  ? `Ctrl Shift ${selectedPeer.screenNumber}`
                  : `屏幕 ${selectedPeer.screenNumber}`}
              </kbd>
            </button>
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
              ? "鼠标监听启动失败；请检查系统辅助功能权限，然后关闭并重新开启共享。"
              : props.state.mouseListenerStarted
              ? "鼠标监听已按需启动；本机物理输入始终优先。"
              : "首次开启后才会启动鼠标监听，不开启时没有额外轮询。"}
          </small>
        </section>
      </div>

      <section className="mouse-support-note">
        当前仅转发鼠标移动、左键、右键、滚轮滚动和滚轮按下；键盘不会跨设备发送。
      </section>
    </div>
  );
}

function SettingsPanel(props: {
  state: UiState;
  diagnosticsMessage: string;
  onDiagnostics(): Promise<void>;
}): React.JSX.Element {
  const [copy, setCopy] = useState(props.state.copyShortcut);
  const [paste, setPaste] = useState(props.state.pasteShortcut);
  const [mouse, setMouse] = useState(props.state.mouseShortcut);
  const [message, setMessage] = useState("");
  const isMac = navigator.userAgent.includes("Mac");

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
    <div className="settings-page">
      <section className="settings-group">
        <div className="settings-intro">
          <Keyboard size={22} />
          <div>
            <h2>跨设备快捷键</h2>
            <p>点击输入框后按下组合键。普通复制和粘贴只保留在本机。</p>
          </div>
        </div>
        <div className="shortcut-grid">
          <ShortcutInput label="跨设备复制" value={copy} onChange={setCopy} />
          <ShortcutInput label="跨设备粘贴" value={paste} onChange={setPaste} />
          <ShortcutInput
            label="开启或关闭鼠标共享"
            value={mouse}
            onChange={setMouse}
          />
        </div>
        <div className="settings-actions">
          <button className="primary-button" type="button" onClick={() => void save()}>
            保存快捷键
          </button>
          {message && <span>{message}</span>}
        </div>
      </section>

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
                          peer.mouseAllowed
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
                          event.target.checked
                        )
                      }
                    />
                    <i aria-hidden="true" />
                    鼠标
                  </label>
                </div>
              ))
          )}
        </div>
      </section>

      <section className="settings-group settings-row">
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
      </section>

      {isMac && (
        <section className="settings-group settings-row">
          <span>
            <strong>辅助功能权限</strong>
            <small>用于跨设备快捷键和鼠标控制，不会转发键盘输入</small>
          </span>
          <div className="diagnostics-setting">
            <button
              type="button"
              onClick={() => void crosscopy.openInputPermissions()}
            >
              <ShieldCheck size={17} />
              打开系统设置
            </button>
          </div>
        </section>
      )}

      <section className="settings-group settings-row">
        <span>
          <strong>诊断日志</strong>
          <small>不包含配对码、剪贴板内容或完整文件路径</small>
        </span>
        <div className="diagnostics-setting">
          <button type="button" onClick={() => void props.onDiagnostics()}>
            <FileText size={17} />
            导出日志
          </button>
          {props.diagnosticsMessage && <small>{props.diagnosticsMessage}</small>}
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

const transferMode = new URLSearchParams(window.location.search).has("transfer");

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {transferMode ? <TransferApp /> : <App />}
  </React.StrictMode>
);
