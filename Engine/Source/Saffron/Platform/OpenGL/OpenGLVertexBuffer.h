#pragma once

#include "Saffron/Rendering/Resources/VertexBuffer.h"

namespace Se
{
class OpenGLVertexBuffer : public VertexBuffer
{
public:
	OpenGLVertexBuffer(void* data, Uint32 size, VertexBufferUsage usage = VertexBufferUsage::Static);
	OpenGLVertexBuffer(Uint32 size, VertexBufferUsage usage = VertexBufferUsage::Dynamic);
	virtual ~OpenGLVertexBuffer();

	void Bind() const override;

	void SetData(const void* data, Uint32 size, Uint32 offset = 0) override;
	void SetData(const Buffer& buffer, Uint32 offset) override;

	const VertexBufferLayout& GetLayout() const override;

	void SetLayout(const VertexBufferLayout& layout) override;

	Uint32 GetSize() const override;

	RendererID GetRendererID() const override;
private:
	RendererID _rendererID = 0;
	Uint32 _size;
	VertexBufferUsage _usage;
	VertexBufferLayout _layout;

	Buffer _localData;
};
}