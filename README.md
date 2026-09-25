# MultiMCP

One binary, many MCP servers. Each JSON file in `<config_dir>/MultiMCP/`
defines an MCP server whose tools are shell commands with typed parameters.
The binary serves one file at a time over stdio:

```sh
multimcp init backup   # creates <config_dir>/MultiMCP/backup.json with an example
multimcp backup        # serves it
```

`<config_dir>` is `$XDG_CONFIG_HOME` on Linux (`~/.config` by default),
with platform equivalents elsewhere. There is intentionally no `--config`
flag and no server discovery: the name is required, and the file must exist.

## Config format

One file per server, JSON only. Scaffold one with `multimcp init <name>`
(it refuses to overwrite an existing file) and edit the commands:

```json
{
  "name": "backup",
  "instructions": "Optional instructions shown to the model.",
  "commands": [
    {
      "name": "greet",
      "description": "Print a greeting",
      "argv": ["echo", "hello {who}"],
      "params": {
        "who": {"type": "string", "description": "Who to greet",
                "required": false, "default": "world"}
      },
      "timeout_secs": 30,
      "read_only": true
    }
  ]
}
```

Parameter types: `string`, `integer`, `number`, `boolean`, `enum`
(`enum` needs a `values` list). Parameters support `description`,
`required` (default `true`), `default`, and `minimum`/`maximum` for numbers.

`{placeholders}` in `argv` are substituted per call. Commands run with **no
shell** (argv array, like `exec`), so substitution cannot inject extra
commands. Use `{{` / `}}` for literal braces.

## Self-registration

Every server exposes a built-in `register_command` tool: give it a name,
description, binary path, arguments, and parameter definitions, and the new
tool is callable immediately and persisted to the server's file, so it
survives restarts. Pass `--disable-register` for a locked-down server.

## Client setup

Build once, then add one entry per server file:

```sh
cargo build --release
```

```json
"mcpServers": {
  "backup": {
    "command": "/path/to/multimcp",
    "args": ["backup"]
  },
  "media": {
    "command": "/path/to/multimcp",
    "args": ["media"]
  }
}
```

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Security notes

- Tools execute with the server's own privileges. Only serve config files
  you trust, and only expose the server to trusted clients.
- `register_command` lets callers add arbitrary persistent commands.
  Disable it with `--disable-register` when that is not wanted.
- Don't hand-edit a server file while that server is running; a
  registration write could clobber your edit.
