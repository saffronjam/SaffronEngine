#pragma once

#include "Saffron/Base.h"

namespace Se
{
class BatchLoader : public ReferenceCounted
{
public:
	explicit BatchLoader(String name);

	void Submit(Function<void()> function, String shortDescription);
	void Execute(Function<void()> onFinish = [] {});

	float GetProgress() const { return m_Progress; }
	const String *GetStatus() const { return m_Status; }

	static Unique<BatchLoader> &GetPreloader();
	static void InvalidatePreloader();

private:
	String m_Name;
	ArrayList<std::pair<Function<void()>, String>> m_Queue;
	Atomic<float> m_Progress = 0.0f;
	Atomic<const String *> m_Status = nullptr;
	Mutex m_QueueMutex;
};
}

