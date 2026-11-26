// 把插件提供的工具注册给 Pi；工具调用经 RPC 的 extension_ui 通道交给插件执行
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { readFileSync } from "node:fs";

type Spec = { name: string; label?: string; description: string; parameters: object };
type Reply = { text?: string; image?: string; error?: string };

const CALL = "droidbridge:call";

/** 读插件写好的工具清单 */
function loadTools(): Spec[] {
  const file = process.env.DB_PI_TOOLS;
  if (!file) return [];
  return JSON.parse(readFileSync(file, "utf8"));
}

/** 把插件的回复转成 Pi 的工具结果 */
function toResult(raw: string | undefined) {
  if (raw === undefined) throw new Error("插件没有返回结果");
  const r: Reply = JSON.parse(raw);
  if (r.error) throw new Error(r.error);
  const content: any[] = [];
  if (r.text) content.push({ type: "text", text: r.text });
  if (r.image) content.push({ type: "image", data: readFileSync(r.image).toString("base64"), mimeType: "image/png" });
  if (!content.length) content.push({ type: "text", text: "(空)" });
  return { content, details: {} };
}

export default function (pi: ExtensionAPI) {
  const tools = loadTools();
  const names = new Set(tools.map((t) => t.name));

  for (const t of tools) {
    pi.registerTool({
      name: t.name,
      label: t.label ?? t.name,
      description: t.description,
      parameters: t.parameters as any,
      async execute(_id, params, _signal, _onUpdate, ctx) {
        const raw = await ctx.ui.input(CALL, JSON.stringify({ tool: t.name, args: params }));
        return toResult(raw);
      },
    });
  }

  pi.on("session_start", () => pi.setActiveTools([...names]));

  pi.on("input", (e, ctx) => {
    if (e.text.trimStart().startsWith("/")) {
      ctx.ui.notify("不支持命令", "warning");
      return { action: "handled" };
    }
    return { action: "continue" };
  });

  pi.on("tool_call", (e) => {
    if (!names.has(e.toolName)) return { block: true, reason: `没有这个工具：${e.toolName}` };
  });

  if (process.env.DB_PI_DEBUG) {
    pi.on("before_agent_start", (_e, ctx) => {
      process.stderr.write("[db] tools=" + pi.getActiveTools().join(",") + "\n");
      process.stderr.write("[db] prompt=" + JSON.stringify(ctx.getSystemPrompt()) + "\n");
    });
  }
}
