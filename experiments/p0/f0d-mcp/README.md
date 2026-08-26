# F0-D MCP client probe

This isolated mock server exercises the F0-D protocol surface without loading a
J-Link DLL or modifying target hardware. It exposes exactly the six V1 tool
names over stdio and persists one synthetic capture so a second client/server
process can recover it by `capture_key`.

## Run the protocol self-test

```powershell
pnpm install --frozen-lockfile
pnpm test
```

The self-test uses the official TypeScript MCP SDK as the client and verifies:

- six-tool discovery plus closed input and output object schemas;
- rejection of an undeclared input field;
- `structuredContent`, `resource_link`, resource reading, and `isError`;
- two-page opaque-cursor traversal; and
- `capture_key` recovery after closing the first client/server process.

## Register the isolated Windows Codex probe

```powershell
codex mcp add jlink_p0_v2_probe --env F0D_STATE_PATH=D:\Github\jlink-mcp-V2\validation\evidence\f0-d\codex-client-state.json -- C:\Users\usre\.cache\codex-runtimes\codex-primary-runtime\dependencies\node\bin\node.exe D:\Github\jlink-mcp-V2\experiments\p0\f0d-mcp\server.mjs
codex mcp get jlink_p0_v2_probe
```

The unique `jlink_p0_v2_probe` name prevents confusion with the existing `jlink`
server. Do not rename it to `jlink`, and do not use it as a production MCP. It
does not load a J-Link DLL or modify hardware. A newly opened Windows Codex
window loads the registered server.

Remove only the probe when P0 client testing is no longer needed:

```powershell
codex mcp remove jlink_p0_v2_probe
```
