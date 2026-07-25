// GPU harness for the persistent-scene address-block contract: resolves the
// instance -> prototype -> material-reference chain through buffer device addresses and
// writes every resolved word for the Rust fixture to compare byte-exactly.
import global_gpu_data;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> output;
[[vk::binding(1, 0)]] StructuredBuffer<GpuSceneAddressBlock> addressBlocks;

[shader("compute")]
[numthreads(1, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = addressBlocks[0];

    let instanceHeader = gpuSceneSlotHeader(addresses.instances, GPU_SCENE_INSTANCE_SLOT_STRIDE, 0u);
    output[0] = instanceHeader.generation;
    output[1] = instanceHeader.occupied;

    let instance = gpuSceneLoadInstance(addresses, 0u);
    output[2] = instance.prototype.index;
    output[3] = instance.prototype.generation;
    output[4] = instance.transformKind;
    output[5] = asuint(instance.transform[0].x);
    output[6] = asuint(instance.transform[0].y);
    output[7] = asuint(instance.transform[0].z);
    output[8] = asuint(instance.transform[7].w);
    output[9] = instance.materialOverrides.count;

    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);
    output[10] = prototype.geometry.index;
    output[11] = prototype.geometry.generation;
    output[12] = asuint(prototype.bounds.w);
    output[13] = prototype.materialRange.count;

    let materialHandle = gpuScenePrototypeMaterialHandle(addresses, prototype.materialRange.first);
    output[14] = materialHandle.index;
    output[15] = materialHandle.generation;

    let material = gpuSceneLoadMaterialReference(addresses, materialHandle.index);
    output[16] = material.target.index;
    output[17] = material.target.generation;
    output[18] = material.sourceRevision.x;
    output[19] = material.sourceRevision.y;

    let overrideElement = gpuSceneOverrideElement(addresses, instance.materialOverrides.first);
    output[20] = overrideElement.slot;
    output[21] = overrideElement.material.index;

    let light = gpuSceneLoadLight(addresses, 0u);
    output[22] = asuint(light.colorIntensity.w);
    output[23] = light.sourceRevision.x;
}
