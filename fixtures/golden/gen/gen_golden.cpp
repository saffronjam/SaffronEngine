// Standalone golden-fixture generator (13-testing-and-verification phase 2).
//
// Emits the byte-exact reference artifacts the Rust snapshot harness diffs against:
// `cube.smesh`, `cube.sanim`, `material.smat`, `cube.smodel`, the two std430 offset
// maps, and the shm header layout. Disk-format writers reproduce the C++ reference owners; current
// GPU ABI structs mirror their Rust/Slang owners directly. The generator needs no Vulkan/Jolt/SDL.
//
// Sources transcribed:
//   .smesh   geometry.cppm encodeMeshImage / SMeshHeader (:386, :1400)
//   .sanim   geometry.cppm saveAnimationToBuffer / SANimHeader / SANimTrackRecord (:406,:1619)
//   .smodel  geometry.cppm writeContainer / SModelHeader / TocEntry (:296,:386)
//   .smat    assets.cppm materialAssetToJson + dump(2) (:1488,:2137)
//   std430   renderer_types.cppm GpuLight (:2018) and the current
//            gpu_types.rs/material_params.slang MaterialParamsData ABI
//   shm      renderer_capture.cpp recreateShmSegment header (:129)
//
// Build/run in the saffron-build toolbox; see fixtures/golden/gen/PROVENANCE.md.

#include <cstdint>
#include <cstring>
#include <cstdio>
#include <fstream>
#include <span>
#include <string>
#include <vector>

#include <nlohmann/json.hpp>

using u8 = std::uint8_t;
using u16 = std::uint16_t;
using u32 = std::uint32_t;
using u64 = std::uint64_t;
using i32 = std::int32_t;
using f32 = float;

namespace {

// ---- geometry types (geometry.cppm :36) ----

struct Vec2 { f32 x, y; };
struct Vec3 { f32 x, y, z; };
struct Vec4 { f32 x, y, z, w; };

struct Vertex {
    Vec3 position{};
    Vec3 normal{};
    Vec2 uv0{};
};
static_assert(sizeof(Vertex) == 32);

struct Submesh {
    u32 firstIndex = 0;
    u32 indexCount = 0;
    i32 vertexOffset = 0;
    u32 materialSlot = 0;
};
static_assert(sizeof(Submesh) == 16);

struct VertexSkin {
    u16 joints[4] = {0, 0, 0, 0};
    f32 weights[4] = {0, 0, 0, 0};
};
static_assert(sizeof(VertexSkin) == 24);

struct Mesh {
    std::vector<Vertex> vertices;
    std::vector<u32> indices;
    std::vector<Submesh> submeshes;
};

// ---- .smesh header (geometry.cppm :386) ----

struct SMeshHeader {
    char magic[4];
    u32 version;
    u32 flags;
    u32 vertexStride;
    u32 vertexCount;
    u32 indexCount;
    u32 indexWidth;
    u32 submeshCount;
    u64 verticesOffset;
    u64 indicesOffset;
    u64 submeshesOffset;
    u32 reserved[2];
};
static_assert(sizeof(SMeshHeader) == 64);

constexpr u32 MeshFormatVersion = 1;
constexpr u32 MeshFormatVersionSkinned = 2;

// encodeMeshImage (geometry.cppm :1400) — an empty skin yields v1.
auto encodeMeshImage(const Mesh& mesh, const std::vector<VertexSkin>& skin) -> std::vector<u8> {
    SMeshHeader header{};
    std::memcpy(header.magic, "SMSH", 4);
    header.version = skin.empty() ? MeshFormatVersion : MeshFormatVersionSkinned;
    header.flags = 0;
    header.vertexStride = sizeof(Vertex);
    header.vertexCount = static_cast<u32>(mesh.vertices.size());
    header.indexCount = static_cast<u32>(mesh.indices.size());
    header.indexWidth = sizeof(u32);
    header.submeshCount = static_cast<u32>(mesh.submeshes.size());
    header.verticesOffset = sizeof(SMeshHeader);
    header.indicesOffset = header.verticesOffset + static_cast<u64>(header.vertexCount) * sizeof(Vertex);
    header.submeshesOffset = header.indicesOffset + static_cast<u64>(header.indexCount) * sizeof(u32);
    const u64 submeshesEnd = header.submeshesOffset + static_cast<u64>(header.submeshCount) * sizeof(Submesh);
    u64 total = submeshesEnd;
    if (!skin.empty()) {
        total += static_cast<u64>(skin.size()) * sizeof(VertexSkin);
    }
    std::vector<u8> bytes(static_cast<std::size_t>(total));
    auto put = [&](u64 off, const void* src, std::size_t count) {
        if (count != 0) std::memcpy(bytes.data() + off, src, count);
    };
    put(0, &header, sizeof(header));
    put(header.verticesOffset, mesh.vertices.data(), mesh.vertices.size() * sizeof(Vertex));
    put(header.indicesOffset, mesh.indices.data(), mesh.indices.size() * sizeof(u32));
    put(header.submeshesOffset, mesh.submeshes.data(), mesh.submeshes.size() * sizeof(Submesh));
    if (!skin.empty()) {
        put(submeshesEnd, skin.data(), skin.size() * sizeof(VertexSkin));
    }
    return bytes;
}

// ---- .sanim (geometry.cppm :406, :1619) ----

struct SANimHeader {
    char magic[4];
    u32 version;
    u32 trackCount;
    f32 duration;
    u32 nameLen;
    u32 reserved[3];
};
static_assert(sizeof(SANimHeader) == 32);

struct SANimTrackRecord {
    i32 joint;
    u8 path;
    u8 interp;
    u16 pad;
    u32 nameLen;
    u32 timeCount;
    u32 valueCount;
};
static_assert(sizeof(SANimTrackRecord) == 20);

enum class AnimPath : u8 { Translation, Rotation, Scale };
enum class AnimInterp : u8 { Step, Linear, CubicSpline };

struct AnimTrack {
    i32 joint = -1;
    std::string jointName;
    AnimPath path = AnimPath::Translation;
    AnimInterp interp = AnimInterp::Linear;
    std::vector<f32> times;
    std::vector<f32> values;
};

struct AnimClip {
    std::string name;
    f32 duration = 0.0f;
    std::vector<AnimTrack> tracks;
};

constexpr u32 AnimFormatVersion = 1;

auto saveAnimationToBuffer(const AnimClip& clip) -> std::vector<u8> {
    std::vector<u8> bytes;
    auto append = [&](const void* src, std::size_t count) {
        const auto* first = reinterpret_cast<const u8*>(src);
        bytes.insert(bytes.end(), first, first + count);
    };
    SANimHeader header{};
    std::memcpy(header.magic, "SANM", 4);
    header.version = AnimFormatVersion;
    header.trackCount = static_cast<u32>(clip.tracks.size());
    header.duration = clip.duration;
    header.nameLen = static_cast<u32>(clip.name.size());
    append(&header, sizeof(header));
    append(clip.name.data(), clip.name.size());
    for (const AnimTrack& track : clip.tracks) {
        SANimTrackRecord record{};
        record.joint = track.joint;
        record.path = static_cast<u8>(track.path);
        record.interp = static_cast<u8>(track.interp);
        record.nameLen = static_cast<u32>(track.jointName.size());
        record.timeCount = static_cast<u32>(track.times.size());
        record.valueCount = static_cast<u32>(track.values.size());
        append(&record, sizeof(record));
        append(track.jointName.data(), track.jointName.size());
        append(track.times.data(), track.times.size() * sizeof(f32));
        append(track.values.data(), track.values.size() * sizeof(f32));
    }
    return bytes;
}

// ---- .smodel (geometry.cppm :296, writeContainer) ----

struct SModelHeader {
    char magic[4];
    u32 containerVersion;
    u32 schemaVersion;
    u32 flags;
    u32 tocCount;
    u32 reserved0;
    u64 tocOffset;
    u64 metaOffset;
    u64 metaLength;
    u64 totalLength;
    u32 reserved[2];
};
static_assert(sizeof(SModelHeader) == 64);

struct TocEntry {
    u32 fourcc;
    u32 flags;
    u64 subId;
    u64 offset;
    u64 length;
};
static_assert(sizeof(TocEntry) == 32);

constexpr u32 ContainerFormatVersion = 1;
constexpr u32 MetadataSchemaVersion = 1;

constexpr auto fourcc(const char tag[4]) -> u32 {
    return static_cast<u32>(static_cast<u8>(tag[0])) |
           (static_cast<u32>(static_cast<u8>(tag[1])) << 8) |
           (static_cast<u32>(static_cast<u8>(tag[2])) << 16) |
           (static_cast<u32>(static_cast<u8>(tag[3])) << 24);
}

enum class ChunkKind : u32 {
    Meta = fourcc("META"),
    Mesh = fourcc("MESH"),
    Texture = fourcc("STEX"),
    Material = fourcc("SMAT"),
    Animation = fourcc("SANM"),
    Thumbnail = fourcc("THMB"),
};

struct ContainerChunk {
    ChunkKind kind;
    u64 subId = 0;
    u32 flags = 0;
    std::span<const u8> bytes;
};

constexpr auto align16(u64 v) -> u64 { return (v + 15) & ~static_cast<u64>(15); }

auto writeContainerBytes(std::span<const ContainerChunk> chunks) -> std::vector<u8> {
    std::vector<const ContainerChunk*> ordered;
    for (const auto& c : chunks)
        if (c.kind == ChunkKind::Meta) ordered.push_back(&c);
    for (const auto& c : chunks)
        if (c.kind != ChunkKind::Meta) ordered.push_back(&c);

    const u64 tocOffset = sizeof(SModelHeader);
    const u64 tocBytes = static_cast<u64>(ordered.size()) * sizeof(TocEntry);

    std::vector<TocEntry> toc(ordered.size());
    u64 cursor = align16(tocOffset + tocBytes);
    u64 metaOffset = 0, metaLength = 0;
    for (std::size_t i = 0; i < ordered.size(); ++i) {
        cursor = align16(cursor);
        toc[i].fourcc = static_cast<u32>(ordered[i]->kind);
        toc[i].flags = ordered[i]->flags;
        toc[i].subId = ordered[i]->subId;
        toc[i].offset = cursor;
        toc[i].length = static_cast<u64>(ordered[i]->bytes.size());
        if (ordered[i]->kind == ChunkKind::Meta) {
            metaOffset = toc[i].offset;
            metaLength = toc[i].length;
        }
        cursor += toc[i].length;
    }
    const u64 totalLength = cursor;

    SModelHeader header{};
    std::memcpy(header.magic, "SMDL", 4);
    header.containerVersion = ContainerFormatVersion;
    header.schemaVersion = MetadataSchemaVersion;
    header.flags = 0;
    header.tocCount = static_cast<u32>(ordered.size());
    header.reserved0 = 0;
    header.tocOffset = tocOffset;
    header.metaOffset = metaOffset;
    header.metaLength = metaLength;
    header.totalLength = totalLength;

    std::vector<u8> buffer(static_cast<std::size_t>(totalLength), 0);
    std::memcpy(buffer.data(), &header, sizeof(header));
    if (!toc.empty())
        std::memcpy(buffer.data() + tocOffset, toc.data(), static_cast<std::size_t>(tocBytes));
    for (std::size_t i = 0; i < ordered.size(); ++i)
        if (!ordered[i]->bytes.empty())
            std::memcpy(buffer.data() + toc[i].offset, ordered[i]->bytes.data(), ordered[i]->bytes.size());
    return buffer;
}

// ---- .smat (assets.cppm :1488, materialAssetToJson + dump(2)) ----

struct MaterialAsset {
    std::string shader = "mesh";
    std::string blend = "opaque";
    bool unlit = false;
    bool doubleSided = false;
    std::string normalConvention = "gl";
    Vec4 baseColor{1, 1, 1, 1};
    f32 metallic = 0.0f;
    f32 roughness = 1.0f;
    Vec3 emissive{0, 0, 0};
    f32 emissiveStrength = 1.0f;
    f32 normalStrength = 1.0f;
    f32 alphaCutoff = 0.5f;
    f32 heightScale = 0.05f;
    Vec2 uvTiling{1, 1};
    Vec2 uvOffset{0, 0};
    u64 albedoTexture = 0;
    u64 ormTexture = 0;
    u64 normalTexture = 0;
    u64 emissiveTexture = 0;
    u64 heightTexture = 0;
    nlohmann::json graph = nlohmann::json{};
    u64 parent = 0;
    nlohmann::json overrides = nlohmann::json{};
};

auto materialAssetToJson(const MaterialAsset& m) -> nlohmann::json {
    const auto u = [](u64 id) { return std::to_string(id); };
    return nlohmann::json{
        {"version", 1},
        {"shader", m.shader},
        {"blend", m.blend},
        {"unlit", m.unlit},
        {"doubleSided", m.doubleSided},
        {"normalConvention", m.normalConvention},
        {"factors",
         {{"baseColor", {m.baseColor.x, m.baseColor.y, m.baseColor.z, m.baseColor.w}},
          {"metallic", m.metallic},
          {"roughness", m.roughness},
          {"emissive", {m.emissive.x, m.emissive.y, m.emissive.z}},
          {"emissiveStrength", m.emissiveStrength},
          {"normalStrength", m.normalStrength},
          {"alphaCutoff", m.alphaCutoff},
          {"heightScale", m.heightScale},
          {"uvTiling", {m.uvTiling.x, m.uvTiling.y}},
          {"uvOffset", {m.uvOffset.x, m.uvOffset.y}}}},
        {"textures",
         {{"albedo", u(m.albedoTexture)},
          {"ormOrMr", u(m.ormTexture)},
          {"normal", u(m.normalTexture)},
          {"emissive", u(m.emissiveTexture)},
          {"height", u(m.heightTexture)}}},
        {"graph", m.graph.is_null() ? nlohmann::json::object() : m.graph},
        {"parent", std::to_string(m.parent)},
        {"overrides", m.overrides.is_null() ? nlohmann::json::object() : m.overrides}};
}

// ---- std430 GPU structs (canonical Rust/Slang ABI, plus C++ reference structs) ----

struct UVec4 { u32 x, y, z, w; };

struct MaterialParamsData {
    Vec4 baseColor;
    Vec4 pbr;
    Vec4 emissive;
    Vec4 uv;
    UVec4 tex0;
    UVec4 tex1;
    Vec4 thinReflection;
    Vec4 thinAbsorption;
    Vec4 thinTransmission;
    UVec4 coverage;
    UVec4 coverageHash;
    Vec4 aggregate0;
    Vec4 aggregateAlbedo;
    Vec4 aggregateTransmission;
    Vec4 aggregateNormal0;
    Vec4 aggregateNormal1;
};
static_assert(sizeof(MaterialParamsData) == 256);

struct GpuLight {
    Vec4 positionRange;
    Vec4 colorIntensity;
    Vec4 directionType;
    Vec4 spotCos;
};
static_assert(sizeof(GpuLight) == 64);

// ---- shm header (renderer_capture.cpp :129) ----

constexpr u32 ShmMagic = 0x53465632;  // "SFV2"
constexpr u32 ShmRingSlots = 4;
constexpr u64 MinShmSlotCapacity = static_cast<u64>(3840) * 2160 * 4;

// ---- canonical fixture inputs ----

// A unit cube centered at the origin: 24 vertices (per-face normals + uvs), 36 indices,
// one submesh. Deterministic, reproduced identically by the Rust fixture builder.
auto cubeMesh() -> Mesh {
    struct Face { Vec3 n; Vec3 c[4]; };
    const Face faces[6] = {
        {{0, 0, 1}, {{-1, -1, 1}, {1, -1, 1}, {1, 1, 1}, {-1, 1, 1}}},     // +Z
        {{0, 0, -1}, {{1, -1, -1}, {-1, -1, -1}, {-1, 1, -1}, {1, 1, -1}}}, // -Z
        {{1, 0, 0}, {{1, -1, 1}, {1, -1, -1}, {1, 1, -1}, {1, 1, 1}}},      // +X
        {{-1, 0, 0}, {{-1, -1, -1}, {-1, -1, 1}, {-1, 1, 1}, {-1, 1, -1}}}, // -X
        {{0, 1, 0}, {{-1, 1, 1}, {1, 1, 1}, {1, 1, -1}, {-1, 1, -1}}},      // +Y
        {{0, -1, 0}, {{-1, -1, -1}, {1, -1, -1}, {1, -1, 1}, {-1, -1, 1}}}, // -Y
    };
    const Vec2 uv[4] = {{0, 0}, {1, 0}, {1, 1}, {0, 1}};
    Mesh mesh;
    for (const auto& f : faces) {
        const u32 base = static_cast<u32>(mesh.vertices.size());
        for (int i = 0; i < 4; ++i)
            mesh.vertices.push_back(Vertex{f.c[i], f.n, uv[i]});
        mesh.indices.insert(mesh.indices.end(),
                            {base + 0, base + 1, base + 2, base + 0, base + 2, base + 3});
    }
    mesh.submeshes.push_back(Submesh{0, static_cast<u32>(mesh.indices.size()), 0, 0});
    return mesh;
}

auto cubeClip() -> AnimClip {
    AnimClip clip;
    clip.name = "CubeSpin";
    clip.duration = 2.0f;
    clip.tracks.push_back(AnimTrack{
        0, "Root", AnimPath::Rotation, AnimInterp::Linear,
        {0.0f, 1.0f, 2.0f},
        {0, 0, 0, 1, 0, 0.70710677f, 0, 0.70710677f, 0, 1, 0, 0}});
    clip.tracks.push_back(AnimTrack{
        1, "Lid", AnimPath::Translation, AnimInterp::Step,
        {0.0f, 2.0f},
        {0, 0, 0, 0, 0.5f, 0}});
    return clip;
}

auto populatedMaterial() -> MaterialAsset {
    MaterialAsset m;
    m.shader = "mesh";
    m.blend = "masked";
    m.unlit = false;
    m.doubleSided = true;
    m.normalConvention = "gl";
    m.baseColor = Vec4{0.8f, 0.4f, 0.2f, 1.0f};
    m.metallic = 0.25f;
    m.roughness = 0.7f;
    m.emissive = Vec3{0.1f, 0.0f, 0.0f};
    m.emissiveStrength = 2.0f;
    m.normalStrength = 1.0f;
    m.alphaCutoff = 0.5f;
    m.heightScale = 0.05f;
    m.uvTiling = Vec2{2.0f, 2.0f};
    m.uvOffset = Vec2{0.0f, 0.0f};
    m.albedoTexture = 4242;
    m.ormTexture = 0;
    m.normalTexture = 4243;
    m.emissiveTexture = 0;
    m.heightTexture = 0;
    m.parent = 1024;
    return m;
}

// ---- known-valued std430 instances + offset-map emission ----

auto hexdumpBytes(std::span<const u8> p) -> std::string {
    std::string out;
    char buf[4];
    for (std::size_t i = 0; i < p.size(); ++i) {
        if (i != 0 && i % 16 == 0) out += "\n";
        std::snprintf(buf, sizeof(buf), "%02x ", p[i]);
        out += buf;
    }
    out += "\n";
    return out;
}

template <typename T>
auto hexdump(const T& value) -> std::string {
    return hexdumpBytes(std::span<const u8>(reinterpret_cast<const u8*>(&value), sizeof(T)));
}

auto knownMaterialParams() -> MaterialParamsData {
    MaterialParamsData d{};
    d.baseColor = Vec4{0.8f, 0.4f, 0.2f, 1.0f};
    d.pbr = Vec4{0.25f, 0.7f, 1.0f, 0.5f};
    d.emissive = Vec4{0.1f, 0.0f, 0.0f, 0.05f};
    d.uv = Vec4{2.0f, 2.0f, 0.0f, 0.0f};
    d.tex0 = UVec4{3, 0, 5, 0};
    d.tex1 = UVec4{0, 0, 0, 7};
    d.thinReflection = Vec4{0.4f, 0.3f, 0.6f, 0.001f};
    d.thinAbsorption = Vec4{0.2f, 0.3f, 0.4f, 0.9f};
    d.thinTransmission = Vec4{0.7f, 0.8f, 0.5f, 0.0f};
    d.coverage = UVec4{11, 1, 1, 2};
    d.coverageHash = UVec4{13, 17, 1024, 512};
    d.aggregate0 = Vec4{0.5f, 0.7f, 0.002f, 0.0f};
    d.aggregateAlbedo = Vec4{0.2f, 0.5f, 0.1f, 0.0f};
    d.aggregateTransmission = Vec4{0.4f, 0.6f, 0.3f, 0.0f};
    d.aggregateNormal0 = Vec4{0.5f, 0.4f, 0.3f, 0.1f};
    d.aggregateNormal1 = Vec4{0.2f, 0.05f, 0.0f, 0.0f};
    return d;
}

auto knownGpuLight() -> GpuLight {
    GpuLight d{};
    d.positionRange = Vec4{1.0f, 2.0f, 3.0f, 10.0f};
    d.colorIntensity = Vec4{1.0f, 0.5f, 0.25f, 4.0f};
    d.directionType = Vec4{0.0f, -1.0f, 0.0f, 1.0f};
    d.spotCos = Vec4{0.9f, 0.7f, 0.0f, 0.0f};
    return d;
}

auto shmHeaderBytes() -> std::vector<u8> {
    // recreateShmSegment writes [magic, width, height, seq, ringSlots, slotCapacity, 0, 0]
    // at creation: width/height/seq = 0 (no frame yet), capacity floored at MinShmSlotCapacity.
    u32 header[8] = {ShmMagic, 0, 0, 0, ShmRingSlots, static_cast<u32>(MinShmSlotCapacity), 0, 0};
    std::vector<u8> out(32);
    std::memcpy(out.data(), header, 32);
    return out;
}

void writeFile(const std::string& path, std::span<const u8> bytes) {
    std::ofstream out(path, std::ios::binary);
    out.write(reinterpret_cast<const char*>(bytes.data()), static_cast<std::streamsize>(bytes.size()));
    std::printf("wrote %s (%zu bytes)\n", path.c_str(), bytes.size());
}

void writeText(const std::string& path, const std::string& text) {
    std::ofstream out(path, std::ios::binary);
    out.write(text.data(), static_cast<std::streamsize>(text.size()));
    std::printf("wrote %s (%zu bytes)\n", path.c_str(), text.size());
}

}  // namespace

int main(int argc, char** argv) {
    const std::string dir = argc > 1 ? argv[1] : ".";

    const Mesh mesh = cubeMesh();
    writeFile(dir + "/cube.smesh", encodeMeshImage(mesh, {}));

    const AnimClip clip = cubeClip();
    const std::vector<u8> clipBytes = saveAnimationToBuffer(clip);
    writeFile(dir + "/cube.sanim", clipBytes);

    const MaterialAsset mat = populatedMaterial();
    const std::string smat = materialAssetToJson(mat).dump(2);
    writeText(dir + "/material.smat", smat);

    // A self-contained .smodel: a small fixed META JSON + the cube MESH chunk.
    const nlohmann::json meta = nlohmann::json{
        {"schemaVersion", 1},
        {"name", "cube"},
        {"meshSubId", 1},
        {"materialCount", 0}};
    const std::string metaStr = meta.dump(2);
    const std::vector<u8> metaBytes(metaStr.begin(), metaStr.end());
    const std::vector<u8> meshBytes = encodeMeshImage(mesh, {});
    const ContainerChunk chunks[2] = {
        ContainerChunk{ChunkKind::Meta, 0, 0,
                       std::span<const u8>(metaBytes.data(), metaBytes.size())},
        ContainerChunk{ChunkKind::Mesh, 1, 0,
                       std::span<const u8>(meshBytes.data(), meshBytes.size())},
    };
    writeFile(dir + "/cube.smodel", writeContainerBytes(std::span<const ContainerChunk>(chunks, 2)));

    // std430 offset maps: a header line per field offset, then a full known-valued hexdump.
    {
        const MaterialParamsData d = knownMaterialParams();
        std::string s = "struct MaterialParamsData size=256 align=16\n";
        s += "offset baseColor 0\noffset pbr 16\noffset emissive 32\noffset uv 48\n";
        s += "offset tex0 64\noffset tex1 80\n";
        s += "offset thinReflection 96\noffset thinAbsorption 112\noffset thinTransmission 128\n";
        s += "offset coverage 144\noffset coverageHash 160\noffset aggregate0 176\n";
        s += "offset aggregateAlbedo 192\noffset aggregateTransmission 208\n";
        s += "offset aggregateNormal0 224\noffset aggregateNormal1 240\n";
        s += "hexdump:\n" + hexdump(d);
        writeText(dir + "/material_params_data.offsets", s);
    }
    {
        const GpuLight d = knownGpuLight();
        std::string s = "struct GpuLight size=64 align=16\n";
        s += "offset positionRange 0\noffset colorIntensity 16\noffset directionType 32\noffset spotCos 48\n";
        s += "hexdump:\n" + hexdump(d);
        writeText(dir + "/gpu_light.offsets", s);
    }

    // shm header layout golden: the 32-byte header words at segment creation.
    {
        const std::vector<u8> hdr = shmHeaderBytes();
        std::string s = "shm header SFV2 32 bytes, 8 u32 words native-endian\n";
        s += "word 0 magic 0x53465632\nword 1 width 0\nword 2 height 0\nword 3 seq 0\n";
        s += "word 4 ringSlots 4\nword 5 slotCapacity 33177600\nword 6 reserved 0\nword 7 reserved 0\n";
        s += "hexdump:\n" + hexdumpBytes(std::span<const u8>(hdr.data(), hdr.size()));
        writeText(dir + "/shm_header.layout", s);
    }

    return 0;
}
