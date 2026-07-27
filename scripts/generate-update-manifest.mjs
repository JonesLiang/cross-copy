import { readdir, readFile, writeFile } from "node:fs/promises";
import { basename, resolve } from "node:path";

const [assetDirectory, version, repository, tag] = process.argv.slice(2);
if (!assetDirectory || !version || !repository || !tag) {
  throw new Error(
    "用法: node generate-update-manifest.mjs <资源目录> <版本> <仓库> <标签>"
  );
}

const directory = resolve(assetDirectory);
const files = await readdir(directory);
const macArchive = findOne(files, (name) => name.endsWith(".app.tar.gz"));
const windowsInstaller = findOne(files, (name) => name.endsWith("-setup.exe"));
const macSignature = await readSignature(directory, macArchive);
const windowsSignature = await readSignature(directory, windowsInstaller);
const releaseBase = `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}`;

const proxiedPlatform = (file, signature) => {
  const direct = `${releaseBase}/${encodeURIComponent(basename(file))}`;
  return {
    signature,
    url: `https://gh-proxy.com/${direct}`
  };
};

const windows = proxiedPlatform(windowsInstaller, windowsSignature);
const manifest = {
  version,
  notes: `CrossCopy ${version} 多设备共享与稳定性更新`,
  pub_date: new Date().toISOString(),
  platforms: {
    "darwin-universal": proxiedPlatform(macArchive, macSignature),
    "windows-x86_64": windows,
    "windows-x86_64-nsis": windows
  }
};

await writeFile(
  resolve(directory, "latest.json"),
  `${JSON.stringify(manifest, null, 2)}\n`,
  "utf8"
);

function findOne(entries, predicate) {
  const matches = entries.filter(predicate);
  if (matches.length !== 1) {
    throw new Error(`发布资源匹配数量应为 1，实际为 ${matches.length}: ${matches.join(", ")}`);
  }
  return matches[0];
}

async function readSignature(directoryPath, file) {
  return (await readFile(resolve(directoryPath, `${file}.sig`), "utf8")).trim();
}
