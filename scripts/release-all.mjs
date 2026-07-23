import { spawnSync } from "node:child_process";
import { mkdirSync } from "node:fs";
import { resolve } from "node:path";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    stdio: "inherit",
    shell: process.platform === "win32",
    ...options
  });
  if (result.status !== 0) {
    process.exit(result.status ?? 1);
  }
}

function output(command, args) {
  const result = spawnSync(command, args, {
    encoding: "utf8",
    shell: process.platform === "win32"
  });
  if (result.status !== 0) {
    process.stderr.write(result.stderr);
    process.exit(result.status ?? 1);
  }
  return result.stdout.trim();
}

if (spawnSync("gh", ["--version"], { stdio: "ignore" }).status !== 0) {
  console.error(
    "未找到 GitHub CLI。请先安装 gh 并执行 gh auth login，或在 GitHub Actions 页面点击“Run workflow”。"
  );
  process.exit(1);
}

const branch = output("git", ["branch", "--show-current"]);
if (!branch) {
  console.error("当前不在 Git 分支上，无法触发远程打包。");
  process.exit(1);
}

console.log(`正在触发 ${branch} 分支的 macOS 和 Windows 打包任务...`);
run("gh", ["workflow", "run", "release-all.yml", "--ref", branch]);

await new Promise((resolveDelay) => setTimeout(resolveDelay, 2500));

const runId = output("gh", [
  "run",
  "list",
  "--workflow",
  "release-all.yml",
  "--branch",
  branch,
  "--limit",
  "1",
  "--json",
  "databaseId",
  "--jq",
  ".[0].databaseId"
]);

if (!runId) {
  console.error("任务已触发，但暂时没有查到任务编号。请前往 GitHub Actions 页面查看。");
  process.exit(1);
}

console.log(`打包任务 #${runId} 已启动，正在等待两个平台完成...`);
run("gh", ["run", "watch", runId, "--exit-status"]);

const destination = resolve("release", runId);
mkdirSync(destination, { recursive: true });
run("gh", ["run", "download", runId, "--dir", destination]);

console.log(`\n打包完成，安装包已下载到：${destination}`);
