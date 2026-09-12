# Shared agent credentials

**Incomplete against the product requirement:** connecting a provider once must let
authorized devices and newly provisioned cloud agents run without another interactive
provider login, including after the original device goes offline. The API-key implementation
below is groundwork, not completion of that requirement. Subscription authentication,
durable refresh ownership, and unattended worker authorization remain unimplemented.
See [the required cloud authentication flow](cloud-agent-auth-requirements.md).

The Agents settings page combines installation toggles, connected accounts, and available
usage meters. Each agent expands independently; account sync has its own compact management
panel. There is no device switcher. Shared accounts are visible on approved devices in the
same Zeron user/organization profile. Native subscription accounts remain marked **This device**.

## What is implemented

| Agent | Portable credentials | Launch boundary |
| --- | --- | --- |
| Claude Code | Anthropic API key | `ANTHROPIC_API_KEY`, managed `CLAUDE_CONFIG_DIR` |
| Codex | OpenAI API key, including explicit import from native API-key login | Managed `CODEX_HOME`, pinned OpenAI Responses provider, child environment |
| Cursor | SDK API key, including explicit import of the browser-minted native SDK key and its expiry | `CURSOR_API_KEY` |
| Devin | API key | `WINDSURF_API_KEY` |
| Grok | xAI API key | Managed `GROK_HOME`; native managed policy and requirements remain linked locally |
| Hermes | Anthropic, OpenAI API, xAI, OpenRouter API key | Managed `HERMES_HOME`, provider configuration and child environment |
| Pi | Anthropic, OpenAI, xAI, OpenRouter API key | Managed `PI_CODING_AGENT_DIR`, default provider, and child environment |
| OpenCode | Anthropic, OpenAI, xAI, OpenRouter API key | Managed data directory, explicit provider options, a fresh local server |

Users connect a key once, approve another device by comparing its pairing code, and can then
launch with that key independently of the source device. Account selection is synchronized
per agent. Existing native Claude/Codex/Cursor sign-in remains available; sharing an eligible
native key is explicit. Import rejects OAuth grants and never copies entire credential files.
Provider usage meters are retained where already supported. API-key rows do not invent quota
measurements: billing and limits remain with the provider.

This does **not** implement cross-device subscription OAuth refresh brokering. Codex ChatGPT,
Claude subscription, Cursor agent OAuth, Nous, and other rotating grants remain native/local.
Existing remote execution is unchanged; this change adds no automatic source-host routing.
See [the provider investigation](cross-device-auth.md) for contracts and the remaining
refresh-owner work. No live provider credential was used during implementation or tests.

## Relationship to PR #252

Adapted the common vault substrate from [PR #252](https://github.com/zeronsh/zeron/pull/252),
pinned to commit [`899c5b61e48f6ca0bcea72129f0c19720b42ed22`](https://github.com/zeronsh/zeron/tree/899c5b61e48f6ca0bcea72129f0c19720b42ed22):

- `zeron-crypto`: signed records, content encryption, HPKE envelopes, membership policy,
  recovery, keyrings, and cross-language fixtures.
- Engine vault store/client/service, device approval and recovery UI, and the edge VaultRoom.
- Multi-device setup, approval, epoch rotation, revocation, and recovery test scenarios.

Workspace encryption migration, workspace codecs, and Noise transport were not brought in.
This implementation adds a credential content purpose, signed credential sequence numbers,
atomic compare-and-swap storage, encrypted pending-write recovery, profile-bound local-file
AAD, native OS keyring access, and a local-only RPC boundary for secret-bearing operations.
The source PR is still open. Reusing its tests is not an independent cryptographic audit;
the imported protocol and this integration need security review before production rollout.
Its draft formats were not released on main; there is no migration from experimental #252
state files to this implementation's profile-bound format.

## Trust and persistence

The edge stores ciphertext and public trust records, never the account names, keys, active
bindings, or recovery secret in plaintext. Vaults are scoped to `(organization, user)`.
A Zeron bearer alone cannot enroll a device, sign an update, or decrypt accounts. Enrollment
requires an existing approved device to confirm the matching comparison code, or the recovery
kit. Initial setup requires confirming that the kit has been saved.

The local vault file is authenticated and encrypted with a per-profile protection key from
macOS Keychain, Windows Credential Manager, or Linux Secret Service. The explicit key-file
and systemd credential options from #252 remain available for headless deployments. An
unavailable or unreadable key store fails closed; it does not silently choose plaintext or
replace an existing unreadable key. Writes use private files, fsync, and atomic rename. The
profile identity is part of local encryption AAD, including when a configured key file is
shared across profiles.

Each credential revision is encrypted and signed by an approved device. The revision number
is covered by the signature and checked against both the request header and decrypted body.
The worker rechecks membership after asynchronous verification and atomically accepts only
the next revision, or an exact-byte retry of the current revision. Concurrent edits produce
an explicit conflict. Clients persist exact pending ciphertext before sending it, so a crash
or lost response can replay the same write without another revision or nonce reuse. Previously
observed revisions are pinned locally; lower revisions and same-revision equivocation fail.
An authenticated empty document records removals, so stale writes cannot resurrect accounts.

Revocation rotates the vault epoch and blocks new grants to the removed member. It cannot
erase an API key already delivered to a device or terminate an already running provider
session. Users must rotate/revoke keys at the provider to invalidate those copies. As with
the source protocol, local pins detect rollback relative to observed history; they do not
prove that a malicious server has shown a fresh device the globally newest history. The edge
can withhold service. This is not a transparency log or a rollback-proof global authority.

## Runtime isolation

The engine resolves credentials inside its user/organization profile, immediately before a
run, title request, or discovery call. The native installation catalog can be shared by engine
instances without sharing credential context. Secrets are passed in a task-local launch
context and child environment, never in `RunRequest`, durable session commands, or account
metadata. Shared discovery bypasses native caches. Inherited provider keys and common endpoint
overrides are removed; supported adapters select the intended provider explicitly. OpenCode
refuses shared credentials with an externally attached server.

Managed homes contain provider configuration, not copied native authentication stores. They
also give shared accounts separate session/config state: native plugins, preferences, and
resume files are not automatically imported. Native CLI enterprise policy still applies;
Grok's home policy files are preserved locally. Provider-specific enterprise installations,
nonstandard endpoints, and real CLI version compatibility require their own validation.
Running agent children and their tools are trusted to use the key they receive. This does
not protect credentials from a compromised approved device or another process with equivalent
OS privileges.

Secret fields mask rendered text and IME surrounding text, suppress copy and undo history,
clear on submit/close, and zeroize owned buffers on release. Local secret-bearing and vault
management RPCs are rejected by the host relay. Ordinary metadata and existing execution RPCs
retain their existing authorization model. Existing local IPC uses loopback WebSockets and
rejects browser Origin headers; it does not authenticate OS peers. The host and processes
able to connect to that IPC are therefore part of the trust boundary. This change does not
make an untrusted multi-user host safe. Recovery-kit copying is an explicit user action
and clears the clipboard after 60 seconds if it still contains the copied kit.

New shared launches currently require the edge to be reachable, even if an encrypted local
revision exists. They fail closed when enrolled vault state is locked, revoked, stale, or
unverifiable. Losing all approved devices and the recovery kit loses access to the vault;
provider keys can be rotated and reconnected, but the old vault cannot be decrypted by Zeron.

## Verification

`cd edge && npm run test:credentials` bundles the actual edge entry point and starts a local
Miniflare/workerd instance. It invokes the Rust integration suite with synthetic credentials:

1. Setup, comparison-code rejection/approval, shared object keys, revocation, and recovery.
2. Credential import on A, metadata and decryption on B, secret-free wire/disk metadata,
   authenticated deletion, stale-write rejection, concurrent edits, and recovery on C.
3. Crash after server commit but before local acknowledgement, exact ciphertext replay,
   and rejection of an attempt to relabel a signed revision.
4. Eight agent bindings imported on A, enrollment of B, removal of all A-side service
   objects, and real OS child-process checks through B's production credential resolver.
   The test driver checks environment delivery/isolation; it does not emulate or authenticate
   the provider CLIs themselves.

The suite also includes crypto fixtures/tamper tests, vault persistence/lifecycle unit tests,
relay rejection, task-local launch isolation, and secret-field masking/clipboard/IME tests.
Native screenshots are generated by the `agents-fixture` example using synthetic account
metadata in the real application shell; they do not depict real user accounts or live usage.

### Reproducing the screenshot flows

On Linux with Xvfb, openbox, xdotool, and ImageMagick installed, run:

```sh
scripts/capture-agents-flow.sh /tmp/agents-flow
```

The native fixture captures the overview in both themes and 23 flow states: initial setup,
saving/confirming the recovery kit, the new and approving sides of device enrollment,
approved-device management, masked API-key entry, successful connection, switching and
forgetting accounts, Hermes provider choices, a concurrent-update error, removal on both
devices, recovery entry/completion, and a locked credential store. It waits for the page's
RPC actions to settle before each capture and verifies that every flow's action reached the
fixture RPC service. The screenshot service uses only synthetic data, including an unusable
recovery key; it is separate from the real-worker cryptographic E2E suite. After recovery,
the fixture invokes the page's Refresh action before capturing restored accounts.
