# Managed harness runtimes

Zeron should manage the runtimes it launches by default. An explicit existing-installation
option preserves users' custom installations. Runtime selection is device-local; account
selection and encrypted credentials belong to the Zeron profile.

Settings separates Accounts (identity, usage, encrypted device access) from Harnesses.
Each harness expands into device rows with installation status, version, enablement, and
installation controls. There is no device switcher on either page. An offline device is
shown explicitly; its absence is not treated as an uninstall.

## Implemented guarantees

- Installations run on the selected execution device and continue after the page closes.
- Managed npm packages use bundled dependency locks and `npm ci`, including platform
  packages and integrity hashes. Dependency-lock hashes separate installation directories.
- Devin bundles use pinned, platform-specific SHA-256 checksums. Downloads are bounded;
  extraction rejects escaping paths, links, special files, and excessive expansion.
- Version checks finish before atomically committing the selected executable. Failed
  installations preserve the selection. Cross-process locks prevent competing changes.
- A previous selection is retained for rollback. Re-selecting the same runtime preserves
  that rollback target. Existing processes retain their executables; new sessions obtain
  fresh drivers. Corrupt explicit selections do not fall back to another PATH binary.
- Node launchers and Cursor SDK shims have immutable, content-addressed paths. Pi uses
  pinned `pi-acp` and passes the exact selected executable through `PI_ACP_PI_COMMAND`.
- Installations do not rewrite native credential homes or change the user's global PATH.
  Shared-account homes link device-local plugin/skill directories on Unix; these files
  are not uploaded as account data. This is not cross-device plugin replication.
- Legacy Zeron account snapshots migrate to authenticated encryption on read, using
  the OS protection provider and bindings for the profile, harness, and slot. Invalid
  snapshots remain untouched. There is no plaintext fallback or plaintext backup.
- Encryption setup starts automatically for a new account vault. Recovery-key confirmation
  remains explicit. An existing vault still requires approval or recovery on a new device.

## Live validation, 2026-09-12

The production installer was exercised under an isolated runtime-selection directory on
Linux x86-64. These are real installations and protocol probes, not UI fixtures.

| Harness | Pinned runtime | Installation/version check | Protocol check |
| --- | --- | --- | --- |
| Codex | 0.153.3 | Passed | Native app-server initialize |
| Claude Code | 2.1.258 | Passed | Native stream-JSON initialize |
| Grok | 1.0.4 | Passed | Native ACP initialize |
| Devin | 3000.10.21 | Passed | Native ACP initialize |
| OpenCode | 1.18.21 | Passed | Native ACP initialize |
| Pi | 0.85.1, pi-acp 0.0.33 | Passed | ACP initialize with the selected Pi command |
| Cursor | SDK 1.0.28 | Passed | Complete live model catalog |
| Hermes | Existing installation | No managed installer yet | Not part of this installer run |

Cursor's catalog exposed a truncated-stdout bug: the shim exited before a JSON response
larger than the pipe buffer finished writing. The models branch now awaits the write;
a regression test emits 3,000 synthetic models through a real Node subprocess.

Authenticated model prompts also passed through the managed Codex, Claude, Grok, Devin,
and OpenCode installations in fresh credential-only homes and restarted processes. Each
returned `ZERON_AUTH_OK`. Original credential stores were hidden with bubblewrap; rotating
refresh grants were excluded from worker copies, source credential hashes were unchanged,
and temporary credentials were removed. These checks still do not exercise renewal.

Separate process checks covered selection persistence, a failed executable check leaving
selection bytes unchanged, repeated selection preserving rollback, rollback, and rejection
of a concurrent installation. Real provider authentication probes are documented separately
in [harness-auth-live-validation.md](harness-auth-live-validation.md).

The native UI screenshots use synthetic devices and accounts. They cover both themes,
installation controls/progress/errors, offline devices, account usage, recovery-key setup,
pairing, shared API-key enrollment, switching/removal, revocation, and recovery. They are
review artifacts, not evidence that subscription renewal or real cloud workers are complete.

## Remaining before the login-once goal is complete

Shared API-key credentials already work through the encrypted vault, including an approved
second device launching after the source device disconnects. The ordinary subscription
login flows still save native account slots. Mandatory local encryption does not make
those subscription accounts shared.

Remaining work includes native browser onboarding into shared account records, provider-
specific credential renewal with a single refresh owner and crash recovery, access-token
delivery to running harnesses, shared subscription usage, and production E2E coverage for
expiry/concurrency/source-device loss. The existing Codex external-token callback probe and
Claude `setup-token` capability inform that implementation; neither substitutes for it.

Hermes still needs a pinned Python/Node installation plan and validation. npm-based
installations currently require npm/Node to be available; Zeron retains a private Node
executable for the installed runtime but does not yet bootstrap Node on a bare device. Cursor uses its
managed SDK rather than accepting an arbitrary Cursor CLI. JavaScript launcher installation
on Windows is not implemented; platform support must be validated before broader rollout.
The vault substrate and credential lifecycle also need an independent security review.

The PR remains a draft until the authentication acceptance criteria are met.
