static const float SKY_SH_PI = 3.14159265359;

void skyShBasis(float3 direction, out float basis[9])
{
    float3 d = normalize(direction);
    basis[0] = 0.282095;
    basis[1] = 0.488603 * d.y;
    basis[2] = 0.488603 * d.z;
    basis[3] = 0.488603 * d.x;
    basis[4] = 1.092548 * d.x * d.y;
    basis[5] = 1.092548 * d.y * d.z;
    basis[6] = 0.315392 * (3.0 * d.z * d.z - 1.0);
    basis[7] = 1.092548 * d.x * d.z;
    basis[8] = 0.546274 * (d.x * d.x - d.y * d.y);
}

float3 skyShRadiance(float3 direction, StructuredBuffer<float4> coefficients)
{
    float basis[9];
    skyShBasis(direction, basis);
    float3 value = float3(0.0);
    for (uint i = 0; i < 9; i = i + 1)
    {
        value += coefficients[i].rgb * basis[i];
    }
    return max(value, float3(0.0));
}

float3 skyShIrradiance(float3 normal, StructuredBuffer<float4> coefficients)
{
    float basis[9];
    skyShBasis(normal, basis);
    float3 value = coefficients[0].rgb * basis[0] * SKY_SH_PI;
    for (uint i = 1; i <= 3; i = i + 1)
    {
        value += coefficients[i].rgb * basis[i] * (2.0 * SKY_SH_PI / 3.0);
    }
    for (uint i = 4; i < 9; i = i + 1)
    {
        value += coefficients[i].rgb * basis[i] * (SKY_SH_PI / 4.0);
    }
    return max(value, float3(0.0));
}
