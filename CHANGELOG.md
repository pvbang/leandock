# Changelog

All notable changes to **LeanDock** will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- `leandock analyze --format json` and `leandock status --format json` for machine-readable output (default `text` output is unchanged).

## [0.2.0] - 2026-11-25

### Added
- **Cross-Platform Tiered Capability Profiler (`src/profiler/`)**: Detects Linux (KVM, bare-metal, VMware, Xen, LXC, OpenVZ, WSL2), Windows 10/11, and macOS (Apple Silicon & Intel) across `x86_64` and `aarch64`.
- **Non-Destructive Compose & DB Auto-Tuner (`src/prerun/`)**:
  - Generates isolated `.leandock/compose.override.yml` without modifying original Compose files or mangling `${VAR:-default}` interpolation.
  - Supports `leandock.ignore=true` in both YAML mapping and string-array label formats.
  - Hardware-aware safe tuning for MongoDB, PostgreSQL, Redis/Valkey (persistence-safe by default), MySQL/MariaDB, and Qdrant.
  - Opt-in static frontend offloading (`--extract-static`) and SlimToolkit minification (`--minify`) with startup verification and fallback.
- **Host & Platform Tuners (`src/prerun/host_tuner/`)**:
  - Linux `/etc/docker/daemon.json` deep-merge with transactional backup and reload rollback.
  - Linux daemonless Podman Quadlet exporter (`leandock quadlet`).
  - Windows `%UserProfile%\.wslconfig` analyzer/merger and NTFS I/O warnings.
  - macOS container VM inspector (Docker Desktop, OrbStack, Colima, Lima).
- **Post-Run Optimizer (`src/postrun/`)**:
  - Real Linux `zram` (`zstd`) and `KSM` (kernel >= 6.1 + systemd >= 254 + `MemoryKSM=yes` drop-ins).
  - Post-deploy Health Verification Guard with 30-line log dump and automatic raw-Compose rollback.
  - Safe post-deploy dangling image and build cache cleaner.
- **Cross-Platform Wake-on-Request Reverse Proxy (`src/proxy/`)**:
  - Direct HTTP-over-Unix-Socket engine client on Unix and async CLI adapter on Windows.
  - Per-service singleflight coordination (`tokio::sync::Notify`) preventing container start stampedes.
  - Built-in gzip + SPA static file server and graceful `SIGINT`/`SIGTERM` shutdown.
- **CLI & Reporting (`src/cli/`, `src/report/`)**: `analyze`, `up`, `down`, `proxy`, `quadlet`, `status`, `bench` (Table / Markdown / JSON), `revert`, and `doctor`.
