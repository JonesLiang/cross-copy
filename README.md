# CrossCopy

在同一局域网的 macOS 和 Windows 电脑之间同步剪贴板。复制文本、文件、文件夹或压缩包后，可直接在另一台电脑粘贴。

## 已实现

- UDP 组播自动发现设备，不需要填写 IP 或端口
- 首次使用 6 位验证码配对，验证码 2 分钟后失效
- 配对后保存 256 位随机密钥
- 文本、任意文件、多个文件、文件夹和压缩包传输
- AES-256-GCM 加密控制消息和每一个文件数据块
- 原生系统剪贴板事件监听，不做高频轮询
- 1 MiB 固定缓冲流式传输，大文件不会整体载入内存
- 文件下载到系统“下载/CrossCopy”目录，完成后自动写入文件剪贴板
- 托盘常驻、暂停同步、开机启动、浅色和深色模式

## 开发

要求 Node.js 22、Rust stable，以及对应平台的 Tauri 系统依赖。

```bash
npm install
npm run dev
```

运行检查：

```bash
npm run typecheck
npm test
```

## 打包

### 一键同时打包 macOS 和 Windows

将代码推送到 GitHub 后，打开仓库的 **Actions** 页面，选择
**一键打包 Mac 和 Windows**，点击 **Run workflow**。完成后会得到：

- `CrossCopy-macOS-Universal`：同时支持 Intel 和 Apple Silicon 的 DMG
- `CrossCopy-Windows-x64`：Windows NSIS EXE 和 MSI 安装包

如果本机已安装并登录 GitHub CLI，也可以运行：

```bash
npm run release:all
```

该命令会触发云端双平台构建、等待完成，并把安装包下载到
`release/<任务编号>`。

### 单独在当前系统打包

macOS 安装包需要在 macOS 构建：

```bash
npm run build
```

Windows 安装包需要在 Windows 构建，建议使用同一条命令或 CI：

```powershell
npm install
npm run build
```

未签名的安装包会触发 macOS Gatekeeper 或 Windows SmartScreen 提示。公开分发前应配置 Apple Developer ID 和 Windows 代码签名证书。

## 网络说明

CrossCopy 只在局域网内工作。企业 Wi-Fi、访客网络或开启 AP 隔离的路由器可能禁止设备互相发现。系统防火墙首次询问时，需要允许 CrossCopy 接收局域网连接。

配对码是短时认证凭据。配对后的内容使用设备密钥加密，但首次配对仍应在可信局域网中完成。

## 性能设计

- 传输文件时不生成 zip、tar 或中间副本
- 已压缩文件不会被重复压缩
- 大文件按 1 MiB 块读取、加密、写入
- 每块只增加 28 字节加密开销
- TCP `NODELAY` 降低短文本延迟
- Release 构建开启 LTO、符号裁剪和单代码生成单元

实际速度取决于网卡、Wi-Fi 信号、磁盘速度和杀毒软件。千兆有线局域网通常由磁盘或链路带宽决定，而不是 CrossCopy 的缓冲区。
