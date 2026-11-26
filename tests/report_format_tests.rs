use assert_cmd::Command;
use leandock::postrun::KernelMemoryStats;
use leandock::prerun::compose_parser::ParsedCompose;
use leandock::prerun::db_tuner::{build_optimization_plan, TunerOptions};
use leandock::prerun::host_tuner::HostTuningReport;
use leandock::profiler::platform::{
    parse_kernel_major_minor, parse_systemd_version, HostOs, PlatformFacts, PlatformProfile,
    VirtualizationKind,
};
use leandock::profiler::{
    ComposeDriver, ContainerRuntimeKind, HardwareInfo, HostProfile, RuntimeInfo,
};
use leandock::report::{
    format_analyze_json, format_status_json, parse_cpu_percent, parse_mem_usage_mb, parse_size_mb,
    LiveContainerStat,
};
use serde_json::Value;

const COMPOSE_YAML: &str = r#"
services:
  db:
    image: postgres:16
  cache:
    image: redis:7
  legacy:
    image: mongo:7
    labels:
      leandock.ignore: "true"
  api:
    image: python:3.12-slim
"#;

fn lxc_non_root_profile() -> HostProfile {
    HostProfile {
        hardware: HardwareInfo {
            cpu_arch: "x86_64".into(),
            logical_cores: 2,
            total_ram_mb: 2048,
            available_ram_mb: 1500,
            total_swap_mb: 0,
            used_swap_mb: 0,
            free_disk_gb: 20.0,
            total_disk_gb: 40.0,
        },
        platform: PlatformProfile::from_facts(PlatformFacts {
            os: HostOs::Linux,
            distro: "Debian GNU/Linux 12".into(),
            kernel_version: Some("6.1.0-18-amd64".into()),
            kernel_major_minor: parse_kernel_major_minor("6.1.0-18-amd64"),
            systemd_version: parse_systemd_version("systemd 252 (252.22-1~deb12u1)"),
            virtualization: VirtualizationKind::Lxc,
            is_root: false,
            can_sudo_noninteractive: false,
            ksm_sysfs_writable: false,
        }),
        runtime: RuntimeInfo {
            active_runtime: ContainerRuntimeKind::Docker,
            compose_driver: ComposeDriver::DockerComposeV2,
            docker_version: Some("Docker version 27.0.0".into()),
            podman_version: None,
            socket_path: None,
            macos_vm_provider: None,
            vm_memory_limit_mb: None,
            has_podlet: false,
            has_slim: false,
        },
    }
}

#[test]
fn analyze_json_includes_tier_skipped_tiers_and_service_limits() {
    let profile = lxc_non_root_profile();
    let compose = ParsedCompose::from_yaml_str(COMPOSE_YAML, None).expect("parse compose");
    let plan = build_optimization_plan(
        &compose,
        &TunerOptions {
            effective_ram_mb: profile.effective_ram_mb(),
            logical_cores: profile.hardware.logical_cores,
            swap_and_zram_mb: profile.hardware.total_swap_mb,
            ..Default::default()
        },
    );
    let preview = HostTuningReport {
        applied: Vec::new(),
        skipped: vec!["non-root: daemon.json tuning skipped".into()],
        warnings: Vec::new(),
    };

    let json: Value = serde_json::from_str(
        &format_analyze_json(&profile, &plan, Some(&preview), Vec::new()).unwrap(),
    )
    .expect("analyze output must be valid JSON");

    assert_eq!(json["host"]["os"], "linux");
    assert_eq!(json["host"]["virtualization"], "lxc");
    assert_eq!(json["host"]["is_root"], false);

    // Non-root LXC: tiers 1 & 2 full, tier 3 partial, tier 4 auto-skipped.
    assert_eq!(json["detected_tier"], 3);
    let tiers = json["tiers"].as_array().expect("tiers array");
    assert_eq!(tiers.len(), 4);
    let skipped = json["skipped_tiers"]
        .as_array()
        .expect("skipped_tiers array");
    let skipped_ids: Vec<u64> = skipped
        .iter()
        .map(|t| t["tier"].as_u64().unwrap())
        .collect();
    assert_eq!(skipped_ids, vec![4]);
    assert_eq!(tiers[2]["status"], "partial");
    for t in skipped {
        assert_eq!(t["status"], "skipped");
        assert!(
            !t["details"].as_str().unwrap_or_default().is_empty(),
            "skipped tier must carry a reason: {t}"
        );
    }

    let services = json["services"].as_array().expect("services array");
    assert_eq!(services.len(), plan.services.len());
    for svc in &plan.services {
        let row = services
            .iter()
            .find(|s| s["name"] == svc.name.as_str())
            .unwrap_or_else(|| panic!("service `{}` missing from JSON", svc.name));
        assert_eq!(row["ignored"], svc.ignored);
        assert_eq!(row["mem_limit_mb"], serde_json::json!(svc.mem_limit_mb));
        assert_eq!(
            row["mem_reservation_mb"],
            serde_json::json!(svc.mem_reservation_mb)
        );
    }
    let db = services.iter().find(|s| s["name"] == "db").unwrap();
    assert!(db["mem_limit_mb"].as_u64().unwrap() > 0);
    let legacy = services.iter().find(|s| s["name"] == "legacy").unwrap();
    assert_eq!(legacy["ignored"], true);

    assert_eq!(json["budget"]["effective_ram_mb"], plan.effective_ram_mb);
    assert_eq!(
        json["budget"]["estimated_savings_mb"],
        plan.estimated_savings_mb
    );
    assert_eq!(
        json["host_preview"]["skipped"][0],
        "non-root: daemon.json tuning skipped"
    );
    assert_eq!(json["errors"], serde_json::json!([]));
}

#[test]
fn analyze_json_never_contains_commands_or_env_values() {
    let profile = lxc_non_root_profile();
    let compose = ParsedCompose::from_yaml_str(COMPOSE_YAML, None).expect("parse compose");
    let mut plan = build_optimization_plan(
        &compose,
        &TunerOptions {
            effective_ram_mb: profile.effective_ram_mb(),
            logical_cores: profile.hardware.logical_cores,
            ..Default::default()
        },
    );
    let cache = plan
        .services
        .iter_mut()
        .find(|s| s.name == "cache")
        .unwrap();
    cache.tuned_command = Some(vec![
        "redis-server".into(),
        "--requirepass".into(),
        "s3cr3t-pass".into(),
    ]);
    cache
        .extra_env
        .insert("REDIS_PASSWORD".into(), "s3cr3t-env".into());

    let out = format_analyze_json(&profile, &plan, None, Vec::new()).unwrap();
    assert!(
        !out.contains("s3cr3t"),
        "credentials leaked into JSON: {out}"
    );
    assert!(!out.contains("requirepass"));

    let json: Value = serde_json::from_str(&out).unwrap();
    let row = json["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "cache")
        .unwrap();
    assert_eq!(row["tunes_command"], true);
    assert_eq!(row["extra_env_keys"], serde_json::json!(["REDIS_PASSWORD"]));
    assert!(row.get("tuned_command").is_none());
    assert!(row.get("extra_env").is_none());
}

#[test]
fn analyze_json_reports_collection_errors() {
    let profile = lxc_non_root_profile();
    let compose = ParsedCompose::from_yaml_str(COMPOSE_YAML, None).expect("parse compose");
    let plan = build_optimization_plan(&compose, &TunerOptions::default());
    let json: Value = serde_json::from_str(
        &format_analyze_json(
            &profile,
            &plan,
            None,
            vec!["host tuning preview unavailable: boom".into()],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(json["host_preview"], Value::Null);
    assert_eq!(
        json["errors"],
        serde_json::json!(["host tuning preview unavailable: boom"])
    );
}

#[test]
fn status_json_includes_containers_kernel_stats_and_wake_states() {
    let containers = vec![
        LiveContainerStat {
            name: "api".into(),
            state: "running".into(),
            cpu_percent: "1.25%".into(),
            mem_usage: "48MiB / 256MiB".into(),
            idle_mode: "Wake Proxy: Active".into(),
        },
        LiveContainerStat {
            name: "admin".into(),
            state: "exited".into(),
            cpu_percent: "0.00%".into(),
            mem_usage: "0B / 0B".into(),
            idle_mode: "Wake Proxy: Sleeping (0 MB)".into(),
        },
    ];
    let kernel = KernelMemoryStats {
        zram_active: true,
        zram_orig_data_mb: 300.0,
        zram_compr_data_mb: 100.0,
        zram_ratio: 3.0,
        ksm_active: true,
        ksm_pages_sharing: 2560,
        ksm_saved_mb: 10.0,
    };

    let json: Value =
        serde_json::from_str(&format_status_json(&containers, &kernel, Vec::new()).unwrap())
            .expect("status output must be valid JSON");

    let rows = json["containers"].as_array().expect("containers array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "api");
    assert_eq!(rows[0]["cpu_percent"], 1.25);
    assert_eq!(rows[0]["mem_used_mb"], 48.0);
    assert_eq!(rows[0]["mem_limit_mb"], 256.0);
    assert_eq!(rows[0]["cpu_text"], "1.25%");
    assert_eq!(rows[0]["mem_text"], "48MiB / 256MiB");
    assert_eq!(rows[1]["mem_used_mb"], 0.0);
    assert_eq!(rows[0]["idle_mode"], "Wake Proxy: Active");
    assert_eq!(rows[1]["idle_mode"], "Wake Proxy: Sleeping (0 MB)");

    assert_eq!(json["kernel"]["zram_active"], true);
    assert_eq!(json["kernel"]["zram_ratio"], 3.0);
    assert_eq!(json["kernel"]["ksm_active"], true);
    assert_eq!(json["kernel"]["ksm_pages_sharing"], 2560);
    assert_eq!(json["kernel"]["ksm_saved_mb"], 10.0);
    assert_eq!(json["errors"], serde_json::json!([]));
}

#[test]
fn status_json_reports_runtime_errors() {
    let json: Value = serde_json::from_str(
        &format_status_json(
            &[],
            &KernelMemoryStats::default(),
            vec!["container stats unavailable: docker not found".into()],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(json["containers"], serde_json::json!([]));
    assert_eq!(json["errors"].as_array().unwrap().len(), 1);
}

#[test]
fn parses_docker_stats_numbers() {
    assert_eq!(parse_cpu_percent("1.25%"), Some(1.25));
    assert_eq!(parse_cpu_percent(" 0.00% "), Some(0.0));
    assert_eq!(parse_cpu_percent("--"), None);
    assert_eq!(parse_size_mb("48MiB"), Some(48.0));
    assert_eq!(parse_size_mb("1.5GiB"), Some(1536.0));
    assert_eq!(parse_size_mb("512KiB"), Some(0.5));
    assert_eq!(parse_size_mb("0B"), Some(0.0));
    assert_eq!(parse_size_mb("2MB"), Some(2e6 / 1048576.0));
    assert_eq!(parse_size_mb("12parsecs"), None);
    assert_eq!(
        parse_mem_usage_mb("48MiB / 256MiB"),
        (Some(48.0), Some(256.0))
    );
    assert_eq!(parse_mem_usage_mb("--"), (None, None));
}

#[test]
fn status_json_with_no_containers_is_still_valid() {
    let json: Value = serde_json::from_str(
        &format_status_json(&[], &KernelMemoryStats::default(), Vec::new()).unwrap(),
    )
    .unwrap();
    assert_eq!(json["containers"], serde_json::json!([]));
    assert_eq!(json["kernel"]["zram_active"], false);
    assert_eq!(json["kernel"]["ksm_active"], false);
}

fn write_compose_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("docker-compose.yml"), COMPOSE_YAML).expect("write compose");
    dir
}

#[test]
fn cli_analyze_format_json_prints_only_json() {
    let dir = write_compose_fixture();
    let out = Command::cargo_bin("leandock")
        .unwrap()
        .current_dir(dir.path())
        .args(["analyze", "--format", "json"])
        .output()
        .expect("run leandock analyze");
    assert!(out.status.success(), "{out:?}");

    let json: Value = serde_json::from_slice(&out.stdout).expect("stdout must be pure JSON");
    assert!(json["detected_tier"].as_u64().unwrap() >= 1);
    assert!(json["skipped_tiers"].is_array());
    assert_eq!(json["services"].as_array().unwrap().len(), 4);
    assert!(
        !dir.path().join(".leandock").exists(),
        "analyze must stay zero-touch"
    );
}

#[test]
fn cli_analyze_default_text_output_is_unchanged() {
    let dir = write_compose_fixture();
    let out = Command::cargo_bin("leandock")
        .unwrap()
        .current_dir(dir.path())
        .arg("analyze")
        .output()
        .expect("run leandock analyze");
    assert!(out.status.success(), "{out:?}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("LeanDock Pre-Flight Analysis"));
    assert!(stdout.contains("Platform Capability Tiers"));
    assert!(stdout.contains("Compose & DB Auto-Tuner + Log Cap + Cleanup"));
    assert!(stdout.contains("Linux Kernel (zram, KSM, Quadlet)"));
    assert!(stdout.contains("Proposed Service Optimizations"));
    assert!(serde_json::from_str::<Value>(&stdout).is_err());
}

#[test]
fn cli_status_format_json_prints_only_json() {
    let out = Command::cargo_bin("leandock")
        .unwrap()
        .args(["status", "--format", "json"])
        .output()
        .expect("run leandock status");
    assert!(out.status.success(), "{out:?}");

    let json: Value = serde_json::from_slice(&out.stdout).expect("stdout must be pure JSON");
    assert!(json["containers"].is_array());
    assert!(json["kernel"]["zram_active"].is_boolean());
    assert!(json["kernel"]["ksm_active"].is_boolean());
    assert!(json["errors"].is_array());
}

#[test]
fn cli_status_default_text_output_is_unchanged() {
    let out = Command::cargo_bin("leandock")
        .unwrap()
        .arg("status")
        .output()
        .expect("run leandock status");
    assert!(out.status.success(), "{out:?}");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("LeanDock Live Stack Status"));
    assert!(stdout.contains("Container"));
    assert!(stdout.contains("Wake-on-Request"));
    assert!(serde_json::from_str::<Value>(&stdout).is_err());
}

#[test]
fn cli_rejects_unknown_format() {
    Command::cargo_bin("leandock")
        .unwrap()
        .args(["status", "--format", "yaml"])
        .assert()
        .failure();
}
