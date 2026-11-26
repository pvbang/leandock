use std::process::Command;

use comfy_table::{presets::UTF8_FULL, Cell, Color, Table};
use owo_colors::OwoColorize;
use serde::{Deserialize, Serialize};

use crate::postrun::KernelMemoryStats;
use crate::prerun::db_tuner::OptimizationPlan;
use crate::prerun::host_tuner::HostTuningReport;
use crate::profiler::{CapabilitySupport, HostProfile};

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
    let caps = &profile.platform.capabilities;
    tier_table.add_row(vec![
        Cell::new("Tier 1"),
        Cell::new("Compose & DB Auto-Tuner + Log Cap + Cleanup"),
        support_cell(caps.tier1_compose_and_db),
        Cell::new("Active across all platforms"),
    ]);
    tier_table.add_row(vec![
        Cell::new("Tier 2"),
        Cell::new("Wake-on-Request Proxy & Static Offload"),
        support_cell(caps.tier2_wake_proxy),
        Cell::new("Cross-platform singleflight HTTP proxy"),
    ]);
    tier_table.add_row(vec![
        Cell::new("Tier 3"),
        Cell::new("Host Runtime & OS Tuning"),
        support_cell(caps.tier3_host_tuning),
        Cell::new(&caps.tier3_reason),
    ]);
    tier_table.add_row(vec![
        Cell::new("Tier 4"),
        Cell::new("Linux Kernel (zram, KSM, Quadlet)"),
        support_cell(caps.tier4_kernel_opt),
        Cell::new(&caps.tier4_reason),
    ]);
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

/// Formats the live `leandock status` dashboard as text.
pub fn format_status_report(
    containers: &[LiveContainerStat],
    kernel_stats: &KernelMemoryStats,
) -> String {
    let mut output = format!("\n{}\n", "LeanDock Live Stack Status".bold().cyan());

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

    output.push_str(&table.to_string());
    output.push_str(&format!("\n\n{}\n", "Kernel Memory Optimizations".bold()));

    if kernel_stats.zram_active {
        output.push_str(&format!(
            "  zram (zstd): Active · Orig: {:.1} MB -> Compressed: {:.1} MB (Ratio: {:.2}x)\n",
            kernel_stats.zram_orig_data_mb,
            kernel_stats.zram_compr_data_mb,
            kernel_stats.zram_ratio
        ));
    } else {
        output.push_str("  zram: Inactive or skipped on this platform\n");
    }

    if kernel_stats.ksm_active {
        output.push_str(&format!(
            "  KSM: Active · Shared pages: {} (~{:.1} MB RAM deduplicated)\n",
            kernel_stats.ksm_pages_sharing, kernel_stats.ksm_saved_mb
        ));
    } else {
        output.push_str("  KSM: Inactive or skipped on this platform\n");
    }

    output
}

/// Prints the live `leandock status` dashboard.
pub fn print_status_report(containers: &[LiveContainerStat], kernel_stats: &KernelMemoryStats) {
    println!("{}", format_status_report(containers, kernel_stats));
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
fn run_runtime_command(
    runtime_bin: &str,
    args: &[&str],
) -> crate::error::Result<std::process::Output> {
    let output = Command::new(runtime_bin).args(args).output()?;

    if !output.status.success() {
        return Err(crate::error::LeanDockError::CommandFailed {
            cmd: format!("{} {}", runtime_bin, args.join(" ")),
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }

    Ok(output)
}

/// Queries live container stats via Docker or Podman.
///
/// Preserves the original infallible public API.
pub fn collect_live_container_stats(runtime_bin: &str) -> Vec<LiveContainerStat> {
    try_collect_live_container_stats(runtime_bin).unwrap_or_default()
}
/// Queries live container stats via Docker or Podman.
pub fn try_collect_live_container_stats(
    runtime_bin: &str,
) -> crate::error::Result<Vec<LiveContainerStat>> {
    let stats = run_runtime_command(
        runtime_bin,
        &[
            "stats",
            "--no-stream",
            "--format",
            "{{.Name}}|{{.CPUPerc}}|{{.MemUsage}}",
        ],
    )?;

    let mut stats_map = std::collections::HashMap::new();

    for line in String::from_utf8_lossy(&stats.stdout).lines() {
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() >= 3 {
            stats_map.insert(
                parts[0].trim().to_string(),
                (parts[1].trim().to_string(), parts[2].trim().to_string()),
            );
        }
    }

    let output = run_runtime_command(
        runtime_bin,
        &["ps", "-a", "--format", "{{.Names}}|{{.State}}|{{.Labels}}"],
    )?;

    let mut rows = Vec::new();

    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() < 3 {
            continue;
        }

        let name = parts[0].trim().to_string();
        let state = parts[1].trim().to_string();
        let labels = parts[2];

        let idle_mode = if !labels.contains("leandock.idle.enable=true") {
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

// JSON output for `leandock analyze`.
pub fn format_analyze_json_report(
    profile: &HostProfile,
    plan: &OptimizationPlan,
    host_preview: Option<&HostTuningReport>,
) -> crate::error::Result<String> {
    let mut plan_value = serde_json::to_value(plan)?;

    // Do not expose database command arguments or credentials.
    if let Some(services) = plan_value
        .get_mut("services")
        .and_then(serde_json::Value::as_array_mut)
    {
        for service in services {
            if let Some(object) = service.as_object_mut() {
                object.remove("tuned_command");
            }
        }
    }

    let mut report = serde_json::Map::new();
    report.insert("host".to_string(), serde_json::to_value(profile)?);
    report.insert("optimization_plan".to_string(), plan_value);
    report.insert(
        "host_preview".to_string(),
        serde_json::to_value(host_preview)?,
    );

    Ok(serde_json::to_string_pretty(&serde_json::Value::Object(
        report,
    ))?)
}

fn parse_cpu_percent(value: &str) -> Option<f64> {
    value.trim().strip_suffix('%')?.trim().parse().ok()
}

fn parse_memory_value_bytes(value: &str) -> Option<u64> {
    let value = value.trim();

    let number_end = value.find(|c: char| !c.is_ascii_digit() && c != '.')?;

    let number: f64 = value[..number_end].parse().ok()?;
    let unit = value[number_end..].trim();

    if !number.is_finite() || number < 0.0 {
        return None;
    }

    let multiplier = match unit {
        "B" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        "KiB" => 1_024.0,
        "MiB" => 1_048_576.0,
        "GiB" => 1_073_741_824.0,
        "TiB" => 1_099_511_627_776.0,
        _ => return None,
    };

    let bytes = number * multiplier;

    if !bytes.is_finite() || bytes > u64::MAX as f64 {
        return None;
    }

    Some(bytes.round() as u64)
}

fn parse_memory_usage(value: &str) -> (Option<u64>, Option<u64>) {
    match value.split_once('/') {
        Some((usage, limit)) => (
            parse_memory_value_bytes(usage),
            parse_memory_value_bytes(limit),
        ),
        None => (parse_memory_value_bytes(value), None),
    }
}

// JSON output for `leandock status`.
pub fn format_status_json_report(
    containers: &[LiveContainerStat],
    kernel_stats: &KernelMemoryStats,
) -> crate::error::Result<String> {
    let mut container_values = serde_json::to_value(containers)?;

    if let Some(items) = container_values.as_array_mut() {
        for item in items {
            let Some(object) = item.as_object_mut() else {
                continue;
            };

            let cpu = object
                .get("cpu_percent")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_cpu_percent);

            let (memory_usage, memory_limit) = object
                .get("mem_usage")
                .and_then(serde_json::Value::as_str)
                .map(parse_memory_usage)
                .unwrap_or((None, None));

            object.insert("cpu_percent_value".to_string(), serde_json::to_value(cpu)?);
            object.insert(
                "memory_usage_bytes".to_string(),
                serde_json::to_value(memory_usage)?,
            );
            object.insert(
                "memory_limit_bytes".to_string(),
                serde_json::to_value(memory_limit)?,
            );
        }
    }

    let mut report = serde_json::Map::new();
    report.insert("containers".to_string(), container_values);
    report.insert(
        "kernel_memory".to_string(),
        serde_json::to_value(kernel_stats)?,
    );

    Ok(serde_json::to_string_pretty(&serde_json::Value::Object(
        report,
    ))?)
}
