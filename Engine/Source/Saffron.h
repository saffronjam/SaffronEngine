//
// Note:	this file is to be included in client applications ONLY
//			NEVER include this file anywhere in the engine codebase
//
#pragma once

#include "Saffron/Core/App.h"
#include "Saffron/Core/Log.h"
#include "Saffron/Core/Time.h"
#include "Saffron/Core/Timer.h"
#include "Saffron/Core/Global.h"

#include "Saffron/Core/Event.h"
#include "Saffron/Core/Events/AppEvent.h"
#include "Saffron/Core/Events/KeyboardEvent.h"
#include "Saffron/Core/Events/MouseEvent.h"

#include "Saffron/Math/AABB.h"
#include "Saffron/Math/Ray.h"

#include "Saffron/Editor/AssetPanel.h"
#include "Saffron/Editor/ViewportPane.h"
#include "Saffron/Editor/EntityComponentsPanel.h"
#include "Saffron/Editor/ScriptPanel.h"

#include "Saffron/Gui/Gui.h"

// --- SaffronRender API ------------------------------
#include "Saffron/Renderer/Renderer.h"
#include "Saffron/Renderer/SceneRenderer.h"
#include "Saffron/Renderer/RenderPass.h"
#include "Saffron/Renderer/Framebuffer.h"
#include "Saffron/Renderer/VertexBuffer.h"
#include "Saffron/Renderer/IndexBuffer.h"
#include "Saffron/Renderer/Pipeline.h"
#include "Saffron/Renderer/Texture.h"
#include "Saffron/Renderer/Shader.h"
#include "Saffron/Renderer/Mesh.h"
#include "Saffron/Renderer/Camera.h"
#include "Saffron/Renderer/Material.h"
// ---------------------------------------------------

// Scenes
#include "Saffron/Entity/Entity.h"
#include "Saffron/Scene/Scene.h"
#include "Saffron/Scene/SceneCamera.h"
#include "Saffron/Serialize/SceneSerializer.h"
#include "Saffron/Serialize/ProjectSerializer.h"
#include "Saffron/Entity/EntityComponents.h"

#include "Saffron/Base.h"
#include "Saffron/Editor/EditorTerminal.h"
#include "Saffron/Editor/EntityComponentsPanel.h"
#include "Saffron/Editor/SceneHierarchyPanel.h"
#include "Saffron/Editor/SplashScreenPane.h"
#include "Saffron/Scene/EditorScene.h"
#include "Saffron/Scene/ModelSpaceScene.h"
#include "Saffron/Scene/RuntimeScene.h"

#include "Saffron/Core/BatchLoader.h"
#include "Saffron/Core/FileIOManager.h"
#include "Saffron/Core/Misc.h"
#include "Saffron/Core/Run.h"
#include "Saffron/Editor/ViewportPane.h"
#include "Saffron/Gui/Gui.h"
#include "Saffron/Input/Keyboard.h"
#include "Saffron/Input/Mouse.h"
#include "Saffron/Script/ScriptEngine.h"

#ifdef SAFFRON_ENTRY_POINT
#include <Saffron/EntryPoint.h>
#endif