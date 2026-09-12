# Live harness authentication validation

Tested 2026-09-12 on Linux x86_64. This is a live provider test report, not a claim that
Zeron's shared OAuth implementation is complete. Product implementation was paused for
this investigation at the user's request.

## Finding

All five requested harnesses completed real model requests in fresh runtime environments
using an existing connection, without another browser login. All five also passed a second
process launch and two simultaneous workers with separate runtime directories. The earlier
characterization of subscription credentials as nonportable was too broad. Initial credential
delivery is technically feasible; durable renewal and provider-specific integration remain work.

| Harness | Installed version | Existing sign-in | Isolated credential delivery | Real prompt | Restart | Two independent workers |
| --- | --- | --- | --- | --- | --- | --- |
| Codex | 0.153.3 | ChatGPT | Auth JSON containing current tokens/account metadata, with the refresh token replaced by an empty string | Pass | Pass | 2/2 pass |
| Claude Code | 2.1.258 | Claude.ai Max | Current OAuth access token in a minimal credential file; separately, access token supplied through `CLAUDE_CODE_OAUTH_TOKEN` without that file | Both pass | Both pass | 2/2 pass |
| Grok | 1.0.4, d846eb93d9 | xAI OIDC | Auth entry in a fresh `GROK_HOME`, with refresh token removed | Pass | Pass | 2/2 pass |
| OpenCode | 1.18.21 | No saved credentials | Existing Codex/ChatGPT access token and account ID mapped to OpenCode's OAuth schema through `OPENCODE_AUTH_CONTENT`; empty refresh token | Pass, `openai/gpt-5.5` | Pass | 2/2 pass |
| Devin | 3000.10.21, 611c1cba | Devin Max | Existing `credentials.toml` in a fresh XDG data directory | Pass | Pass | 2/2 pass |

Every successful request returned `ZERON_AUTH_OK`. Native baseline requests also passed for
Codex, Claude, Grok, and Devin. OpenCode's native baseline had no credentials and did not
complete a model request. No OpenCode login was added to the user's normal installation.

## Isolation and controls

- Used the installed, unmodified binaries and live provider endpoints. The prompt requested
  only the marker above, with no tools or file inspection. Used empty working directories;
  disabled tools/customizations through CLI options where available.
- Bubblewrap hid the original Codex, Claude, Grok, Devin and OpenCode credential directories.
  Each worker received fresh app/XDG directories. Executables were mounted separately so
  hiding credential directories did not hide the executable being tested.
- Kept the original home path unchanged; isolation used mount namespaces and app-specific
  directories. These are separate runtime environments on one host/network, not two physical
  devices, a different IP, or an actual provisioned cloud VM.
- Temporary credentials were private files in `/dev/shm` or child environment variables.
  Removed actual refresh tokens from OAuth worker inputs to avoid rotating the user's grant
  from a copied store. Deleted temporary runtime directories after each test.
- Compared the original four credential files before/after each isolated test. Every check
  reported unchanged bytes. No logout, provider revocation, or forced live grant refresh ran.
- Credential-free controls failed for all five: Codex returned 401, Claude requested login,
  Grok requested sign-in, Devin canceled unavailable login, and OpenCode returned an error.
  This rules out successful test runs silently using the hidden native credential files.

## Codex external-token recovery: live verified

Started the real app-server with no auth file and initialized `experimentalApi`. Supplied
`chatgptAuthTokens` using a deliberately corrupted access-token signature and the correct
account ID. Started a thread and turn. The server issued
`account/chatgptAuthTokens/refresh` with reason `unauthorized` and the expected account ID.
The test host supplied the original valid access token. The turn completed successfully
with `ZERON_AUTH_OK`.

This proves that a Zeron host can replace a rejected token for this installed app-server
without another interactive login or giving the app-server a refresh grant. It does not
test an actual provider refresh-token exchange: the replacement access token was already valid.

## Claude unattended-token option: documented, partly live verified

The installed CLI advertises `claude setup-token`. Anthropic documents it as a browser
authorization flow that emits a one-year subscription OAuth token for unattended CLI use.
The token can make model requests; some account features are excluded. This is a concrete
candidate for browser-only account onboarding in Zeron, without asking users to find API keys.

The environment-variable injection path passed using the user's existing access token.
Generating a new one-year token was not tested because it requires browser approval. A
normal login's current access token does not become long-lived when put in that variable.
The ordinary stored Claude grant also had finite refresh-token expiry metadata; indefinite
unattended access must not be inferred from these successful requests.

[Claude authentication and setup-token documentation](https://code.claude.com/docs/en/authentication#generate-a-long-lived-token).
Native CLI capability and the permitted Zeron credential-storage/delegation arrangement
are separate questions; this technical test does not settle provider-contract requirements.

## Failures that changed the test design

1. **Codex schema:** deleting `refresh_token` entirely made the copied auth JSON unusable,
   leading to unauthenticated requests. Keeping the required field as an empty string
   passed. Also mounted the companion Codex executables after an initial missing-code-mode
   helper error. Managing a harness means preserving its executable bundle and auth schema.
2. **Grok status:** `grok models` printed “You are not authenticated” while a native model
   request succeeded with the existing OIDC credential. This status command is insufficient
   as a readiness check in this installed version.
3. **OpenCode model selection:** the transferred ChatGPT connection received HTTP 400 for
   `gpt-5.4` because that model was unsupported for this account. Selecting `gpt-5.5` passed.
   Provider/model compatibility must be distinguished from an authentication failure.
4. **OpenCode concurrent initialization:** two processes sharing the same brand-new data
   directory raced its SQLite initialization; one failed creating the `workspace` table.
   Both processes passed when each had its own fresh data directory. Share the connection,
   while giving independent workers independent runtime databases.

## What is still unproven

- Fresh provider login initiated and completed through Zeron, including native `setup-token`
  output capture. Existing credentials were used for these probes.
- Live rotating-refresh-token exchange, concurrent refresh, owner crash/recovery, ambiguous
  provider responses, long-duration expiry, and required reauthentication. No refresh owner
  was implemented during this investigation.
- Actual cloud provisioning, different hosts/operating systems/IP addresses, workload-key
  delegation, device revocation, and source-machine outage. The tests establish independence
  from source credential files, not a deployed cloud control plane.
- End-to-end runs through Zeron's production adapters/vault; the probes directly exercised
  native harnesses. Plugin/MCP preservation and managed enterprise policy were not validated.
- Other providers inside OpenCode. Only its ChatGPT path was tested; the result does not
  generalize to every OAuth plugin or subscription.

The required next implementation is credential delivery and lifecycle management for these
verified native paths. The API-key-only PR is still incomplete, but lack of technical
subscription-token transfer is not an established blocker for these five tested cases.

## Evidence and reproduction

Local redacted probe records: `/tmp/zeron-auth-validation-20260912/`. Probe drivers are
`/tmp/zeron-auth-probe.py`, `/tmp/zeron-isolated-auth-probe.py`, and
`/tmp/zeron-codex-external-probe.py`. They contain no embedded credentials. They read existing
native credentials at runtime; do not upload raw credential files or environments.

The isolated driver accepts a harness and mode: `empty`, `transfer`, `access-env`,
`concurrent` (shared fresh runtime), or `worker-a`/`worker-b` (separate fresh runtimes).
For example, `python3 /tmp/zeron-isolated-auth-probe.py claude access-env` tests environment
delivery and restart. Launch separate worker modes concurrently to reproduce independent
worker tests. Inspect the recorded child exit code and expected model response; the outer
driver exits successfully after recording an expected failed control as well.

OpenCode auth schema and plugin behavior were checked against the installed release source:
[auth storage](https://github.com/anomalyco/opencode/blob/v1.18.21/packages/opencode/src/auth/index.ts),
[ChatGPT plugin](https://github.com/anomalyco/opencode/blob/v1.18.21/packages/opencode/src/plugin/openai/codex.ts).
