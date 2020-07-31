﻿#include "Saffron/SaffronPCH.h"

#include <glad/glad.h>

#include "Platform/OpenGL/OpenGLRendererAPI.h"

namespace Se
{

void OpenGLMessageCallback(
	unsigned source,
	unsigned type,
	unsigned id,
	unsigned severity,
	int length,
	const char *message,
	const void *userParam)
{
	switch ( severity )
	{
	case GL_DEBUG_SEVERITY_HIGH:			SE_CORE_CRITICAL(message); return;
	case GL_DEBUG_SEVERITY_MEDIUM:			SE_CORE_ERROR(message); return;
	case GL_DEBUG_SEVERITY_LOW:				SE_CORE_WARN(message); return;
	case GL_DEBUG_SEVERITY_NOTIFICATION:	SE_CORE_TRACE(message); return;
	default:								SE_CORE_ASSERT(false, "Unknown severity level!");
	}
}

void OpenGLRendererAPI::Init()
{
	//SE_PROFILE_FUNCTION();

#ifdef SE_DEBUG
	glEnable(GL_DEBUG_OUTPUT);
	glEnable(GL_DEBUG_OUTPUT_SYNCHRONOUS);
	glDebugMessageCallback(OpenGLMessageCallback, nullptr);

	glDebugMessageControl(GL_DONT_CARE, GL_DONT_CARE, GL_DEBUG_SEVERITY_NOTIFICATION, 0, nullptr, GL_FALSE);
#endif

	glEnable(GL_BLEND);
	glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);

	glEnable(GL_DEPTH_TEST);
}

void OpenGLRendererAPI::SetViewport(Uint32 x, Uint32 y, Uint32 width, Uint32 height)
{
	glViewport(x, y, width, height);
}

void OpenGLRendererAPI::SetClearColor(const glm::vec4 &color)
{
	glClearColor(color.r, color.g, color.b, color.a);
}

void OpenGLRendererAPI::Clear()
{
	glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
}

void OpenGLRendererAPI::DrawIndexed(const Ref<VertexArray> &vertexArray, Uint32 indexCount)
{
	const Uint32 count = indexCount ? indexCount : vertexArray->GetIndexBuffer()->GetCount();
	glDrawElements(GL_TRIANGLES, count, GL_UNSIGNED_INT, nullptr);
	glBindTexture(GL_TEXTURE_2D, 0);
}
}