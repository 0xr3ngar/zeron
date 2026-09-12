# Account continuity for devices and cloud agents

Status: required product behavior and architecture proposal, not implemented or live-validated.
Recorded 2026-09-12 following clarification that cloud agents must authenticate automatically.
This supersedes the API-key-first delivery scope as the definition of feature completion.

## Required experience

Connect a provider once. Every authorized execution device and newly provisioned cloud
worker can use that connection without a new provider sign-in. Replacing workers, restarting
the auth service, and ordinary token expiry must preserve this experience. The original
sign-in device must be allowed to go offline. Provider revocation, mandated reauthentication,
or account-policy changes are exceptional reconnect cases; worker creation is not one.

Always encrypt credentials that Zeron persists and replicate them only in encrypted form.
Local encryption does not depend on enabling account sharing. Migrate legacy native account
snapshots as well as protecting new shared accounts. A provider connection must not be shown
as available everywhere until its destination execution and refresh path have been verified.

## Proposed lifecycle

1. Connect through the provider's supported authorization flow. Record provider identity,
   issuer, account/organization, scopes, auth kind, and grant lineage separately from the
   harness binding. Do not infer token interchangeability from matching account emails.
2. Establish durable ownership of the authorization grant. For refreshable credentials,
   one logical auth service owns refresh across local devices and cloud workers. Avoid
   exporting refresh tokens to every harness process. Moving ownership must stop the old
   owner; copying an unmanaged CLI grant leaves an uncoordinated refresher behind.
3. Authorize devices through existing approval/recovery. For unattended cloud launches,
   establish an explicit, revocable cloud-execution delegation once. A scoped workload
   identity can then request only the connection authorized for its job. Worker creation
   must not enroll the worker as a full member of the user's credential vault.
4. Resolve authentication at launch through a provider adapter. Prefer access tokens with
   limited lifetime, or an authenticated provider gateway where supported. A harness gets
   only the credential it needs, outside durable commands, logs, and ordinary config sync.
   Static API keys cannot be made provider-scoped or short-lived merely by wrapping delivery
   in a short-lived Zeron authorization; document their broader exposure accurately.
5. Refresh through the durable owner and deliver replacements to active workers. Coordinate
   concurrent requests, persist the next grant generation before distributing it, and
   reject stale generations. A provider refresh and our database commit are not an atomic
   transaction: lost responses and crashes after rotation need provider-specific recovery.
   Leases alone cannot guarantee exactly-once refresh or fence an external process.
6. Revoke job/device delegation independently from disconnecting the provider account.
   Stop issuing credentials immediately on revocation; previously issued provider tokens
   remain valid until provider expiry/revocation. Worker teardown removes Zeron-owned caches.

## Cloud trust boundary

An opaque ciphertext relay cannot refresh a credential it cannot use. Independence from
all personal devices requires an authorized, persistent cloud auth endpoint or a supported
provider delegation mechanism. The auth endpoint must survive ephemeral worker replacement.

Explicitly enrolling that endpoint changes who is trusted: encrypted storage and delivery
do not hide credentials from the endpoint while it uses them. Do not claim cloud-operator
inaccessibility without an implemented and verified isolation/attestation design. Keep the
general synchronization service and disposable workers out of the refresh-secret trust
boundary. Per-connection authorization also requires narrower key distribution than the
current single encrypted accounts document provides; full-vault membership is not sufficient.

## Provider integration gates

| Connection | Evidence and remaining work |
| --- | --- |
| Codex ChatGPT | App-server documents experimental external access tokens and refresh callbacks. Implement a managed, compatible runtime and durable grant owner; validate supported grant acquisition and refresh, simultaneous workers, and restart recovery. The callback interface alone does not implement ownership or crash recovery. |
| Claude subscription | Current Anthropic rules restrict third-party collection/storage/brokering of subscription tokens, while allowing native sign-in to hosted unmodified Claude Code. Investigate persistent user-owned native auth environments and seek a supported delegation/provisioning contract where needed. Persistence alone does not prove safe concurrent execution or arbitrary host transfer. This is unresolved, not evidence that users must log in on every new worker. |
| Grok OAuth | The documented external auth helper can obtain tokens noninteractively. Validate the installed version, issuer, refresh ownership, and helper deadlines before enabling it. |
| Nous and OAuth providers behind Hermes, Pi, OpenCode | Implement per-provider grant contracts and harness integration. Local file locks and whole-auth-file copies do not establish distributed ownership. Preserve plugin/configuration discovery separately from authentication. |
| API keys and persistent SDK/CLI credentials | Existing encrypted delivery is groundwork. Validate native credential schemas, real harness launches, destination policy, expiry, and job-scoped cloud authorization. |

Sources checked 2026-09-12: [Codex external-token authentication](https://learn.chatgpt.com/docs/app-server#3c-log-in-with-externally-managed-chatgpt-tokens-chatgptauthtokens),
[Claude credential-use rules](https://code.claude.com/docs/en/legal-and-compliance#authentication-and-credential-use),
[Grok external auth provider](https://docs.x.ai/build/enterprise).
Additional provider source evidence is in [the provider investigation](cross-device-auth.md).

## Required end-to-end evidence

- Connect on A; authorize B; stop A; run on B without interactive provider login.
- Under an existing cloud delegation, provision a clean worker C and authenticate it without
  manual approval or provider sign-in. Destroy C and repeat on D without copying C's disk.
- Expire an access token during a run; recover without prompting. Run concurrent workers
  through rotation and ensure none refresh independently with stale refresh credentials.
- Restart the auth owner before refresh, during the request, and after provider rotation but
  before persistence. Exercise lost responses, unavailable owner, takeover, and stale owners.
- Reject wrong-user, wrong-organization, wrong-job, replayed, expired, and revoked workload
  authorizations. Verify workers cannot retrieve unrelated accounts or refresh secrets.
- Preserve native plugins, project configuration, managed policy, and active-session identity
  through account switches, runtime upgrades, and worker replacement.
- Verify mandatory encryption and legacy migration, with key-store denial failing closed;
  check credentials never enter snapshots, command histories, logs, or screenshots.
- Use actual supported harness binaries and provider-backed sign-in/refresh for each claimed
  connection type. Synthetic tests are required for fault injection but do not establish
  provider compatibility. Record exact versions and distinguish live from simulated evidence.

PR #335 currently has synthetic multi-device API-key delivery coverage. It does not yet pass
the subscription/cloud lifecycle above and must remain described as incomplete groundwork.
