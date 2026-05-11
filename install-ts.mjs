#!/usr/bin/env node
/**
 * 米醋画图 MCP TypeScript 版一键安装脚本。
 *
 * 与 Python 版 install.py 共存：默认写 mcpServers.micu-image-ts 名字，
 * 不会覆盖 Python 版的 micu-image 配置。
 *
 * 跨平台（macOS / Linux / Windows）：
 * - 检查 Node >= 20
 * - 跑 npm install（除非 --skip-install）
 * - 交互问 API key（隐藏输入）+ 输出目录
 * - 写 ~/.claude.json / ~/.codex/config.toml，command=node args=[tsx/dist/cli.mjs, src/index.ts]
 * - 同步设 MICU_SAVE_DIR_ROOT 沙箱根，避免自定义目录被沙箱拒
 * - 自检 server 能不能起来
 *
 * 用法：
 *   node install-ts.mjs
 *   node install-ts.mjs --skip-install                  # 已装过 npm 依赖，跳过
 *   node install-ts.mjs --baseurl https://...           # 覆盖 baseurl
 *   node install-ts.mjs --no-codex                      # 不写 Codex 配置
 *   node install-ts.mjs --no-claude                     # 不写 Claude 配置
 *   node install-ts.mjs --yes                           # 非交互, 全用环境变量
 *       MICU_API_KEY=... MICU_SAVE_DIR=... node install-ts.mjs --yes
 *   node install-ts.mjs --mcp-name micu-image           # 自定义 MCP server 名（默认 micu-image-ts）
 */

import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import process from "node:process";
import readline from "node:readline";
import { fileURLToPath } from "node:url";

const NODE_MIN = 20;
const DEFAULT_BASEURL = "https://www.micuapi.ai";
const DEFAULT_MCP_NAME = "micu-image-ts";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)));

// ---------- 日志 ----------
const info = (m) => console.log(`[..] ${m}`);
const ok = (m) => console.log(`[OK] ${m}`);
const warn = (m) => console.log(`[!!] ${m}`);
const step = (m) => console.log(`\n>>> ${m}`);
const fail = (m) => { console.error(`[ERR] ${m}`); process.exit(1); };

const maskKey = (k) => k.length <= 8 ? "***" : `${k.slice(0, 5)}...${k.slice(-4)}`;

// ---------- CLI 参数 ----------
const parseArgs = () => {
  const a = process.argv.slice(2);
  const args = {
    noClaude: false, noCodex: false, noSmoke: false, yes: false,
    skipInstall: false, baseurl: DEFAULT_BASEURL, mcpName: DEFAULT_MCP_NAME,
    help: false,
  };
  for (let i = 0; i < a.length; i++) {
    const v = a[i];
    if (v === "--no-claude") args.noClaude = true;
    else if (v === "--no-codex") args.noCodex = true;
    else if (v === "--no-smoke") args.noSmoke = true;
    else if (v === "--yes") args.yes = true;
    else if (v === "--skip-install") args.skipInstall = true;
    else if (v === "--baseurl") args.baseurl = a[++i] ?? DEFAULT_BASEURL;
    else if (v === "--mcp-name") args.mcpName = a[++i] ?? DEFAULT_MCP_NAME;
    else if (v === "--help" || v === "-h") args.help = true;
    else fail(`未知参数: ${v}`);
  }
  return args;
};

// ---------- 环境检查 ----------
const checkNode = () => {
  const major = Number(process.versions.node.split(".")[0]);
  if (major < NODE_MIN) {
    console.error(`[ERR] 需要 Node >= ${NODE_MIN}, 当前 ${process.versions.node}`);
    console.error("      下载: https://nodejs.org/");
    process.exit(1);
  }
  ok(`Node ${process.versions.node}`);
};

const checkRepoFiles = () => {
  const required = ["package.json", "src/index.ts", "src/tools/image_generate.ts"];
  for (const rel of required) {
    if (!existsSync(path.join(REPO_ROOT, rel))) {
      fail(`仓库缺文件 ${rel}（REPO_ROOT=${REPO_ROOT}）`);
    }
  }
  info(`仓库: ${REPO_ROOT}`);
};

// ---------- 子进程 ----------
const run = (cmd, args, opts = {}) => new Promise((resolve) => {
  const p = spawn(cmd, args, { stdio: "inherit", shell: false, ...opts });
  p.on("exit", (code) => resolve(code ?? 1));
  p.on("error", (e) => { console.error(`[ERR] spawn ${cmd}: ${e.message}`); resolve(1); });
});

const runCaptured = (cmd, args, opts = {}) => new Promise((resolve) => {
  const p = spawn(cmd, args, { stdio: ["ignore", "pipe", "pipe"], shell: false, ...opts });
  let stdout = ""; let stderr = "";
  p.stdout.on("data", (b) => { stdout += b.toString(); });
  p.stderr.on("data", (b) => { stderr += b.toString(); });
  p.on("exit", (code) => resolve({ code: code ?? 1, stdout, stderr }));
  p.on("error", (e) => resolve({ code: 1, stdout, stderr: e.message }));
});

// ---------- 装依赖 ----------
const installDeps = async () => {
  step("安装依赖（npm install）");
  const npmCmd = process.platform === "win32" ? "npm.cmd" : "npm";
  const code = await run(npmCmd, ["install"], { cwd: REPO_ROOT });
  if (code !== 0) fail("npm install 失败。确认 Node >= 20 + 能访问 npm registry。");
  ok("依赖就绪");
};

// ---------- tsx 路径解析 ----------
const resolveTsxCli = () => {
  const cli = path.join(REPO_ROOT, "node_modules", "tsx", "dist", "cli.mjs");
  if (!existsSync(cli)) {
    fail(`找不到 tsx CLI: ${cli}\n      先跑 'npm install' 或去掉 --skip-install`);
  }
  return cli;
};

// ---------- 交互输入 ----------
const ask = (prompt, def = "") => new Promise((resolve) => {
  const rl = readline.createInterface({ input: process.stdin, output: process.stdout });
  const suffix = def ? ` [${def}]` : "";
  rl.question(`${prompt}${suffix}: `, (v) => {
    rl.close();
    resolve(v.trim() || def);
  });
});

const askYesNo = async (prompt, def = true) => {
  const hint = def ? "[Y/n]" : "[y/N]";
  while (true) {
    const v = (await ask(`${prompt} ${hint}`, def ? "y" : "n")).toLowerCase();
    if (!v) return def;
    if (["y", "yes"].includes(v)) return true;
    if (["n", "no"].includes(v)) return false;
  }
};

// 隐藏输入（无 Node stdlib 等价，自己 raw mode 实现）
const askSecret = (prompt) => new Promise((resolve, reject) => {
  process.stdout.write(`${prompt}: `);
  const stdin = process.stdin;
  if (!stdin.isTTY || typeof stdin.setRawMode !== "function") {
    // 非 TTY（如管道输入）：直接读一行不隐藏
    const rl = readline.createInterface({ input: stdin, output: process.stdout });
    rl.question("", (v) => { rl.close(); resolve(v.trim()); });
    return;
  }
  const wasRaw = stdin.isRaw;
  stdin.setRawMode(true);
  stdin.resume();
  stdin.setEncoding("utf8");
  let buf = "";
  const cleanup = () => {
    stdin.setRawMode(wasRaw);
    stdin.pause();
    stdin.removeListener("data", onData);
  };
  const onData = (ch) => {
    if (ch === "\r" || ch === "\n") {
      cleanup();
      process.stdout.write("\n");
      resolve(buf);
    } else if (ch === "\x03") { // Ctrl-C
      cleanup();
      reject(new Error("用户取消"));
    } else if (ch === "\x04") { // Ctrl-D
      cleanup();
      process.stdout.write("\n");
      resolve(buf);
    } else if (ch === "\b" || ch === "\x7f") {
      if (buf.length > 0) buf = buf.slice(0, -1);
    } else {
      buf += ch;
    }
  };
  stdin.on("data", onData);
});

// ---------- 收集配置 ----------
const collectConfig = async (nonInteractive, baseurl) => {
  const defaultSave = path.join(homedir(), "Pictures", "micu-out");
  let apiKey; let saveDirRaw;

  if (nonInteractive) {
    apiKey = (process.env.MICU_API_KEY ?? "").trim();
    saveDirRaw = (process.env.MICU_SAVE_DIR ?? defaultSave).trim();
    if (!apiKey) fail("--yes 模式需要环境变量 MICU_API_KEY=sk-...");
  } else {
    console.log("\n=== 配置米醋 MCP（TS 版）===");
    info(`baseurl: ${baseurl}`);
    info("API key 在米醋后台拿: https://www.micuapi.ai");
    while (true) {
      apiKey = await askSecret("米醋 API key (sk-...)");
      if (!apiKey) { warn("API key 不能为空"); continue; }
      const preview = maskKey(apiKey);
      if (!apiKey.startsWith("sk-")) warn(`API key 不以 sk- 开头，可能粘错: ${preview}`);
      else info(`输入的 key: ${preview}`);
      if (await askYesNo("确认这个 key?", true)) break;
    }
    saveDirRaw = await ask("输出目录（生成的图存这里）", defaultSave);
  }

  const savePath = path.resolve(saveDirRaw.startsWith("~")
    ? path.join(homedir(), saveDirRaw.slice(1))
    : saveDirRaw);
  try {
    await mkdir(savePath, { recursive: true });
  } catch (e) {
    fail(`创建输出目录失败: ${savePath}\n${e.message}`);
  }
  ok(`输出目录: ${savePath}`);
  ok(`沙箱根目录: ${savePath}`);
  return { apiKey, saveDir: savePath, saveRoot: savePath };
};

const buildEnv = ({ apiKey, saveDir, saveRoot, baseurl }) => {
  const env = {
    MICU_API_KEY: apiKey,
    MICU_SAVE_DIR: saveDir,
    MICU_SAVE_DIR_ROOT: saveRoot,
  };
  if (baseurl !== DEFAULT_BASEURL) env.MICU_BASEURL = baseurl;
  return env;
};

// ---------- 备份 ----------
const backup = async (filePath) => {
  if (!existsSync(filePath)) return null;
  const stamp = new Date().toISOString().replace(/[-:T]/g, "").slice(0, 15);
  const bak = `${filePath}.bak.${stamp}`;
  await copyFile(filePath, bak);
  info(`备份: ${path.basename(filePath)} -> ${path.basename(bak)}`);
  return bak;
};

// ---------- 写 Claude 配置 ----------
const writeClaude = async (mcpName, tsxCli, indexTs, envDict) => {
  step("配置 Claude Code");
  const cfg = path.join(homedir(), ".claude.json");
  let data = {};
  if (existsSync(cfg)) {
    await backup(cfg);
    try {
      data = JSON.parse(await readFile(cfg, "utf8"));
    } catch (e) {
      warn(`现有 ~/.claude.json 不是合法 JSON: ${e.message}`);
      warn("备份已留，可手动修复后重跑，或加 --no-claude 跳过");
      fail("退出，避免破坏现有配置");
    }
    if (typeof data !== "object" || data === null || Array.isArray(data)) {
      fail("~/.claude.json 顶层不是 object，备份已留，请手动检查");
    }
  }
  data.mcpServers ??= {};
  if (data.mcpServers[mcpName]) info(`已存在 ${mcpName} 配置，覆盖`);
  data.mcpServers[mcpName] = {
    command: process.execPath, // 绝对 node 路径
    args: [tsxCli, indexTs],
    env: envDict,
  };
  await writeFile(cfg, JSON.stringify(data, null, 2) + "\n", "utf8");
  ok(`写入 ${cfg}`);
  return cfg;
};

// ---------- 写 Codex 配置 ----------
const writeCodex = async (mcpName, tsxCli, indexTs, envDict) => {
  step("配置 Codex CLI");
  const cfgDir = path.join(homedir(), ".codex");
  const cfg = path.join(cfgDir, "config.toml");
  await mkdir(cfgDir, { recursive: true });

  // JSON.stringify 输出的字符串字面量恰好也是合法 TOML basic string（含反斜杠转义）
  const tstr = (s) => JSON.stringify(s);
  const envInline = Object.entries(envDict)
    .map(([k, v]) => `${k} = ${tstr(v)}`)
    .join(", ");
  const block =
    `\n[mcp_servers.${mcpName}]\n` +
    `command = ${tstr(process.execPath)}\n` +
    `args = [${tstr(tsxCli)}, ${tstr(indexTs)}]\n` +
    `env = { ${envInline} }\n`;

  if (existsSync(cfg)) {
    const existing = await readFile(cfg, "utf8");
    if (existing.includes(`[mcp_servers.${mcpName}]`)) {
      warn(`已存在 [mcp_servers.${mcpName}] 节，跳过（避免破坏其他配置）`);
      warn(`如需更新：手动删旧节后重跑，或编辑 ${cfg}`);
      return cfg;
    }
    await backup(cfg);
    await writeFile(cfg, existing + block, "utf8");
  } else {
    await writeFile(cfg, block.replace(/^\n/, ""), "utf8");
  }
  ok(`写入 ${cfg}`);
  return cfg;
};

// ---------- 自检 ----------
const smokeTest = (tsxCli, indexTs, envDict) => new Promise((resolve) => {
  step("自检 server 启动");
  const env = { ...process.env, ...envDict };
  const initMsg = JSON.stringify({
    jsonrpc: "2.0", id: 1, method: "initialize",
    params: {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "installer-ts", version: "1" },
    },
  }) + "\n";

  const child = spawn(process.execPath, [tsxCli, indexTs], {
    env, stdio: ["pipe", "pipe", "pipe"], shell: false,
  });
  let stdout = ""; let stderr = "";
  const timeout = setTimeout(() => {
    try { child.kill(); } catch { /* ignore */ }
  }, 15000);

  child.stdout.on("data", (b) => { stdout += b.toString(); });
  child.stderr.on("data", (b) => { stderr += b.toString(); });
  child.on("error", (e) => {
    clearTimeout(timeout);
    warn(`启动失败: ${e.message}`);
    resolve();
  });
  child.on("exit", () => {
    clearTimeout(timeout);
    if (stdout.includes('"result"') && stdout.includes('"protocolVersion"')) {
      ok("server initialize 握手成功");
    } else {
      warn("server 没正常握手，但依赖装好了，可以重启客户端再试");
      if (stderr) info(`stderr 末 300 字:\n${stderr.slice(-300)}`);
    }
    resolve();
  });
  child.stdin.write(initMsg);
  child.stdin.end();
});

// ---------- 摘要 ----------
const summary = ({ envDict, claudeCfg, codexCfg, mcpName, tsxCli, indexTs }) => {
  console.log("\n=== 完成 ===");
  console.log(`  node        : ${process.execPath}`);
  console.log(`  tsx cli     : ${tsxCli}`);
  console.log(`  src/index.ts: ${indexTs}`);
  console.log(`  mcp name    : ${mcpName}（与 Python 版 micu-image 共存）`);
  console.log(`  api key     : ${maskKey(envDict.MICU_API_KEY ?? "")}`);
  console.log(`  save_dir    : ${envDict.MICU_SAVE_DIR ?? ""}`);
  console.log(`  save_root   : ${envDict.MICU_SAVE_DIR_ROOT ?? ""}`);
  if (envDict.MICU_BASEURL) console.log(`  baseurl     : ${envDict.MICU_BASEURL}`);
  if (claudeCfg) console.log(`  Claude 配置 : ${claudeCfg}`);
  if (codexCfg) console.log(`  Codex  配置 : ${codexCfg}`);
  console.log("\n下一步:");
  console.log("  1. 重启 Claude Code / Codex CLI");
  console.log(`  2. 让 LLM 说：'调用 ${mcpName} 的 server_info'`);
  console.log("  3. 看到 baseurl / 路由规则就装好了");
  console.log("\n注意：TS 版当前只移植了 image_generate + server_info；");
  console.log("      要 image_edit / batch_edit / multi_reference 还得用 Python 版（main 分支）。");
};

// ---------- main ----------
const printHelp = () => {
  console.log(`米醋画图 MCP TypeScript 版一键安装

用法：node install-ts.mjs [选项]

选项：
  --no-claude          不写 Claude Code 配置（~/.claude.json）
  --no-codex           不写 Codex CLI 配置（~/.codex/config.toml）
  --no-smoke           跳过 server 启动自检
  --skip-install       跳过 npm install（依赖已装时）
  --yes                非交互模式（读 env MICU_API_KEY / MICU_SAVE_DIR）
  --baseurl URL        覆盖米醋 baseurl（默认 ${DEFAULT_BASEURL}）
  --mcp-name NAME      MCP server 名（默认 ${DEFAULT_MCP_NAME}，与 Python 版的 micu-image 共存）
  -h, --help           显示帮助
`);
};

const main = async () => {
  const args = parseArgs();
  if (args.help) { printHelp(); return; }

  console.log("=== 米醋画图 MCP TypeScript 版一键安装 ===\n");
  checkNode();
  checkRepoFiles();

  if (!args.skipInstall) await installDeps();
  else info("跳过 npm install（--skip-install）");

  const tsxCli = resolveTsxCli();
  const indexTs = path.join(REPO_ROOT, "src", "index.ts");
  if (!existsSync(indexTs)) fail(`找不到 src/index.ts: ${indexTs}`);

  const cfg = await collectConfig(args.yes, args.baseurl);
  const envDict = buildEnv({ ...cfg, baseurl: args.baseurl });

  const claudeCfg = args.noClaude ? null : await writeClaude(args.mcpName, tsxCli, indexTs, envDict);
  const codexCfg = args.noCodex ? null : await writeCodex(args.mcpName, tsxCli, indexTs, envDict);

  if (!args.noSmoke) await smokeTest(tsxCli, indexTs, envDict);

  summary({ envDict, claudeCfg, codexCfg, mcpName: args.mcpName, tsxCli, indexTs });
};

main().catch((e) => {
  if (e?.message === "用户取消") {
    console.log("\n\n[!!] 用户取消。已写入的备份文件 (.bak.*) 保留供回滚。");
    process.exit(130);
  }
  console.error(`\n[ERR] ${e?.stack ?? e}`);
  process.exit(1);
});
