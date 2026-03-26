# omnidaemon API Reference

## Protocol

Line-delimited JSON over Unix domain socket (`~/.omnidea/daemon.sock`) or Named Pipe (`\\.\pipe\omnidea-daemon`).

### Authentication

The first message on any connection is a **handshake**:

```json
{"auth": "<hex-token>", "client_type": "beryllium", "program_id": null}
```

The token is read from `~/.omnidea/auth.token` (generated at daemon boot, `0600` permissions). Client types: `beryllium`, `tray`, `cli`, `program`. Programs have restricted permissions (cannot call `daemon.stop`, `crown.delete`, etc.).

Response:

```json
{"auth": "ok", "session_id": "abc123", "client_type": "beryllium"}
```

### Request Format

```json
{"id": 1, "method": "crown.state", "params": {}}
```

- `id` — monotonically increasing integer, used to match responses.
- `method` — dotted namespace (e.g., `crown.create`, `idea.list`, `daemon.ping`).
- `params` — **always a JSON object**, never an array. Fields are named.

### Response Format

Success:
```json
{"id": 1, "result": {"exists": true, "unlocked": true}}
```

Error:
```json
{"id": 1, "error": {"code": -32601, "message": "unknown method: foo.bar"}}
```

### Push Events (server-initiated)

```json
{"event": "peer.connected", "data": {"pubkey": "cpub1..."}}
```

No `id` field — these are fire-and-forget from the daemon.

## Params Convention

**All params are JSON objects with named fields.** This is the Equipment Phone handler convention. Never send arrays.

### Correct

```json
{"id": 1, "method": "idea.create", "params": {"type": "text", "title": "My Note", "content": "Hello"}}
{"id": 2, "method": "idea.load", "params": {"id": "550e8400-e29b-41d4-a716-446655440000"}}
{"id": 3, "method": "idea.list", "params": {"extended_type": "text"}}
```

### Wrong (legacy orchestrator format — do not use)

```json
{"id": 1, "method": "idea.create", "params": ["text", "My Note", "Hello"]}
{"id": 2, "method": "idea.load", "params": ["550e8400-e29b-41d4-a716-446655440000"]}
```

## Error Codes

| Code | Meaning |
|------|---------|
| -32700 | Parse error (invalid JSON) |
| -32601 | Method not found |
| -1 | Handler error (check `message` for details) |
| -2 | Serialization error |
| -5 | Permission denied (Program client calling restricted method) |
| -6 | Authentication failed |

## Methods

### Daemon

| Method | Params | Returns |
|--------|--------|---------|
| `daemon.ping` | `{}` | `{"pong": true}` |
| `daemon.version` | `{}` | `{"daemon": "0.1.0", "op_count": 524, "equipment_ready": true}` |
| `daemon.health` | `{}` | `{"healthy": true, "equipment_ready": true, "omnibus_running": true, ...}` |
| `daemon.status` | `{}` | Full status object (crown, omnibus, tower, pid) |
| `daemon.stop` | `{}` | `{"ok": true}` — shuts down the daemon |

### Crown (Identity)

| Method | Params | Returns |
|--------|--------|---------|
| `crown.state` | `{}` | `{"exists": bool, "unlocked": bool, "crown_id": str?, "display_name": str?}` |
| `crown.create` | `{"name": "Display Name"}` | `{"crown_id": "cpub1..."}` |
| `crown.lock` | `{}` | `{"locked": true}` |
| `crown.unlock` | `{}` | `{"unlocked": true}` |
| `crown.profile` | `{}` | Profile object (requires unlocked) |
| `crown.update_profile` | `{"display_name": "New Name"}` | `{"ok": true}` |
| `crown.set_status` | `{"online": true}` | `{"ok": true, "online": true}` |

### Ideas (Content)

| Method | Params | Returns |
|--------|--------|---------|
| `idea.create` | `{"type": "text", "title": "My Note", "content": "Hello"}` | ManifestEntry |
| `idea.list` | `{}` or `{"extended_type": "text", "title_contains": "foo", "creator": "cpub1..."}` | `[ManifestEntry, ...]` |
| `idea.load` | `{"id": "uuid"}` | IdeaPackage |
| `idea.save` | `{"id": "uuid", "package": IdeaPackage}` | `{"ok": true}` |
| `idea.delete` | `{"id": "uuid"}` | `{"ok": true}` |
| `idea.search` | `{"query": "search terms", "limit": 20}` | `[SearchHit, ...]` |

### Vault

| Method | Params | Returns |
|--------|--------|---------|
| `vault.status` | `{}` | `{"unlocked": bool}` |

### Equipment

| Method | Params | Returns |
|--------|--------|---------|
| `op.list` | `{}` | `["crown.state", "idea.create", ...]` (all registered ops) |
| `op.has` | `{"op": "idea.create"}` | `{"exists": true}` |
| `op.count` | `{}` | `{"count": 524}` |

### Identity Aliases (backward compat)

| Method | Maps to |
|--------|---------|
| `identity.create` | `crown.create` |
| `identity.load` | Loads identity from path |
| `identity.profile` | `crown.profile` |
| `identity.pubkey` | Returns public key |

## Pipeline Bridge

Programs call the daemon through the bridge, which wraps calls in a pipeline:

```json
{
  "source": "tome",
  "steps": [
    {"id": "s1", "op": "idea.create", "input": {"type": "text", "title": "Note"}}
  ]
}
```

The bridge's `handle_run` iterates steps, calling `dispatch_op` for each. Most ops fall through to `daemon_call(op, input)` which forwards directly to the daemon. The `input` field **must be a JSON object** — it becomes the `params` in the daemon request.

## TypeScript Usage

```typescript
// Via the pipeline bridge (from a Program)
const result = await window.omninet.run(JSON.stringify({
  source: 'my-program',
  steps: [
    { id: 'step', op: 'idea.create', input: { type: 'text', title: 'Hello', content: 'World' } }
  ]
}));

// Via platform capabilities
const state = await window.omninet.platform('crown.state', '{}');
```
