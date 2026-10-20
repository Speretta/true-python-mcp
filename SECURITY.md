# Security policy

## Reporting a vulnerability

Do not open a public issue for security problems. Use GitHub private
vulnerability reporting on this repository:

1. Open the Security tab:
   <https://github.com/Speretta/true-python-mcp/security>
2. Choose "Report a vulnerability".
3. Include the tpmcp version (`tpmcp --version`), your OS, the MCP host you
   use, and a minimal reproduction.

If private reporting is unavailable, email `speretta@protonmail.com`.

We aim to answer within 7 days. Fixes ship in the next release; older
releases get no backports.

## Supported versions

| Version | Supported |
| --- | --- |
| latest release | yes |
| older releases | no; upgrade to the latest release |

## Dependencies

The pre-push git hook (`git config core.hooksPath .githooks`) scans the
dependency graph with [cargo-audit](https://rustsec.org) before every push,
and CI runs the same scan on every push (the `audit` job in ci.yml), so a new
advisory fails both. `cargo outdated` blocks on outdated direct dependencies
in the hook. Reviewed exceptions — advisories that do not apply to tpmcp's
usage, or that are transitive through the pinned ruff train — are documented
in `.cargo/audit.toml`; a new advisory blocks the push until fixed or
reviewed (`git push --no-verify` bypasses deliberately).

## Scope

tpmcp edits files with the full rights of the user who started it. That
access is the product: the MCP host's approval UI is the gate for every
write, and tpmcp never executes the code it edits.

In scope:

- a write that lands despite a stale revision (the revision guard failed)
- byte corruption of a notebook tpmcp edited or converted
- a cross-process lock bypass between two tpmcp processes
- a panic, abort, or stack overflow reachable from file content

Out of scope:

- a model asking for an edit that you approved in your host
- hosts configured to auto-approve writes without asking
