//! The checked-in conformance records must describe the corpus this tree ships.

use std::collections::BTreeSet;
use std::path::PathBuf;

use saffron_vegetation::{
    BIOME_NODE_VERSION, GRAPH_GPU_ABI_VERSION, GraphOperator, graph_gpu_abi_hash,
    qualification_corpus, qualification_corpus_hash, qualification_reference_hash,
};
use serde_json::Value;

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
        if !name.starts_with("compute-conformance-") || !name.ends_with(".json") {
            continue;
        }
        let bytes = std::fs::read(&path).expect("readable conformance record");
        records.push((
            name,
            serde_json::from_slice(&bytes).expect("conformance record JSON"),
        ));
    }
    records.sort_by(|left, right| left.0.cmp(&right.0));
    records
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn slang_executor_operators() -> Vec<&'static str> {
    GraphOperator::ALL
        .iter()
        .filter(|operator| operator.has_slang_executor())
        .map(|operator| operator.as_wire())
        .collect()
}

#[test]
fn every_conformance_record_binds_the_current_corpus_and_abi() {
    let records = records();
    assert!(
        !records.is_empty(),
        "no conformance record is checked in; run `just compute-conformance`"
    );
    let corpus = qualification_corpus();
    let invocations: usize = corpus
        .iter()
        .map(|batch| batch.invocation_batch.invocation_count())
        .sum();
    let reference = hex(&qualification_reference_hash());
    let operators = slang_executor_operators();

    for (name, record) in &records {
        assert_eq!(record["schemaVersion"], 1, "{name}");
        assert!(
            ["discrete-gpu", "integrated-gpu"]
                .contains(&record["profile"]["deviceType"].as_str().unwrap_or_default()),
            "{name} was not captured on a physical GPU"
        );

        let graph = &record["graphProgram"];
        assert_eq!(graph["abiVersion"], GRAPH_GPU_ABI_VERSION, "{name}");
        assert_eq!(graph["abiSha256"], hex(&graph_gpu_abi_hash()), "{name}");
        assert_eq!(graph["corpusProgramCount"], corpus.len(), "{name}");
        assert_eq!(graph["corpusInvocationCount"], invocations, "{name}");
        assert_eq!(
            graph["corpusSha256"],
            hex(&qualification_corpus_hash()),
            "{name} qualified a corpus this tree no longer defines"
        );
        assert_eq!(graph["rustReferenceSha256"], reference.as_str(), "{name}");
        assert_eq!(
            graph["slangSha256"], graph["rustReferenceSha256"],
            "{name} recorded a Slang result that differs from Rust"
        );
        assert_eq!(
            graph["qualifiedOperators"]
                .as_array()
                .expect("qualified operator array")
                .iter()
                .map(|operator| operator["operator"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            operators,
            "{name}"
        );
        assert!(
            graph["qualifiedOperators"]
                .as_array()
                .expect("qualified operator array")
                .iter()
                .all(|operator| operator["semanticVersion"] == BIOME_NODE_VERSION),
            "{name}"
        );

        let spatial = &record["spatialNumeric"];
        assert_eq!(
            spatial["slangSha256"], spatial["rustReferenceSha256"],
            "{name} recorded a spatial-numeric result that differs from Rust"
        );
        assert_eq!(record["validation"]["newIssues"], 0, "{name}");
    }
}

/// A cross-platform byte-equality claim only means something when every record compiled the same
/// declared source closure with the same flags. Compiler build and emitted SPIR-V may differ per
/// platform; the compile input and the results may not.
#[test]
fn conformance_records_compare_one_shader_input_across_platforms() {
    let records = records();
    for shader in ["spatialNumeric", "graphProgram"] {
        for field in ["sourceFiles", "spirvFlags", "defines", "compileInputSha256"] {
            let distinct = records
                .iter()
                .map(|(_, record)| record[shader]["shaderArtifact"][field].to_string())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                distinct.len(),
                1,
                "records disagree on {shader}.{field}: {distinct:?}"
            );
        }
        let results = records
            .iter()
            .map(|(_, record)| record[shader]["rustReferenceSha256"].to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(results.len(), 1, "records disagree on {shader} results");
    }
    let devices = records
        .iter()
        .map(|(_, record)| record["profile"]["deviceUuid"].to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        devices.len(),
        records.len(),
        "two records describe the same physical device"
    );
}

/// MoltenVK is hardware this checkout cannot reach. Run `just compute-conformance` on an Apple
/// device and check the record in; the currency test then holds it to this corpus.
#[test]
#[ignore = "needs Apple hardware: run `just compute-conformance` there and check the record in"]
fn a_moltenvk_conformance_record_is_checked_in() {
    assert!(
        records()
            .iter()
            .any(|(_, record)| record["profile"]["moltenVk"] == Value::Bool(true)),
        "no MoltenVK conformance record is checked in"
    );
}
