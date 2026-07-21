//! Vulkan execution gate for the resident vegetation graph-program corpus.

use std::sync::Arc;
use std::time::{Duration, Instant};

use saffron_vegetation::{
    BIOME_NODE_VERSION, GraphCancellationToken, GraphComputeExecutor, GraphGpuInvocationBatch,
    GraphOperator, evaluate_gpu_program_reference, qualification_corpus,
};

use crate::{Device, SurfaceSource, VulkanGraphComputeExecutor, validation_issue_count};

#[test]
fn every_resident_program_matches_rust_and_is_qualified_on_the_exact_artifact() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => Arc::new(device),
        Err(error) => {
            eprintln!("skipping: no Vulkan device obtainable ({error})");
            return;
        }
    };
    let before = validation_issue_count();
    let executor = VulkanGraphComputeExecutor::new(Arc::clone(&device))
        .expect("resident vegetation graph qualification");
    let artifact_identity = executor.shader_artifact_identity();
    assert_eq!(artifact_identity.shader(), "vegetation_graph");
    assert_eq!(artifact_identity.source(), "vegetation_graph.slang");
    assert_eq!(artifact_identity.artifact(), "vegetation_graph.spv");
    assert_eq!(
        artifact_identity
            .source_files()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["spatial_numeric.slang", "vegetation_graph.slang"]
    );
    assert!(artifact_identity.defines().is_empty());
    assert_ne!(artifact_identity.record_sha256().bytes(), [0; 32]);

    for batch in qualification_corpus() {
        let expected = evaluate_gpu_program_reference(&batch.program, &batch.invocation_batch)
            .expect("canonical resident graph reference execution");
        let actual = executor
            .execute_program(
                &batch.program,
                &batch.invocation_batch,
                &GraphCancellationToken::default(),
                Instant::now() + Duration::from_secs(30),
            )
            .expect("resident graph-program GPU dispatch");
        assert_eq!(actual, expected);
    }
    for operator in GraphOperator::ALL
        .iter()
        .copied()
        .filter(|operator| operator.has_slang_executor())
    {
        assert!(executor.qualifications().contains(
            operator,
            BIOME_NODE_VERSION,
            executor.profile()
        ));
    }
    drop(executor);
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(validation_issue_count(), before);
}

#[test]
fn resident_program_chunks_invocations_and_preserves_order() {
    let device = match Device::new(&SurfaceSource::Offscreen) {
        Ok(device) => Arc::new(device),
        Err(error) => {
            eprintln!("skipping: no Vulkan device obtainable ({error})");
            return;
        }
    };
    let before = validation_issue_count();
    let executor = VulkanGraphComputeExecutor::new(Arc::clone(&device))
        .expect("resident vegetation graph qualification");
    let batch = qualification_corpus().into_iter().next().unwrap();
    let mut invocation_batch = GraphGpuInvocationBatch::with_capacity(&batch.program, 65_537)
        .expect("flat chunking test batch");
    for invocation in batch.invocation_batch.invocations().cycle().take(65_537) {
        invocation_batch
            .push(invocation.iter().copied().map(Ok))
            .expect("valid repeated invocation");
    }
    let expected = evaluate_gpu_program_reference(&batch.program, &invocation_batch)
        .expect("chunked resident graph reference execution");
    let actual = executor
        .execute_program(
            &batch.program,
            &invocation_batch,
            &GraphCancellationToken::default(),
            Instant::now() + Duration::from_secs(30),
        )
        .expect("chunked resident graph-program GPU dispatch");
    assert_eq!(actual, expected);

    let cancellation = GraphCancellationToken::default();
    cancellation.cancel();
    assert!(matches!(
        executor.execute_program(
            &batch.program,
            &batch.invocation_batch,
            &cancellation,
            Instant::now() + Duration::from_secs(30)
        ),
        Err(saffron_vegetation::Error::GraphCancelled)
    ));
    assert!(matches!(
        executor.execute_program(
            &batch.program,
            &batch.invocation_batch,
            &GraphCancellationToken::default(),
            Instant::now() - Duration::from_millis(1),
        ),
        Err(saffron_vegetation::Error::GraphLimit {
            resource: "time milliseconds",
            ..
        })
    ));

    drop(executor);
    device.wait_idle().expect("idle before teardown");
    drop(device);
    assert_eq!(validation_issue_count(), before);
}
