﻿#pragma once

#include "Saffron/Base.h"
#include "Saffron/Rendering/RenderGraph.h"

namespace Se
{
class RenderPass
{
public:
	virtual ~RenderPass() = default;

	virtual void Execute(const RenderGraph& rendergraph) = 0;
};
}