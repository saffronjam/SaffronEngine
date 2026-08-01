//! The checked-in phase-1 baseline records must derive every ceiling from their own measurements.
//!
//! `tools/bench-foliage-phase1/check.ts` grades a live run against the record whose device it is
//! running on, so a record is an acceptance threshold, not a note. A threshold that no longer
//! follows from the observations beside it, or that describes a device other than the one its
//! filename names, would grade the wrong machine against the wrong number.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde_json::Value;

/// The derivation every record declares and every threshold in it must follow.
const DERIVATION: &str =
    "Anima steady-state p95 plus 25% or an absolute noise floor, whichever is larger";

/// Each graded p95 leg: the observation, the threshold derived from it, and the absolute noise
/// floor its ceiling carries. Only p95 is gateable — a record's own p99 sits an order of magnitude
/// above its p95 because the capture window includes pipeline compilation.
const TIMING_LEGS: [(&str, &str, f64); 3] = [
    ("sceneGatherMs", "sceneGatherP95Ms", 0.05),
    ("cpuFrameMs", "cpuFrameP95Ms", 0.25),
    ("gpuFrameMs", "gpuFrameP95Ms", 0.25),
];

/// Each graded counter leg: the observation and the threshold that is exactly it.
const COUNTER_LEGS: [(&str, &str); 3] = [
    ("drawCalls", "drawCallsMax"),
    ("instanceUploadBytes", "instanceUploadBytesMax"),
    ("retainedMeshCpuBytes", "retainedMeshCpuBytesMax"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
}

/// What a record may be compared against: the vendor of the physical device plus whether Vulkan
/// reaches it through MoltenVK. A result from one class is never another class's threshold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DeviceClass {
    vendor: Vendor,
    molten_vk: bool,
}

fn records() -> Vec<(String, Value)> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../benchmarks/foliage-veg")
        .canonicalize()
        .expect("the benchmark directory");
    let mut records = Vec::new();
    for entry in std::fs::read_dir(&directory).expect("readable benchmark directory") {
        let path = entry.expect("benchmark directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        if !name.starts_with("phase-1-") || !name.ends_with(".json") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("readable baseline record");
        records.push((
            name,
            serde_json::from_slice(&bytes).expect("baseline record JSON"),
        ));
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    records
}

fn number(value: &Value, path: &str) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("{path} is a number"))
}

fn text<'a>(value: &'a Value, path: &str) -> &'a str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{path} is a string"))
}

/// `max(value * 1.25, value + headroom)`, quantized to three decimals — the one derivation behind
/// every recorded ceiling, restated here so a hand-edited threshold cannot pass as derived.
fn ceiling(value: f64, absolute_headroom: f64) -> f64 {
    ((value * 1.25).max(value + absolute_headroom) * 1000.0).round() / 1000.0
}

fn vendor_of(token: &str) -> Option<Vendor> {
    match token {
        "nvidia" => Some(Vendor::Nvidia),
        "amd" | "radeon" => Some(Vendor::Amd),
        "intel" => Some(Vendor::Intel),
        "apple" => Some(Vendor::Apple),
        _ => None,
    }
}

fn tokens(text: &str) -> Vec<String> {
    text.to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn class_from_platform(platform: &Value, name: &str) -> DeviceClass {
    let gpu = text(&platform["gpu"], "platform.gpu");
    let vendor = tokens(gpu)
        .iter()
        .find_map(|token| vendor_of(token))
        .unwrap_or_else(|| panic!("{name}: '{gpu}' names no known GPU vendor"));
    DeviceClass {
        vendor,
        molten_vk: text(&platform["os"], "platform.os") == "darwin",
    }
}

fn class_from_file_name(name: &str) -> DeviceClass {
    let slug = name
        .strip_prefix("phase-1-")
        .and_then(|slug| slug.strip_suffix(".json"))
        .unwrap_or_else(|| panic!("{name} is not a phase-1 record filename"));
    let parts = tokens(slug);
    let vendor = parts
        .first()
        .and_then(|token| vendor_of(token))
        .unwrap_or_else(|| panic!("{name} does not open with a device vendor"));
    DeviceClass {
        vendor,
        molten_vk: parts.iter().any(|token| token == "moltenvk"),
    }
}

/// The filename's model tokens in order within the device name, so a record cannot be filed under
/// one device while carrying another's measurements.
fn names_the_device(name: &str, gpu: &str) -> bool {
    let slug = name
        .strip_prefix("phase-1-")
        .and_then(|slug| slug.strip_suffix(".json"))
        .unwrap_or_default();
    let mut device = tokens(gpu).into_iter();
    tokens(slug)
        .into_iter()
        .filter(|token| token != "moltenvk")
        .all(|wanted| device.any(|token| token == wanted))
}

#[test]
fn every_baseline_record_derives_its_budgets_from_its_own_observations() {
    let records = records();
    assert!(
        !records.is_empty(),
        "no phase-1 baseline record is checked in; run `just bench-foliage-phase1`"
    );

    let expected_keys: BTreeSet<&str> = ["derivation"]
        .into_iter()
        .chain(TIMING_LEGS.iter().map(|(_, threshold, _)| *threshold))
        .chain(COUNTER_LEGS.iter().map(|(_, threshold)| *threshold))
        .collect();

    for (name, record) in &records {
        assert_eq!(record["schemaVersion"], 1, "{name}");
        assert_eq!(
            record["fixture"]["name"], "phase-1-heterogeneous-mesh-baseline",
            "{name}"
        );
        assert_eq!(
            record["validationErrors"].as_array().map(Vec::len),
            Some(0),
            "{name} was captured through validation errors"
        );

        let budgets = record["budgets"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} has no budgets block"));
        assert_eq!(
            budgets.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            expected_keys,
            "{name} publishes a threshold set the gate does not grade"
        );
        assert_eq!(budgets["derivation"], DERIVATION, "{name}");

        let observed = &record["observed"];
        for (leg, threshold, headroom) in TIMING_LEGS {
            let distribution = &observed[leg];
            let p50 = number(&distribution["p50"], leg);
            let p95 = number(&distribution["p95"], leg);
            let p99 = number(&distribution["p99"], leg);
            let max = number(&distribution["max"], leg);
            assert!(
                p50 <= p95 && p95 <= p99 && p99 <= max,
                "{name}: observed.{leg} is not an ordered distribution"
            );
            let expected = ceiling(p95, headroom);
            let recorded = number(&budgets[threshold], threshold);
            assert!(
                (recorded - expected).abs() < 1e-9,
                "{name}: budgets.{threshold} is {recorded}, but {leg}.p95 of {p95} derives {expected}"
            );
        }
        for (leg, threshold) in COUNTER_LEGS {
            assert_eq!(
                budgets[threshold], observed[leg],
                "{name}: budgets.{threshold} is not observed.{leg}"
            );
        }
    }
}

/// A zero in a capability column is an absence, not a measurement. An absent capability may not
/// reach the gate as a ceiling of zero, and it may not be reported half-present either.
#[test]
fn every_baseline_record_reports_absent_capabilities_as_absent() {
    for (name, record) in &records() {
        let observed = &record["observed"];
        let platform = &record["platform"];

        if platform["rtSupported"] == Value::Bool(false) {
            assert_eq!(
                observed["rtInstances"], 0,
                "{name} claims no ray tracing yet records TLAS instances"
            );
        } else {
            assert!(
                number(&observed["rtInstances"], "observed.rtInstances") > 0.0,
                "{name} has ray tracing available but exercised none of it"
            );
        }

        let usage = number(&observed["vramUsageBytes"], "observed.vramUsageBytes");
        let budget = number(&observed["vramBudgetBytes"], "observed.vramBudgetBytes");
        if budget == 0.0 {
            assert_eq!(
                usage, 0.0,
                "{name} reports device memory in use against no budget"
            );
        } else {
            assert!(
                usage > 0.0 && usage <= budget,
                "{name} reports {usage} bytes of device memory against a budget of {budget}"
            );
        }
    }
}

/// Every record names the device it measured, and the measurement was taken on that device.
#[test]
fn every_baseline_record_matches_the_device_its_filename_names() {
    for (name, record) in &records() {
        let platform = &record["platform"];
        assert_eq!(
            platform["softwareGpu"],
            Value::Bool(false),
            "{name} was captured on the software rasterizer"
        );
        assert_eq!(
            platform["timestampsSupported"],
            Value::Bool(true),
            "{name} carries frame times from a device that serves no timestamps"
        );
        assert_eq!(platform["profilerMode"], "timestamps", "{name}");
        assert_eq!(
            class_from_platform(platform, name),
            class_from_file_name(name),
            "{name} is filed under a device class its platform block contradicts"
        );
        let gpu = text(&platform["gpu"], "platform.gpu");
        assert!(names_the_device(name, gpu), "{name} does not name '{gpu}'");
    }
}

/// One record per device, one fixture across all of them, and no timing threshold shared between
/// two devices — a ceiling measured on one class is never relabelled as another class's.
#[test]
fn baseline_records_grade_one_fixture_and_never_share_a_threshold() {
    let records = records();
    let fixtures = records
        .iter()
        .map(|(_, record)| record["fixture"].to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fixtures.len(),
        1,
        "records measure different fixtures: {fixtures:?}"
    );

    let devices = records
        .iter()
        .map(|(_, record)| {
            format!(
                "{}/{}",
                text(&record["platform"]["os"], "platform.os"),
                text(&record["platform"]["gpu"], "platform.gpu").to_ascii_lowercase()
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        devices.len(),
        records.len(),
        "two records claim the same device, so the gate cannot tell whose ceiling to grade"
    );

    for (_, threshold, _) in TIMING_LEGS {
        let mut owners: BTreeMap<String, &str> = BTreeMap::new();
        for (name, record) in &records {
            let value = number(&record["budgets"][threshold], threshold).to_string();
            if let Some(previous) = owners.insert(value.clone(), name) {
                panic!("{name} and {previous} publish the same {threshold} of {value}");
            }
        }
    }
}
