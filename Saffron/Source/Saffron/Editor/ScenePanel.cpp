#include "SaffronPCH.h"

#include <assimp/scene.h>

#include "Saffron/Core/Math/SaffronMath.h"
#include "Saffron/Core/Misc.h"
#include "Saffron/Editor/ScenePanel.h"
#include "Saffron/Gui/Gui.h"


namespace Se
{
Matrix4f Mat4FromAssimpMat4(const aiMatrix4x4 &matrix);

SignalAggregate<Entity> ScenePanel::Signals::OnEntityDeleted;
SignalAggregate<Entity> ScenePanel::Signals::OnEntityCopied;
SignalAggregate<Entity> ScenePanel::Signals::OnNewSelection;
SignalAggregate<Entity> ScenePanel::Signals::OnViewInModelSpace;

ScenePanel::ScenePanel(const std::shared_ptr<Scene> &context)
	: m_Context(context)
{
}

void ScenePanel::OnGuiRender(const std::shared_ptr<ScriptPanel> &scriptPanel)
{
	ImGui::Begin("Scene Hierarchy");
	if ( m_Context )
	{
		m_Context->GetEntityRegistry().each([&](auto entityHandle)
											{
												const Entity entity(entityHandle, m_Context.get());
												if ( entity.HasComponent<IDComponent>() )
													DrawEntityNode(entity);
											});

		bool viewCreateEntityModal = false;
		if ( ImGui::BeginPopupContextWindow("Create Entity Context", 1, false) )
		{
			if ( ImGui::BeginMenu("Create") )
			{
				if ( ImGui::MenuItem("Entity") )
				{
					viewCreateEntityModal = true;
				}
				if ( ImGui::MenuItem("Mesh") )
				{
					OnCreateMesh();
				}
				ImGui::Separator();
				if ( ImGui::MenuItem("Directional Light") )
				{
					OnCreateDirectionalLight();
				}
				if ( ImGui::MenuItem("Sky Light") )
				{
					OnCreateSkylight();
				}
				ImGui::EndMenu();
			}
			ImGui::EndPopup();
		}
		OnCreateEntity(viewCreateEntityModal, scriptPanel);


	}
	ImGui::End();
}

void ScenePanel::OnCreateEntity(bool viewModal, const std::shared_ptr<ScriptPanel> &scriptPanel)
{
	if ( viewModal )
	{
		ImGui::SetNextWindowPos(ImGui::GetMainViewport()->GetCenter(), ImGuiCond_Appearing, ImVec2(0.5f, 0.5f));
		ImGui::OpenPopup("Create new Entity");
	}

	bool badEntityName = false;
	ImGui::SetNextWindowContentSize(ImVec2(400, 0.0f));
	if ( ImGui::BeginPopupModal("Create new Entity", nullptr, ImGuiWindowFlags_AlwaysAutoResize) )
	{
		static String entityName;
		static bool meshComponent = false;
		static bool cameraComponent = false;
		static bool directionalLightComponent = false;
		static bool skylightComponent = false;
		static bool scriptComponent = false;
		static int scriptChosen = 0;
		static bool spriteRendererComponent = false;
		static bool rigidBody2DComponent = false;
		static bool boxCollider2DComponent = false;
		static bool circleCollider2DComponent = false;
		static bool rigidBody3DComponent = false;
		static bool boxCollider3DComponent = false;
		static bool sphereCollider3DComponent = false;

		Gui::BeginPropertyGrid();
		Gui::Property("Name", entityName);
		Gui::Property("Mesh", meshComponent);
		Gui::Property("Script", scriptComponent);
		Gui::Property("Camera", cameraComponent);
		Gui::Property("Directional Light", directionalLightComponent);
		Gui::Property("Sky Light", skylightComponent);
		if ( scriptComponent )
		{
			ImGui::NextColumn();
			OutputStringStream oss;
			for ( const auto &scriptName : scriptPanel->GetScriptStats() ) { oss << scriptName.Class << '\0'; }
			oss << '\0';
			ImGui::Combo("##EntityCreateScriptComboOption", &scriptChosen, oss.str().c_str());
			ImGui::NextColumn();
		}
		Gui::Property("Sprite", spriteRendererComponent);
		Gui::Property("Rigid Body 2D", rigidBody2DComponent);
		Gui::Property("Box Collider 2D", boxCollider2DComponent);
		Gui::Property("Circle Collider 2D", circleCollider2DComponent);
		Gui::Property("Rigid Body 3D", rigidBody3DComponent);
		Gui::Property("Box Collider 3D", boxCollider3DComponent);
		Gui::Property("Circle Collider 3D", sphereCollider3DComponent);

		if ( ImGui::Button("Cancel") )
		{
			scriptChosen = 0;
			ImGui::CloseCurrentPopup();
		}
		ImGui::SameLine(0, 5);
		if ( ImGui::Button("Create") )
		{
			if ( entityName.empty() )
			{
				badEntityName = true;
			}
			else
			{
				Entity newEntity = m_Context->CreateEntity(entityName);

				if ( m_Context->GetEntity().HasComponent<EditorCameraComponent>() )
				{
					// Put new Entity in front of editor camera
					auto &editorCamera = m_Context->GetEntity().GetComponent<EditorCameraComponent>().Camera;
					auto &transform = newEntity.GetComponent<TransformComponent>().Transform;
					auto [position, rotation, scale] = Misc::GetTransformDecomposition(transform);
					auto cameraFrontPosition = editorCamera->GetPosition() + editorCamera->GetForwardDirection() * 30.0f;
					transform = glm::translate(cameraFrontPosition) * glm::toMat4(rotation) * glm::scale(scale);
				}

				if ( meshComponent )
				{
					const String defaultMeshPath = "Resources/Assets/meshes/Cube1m.fbx";
					newEntity.AddComponent<MeshComponent>(CreateShared<Mesh>(defaultMeshPath));
					meshComponent = false;
				}
				if ( cameraComponent )
				{
					newEntity.AddComponent<CameraComponent>();
					cameraComponent = false;
				}
				if ( directionalLightComponent )
				{
					newEntity.AddComponent<DirectionalLightComponent>();
					cameraComponent = false;
				}
				if ( skylightComponent )
				{
					newEntity.AddComponent<SkylightComponent>(SceneEnvironment::Load("Resources/Assets/Env/pink_sunrise_4k.hdr"));
					cameraComponent = false;
				}
				if ( scriptComponent )
				{
					newEntity.AddComponent<ScriptComponent>(scriptPanel->GetScriptStats().at(scriptChosen).Full);
					scriptComponent = false;
				}
				if ( spriteRendererComponent )
				{
					newEntity.AddComponent<SpriteRendererComponent>();
					spriteRendererComponent = false;
				}
				if ( rigidBody2DComponent )
				{
					newEntity.AddComponent<RigidBody2DComponent>();
					rigidBody2DComponent = false;
				}
				if ( boxCollider2DComponent )
				{
					newEntity.AddComponent<BoxCollider2DComponent>();
					boxCollider2DComponent = false;
				}
				if ( circleCollider2DComponent )
				{
					newEntity.AddComponent<CircleCollider2DComponent>();
					circleCollider2DComponent = false;
				}
				if ( rigidBody3DComponent )
				{
					newEntity.AddComponent<RigidBody3DComponent>();
					rigidBody3DComponent = false;
				}
				if ( boxCollider3DComponent )
				{
					newEntity.AddComponent<BoxCollider3DComponent>();
					boxCollider3DComponent = false;
				}
				if ( sphereCollider3DComponent )
				{
					newEntity.AddComponent<SphereCollider3DComponent>();
					sphereCollider3DComponent = false;
				}
				entityName.clear();
				ImGui::CloseCurrentPopup();
				GetSignals().Emit(Signals::OnNewSelection, newEntity);
			}
		}

		Gui::InfoModal("Bad Entity Name", "Entity must have a name", badEntityName);

		Gui::EndPropertyGrid();

		ImGui::EndPopup();
	}
}

void ScenePanel::OnCreateMesh()
{
	auto newEntity = m_Context->CreateEntity("Mesh");
	const String defaultMeshPath = "Resources/Assets/meshes/Cube1m.fbx";
	newEntity.AddComponent<MeshComponent>(CreateShared<Mesh>(defaultMeshPath));
	GetSignals().Emit(Signals::OnNewSelection, newEntity);
}

void ScenePanel::OnCreateDirectionalLight()
{
	auto newEntity = m_Context->CreateEntity("Directional Light");
	newEntity.AddComponent<DirectionalLightComponent>();
	newEntity.GetComponent<TransformComponent>().Transform =
		glm::toMat4(Quaternion(glm::radians(Vector3f{ 80.0f, 10.0f, 0.0f })));
	GetSignals().Emit(Signals::OnNewSelection, newEntity);
}

void ScenePanel::OnCreateSkylight()
{
	auto newEntity = m_Context->CreateEntity("Sky Light");
	newEntity.AddComponent<SkylightComponent>(SceneEnvironment::Load("Resources/Assets/Env/pink_sunrise_4k.hdr"));
	GetSignals().Emit(Signals::OnNewSelection, newEntity);
}


void ScenePanel::DrawEntityNode(Entity entity)
{
	String name = "Unnamed";
	if ( entity.HasComponent<TagComponent>() )
		name = entity.GetComponent<TagComponent>().Tag;

	const ImGuiTreeNodeFlags node_flags = (entity == m_SelectionContext ? ImGuiTreeNodeFlags_Selected : 0) | ImGuiTreeNodeFlags_OpenOnArrow;

	const bool opened = ImGui::TreeNodeEx(reinterpret_cast<void *>(&entity.GetComponent<IDComponent>().ID), node_flags, name.c_str());
	if ( ImGui::IsItemClicked() )
	{
		m_SelectionContext = entity;
		GetSignals().Emit(Signals::OnNewSelection, m_SelectionContext);
	}

	bool entityDeleted = false;
	bool entityCopied = false;
	bool entityViewInModelSpace = false;
	if ( ImGui::BeginPopupContextItem() )
	{
		if ( ImGui::MenuItem("Delete") )
			entityDeleted = true;
		if ( ImGui::MenuItem("Copy") )
			entityCopied = true;
		if ( ImGui::MenuItem("View in model space") )
			entityViewInModelSpace = true;

		ImGui::EndPopup();
	}
	if ( opened )
	{
		if ( entity.HasComponent<MeshComponent>() )
		{
			const auto mesh = entity.GetComponent<MeshComponent>().Mesh;
			if ( mesh )
				DrawMeshNode(mesh, entity.GetComponent<IDComponent>().ID);
		}
		ImGui::TreePop();
	}

	// Defer deletion until end of node UI
	if ( entityDeleted )
	{
		GetSignals().Emit(Signals::OnEntityDeleted, entity);
	}
	if ( entityCopied )
	{
		GetSignals().Emit(Signals::OnEntityCopied, entity);
	}
	if ( entityViewInModelSpace )
	{
		GetSignals().Emit(Signals::OnViewInModelSpace, entity);
	}


}

void ScenePanel::DrawMeshNode(const std::shared_ptr<Mesh> &mesh, UUID &entityUUID) const
{
	OutputStringStream oss;
	oss << "Mesh##" << entityUUID;

	// Mesh Hierarchy
	if ( ImGui::TreeNode(oss.str().c_str()) )
	{
		auto *rootNode = mesh->m_Scene->mRootNode;
		MeshNodeHierarchy(mesh, rootNode);
		ImGui::TreePop();
	}
}

void ScenePanel::MeshNodeHierarchy(const std::shared_ptr<Mesh> &mesh,
								   aiNode *node,
								   const Matrix4f &parentTransform,
								   Uint32 level) const
{
	const Matrix4f localTransform = Mat4FromAssimpMat4(node->mTransformation);
	const Matrix4f transform = parentTransform * localTransform;

	if ( ImGui::TreeNode(node->mName.C_Str()) )
	{
		{
			auto [translation, rotation, scale] = Misc::GetTransformDecomposition(transform);
			ImGui::Text("World Transform");
			ImGui::Text("  Translation: %.2f, %.2f, %.2f", translation.x, translation.y, translation.z);
			ImGui::Text("  Scale: %.2f, %.2f, %.2f", scale.x, scale.y, scale.z);
		}
		{
			auto [translation, rotation, scale] = Misc::GetTransformDecomposition(localTransform);
			ImGui::Text("Local Transform");
			ImGui::Text("  Translation: %.2f, %.2f, %.2f", translation.x, translation.y, translation.z);
			ImGui::Text("  Scale: %.2f, %.2f, %.2f", scale.x, scale.y, scale.z);
		}

		for ( Uint32 i = 0; i < node->mNumChildren; i++ )
			MeshNodeHierarchy(mesh, node->mChildren[i], transform, level + 1);

		ImGui::TreePop();
	}
}
}
