#pragma once

#include <imgui.h>
#include <imgui_internal.h>
#include "ImGuizmo.h"

#include "Saffron/Base.h"
#include "Saffron/Math/SaffronMath.h"

namespace Se
{
using Font = ImFont;

class Gui : public Instansiated<Gui>
{
public:
	enum class Style : int
	{
		DarkColorful,
		Dark,
		Light
	};

	enum class PropertyFlag
	{
		None = 0,
		Color = 1,
		Drag = 2,
		Slider = 4
	};

public:
	Gui();
	~Gui();

	void Begin();
	void End();

	static void BeginPropertyGrid(float width = -1.0);
	static void EndPropertyGrid();

	static bool BeginTreeNode(const String& label, bool defaultOpen = true);
	static void EndTreeNode();

	static void Property(const String& label, const Function<void()>& onClick, bool secondColumn = false);
	static void Property(const String& label, const String& value);
	static bool Property(const String& label, String& value, bool error = false);
	static bool Property(const String& label, bool& value);
	static bool Property(const String& label, const String& text, const String& buttonName,
	                     const Function<void()>& onButtonPress);
	static bool Property(const String& label, int& value, int min = -1, int max = 1, float step = 1,
	                     PropertyFlag flags = PropertyFlag::None);
	static bool Property(const String& label, float& value, float min = -1.0f, float max = 1.0f, float step = 1.0f,
	                     PropertyFlag flags = PropertyFlag::None);
	static bool Property(const String& label, Vector2f& value, PropertyFlag flags);
	static bool Property(const String& label, Vector2f& value, float min = -1.0f, float max = 1.0f, float step = 1.0f,
	                     PropertyFlag flags = PropertyFlag::None);
	static bool Property(const String& label, Vector3f& value, PropertyFlag flags);
	static bool Property(const String& label, Vector3f& value, float min = -1.0f, float max = 1.0f, float step = 1.0f,
	                     PropertyFlag flags = PropertyFlag::None);
	static bool Property(const String& label, Vector4f& value, PropertyFlag flags);
	static bool Property(const String& label, Vector4f& value, float min = -1.0f, float max = 1.0f, float step = 1.0f,
	                     PropertyFlag flags = PropertyFlag::None);

	static void HelpMarker(const String& desc);
	static void InfoModal(const char* title, const char* text, bool& open);

	static int GetFontSize();
	static void SetStyle(Style style);
	static void SetFontSize(int size);

	static Font* AddFont(const Filepath& filepath, int size);

	static void ForceHideBarTab();

	static Vector4f GetSaffronOrange(float opacity = 1.0f);
	static Vector4f GetSaffronPurple(float opacity = 1.0f);

private:
	static void PushID();
	static void PopID();
	static Font* GetAppropriateFont(int size);

	static void ApplyStyle(Style style);

private:
	Style m_CurrentStyle;
	Map<int, ImFont*> m_Fonts;
	Pair<int, ImFont*> m_CurrentFont;

	char s_IDBuffer[16];
	Uint32 s_Counter = 0;
	
	static constexpr const char *FontsLocation = "Assets/Fonts/";
	static constexpr const char *FontsExtension = ".ttf";
};
}
