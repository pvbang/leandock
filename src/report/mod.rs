use std::process::Command;

use comfy_table::{presets::UTF8_FULL, Cell, Color, Table};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};

use crate::postrun::KernelMemoryStats;
use crate::prerun::compose_parser::{DatabaseKind, ServiceRole};
use crate::prerun::db_tuner::{OptimizationPlan, ServiceTuningPlan};
use crate::prerun::host_tuner::HostTuningReport;
use crate::profiler::{CapabilitySupport, HostOs, HostProfile, VirtualizationKind};

/// Live per-container telemetry row displayed by `leandock status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveContainerStat {
    pub name: String,
    pub state: String,
    pub cpu_percent: String,
    pub mem_usage: String,
    pub idle_mode: String,
}

/// Reproducible resource benchmark snapshot captured by `leandock bench`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkSnapshot {
    pub label: String,
    pub ram_used_mb: u64,
    pub swap_zram_used_mb: u64,
    pub disk_used_gb: f64,
    pub process_count: usize,
    pub startup_ms: u64,
}

/// Comparison report for `leandock bench`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkComparison {
    pub before: BenchmarkSnapshot,
    pub after: BenchmarkSnapshot,
    pub ram_delta_mb: i64,
    pub ram_delta_pct: f64,
    pub disk_delta_gb: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Markdown,
    Json,
}

/// One row of the platform capability tier matrix shown by `leandock analyze`.
#[derive(Debug, Clone, Serialize)]
pub struct TierReport {
    pub tier: u8,
    pub capability: String,
    pub status: CapabilitySupport,
    pub details: String,
}

/// Host summary included in `leandock analyze --format json`.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzeHostSummary<'a> {
    pub os: HostOs,
    pub distro: &'a str,
    pub cpu_arch: &'a str,
    pub logical_cores: usize,
    pub total_ram_mb: u64,
    pub free_disk_gb: f64,
    pub virtualization: VirtualizationKind,
    pub compose_driver: String,
    pub is_root: bool,
}

/// RAM budget summary included in `leandock analyze --format json`.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzeBudget {
    pub effective_ram_mb: u64,
    pub os_reserve_mb: u64,
    pub allocatable_ram_mb: u64,
    pub total_budgeted_mb: u64,
    pub estimated_default_ram_mb: u64,
    pub estimated_savings_mb: u64,
}

/// One service row in `leandock analyze --format json`.
///
/// A dedicated projection of `ServiceTuningPlan`, so the published JSON schema does not change when the
/// internal tuning model does. Command lines and environment values are left out because they can carry
/// credentials (`--requirepass ...`, `POSTGRES_PASSWORD=...`); only the injected environment *names* are listed.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceJson {
    pub name: String,
    pub role: ServiceRole,
    pub db_kind: Option<DatabaseKind>,
    pub ignored: bool,
    pub mem_limit_mb: Option<u64>,
    pub mem_reservation_mb: Option<u64>,
    pub inject_log_rotation: bool,
    pub tunes_command: bool,
    pub extra_env_keys: Vec<String>,
    pub offloaded_static: bool,
    pub minified_image: Option<String>,
    pub notes: Vec<String>,
}

impl From<&ServiceTuningPlan> for ServiceJson {
    fn from(svc: &ServiceTuningPlan) -> Self {
        Self {
            name: svc.name.clone(),
            role: svc.role,
            db_kind: svc.db_kind,
            ignored: svc.ignored,
            mem_limit_mb: svc.mem_limit_mb,
            mem_reservation_mb: svc.mem_reservation_mb,
            inject_log_rotation: svc.inject_log_rotation,
            tunes_command: svc.tuned_command.is_some(),
            extra_env_keys: svc.extra_env.keys().cloned().collect(),
            offloaded_static: svc.offloaded_static,
            minified_image: svc.minified_image.clone(),
            notes: svc.notes.clone(),
        }
    }
}

/// Machine-readable `leandock analyze --format json` report.
#[derive(Debug, Clone, Serialize)]
pub struct AnalyzeJsonReport<'a> {
    pub host: AnalyzeHostSummary<'a>,
    /// Highest tier N such that tiers 1..=N are all active (not auto-skipped).
    pub detected_tier: u8,
    pub tiers: Vec<TierReport>,
    pub skipped_tiers: Vec<TierReport>,
    pub budget: AnalyzeBudget,
    pub services: Vec<ServiceJson>,
    pub host_preview: Option<&'a HostTuningReport>,
    pub warnings: &'a [String],
    /// Data that could not be collected (for example a failed host-tuning preview), so callers can
    /// tell a missing section from an empty one.
    pub errors: Vec<String>,
}

/// One container row in `leandock status --format json`, with numeric CPU and memory values next to
/// the runtime's formatted text. A number is `null` when the runtime's text could not be parsed.
#[derive(Debug, Clone, Serialize)]
pub struct ContainerJson {
    pub name: String,
    pub state: String,
    pub idle_mode: String,
    pub cpu_percent: Option<f64>,
    pub mem_used_mb: Option<f64>,
    pub mem_limit_mb: Option<f64>,
    pub cpu_text: String,
    pub mem_text: String,
}

impl From<&LiveContainerStat> for ContainerJson {
    fn from(c: &LiveContainerStat) -> Self {
        let (used, limit) = parse_mem_usage_mb(&c.mem_usage);
        Self {
            name: c.name.clone(),
            state: c.state.clone(),
            idle_mode: c.idle_mode.clone(),
            cpu_percent: parse_cpu_percent(&c.cpu_percent),
            mem_used_mb: used,
            mem_limit_mb: limit,
            cpu_text: c.cpu_percent.clone(),
            mem_text: c.mem_usage.clone(),
        }
    }
}

/// Machine-readable `leandock status --format json` report.
#[derive(Debug, Clone, Serialize)]
pub struct StatusJsonReport<'a> {
    pub containers: Vec<ContainerJson>,
    pub kernel: &'a KernelMemoryStats,
    /// Data that could not be collected (for example the container runtime query failing), so an
    /// empty `containers` list is not mistaken for "no containers".
    pub errors: Vec<String>,
}

/// Parses `docker stats` CPU text such as `"1.25%"`.
pub fn parse_cpu_percent(text: &str) -> Option<f64> {
    text.trim()
        .strip_suffix('%')
        .and_then(|n| n.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

/// Parses a `docker stats` size such as `"48MiB"`, `"1.5GiB"`, `"512kB"` or `"0B"` into MiB.
pub fn parse_size_mb(text: &str) -> Option<f64> {
    let t = text.trim();
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let value: f64 = num.trim().parse().ok()?;
    let bytes = match unit.trim() {
        "B" | "" => 1.0,
        "KiB" => 1024.0,
        "MiB" => 1024.0 * 1024.0,
        "GiB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" => 1024.0_f64.powi(4),
        "kB" | "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    let mb = value * bytes / (1024.0 * 1024.0);
    mb.is_finite().then_some(mb)
}

/// Parses `docker stats` memory text such as `"48MiB / 256MiB"` into (used, limit) MiB.
pub fn parse_mem_usage_mb(text: &str) -> (Option<f64>, Option<f64>) {
    match text.split_once('/') {
        Some((used, limit)) => (parse_size_mb(used), parse_size_mb(limit)),
        None => (parse_size_mb(text), None),
    }
}

/// Returns the platform capability tier matrix for the detected host.
pub fn capability_tiers(profile: &HostProfile) -> Vec<TierReport> {
    let caps = &profile.platform.capabilities;
    vec![
        TierReport {
            tier: 1,
            capability: "Compose & DB Auto-Tuner + Log Cap + Cleanup".to_string(),
            status: caps.tier1_compose_and_db,
            details: "Active across all platforms".to_string(),
        },
        TierReport {
            tier: 2,
            capability: "Wake-on-Request Proxy & Static Offload".to_string(),
            status: caps.tier2_wake_proxy,
            details: "Cross-platform singleflight HTTP proxy".to_string(),
        },
        TierReport {
            tier: 3,
            capability: "Host Runtime & OS Tuning".to_string(),
            status: caps.tier3_host_tuning,
            details: caps.tier3_reason.clone(),
        },
        TierReport {
            tier: 4,
            capability: "Linux Kernel (zram, KSM, Quadlet)".to_string(),
            status: caps.tier4_kernel_opt,
            details: caps.tier4_reason.clone(),
        },
    ]
}

/// Builds the machine-readable report behind `leandock analyze --format json`.
pub fn build_analyze_json_report<'a>(
    profile: &'a HostProfile,
    plan: &'a OptimizationPlan,
    host_preview: Option<&'a HostTuningReport>,
    errors: Vec<String>,
) -> AnalyzeJsonReport<'a> {
    let tiers = capability_tiers(profile);
    let detected_tier = tiers
        .iter()
        .take_while(|t| t.status != CapabilitySupport::Skipped)
        .map(|t| t.tier)
        .last()
        .unwrap_or(0);
    let skipped_tiers = tiers
        .iter()
        .filter(|t| t.status == CapabilitySupport::Skipped)
        .cloned()
        .collect();

    AnalyzeJsonReport {
        host: AnalyzeHostSummary {
            os: profile.platform.os,
            distro: &profile.platform.distro,
            cpu_arch: &profile.hardware.cpu_arch,
            logical_cores: profile.hardware.logical_cores,
            total_ram_mb: profile.hardware.total_ram_mb,
            free_disk_gb: profile.hardware.free_disk_gb,
            virtualization: profile.platform.virtualization,
            compose_driver: profile.runtime.compose_driver.to_string(),
            is_root: profile.platform.is_root,
        },
        detected_tier,
        tiers,
        skipped_tiers,
        budget: AnalyzeBudget {
            effective_ram_mb: plan.effective_ram_mb,
            os_reserve_mb: plan.os_reserve_mb,
            allocatable_ram_mb: plan.allocatable_ram_mb,
            total_budgeted_mb: plan.total_budgeted_mb,
            estimated_default_ram_mb: plan.estimated_default_ram_mb,
            estimated_savings_mb: plan.estimated_savings_mb,
        },
        services: plan.services.iter().map(ServiceJson::from).collect(),
        host_preview,
        warnings: &plan.warnings,
        errors,
    }
}

/// Renders `leandock analyze --format json` as pretty-printed JSON.
pub fn format_analyze_json(
    profile: &HostProfile,
    plan: &OptimizationPlan,
    host_preview: Option<&HostTuningReport>,
    errors: Vec<String>,
) -> crate::error::Result<String> {
    Ok(serde_json::to_string_pretty(&build_analyze_json_report(
        profile,
        plan,
        host_preview,
        errors,
    ))?)
}

/// Renders `leandock status --format json` as pretty-printed JSON.
pub fn format_status_json(
    containers: &[LiveContainerStat],
    kernel_stats: &KernelMemoryStats,
    errors: Vec<String>,
) -> crate::error::Result<String> {
    Ok(serde_json::to_string_pretty(&StatusJsonReport {
        containers: containers.iter().map(ContainerJson::from).collect(),
        kernel: kernel_stats,
        errors,
    })?)
}

/// Prints the zero-touch `leandock analyze` dry-run report.
pub fn print_analyze_report(
    profile: &HostProfile,
    plan: &OptimizationPlan,
    host_preview: Option<&HostTuningReport>,
) {
    println!(
        "\n{}",
        "LeanDock Pre-Flight Analysis (Zero-Touch Dry Run)"
            .bold()
            .cyan()
    );
    println!(
        "Host: {} ({}, {} cores, {} MB RAM, {:.1} GB free disk) · Virtualization: {}",
        profile.platform.distro,
        profile.hardware.cpu_arch,
        profile.hardware.logical_cores,
        profile.hardware.total_ram_mb,
        profile.hardware.free_disk_gb,
        profile.platform.virtualization
    );
    println!(
        "Runtime: {} · Root privileges: {}",
        profile.runtime.compose_driver,
        if profile.platform.is_root {
            "yes"
        } else {
            "no (non-root mode)"
        }
    );

    println!("\n{}", "Platform Capability Tiers".bold());
    let mut tier_table = Table::new();
    tier_table.load_preset(UTF8_FULL);
    tier_table.set_header(vec!["Tier", "Capability", "Status", "Details"]);
    for tier in capability_tiers(profile) {
        tier_table.add_row(vec![
            Cell::new(format!("Tier {}", tier.tier)),
            Cell::new(tier.capability),
            support_cell(tier.status),
            Cell::new(tier.details),
        ]);
    }
    println!("{tier_table}");

    println!("\n{}", "Proposed Service Optimizations".bold());
    let mut svc_table = Table::new();
    svc_table.load_preset(UTF8_FULL);
    svc_table.set_header(vec![
        "Service",
        "Role",
        "Memory Cap",
        "Reservation",
        "Tuning Actions",
    ]);

    for svc in &plan.services {
        let limit_str = if svc.ignored {
            "IGNORED".to_string()
        } else if svc.offloaded_static {
            "0 MB (offloaded)".to_string()
        } else {
            svc.mem_limit_mb
                .map(|m| format!("{} MB", m))
                .unwrap_or_else(|| "user-defined".to_string())
        };

        let res_str = svc
            .mem_reservation_mb
            .map(|m| format!("{} MB", m))
            .unwrap_or_else(|| "-".to_string());

        let mut action_parts = svc.notes.clone();
        if svc.inject_log_rotation {
            action_parts.push("log cap 10m x 3".to_string());
        }
        if action_parts.is_empty() {
            action_parts.push("memory & log hygiene".to_string());
        }

        svc_table.add_row(vec![
            Cell::new(&svc.name),
            Cell::new(svc.role.to_string()),
            Cell::new(limit_str),
            Cell::new(res_str),
            Cell::new(action_parts.join("; ")),
        ]);
    }
    println!("{svc_table}");

    println!(
        "\n{} Effective budget: {} MB (OS reserve: {} MB) · Stack budgeted: {} MB · Est. unbounded default: {} MB · Est. RAM saved: {} MB",
        "Summary:".bold().green(),
        plan.effective_ram_mb,
        plan.os_reserve_mb,
        plan.total_budgeted_mb,
        plan.estimated_default_ram_mb,
        plan.estimated_savings_mb
    );

    if let Some(hp) = host_preview {
        for item in &hp.applied {
            println!("  [host] {}", item);
        }
        for item in &hp.skipped {
            println!("  [host-skip] {}", item);
        }
        for w in &hp.warnings {
            println!("  {} {}", "WARNING:".yellow().bold(), w);
        }
    }

    for w in &plan.warnings {
        println!("  {} {}", "WARNING:".yellow().bold(), w);
    }
}

/// Prints the live `leandock status` dashboard.
pub fn print_status_report(containers: &[LiveContainerStat], kernel_stats: &KernelMemoryStats) {
    println!("\n{}", "LeanDock Live Stack Status".bold().cyan());

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec![
        "Container",
        "State",
        "CPU %",
        "RAM Usage",
        "Wake-on-Request",
    ]);

    if containers.is_empty() {
        table.add_row(vec![
            Cell::new("(no containers found)"),
            Cell::new("-"),
            Cell::new("-"),
            Cell::new("-"),
            Cell::new("-"),
        ]);
    } else {
        for c in containers {
            table.add_row(vec![
                Cell::new(&c.name),
                Cell::new(&c.state),
                Cell::new(&c.cpu_percent),
                Cell::new(&c.mem_usage),
                Cell::new(&c.idle_mode),
            ]);
        }
    }
    println!("{table}");

    println!("\n{}", "Kernel Memory Optimizations".bold());
    if kernel_stats.zram_active {
        println!(
            "  zram (zstd): Active · Orig: {:.1} MB -> Compressed: {:.1} MB (Ratio: {:.2}x)",
            kernel_stats.zram_orig_data_mb,
            kernel_stats.zram_compr_data_mb,
            kernel_stats.zram_ratio
        );
    } else {
        println!("  zram: Inactive or skipped on this platform");
    }

    if kernel_stats.ksm_active {
        println!(
            "  KSM: Active · Shared pages: {} (~{:.1} MB RAM deduplicated)",
            kernel_stats.ksm_pages_sharing, kernel_stats.ksm_saved_mb
        );
    } else {
        println!("  KSM: Inactive or skipped on this platform");
    }
}

/// Prints the `leandock doctor` full diagnostic report.
pub fn print_doctor_report(profile: &HostProfile) {
    println!("\n{}", "LeanDock Environment Doctor".bold().cyan());

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec!["Check", "Value", "Status"]);

    table.add_row(vec![
        Cell::new("Operating System"),
        Cell::new(format!(
            "{} ({})",
            profile.platform.distro, profile.platform.os
        )),
        Cell::new("OK").fg(Color::Green),
    ]);
    table.add_row(vec![
        Cell::new("CPU Architecture"),
        Cell::new(format!(
            "{} ({} logical cores)",
            profile.hardware.cpu_arch, profile.hardware.logical_cores
        )),
        Cell::new("OK").fg(Color::Green),
    ]);
    table.add_row(vec![
        Cell::new("Memory & Swap"),
        Cell::new(format!(
            "{} MB RAM ({} MB avail) / {} MB swap",
            profile.hardware.total_ram_mb,
            profile.hardware.available_ram_mb,
            profile.hardware.total_swap_mb
        )),
        if profile.hardware.is_ultra_low_ram() {
            Cell::new("TIGHT").fg(Color::Yellow)
        } else {
            Cell::new("OK").fg(Color::Green)
        },
    ]);
    table.add_row(vec![
        Cell::new("Virtualization"),
        Cell::new(profile.platform.virtualization.to_string()),
        if profile
            .platform
            .virtualization
            .is_restricted_container_vps()
        {
            Cell::new("RESTRICTED").fg(Color::Yellow)
        } else {
            Cell::new("OK").fg(Color::Green)
        },
    ]);
    table.add_row(vec![
        Cell::new("Kernel / systemd"),
        Cell::new(format!(
            "{} / systemd {}",
            profile.platform.kernel_version.as_deref().unwrap_or("n/a"),
            profile
                .platform
                .systemd_version
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        )),
        Cell::new("INFO"),
    ]);
    table.add_row(vec![
        Cell::new("Compose Driver"),
        Cell::new(profile.runtime.compose_driver.to_string()),
        if profile.runtime.compose_driver.command_prefix().is_some() {
            Cell::new("OK").fg(Color::Green)
        } else {
            Cell::new("MISSING").fg(Color::Red)
        },
    ]);
    table.add_row(vec![
        Cell::new("Container Socket"),
        Cell::new(
            profile
                .runtime
                .socket_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "CLI fallback adapter".to_string()),
        ),
        Cell::new("OK").fg(Color::Green),
    ]);
    table.add_row(vec![
        Cell::new("Optional Helpers"),
        Cell::new(format!(
            "podlet: {} · slim: {}",
            if profile.runtime.has_podlet {
                "installed"
            } else {
                "absent"
            },
            if profile.runtime.has_slim {
                "installed"
            } else {
                "absent"
            }
        )),
        Cell::new("INFO"),
    ]);
    table.add_row(vec![
        Cell::new("Privileges"),
        Cell::new(if profile.platform.is_root {
            "root (all tiers enabled where supported)"
        } else {
            "non-root (Tier 1 & Tier 2 active; Tier 3/4 skipped safely)"
        }),
        if profile.platform.is_root {
            Cell::new("FULL").fg(Color::Green)
        } else {
            Cell::new("SAFE-USER").fg(Color::Yellow)
        },
    ]);

    println!("{table}");
}

/// Captures a live host/stack snapshot for `leandock bench`.
pub fn capture_benchmark_snapshot(label: &str, startup_ms: u64) -> BenchmarkSnapshot {
    let mut sys = sysinfo::System::new_all();
    sys.refresh_memory();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let ram_used_mb = sys.used_memory() / (1024 * 1024);
    let swap_zram_used_mb = sys.used_swap() / (1024 * 1024);
    let process_count = sys.processes().len();

    let disks = sysinfo::Disks::new_with_refreshed_list();
    let disk_used_gb = disks
        .list()
        .first()
        .map(|d| (d.total_space().saturating_sub(d.available_space())) as f64 / 1_073_741_824.0)
        .unwrap_or(0.0);

    BenchmarkSnapshot {
        label: label.to_string(),
        ram_used_mb,
        swap_zram_used_mb,
        disk_used_gb,
        process_count,
        startup_ms,
    }
}

/// Computes and renders a Before vs. After benchmark report in Table, Markdown, or JSON format.
pub fn format_benchmark_report(
    before: BenchmarkSnapshot,
    after: BenchmarkSnapshot,
    format: OutputFormat,
) -> String {
    let ram_delta_mb = after.ram_used_mb as i64 - before.ram_used_mb as i64;
    let ram_delta_pct = if before.ram_used_mb > 0 {
        (ram_delta_mb as f64 / before.ram_used_mb as f64) * 100.0
    } else {
        0.0
    };
    let disk_delta_gb = after.disk_used_gb - before.disk_used_gb;

    let comparison = BenchmarkComparison {
        before,
        after,
        ram_delta_mb,
        ram_delta_pct,
        disk_delta_gb,
    };

    match format {
        OutputFormat::Json => serde_json::to_string_pretty(&comparison).unwrap_or_default(),
        OutputFormat::Markdown => {
            format!(
                "| Metric | Before (`{}`) | After (`{}`) | Delta |\n\
                 |---|---:|---:|---:|\n\
                 | RAM Used (MB) | {} | {} | {:+} ({:+.1}%) |\n\
                 | Swap/zram Used (MB) | {} | {} | {:+} |\n\
                 | Disk Used (GB) | {:.2} | {:.2} | {:+.2} |\n\
                 | Processes | {} | {} | {:+} |\n\
                 | Startup Time (ms) | {} | {} | {:+} |\n",
                comparison.before.label,
                comparison.after.label,
                comparison.before.ram_used_mb,
                comparison.after.ram_used_mb,
                comparison.ram_delta_mb,
                comparison.ram_delta_pct,
                comparison.before.swap_zram_used_mb,
                comparison.after.swap_zram_used_mb,
                comparison.after.swap_zram_used_mb as i64
                    - comparison.before.swap_zram_used_mb as i64,
                comparison.before.disk_used_gb,
                comparison.after.disk_used_gb,
                comparison.disk_delta_gb,
                comparison.before.process_count,
                comparison.after.process_count,
                comparison.after.process_count as i64 - comparison.before.process_count as i64,
                comparison.before.startup_ms,
                comparison.after.startup_ms,
                comparison.after.startup_ms as i64 - comparison.before.startup_ms as i64,
            )
        }
        OutputFormat::Table => {
            let mut table = Table::new();
            table.load_preset(UTF8_FULL);
            table.set_header(vec![
                "Metric",
                &comparison.before.label,
                &comparison.after.label,
                "Delta",
            ]);
            table.add_row(vec![
                "RAM Used (MB)".to_string(),
                comparison.before.ram_used_mb.to_string(),
                comparison.after.ram_used_mb.to_string(),
                format!(
                    "{:+} MB ({:+.1}%)",
                    comparison.ram_delta_mb, comparison.ram_delta_pct
                ),
            ]);
            table.add_row(vec![
                "Swap/zram Used (MB)".to_string(),
                comparison.before.swap_zram_used_mb.to_string(),
                comparison.after.swap_zram_used_mb.to_string(),
                format!(
                    "{:+} MB",
                    comparison.after.swap_zram_used_mb as i64
                        - comparison.before.swap_zram_used_mb as i64
                ),
            ]);
            table.add_row(vec![
                "Disk Used (GB)".to_string(),
                format!("{:.2}", comparison.before.disk_used_gb),
                format!("{:.2}", comparison.after.disk_used_gb),
                format!("{:+.2} GB", comparison.disk_delta_gb),
            ]);
            table.add_row(vec![
                "Host Processes".to_string(),
                comparison.before.process_count.to_string(),
                comparison.after.process_count.to_string(),
                format!(
                    "{:+}",
                    comparison.after.process_count as i64 - comparison.before.process_count as i64
                ),
            ]);
            table.add_row(vec![
                "Startup Time (ms)".to_string(),
                comparison.before.startup_ms.to_string(),
                comparison.after.startup_ms.to_string(),
                format!(
                    "{:+} ms",
                    comparison.after.startup_ms as i64 - comparison.before.startup_ms as i64
                ),
            ]);
            table.to_string()
        }
    }
}

/// Queries live container stats via `docker stats --no-stream` / `podman stats --no-stream`.
pub fn collect_live_container_stats(runtime_bin: &str) -> Vec<LiveContainerStat> {
    try_collect_live_container_stats(runtime_bin).unwrap_or_default()
}

/// Like [`collect_live_container_stats`], but reports a failed `ps` query (runtime missing or daemon
/// not reachable) as an error instead of an empty list.
pub fn try_collect_live_container_stats(
    runtime_bin: &str,
) -> crate::error::Result<Vec<LiveContainerStat>> {
    let mut stats_map = std::collections::HashMap::new();
    if let Ok(out) = Command::new(runtime_bin)
        .args([
            "stats",
            "--no-stream",
            "--format",
            "{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}",
        ])
        .output()
    {
        if out.status.success() {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 3 {
                    stats_map.insert(
                        parts[0].trim().to_string(),
                        (parts[1].trim().to_string(), parts[2].trim().to_string()),
                    );
                }
            }
        }
    }

    let mut rows = Vec::new();
    let out = Command::new(runtime_bin)
        .args(["ps", "-a", "--format", "{{.Names}}|{{.State}}|{{.Labels}}"])
        .output()
        .map_err(|_| crate::error::LeanDockError::RuntimeNotFound)?;
    if !out.status.success() {
        return Err(crate::error::LeanDockError::CommandFailed {
            cmd: format!("{runtime_bin} ps -a"),
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() < 3 {
            continue;
        }
        let name = parts[0].trim().to_string();
        let state = parts[1].trim().to_string();
        let labels = parts[2];
        let idle_enabled = labels.contains("leandock.idle.enable=true");
        let idle_mode = if !idle_enabled {
            "Always-On".to_string()
        } else if state.eq_ignore_ascii_case("running") {
            "Wake Proxy: Active".to_string()
        } else {
            "Wake Proxy: Sleeping (0 MB)".to_string()
        };

        let (cpu, mem) = stats_map
            .get(&name)
            .cloned()
            .unwrap_or_else(|| ("0.00%".to_string(), "0B / 0B".to_string()));

        rows.push(LiveContainerStat {
            name,
            state,
            cpu_percent: cpu,
            mem_usage: mem,
            idle_mode,
        });
    }

    Ok(rows)
}

fn support_cell(support: CapabilitySupport) -> Cell {
    match support {
        CapabilitySupport::Full => Cell::new("Full").fg(Color::Green),
        CapabilitySupport::Partial => Cell::new("Partial").fg(Color::Yellow),
        CapabilitySupport::Skipped => Cell::new("Auto-Skipped").fg(Color::DarkGrey),
    }
}
