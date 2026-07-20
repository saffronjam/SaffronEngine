// Cross-platform authoritative spatial numerics. Integer decisions use these functions so
// dispatch order and floating-point implementation cannot change placement or identity.

static const uint SpatialPhiloxM0 = 0xD2511F53u;
static const uint SpatialPhiloxM1 = 0xCD9E8D57u;
static const uint SpatialPhiloxW0 = 0x9E3779B9u;
static const uint SpatialPhiloxW1 = 0xBB67AE85u;

struct SpatialRandomDomain
{
    uint4 map;
    uint4 nodeGuid;
    uint nodeSemanticRevision;
    uint4 seedNamespace;
    uint cellLevel;
    int64_t cellX;
    int64_t cellY;
    int64_t cellZ;
    uint64_t candidate;
    uint64_t ancestor;
    uint4 species;
    uint channel;
};

struct SpatialRandomStream
{
    uint4 counter;
    uint2 key;
};

uint4 spatialPhilox4x32_10(uint4 counter, uint2 key)
{
    for (uint round = 0u; round < 10u; round = round + 1u)
    {
        uint64_t product0 = uint64_t(SpatialPhiloxM0) * uint64_t(counter.x);
        uint64_t product1 = uint64_t(SpatialPhiloxM1) * uint64_t(counter.z);
        counter = uint4(
            uint(product1 >> 32u) ^ counter.y ^ key.x,
            uint(product1),
            uint(product0 >> 32u) ^ counter.w ^ key.y,
            uint(product0));
        if (round != 9u)
        {
            key.x = key.x + SpatialPhiloxW0;
            key.y = key.y + SpatialPhiloxW1;
        }
    }
    return counter;
}

uint spatialRotateLeft(uint value, uint amount)
{
    amount = amount & 31u;
    return amount == 0u ? value : (value << amount) | (value >> (32u - amount));
}

void spatialFoldWord(inout SpatialRandomStream stream, uint word, uint index)
{
    uint lane = index & 3u;
    uint neighbour = (lane + 1u) & 3u;
    stream.counter[lane] = stream.counter[lane] ^ word;
    stream.counter[neighbour] = stream.counter[neighbour]
        + spatialRotateLeft(word, (index * 7u + 5u) & 31u);
    uint keyLane = index & 1u;
    stream.key[keyLane] = stream.key[keyLane] + (word ^ (index * SpatialPhiloxW0));
    stream.counter = spatialPhilox4x32_10(stream.counter, stream.key);
}

uint64_t spatialZigZag64(int64_t value)
{
    uint64_t signMask = value < 0 ? ~uint64_t(0) : uint64_t(0);
    return (uint64_t(value) << 1u) ^ signMask;
}

void spatialFoldU64(inout SpatialRandomStream stream, uint64_t value, inout uint index)
{
    spatialFoldWord(stream, uint(value), index++);
    spatialFoldWord(stream, uint(value >> 32u), index++);
}

void spatialFoldU128(inout SpatialRandomStream stream, uint4 value, inout uint index)
{
    spatialFoldWord(stream, value.x, index++);
    spatialFoldWord(stream, value.y, index++);
    spatialFoldWord(stream, value.z, index++);
    spatialFoldWord(stream, value.w, index++);
}

SpatialRandomStream spatialRandomStream(SpatialRandomDomain domain)
{
    SpatialRandomStream stream;
    stream.counter = uint4(0x53504154u, 0x49414C31u, 0xA511E9B3u, 0x63D835F1u);
    stream.key = uint2(0xC0DECAFEu, 0x9E3779B9u);
    uint index = 0u;
    spatialFoldU128(stream, domain.map, index);
    spatialFoldU128(stream, domain.nodeGuid, index);
    spatialFoldWord(stream, domain.nodeSemanticRevision, index++);
    spatialFoldU128(stream, domain.seedNamespace, index);
    spatialFoldWord(stream, domain.cellLevel, index++);
    spatialFoldU64(stream, spatialZigZag64(domain.cellX), index);
    spatialFoldU64(stream, spatialZigZag64(domain.cellY), index);
    spatialFoldU64(stream, spatialZigZag64(domain.cellZ), index);
    spatialFoldU64(stream, domain.candidate, index);
    spatialFoldU64(stream, domain.ancestor, index);
    spatialFoldU128(stream, domain.species, index);
    spatialFoldWord(stream, domain.channel, index++);
    return stream;
}

uint4 spatialRandomSample(SpatialRandomStream stream, uint64_t sampleIndex)
{
    uint low = uint(sampleIndex);
    uint high = uint(sampleIndex >> 32u);
    uint counter0 = stream.counter.x + low;
    uint carry = counter0 < stream.counter.x ? 1u : 0u;
    uint counter1 = stream.counter.y + high + carry;
    return spatialPhilox4x32_10(
        uint4(counter0, counter1, stream.counter.z, stream.counter.w),
        stream.key);
}

bool spatialChanceFromDraw(uint draw, uint probability)
{
    return uint64_t(draw) * uint64_t(65535u)
        < uint64_t(probability) * (uint64_t(0xFFFFFFFFu) + uint64_t(1u));
}

bool spatialRandomChance(
    SpatialRandomStream stream,
    uint64_t sampleIndex,
    uint lane,
    uint probability)
{
    return spatialChanceFromDraw(spatialRandomSample(stream, sampleIndex)[lane & 3u], probability);
}

uint64_t spatialSignedMagnitude(int64_t value)
{
    return value < 0 ? uint64_t(-(value + 1)) + 1u : uint64_t(value);
}

int64_t spatialDivRoundTiesEven(int64_t numerator, int64_t denominator, out bool valid)
{
    if (denominator == 0)
    {
        valid = false;
        return 0;
    }
    bool negative = (numerator < 0) != (denominator < 0);
    uint64_t numeratorMagnitude = spatialSignedMagnitude(numerator);
    uint64_t denominatorMagnitude = spatialSignedMagnitude(denominator);
    uint64_t quotient = numeratorMagnitude / denominatorMagnitude;
    uint64_t remainder = numeratorMagnitude % denominatorMagnitude;
    bool pastHalf = remainder > denominatorMagnitude - remainder;
    bool exactlyHalf = remainder == denominatorMagnitude - remainder;
    if (pastHalf || (exactlyHalf && (quotient & 1u) != 0u))
    {
        quotient = quotient + 1u;
    }
    uint64_t negativeLimit = uint64_t(1) << 63u;
    uint64_t positiveLimit = negativeLimit - 1u;
    if ((!negative && quotient > positiveLimit) || (negative && quotient > negativeLimit))
    {
        valid = false;
        return 0;
    }
    valid = true;
    if (!negative)
    {
        return int64_t(quotient);
    }
    return quotient == negativeLimit
        ? -int64_t(positiveLimit) - 1
        : -int64_t(quotient);
}

int spatialFixedAdd(int left, int right, out bool valid)
{
    int64_t value = int64_t(left) + int64_t(right);
    valid = value >= -2147483648 && value <= 2147483647;
    return valid ? int(value) : 0;
}

int spatialFixedSub(int left, int right, out bool valid)
{
    int64_t value = int64_t(left) - int64_t(right);
    valid = value >= -2147483648 && value <= 2147483647;
    return valid ? int(value) : 0;
}

int spatialFixedMulQ16(int left, int right, out bool valid)
{
    bool divisionValid;
    int64_t value = spatialDivRoundTiesEven(
        int64_t(left) * int64_t(right),
        int64_t(65536),
        divisionValid);
    valid = divisionValid && value >= -2147483648 && value <= 2147483647;
    return valid ? int(value) : 0;
}

int spatialFixedDivQ16(int numerator, int denominator, out bool valid)
{
    bool divisionValid;
    int64_t value = spatialDivRoundTiesEven(
        int64_t(numerator) * int64_t(65536),
        int64_t(denominator),
        divisionValid);
    valid = divisionValid && value >= -2147483648 && value <= 2147483647;
    return valid ? int(value) : 0;
}

int spatialFixedLerpQ16(int start, int end, uint weight, out bool valid)
{
    if (weight > 65535u)
    {
        valid = false;
        return 0;
    }
    bool divisionValid;
    int64_t weighted = spatialDivRoundTiesEven(
        (int64_t(end) - int64_t(start)) * int64_t(weight),
        int64_t(65535),
        divisionValid);
    int64_t value = int64_t(start) + weighted;
    valid = divisionValid && value >= -2147483648 && value <= 2147483647;
    return valid ? int(value) : 0;
}

int spatialFixedCurveSegmentQ16(
    uint x,
    uint x0,
    uint x1,
    int y0,
    int y1,
    out bool valid)
{
    if (x0 >= x1 || x < x0 || x > x1 || x1 > 65535u)
    {
        valid = false;
        return 0;
    }
    bool divisionValid;
    int64_t weight = spatialDivRoundTiesEven(
        int64_t(x - x0) * int64_t(65535),
        int64_t(x1 - x0),
        divisionValid);
    if (!divisionValid)
    {
        valid = false;
        return 0;
    }
    return spatialFixedLerpQ16(y0, y1, uint(weight), valid);
}
