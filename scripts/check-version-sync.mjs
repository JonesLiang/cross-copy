import { readFile, readdir } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const [assetDirectory, assetPlatform = "all"] = process.argv.slice(2);
const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const packageJson = JSON.parse(await readFile(resolve(root, "package.json"), "utf8"));
const packageLock = JSON.parse(
  await readFile(resolve(root, "package-lock.json"), "utf8")
);
const tauriConfig = JSON.parse(
  await readFile(resolve(root, "src-tauri/tauri.conf.json"), "utf8")
);
const cargoToml = await readFile(resolve(root, "src-tauri/Cargo.toml"), "utf8");
const cargoLock = await readFile(resolve(root, "src-tauri/Cargo.lock"), "utf8");

const expected = packageJson.version;
const versions = new Map([
  ["package.json", expected],
  ["package-lock.json", packageLock.version],
  ["package-lock root package", packageLock.packages?.[""]?.version],
  ["tauri.conf.json", tauriConfig.version],
  ["Cargo.toml", packageVersion(cargoToml, "Cargo.toml")],
  ["Cargo.lock crosscopy", lockedPackageVersion(cargoLock, "crosscopy")]
]);

const mismatches = [...versions].filter(([, version]) => version !== expected);
if (mismatches.length > 0) {
  const details = [...versions]
    .map(([source, version]) => `${source}=${version ?? "<missing>"}`)
    .join("\n");
  throw new Error(`版本号不一致，拒绝构建或发布：\n${details}`);
}

if (assetDirectory) {
  const files = await readdir(resolve(assetDirectory));
  const versionedAssets = files.filter((file) =>
    /^CrossCopy_\d+\.\d+\.\d+_/.test(file)
  );
  const wrongAssets = versionedAssets.filter(
    (file) => !file.startsWith(`CrossCopy_${expected}_`)
  );
  if (wrongAssets.length > 0) {
    throw new Error(
      `发布目录混入其他版本，拒绝发布：${wrongAssets.join(", ")}`
    );
  }
  if (assetPlatform === "all" || assetPlatform === "mac") {
    requireOne(
      files,
      `CrossCopy_${expected}_universal.app.tar.gz`,
      "macOS 更新包"
    );
    requireOne(
      files,
      `CrossCopy_${expected}_universal.app.tar.gz.sig`,
      "macOS 更新签名"
    );
  }
  if (assetPlatform === "all" || assetPlatform === "windows") {
    requireOne(files, `CrossCopy_${expected}_x64-setup.exe`, "Windows 更新包");
    requireOne(
      files,
      `CrossCopy_${expected}_x64-setup.exe.sig`,
      "Windows 更新签名"
    );
  }
}

console.log(`CrossCopy version verified: ${expected}`);

function packageVersion(toml, source) {
  const packageSection = toml.match(/^\[package\]\s*\n([\s\S]*?)(?=^\[)/m)?.[1];
  const version = packageSection?.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) throw new Error(`${source} 缺少 [package].version`);
  return version;
}

function lockedPackageVersion(lock, name) {
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const block = lock.match(
    new RegExp(
      `\\[\\[package\\]\\]\\s*\\nname = "${escaped}"\\s*\\nversion = "([^"]+)"`
    )
  );
  if (!block) throw new Error(`Cargo.lock 缺少 ${name} 包版本`);
  return block[1];
}

function requireOne(files, expectedName, label) {
  const count = files.filter((file) => file === expectedName).length;
  if (count !== 1) {
    throw new Error(`${label}数量应为 1：${expectedName}，实际 ${count}`);
  }
}
