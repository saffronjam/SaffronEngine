﻿#pragma once

#include <chrono>

#include "Saffron/System/Time.h"

namespace Se
{
class Timer
{
public:
	using TimePoint = std::chrono::steady_clock::time_point;

public:
	explicit Timer(const char *name = "Unnamed");

	Time Mark();
	Time Peek() const;
	Time PeekTotal() const;

	TimePoint GetStart() const;
	TimePoint GetInitialStart() const;

protected:
	const char *m_Name;
	TimePoint m_LastTimePoint;
	TimePoint m_InitialTimePoint;
};
}
