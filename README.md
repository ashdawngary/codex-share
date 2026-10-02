# codex-share

Share a masked, read-only live view of a local Codex thread. The host continues to use Codex normally; `codex-share` passively tails the rollout JSONL, projects it into a deliberately small public event schema, and serves an authenticated web view.

## Security model

Raw rollout records are never sent to the browser. The local process can project only:

- user and assistant message text;
- tool name plus started/completed state (never arguments or output), when activity is granted;
- structured file paths and patch content, when diffs are granted;
- coarse working/idle state for the in-thread live indicator.

The default conversation grant exposes messages plus that coarse turn state. It does not expose which tools are running. Internal instructions, reasoning records, environment/configuration, command arguments, command output, and unknown future record types are always dropped. Allowed message and diff text is then masked for home-directory paths, common token formats, secret assignments, URL credentials, and user-supplied literal values.

This is defense in depth, not a guarantee that arbitrary prose contains no sensitive information. Review what the agent is discussing before sharing it.

## Run

```console
cargo run --release -- --file ~/.codex/sessions/2026/10/01/rollout-....jsonl
```

By default, `codex-share` opens an interactive picker for recent threads. It shows each thread's ID, relative time, and first user-message preview; enter its number to share it or `q` to cancel:

```console
cargo run --release
```

Skip the picker and share the newest session:

```console
cargo run --release -- --latest
```

Show more than the default 12 recent threads:

```console
cargo run --release -- --recent 20
```

Grant only the additional information a viewer needs:

```console
cargo run --release -- --permission activity
cargo run --release -- --permission diffs
cargo run --release -- --permission activity-diffs
```

Permissions are enforced by the server for both the snapshot and live WebSocket stream. Each capability token is minted with its scope (`cs1_c_…`, `cs1_ca_…`, `cs1_cd_…`, or `cs1_cad_…`); changing that visible scope marker invalidates the token. No scope exposes command output, raw tool arguments, or reasoning. Diff scopes expose masked file paths and patch content, capped at 100 files and 256 KiB per event.

The web view opens on the newest events, follows the bottom while the viewer remains there, and progressively loads older retained events when they scroll toward the top. The retained window defaults to 2,000 normalized events and can be changed with `--history`.

With a tunnel CLI already installed:

```console
cargo run --release -- --tunnel cloudflare
cargo run --release -- --tunnel ngrok
```

Tunnel preflight runs before either CLI is started. Cloudflare Quick Tunnels need only `cloudflared` on your `PATH`. ngrok additionally needs an `NGROK_AUTHTOKEN` environment variable or an `authtoken:` entry in its normal configuration file; the check reports the missing requirement before starting a tunnel. This validates local setup only—credential validity and account limits are confirmed by the tunnel provider when it connects.

Check both providers without starting a share or invoking either tunnel client:

```console
cargo run --release -- --check-tunnels
```

Additional literal masking:

```console
cargo run --release -- --redact customer-name --redact internal.example.com
CODEX_SHARE_REDACT=customer-name,internal.example.com cargo run --release
```

The generated share token has 192 bits of entropy, lives in the URL path, is checked by the local server, and expires when the process exits. The server binds to `127.0.0.1:48123` by default.

## Architecture direction

`JsonlSource -> PublicEvent -> Masker -> SharedFeed -> HTTP/WebSocket`

The next ingestion adapter should speak the versioned App Server protocol and emit the same `PublicEvent` values. App Server schemas should be generated from the installed Codex binary so the adapter stays aligned with that exact CLI version.

The tunnel is similarly isolated behind `ExposureProvider`; ngrok and Cloudflare Quick Tunnels are initial subprocess implementations.
