﻿#pragma once

#include "Saffron/Base.h"
#include "Saffron/Rendering/Bindables.h"

namespace Se
{
struct MeshVertex
{
	Vector3 Position;
	Vector3 Normal;
	Vector3 Tangent;
	Vector3 Binormal;
	Vector2 TexCoord;

	static auto VertexLayout() -> class VertexLayout
	{
		return {
			{"Position", ElementType::Float3}, {"Normal", ElementType::Float3}, {"Tangent", ElementType::Float3},
			{"Binormal", ElementType::Float3}, {"TexCoord", ElementType::Float2},
		};
	}
};

struct MeshFace
{
	uint x, y, z;
};

struct SubMesh
{
	uint BaseVertex;
	uint BaseIndex;
	uint MaterialIndex;
	uint IndexCount;
	uint VertexCount;

	Matrix Transform;

	std::string NodeName, MeshName;
};

class Mesh
{
	friend class MeshStore;
public:
	void Bind();

	auto SubMeshes() const -> const std::vector<SubMesh>&;

	static auto Create(const std::filesystem::path& path) -> std::shared_ptr<Mesh>;

private:
	Mesh();

private:
	std::shared_ptr<VertexBuffer> _vertexBuffer;
	std::shared_ptr<IndexBuffer> _indexBuffer;
	std::shared_ptr<InputLayout> _inputLayout;
	std::shared_ptr<VertexShader> _vertexShader;
	std::shared_ptr<PixelShader> _pixelShader;
	std::vector<SubMesh> _subMeshes;
};
}