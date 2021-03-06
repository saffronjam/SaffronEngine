﻿#pragma once

#include "Saffron/Base.h"
#include "Saffron/Renderer/PrimitiveType.h"

namespace Se
{
using RendererID = Uint32;

class RendererAPI
{
public:
	enum class Type
	{
		None = 0, OpenGL = 1
	};

	struct Capabilities
	{
		String Vendor;
		String Renderer;
		String Version;

		int MaxSamples = 0;
		float MaxAnisotropy = 0.0f;
		int MaxTextureUnits = 0;
	};

public:
	virtual ~RendererAPI() = default;

	static void Init();
	static void Shutdown();

	static void Clear(float r, float g, float b, float a);
	static void DrawIndexed(Uint32 count, PrimitiveType type, bool depthTest = true);

	static Capabilities &GetCapabilities() { return m_sCapabilities; }
	static void SetLineThickness(float thickness);
	static void SetClearColor(float r, float g, float b, float a);

	static Type Current() { return m_sCurrentAPI; }

private:
	static Type m_sCurrentAPI;
	static Capabilities m_sCapabilities;
	static float m_LineThickness;
};

//RendererAPI::Capabilities RendererAPI::m_sCapabilities = {};

}
