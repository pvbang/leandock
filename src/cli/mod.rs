use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use owo_colors::OwoColorize;

use crate::backup::BackupManager;
use crate::error::{LeanDockError, Result};
use crate::postrun::cleaner::run_safe_cleanup;
use crate::postrun::health_guard::verify_deployment_health;
use crate::postrun::{apply_kernel_optimizations, read_kernel_memory_stats};
use crate::prerun::compose_parser::ParsedCompose;
use crate::prerun::db_tuner::{build_optimization_plan, TunerOptions};
use crate::prerun::host_tuner::{apply_host_tuning, scan_podman_compatibility};
use crate::prerun::minifier::minify_eligible_services;
use crate::prerun::static_offload::extract_static_frontends;
use crate::prerun::write_override_file;
use crate::profiler::{discover_compose_file, ComposeDriver, ContainerRuntimeKind, HostProfile};
use crate::proxy::start_proxy_daemon;
use crate::report::{
    capture_benchmark_snapshot, format_analyze_json_report, format_benchmark_report,
    format_status_json_report, print_analyze_report, print_doctor_report, print_status_report,
    try_collect_live_container_stats, OutputFormat,
};

#[derive(Debug, Parser)]
#[command(
    name = "leandock",
    version,
    about = "Drop-in, zero-config, non-destructive resource optimizer for Docker Compose",
    long_about = "LeanDock optimizes Docker Compose stacks across Linux VPS, Windows (WSL2), and macOS without modifying your original docker-compose.yml."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum EngineArg {
    Docker,
    Podman,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum BenchFormatArg {
    Table,
    Markdown,
    Json,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReportFormatArg {
    #[value(name = "text", alias = "table")]
    Table,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Zero-touch dry-run: inspect hardware tier, ignored services, and proposed optimizations.
    Analyze {
        /// Path to docker-compose.yml / compose.yaml (auto-discovered if omitted).
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
        /// Preview Redis/Valkey ephemeral cache mode (`allkeys-lru` + `--save ""`).
        #[arg(long)]
        ephemeral_cache: bool,
    },

    /// Generate non-destructive `.leandock/compose.override.yml`, apply safe optimizations, and start stack.
    Up {
        /// Path to docker-compose.yml / compose.yaml.
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
        /// Select container engine (`docker` or `podman`).
        #[arg(long, value_enum)]
        engine: Option<EngineArg>,
        /// Skip Linux kernel optimizations (`zram`, `KSM`).
        #[arg(long)]
        no_kernel: bool,
        /// Skip post-deploy dangling image & build cache cleanup.
        #[arg(long)]
        no_cleanup: bool,
        /// Opt-in: minify eligible app images with SlimToolkit (`slim`) + startup verification.
        #[arg(long)]
        minify: bool,
        /// Opt-in: extract static assets from `leandock.static_dir` frontends to LeanDock's Rust server.
        #[arg(long)]
        extract_static: bool,
        /// Opt-in: treat Redis/Valkey as an ephemeral cache (`allkeys-lru` + `--save ""`).
        #[arg(long)]
        ephemeral_cache: bool,
        /// Automatically roll back to raw Compose if any container crash-loops post-deploy.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        auto_rollback: bool,
        /// Health guard stability window in seconds.
        #[arg(long, default_value_t = 10)]
        health_window: u64,
        /// Show planned actions without modifying host files or launching containers.
        #[arg(long)]
        dry_run: bool,
    },

    /// Stop and remove the Compose stack cleanly.
    Down {
        /// Path to docker-compose.yml / compose.yaml.
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
        /// Select container engine (`docker` or `podman`).
        #[arg(long, value_enum)]
        engine: Option<EngineArg>,
    },

    /// Run the cross-platform Wake-on-Request reverse proxy & optional static frontend server.
    Proxy {
        /// Manual port mappings (`service:listen_port:target_port[:idle_timeout]`).
        #[arg(long = "port-map")]
        port_map: Vec<String>,
        /// Linux only: install a lightweight systemd unit (`leandock-proxy.service`) instead of running foreground.
        #[arg(long)]
        install_service: bool,
    },

    /// Export daemonless systemd Quadlet units (Linux + Podman).
    Quadlet {
        /// Path to docker-compose.yml / compose.yaml.
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
        /// Output directory for `.container` unit files.
        #[arg(short = 'o', long = "output", default_value = ".leandock/quadlet")]
        output: PathBuf,
    },

    /// Show live container RAM/CPU, zram compression, KSM savings, and Wake-on-Request states.
    Status,

    /// Capture reproducible Before vs. After resource metrics (RAM, Swap/zram, Disk, Processes, Startup).
    Bench {
        /// Path to docker-compose.yml / compose.yaml.
        #[arg(short = 'f', long = "file")]
        file: Option<PathBuf>,
        /// Output format (`table`, `markdown`, or `json`).
        #[arg(long, value_enum, default_value_t = BenchFormatArg::Table)]
        format: BenchFormatArg,
        /// Seconds to let containers settle before sampling RAM.
        #[arg(long, default_value_t = 8)]
        settle_secs: u64,
    },

    /// Restore backed-up host configs (`daemon.json`, `sysctl`, `zram`, `KSM`, `.wslconfig`) and clean `.leandock/`.
    Revert {
        /// Restore all historical backup snapshots (defaults to latest snapshot).
        #[arg(long)]
        all: bool,
    },

    /// Run full cross-platform environment diagnostics (OS tier, virtualization, sockets, permissions).
    Doctor,
}

/// Top-level async CLI dispatcher.
fn extract_report_format_args(mut args: Vec<OsString>) -> (Vec<OsString>, ReportFormatArg) {
    let is_report_command = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|command| matches!(command, "analyze" | "status"));

    if !is_report_command {
        return (args, ReportFormatArg::Table);
    }

    let mut filtered = Vec::with_capacity(args.len());
    filtered.push(args.remove(0));

    let mut format = ReportFormatArg::Table;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if arg == OsStr::new("--format") {
            let value = iter.next().unwrap_or_else(|| {
                clap::Error::raw(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "Missing value for --format (expected text or json)",
                )
                .exit()
            });

            format = match value.to_str() {
                Some("text" | "table") => ReportFormatArg::Table,
                Some("json") => ReportFormatArg::Json,
                _ => clap::Error::raw(
                    clap::error::ErrorKind::InvalidValue,
                    "Invalid --format value (expected text or json)",
                )
                .exit(),
            };
        } else if let Some(value) = arg.to_str().and_then(|s| s.strip_prefix("--format=")) {
            format = match value {
                "text" | "table" => ReportFormatArg::Table,
                "json" => ReportFormatArg::Json,
                _ => clap::Error::raw(
                    clap::error::ErrorKind::InvalidValue,
                    "Invalid --format value (expected text or json)",
                )
                .exit(),
            };
        } else {
            filtered.push(arg);
        }
    }

    (filtered, format)
}

/// Top-level async CLI dispatcher.
pub async fn run() -> Result<()> {
    let (args, report_format) = extract_report_format_args(std::env::args_os().collect());
    let cli = Cli::parse_from(args);
    let cwd = std::env::current_dir()?;

    match cli.command {
        Commands::Analyze {
            file,
            ephemeral_cache,
        } => cmd_analyze(&cwd, file.as_deref(), ephemeral_cache, report_format),
        Commands::Up {
            file,
            engine,
            no_kernel,
            no_cleanup,
            minify,
            extract_static,
            ephemeral_cache,
            auto_rollback,
            health_window,
            dry_run,
        } => cmd_up(UpArgs {
            cwd: &cwd,
            file: file.as_deref(),
            engine,
            no_kernel,
            no_cleanup,
            minify,
            extract_static,
            ephemeral_cache,
            auto_rollback,
            health_window,
            dry_run,
        }),
        Commands::Down { file, engine } => cmd_down(&cwd, file.as_deref(), engine),
        Commands::Proxy {
            port_map,
            install_service,
        } => {
            let profile = HostProfile::detect();
            if install_service {
                return cmd_install_proxy_service(&profile, &cwd, &port_map);
            }
            println!(
                "{}",
                "Starting LeanDock Wake-on-Request Reverse Proxy (Ctrl+C to stop)..."
                    .bold()
                    .cyan()
            );
            start_proxy_daemon(&profile, &cwd, &port_map).await
        }
        Commands::Quadlet { file, output } => cmd_quadlet(&cwd, file.as_deref(), &output),
        Commands::Status => cmd_status(report_format),
        Commands::Bench {
            file,
            format,
            settle_secs,
        } => cmd_bench(&cwd, file.as_deref(), format, settle_secs),
        Commands::Revert { all } => cmd_revert(&cwd, all),
        Commands::Doctor => {
            let profile = HostProfile::detect();
            print_doctor_report(&profile);
            Ok(())
        }
    }
}

fn cmd_analyze(
    cwd: &Path,
    file: Option<&Path>,
    ephemeral_cache: bool,
    format: ReportFormatArg,
) -> Result<()> {
    let profile = HostProfile::detect();
    let compose_path = discover_compose_file(file, cwd)?;
    let compose = ParsedCompose::from_file(&compose_path)?;

    let opts = TunerOptions {
        effective_ram_mb: profile.effective_ram_mb(),
        logical_cores: profile.hardware.logical_cores,
        swap_and_zram_mb: profile.hardware.total_swap_mb,
        ephemeral_cache,
        offloaded_frontends: Vec::new(),
        minified_images: BTreeMap::new(),
    };

    let plan = build_optimization_plan(&compose, &opts);
    let backup_mgr = BackupManager::default_for_host(profile.platform.is_root);
    let host_preview = backup_mgr
        .start_session()
        .ok()
        .and_then(|mut s| apply_host_tuning(&profile, cwd, &mut s, true).ok());

    match format {
        ReportFormatArg::Table => print_analyze_report(&profile, &plan, host_preview.as_ref()),
        ReportFormatArg::Json => println!(
            "{}",
            format_analyze_json_report(&profile, &plan, host_preview.as_ref())?
        ),
    }
    Ok(())
}

struct UpArgs<'a> {
    cwd: &'a Path,
    file: Option<&'a Path>,
    engine: Option<EngineArg>,
    no_kernel: bool,
    no_cleanup: bool,
    minify: bool,
    extract_static: bool,
    ephemeral_cache: bool,
    auto_rollback: bool,
    health_window: u64,
    dry_run: bool,
}

fn cmd_up(args: UpArgs<'_>) -> Result<()> {
    let profile = HostProfile::detect();
    let compose_path = discover_compose_file(args.file, args.cwd)?;
    let project_dir = compose_path.parent().unwrap_or(args.cwd);
    let compose = ParsedCompose::from_file(&compose_path)?;

    let requested_engine = args.engine.map(|e| match e {
        EngineArg::Docker => "docker",
        EngineArg::Podman => "podman",
    });

    if requested_engine == Some("podman") {
        for warn in scan_podman_compatibility(&compose) {
            println!("{} {}", "PODMAN WARNING:".yellow().bold(), warn);
        }
    }

    let driver = profile.runtime.resolve_driver(requested_engine);
    if driver.command_prefix().is_none() && !args.dry_run {
        return Err(LeanDockError::RuntimeNotFound);
    }

    let runtime_bin = match driver {
        ComposeDriver::PodmanCompose => "podman",
        _ => "docker",
    };

    // Opt-in static frontend offloading.
    let mut offloaded_frontends = Vec::new();
    if args.extract_static && !args.dry_run {
        let leandock_dir = project_dir.join(".leandock");
        let sites = extract_static_frontends(&compose, runtime_bin, &leandock_dir)?;
        for site in sites {
            println!(
                "  {} Extracted static frontend `{}` -> {} (port {})",
                "✓".green(),
                site.service_name,
                site.output_dir.display(),
                site.listen_port
            );
            offloaded_frontends.push(site.service_name);
        }
    }

    // Opt-in SlimToolkit minification.
    let mut minified_images = BTreeMap::new();
    if args.minify && !args.dry_run {
        let (min_map, outcomes) =
            minify_eligible_services(&compose, runtime_bin, profile.runtime.has_slim)?;
        for o in outcomes {
            println!("  [minify] {}", o.message);
        }
        minified_images = min_map;
    }

    let opts = TunerOptions {
        effective_ram_mb: profile.effective_ram_mb(),
        logical_cores: profile.hardware.logical_cores,
        swap_and_zram_mb: profile.hardware.total_swap_mb,
        ephemeral_cache: args.ephemeral_cache,
        offloaded_frontends,
        minified_images,
    };
    let plan = build_optimization_plan(&compose, &opts);

    let backup_mgr = BackupManager::default_for_host(profile.platform.is_root);
    let mut backup_session = backup_mgr.start_session()?;

    // Phase 1.1: Host & platform tuning (Tier 3)
    let host_report = apply_host_tuning(&profile, project_dir, &mut backup_session, args.dry_run)?;

    if args.dry_run {
        print_analyze_report(&profile, &plan, Some(&host_report));
        if !args.no_kernel {
            let kernel_preview = apply_kernel_optimizations(&profile, &mut backup_session, true)?;
            for item in &kernel_preview.applied {
                println!("  [kernel] {}", item);
            }
            for item in &kernel_preview.skipped {
                println!("  [kernel-skip] {}", item);
            }
        }
        println!(
            "\n{}",
            "Dry run complete. Zero files or containers were modified.".cyan()
        );
        return Ok(());
    }

    for msg in &host_report.applied {
        println!("  {} {}", "✓".green(), msg);
    }
    for msg in &host_report.skipped {
        println!("  {} {}", "·".dimmed(), msg);
    }
    for warn in &host_report.warnings {
        println!("  {} {}", "!".yellow().bold(), warn);
    }

    // Phase 1.2: Emit isolated `.leandock/compose.override.yml`
    let override_path = write_override_file(project_dir, &plan)?;
    println!(
        "  {} Generated non-destructive override: {}",
        "✓".green(),
        override_path.display()
    );

    // Phase 2.1: Linux kernel memory optimizations (`zram`, `KSM`)
    if !args.no_kernel {
        let kernel_report = apply_kernel_optimizations(&profile, &mut backup_session, false)?;
        for msg in &kernel_report.applied {
            println!("  {} {}", "✓".green(), msg);
        }
        for msg in &kernel_report.skipped {
            println!("  {} {}", "·".dimmed(), msg);
        }
        for warn in &kernel_report.warnings {
            println!("  {} {}", "!".yellow().bold(), warn);
        }
    }

    // Launch Compose with original file + `.leandock/compose.override.yml`.
    println!("\n{}", "Starting optimized Compose stack...".bold().cyan());
    invoke_compose_up(&driver, &compose_path, Some(&override_path))?;

    // Phase 2.2: Health Verification Guard + Auto-Rollback
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    spinner.set_message(format!(
        "Verifying container stability for {}s (Health Guard)...",
        args.health_window
    ));
    spinner.enable_steady_tick(Duration::from_millis(120));

    let health = verify_deployment_health(
        &driver,
        runtime_bin,
        &compose_path,
        &override_path,
        Duration::from_secs(args.health_window),
        args.auto_rollback,
    )?;
    spinner.finish_and_clear();

    if !health.healthy {
        let failing = health
            .failing_service
            .as_deref()
            .unwrap_or("unknown-service");
        eprintln!(
            "\n{} Container `{}` entered an unhealthy/crash-loop state!",
            "HEALTH GUARD ALERT:".red().bold(),
            failing
        );
        if !health.failing_logs_tail.is_empty() {
            eprintln!("--- Last 30 lines of logs for `{}` ---", failing);
            for line in &health.failing_logs_tail {
                eprintln!("  {}", line);
            }
            eprintln!("--------------------------------------");
        }
        if health.rolled_back {
            eprintln!(
                "{} Automatically rolled back to your unmodified original Compose file.",
                "AUTO-ROLLBACK COMPLETE:".yellow().bold()
            );
        }
        return Err(LeanDockError::HealthCheckFailed(format!(
            "Service `{}` failed post-deploy stability check",
            failing
        )));
    }

    println!(
        "  {} Health Guard verified {} container(s) stable.",
        "✓".green(),
        health.checked_containers.len()
    );

    // Phase 2.3: Safe Post-Deploy Cleanup (ONLY after health_guard passes!)
    if !args.no_cleanup {
        let cleanup = run_safe_cleanup(runtime_bin, false)?;
        println!(
            "  {} Safe disk cleanup reclaimed {:.1} MB (volumes and active images untouched).",
            "✓".green(),
            cleanup.total_reclaimed_mb
        );
    }

    println!(
        "\n{} Stack is up and optimized. Estimated RAM saved: ~{} MB.",
        "LeanDock Ready:".bold().green(),
        plan.estimated_savings_mb
    );
    Ok(())
}

fn cmd_down(cwd: &Path, file: Option<&Path>, engine: Option<EngineArg>) -> Result<()> {
    let profile = HostProfile::detect();
    let compose_path = discover_compose_file(file, cwd)?;
    let project_dir = compose_path.parent().unwrap_or(cwd);
    let override_path = project_dir.join(".leandock").join("compose.override.yml");

    let requested_engine = engine.map(|e| match e {
        EngineArg::Docker => "docker",
        EngineArg::Podman => "podman",
    });
    let driver = profile.runtime.resolve_driver(requested_engine);
    let Some((bin, prefix)) = driver.command_prefix() else {
        return Err(LeanDockError::RuntimeNotFound);
    };

    let mut args: Vec<String> = prefix.into_iter().map(|s| s.to_string()).collect();
    args.push("-f".to_string());
    args.push(compose_path.to_string_lossy().to_string());
    if override_path.exists() {
        args.push("-f".to_string());
        args.push(override_path.to_string_lossy().to_string());
    }
    args.push("down".to_string());

    let status = Command::new(bin).args(&args).status()?;
    if !status.success() {
        return Err(LeanDockError::CommandFailed {
            cmd: format!("{} {}", bin, args.join(" ")),
            code: status.code(),
            stderr: "compose down exited non-zero".to_string(),
        });
    }
    Ok(())
}

fn cmd_quadlet(cwd: &Path, file: Option<&Path>, output: &Path) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let profile = HostProfile::detect();
        let compose_path = discover_compose_file(file, cwd)?;
        let compose = ParsedCompose::from_file(&compose_path)?;
        let opts = TunerOptions {
            effective_ram_mb: profile.effective_ram_mb(),
            logical_cores: profile.hardware.logical_cores,
            swap_and_zram_mb: profile.hardware.total_swap_mb,
            ..Default::default()
        };
        let plan = build_optimization_plan(&compose, &opts);
        let report = crate::prerun::host_tuner::podman_quadlet::export_quadlet_units(
            &compose_path,
            &compose,
            &plan,
            output,
            profile.runtime.has_podlet,
        )?;

        println!(
            "{} Exported {} Quadlet unit(s) to {}",
            "✓".green(),
            report.generated_files.len(),
            report.output_dir.display()
        );
        for f in &report.generated_files {
            println!("  - {}", f.display());
        }
        for w in &report.warnings {
            println!("  {} {}", "WARNING:".yellow().bold(), w);
        }
        println!(
            "Copy units to `~/.config/containers/systemd/` and run `systemctl --user daemon-reload`."
        );
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (cwd, file, output);
        println!(
            "{}",
            "Quadlet export is a Linux systemd + Podman feature and is skipped cleanly on this OS."
                .yellow()
        );
        Ok(())
    }
}

fn cmd_status(format: ReportFormatArg) -> Result<()> {
    let profile = HostProfile::detect();
    let runtime_bin = match profile.runtime.active_runtime {
        ContainerRuntimeKind::Podman => "podman",
        _ => "docker",
    };
    let containers = try_collect_live_container_stats(runtime_bin)?;
    let kernel_stats = read_kernel_memory_stats();
    match format {
        ReportFormatArg::Table => print_status_report(&containers, &kernel_stats),
        ReportFormatArg::Json => {
            println!("{}", format_status_json_report(&containers, &kernel_stats)?)
        }
    }
    Ok(())
}

fn cmd_bench(
    cwd: &Path,
    file: Option<&Path>,
    format: BenchFormatArg,
    settle_secs: u64,
) -> Result<()> {
    let profile = HostProfile::detect();
    let compose_path = discover_compose_file(file, cwd)?;
    let project_dir = compose_path.parent().unwrap_or(cwd);
    let compose = ParsedCompose::from_file(&compose_path)?;
    let driver = profile.runtime.compose_driver.clone();
    if driver.command_prefix().is_none() {
        return Err(LeanDockError::RuntimeNotFound);
    }

    // 1. Measure baseline unmodified Compose stack
    println!(
        "{}",
        "Benchmark Phase 1/2: Measuring raw `docker compose up -d`...".cyan()
    );
    let t0 = Instant::now();
    invoke_compose_up(&driver, &compose_path, None)?;
    let raw_startup_ms = t0.elapsed().as_millis() as u64;
    std::thread::sleep(Duration::from_secs(settle_secs));
    let before = capture_benchmark_snapshot("Raw Compose", raw_startup_ms);

    // Tear down raw stack cleanly before optimized run.
    let _ = cmd_down(cwd, Some(&compose_path), None);

    // 2. Measure LeanDock-optimized Compose stack
    println!(
        "{}",
        "Benchmark Phase 2/2: Measuring `leandock up`...".cyan()
    );
    let opts = TunerOptions {
        effective_ram_mb: profile.effective_ram_mb(),
        logical_cores: profile.hardware.logical_cores,
        swap_and_zram_mb: profile.hardware.total_swap_mb,
        ..Default::default()
    };
    let plan = build_optimization_plan(&compose, &opts);
    let override_path = write_override_file(project_dir, &plan)?;

    let t1 = Instant::now();
    invoke_compose_up(&driver, &compose_path, Some(&override_path))?;
    let opt_startup_ms = t1.elapsed().as_millis() as u64;
    std::thread::sleep(Duration::from_secs(settle_secs));
    let after = capture_benchmark_snapshot("LeanDock Optimized", opt_startup_ms);

    let out_fmt = match format {
        BenchFormatArg::Table => OutputFormat::Table,
        BenchFormatArg::Markdown => OutputFormat::Markdown,
        BenchFormatArg::Json => OutputFormat::Json,
    };

    println!("\n{}", format_benchmark_report(before, after, out_fmt));
    Ok(())
}

fn cmd_revert(cwd: &Path, all: bool) -> Result<()> {
    let profile = HostProfile::detect();
    let backup_mgr = BackupManager::default_for_host(profile.platform.is_root);
    let actions = backup_mgr.revert(all)?;

    for act in actions {
        println!("  {} {}", "✓".green(), act);
    }

    let leandock_dir = cwd.join(".leandock");
    if leandock_dir.exists() {
        fs::remove_dir_all(&leandock_dir)?;
        println!(
            "  {} Removed isolated override directory {}",
            "✓".green(),
            leandock_dir.display()
        );
    }

    println!(
        "\n{}",
        "LeanDock revert complete. Host and workspace restored."
            .bold()
            .green()
    );
    Ok(())
}

fn invoke_compose_up(
    driver: &ComposeDriver,
    compose_file: &Path,
    override_file: Option<&Path>,
) -> Result<()> {
    let Some((bin, prefix)) = driver.command_prefix() else {
        return Err(LeanDockError::RuntimeNotFound);
    };

    let mut args: Vec<String> = prefix.into_iter().map(|s| s.to_string()).collect();
    args.push("-f".to_string());
    args.push(compose_file.to_string_lossy().to_string());
    if let Some(ov) = override_file {
        args.push("-f".to_string());
        args.push(ov.to_string_lossy().to_string());
    }
    args.push("up".to_string());
    args.push("-d".to_string());

    let out = Command::new(bin).args(&args).output()?;
    if !out.status.success() {
        return Err(LeanDockError::CommandFailed {
            cmd: format!("{} {}", bin, args.join(" ")),
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(())
}

fn cmd_install_proxy_service(
    profile: &HostProfile,
    cwd: &Path,
    port_maps: &[String],
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("leandock"));
        let mut exec_args = vec![exe.to_string_lossy().to_string(), "proxy".to_string()];
        for pm in port_maps {
            exec_args.push("--port-map".to_string());
            exec_args.push(pm.clone());
        }

        let unit_content = format!(
            "# Managed by LeanDock — Wake-on-Request Reverse Proxy Service\n\
[Unit]\n\
Description=LeanDock Wake-on-Request Reverse Proxy\n\
After=network-online.target docker.service\n\
Wants=network-online.target\n\n\
[Service]\n\
Type=simple\n\
WorkingDirectory={}\n\
ExecStart={}\n\
Restart=on-failure\n\
RestartSec=3\n\n\
[Install]\n\
WantedBy={}\n",
            cwd.display(),
            exec_args.join(" "),
            if profile.platform.is_root {
                "multi-user.target"
            } else {
                "default.target"
            }
        );

        let unit_path = if profile.platform.is_root {
            PathBuf::from("/etc/systemd/system/leandock-proxy.service")
        } else {
            crate::backup::user_home_dir().join(".config/systemd/user/leandock-proxy.service")
        };

        let backup_mgr = BackupManager::default_for_host(profile.platform.is_root);
        let mut session = backup_mgr.start_session()?;
        session.backup_and_write(
            &unit_path,
            unit_content.as_bytes(),
            "proxy-systemd-service",
            None,
        )?;

        println!(
            "  {} Installed systemd unit: {}",
            "✓".green(),
            unit_path.display()
        );
        if profile.platform.is_root {
            println!("  Enable & start: sudo systemctl daemon-reload && sudo systemctl enable --now leandock-proxy.service");
        } else {
            println!("  Enable & start: systemctl --user daemon-reload && systemctl --user enable --now leandock-proxy.service");
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = (profile, cwd, port_maps);
        println!(
            "{}",
            "`--install-service` generates a Linux systemd unit and is skipped cleanly on this OS. Run `leandock proxy` directly instead."
                .yellow()
        );
        Ok(())
    }
}

#[cfg(test)]
mod report_format_cli_tests {
    use super::*;

    fn extract(args: &[&str]) -> (Vec<OsString>, ReportFormatArg) {
        extract_report_format_args(args.iter().map(OsString::from).collect())
    }

    #[test]
    fn parses_json_format() {
        let (args, format) = extract(&["leandock", "analyze", "--format", "json"]);
        assert!(matches!(format, ReportFormatArg::Json));
        assert!(Cli::try_parse_from(args).is_ok());
    }

    #[test]
    fn parses_text_and_table_formats() {
        for value in ["text", "table"] {
            let (args, format) = extract(&["leandock", "status", "--format", value]);
            assert!(matches!(format, ReportFormatArg::Table));
            assert!(Cli::try_parse_from(args).is_ok());
        }
    }

    #[test]
    fn defaults_to_text_format() {
        let (args, format) = extract(&["leandock", "analyze"]);
        assert!(matches!(format, ReportFormatArg::Table));
        assert!(Cli::try_parse_from(args).is_ok());
    }

    #[test]
    fn preserves_public_commands_api() {
        let analyze = Cli::try_parse_from(["leandock", "analyze"]).unwrap();
        let status = Cli::try_parse_from(["leandock", "status"]).unwrap();

        assert!(matches!(analyze.command, Commands::Analyze { .. }));
        assert!(matches!(status.command, Commands::Status));
    }
}
