#include "SaffronPCH.h"

#include <box2d/box2d.h>

#include "Saffron/Core/GlobalTimer.h"
#include "Saffron/Core/Misc.h"
#include "Saffron/Entity/Entity.h"
#include "Saffron/Gui/Gui.h"
#include "Saffron/Physics/PhysicsWorld2D.h"
#include "Saffron/Scene/Scene.h"
#include "Saffron/Script/ScriptEngine.h"

namespace Se
{
//////////////////////////////////////////////////////////////
/// Contact Listener
//////////////////////////////////////////////////////////////

class ContactListener2D : public b2ContactListener
{
public:
	void BeginContact(b2Contact *contact) override
	{
		Entity &a = *static_cast<Entity *>(contact->GetFixtureA()->GetBody()->GetUserData());
		Entity &b = *static_cast<Entity *>(contact->GetFixtureB()->GetBody()->GetUserData());

		// TODO: improve these if checks
		if ( a.HasComponent<ScriptComponent>() && ScriptEngine::ModuleExists(a.GetComponent<ScriptComponent>().ModuleName) )
			ScriptEngine::OnCollision2DBegin(a);

		if ( b.HasComponent<ScriptComponent>() && ScriptEngine::ModuleExists(b.GetComponent<ScriptComponent>().ModuleName) )
			ScriptEngine::OnCollision2DBegin(b);
	}

	/// Called when two fixtures cease to touch.
	void EndContact(b2Contact *contact) override
	{
		Entity &a = *static_cast<Entity *>(contact->GetFixtureA()->GetBody()->GetUserData());
		Entity &b = *static_cast<Entity *>(contact->GetFixtureB()->GetBody()->GetUserData());

		// TODO: improve these if checks
		if ( a.HasComponent<ScriptComponent>() && ScriptEngine::ModuleExists(a.GetComponent<ScriptComponent>().ModuleName) )
			ScriptEngine::OnCollision2DEnd(a);

		if ( b.HasComponent<ScriptComponent>() && ScriptEngine::ModuleExists(b.GetComponent<ScriptComponent>().ModuleName) )
			ScriptEngine::OnCollision2DEnd(b);
	}

	void PreSolve(b2Contact *contact, const b2Manifold *oldManifold) override
	{
		B2_NOT_USED(contact);
		B2_NOT_USED(oldManifold);
	}

	void PostSolve(b2Contact *contact, const b2ContactImpulse *impulse) override
	{
		B2_NOT_USED(contact);
		B2_NOT_USED(impulse);
	}
};

static ContactListener2D s_ContactListener2D;


//////////////////////////////////////////////////////////////
/// Physics World 2D
//////////////////////////////////////////////////////////////

PhysicsWorld2D::PhysicsWorld2D(Scene &scene)
	: m_NativeWorld(new b2World(b2Vec2{ 0.0f, -9.8f })),
	m_Scene(&scene),
	m_PhysicsBodyEntityBuffer(nullptr)
{
	m_NativeWorld->SetContactListener(&s_ContactListener2D);
}

PhysicsWorld2D::~PhysicsWorld2D()
{
	OnStop();
	delete[] m_PhysicsBodyEntityBuffer;
	m_PhysicsBodyEntityBuffer = nullptr;
}

PhysicsWorld2D::PhysicsWorld2D(const PhysicsWorld2D &world)
	: m_NativeWorld(new b2World(*world.m_NativeWorld)),
	m_Scene(world.m_Scene),
	m_PhysicsBodyEntityBuffer(nullptr)
{
}

PhysicsWorld2D::PhysicsWorld2D(PhysicsWorld2D &&world) noexcept
	: m_NativeWorld(world.m_NativeWorld),
	m_Scene(world.m_Scene),
	m_PhysicsBodyEntityBuffer(world.m_PhysicsBodyEntityBuffer)
{
	world.m_NativeWorld = nullptr;
	world.m_Scene = nullptr;
	world.m_PhysicsBodyEntityBuffer = nullptr;
}

PhysicsWorld2D &PhysicsWorld2D::operator=(const PhysicsWorld2D &world)
{
	if ( &world == this )
		return *this;

	m_NativeWorld = new b2World(*world.m_NativeWorld);
	m_Scene = world.m_Scene;
	m_PhysicsBodyEntityBuffer = nullptr;

	return *this;
}

void PhysicsWorld2D::OnUpdate()
{
	const Time ts = GlobalTimer::GetStep();

	const Int32 velocityIterations = 6;
	const Int32 positionIterations = 2;
	m_NativeWorld->Step(ts.sec(), velocityIterations, positionIterations);

	{
		auto view = m_Scene->GetEntityRegistry().view<RigidBody2DComponent>();
		for ( auto entityHandle : view )
		{
			Entity entity = { entityHandle, m_Scene };

			auto &transform = entity.Transform();
			auto &rigidBody2D = entity.GetComponent<RigidBody2DComponent>();
			auto *body = static_cast<b2Body *>(rigidBody2D.RuntimeBody);


			if ( entity.HasComponent<BoxCollider2DComponent>() )
			{
				auto &component = entity.GetComponent<BoxCollider2DComponent>();
				auto *fixture = static_cast<b2Fixture *>(component.RuntimeFixture);
				fixture->SetDensity(component.Density);
				fixture->SetFriction(component.Friction);
				auto *polygonShape = dynamic_cast<b2PolygonShape *>(fixture->GetShape());
				polygonShape->SetAsBox(component.Size.x, component.Size.y);
			}
			if ( entity.HasComponent<CircleCollider2DComponent>() )
			{
				auto &component = entity.GetComponent<CircleCollider2DComponent>();
				auto *fixture = static_cast<b2Fixture *>(component.RuntimeFixture);
				fixture->SetDensity(component.Density);
				fixture->SetFriction(component.Friction);
				auto *circleShape = dynamic_cast<b2CircleShape *>(fixture->GetShape());
				circleShape->m_radius = component.Radius;
			}

			body->ResetMassData();

			const auto &position = body->GetPosition();
			const auto decomposition = Misc::GetTransformDecomposition(transform);
			const Vector3f rotation = glm::eulerAngles(decomposition.Rotation);

			transform = glm::translate(Vector3f{ position.x, position.y, transform[3].z }) *
				glm::toMat4(Quaternion({ rotation.x, rotation.y, body->GetAngle() })) *
				glm::scale(decomposition.Scale);
		}
	}
}

void PhysicsWorld2D::OnGuiRender()
{
	ImGui::Begin("Physics World 2D");
	Gui::BeginPropertyGrid();
	auto gravity = GetGravity();
	if ( Gui::Property("Gravity", gravity, -10000.0f, 10000.0f, 1.0f, Gui::PropertyFlag::Drag) )
	{
		SetGravity(gravity);
	}
	Gui::EndPropertyGrid();
	ImGui::End();
}

void PhysicsWorld2D::OnStart()
{
	if ( !m_FilledWorld )
	{
		{
			auto view = m_Scene->GetEntityRegistry().view<RigidBody2DComponent>();
			m_PhysicsBodyEntityBuffer = new Entity[view.size()];
			Uint32 physicsBodyEntityBufferIndex = 0;
			for ( auto entityHandle : view )
			{
				Entity entity = { entityHandle, m_Scene };
				auto &transform = entity.Transform();
				auto &rigidBody2D = entity.GetComponent<RigidBody2DComponent>();

				b2BodyDef bodyDef;
				if ( rigidBody2D.BodyType == RigidBody2DComponent::Type::Static )
					bodyDef.type = b2_staticBody;
				else if ( rigidBody2D.BodyType == RigidBody2DComponent::Type::Dynamic )
					bodyDef.type = b2_dynamicBody;
				else if ( rigidBody2D.BodyType == RigidBody2DComponent::Type::Kinematic )
					bodyDef.type = b2_kinematicBody;
				bodyDef.position.Set(transform[3].x, transform[3].y);

				const auto decomposition = Misc::GetTransformDecomposition(transform);
				Vector3f rotation = glm::eulerAngles(decomposition.Rotation);
				bodyDef.angle = rotation.z;

				b2Body *body = m_NativeWorld->CreateBody(&bodyDef);
				body->SetFixedRotation(rigidBody2D.FixedRotation);
				Entity *entityStorage = &m_PhysicsBodyEntityBuffer[physicsBodyEntityBufferIndex++];
				*entityStorage = entity;
				body->SetUserData(static_cast<void *>(entityStorage));
				rigidBody2D.RuntimeBody = body;
			}
		}

		{
			auto view = m_Scene->GetEntityRegistry().view<BoxCollider2DComponent>();
			for ( auto entityHandle : view )
			{
				Entity entity = { entityHandle, m_Scene };

				auto &boxCollider2D = entity.GetComponent<BoxCollider2DComponent>();
				if ( entity.HasComponent<RigidBody2DComponent>() )
				{
					auto &rigidBody2D = entity.GetComponent<RigidBody2DComponent>();
					SE_CORE_ASSERT(rigidBody2D.RuntimeBody);
					auto *body = static_cast<b2Body *>(rigidBody2D.RuntimeBody);

					b2PolygonShape polygonShape;
					polygonShape.SetAsBox(boxCollider2D.Size.x, boxCollider2D.Size.y);

					b2FixtureDef fixtureDef;
					fixtureDef.shape = &polygonShape;
					fixtureDef.density = boxCollider2D.Density;
					fixtureDef.friction = boxCollider2D.Friction;
					b2Fixture *fixture = body->CreateFixture(&fixtureDef);
					boxCollider2D.RuntimeFixture = fixture;
				}
			}
		}

		{
			auto view = m_Scene->GetEntityRegistry().view<CircleCollider2DComponent>();
			for ( auto entityHandle : view )
			{
				Entity entity = { entityHandle, m_Scene };

				auto &circleCollider2D = entity.GetComponent<CircleCollider2DComponent>();
				if ( entity.HasComponent<RigidBody2DComponent>() )
				{
					auto &rigidBody2D = entity.GetComponent<RigidBody2DComponent>();
					SE_CORE_ASSERT(rigidBody2D.RuntimeBody);
					auto *body = static_cast<b2Body *>(rigidBody2D.RuntimeBody);

					b2CircleShape circleShape;
					circleShape.m_radius = circleCollider2D.Radius;

					b2FixtureDef fixtureDef;
					fixtureDef.shape = &circleShape;
					fixtureDef.density = circleCollider2D.Density;
					fixtureDef.friction = circleCollider2D.Friction;
					b2Fixture *fixture = body->CreateFixture(&fixtureDef);
					circleCollider2D.RuntimeFixture = fixture;
				}
			}
		}
		m_FilledWorld = true;
	}
}

void PhysicsWorld2D::OnStop()
{
	if ( m_FilledWorld )
	{
		b2Body *body = m_NativeWorld->GetBodyList();
		while ( body )
		{
			b2Body *nextBody = body->GetNext();
			b2Fixture *fixture = body->GetFixtureList();
			while ( fixture )
			{
				b2Fixture *nextFixture = fixture->GetNext();
				body->DestroyFixture(fixture);
				fixture = nextFixture;
			}

			m_NativeWorld->DestroyBody(body);
			body = nextBody;
		}
		m_FilledWorld = false;
	}
}

Vector2f PhysicsWorld2D::GetGravity() const
{
	const b2Vec2 b2Gravity = m_NativeWorld->GetGravity();
	return Vector2f(b2Gravity.x, b2Gravity.y);
}

void PhysicsWorld2D::SetGravity(const Vector2f &gravity)
{
	m_NativeWorld->SetGravity(b2Vec2{ gravity.x, gravity.y });
}
}
