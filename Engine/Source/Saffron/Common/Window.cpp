﻿#include "SaffronPCH.h"

#include "Saffron/Common/Window.h"
#include "Saffron/Platform/Windows/WindowsWindow.h"

namespace Se
{
Window::Window(const WindowSpec& spec) :
	_spec(spec)
{
}

void Window::OnUpdate()
{
}

void Window::PushEvent(const Event& event)
{
	_events.push_back(event);
}

void Window::DispatchEvents()
{
	for (auto& event : _events)
	{
		switch (event.Type)
		{
		case EventType::Closed:
		{
			Closed.Invoke();
			break;
		}
		case EventType::Resized:
		{
			Resized.Invoke(event.Size);
			break;
		}
		case EventType::LostFocus:
		{
			LostFocus.Invoke();
			break;
		}
		case EventType::GainedFocus:
		{
			GainedFocus.Invoke();
			break;
		}
		case EventType::TextEntered:
		{
			break;
		}
		case EventType::KeyPressed:
		{
			break;
		}
		case EventType::KeyReleased:
		{
			break;
		}
		case EventType::MouseWheelScrolled:
		{
			break;
		}
		case EventType::MouseButtonPressed:
		{
			break;
		}
		case EventType::MouseButtonReleased:
		{
			break;
		}
		case EventType::MouseMoved:
		{
			break;
		}
		case EventType::MouseEntered:
		{
			break;
		}
		case EventType::MouseLeft:
		{
			break;
		}
		case EventType::Count:
		{
			break;
		}
		}
	}
	_events.clear();
}

auto Window::Width() const -> uint
{
	return _spec.Width;
}

auto Window::Height() const -> uint
{
	return _spec.Height;
}

auto Window::Create(const WindowSpec& spec) -> std::unique_ptr<Window>
{
#ifdef SE_PLATFORM_WINDOWS
	return std::make_unique<WindowsWindow>(spec);
#else
	return nullptr;
#endif
}
}
