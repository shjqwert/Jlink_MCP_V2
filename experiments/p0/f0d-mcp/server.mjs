import { promises as fs } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, resolve } from "node:path";

import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { z } from "zod";

const SERVER_NAME = "jlink-mcp-f0d-probe";
const SERVER_VERSION = "0.1.0";
const CAPTURE_ID = "cap_f0d_001";
const RESOURCE_URI = `jlink-mcp://capture/${CAPTURE_ID}/raw`;
const RESOURCE_MIME = "application/vnd.jlink-mcp.capture.v1+binary";
const NEXT_CURSOR = "f0d:v1:changes:page-2";
const STATE_PATH = resolve(
  process.env.F0D_STATE_PATH ?? resolve(tmpdir(), "jlink-mcp-f0d-state.json"),
);

const errorSchema = z
  .object({
    code: z.string(),
    message: z.string(),
    retryable: z.boolean(),
    details: z.record(z.unknown()).optional(),
  })
  .strict();
const targetInput = z
  .object({
    action: z.enum(["connect", "disconnect", "status", "validate", "config_get", "config_set"]),
    scope: z.enum(["project", "user"]).optional(),
    values: z
      .object({
        "target.device": z.string().optional(),
        "target.interface": z.enum(["swd", "jtag"]).optional(),
        "target.speed_khz": z.number().int().positive().optional(),
        "symbols.elf": z.string().optional(),
        "jlink.dll_path": z.string().optional(),
        "jlink.dll_version": z.string().optional(),
        "jlink.dll_sha256": z.string().optional(),
        "capture.max_bytes": z.number().int().positive().optional(),
        "probe.serial": z.string().optional(),
      })
      .strict()
      .optional(),
  })
  .strict();
const targetOutput = z
  .object({
    notices: z.array(z.string()).optional(),
    connection: z.enum(["disconnected", "connecting", "connected", "faulted"]).optional(),
    state: z.enum(["running", "halted", "hardfault", "unknown"]).optional(),
    dll: z.record(z.unknown()).optional(),
    probe: z.record(z.unknown()).optional(),
    target: z.record(z.unknown()).optional(),
    effective: z.record(z.unknown()).optional(),
    sources: z.record(z.string()).optional(),
    error: errorSchema.optional(),
  })
  .strict();

const programInput = z
  .object({
    action: z.enum(["flash", "erase", "verify"]),
    image: z.string().optional(),
    verify: z.boolean().optional(),
    after: z.enum(["none", "reset_halt", "reset_run"]).optional(),
    address: z.string().regex(/^0x[0-9a-fA-F]+$/).optional(),
    length: z.number().int().positive().optional(),
  })
  .strict();
const emptyOrErrorOutput = z.object({ error: errorSchema.optional() }).strict();

const sliceSchema = z
  .object({ start: z.number().int().nonnegative(), count: z.number().int().positive() })
  .strict();
const typedValue = z.union([
  z.boolean(),
  z.number(),
  z.string(),
  z.array(z.unknown()),
  z.record(z.unknown()),
]);
const inspectInput = z
  .object({
    action: z.enum(["variable", "memory", "register", "symbols"]),
    path: z.string().min(1).optional(),
    slice: sliceSchema.optional(),
    address: z.string().regex(/^0x[0-9a-fA-F]+$/).optional(),
    length: z.number().int().min(1).max(4096).optional(),
    name: z.string().min(1).optional(),
    query: z.string().min(1).optional(),
    limit: z.number().int().min(1).max(50).optional(),
  })
  .strict();
const inspectOutput = z
  .object({
    value: typedValue.optional(),
    data: z.string().regex(/^[0-9a-f]*$/).optional(),
    symbols: z.array(z.string()).optional(),
    error: errorSchema.optional(),
  })
  .strict();

const writeInput = z
  .object({
    action: z.enum(["variable", "memory", "register"]),
    path: z.string().min(1).optional(),
    value: typedValue.optional(),
    verify: z.enum(["none", "readback"]).optional(),
    address: z.string().regex(/^0x[0-9a-fA-F]+$/).optional(),
    data: z.string().regex(/^(?:[0-9a-fA-F]{2}){1,4096}$/).optional(),
    name: z.string().min(1).optional(),
  })
  .strict();
const controlInput = z
  .object({
    action: z.enum(["halt", "resume", "reset", "step"]),
    after: z.enum(["run", "halt"]).optional(),
  })
  .strict();

const selectorSchema = z
  .object({ path: z.string().min(1), slice: sliceSchema.optional() })
  .strict();
const ruleSchema = z
  .object({
    id: z.string().min(1),
    path: z.string().min(1),
    kind: z.enum(["abs_delta_gte", "outside", "equals", "crosses"]),
    value: typedValue.optional(),
    min: z.number().optional(),
    max: z.number().optional(),
    direction: z.enum(["up", "down", "either"]).optional(),
  })
  .strict();
const hssInput = z
  .object({
    action: z.enum(["start", "status", "query"]),
    capture_id: z.string().min(1).optional(),
    capture_key: z.string().min(1).optional(),
    duration_s: z.number().int().min(1).max(300).optional(),
    rate_hz: z.number().int().min(1).max(1000).optional(),
    variables: z.array(selectorSchema).min(1).max(10).optional(),
    return_when: z.enum(["started", "completed"]).optional(),
    view: z.enum(["overview", "changes", "window", "around_event"]).optional(),
    cursor: z.string().min(1).optional(),
    limit: z.number().int().min(1).max(1000).optional(),
    series: z.array(z.string().min(1)).optional(),
    from_us: z.number().int().nonnegative().optional(),
    to_us: z.number().int().positive().optional(),
    event_id: z.string().min(1).optional(),
    before_us: z.number().int().nonnegative().optional(),
    after_us: z.number().int().nonnegative().optional(),
    mode: z.enum(["raw", "min_max", "first_last", "transitions"]).optional(),
    points: z.number().int().positive().optional(),
    rules: z.array(ruleSchema).optional(),
  })
  .strict();
const hssOutput = z
  .object({
    capture_id: z.string().optional(),
    state: z.enum(["starting", "running", "stopping", "completed", "failed", "aborted"]).optional(),
    elapsed_us: z.number().int().nonnegative().optional(),
    dictionary: z.record(z.string()).optional(),
    changes: z.array(z.record(z.unknown())).optional(),
    variables: z.array(z.record(z.unknown())).optional(),
    events: z.number().int().nonnegative().optional(),
    truncated: z.boolean().optional(),
    next_cursor: z.string().optional(),
    error: errorSchema.optional(),
  })
  .strict();

function toolError(code, message, retryable, details) {
  const error = { code, message, retryable };
  if (details !== undefined) {
    error.details = details;
  }
  return {
    isError: true,
    content: [{ type: "text", text: `${code}: ${message}` }],
    structuredContent: { error },
  };
}

function structured(value, content = []) {
  return { content, structuredContent: value };
}

async function loadCapture() {
  try {
    return JSON.parse(await fs.readFile(STATE_PATH, "utf8"));
  } catch (error) {
    if (error?.code === "ENOENT") {
      return undefined;
    }
    throw error;
  }
}

async function storeCapture(capture) {
  await fs.mkdir(dirname(STATE_PATH), { recursive: true });
  const temporary = `${STATE_PATH}.partial`;
  await fs.writeFile(temporary, `${JSON.stringify(capture)}\n`, "utf8");
  await fs.rename(temporary, STATE_PATH);
}

async function currentCapture() {
  const capture = await loadCapture();
  if (capture?.state === "running" && Date.now() >= capture.ready_at_ms) {
    capture.state = "completed";
    await storeCapture(capture);
  }
  return capture;
}

function buildServer() {
  const server = new McpServer(
    { name: SERVER_NAME, version: SERVER_VERSION },
    { instructions: "F0-D protocol capability probe only. This mock never accesses J-Link hardware." },
  );

  server.registerTool(
    "jlink_target",
    {
      description: "F0-D mock for target connection, status, validation, and configuration.",
      inputSchema: targetInput,
      outputSchema: targetOutput,
      annotations: {
        readOnlyHint: false,
        destructiveHint: false,
        idempotentHint: false,
        openWorldHint: false,
      },
    },
    async ({ action }) => {
      if (action === "status") {
        return structured({ connection: "connected", state: "running" });
      }
      if (action === "validate") {
        return structured({
          dll: { version: "6.98a", sha256_match: true, exports: "complete" },
          probe: { serial: "f0d-mock", hss: true },
          target: { reachable: true },
        });
      }
      if (action === "config_get") {
        return structured({
          effective: {
            "target.device": "S32K144",
            "target.interface": "swd",
            "target.speed_khz": 4000,
          },
          sources: {
            "target.device": "project",
            "target.interface": "project",
            "target.speed_khz": "project",
          },
        });
      }
      return structured({});
    },
  );

  server.registerTool(
    "jlink_program",
    {
      description: "F0-D mock for flash, erase, and verify. No hardware is modified.",
      inputSchema: programInput,
      outputSchema: emptyOrErrorOutput,
      annotations: {
        readOnlyHint: false,
        destructiveHint: true,
        idempotentHint: false,
        openWorldHint: false,
      },
    },
    async () => structured({}),
  );

  server.registerTool(
    "jlink_inspect",
    {
      description: "F0-D mock for variable, memory, register, and symbol inspection.",
      inputSchema: inspectInput,
      outputSchema: inspectOutput,
      annotations: {
        readOnlyHint: true,
        destructiveHint: false,
        idempotentHint: true,
        openWorldHint: false,
      },
    },
    async ({ action, path, query }) => {
      if (action === "variable" && path === "motor.sped") {
        return toolError(
          "SYMBOL_NOT_FOUND",
          "Variable 'motor.sped' was not found",
          false,
          { suggestions: ["motor.speed"] },
        );
      }
      if (action === "variable") {
        return structured({ value: 1200 });
      }
      if (action === "memory") {
        return structured({ data: "78563412" });
      }
      if (action === "register") {
        return structured({ value: "0x00001001" });
      }
      return structured({ symbols: [`${query}.state`, `${query}.speed`] });
    },
  );

  for (const [name, description, inputSchema, annotations] of [
    [
      "jlink_write",
      "F0-D mock for variable, memory, and register writes. No hardware is modified.",
      writeInput,
      { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
    ],
    [
      "jlink_control",
      "F0-D mock for halt, resume, reset, and step. No hardware is modified.",
      controlInput,
      { readOnlyHint: false, destructiveHint: true, idempotentHint: false, openWorldHint: false },
    ],
  ]) {
    server.registerTool(
      name,
      { description, inputSchema, outputSchema: emptyOrErrorOutput, annotations },
      async () => structured({}),
    );
  }

  server.registerTool(
    "jlink_hss",
    {
      description: "F0-D mock for fixed-duration capture, status recovery, and paginated query.",
      inputSchema: hssInput,
      outputSchema: hssOutput,
      annotations: {
        readOnlyHint: false,
        destructiveHint: true,
        idempotentHint: true,
        openWorldHint: false,
      },
    },
    async (input) => {
      if (input.action === "start") {
        const existing = await currentCapture();
        if (existing && existing.capture_key !== input.capture_key) {
          return toolError(
            "CAPTURE_KEY_CONFLICT",
            "The F0-D mock has a different persisted capture key",
            false,
          );
        }
        const capture =
          existing ??
          {
            capture_id: CAPTURE_ID,
            capture_key: input.capture_key,
            state: "running",
            started_at_ms: Date.now(),
            ready_at_ms: Date.now() + 200,
          };
        await storeCapture(capture);
        return structured({ capture_id: capture.capture_id, state: capture.state });
      }

      const capture = await currentCapture();
      if (!capture) {
        return toolError("CAPTURE_NOT_FOUND", "No F0-D mock capture exists", false);
      }
      if (
        (input.capture_id && input.capture_id !== capture.capture_id) ||
        (input.capture_key && input.capture_key !== capture.capture_key)
      ) {
        return toolError("CAPTURE_NOT_FOUND", "The requested F0-D mock capture was not found", false);
      }
      if (input.action === "status") {
        const elapsedUs = Math.max(0, Date.now() - capture.started_at_ms) * 1000;
        return structured({ state: capture.state, elapsed_us: elapsedUs });
      }
      if (input.cursor !== undefined) {
        if (input.cursor !== NEXT_CURSOR) {
          return toolError("CURSOR_INVALID", "The pagination cursor is invalid", false);
        }
        return structured({
          dictionary: { s0: "motor.speed" },
          changes: [{ series: "s0", after_us: 2000, observed_by_us: 3000, from: 2, to: 3 }],
          truncated: false,
        });
      }
      if (input.view === "overview") {
        return structured(
          {
            capture_id: capture.capture_id,
            state: capture.state,
            variables: [{ path: "motor.speed", samples: 3, changes: 2 }],
            events: 0,
            truncated: false,
          },
          [
            {
              type: "resource_link",
              uri: RESOURCE_URI,
              name: `${capture.capture_id}-raw`,
              description: "Complete self-describing F0-D mock capture",
              mimeType: RESOURCE_MIME,
            },
          ],
        );
      }
      return structured({
        dictionary: { s0: "motor.speed" },
        changes: [
          { series: "s0", after_us: 0, observed_by_us: 1000, from: 0, to: 1 },
          { series: "s0", after_us: 1000, observed_by_us: 2000, from: 1, to: 2 },
        ],
        truncated: true,
        next_cursor: NEXT_CURSOR,
      });
    },
  );

  server.registerResource(
    "f0d-capture-raw",
    RESOURCE_URI,
    { description: "Complete self-describing F0-D mock capture", mimeType: RESOURCE_MIME },
    async (uri) => ({
      contents: [
        {
          uri: uri.href,
          mimeType: RESOURCE_MIME,
          blob: Buffer.from("F0-D-MOCK-CAPTURE-v1", "utf8").toString("base64"),
        },
      ],
    }),
  );

  return server;
}

const server = buildServer();
const transport = new StdioServerTransport();
await server.connect(transport);
console.error(`${SERVER_NAME} ${SERVER_VERSION} listening on stdio`);
