use leandock::postrun::KernelMemoryStats;
use leandock::prerun::db_tuner::OptimizationPlan;
use leandock::profiler::HostProfile;
use leandock::report::{
    format_analyze_json_report, format_status_json_report, format_status_report, LiveContainerStat,
};

#[test]
fn analyze_json_report_contains_expected_sections() {
    let profile = HostProfile::detect();

    let plan = OptimizationPlan {
        effective_ram_mb: 4096,
        os_reserve_mb: 614,
        allocatable_ram_mb: 3482,
        total_budgeted_mb: 0,
        estimated_default_ram_mb: 0,
        estimated_savings_mb: 0,
        services: Vec::new(),
        warnings: Vec::new(),
    };

    let output = format_analyze_json_report(&profile, &plan, None).unwrap();
    let json: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert!(json.get("host").is_some());
    assert!(json.get("optimization_plan").is_some());
    assert!(json.get("host_preview").is_some());
}

#[test]
fn status_json_report_contains_expected_sections() {
    let containers = vec![LiveContainerStat {
        name: "test-container".to_string(),
        state: "running".to_string(),
        cpu_percent: "2.5%".to_string(),
        mem_usage: "128MiB / 1GiB".to_string(),
        idle_mode: "active".to_string(),
    }];

    let kernel_stats = KernelMemoryStats::default();

    let output = format_status_json_report(&containers, &kernel_stats).unwrap();
    let json: serde_json::Value = serde_json::from_str(&output).unwrap();

    assert!(json.get("containers").is_some());
    assert!(json.get("kernel_memory").is_some());
    assert_eq!(json["containers"][0]["name"], "test-container");
    assert_eq!(json["containers"][0]["cpu_percent_value"], 2.5);
    assert_eq!(json["containers"][0]["memory_usage_bytes"], 134217728);
    assert_eq!(json["containers"][0]["memory_limit_bytes"], 1073741824);
}

#[test]
fn status_table_report_contains_headers_and_container_data() {
    let containers = vec![LiveContainerStat {
        name: "test-container".to_string(),
        state: "running".to_string(),
        cpu_percent: "2.5%".to_string(),
        mem_usage: "128MiB / 1GiB".to_string(),
        idle_mode: "Wake Proxy: Active".to_string(),
    }];

    let output = format_status_report(&containers, &KernelMemoryStats::default());

    assert!(output.contains("LeanDock Live Stack Status"));
    assert!(output.contains("Container"));
    assert!(output.contains("State"));
    assert!(output.contains("CPU %"));
    assert!(output.contains("RAM Usage"));
    assert!(output.contains("Wake-on-Request"));
    assert!(output.contains("test-container"));
    assert!(output.contains("running"));
    assert!(output.contains("2.5%"));
    assert!(output.contains("128MiB / 1GiB"));
    assert!(output.contains("Wake Proxy: Active"));
    assert!(output.contains("Kernel Memory Optimizations"));
}
