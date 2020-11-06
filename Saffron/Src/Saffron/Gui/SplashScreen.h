#pragma once

#include "Saffron/Base.h"
#include "Saffron/Core/BatchLoader.h"
#include "Saffron/Renderer/Texture.h"

namespace Se
{
class SplashScreen
{
public:
	SplashScreen();

	void OnGuiRender();

	void Show();
	void Hide();
	bool IsIdle() const;
	bool IsFinished() const;

private:
	const Unique<BatchLoader> &m_BatchLoader;
	Shared<Texture2D> m_Texture;
	bool m_Hidden = false;
	String m_FinalizingStatus;

	float m_GoalProgressView = 0.0f;
	float m_CurrentProgressView = 0.0f;
	float m_CurrentSinTimer = 0.0f;
};
}

