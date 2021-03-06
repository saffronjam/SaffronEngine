#include "SaffronPCH.h"

#include "Saffron/Core/FileIOManager.h"
#include "Saffron/Entity/Entity.h"
#include "Saffron/Entity/EntityComponents.h"
#include "Saffron/Gui/Gui.h"
#include "Saffron/Renderer/SceneRenderer.h"
#include "Saffron/Scene/Scene.h"
#include "Saffron/Script/ScriptEngine.h"


namespace Se
{


///////////////////////////////////////////////////////////////////////////
/// Statics
///////////////////////////////////////////////////////////////////////////

UnorderedMap<UUID, Scene *> s_ActiveScenes;

///////////////////////////////////////////////////////////////////////////
/// Helper functions
///////////////////////////////////////////////////////////////////////////

void OnScriptComponentConstruct(EntityRegistry &registry, EntityHandle entity)
{
	const auto sceneView = registry.view<SceneIDComponent>();
	const UUID sceneID = registry.get<SceneIDComponent>(sceneView.front()).SceneID;

	Scene *scene = s_ActiveScenes[sceneID];

	const auto entityID = registry.get<IDComponent>(entity).ID;
	SE_CORE_ASSERT(scene->m_EntityIDMap.find(entityID) != scene->m_EntityIDMap.end());
	ScriptEngine::InitScriptEntity(scene->m_EntityIDMap.at(entityID));
}

void OnScriptComponentDestroy(EntityRegistry &registry, EntityHandle entity)
{
	const auto sceneView = registry.view<SceneIDComponent>();
	const UUID sceneID = registry.get<SceneIDComponent>(sceneView.front()).SceneID;

	[[maybe_unused]] Scene *scene = s_ActiveScenes[sceneID];

	const auto entityID = registry.get<IDComponent>(entity).ID;
	ScriptEngine::OnScriptComponentDestroyed(sceneID, entityID);
}

template<typename T>
static void CopyComponentIfExists(entt::entity dst, entt::entity src, entt::registry &registry)
{
	if ( registry.has<T>(src) )
	{
		auto &srcComponent = registry.get<T>(src);
		registry.emplace_or_replace<T>(dst, srcComponent);
	}
}


///////////////////////////////////////////////////////////////////////////
/// Scene
///////////////////////////////////////////////////////////////////////////

Scene::Scene()
	:
	m_FallbackSceneEnvironment(SceneEnvironment::Load("Resources/Assets/Env/pink_sunrise_4k.hdr")),
	m_SceneEntity(m_EntityRegistry.create(), this),
	m_ViewportWidth(100),
	m_ViewportHeight(100)
{
	m_EntityRegistry.on_construct<ScriptComponent>().connect<&OnScriptComponentConstruct>();
	m_EntityRegistry.on_destroy<ScriptComponent>().connect<&OnScriptComponentDestroy>();

	m_SceneEntity.AddComponent<SceneIDComponent>(m_SceneID);
	m_SceneEntity.AddComponent<PhysicsWorld2DComponent>(*this);
	m_SceneEntity.AddComponent<PhysicsWorld3DComponent>(*this);

	s_ActiveScenes[m_SceneID] = this;

	const auto skyboxShader = Factory::Create<Shader>(Filepath{ "Resources/Assets/shaders/Skybox.glsl" });
	m_Skybox.Material = MaterialInstance::Create(Factory::Create<Material>(skyboxShader));
	m_Skybox.Material->SetFlag(Material::Flag::DepthTest, false);
}

Scene::~Scene()
{
	if ( m_SceneEntity.HasComponent<PhysicsWorld2DComponent>() )
	{
		m_SceneEntity.RemoveComponent<PhysicsWorld2DComponent>();
	}
	if ( m_SceneEntity.HasComponent<PhysicsWorld3DComponent>() )
	{
		m_SceneEntity.RemoveComponent<PhysicsWorld3DComponent>();
	}

	m_EntityRegistry.on_destroy<ScriptComponent>().disconnect();
	m_EntityRegistry.clear();

	s_ActiveScenes.erase(m_SceneID);
}

Entity Scene::CreateEntity(String name)
{
	auto entity = Entity{ m_EntityRegistry.create(), this };
	auto &idComponent = entity.AddComponent<IDComponent>();
	idComponent.ID = {};

	entity.AddComponent<TransformComponent>(Matrix4f(1.0f));
	if ( !name.empty() )
		entity.AddComponent<TagComponent>(Move(name));

	m_EntityIDMap[idComponent.ID] = entity;
	return entity;
}

Entity Scene::CreateEntity(UUID uuid, const String &name)
{
	auto entity = Entity{ m_EntityRegistry.create(), this };
	auto &idComponent = entity.AddComponent<IDComponent>();
	idComponent.ID = uuid;

	entity.AddComponent<TransformComponent>(Matrix4f(1.0f));
	if ( !name.empty() )
		entity.AddComponent<TagComponent>(name);

	SE_CORE_ASSERT(m_EntityIDMap.find(uuid) == m_EntityIDMap.end());
	m_EntityIDMap[uuid] = entity;
	return entity;
}

void Scene::DestroyEntity(Entity entity)
{
	if ( entity.HasComponent<ScriptComponent>() )
		ScriptEngine::OnScriptComponentDestroyed(m_SceneID, entity.GetUUID());

	m_EntityRegistry.destroy(entity.GetHandle());
}

Entity Scene::GetEntity(const String &tag)
{
	// TODO: If this becomes used often, consider indexing by tag
	auto view = m_EntityRegistry.view<TagComponent>();
	for ( auto entity : view )
	{
		const auto &candidate = view.get<TagComponent>(entity).Tag;
		if ( candidate == tag )
			return Entity(entity, this);
	}

	return Entity::Null();
}

void Scene::SetViewportSize(Uint32 width, Uint32 height)
{
	m_ViewportWidth = width;
	m_ViewportHeight = height;
}

const std::shared_ptr<SceneEnvironment> &Scene::GetSceneEnvironment() const
{
	return m_SceneEnvironment ? m_SceneEnvironment : m_FallbackSceneEnvironment;
}

Entity Scene::GetMainCameraEntity()
{
	auto view = m_EntityRegistry.view<CameraComponent>();
	for ( auto entity : view )
	{
		auto &comp = view.get<CameraComponent>(entity);
		if ( comp.Primary )
			return { entity, this };
	}
	return Entity::Null();
}

std::shared_ptr<Scene> Scene::GetScene(UUID uuid)
{
	if ( s_ActiveScenes.find(uuid) != s_ActiveScenes.end() )
		return s_ActiveScenes.at(uuid)->GetShared();

	return nullptr;
}

void Scene::SetSkybox(const std::shared_ptr<TextureCube> &skybox)
{
	m_Skybox.Texture = skybox;
	m_Skybox.Material->Set("u_Texture", skybox);
}

void Scene::ShowMeshBoundingBoxes(bool show)
{
	SceneRenderer::GetOptions().ShowMeshBoundingBoxes = show;
}

void Scene::ShowPhysicsBodyBoundingBoxes(bool show)
{
	SceneRenderer::GetOptions().ShowPhysicsBodyBoundingBoxes = show;
}

bool Scene::IsValidFilepath(const Filepath &filepath)
{
	return !filepath.empty() && filepath.extension() == ".ssc" && FileIOManager::FileExists(filepath);
}
}
