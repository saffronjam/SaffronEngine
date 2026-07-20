import spatial_numeric;

static const uint VegetationGraphMagic = 0x53414750u;
static const uint VegetationGraphAbiVersion = 1u;
static const uint VegetationGraphProgramHeaderWords = 8u;
static const uint VegetationGraphInstructionWords = 16u;
static const uint VegetationGraphInvocationWords = 4u;
static const uint VegetationGraphOutputWords = 4u;
static const uint VegetationGraphMaximumInputs = 32u;
static const uint VegetationGraphMaximumInstructions = 32u;
static const uint VegetationGraphMaximumRegisters = 64u;
static const uint VegetationGraphMaximumCurvePoints = 6u;
static const uint VegetationGraphNoRegister = 0xffffffffu;

static const uint VegetationGraphNoise = 1u;
static const uint VegetationGraphGradient = 2u;
static const uint VegetationGraphCurve = 3u;
static const uint VegetationGraphRemap = 4u;
static const uint VegetationGraphCombine = 5u;
static const uint VegetationGraphClamp = 6u;
static const uint VegetationGraphFieldImportance = 7u;

static const uint VegetationGraphFixedScalar = 1u;
static const uint VegetationGraphUnit = 2u;
static const uint VegetationGraphCandidateMask = 3u;
static const uint VegetationGraphWorldTick = 4u;

[[vk::binding(0, 0)]] StructuredBuffer<uint> program;
[[vk::binding(1, 0)]] StructuredBuffer<uint> invocations;
[[vk::binding(2, 0)]] RWStructuredBuffer<uint> outputs;

bool vegetationGraphRegister(
    uint registerIndex,
    uint destination,
    uint expectedType,
    uint registerTypes[VegetationGraphMaximumRegisters])
{
    return registerIndex < destination && registerTypes[registerIndex] == expectedType;
}

void vegetationGraphWrite(
    uint invocationIndex,
    bool valid,
    bool candidateMask,
    uint valueType,
    int3 value)
{
    uint outputBase = invocationIndex * VegetationGraphOutputWords;
    outputs[outputBase] = valid ? 1u : 0u;
    outputs[outputBase + 1u] = valid && candidateMask ? 1u : 0u;
    outputs[outputBase + 2u] = valueType;
    outputs[outputBase + 3u] = valid ? uint(value.x) : 0u;
}

bool vegetationGraphWideNegative(uint4 value)
{
    return (value.w & 0x80000000u) != 0u;
}

uint4 vegetationGraphWideNegate(uint4 value)
{
    value = ~value;
    uint carry = 1u;
    for (uint limb = 0u; limb < 4u; limb++)
    {
        uint next = value[limb] + carry;
        carry = carry != 0u && next == 0u ? 1u : 0u;
        value[limb] = next;
    }
    return value;
}

uint4 vegetationGraphWideAdd(uint4 left, uint4 right, out bool valid)
{
    uint4 result;
    uint carry = 0u;
    for (uint limb = 0u; limb < 4u; limb++)
    {
        uint addend = right[limb] + carry;
        uint addendCarry = addend < right[limb] ? 1u : 0u;
        result[limb] = left[limb] + addend;
        carry = addendCarry != 0u || result[limb] < left[limb] ? 1u : 0u;
    }
    bool leftNegative = vegetationGraphWideNegative(left);
    bool rightNegative = vegetationGraphWideNegative(right);
    bool resultNegative = vegetationGraphWideNegative(result);
    valid = leftNegative != rightNegative || resultNegative == leftNegative;
    return result;
}

uint4 vegetationGraphWideSub(uint4 left, uint4 right, out bool valid)
{
    uint4 result;
    uint borrow = 0u;
    for (uint limb = 0u; limb < 4u; limb++)
    {
        uint subtrahend = right[limb] + borrow;
        uint subtrahendCarry = subtrahend < right[limb] ? 1u : 0u;
        result[limb] = left[limb] - subtrahend;
        borrow = subtrahendCarry != 0u || left[limb] < subtrahend ? 1u : 0u;
    }
    bool leftNegative = vegetationGraphWideNegative(left);
    bool rightNegative = vegetationGraphWideNegative(right);
    bool resultNegative = vegetationGraphWideNegative(result);
    valid = leftNegative == rightNegative || resultNegative == leftNegative;
    return result;
}

uint4 vegetationGraphWideFromI32(int value)
{
    uint extension = value < 0 ? 0xffffffffu : 0u;
    return uint4(uint(value), extension, extension, extension);
}

uint4 vegetationGraphWideMulI32(uint4 value, int factor, out bool valid)
{
    if (factor == 0 || all(value == uint4(0u)))
    {
        valid = true;
        return uint4(0u);
    }
    bool negative = vegetationGraphWideNegative(value) != (factor < 0);
    uint4 magnitude = vegetationGraphWideNegative(value)
        ? vegetationGraphWideNegate(value)
        : value;
    uint factorMagnitude = factor < 0 ? uint(-int64_t(factor)) : uint(factor);
    uint4 product;
    uint64_t carry = uint64_t(0);
    for (uint limb = 0u; limb < 4u; limb++)
    {
        uint64_t lane = uint64_t(magnitude[limb]) * uint64_t(factorMagnitude) + carry;
        product[limb] = uint(lane);
        carry = lane >> 32u;
    }
    bool positiveRange = (product.w & 0x80000000u) == 0u;
    bool negativeRange = product.w < 0x80000000u
        || (product.w == 0x80000000u
            && product.x == 0u
            && product.y == 0u
            && product.z == 0u);
    valid = carry == 0u && (negative ? negativeRange : positiveRange);
    return negative ? vegetationGraphWideNegate(product) : product;
}

int vegetationGraphWideRoundQ28(uint4 value, out bool valid)
{
    bool negative = vegetationGraphWideNegative(value);
    uint4 magnitude = negative ? vegetationGraphWideNegate(value) : value;
    uint4 quotient = uint4(
        (magnitude.x >> 28u) | (magnitude.y << 4u),
        (magnitude.y >> 28u) | (magnitude.z << 4u),
        (magnitude.z >> 28u) | (magnitude.w << 4u),
        magnitude.w >> 28u);
    uint remainder = magnitude.x & 0x0fffffffu;
    if (remainder > 0x08000000u
        || (remainder == 0x08000000u && (quotient.x & 1u) != 0u))
    {
        uint carry = 1u;
        for (uint limb = 0u; limb < 4u; limb++)
        {
            uint next = quotient[limb] + carry;
            carry = carry != 0u && next == 0u ? 1u : 0u;
            quotient[limb] = next;
        }
    }
    uint limit = negative ? 0x80000000u : 0x7fffffffu;
    valid = quotient.y == 0u
        && quotient.z == 0u
        && quotient.w == 0u
        && quotient.x <= limit;
    if (!valid)
    {
        return 0;
    }
    return negative
        ? (quotient.x == 0x80000000u ? -2147483647 - 1 : -int(quotient.x))
        : int(quotient.x);
}

[shader("compute")]
[numthreads(64, 1, 1)]
void computeMain(uint3 threadId : SV_DispatchThreadID)
{
    uint programWordCount;
    uint programStride;
    program.GetDimensions(programWordCount, programStride);
    if (programWordCount < VegetationGraphProgramHeaderWords)
    {
        return;
    }

    uint inputCount = program[2u];
    uint instructionCount = program[3u];
    uint registerCount = program[4u];
    uint outputRegister = program[5u];
    uint outputType = program[6u];
    uint maskRegister = program[7u];
    uint invocationWordCount;
    uint invocationStride;
    invocations.GetDimensions(invocationWordCount, invocationStride);
    uint wordsPerInvocation = inputCount * VegetationGraphInvocationWords;
    if (wordsPerInvocation == 0u || invocationWordCount % wordsPerInvocation != 0u)
    {
        return;
    }
    uint invocationCount = invocationWordCount / wordsPerInvocation;
    if (threadId.x >= invocationCount)
    {
        return;
    }

    bool valid = program[0u] == VegetationGraphMagic
        && program[1u] == VegetationGraphAbiVersion
        && inputCount > 0u
        && inputCount <= VegetationGraphMaximumInputs
        && instructionCount > 0u
        && instructionCount <= VegetationGraphMaximumInstructions
        && registerCount == inputCount + instructionCount
        && registerCount <= VegetationGraphMaximumRegisters
        && programWordCount == VegetationGraphProgramHeaderWords
            + inputCount
            + instructionCount * VegetationGraphInstructionWords;
    int4 registers[VegetationGraphMaximumRegisters];
    uint registerTypes[VegetationGraphMaximumRegisters];
    for (uint registerIndex = 0u; registerIndex < VegetationGraphMaximumRegisters; registerIndex++)
    {
        registers[registerIndex] = int4(0);
        registerTypes[registerIndex] = 0u;
    }

    uint invocationBase = threadId.x * wordsPerInvocation;
    for (uint inputIndex = 0u; inputIndex < inputCount; inputIndex++)
    {
        uint registerType = program[VegetationGraphProgramHeaderWords + inputIndex];
        uint valueBase = invocationBase + inputIndex * VegetationGraphInvocationWords;
        int4 value = int4(
            int(invocations[valueBase]),
            int(invocations[valueBase + 1u]),
            int(invocations[valueBase + 2u]),
            int(invocations[valueBase + 3u]));
        valid = valid
            && registerType >= VegetationGraphFixedScalar
            && registerType <= VegetationGraphWorldTick;
        if (registerType == VegetationGraphUnit)
        {
            valid = valid && invocations[valueBase] <= 65535u;
        }
        else if (registerType == VegetationGraphCandidateMask)
        {
            valid = valid && invocations[valueBase] <= 1u;
        }
        registerTypes[inputIndex] = registerType;
        registers[inputIndex] = value;
    }

    uint instructionBase = VegetationGraphProgramHeaderWords + inputCount;
    for (uint instructionIndex = 0u; instructionIndex < instructionCount; instructionIndex++)
    {
        uint base = instructionBase + instructionIndex * VegetationGraphInstructionWords;
        uint operation = program[base];
        uint destination = program[base + 1u];
        valid = valid && destination == inputCount + instructionIndex;
        int3 value = int3(0);
        uint resultType = VegetationGraphFixedScalar;

        if (operation == VegetationGraphNoise)
        {
            for (uint cornerIndex = 0u; cornerIndex < 8u; cornerIndex++)
            {
                valid = valid && vegetationGraphRegister(
                    program[base + 2u + cornerIndex],
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes);
            }
            for (uint blendIndex = 0u; blendIndex < 3u; blendIndex++)
            {
                valid = valid && vegetationGraphRegister(
                    program[base + 10u + blendIndex],
                    destination,
                    VegetationGraphUnit,
                    registerTypes);
            }
            bool laneValid;
            int x00 = spatialFixedLerpQ16(
                registers[program[base + 2u]].x,
                registers[program[base + 3u]].x,
                uint(registers[program[base + 10u]].x),
                laneValid);
            valid = valid && laneValid;
            int x10 = spatialFixedLerpQ16(
                registers[program[base + 4u]].x,
                registers[program[base + 5u]].x,
                uint(registers[program[base + 10u]].x),
                laneValid);
            valid = valid && laneValid;
            int x01 = spatialFixedLerpQ16(
                registers[program[base + 6u]].x,
                registers[program[base + 7u]].x,
                uint(registers[program[base + 10u]].x),
                laneValid);
            valid = valid && laneValid;
            int x11 = spatialFixedLerpQ16(
                registers[program[base + 8u]].x,
                registers[program[base + 9u]].x,
                uint(registers[program[base + 10u]].x),
                laneValid);
            valid = valid && laneValid;
            int y0 = spatialFixedLerpQ16(
                x00,
                x10,
                uint(registers[program[base + 11u]].x),
                laneValid);
            valid = valid && laneValid;
            int y1 = spatialFixedLerpQ16(
                x01,
                x11,
                uint(registers[program[base + 11u]].x),
                laneValid);
            valid = valid && laneValid;
            int noise = spatialFixedLerpQ16(
                y0,
                y1,
                uint(registers[program[base + 12u]].x),
                laneValid);
            valid = valid && laneValid;
            value.x = spatialFixedMulQ16(noise, int(program[base + 13u]), laneValid);
            valid = valid && laneValid;
        }
        else if (operation == VegetationGraphGradient)
        {
            uint3 position = uint3(
                program[base + 2u],
                program[base + 3u],
                program[base + 4u]);
            uint3 exactOrigin = uint3(
                program[base + 5u],
                program[base + 6u],
                program[base + 7u]);
            for (uint lane = 0u; lane < 3u; lane++)
            {
                valid = valid && vegetationGraphRegister(
                    position[lane],
                    destination,
                    VegetationGraphWorldTick,
                    registerTypes);
                valid = valid && vegetationGraphRegister(
                    exactOrigin[lane],
                    destination,
                    VegetationGraphWorldTick,
                    registerTypes);
            }
            int3 direction = int3(
                int(program[base + 8u]),
                int(program[base + 9u]),
                int(program[base + 10u]));
            valid = valid && any(direction != int3(0));
            int scale = int(program[base + 11u]);
            int bias = int(program[base + 12u]);
            if (scale == 0)
            {
                value.x = bias;
            }
            else
            {
                uint4 dotNumerator = uint4(0u);
                for (uint lane = 0u; lane < 3u; lane++)
                {
                    bool laneValid;
                    uint4 delta = vegetationGraphWideSub(
                        uint4(registers[position[lane]]),
                        uint4(registers[exactOrigin[lane]]),
                        laneValid);
                    valid = valid && laneValid;
                    uint4 term = vegetationGraphWideMulI32(
                        delta,
                        direction[lane],
                        laneValid);
                    valid = valid && laneValid;
                    dotNumerator = vegetationGraphWideAdd(
                        dotNumerator,
                        term,
                        laneValid);
                    valid = valid && laneValid;
                }
                bool laneValid;
                uint4 scaled = vegetationGraphWideMulI32(
                    dotNumerator,
                    scale,
                    laneValid);
                valid = valid && laneValid;
                uint4 biasQ28 = vegetationGraphWideMulI32(
                    vegetationGraphWideFromI32(bias),
                    268435456,
                    laneValid);
                valid = valid && laneValid;
                uint4 biased = vegetationGraphWideAdd(
                    scaled,
                    biasQ28,
                    laneValid);
                valid = valid && laneValid;
                value.x = vegetationGraphWideRoundQ28(biased, laneValid);
                valid = valid && laneValid;
            }
        }
        else if (operation == VegetationGraphCurve)
        {
            uint input = program[base + 2u];
            uint pointCount = program[base + 3u];
            valid = valid
                && vegetationGraphRegister(
                    input,
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes)
                && pointCount > 0u
                && pointCount <= VegetationGraphMaximumCurvePoints;
            for (uint pointIndex = 1u; pointIndex < pointCount; pointIndex++)
            {
                valid = valid
                    && program[base + 4u + (pointIndex - 1u) * 2u]
                        < program[base + 4u + pointIndex * 2u];
            }
            uint x = uint(clamp(registers[input].x, 0, 65535));
            if (x <= program[base + 4u] || pointCount == 1u)
            {
                value.x = int(program[base + 5u]);
            }
            else if (x >= program[base + 4u + (pointCount - 1u) * 2u])
            {
                value.x = int(program[base + 5u + (pointCount - 1u) * 2u]);
            }
            else
            {
                bool sampled = false;
                for (uint pointIndex = 1u; pointIndex < pointCount; pointIndex++)
                {
                    uint x1 = program[base + 4u + pointIndex * 2u];
                    if (!sampled && x <= x1)
                    {
                        bool laneValid;
                        value.x = spatialFixedCurveSegmentQ16(
                            x,
                            program[base + 4u + (pointIndex - 1u) * 2u],
                            x1,
                            int(program[base + 5u + (pointIndex - 1u) * 2u]),
                            int(program[base + 5u + pointIndex * 2u]),
                            laneValid);
                        valid = valid && laneValid;
                        sampled = true;
                    }
                }
                valid = valid && sampled;
            }
        }
        else if (operation == VegetationGraphRemap)
        {
            uint input = program[base + 2u];
            int inputMinimum = int(program[base + 3u]);
            int inputMaximum = int(program[base + 4u]);
            int outputMinimum = int(program[base + 5u]);
            int outputMaximum = int(program[base + 6u]);
            valid = valid
                && vegetationGraphRegister(
                    input,
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes)
                && inputMinimum < inputMaximum;
            bool laneValid;
            int inputSpan = spatialFixedSub(inputMaximum, inputMinimum, laneValid);
            valid = valid && laneValid;
            int outputSpan = spatialFixedSub(outputMaximum, outputMinimum, laneValid);
            valid = valid && laneValid;
            int numerator = spatialFixedSub(
                clamp(registers[input].x, inputMinimum, inputMaximum),
                inputMinimum,
                laneValid);
            valid = valid && laneValid;
            int ratio = spatialFixedDivQ16(numerator, inputSpan, laneValid);
            valid = valid && laneValid;
            int scaled = spatialFixedMulQ16(outputSpan, ratio, laneValid);
            valid = valid && laneValid;
            value.x = spatialFixedAdd(outputMinimum, scaled, laneValid);
            valid = valid && laneValid;
        }
        else if (operation == VegetationGraphCombine)
        {
            uint left = program[base + 2u];
            uint right = program[base + 3u];
            uint combine = program[base + 4u];
            valid = valid
                && vegetationGraphRegister(
                    left,
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes)
                && vegetationGraphRegister(
                    right,
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes);
            bool laneValid = true;
            if (combine == 0u)
            {
                value.x = spatialFixedAdd(registers[left].x, registers[right].x, laneValid);
            }
            else if (combine == 1u)
            {
                value.x = spatialFixedMulQ16(registers[left].x, registers[right].x, laneValid);
            }
            else if (combine == 2u)
            {
                value.x = min(registers[left].x, registers[right].x);
            }
            else if (combine == 3u)
            {
                value.x = max(registers[left].x, registers[right].x);
            }
            else
            {
                laneValid = false;
            }
            valid = valid && laneValid;
        }
        else if (operation == VegetationGraphClamp)
        {
            uint input = program[base + 2u];
            int minimum = int(program[base + 3u]);
            int maximum = int(program[base + 4u]);
            valid = valid
                && vegetationGraphRegister(
                    input,
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes)
                && minimum <= maximum;
            value.x = clamp(registers[input].x, minimum, maximum);
        }
        else if (operation == VegetationGraphFieldImportance)
        {
            uint candidates = program[base + 2u];
            uint weights = program[base + 3u];
            uint threshold = program[base + 4u];
            valid = valid
                && vegetationGraphRegister(
                    candidates,
                    destination,
                    VegetationGraphCandidateMask,
                    registerTypes)
                && vegetationGraphRegister(
                    weights,
                    destination,
                    VegetationGraphFixedScalar,
                    registerTypes)
                && threshold <= 65535u;
            value.x = registers[candidates].x != 0 && registers[weights].x >= int(threshold)
                ? 1
                : 0;
            resultType = VegetationGraphCandidateMask;
        }
        else
        {
            valid = false;
        }

        registers[destination] = int4(value, 0);
        registerTypes[destination] = resultType;
    }

    valid = valid
        && maskRegister < registerCount
        && registerTypes[maskRegister] == VegetationGraphCandidateMask
        && ((outputRegister == VegetationGraphNoRegister && outputType == 0u)
            || (outputRegister < registerCount
                && registerTypes[outputRegister] == outputType
                && outputType != VegetationGraphCandidateMask));
    bool candidateMask = false;
    if (valid)
    {
        candidateMask = registers[maskRegister].x != 0;
    }
    int3 outputValue = int3(0);
    if (outputRegister != VegetationGraphNoRegister)
    {
        outputValue = registers[outputRegister].xyz;
    }
    vegetationGraphWrite(threadId.x, valid, candidateMask, outputType, outputValue);
}
