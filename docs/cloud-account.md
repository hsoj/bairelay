# Cloud ("account device") cameras

> **Recommendation: don't use this.** bairelay is a local-first bridge — it talks to your camera over your own network and avoids depending on a vendor cloud. Binding a camera to a Reolink account works against that design. If you can, **remove the camera from your Reolink account** so it logs in with a normal local password, and configure it with the ordinary local discovery modes instead.
>
> Cloud login exists here for completeness — for cameras you cannot or will not unbind. It is **fragile**: it relies on Reolink's cloud API and a cloud-signed, short-lived token, both of which Reolink can change or revoke at any time without notice. When that happens, the camera stops connecting in bairelay until the implementation is updated. There is no fallback path.

## What an account device is

A Reolink camera becomes an **account device** when it is added to a Reolink account (through the Reolink app). In that state the camera refuses local-password logins and accepts only the **sigV3** login: a per-session cryptographic challenge plus a short-lived token that only the bound Reolink account can mint.

- **Created:** adding the camera to the Reolink account during onboarding in the app or at a later point binds it to that account.
- **Disconnected:** remove the device from the Reolink account in the Reolink app, or factory-reset the camera and set it up without adding it to an account. Once unbound, the camera logs in with a normal local password again — configure it with a regular `discovery` mode (`local`, `remote`, `map`, `relay`, …) and drop the cloud settings. **This is the recommended state.**

## Configuring cloud cameras

Place the Reolink **account** credentials once at the top level of `config.toml` (this is the account email and password you log into the Reolink app with — not a per-camera password):

```toml
cloud_account  = "you@example.com"
cloud_password = "your-reolink-password"
```

Then mark each cloud camera with `discovery = "cloud"` and its `uid`. A cloud camera needs a `username` but no local `password` — the cloud token authenticates it:

```toml
[[cameras]]
name      = "Backyard"
uid       = "9527000XXXXXXXXX"
username  = "admin"
discovery = "cloud"
```

Several cameras on the same account share the single top-level credential pair — add one `[[cameras]]` block per camera, each with `discovery = "cloud"` and its own `uid`:

```toml
[[cameras]]
name      = "Backyard"
uid       = "9527000XXXXXXXXX"
username  = "admin"
discovery = "cloud"

[[cameras]]
name      = "Driveway"
uid       = "9527000YYYYYYYYY"
username  = "admin"
discovery = "cloud"
```

`check-config` rejects a `cloud` camera that is missing `cloud_account`, `cloud_password`, or `uid`.

## Clearing login verification (`cloud-authorise`)

Reolink applies **login verification (MFA)** to cloud logins from new devices. This fires for the explicit Two-Step Verification toggle *or* for logins from a device or IP the account hasn't seen before — and that may apply to the host bairelay runs from. When it triggers, the token mint fails and bairelay reports:

> `8208 — the extra identification is required`

The `cloud-authorise` command has been implemented to solve this. It is a **one-time, interactive, per-host bootstrap**: run it once on the machine (and from the network) bairelay connects from, complete a single verification challenge interactively, and bairelay stores the resulting long-lived credential so every later cloud connect from that host should succeed headlessly.

### Running it

```bash
bairelay cloud-authorise
```

It reads `cloud_account` / `cloud_password` from the config, asks Reolink to issue a challenge, prints a prompt, and waits for you to paste the code:

```text
A verification code was emailed to you@example.com.
Enter the code: 123456
Authorised. Trust token stored at config-cloud-auth.json (valid ~30 days).
Cloud cameras on this host will now connect without prompting.
```

Choose the challenge method with `--method`:

| `--method`        | Where the code comes from                                             |
|-------------------|-----------------------------------------------------------------------|
| `email` (default) | A code Reolink emails to the account address.                         |
| `totp`            | Your authenticator app, if the account has an authenticator set up.   |
| `backup_code`     | One of the account's saved backup codes.                              |

### What it stores

The credential lands in **`config-cloud-auth.json`**, written next to your `config.toml` with `0600` permissions (owner-only — it holds account tokens, so treat it like a password). On every cloud connect bairelay loads this file automatically, matches it to `cloud_account`, checks it hasn't expired, and replays it to the token mint — there is no flag or config key to "turn it on". Reolink issues one or both of a **trust token** (~30 days, marks this host's IP trusted) and a **refresh token** (~90 days, refreshes the session without a fresh password login); bairelay uses whichever it gets. When the stored credential lapses the next connect fails with `8208` again and you need to re-run `cloud-authorise` (bairelay logs a warning naming the stale file).

### Per-host, per-network

The credential authorises **the host/network it was minted from**. Move bairelay to a different machine or a materially different outbound IP and Reolink may challenge again — just run `cloud-authorise` once on the new host. To avoid the whole dance: run bairelay from an IP Reolink already trusts, or **unbind the camera and use local login** (the recommendation at the top of this page).

## Gotchas and limitations

- **Single account.** All cloud cameras share the one top-level `cloud_account` / `cloud_password`. Cameras bound to different Reolink accounts can't be served from the same config.
- **Internet required at connect.** The token is short-lived and freshly minted on every connect, so each connection needs outbound HTTPS to Reolink's cloud. No internet → no login. The video itself still flows locally.
- **Reachable, not relayed.** bairelay talks to the camera directly — it never tunnels through Reolink's relay — so the host must be able to reach the camera (same LAN, or routed/VPN'd to it). On a host that can't *broadcast* to the camera (e.g. a server on another subnet or VPN), set the camera's `address` so discovery can unicast to it. The cloud is used only to mint the token, never to carry video.
- **Credentials in plaintext.** `cloud_account` / `cloud_password` sit in `config.toml` in plaintext, like other Reolink credentials — keep the file readable only by the service user.
- **Subject to change at Reolink's discretion.** The login depends on Reolink's cloud API shape, the sigV3 handshake, and the cloud-signed certificate chain. A firmware or server-side change can break it with no warning and no workaround short of a code update. It's best to treat cloud login as best-effort, not load-bearing.
