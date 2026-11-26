# Roadmap

This roadmap is a statement of intent, not a contract. Items move when users report real problems and
when contributors have time. Everything here must respect the three oaths in
[CONTRIBUTING.md](../CONTRIBUTING.md): no workflow friction, fully reversible, and graceful degradation.

Want to help? Issues labelled
[`good first issue`](https://github.com/pvbang/leandock/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
and [`help wanted`](https://github.com/pvbang/leandock/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22)
are curated to be self-contained.

## Now — 0.2.x (stability)

- Fix regressions reported against 0.2.0 on real VPS and laptop setups.
- Keep the platform matrix honest: every skipped optimization must explain itself.
- Improve tests around host-file backup and `leandock revert` on all three operating systems.
- Keep release artifacts reproducible and checksum-verified across GitHub, npm, PyPI, and crates.io.

## Next — 0.3 (proposed)

| Item | Why | Status |
|---|---|---|
| `leandock doctor --report` produces a redacted JSON bundle for bug reports | Support quality and faster triage | Proposed |
| `--format json` for `analyze` and `status` | Scripting and dashboards | Done (unreleased) |
| Windows ARM64 release archives and npm package | Growing ARM laptops | Proposed |
| Installer linting (shellcheck, PowerShell analyzer) in CI | Installers are the first thing users run | Proposed |
| More worked examples with measured numbers on 1–2 GB VPS | Users want proof, not claims | Proposed |
| Additional database engines behind the existing tuner interface | Coverage without special cases | Evaluating |

## Later — exploring

- `leandock top`: a TUI for live RAM, zram, KSM, and wake states across projects.
- Multi-host status aggregation for people running several small VPS instances.
- HTTP/2 and WebSocket proxying in `leandock proxy` (requires a larger HTTP stack).
- Team policy packs (org-wide label defaults and guardrails) — only useful once teams ask for it.

## Non-goals

- Replacing a real orchestrator (Kubernetes, Swarm, Nomad) or a DBA's benchmarking.
- Editing users' Compose files or Dockerfiles in place.
- CRIU checkpoint/restore for container sleeping.
- Deleting volumes, bind mounts, or tagged images during cleanup.
