// Byte-locked std430 material record shared by every geometry pass.

public struct MaterialParams
{
    public float4 baseColor;
    public float4 pbr;
    public float4 emissive;
    public float4 uv;
    public uint4 tex0;
    public uint4 tex1;
    public float4 thinReflection;
    public float4 thinAbsorption;
    public float4 thinTransmission;
    public uint4 coverage;
    public uint4 coverageHash;
    public float4 aggregate0;
    public float4 aggregateAlbedo;
    public float4 aggregateTransmission;
    public float4 aggregateNormal0;
    public float4 aggregateNormal1;
};

public static const uint FEATURE_NORMAL = 1u;
public static const uint FEATURE_EMISSIVE_TEX = 2u;
public static const uint FEATURE_OCCLUSION = 4u;
public static const uint FEATURE_HEIGHT = 8u;
public static const uint FEATURE_ALPHACLIP = 16u;
public static const uint FEATURE_DISPLACE = 32u;
public static const uint FEATURE_HEIGHT_BUMP = 64u;
public static const uint FEATURE_THIN_SHEET = 128u;

public static const uint SURFACE_STANDARD = 0u;
public static const uint SURFACE_THIN_SHEET = 1u;
public static const uint THIN_NORMAL_PRESERVE = 0u;
public static const uint THIN_NORMAL_FACE_FORWARD_BACK = 1u;
public static const uint THIN_NORMAL_SYMMETRIC = 2u;
