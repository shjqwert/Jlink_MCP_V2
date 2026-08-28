import assert from "node:assert/strict";
import { promises as fs } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const HERE = dirname(fileURLToPath(import.meta.url));
const STATE_PATH =
  process.env.F0D_STATE_PATH ?? resolve(tmpdir(), `jlink-mcp-f0d-${process.pid}.json`);
const EVIDENCE_PATH = process.env.F0D_EVIDENCE_PATH;
const EXPECTED_TOOLS = [
  "jlink_control",
  "jlink_hss",
  "jlink_inspect",
  "jlink_program",
  "jlink_target",
  "jlink_write",
];

async function removeIfPresent(path) {
  try {
    await fs.unlink(path);
  } catch (error) {
    if (error?.code !== "ENOENT") {
      throw error;
    }
  }
}

async function connectClient(name) {
  const client = new Client({ name, version: "0.1.0" });
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [resolve(HERE, "server.mjs")],
    env: { ...process.env, F0D_STATE_PATH: STATE_PATH },
    stderr: "pipe",
  });
  await client.connect(transport);
  return client;
}

async function packageVersion(name) {
  const path = resolve(HERE, "node_modules", ...name.split("/"), "package.json");
  return JSON.parse(await fs.readFile(path, "utf8")).version;
}

await removeIfPresent(STATE_PATH);
await removeIfPresent(`${STATE_PATH}.partial`);

const checks = [];
const first = await connectClient("f0d-selftest-first");
const toolsResult = await first.listTools();
const tools = [...toolsResult.tools].sort((left, right) => left.name.localeCompare(right.name));
assert.deepEqual(
  tools.map((tool) => tool.name),
  EXPECTED_TOOLS,
);
for (const tool of tools) {
  assert.ok(tool.inputSchema, `${tool.name} inputSchema missing`);
  assert.ok(tool.outputSchema, `${tool.name} outputSchema missing`);
  assert.equal(tool.inputSchema.type, "object", `${tool.name} input schema is not an object`);
  assert.equal(
    tool.inputSchema.additionalProperties,
    false,
    `${tool.name} input schema accepts undeclared fields`,
  );
  assert.equal(tool.outputSchema.type, "object", `${tool.name} output schema is not an object`);
  assert.equal(
    tool.outputSchema.additionalProperties,
    false,
    `${tool.name} output schema accepts undeclared fields`,
  );
}
checks.push("six_tool_discovery", "closed_input_and_output_schemas");

const invalidInput = await first.callTool({
  name: "jlink_target",
  arguments: { action: "status", undeclared: true },
});
assert.equal(invalidInput.isError, true);
assert.match(invalidInput.content[0].text, /Invalid arguments/);
checks.push("undeclared_field_rejected");

const status = await first.callTool({
  name: "jlink_target",
  arguments: { action: "status" },
});
assert.deepEqual(status.structuredContent, { connection: "connected", state: "running" });
assert.deepEqual(status.content, []);
checks.push("structured_content_consumed");

const errorResult = await first.callTool({
  name: "jlink_inspect",
  arguments: { action: "variable", path: "motor.sped" },
});
assert.equal(errorResult.isError, true);
assert.equal(errorResult.structuredContent.error.code, "SYMBOL_NOT_FOUND");
assert.match(errorResult.content[0].text, /^SYMBOL_NOT_FOUND:/);
checks.push("tool_error_consumed");

const started = await first.callTool({
  name: "jlink_hss",
  arguments: {
    action: "start",
    capture_key: "f0d-recovery-001",
    duration_s: 1,
    rate_hz: 1000,
    variables: [{ path: "motor.speed" }],
    return_when: "started",
  },
});
assert.deepEqual(started.structuredContent, { capture_id: "cap_f0d_001", state: "running" });

const overview = await first.callTool({
  name: "jlink_hss",
  arguments: { action: "query", capture_key: "f0d-recovery-001", view: "overview" },
});
const resourceLink = overview.content.find((item) => item.type === "resource_link");
assert.equal(resourceLink?.uri, "jlink-mcp://capture/cap_f0d_001/raw");
const resource = await first.readResource({ uri: resourceLink.uri });
assert.equal(
  resource.contents[0].blob,
  Buffer.from("F0-D-MOCK-CAPTURE-v1").toString("base64"),
);
checks.push("resource_link_and_read");

const pageOne = await first.callTool({
  name: "jlink_hss",
  arguments: {
    action: "query",
    capture_key: "f0d-recovery-001",
    view: "changes",
    limit: 2,
  },
});
assert.equal(pageOne.structuredContent.truncated, true);
assert.equal(pageOne.structuredContent.changes.length, 2);
assert.equal(typeof pageOne.structuredContent.next_cursor, "string");
const pageTwo = await first.callTool({
  name: "jlink_hss",
  arguments: {
    action: "query",
    capture_key: "f0d-recovery-001",
    cursor: pageOne.structuredContent.next_cursor,
  },
});
assert.equal(pageTwo.structuredContent.truncated, false);
assert.equal(pageTwo.structuredContent.changes.length, 1);
assert.equal("next_cursor" in pageTwo.structuredContent, false);
checks.push("opaque_cursor_pagination");

await first.close();
await new Promise((resolveDelay) => setTimeout(resolveDelay, 300));
const second = await connectClient("f0d-selftest-second");
const recovered = await second.callTool({
  name: "jlink_hss",
  arguments: { action: "status", capture_key: "f0d-recovery-001" },
});
assert.equal(recovered.structuredContent.state, "completed");
assert.ok(recovered.structuredContent.elapsed_us >= 200_000);
checks.push("capture_key_recovery_after_server_restart");
await second.close();

const evidence = {
  verdict: "PASS",
  transport: "stdio",
  node_version: process.version,
  packages: {
    "@modelcontextprotocol/sdk": await packageVersion("@modelcontextprotocol/sdk"),
    zod: await packageVersion("zod"),
  },
  server: { name: "jlink-mcp-f0d-probe", version: "0.1.0" },
  tools: EXPECTED_TOOLS,
  checks,
};

if (EVIDENCE_PATH) {
  await fs.mkdir(dirname(EVIDENCE_PATH), { recursive: true });
  await fs.writeFile(EVIDENCE_PATH, `${JSON.stringify(evidence, null, 2)}\n`, "utf8");
}
console.log(JSON.stringify(evidence, null, 2));

await removeIfPresent(STATE_PATH);
