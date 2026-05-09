#!/usr/bin/env bun
import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { imageGenerate, imageGenerateInputSchema } from "./tools/image_generate.ts";
import { serverInfo } from "./tools/server_info.ts";

const server = new McpServer({
  name: "micu-image",
  version: "0.1.0-ts.0",
});

server.tool(
  "image_generate",
  "文本生成图像（text-to-image）。米醋代理 + gpt-image-2 系列。\n\n" +
    "[WHEN TO USE] 用户要画/生成/创建一张图且没提供任何参考图 → 用此 tool。\n" +
    "[SIZE] 留空让 MCP 从 prompt 关键字推断；强 LLM 已知偏好时直接显式传更准。" +
    "W/H 必须都是 8 的倍数。1K 档（≤2.25MP）会被代理压到 ~1.57MP；≥4MP 严格 1:1。\n" +
    "[N] 1-10。1K + non-pro 自动 5 并发；≥2K 强制 N=1（origin 限流）。",
  imageGenerateInputSchema,
  async (args) => {
    const result = await imageGenerate(args as Parameters<typeof imageGenerate>[0]);
    return {
      content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
      structuredContent: result as Record<string, unknown>,
      isError: !result.ok,
    };
  },
);

server.tool(
  "server_info",
  "诊断 / 能力查询：先调一次此 tool 拿到完整路由规则与 size 约束矩阵。",
  {},
  async () => {
    const info = serverInfo();
    return {
      content: [{ type: "text", text: JSON.stringify(info, null, 2) }],
      structuredContent: info,
    };
  },
);

const main = async (): Promise<void> => {
  const transport = new StdioServerTransport();
  await server.connect(transport);
};

main().catch((e) => {
  // stderr only — stdio transport 的 stdout 是协议通道，不能污染
  process.stderr.write(`fatal: ${(e as Error).stack ?? e}\n`);
  process.exit(1);
});
