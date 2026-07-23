import React, { useEffect, useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import {
  ArrowDownLeft,
  ArrowUpRight,
  Check,
  Clipboard,
  Copy,
  Desktop,
  Gear,
  Link,
  Plus,
  ShieldCheck,
  Trash,
  WarningCircle,
  X
} from "@phosphor-icons/react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { UiState } from "./types";
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
  unpair: (peerId: string) => invoke<void>("unpair", { peerId })
};

const EMPTY_STATE: UiState = {
  deviceName: "",
  syncEnabled: true,
  launchAtLogin: false,
  pairingCode: null,
  pairingExpiresAt: null,
  peers: [],
  activity: []
};

type PairMode = "choose" | "show" | "enter" | null;

function App(): React.JSX.Element {
  const [state, setState] = useState<UiState>(EMPTY_STATE);
  const [ready, setReady] = useState(false);
  const [pairMode, setPairMode] = useState<PairMode>(null);
  const [code, setCode] = useState("");
  const [error, setError] = useState("");
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    void crosscopy
      .getState()
      .then(setState)
      .catch(() => setState(EMPTY_STATE))
      .finally(() => setReady(true));
    void listen<UiState>("state", (event) => setState(event.payload))
      .then((stop) => {
        unlisten = stop;
      })
      .catch(() => undefined);
    return () => unlisten?.();
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
      setError(reason instanceof Error ? reason.message : "配对失败，请重试");
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
          <button className="nav-item active" type="button">
            <Clipboard size={18} />
            剪贴板
          </button>
          <button className="nav-item" type="button">
            <Gear size={18} />
            设置
          </button>
        </nav>

        <div className="device-label">
          <Desktop size={17} />
          <span>
            <small>本机</small>
            {state.deviceName || "正在读取"}
          </span>
        </div>
      </aside>

      <section className="content">
        <header className="topbar">
          <div>
            <h1>剪贴板</h1>
            <p>在已配对电脑间复制文本、文件和文件夹</p>
          </div>
          <button
            className="primary-button"
            type="button"
            onClick={() => setPairMode("choose")}
          >
            <Plus size={17} weight="bold" />
            添加电脑
          </button>
        </header>

        {!ready ? (
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

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
