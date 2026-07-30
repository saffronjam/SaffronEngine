// Hierarchy traversal for the persistent GPU scene: one thread per visible instance
// walks the prototype's page hierarchy from its guaranteed root. A node refines into its
// children when its projected appearance error exceeds the threshold AND every child
// page's payload is resident; a missing child appends a page request and keeps the
// resident parent drawable — the cut can never hole. Nodes on the cut emit one semantic
// GpuDrawRecord per triangle cluster (or one for the voxel brick), with the material
// resolved through the instance's sparse overrides and the prototype defaults.
//
// Representation transitions: a cross-frame state table keyed on (instance slot, page)
// remembers each flip node — a node that moved between drawing itself and descending.
// A flip crossfades over GPU_TRANSITION_FRAMES frames: both representations draw, each
// carrying a transition word whose stochastic coverage partitions every pixel between
// them, keyed on the flip node so the shares stay exactly complementary. The state
// advances once per frame (stamp-guarded, so the survivor pass never double-steps).
//
// A demand-only walk (`Push::demandOnly`) runs the same descent for its page requests
// and emits nothing — the shape a view takes when it reads the scene through something
// other than the record stream but still has to keep what it reads resident.
//
// Set 0: b0 = counters (3 records count, 4 record overflow), b1 = visible slot list,
// b2 = the semantic record stream, b3 = the GPU-scene address block, b4 = the
// cross-frame transition state table.

import global_gpu_data;

public static const uint SCENE_TRAVERSAL_OVERFLOW_RECORDS = 4u;
public static const uint SCENE_TRANSITION_PRESSURE = 32u;
public static const uint SCENE_TRAVERSAL_STACK_DEPTH = 32u;
// Entries in the transition state table (a power of two; four words per entry).
public static const uint SCENE_TRANSITION_STATE_CAPACITY = 65536u;
static const uint TRANSITION_PROBE_STEPS = 8u;
static const uint TRANSITION_MODE_ON_CUT = 1u;
static const uint TRANSITION_MODE_DESCENDED = 2u;

[[vk::binding(0, 0)]] RWStructuredBuffer<uint> counters;
[[vk::binding(1, 0)]] RWStructuredBuffer<uint> visibleList;
[[vk::binding(2, 0)]] RWStructuredBuffer<GpuDrawRecord> records;
[[vk::binding(3, 0)]] ConstantBuffer<GpuSceneAddressBlock> sceneAddresses;
[[vk::binding(4, 0)]] RWStructuredBuffer<uint> transitions;

// Cut overrides for `Push::representationOverride`.
static const uint SCENE_CUT_AUTO = 0u;
static const uint SCENE_CUT_FORCE_COARSE = 1u;
static const uint SCENE_CUT_FORCE_FINE = 2u;

struct Push
{
    // World → clip for the node cull. The walk rejects a node whose swept world bounds
    // leave this frustum and never descends into it.
    float4x4 viewProj;
    float3 eye;
    float projScale;
    float errorThresholdPx;
    uint recordCapacity;
    uint listCapacity;
    uint survivor;
    // Nonzero: an instance holding a row in the frame's displacement arena emits one
    // record for its amplified micro-geometry instead of walking its base hierarchy.
    // Zero: every instance walks its base pages, which is what a view that reads the
    // undisplaced surface (shadow pages, the reach walk) wants.
    uint displacedRecords;
    // Crossfade length in frames for this view; zero disables transitions (the view
    // draws settled cuts only and never touches the state table).
    uint transitionFrames;
    // Monotonic frame counter for state staleness and once-per-frame advancement.
    uint frameStamp;
    // Debug: pin the hierarchy cut instead of letting projected error choose it.
    // 0 = follow the error threshold, 1 = never refine (coarsest cut, aggregate voxels),
    // 2 = always refine (finest cut, triangle clusters). A representation comparison needs
    // the cut to move while the camera holds still; distance moves both at once.
    uint representationOverride;
    // Zero: skip the node cull and walk every node of every visible instance. The
    // cull is a pure reduction, so a host with it off must render the identical frame.
    uint nodeCull;
    // Nonzero: walk for page demand only and emit no draw records. A view that reads
    // the scene through something other than the record stream still has to keep the
    // pages it reads resident, and that is the whole of what its walk is for.
    uint demandOnly;
    // Which class of view this walk serves; rides every missing-page request so the
    // CPU prices the demand by who missed rather than treating every miss alike.
    uint viewClass;
};
[[vk::push_constant]] Push pc;

bool pageResident(GpuSceneAddressBlock addresses, GpuHandle handle, out GpuPageRecord record)
{
    let header = gpuSceneSlotHeader(addresses.pages, GPU_PAGE_TABLE_STRIDE, handle.index);
    record = gpuSceneResidentPage(addresses, handle);
    return gpuHandleMatches(handle, header) && record.byteLength != 0u;
}

GpuHandle resolveMaterial(
    GpuSceneAddressBlock addresses,
    GpuSceneInstanceGpuRecord instance,
    GpuScenePrototypeGpuRecord prototype,
    uint materialSlot)
{
    GpuHandle sceneMaterial = gpuScenePrototypeMaterialHandle(
        addresses,
        prototype.materialRange.first
            + min(materialSlot, prototype.materialRange.count - 1u));
    for (uint element = 0u; element < instance.materialOverrides.count; ++element)
    {
        let override = gpuSceneOverrideElement(
            addresses, instance.materialOverrides.first + element);
        if (override.slot == materialSlot)
        {
            sceneMaterial = override.material;
            break;
        }
    }
    return sceneMaterial;
}

void emitRecord(GpuDrawRecord record, uint indexCount)
{
    uint index;
    InterlockedAdd(counters[3], 1u, index);
    if (index < pc.recordCapacity)
    {
        records[index] = record;
        // Counter word 8: triangles the emitted records rasterize.
        InterlockedAdd(counters[8], indexCount / 3u);
        if (record.transition != 0u)
        {
            // Counter word 10: records mid representation-crossfade this frame.
            InterlockedAdd(counters[10], 1u);
        }
    }
    else
    {
        InterlockedOr(counters[4], SCENE_TRAVERSAL_OVERFLOW_RECORDS);
    }
}

float instanceScale(float4 c0, float4 c1, float4 c2)
{
    return max(length(c0.xyz), max(length(c1.xyz), length(c2.xyz)));
}

// Mean basis-column length of an assembly use's transform — the scale factor its
// prototype-local error projects through.
float assemblyUseScale(GpuAssemblyUseRecord use)
{
    float3 c0 = float3(use.row0.x, use.row1.x, use.row2.x);
    float3 c1 = float3(use.row0.y, use.row1.y, use.row2.y);
    float3 c2 = float3(use.row0.z, use.row1.z, use.row2.z);
    return max((length(c0) + length(c1) + length(c2)) / 3.0, 1e-4);
}

// Whether this node's swept world bounds leave the view entirely.
//
// The local box is the node's DEFORMED bounds: the cooked swept extent, which encloses
// the node's rest geometry, its authored deformation, and — by the cooker's subtree
// closure — every descendant. Runtime wind is not cooked, so the world box additionally
// gains the prepass record's world-space sway slack, the same term the instance sphere
// adds. A rejected node therefore never hides geometry the frame can see, at any level
// of the walk, which is what lets the rejection take the whole subtree with it.
bool nodeCulled(
    GpuSceneAddressBlock addresses,
    GpuSceneInstanceGpuRecord instance,
    uint slot,
    GpuPageNodeRecord node,
    GpuGeometryRecord geometry,
    uint use)
{
    float4 c0, c1, c2, c3;
    gpuSceneInstanceColumns(instance, false, c0, c1, c2, c3);
    bool assemblyUse = use != GPU_ASSEMBLY_NO_USE;
    GpuAssemblyUseRecord useRecord;
    if (assemblyUse)
    {
        useRecord = gpuSceneAssemblyUse(addresses, geometry, geometry.reserved, use);
    }
    float3 worldMin = float3(1e30);
    float3 worldMax = float3(-1e30);
    for (uint corner = 0u; corner < 8u; ++corner)
    {
        float3 local = float3(
            (corner & 1u) != 0u ? node.deformedMax.x : node.deformedMin.x,
            (corner & 2u) != 0u ? node.deformedMax.y : node.deformedMin.y,
            (corner & 4u) != 0u ? node.deformedMax.z : node.deformedMin.z);
        if (assemblyUse)
        {
            local = gpuSceneAssemblyTransformPoint(useRecord, local);
        }
        float3 world = c0.xyz * local.x + c1.xyz * local.y + c2.xyz * local.z + c3.xyz;
        worldMin = min(worldMin, world);
        worldMax = max(worldMax, world);
    }
    if ((instance.flags & GPU_SCENE_INSTANCE_FLAG_WIND) != 0u
        && addresses.windRecords != 0)
    {
        GpuWindInstanceRecord* record = (GpuWindInstanceRecord*)(
            addresses.windRecords + uint64_t(slot) * GPU_WIND_INSTANCE_RECORD_SIZE);
        float slack = (*record).boundsInflation;
        worldMin -= float3(slack);
        worldMax += float3(slack);
    }
    float3 ndcMin = float3(1e30);
    float3 ndcMax = float3(-1e30);
    for (uint corner = 0u; corner < 8u; ++corner)
    {
        float3 world = float3(
            (corner & 1u) != 0u ? worldMax.x : worldMin.x,
            (corner & 2u) != 0u ? worldMax.y : worldMin.y,
            (corner & 4u) != 0u ? worldMax.z : worldMin.z);
        float4 clip = mul(pc.viewProj, float4(world, 1.0));
        // Straddling the near plane: the projected extent is unbounded, so keep it.
        if (clip.w <= 1e-5)
        {
            return false;
        }
        float3 ndc = clip.xyz / clip.w;
        ndcMin = min(ndcMin, ndc);
        ndcMax = max(ndcMax, ndc);
    }
    return ndcMax.x < -1.0 || ndcMin.x > 1.0 || ndcMax.y < -1.0 || ndcMin.y > 1.0
        || ndcMax.z < 0.0 || ndcMin.z > 1.0;
}

// The stable flip key of one (instance slot, page, use) triple — never zero, so zero
// marks an empty table entry. An assembly visits one node once per use and each use's
// cut decision is its own: sharing one entry would let two uses read each other's mode
// as a flip and restart the crossfade forever, which draws the aggregate over the fine
// geometry every frame. `GPU_ASSEMBLY_NO_USE + 1u` is zero, so a plain mesh's key reduces
// to the (slot, page) hash.
uint transitionKey(uint slot, uint page, uint use)
{
    uint hash = slot * 0x9e3779b9u ^ page * 0x85ebca6bu ^ ((use + 1u) * 0xc2b2ae35u);
    hash ^= hash >> 16u;
    hash *= 0x7feb352du;
    hash ^= hash >> 15u;
    return max(hash, 1u);
}

// Finds the key's entry, claiming an empty or expired slot when absent. Returns the
// entry's first word index, or 0xffffffff under table pressure (the caller settles).
uint transitionSlot(uint key)
{
    uint start = key & (SCENE_TRANSITION_STATE_CAPACITY - 1u);
    for (uint probe = 0u; probe < TRANSITION_PROBE_STEPS; ++probe)
    {
        uint entry = ((start + probe) & (SCENE_TRANSITION_STATE_CAPACITY - 1u)) * 4u;
        uint existing = transitions[entry];
        if (existing == key)
        {
            return entry;
        }
        bool expired = existing != 0u && (pc.frameStamp - transitions[entry + 2u]) > 4u;
        if (existing == 0u || expired)
        {
            uint previous;
            InterlockedCompareExchange(transitions[entry], existing, key, previous);
            if (previous == existing)
            {
                // Claimed: reset to an absent state under the new key.
                transitions[entry + 1u] = 0u;
                transitions[entry + 2u] = 0u;
                return entry;
            }
            if (previous == key)
            {
                return entry;
            }
        }
    }
    InterlockedOr(counters[4], SCENE_TRANSITION_PRESSURE);
    return 0xffffffffu;
}

// Emits the draw records for one cut node under one assembly use (GPU_ASSEMBLY_NO_USE
// outside assemblies): the voxel payload as one record, or one record per triangle
// cluster, each carrying the use index in `clusterState` and the representation
// crossfade word in `transition` (zero when settled).
void emitNodeRecords(
    GpuSceneAddressBlock addresses,
    GpuSceneInstanceGpuRecord instance,
    GpuScenePrototypeGpuRecord prototype,
    uint slot,
    GpuHandle pageHandle,
    GpuPageRecord page,
    GpuPageNodeRecord node,
    uint use,
    uint transition,
    bool subQuad)
{
    if (pc.demandOnly != 0u)
    {
        return;
    }
    GpuDrawRecord record;
    record.geometry = prototype.geometry;
    record.instance = GpuHandle(slot, instance.sourceGeneration);
    record.deformation = instance.deformation;
    record.contentIndex = pageHandle.index;
    record.representation = node.representation;
    record.sourceGeneration = instance.sourceGeneration;
    record.transition = transition;
    record.clusterState = use;
    if (node.representation == GPU_REPRESENTATION_AGGREGATE_VOXEL)
    {
        let sceneMaterial = resolveMaterial(addresses, instance, prototype, 0u);
        let materialReference =
            gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
        let material = gpuSceneResidentMaterial(addresses, materialReference.target);
        record.material = sceneMaterial;
        record.part = 0u;
        record.psoBin = composeGpuPsoBin(
            node.representation, material.materialClass, GPU_DEFORMATION_RIGID, 0u);
        record.reserved = material.shaderIndex;
        // Counter word 11: aggregate-voxel records on the cut.
        InterlockedAdd(counters[11], 1u);
        if (subQuad)
        {
            InterlockedAdd(counters[15], node.indexCount / 3u);
        }
        emitRecord(record, node.indexCount);
    }
    else
    {
        for (uint cluster = 0u; cluster < node.clusterCount; ++cluster)
        {
            let clusterRecord = gpuScenePageCluster(addresses, page, node, cluster);
            let sceneMaterial = resolveMaterial(
                addresses, instance, prototype, clusterRecord.materialSlot);
            let materialReference =
                gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
            let material = gpuSceneResidentMaterial(addresses, materialReference.target);
            record.material = sceneMaterial;
            record.part = cluster;
            record.psoBin = composeGpuPsoBin(
                node.representation,
                material.materialClass,
                GPU_DEFORMATION_RIGID,
                0u);
            record.reserved = material.shaderIndex;
            if (subQuad)
            {
                // Counter word 15: triangles of records projecting under one quad.
                InterlockedAdd(counters[15], clusterRecord.indexCount / 3u);
            }
            emitRecord(record, clusterRecord.indexCount);
        }
    }
}


// Emits the one record a displaced instance draws through: its whole amplified
// micro-geometry, addressed by the row it owns in the frame's displacement arena. The
// amplification covers the base mesh in one packed slice, so there is no cut to walk and
// no cluster to partition — the row IS the instance's geometry for this view. Slot-0
// material, matching the arena the emit kernel displaced through. The triangle count
// lives in the arena's draw seed, so the binner adds it once it resolves the command.
void emitDisplacedRecord(
    GpuSceneAddressBlock addresses,
    GpuSceneInstanceGpuRecord instance,
    GpuScenePrototypeGpuRecord prototype,
    uint slot,
    uint row)
{
    let sceneMaterial = resolveMaterial(addresses, instance, prototype, 0u);
    let materialReference = gpuSceneLoadMaterialReference(addresses, sceneMaterial.index);
    let material = gpuSceneResidentMaterial(addresses, materialReference.target);
    GpuDrawRecord record;
    record.geometry = prototype.geometry;
    record.material = sceneMaterial;
    record.instance = GpuHandle(slot, instance.sourceGeneration);
    record.deformation = instance.deformation;
    record.contentIndex = row;
    record.part = 0u;
    record.representation = GPU_REPRESENTATION_DISPLACED_MICRO;
    record.sourceGeneration = instance.sourceGeneration;
    record.psoBin = composeGpuPsoBin(
        GPU_REPRESENTATION_DISPLACED_MICRO,
        material.materialClass,
        GPU_DEFORMATION_RIGID,
        0u);
    record.transition = 0u;
    record.clusterState = GPU_ASSEMBLY_NO_USE;
    record.reserved = material.shaderIndex;
    emitRecord(record, 0u);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 tid : SV_DispatchThreadID)
{
    let addresses = sceneAddresses;
    // A survivor pass walks only the retest-merged tail of the visible list
    // (counter word 6 snapshots the provisional count).
    uint base = pc.survivor != 0u ? counters[6] : 0u;
    uint index = base + tid.x;
    uint visibleCount = min(counters[0], pc.listCapacity);
    if (index >= visibleCount)
    {
        return;
    }
    uint slot = visibleList[index];
    let instance = gpuSceneLoadInstance(addresses, slot);
    let prototype = gpuSceneLoadPrototype(addresses, instance.prototype.index);

    uint displacedRow;
    if (pc.displacedRecords != 0u && pc.demandOnly == 0u
        && gpuSceneDisplacedRow(addresses, slot, displacedRow))
    {
        emitDisplacedRecord(addresses, instance, prototype, slot, displacedRow);
        return;
    }

    // The prototype's root is a scene-page handle; children in payloads are resident
    // page-table handles already.
    let rootScene = gpuSceneLoadPage(addresses, prototype.rootPage.index);
    // The assembly vocabulary: a geometry with parts places each source prototype
    // through per-use family-local transforms; the walk forks per use at the first
    // single-prototype node, and every emitted record carries its use index.
    let geometry = gpuSceneResidentGeometry(addresses, prototype.geometry);
    bool assemblyActive = geometry.parts.count != 0u;
    GpuHandle pageStack[SCENE_TRAVERSAL_STACK_DEPTH];
    uint useStack[SCENE_TRAVERSAL_STACK_DEPTH];
    // The inherited incoming-crossfade word: descendants of a refining flip node
    // carry its phase and flip id down the walk (zero outside a transition).
    uint transStack[SCENE_TRAVERSAL_STACK_DEPTH];
    // Hierarchy level per pending node (root 0), for the max-cut-depth counter.
    uint levelStack[SCENE_TRAVERSAL_STACK_DEPTH];
    uint depth = 0u;
    pageStack[0] = rootScene.table;
    useStack[0] = GPU_ASSEMBLY_NO_USE;
    transStack[0] = 0u;
    levelStack[0] = 0u;
    depth = 1u;

    // The combination-crossfade words ride the static payload (previous
    // combination at word 30, flip stamp at 31). During the transition window the
    // fork loops emit uses of BOTH masks with a shared-flip-id transition word, so
    // a seasonal phenotype swap crossfades instead of popping.
    uint4 flipWords = asuint(instance.transform[7]);
    uint flipPrevious = flipWords.z;
    uint flipStamp = flipWords.w;
    uint flipAge = pc.frameStamp - flipStamp;
    bool combinationFlip = instance.transformKind == GPU_SCENE_TRANSFORM_STATIC
        && flipStamp != 0u && flipPrevious != instance.reserved
        && flipAge < GPU_TRANSITION_FRAMES;
    uint combinationTransition = 0u;
    if (combinationFlip)
    {
        uint phase = clamp(flipAge + 1u, 1u, GPU_TRANSITION_FRAMES);
        uint flipId = (((slot * 0x9e3779b9u) ^ (flipStamp * 0x85ebca6bu)) >> 16u) & 0xffffu;
        // The outgoing bit is per-use; pack both variants once.
        combinationTransition = gpuTransitionPack(phase, false, flipId);
    }

    float4 instanceC0, instanceC1, instanceC2, instanceC3;
    gpuSceneInstanceColumns(instance, false, instanceC0, instanceC1, instanceC2, instanceC3);
    float scale = instanceScale(instanceC0, instanceC1, instanceC2);
    float distance = max(length(instanceC3.xyz - pc.eye), 0.05);

    while (depth > 0u)
    {
        depth -= 1u;
        GpuHandle pageHandle = pageStack[depth];
        uint use = useStack[depth];
        uint inherited = transStack[depth];
        uint level = levelStack[depth];
        GpuPageRecord page;
        if (!pageResident(addresses, pageHandle, page))
        {
            gpuSceneRequestPage(addresses, pageHandle.index, pc.viewClass);
            continue;
        }
        let node = gpuScenePageNode(addresses, page);

        // The assembly fork: the first node owned by a single prototype re-enters the
        // walk once per use of that prototype. A full stack degrades to drawing this
        // node inline per use (a coarser cut, never a hole).
        float useScale = 1.0;
        if (assemblyActive && use == GPU_ASSEMBLY_NO_USE
            && node.prototype != GPU_PAGE_NODE_NO_PROTOTYPE)
        {
            let assemblyPrototype =
                gpuSceneAssemblyPrototype(addresses, geometry, node.prototype);
            // The instance's (variation, phenotype) combination masks which uses draw.
            let assemblyHeader = gpuSceneAssemblyHeader(addresses, geometry);
            if (depth + assemblyPrototype.useCount <= SCENE_TRAVERSAL_STACK_DEPTH)
            {
                for (uint fork = 0u; fork < assemblyPrototype.useCount; ++fork)
                {
                    uint forkUse = assemblyPrototype.firstUse + fork;
                    bool activeNew = gpuSceneAssemblyUseActive(
                        addresses, geometry, assemblyHeader, instance.reserved, forkUse);
                    bool activeOld = combinationFlip
                        ? gpuSceneAssemblyUseActive(
                              addresses, geometry, assemblyHeader, flipPrevious, forkUse)
                        : activeNew;
                    if (!activeNew && !activeOld)
                    {
                        continue;
                    }
                    uint useTransition = inherited;
                    if (inherited == 0u && activeNew != activeOld)
                    {
                        useTransition = combinationTransition
                            | (activeOld ? GPU_TRANSITION_OUTGOING : 0u);
                    }
                    pageStack[depth] = pageHandle;
                    useStack[depth] = forkUse;
                    transStack[depth] = useTransition;
                    levelStack[depth] = level;
                    depth += 1u;
                }
                continue;
            }
            for (uint fork = 0u; fork < assemblyPrototype.useCount; ++fork)
            {
                uint forkUse = assemblyPrototype.firstUse + fork;
                bool activeNew = gpuSceneAssemblyUseActive(
                    addresses, geometry, assemblyHeader, instance.reserved, forkUse);
                bool activeOld = combinationFlip
                    ? gpuSceneAssemblyUseActive(
                          addresses, geometry, assemblyHeader, flipPrevious, forkUse)
                    : activeNew;
                if (!activeNew && !activeOld)
                {
                    continue;
                }
                uint useTransition = inherited;
                if (inherited == 0u && activeNew != activeOld)
                {
                    useTransition = combinationTransition
                        | (activeOld ? GPU_TRANSITION_OUTGOING : 0u);
                }
                emitNodeRecords(
                    addresses,
                    instance,
                    prototype,
                    slot,
                    pageHandle,
                    page,
                    node,
                    forkUse,
                    useTransition,
                    false);
            }
            continue;
        }
        if (use != GPU_ASSEMBLY_NO_USE)
        {
            let useRecord = gpuSceneAssemblyUse(addresses, geometry, geometry.reserved, use);
            useScale = assemblyUseScale(useRecord);
        }

        // Counter word 17: nodes reached with a resolved use — the denominator.
        InterlockedAdd(counters[17], 1u);
        if (pc.nodeCull != 0u && nodeCulled(addresses, instance, slot, node, geometry, use))
        {
            // Counter word 16: nodes rejected on their swept bounds, subtree and all.
            InterlockedAdd(counters[16], 1u);
            continue;
        }

        // Refine when the projected error is still too large, every child payload is
        // resident, and the stack can hold the children; otherwise draw this node.
        float projectedPx =
            (float(node.appearanceTotal) / 65536.0) * scale * useScale * pc.projScale / distance;
        bool wantRefine = projectedPx > pc.errorThresholdPx;
        if (pc.representationOverride == SCENE_CUT_FORCE_COARSE)
        {
            wantRefine = false;
        }
        else if (pc.representationOverride == SCENE_CUT_FORCE_FINE)
        {
            wantRefine = true;
        }
        bool refine = node.childCount != 0u && wantRefine
            && depth + node.childCount <= SCENE_TRAVERSAL_STACK_DEPTH;
        if (refine)
        {
            for (uint child = 0u; child < node.childCount; ++child)
            {
                GpuHandle childHandle = gpuScenePageChild(addresses, page, child);
                GpuPageRecord childRecord;
                if (!pageResident(addresses, childHandle, childRecord))
                {
                    gpuSceneRequestPage(addresses, childHandle.index, pc.viewClass);
                    refine = false;
                }
            }
        }

        // The flip-node state machine: only nodes outside an inherited crossfade own
        // state. It remembers whether this node drew or descended recently and starts
        // a crossfade on the frame the decision flips.
        uint ownTransition = inherited;
        uint childTransition = inherited;
        bool emitParentDuringRefine = false;
        if (pc.transitionFrames != 0u && inherited == 0u)
        {
            uint key = transitionKey(slot, pageHandle.index, use);
            uint entry = transitionSlot(key);
            if (entry != 0xffffffffu)
            {
                uint state = transitions[entry + 1u];
                uint stamp = transitions[entry + 2u];
                bool fresh = state != 0u && (pc.frameStamp - stamp) <= 2u;
                uint mode = fresh ? state & 3u : 0u;
                uint phase = fresh ? (state >> 8u) & 0x3fu : 0u;
                bool advance = stamp != pc.frameStamp;
                uint flip = key >> 16u;
                if (refine)
                {
                    if (mode == TRANSITION_MODE_ON_CUT)
                    {
                        // The node left the cut this frame: crossfade to children.
                        phase = 1u;
                    }
                    else if (mode == TRANSITION_MODE_DESCENDED && phase != 0u && advance)
                    {
                        phase = phase + 1u > pc.transitionFrames ? 0u : phase + 1u;
                    }
                    else if (mode == 0u)
                    {
                        phase = 0u;
                    }
                    transitions[entry + 1u] = TRANSITION_MODE_DESCENDED | (phase << 8u);
                    transitions[entry + 2u] = pc.frameStamp;
                    if (phase != 0u)
                    {
                        ownTransition = gpuTransitionPack(phase, true, flip);
                        childTransition = gpuTransitionPack(phase, false, flip);
                        emitParentDuringRefine = true;
                    }
                }
                else
                {
                    if (mode == TRANSITION_MODE_DESCENDED)
                    {
                        // The node returned to the cut this frame: crossfade back.
                        phase = 1u;
                    }
                    else if (mode == TRANSITION_MODE_ON_CUT && phase != 0u && advance)
                    {
                        phase = phase + 1u > pc.transitionFrames ? 0u : phase + 1u;
                    }
                    else if (mode == 0u)
                    {
                        phase = 0u;
                    }
                    transitions[entry + 1u] = TRANSITION_MODE_ON_CUT | (phase << 8u);
                    transitions[entry + 2u] = pc.frameStamp;
                    if (phase != 0u)
                    {
                        // The node dissolves in; its immediate children dissolve out.
                        ownTransition = gpuTransitionPack(phase, false, flip);
                        for (uint child = 0u; child < node.childCount; ++child)
                        {
                            GpuHandle childHandle = gpuScenePageChild(addresses, page, child);
                            GpuPageRecord childPage;
                            if (!pageResident(addresses, childHandle, childPage))
                            {
                                continue;
                            }
                            let childNode = gpuScenePageNode(addresses, childPage);
                            emitNodeRecords(
                                addresses,
                                instance,
                                prototype,
                                slot,
                                childHandle,
                                childPage,
                                childNode,
                                use,
                                gpuTransitionPack(phase, true, flip),
                                false);
                        }
                    }
                }
            }
        }

        if (refine)
        {
            if (emitParentDuringRefine)
            {
                emitNodeRecords(
                    addresses, instance, prototype, slot, pageHandle, page, node, use,
                    ownTransition, false);
            }
            for (uint child = 0u; child < node.childCount; ++child)
            {
                pageStack[depth] = gpuScenePageChild(addresses, page, child);
                useStack[depth] = use;
                transStack[depth] = childTransition;
                levelStack[depth] = level + 1u;
                depth += 1u;
            }
            continue;
        }

        // Counter word 12: the deepest hierarchy level on the emitted cut.
        InterlockedMax(counters[12], level);
        emitNodeRecords(
            addresses, instance, prototype, slot, pageHandle, page, node, use, ownTransition,
            projectedPx < 2.0);
    }
}
