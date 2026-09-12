# Cross-device provider access

Investigated 2026-09-12 against `origin/main` at `7655c662` (v0.2.61).
Status: proposal; no authentication behavior has been changed or live credentials transferred.

Users should see their connected accounts on every Zeron device. Whether a device can
execute locally with that account must be explicit. Remote control is already supported;
independent local execution requires additional credential infrastructure and provider-specific support.

Coverage: all eight production `HarnessId` variants — Claude Code, Codex, Cursor, Devin,
Grok, Hermes, Pi and OpenCode. `Mock` is test-only. The expanded investigation below also
checks ACP authentication and the upstream-provider distinction for multi-provider harnesses.

Recommended direction: a private account catalog, existing remote execution for all harnesses,
an encrypted vault for supported API keys/persistent CLI tokens, and separately validated
Codex and Grok token brokers. Devin and Cursor are stronger initial candidates than the
first pass established: their official documentation supports portable credential workflows.
Do not implement universal OAuth-file replication.

## What exists

| Area | Evidence | Consequence |
| --- | --- | --- |
| Provider accounts | [`agent_accounts.rs`](../../crates/engine/src/agent_accounts.rs), `AgentAccounts`, `Slot`, `list`, `activate` | Claude Code, Codex and Cursor accounts are JSON slots under the common data directory, outside profile-scoped sync. Activation overwrites the device's live CLI credentials. |
| Remote account management | [`rpc.rs`](../../crates/engine/src/rpc.rs), `is_device_method`; [`accounts.rs`](../../crates/ui/src/settings/accounts.rs), `target_device` | List, activate and login RPCs already target another device. The UI displays one device at a time. |
| Execution | [`RunRequest`](../../crates/proto/src/agent.rs), native [harnesses](../../crates/harness/src/lib.rs) | Runs have a harness/model/cwd but no account reference. Children use the host's authentication environment/store. Copying account metadata cannot authenticate a run. |
| OpenCode | [`opencode/mod.rs`](../../crates/harness/src/opencode/mod.rs), `server`, `discover`, `Server::spawn` | Already runs its own loopback HTTP/SSE server and discovers connected providers, but is absent from account listing, activation and login. |
| Zeron identity | [`auth.ts`](../../edge/src/auth.ts), [`device-room.ts`](../../edge/src/device-room.ts), [`ARCHITECTURE.md`](../../ARCHITECTURE.md) | WorkOS authenticates Zeron peers. Device rooms check the same user; private registry rooms use organization plus user. This does not sign a user into model providers. |
| Secret transport | [`device_room.rs`](../../crates/rpc/src/device_room.rs), `encode_device_frame` | Frames contain JSON headers and raw payloads. The authenticated relay does not provide application-level end-to-end encryption for credentials. |

Two existing details affect the design:

- Codex detection/activation only reads/writes `auth.json`. `start_codex_login` starts
  `codex login` in a temporary `CODEX_HOME`, then waits for its browser callback. Opening
  that URL on device B cannot reach device A's localhost callback without forwarding.
  This is a code-path finding, not a reproduced live login failure.
- Claude usage probing deliberately avoids refreshing the active account because the CLI
  may hold its refresh token. The inactive-slot guard is an in-process mutex; it cannot
  coordinate another engine, another computer, or an independently launched CLI.

## Provider feasibility

| Provider/auth type | Supported direction | Limit or work needed |
| --- | --- | --- |
| Codex ChatGPT | Remote execution now; investigate externally managed access tokens for local execution on peers | App-server has an experimental integration surface. Requires one refresh owner and version/capability checks. |
| Codex API key | Encrypted key distribution to authorized execution devices | Add credential resolution at child startup; respect managed authentication restrictions. |
| Claude subscription | User signs into the unmodified Claude Code binary on the execution host; other devices control that host | Do not extend Zeron's current custom OAuth/token-slot implementation into subscription-token syncing. |
| Claude API key / supported cloud provider | Customer-managed API keys can use the vault; cloud identity needs its own adapter | Cloud credentials and local credential chains are not interchangeable with subscription login. |
| OpenCode | Integrate connected accounts per upstream provider, beginning with API keys | OpenCode is a harness over many providers, not a single transferable identity. OAuth/plugin support needs separate validation. |
| Cursor | Distribute the SDK's supported user API key and inject it per process | Native browser login mints a key; preserve expiry/backend metadata. Do not import Cursor CLI session tokens. |
| Devin | Encrypted distribution of its persistent CLI credential | Official CLI docs explicitly allow reuse between the user's own machines; preserve enterprise identity and policy. |
| Grok | API-key distribution; external auth-provider command for a broker | Supports device-code sign-in and token-helper refresh; verify behavior against the installed binary and enterprise policy. |
| Hermes | API-key adapters by provider; native Nous device-code login; host execution for OAuth initially | Local shared Nous store and credential-pool locks do not coordinate different devices. |
| Pi | API-key adapters by provider; native provider login for OAuth initially | Relocatable agent directory and credential storage interfaces exist; Zeron uses a pinned ACP adapter whose compatibility must be checked. |

Official OpenAI documentation explicitly describes copying a Codex auth cache to a headless
machine, and supports file, keyring, auto and ephemeral storage. That establishes a supported
bootstrap technique; it does not establish safe concurrent replication of rotating credentials.
Device-code login is available for headless sign-in, subject to account/workspace settings.
[OpenAI authentication documentation](https://learn.chatgpt.com/docs/auth).

Codex app-server documents `chatgptAuthTokens`: a host supplies an access token and account
ID, then responds to `account/chatgptAuthTokens/refresh`. It requires `experimentalApi` and
host ownership of the auth lifecycle. The refresh request has an approximately ten-second
timeout. App-server also documents managed device-code login. This is the strongest
integration candidate for a broker, but does not itself provide a cross-device broker.
[OpenAI App Server documentation](https://learn.chatgpt.com/docs/app-server).

Anthropic's current documentation prohibits third-party apps from collecting, storing or
intermediating Claude.ai credentials/session tokens and from offering their own Claude.ai
login. It separately allows users to sign into the unmodified Claude Code binary on a hosted
platform and permits customer-managed API-key provisioning. This directly affects our
existing custom PKCE flow and subscription credential snapshots: revisit them as part of
the auth redesign. Merely encrypting copied tokens does not address this restriction.
[Anthropic authentication and credential-use rules](https://code.claude.com/docs/en/legal-and-compliance).

OpenCode documents credentials in `~/.local/share/opencode/auth.json`; provider configuration
is separate, and some providers use environment/cloud credentials instead. Custom-provider
configuration may therefore need to accompany a key, with secrets kept out of ordinary
config sync. [OpenCode providers](https://opencode.ai/docs/providers).
Its server exposes provider authentication methods, OAuth authorize/callback routes and
credential-setting APIs. Use that interface where the installed version supports it.
[OpenCode server API](https://opencode.ai/docs/server/).

## Expanded findings for every remaining harness

### Hermes

Hermes supports both direct provider keys and provider OAuth. Its documented storage splits
`config.yaml`, `.env` secrets and `auth.json` provider state. Config contains provider/model
and tool-backend choices, so an inference credential alone is not a complete usable setup.
Only enroll selected credentials; a whole-home copy would include unrelated tools, bot
tokens, history and agent state. [Hermes configuration](https://hermes-agent.nousresearch.com/docs/user-guide/configuration).

Nous Portal is a particularly relevant option: its login covers inference and supported
tool-gateway services. Hermes stores refresh credentials and automatically obtains short-lived
tokens, including background keepalive for long-running processes. A Zeron adapter should
represent this as a Nous connection used by Hermes, with its scopes and routing configuration,
rather than manufacture a generic Hermes account.
[Nous Portal integration](https://hermes-agent.nousresearch.com/docs/integrations/nous-portal).

Upstream source inspected at `1c671beab29164d8931c5d01c5739502267089d8` adds two concrete constraints:

- Nous credentials also have a shared store at `<Hermes root>/shared/nous_auth.json`,
  overridable through `HERMES_SHARED_AUTH_DIR`. Login and refresh update it; named profiles
  reuse it under a local cross-profile file lock. Nous has a device-code flow with a
  verification URL callback. This is useful for sign-in initiated on a remote host, but
  local filesystem coordination does not establish a distributed refresh owner.
  [Nous auth implementation](https://github.com/NousResearch/hermes-agent/blob/1c671beab29164d8931c5d01c5739502267089d8/hermes_cli/auth_nous.py).
- Hermes explicitly strips cloned single-use OAuth grants for Anthropic, Codex and xAI from
  copied profiles, then borrows the root grant. Static API-key entries remain copyable.
  It also has repair logic for previously forked grants. This is direct implementation
  evidence against syncing whole Hermes auth files to independent machines.
  [OAuth grant ownership](https://github.com/NousResearch/hermes-agent/blob/1c671beab29164d8931c5d01c5739502267089d8/hermes_cli/auth_oauth_grants.py).

Hermes' ACP server advertises terminal setup (`--setup`) and a configured runtime provider.
Its `authenticate` implementation checks that a provider already resolves; it does not
implement a general remote credential-import or token-broker API.
[ACP auth descriptors](https://github.com/NousResearch/hermes-agent/blob/1c671beab29164d8931c5d01c5739502267089d8/acp_adapter/auth.py),
[ACP server](https://github.com/NousResearch/hermes-agent/blob/1c671beab29164d8931c5d01c5739502267089d8/acp_adapter/server.py).

**Proposed Hermes path:** provider inventory and native terminal/device-code login first;
encrypted API-key grants plus sanitized provider configuration for independent execution.
Keep Nous OAuth on a host initially. Investigate a Nous-specific broker or provider-supported
independent grants separately; do not infer permission or concurrency guarantees from local
shared-store code. Use `hermes auth` for current credential-pool operations; `hermes login`
is deprecated/removed in current documentation.
[Hermes CLI reference](https://hermes-agent.nousresearch.com/docs/reference/cli-commands).

### Pi

Pi stores provider credentials in `~/.pi/agent/auth.json`, supports API-key environment
variables and subscription login through `/login`, and refreshes OAuth credentials.
OpenRouter login is a useful exception: browser authorization mints an API key rather than
an expiring OAuth credential. Treat it as an API-key grant. Provider configuration and model
catalog state are separate. [Pi provider documentation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/providers.md).

Source inspected at `71dca871bc80b6bc97be37f0ca3189399d651fff` exposes a `CredentialStore`,
file-backed and in-memory storage, and per-file locking. These are possible integration
seams for a future native Pi driver; the file locks only coordinate users of the same store.
`PI_CODING_AGENT_DIR` relocates the agent directory, including `auth.json` and `models.json`.
[Credential storage](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/coding-agent/src/core/auth-storage.ts),
[path configuration](https://github.com/earendil-works/pi/blob/71dca871bc80b6bc97be37f0ca3189399d651fff/packages/coding-agent/src/config.ts).

**Proposed Pi path:** inject API keys into an account-scoped launch context or managed
per-provider auth entries, and preserve model configuration. Zeron currently pins
`pi-acp@0.0.33`; the current upstream storage API is not evidence that this adapter exposes
it. Validate environment/store isolation and login against the adapter before promising
in-memory broker support. Subscription grants retain the upstream provider's refresh and
usage constraints.

Hermes/Pi documentation describes some Anthropic subscription integrations, whereas the
Anthropic credential-use rules cited above restrict third-party collection/intermediation.
Those harness claims do not establish permission for Zeron to collect or sync Claude
subscription tokens. Keep this an explicit unresolved provider-contract issue.

### Devin

Devin's official CLI documentation expressly permits copying `credentials.toml` between
the user's own machines. The stored API token is persistent and does not expire by default.
On macOS/Linux the path is `$XDG_DATA_HOME/devin/credentials.toml`, falling back to
`~/.local/share/devin/credentials.toml`; Windows uses `%APPDATA%\devin\credentials.toml`.
The enterprise auth flow also enforces CLI access permissions and can be restricted to an
enterprise host/account by system policy.
[Devin CLI authentication](https://docs.devin.ai/cli/enterprise/devin-auth).

The ACP server accepts `WINDSURF_API_KEY` before saved CLI credentials and can accept auth
through `authenticate`. Native login has a manual-token option for remote environments.
The exact runtime auth payload must be checked against the installed CLI; generic ACP
does not define a universal token field. Do not substitute a Devin Cloud API credential
merely because it authenticates the separate cloud API.
[Devin commands and ACP](https://docs.devin.ai/cli/reference/commands).

**Proposed Devin path:** include the documented CLI credential in the first vault release,
with backend/enterprise identity and supported schema. Prefer runtime authentication where
verified; otherwise materialize a protected, managed credential store. Check the same identity
on destination startup and report policy mismatch. Source-device availability is unnecessary
after provisioning a valid persistent token; revocation still needs provider-side action.

### Grok

Grok supports browser OIDC, `grok login --device-auth`, `XAI_API_KEY`, and an
`auth_provider_command` executable. The helper may return an access token with expiry and
optionally a refresh token. Background refresh sets `GROK_AUTH_EXPIRED=1` and requires a
fast noninteractive response. Credentials have model-specific precedence; managed policy
can disable API-key auth or require a particular team.
[Grok enterprise authentication](https://docs.x.ai/build/enterprise).

**Proposed Grok path:** API-key grants first, plus a broker spike using a Zeron-owned helper
that returns access tokens only. Keeping refresh credentials at one owner avoids handing
refresh ownership to every Grok process. Validate expiry/reinvocation and helper deadlines;
the existence of a helper is not proof that any externally minted token is accepted by
the selected backend. The helper must authenticate the local caller and selected Zeron
account, and must not prompt during background refresh.

Our ACP spec pins an install fallback to Grok `1.0.4` and disables shared-leader mode.
Check the installed binary's helper support and isolated config behavior; do not assume
current online docs describe every deployed version. Grok OAuth cache format/OS-keychain
portability was not established here, and the helper design avoids depending on it.

### Cursor

The SDK officially supports user/service-account API keys for local and cloud runs.
`Cursor.auth.login()` returns a minted user key, with a default 90-day lifetime, and supports
custom or in-memory storage. The default is `~/.cursor/sdk/auth.json`; resolution is explicit
`apiKey`, then `CURSOR_API_KEY`, then the saved SDK login. This confirms an API-key injection
path for the native SDK harness without copying Cursor app/CLI session tokens.
[Cursor TypeScript SDK authentication](https://cursor.com/docs/sdk/typescript#cursorauth).

**Proposed Cursor path:** enroll the SDK key with expiry and backend identity, inject it
explicitly for runs and discovery, and surface reconnect when it expires. Check this against
our `@cursor/sdk@1.0.28` pin. A long-lived key has a simpler cross-device lifecycle than an
OAuth refresh grant, but should not be presented as permanently signed in.

## Common ACP gap and upstream-account mapping

[`AcpHarness`](../../crates/harness/src/acp/mod.rs) starts subprocesses with inherited
environment and proceeds from `initialize` to session operations. It does not consume
`authMethods` or negotiate `authenticate`. This affects Devin, Grok, Hermes and Pi.
The protocol supports agent-managed authentication and separately launched terminal login;
terminal flows require capability advertisement and reinitialization afterward. Logout is
capability-gated. Implement these flows on the selected execution host, including remote
terminal rendering, cancellation and redaction, rather than sending login input as a chat
prompt. [ACP authentication](https://agentclientprotocol.com/protocol/v1/authentication).

Separate three entities in the design: **provider identity**, **credential grant**, and
**harness binding**. Two grants can belong to the same account without sharing refresh-token
lineage. One compatible API-key grant can be bound to several harnesses without duplicating
its lifecycle. Provider IDs require a mapping: for example Pi calls direct OpenAI `openai`,
while Hermes calls it `openai-api`; OAuth Codex is a distinct auth mode. Do not merge grants
just by email or copy an OAuth token between harnesses because the provider names match.
Bind grants to issuer/backend, organization and scope as well as identity.

This also covers the long tail behind Hermes, Pi and OpenCode: OpenRouter, direct OpenAI,
Anthropic, xAI and other API keys use key adapters; Copilot and other OAuth providers need
their own supported grant/refresh contracts; cloud IAM/workload identity needs destination
authorization; local unauthenticated models need endpoint reachability. Those upstreams
are not additional Zeron `HarnessId` variants, and were not individually validated for
subscription-token portability in this investigation.

## Product behavior

| User action | Result | Dependency |
| --- | --- | --- |
| Open Zeron on another device | See the same account catalog and devices able to use each account | Zeron sign-in and metadata sync |
| Use an account connected on a desktop from a phone | Run on the desktop's selected space and stream/control the session | Desktop must be online; files and tools execute there |
| Run on a second laptop using a shared API key | Resolve its granted key locally and launch the harness there | Trusted-device enrollment and vault access |
| Run Codex locally using a brokered ChatGPT connection | Receive an access token; ask the refresh owner for replacements | Broker available when fresh credentials are required |
| Run while the original login device is offline | Works for already provisioned valid API keys; brokered OAuth depends on where the broker runs | A cloud vault storing ciphertext cannot itself refresh encrypted OAuth credentials |

Show availability separately from identity: connected locally, available on a named device,
ready for local use, host offline, or needs reconnect. Do not silently move execution to a
different device: a space includes its host and checkout. A second-device login ceremony
completed in the first device's browser is useful, but is still another provider authorization.

## Proposed architecture

1. **Private account catalog.** Add a metadata-only account entity with an opaque account ID,
   harness, upstream provider ID, auth kind, display identity, credential generation and
   supported sharing mode. Store per-device availability separately. Initially scope sharing
   to the active synced user and organization; cross-organization sharing would be an explicit
   later policy. Existing local slots remain local until enrolled, since the common slot
   directory spans profiles and must not be auto-published under whichever user signs in next.

2. **Separate credential vault.** Store only supported, normalized secret fields in encrypted
   envelopes, outside registry/chat logs and attachments. Bind ciphertext to owner, account,
   provider and generation using authenticated encryption. Enroll device keys through an
   existing trusted device (or a recovery key), with private keys in OS storage. WorkOS login
   authorizes retrieval but is not a decryption key. The first device creates the vault;
   a new browser/phone can remain a remote controller without receiving provider secrets.
   Existing-device approval/recovery is necessary if Zeron's server must not be able to
   substitute a recipient key and read the vault. A server-managed KMS alternative makes
   enrollment simpler but gives the backend access to plaintext; that is a different trust model.

3. **Credential resolver on the execution host.** Carry only an account reference in session
   configuration and run commands. Resolve secrets after host authorization, outside durable
   commands and logs, and pass them through a separate runtime-only harness context. Give
   model discovery and title generation the same context, and key caches by account/generation.
   Prefer per-process authentication or managed isolated stores over changing the user's
   global CLI login. Validate precedence against inherited environment keys and preserve
   relevant settings, plugins and resume paths when isolating CLI homes.

4. **Provider adapters.** Define detect/list, native login, validate, resolve-for-run and
   disconnect capabilities. API-key adapters can grant independent local execution. OpenCode
   needs an upstream provider ID in addition to `HarnessId::Opencode`, per-entry merge rather
   than whole-file replacement, and discovery invalidation after credentials change. Never
   export entire Claude config/credential blobs containing MCP or plugin secrets.

5. **Codex refresh owner.** Prototype one dedicated managed auth store whose refresh lifecycle
   is exclusively owned by the broker. All managed execution children, including local ones,
   receive access tokens through app-server's external-token mode; they never receive the
   refresh token. Handle refresh requests with bounded latency, account-ID validation and
   single-flight refresh. Do not seed this by leaving a copied refresh token active in an
   unmanaged CLI: that bypasses broker coordination. Verify the supported token-acquisition
   and refresh mechanism before selecting the implementation.

6. **Lifecycle.** Separate disconnect-this-device, remove-shared-account, and provider revoke.
   Persist versioned deletion tombstones so offline peers cannot resurrect removed entries.
   Stop new token grants to removed devices and clear owned local caches on logout. Revoking
   a Zeron grant cannot claw back a copied API key or a still-valid access token; provider
   rotation/revocation is required for that. Crash recovery must never replay a possibly
   consumed refresh token blindly. A distributed lease alone cannot fence an external CLI
   or undo a provider refresh after an ambiguous network failure.

The first broker can run on an always-on user device over the existing relay with encrypted
token delivery. For independence from every user device, a backend broker must be able to
decrypt/use the refresh credential, or the provider must support independently authorized
device grants. An encrypted cloud backup alone cannot deliver that availability guarantee.

## Suggested delivery order

1. Add an account/availability catalog for all eight harnesses and a unified accounts view
   using existing remote routing. Add provider inventory for Hermes, Pi and OpenCode.
2. Implement capability-driven ACP sign-in and remote native-login UI. Fix remote Codex/Grok
   login with device-code modes, verification URL/user code, expiry, cancellation and
   capability fallback; account discovery must respect the configured store.
   Move Claude subscription sign-in through the native binary instead of extending custom OAuth.
3. Implement trusted-device vault enrollment and runtime credential resolution. Start with
   Cursor SDK keys, documented Devin CLI credentials, and direct API keys for Hermes, Pi,
   OpenCode, Grok, Codex and Claude. Use explicit account selection and grant/harness bindings.
4. Run Codex external-token and Grok auth-helper broker spikes against supported versions.
   Prove refresh ownership and recovery before enabling cross-device OAuth execution.
   Investigate Nous separately; add other OAuth providers only when their contracts support it.

## Validation needed before implementation is considered complete

- Two engines: connect on A, see identity on B, run on A from B; disconnect A and show offline.
- Remote Codex device-code flow completed in B's browser, including cancellation and expiry.
- API-key enrollment on B followed by local execution with A offline; incorrect-device,
  incorrect-user and incorrect-organization envelope access rejected.
- Codex concurrent refresh requests, expired tokens, broker restart, ambiguous refresh failure,
  owner outage and unmanaged-CLI coexistence. Demonstrate a single effective refresh owner.
- OpenCode multiple provider entries and custom endpoints; preserve unrelated credentials.
- Hermes named profiles, shared Nous store and credential pools; no cloned OAuth grants or
  unrelated bot/tool secrets; provider/config precedence after launch-context injection.
- Pi pinned-adapter compatibility, relocated agent directory, key precedence, and unchanged
  sibling provider entries. Verify local credential locks are not mistaken for distributed locks.
- Devin destination enterprise policy and credential identity; Cursor key expiry and SDK
  precedence; Grok helper deadlines, background refresh, account/team restrictions and no
  refresh-token delivery to execution children.
- ACP advertised agent/terminal login methods, cancellation, reconnect, auth-required recovery,
  and supported logout on a remote host without credentials entering the chat transcript.
- Account switches while sessions run, model discovery/title isolation, managed auth policy,
  keyring-only Codex login, and macOS credential-store denial.
- Logout/device removal and offline tombstone replay; secrets absent from registry snapshots,
  durable commands, diagnostics and logs.

Existing seams: [`device_routing.rs`](../../crates/engine/tests/device_routing.rs),
[`m5c_accounts_uploads_titles.rs`](../../crates/engine/tests/m5c_accounts_uploads_titles.rs),
and the [harness tests](../../crates/harness/tests). This investigation used source inspection
and current official documentation. No live provider login or refresh experiment was
performed. The subsequent implementation and its automated multi-device validation are
tracked in [shared-credentials-implementation.md](shared-credentials-implementation.md).
