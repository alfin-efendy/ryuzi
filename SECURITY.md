# Security Policy

## Supported versions

harness-router is pre-1.0 and ships from a single line of development. Security
fixes are applied to the latest released version on npm (`hrctl`) and the latest
GitHub Release. Please upgrade to the latest version before reporting.

## Reporting a vulnerability

**Do not open a public issue for security problems.**

Report privately via GitHub's "Report a vulnerability" form
(Security → Advisories → Report a vulnerability) on
https://github.com/alfin-efendy/harness-router/security/advisories/new

If you cannot use that form, contact the maintainer directly and we will open a
private advisory on your behalf.

Please include: affected version (`hr --version`), platform, a description of the
issue, and reproduction steps or a proof of concept if you have one.

## What to expect

- Acknowledgement of your report within 7 days.
- An initial assessment and severity within 14 days.
- Coordinated disclosure: we will agree on a timeline with you and credit you in
  the advisory unless you prefer to remain anonymous.

## Scope

harness-router runs an agent that executes code inside git repositories and can
be driven from chat clients. Reports we are especially interested in:

- Compromise of the release/distribution path (npm, Docker/GHCR, installer, CI).
- Leakage of locally stored secrets (e.g. the Discord bot token) via logs,
  telemetry, published artifacts, or file permissions.
- Privilege/authorization flaws in the gateway access controls (admin/approver
  roles, permission modes).

Out of scope: attacks that require an untrusted user to already have authorized
control of an agent session (the documented trust model is single-user or
small-trusted-team).
