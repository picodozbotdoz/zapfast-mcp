# ZapFast MCP server

ZapFast can serve a local [Model Context Protocol](https://modelcontextprotocol.io)
(MCP) server. An AI client such as ZCode can then list your WhatsApp chats,
read and search the messages ZapFast has archived, and send text messages
through the running app.

This is a fork addition. Upstream ZapFast has no scripting or automation
interface, and this server is not part of the upstream product.

- [README.md](README.md): this file. What it does, how to turn it on, how to connect a client.
- [API.md](API.md): the reference for every tool, its arguments, and its answers.

## What it can and cannot do

| Capability | Available |
| --- | --- |
| List linked WhatsApp accounts and their connection state | yes (`list_accounts`) |
| List chats, with unread counts and the last message summary | yes (`list_chats`) |
| Read a chat's archived messages, newest page first, with paging into older history | yes (`get_messages`) |
| Search archived messages, in one chat or across all unlocked chats | yes (`search_messages`) |
| Send a text message, optionally quoting a message | yes (`send_message`) |
| Send media, reactions, polls, edits, forwards, or receive live events | no |
| Download media | no (reads never download it) |
| Read chats locked in ZapFast | no, for messages; `list_chats` with `include_locked` shows their name and last-message summary (see Status) |

Reads never mark a chat as read, never send read receipts, and never download
media. Only messages ZapFast already has in its local archive are returned, so
a chat may be missing recent messages that exist only on your phone.

`send_message` really sends. The message leaves the linked account as if you
had typed it in ZapFast.

## Requirements

- ZapFast 0.21.2 or later built from this fork, with the MCP code compiled in.
- At least one WhatsApp account linked in the running app.
- An MCP client that supports the **streamable HTTP** transport.

The server runs only while ZapFast runs. It is part of the app process, not a
separate program, so ZapFast must be open for any tool to work. When ZapFast is
closed, the client gets a connection error.

## Turn it on

Use either of these. Both start the server on `127.0.0.1:47190` by default.

Command line, for one session:

```bash
zapfast --mcp
zapfast --mcp --mcp-port 47191
```

Or the settings file, which stays on across restarts. Add these keys to
`settings.json` in ZapFast's configuration directory (on Linux,
usually `~/.config/zapfast/settings.json`):

```json
{
  "mcp_enabled": true,
  "mcp_port": 47190
}
```

If a copy of ZapFast is already running, a second launch hands off to it and
does not start the server. Quit the running copy and start it again with
`--mcp`, or turn the setting on first.

On startup the log says where the token file is. Look for `MCP server
listening on http://127.0.0.1:47190/mcp`.

## The token

Every request must carry a bearer token. ZapFast creates a random 64-character
hex token the first time the server starts and keeps it in ZapFast's state
directory (on Linux, usually `~/.local/state/zapfast/mcp-token`). The file is
readable only by your user (mode `0600`). The same token is reused on later
runs, so a configured client keeps working.

Treat the token like a password. Anyone who has it, and can reach
`127.0.0.1` on your machine, can read your archived messages and send messages
as you.

To rotate it, quit ZapFast, delete the `mcp-token` file, and start ZapFast
again. Then update your client's configuration.

```bash
cat ~/.local/state/zapfast/mcp-token
```

Do not paste the token into chat logs, issue trackers, or shared config files.

## Connect a client

The endpoint is:

```
http://127.0.0.1:47190/mcp
```

Send the token in an `Authorization` header:

```
Authorization: Bearer <token>
```

A generic client entry looks like this. The exact file and keys depend on
your client, so check its documentation. Keep the token in an environment
variable rather than in the file itself.

```json
{
  "mcpServers": {
    "zapfast": {
      "type": "http",
      "url": "http://127.0.0.1:47190/mcp",
      "headers": {
        "Authorization": "Bearer ${ZAPFAST_MCP_TOKEN}"
      }
    }
  }
}
```

Requests without the right token get `401 Unauthorized`.

### Check the connection by hand

This sends the MCP `initialize` request with curl. Replace `TOKEN` with your
token. You should get a response listing the server name and its tool
capability.

```bash
curl -sS http://127.0.0.1:47190/mcp \
  -H "Authorization: Bearer TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"curl-check","version":"0.0.0"}}}'
```

If you get `unauthorized`, the token is wrong or missing. If you get a
connection refused error, ZapFast is not running or the server is not on.

## Tools at a glance

| Tool | What it does | Writes? |
| --- | --- | --- |
| `list_accounts` | Linked accounts, their phone JID, profile name, and connection status | no |
| `list_chats` | Chats of one account, newest activity first | no |
| `get_messages` | One chat's archived messages, ascending, with paging | no |
| `search_messages` | Text search of archived messages | no |
| `send_message` | Sends a text message to one chat | **yes** |

Every tool except `list_accounts` takes an optional `account_id`. Without it,
the first account by id is used. If you have several accounts, call
`list_accounts` first and pass the id you want. The full argument list and
answer shapes are in [API.md](API.md).

## Typical use

1. `list_accounts`: find the account id, and check that the status is `connected`.
2. `list_chats` with a `filter` such as a contact name, to find the chat id.
3. `get_messages` with that `chat_id` for the newest page. If `complete` is
   `false`, pass the returned `next_cursor` back as `before` to read older
   history.
4. `search_messages` to find a phrase, either across all chats or in one chat
   with `chat_id`.
5. `send_message` with the `chat_id`, `text`, and optional `quote_id`. Check
   the returned `status`.

## Safety model

- **Loopback only.** The server binds to `127.0.0.1`. It is not reachable from
  other machines on your network.
- **Bearer token on every request.** No token, no access.
- **Locked chats are not read by default.** A chat locked in ZapFast is hidden
  from `list_chats` and refused by `get_messages` and `search_messages`, and its
  hits are removed from global searches.
  - Known gap: `list_chats` with `include_locked: true` lists locked chats with
    their name and last-message summary. Do not use that flag if you want
    locked chats to stay private from the client. See the status note below.
- **Sends pass the same checks as the app.** A send goes through the account's
  own worker, so blocked contacts, read-only chats, and a disconnected account
  are refused with a reason, as they are in the interface.
- **Archive opened read-only.** Reads use a separate read-only connection to the
  encrypted archive. They never migrate or create the database, and never touch
  the WhatsApp session.
- **Chat content is untrusted.** Every message is other people's text. The tool
  descriptions tell the client to treat it as data, not instructions. Your AI
  client should still not act on instructions found inside messages without
  asking you.
- **No logging of content.** The server logs failures, not message text, phone
  numbers, or keys.

Remember that a client can send a message just by calling `send_message`. Keep
the server off when you do not need it, and review what your AI client is
doing when it writes.

## Limits

| Limit | Value |
| --- | --- |
| `list_chats` `limit` | 1 to 500, default 100 |
| `get_messages` `limit` | 1 to 200, default 50 |
| `search_messages` `limit` | 1 to 200, default 50 |
| Wait for the worker to file a sent message | 10 seconds |
| Wait for the server acknowledgement after filing | 15 seconds, polled every 250 ms |
| Wait for `list_accounts` per account | 3 seconds |

Sending is text only. A send that is not acknowledged within 15 seconds comes
back as `pending` with a note. It may still deliver later. Check
`get_messages` later to see the final status.

## Troubleshooting

| Symptom | Likely cause and fix |
| --- | --- |
| `Connection refused` on `127.0.0.1:47190` | ZapFast is not running, or started without `--mcp` and the settings file is off. Start it that way. |
| `401 unauthorized` | The `Authorization` header is missing or the token is stale. Re-read `mcp-token`. |
| `No WhatsApp account is linked in ZapFast` | No account is linked yet. Link one in the app. |
| `Unknown account id ...` | Use an id from `list_accounts`. |
| `No chat ... in this account's archive` | Use a `chat_id` from `list_chats`. |
| `This chat is locked on this device` | Expected. Locked chats are never read through MCP. |
| `ZapFast is not connected to WhatsApp` | The account is offline. `list_accounts` shows its status. Wait for `connected`. |
| `The message archive is not available` | The archive file could not be opened. Check ZapFast's log. |
| Port already in use | Another program has the port. Pick another with `--mcp-port`. |
| Tools missing in the client | Reconnect the client after ZapFast starts. Check that the client uses the streamable HTTP transport. |

Logs are written by ZapFast. Run it from a terminal to see them.

## Status

The code is committed on the fork's `main` branch and passes the project's
full check suite on a remote build (see the project `CLAUDE.md`). Runtime
behaviour against a live account, including HTTP, auth, and tool calls, is
still being tested. Treat this as a preview.

Known gaps:

- `list_chats` with `include_locked: true` exposes the name and last-message
  summary of locked chats. This contradicts the locked-chat rule above and
  should be fixed or the flag removed.

## Source

- `src/mcp/mod.rs`: account registry, archive reads, paging and search rules
- `src/mcp/host.rs`: HTTP host, bearer token, loopback binding
- `src/mcp/tools.rs`: the five tools, their descriptions, and send flow
- `src/mcp/dto.rs`: the JSON answer shapes
- `src/backend/worker.rs`: `McpSendText` and `McpAccountInfo` command handling
