// The C++ side of the Jolt FFI seam: implements the free functions of the `cxx` bridge declared
// in `src/bridge.rs` against vendored Jolt. This shim TU and the Jolt TUs are the only code
// compiled with the determinism + AVX2 flags, confined to this crate.
//
// The four virtual subclasses `cxx` cannot synthesize from a Rust trait (the three fixed-policy
// layer filters and the `ContactListener`) live in `jolt_bridge.h`, embedded by value in
// `JoltWorld`. The filters encode pure v1 policy with no per-project state, so routing them back
// to Rust per-call would only put a Rust callback on Jolt's hot cull path; they stay in C++. The
// contact listener fires from Jolt job threads, so its buffer is mutex-guarded and drained on the
// sim thread.
//
// This TU includes the generated `bridge.rs.h` so the shared `PendingContact` is a *complete*
// type here (it is only forward-declared in `jolt_bridge.h`), which lets `drain()` translate the
// Jolt-side `RawContact` records into the bridge struct.

// `jolt_bridge.h` pulls `<Jolt/Jolt.h>` first (which defines `JPH::uint32` etc.), so it must
// precede the finer-grained Jolt headers below — those assume the umbrella header is already in.
#include "jolt_bridge.h"

#include <Jolt/Core/Factory.h>
#include <Jolt/Core/Memory.h>
#include <Jolt/Math/Quat.h>
#include <Jolt/Physics/Body/BodyInterface.h>
#include <Jolt/Physics/Body/BodyLock.h>
#include <Jolt/Physics/Collision/CastResult.h>
#include <Jolt/Physics/Collision/CollisionCollectorImpl.h>
#include <Jolt/Physics/Collision/NarrowPhaseQuery.h>
#include <Jolt/Physics/Collision/RayCast.h>
#include <Jolt/Physics/Collision/ShapeCast.h>
#include <Jolt/Physics/Collision/Shape/CapsuleShape.h>
#include <Jolt/Physics/Collision/Shape/ConvexHullShape.h>
#include <Jolt/Physics/Collision/Shape/MeshShape.h>
#include <Jolt/Physics/Collision/Shape/SphereShape.h>
#include <Jolt/Physics/Constraints/FixedConstraint.h>
#include <Jolt/Physics/Constraints/HingeConstraint.h>
#include <Jolt/Physics/Constraints/MotorSettings.h>
#include <Jolt/Physics/Constraints/PointConstraint.h>
#include <Jolt/Physics/Constraints/SwingTwistConstraint.h>
#include <Jolt/Physics/Constraints/TwoBodyConstraint.h>
#include <Jolt/Physics/EActivation.h>
#include <Jolt/Physics/PhysicsSettings.h>
#include <Jolt/Skeleton/Skeleton.h>
#include <Jolt/RegisterTypes.h>

#include <algorithm>
#include <string>

#include <cstdarg>
#include <cstdint>
#include <cstdio>
#include <memory>

#include "saffron-physics-sys/src/bridge.rs.h"

namespace saffron::physics
{
    namespace
    {
        // Jolt routes its trace output here. It goes to stderr so a message is never silently
        // dropped.
        void jolt_trace(const char *format, ...)
        {
            va_list args;
            va_start(args, format);
            std::vfprintf(stderr, format, args);
            std::fputc('\n', stderr);
            va_end(args);
        }

#ifdef JPH_ENABLE_ASSERTS
        bool jolt_assert_failed(const char *expression, const char *message, const char *file,
                                JPH::uint line)
        {
            std::fprintf(stderr, "[jolt assert] %s:%u: %s%s%s\n", file, line, expression,
                         message != nullptr ? " — " : "", message != nullptr ? message : "");
            return true;
        }
#endif
    } // namespace

    rust::Vec<PendingContact> ContactListenerImpl::drain()
    {
        const std::scoped_lock lock(mutex_);
        rust::Vec<PendingContact> out;
        out.reserve(pending_.size());
        for (const RawContact &contact : pending_)
        {
            out.push_back(PendingContact{
                .a = contact.a,
                .b = contact.b,
                .point = { contact.point[0], contact.point[1], contact.point[2] },
                .normal = { contact.normal[0], contact.normal[1], contact.normal[2] },
                .begin = contact.begin,
            });
        }
        pending_.clear();
        return out;
    }

    bool jolt_init()
    {
        JPH::RegisterDefaultAllocator();
        JPH::Trace = jolt_trace;
        JPH_IF_ENABLE_ASSERTS(JPH::AssertFailed = jolt_assert_failed;)

        if (JPH::Factory::sInstance == nullptr)
        {
            JPH::Factory::sInstance = new JPH::Factory();
            JPH::RegisterTypes();
        }
        return true;
    }

    void jolt_shutdown()
    {
        if (JPH::Factory::sInstance != nullptr)
        {
            JPH::UnregisterTypes();
            delete JPH::Factory::sInstance;
            JPH::Factory::sInstance = nullptr;
        }
    }

    std::uint32_t jolt_version()
    {
        return (static_cast<std::uint32_t>(JPH_VERSION_MAJOR) << 16)
               | (static_cast<std::uint32_t>(JPH_VERSION_MINOR) << 8)
               | static_cast<std::uint32_t>(JPH_VERSION_PATCH);
    }

    bool jolt_is_deterministic()
    {
        return true; // guaranteed by the #error guard in jolt_bridge.h
    }

    bool jolt_is_single_precision()
    {
        return true; // guaranteed by the #error guard in jolt_bridge.h
    }

    bool jolt_layers_collide(std::uint8_t a, std::uint8_t b)
    {
        return layers_collide_impl(static_cast<ObjectLayer>(a), static_cast<ObjectLayer>(b));
    }

    std::unique_ptr<JoltWorld> jolt_world_new()
    {
        auto world = std::make_unique<JoltWorld>();
        // 10 MiB scratch for the solver; the canonical Jolt job-system bounds + auto thread count.
        world->tempAllocator = std::make_unique<JPH::TempAllocatorImpl>(10 * 1024 * 1024);
        world->jobSystem = std::make_unique<JPH::JobSystemThreadPool>(JPH::cMaxPhysicsJobs,
                                                                      JPH::cMaxPhysicsBarriers, -1);
        return world;
    }

    void jolt_world_init(JoltWorld &world)
    {
        // v1 limits: 1024 bodies, default mutex count, 1024 body pairs / contact constraints.
        world.system.Init(1024, 0, 1024, 1024, world.broadPhaseLayer, world.objectVsBroadPhase,
                          world.objectLayerPair);
        world.system.SetGravity(JPH::Vec3(0.0F, -9.81F, 0.0F));
        world.system.SetContactListener(&world.contactListener);
    }

    std::uint32_t jolt_world_body_count(const JoltWorld &world)
    {
        return world.system.GetNumBodies();
    }

    void jolt_world_step(JoltWorld &world, float dt, std::int32_t collision_steps)
    {
        world.system.Update(dt, collision_steps, world.tempAllocator.get(), world.jobSystem.get());
    }

    rust::Vec<PendingContact> jolt_drain_contacts(JoltWorld &world)
    {
        return world.contactListener.drain();
    }

    namespace
    {
        // Create a shape from any settings (virtual ShapeSettings::Create); null + a loud log on
        // error.
        JPH::ShapeRefC create_shape(const JPH::ShapeSettings &settings, const char *what)
        {
            const JPH::ShapeSettings::ShapeResult result = settings.Create();
            if (result.HasError())
            {
                std::fprintf(stderr, "physics: %s shape create failed: %s\n", what,
                             result.GetError().c_str());
                return {};
            }
            return result.Get();
        }

        // Place a shape in the body's local frame when the collider has a non-zero offset.
        JPH::ShapeRefC wrap_offset(JPH::ShapeRefC shape, JPH::Vec3Arg offset)
        {
            if (shape == nullptr || offset == JPH::Vec3::sZero())
            {
                return shape;
            }
            const JPH::RotatedTranslatedShapeSettings wrap(offset, JPH::Quat::sIdentity(), shape);
            return create_shape(wrap, "offset");
        }

        // Reconstruct a Jolt BodyID from the raw index+sequence the bridge round-trips. The bodies
        // the safe layer tracks were created by Jolt, so the broad-phase bit is clear and the
        // explicit `BodyID(uint32)` ctor's invariant holds.
        JPH::BodyID body_id(std::uint32_t raw)
        {
            return JPH::BodyID(raw);
        }
    } // namespace

    namespace
    {
        // The shape kinds the bridge's `BodyCreate.shape` discriminant selects.
        enum class Shape : std::uint8_t
        {
            Box,
            Sphere,
            Capsule,
            ConvexHull,
            Mesh,
        };

        // Build the Jolt collision shape for a collider. Analytic shapes size from the
        // (auto-fitted) component fields; ConvexHull/Mesh build from the cooked geometry the safe
        // layer fed in index order (so the cooked shape is reproducible run-to-run). Mesh-on-Dynamic
        // is rejected on the safe side, so it never reaches here. Returns null (a loud log) on a
        // cook/build failure; the caller maps null to `cInvalidBodyID`.
        JPH::ShapeRefC build_collider_shape(const BodyCreate &create,
                                            rust::Slice<const float> hull_points,
                                            rust::Slice<const float> mesh_vertices,
                                            rust::Slice<const std::uint32_t> mesh_indices)
        {
            switch (static_cast<Shape>(create.shape))
            {
            case Shape::Box:
            {
                // Jolt rejects a degenerate box; the convex radius is half the smallest half-extent,
                // capped at 0.05.
                const JPH::Vec3 he(std::max(create.half_extents[0], 0.01F),
                                   std::max(create.half_extents[1], 0.01F),
                                   std::max(create.half_extents[2], 0.01F));
                const float convexRadius =
                    std::min(0.05F, std::min({ he.GetX(), he.GetY(), he.GetZ() }) * 0.5F);
                return create_shape(JPH::BoxShapeSettings(he, convexRadius), "box");
            }
            case Shape::Sphere:
            {
                const float radius = std::max(create.half_extents[0], 0.01F); // radius packed in .x
                return create_shape(JPH::SphereShapeSettings(radius), "sphere");
            }
            case Shape::Capsule:
            {
                const float radius = std::max(create.half_extents[0], 0.01F); // radius in .x
                const float halfHeight =
                    std::max(create.half_extents[1], 0.01F); // cylinder half-height in .y (Y-up)
                return create_shape(JPH::CapsuleShapeSettings(halfHeight, radius), "capsule");
            }
            case Shape::ConvexHull:
            {
                // Points arrive as a flat xyz stream in index order — stable for determinism.
                JPH::Array<JPH::Vec3> points;
                points.reserve(hull_points.size() / 3);
                for (std::size_t i = 0; i + 2 < hull_points.size(); i = i + 3)
                {
                    points.push_back(
                        JPH::Vec3(hull_points[i], hull_points[i + 1], hull_points[i + 2]));
                }
                if (points.empty())
                {
                    std::fprintf(stderr, "physics: convex-hull source mesh has no vertices\n");
                    return {};
                }
                return create_shape(JPH::ConvexHullShapeSettings(points), "convex hull");
            }
            case Shape::Mesh:
            {
                // Vertices as a flat xyz stream; indices as a flat triangle list, both index-ordered.
                JPH::VertexList vertices;
                vertices.reserve(mesh_vertices.size() / 3);
                for (std::size_t i = 0; i + 2 < mesh_vertices.size(); i = i + 3)
                {
                    vertices.push_back(
                        JPH::Float3(mesh_vertices[i], mesh_vertices[i + 1], mesh_vertices[i + 2]));
                }
                JPH::IndexedTriangleList triangles;
                triangles.reserve(mesh_indices.size() / 3);
                for (std::size_t i = 0; i + 2 < mesh_indices.size(); i = i + 3)
                {
                    triangles.push_back(JPH::IndexedTriangle(mesh_indices[i], mesh_indices[i + 1],
                                                             mesh_indices[i + 2], 0));
                }
                if (triangles.empty())
                {
                    std::fprintf(stderr, "physics: mesh source has no triangles\n");
                    return {};
                }
                return create_shape(JPH::MeshShapeSettings(vertices, triangles), "mesh");
            }
            }
            return {};
        }
    } // namespace

    std::uint32_t jolt_create_body(JoltWorld &world, const BodyCreate &create,
                                   rust::Slice<const float> hull_points,
                                   rust::Slice<const float> mesh_vertices,
                                   rust::Slice<const std::uint32_t> mesh_indices)
    {
        const JPH::Vec3 offset(create.offset[0], create.offset[1], create.offset[2]);
        const JPH::ShapeRefC shape = wrap_offset(
            build_collider_shape(create, hull_points, mesh_vertices, mesh_indices), offset);
        if (shape == nullptr)
        {
            return JPH::BodyID::cInvalidBodyID;
        }

        const JPH::RVec3 position(create.position[0], create.position[1], create.position[2]);
        // glam's Quat storage is xyzw — the same component order as JPH::Quat.
        const JPH::Quat rotation(create.rotation[0], create.rotation[1], create.rotation[2],
                                 create.rotation[3]);
        const auto motion = static_cast<JPH::EMotionType>(create.motion);
        const auto objectLayer = static_cast<JPH::ObjectLayer>(create.object_layer);
        JPH::BodyCreationSettings settings(shape, position, rotation, motion, objectLayer);
        settings.mIsSensor = create.is_sensor;
        settings.mFriction = create.friction;
        settings.mRestitution = create.restitution;
        if (motion == JPH::EMotionType::Dynamic)
        {
            settings.mLinearDamping = create.linear_damping;
            settings.mAngularDamping = create.angular_damping;
            settings.mGravityFactor = create.gravity_factor;
            settings.mAllowedDOFs = static_cast<JPH::EAllowedDOFs>(create.allowed_dofs);
            settings.mOverrideMassProperties = JPH::EOverrideMassProperties::CalculateInertia;
            settings.mMassPropertiesOverride.mMass = create.mass;
        }
        const JPH::EActivation activation = motion == JPH::EMotionType::Static
                                                ? JPH::EActivation::DontActivate
                                                : JPH::EActivation::Activate;
        const JPH::BodyID id = world.system.GetBodyInterface().CreateAndAddBody(settings, activation);
        if (id.IsInvalid())
        {
            std::fprintf(stderr, "physics: body create failed (body limit reached?)\n");
            return JPH::BodyID::cInvalidBodyID;
        }
        return id.GetIndexAndSequenceNumber();
    }

    rust::Vec<std::uint32_t> jolt_create_static_batch(JoltWorld &world,
                                                      rust::Slice<const BodyCreate> creates)
    {
        rust::Vec<std::uint32_t> ids;
        ids.reserve(creates.size());
        std::vector<JPH::BodyID> added;
        added.reserve(creates.size());
        JPH::BodyInterface &bi = world.system.GetBodyInterface();
        for (const BodyCreate &create : creates)
        {
            const JPH::Vec3 offset(create.offset[0], create.offset[1], create.offset[2]);
            const JPH::ShapeRefC shape = wrap_offset(
                build_collider_shape(create, rust::Slice<const float>(),
                                     rust::Slice<const float>(),
                                     rust::Slice<const std::uint32_t>()),
                offset);
            if (shape == nullptr)
            {
                ids.push_back(JPH::BodyID::cInvalidBodyID);
                continue;
            }
            const JPH::RVec3 position(create.position[0], create.position[1],
                                      create.position[2]);
            const JPH::Quat rotation(create.rotation[0], create.rotation[1], create.rotation[2],
                                     create.rotation[3]);
            JPH::BodyCreationSettings settings(shape, position, rotation,
                                               static_cast<JPH::EMotionType>(create.motion),
                                               static_cast<JPH::ObjectLayer>(create.object_layer));
            settings.mIsSensor = create.is_sensor;
            settings.mFriction = create.friction;
            settings.mRestitution = create.restitution;
            JPH::Body *body = bi.CreateBody(settings);
            if (body == nullptr)
            {
                std::fprintf(stderr, "physics: batch body create failed (body limit reached?)\n");
                ids.push_back(JPH::BodyID::cInvalidBodyID);
                continue;
            }
            ids.push_back(body->GetID().GetIndexAndSequenceNumber());
            added.push_back(body->GetID());
        }
        if (!added.empty())
        {
            JPH::BodyInterface::AddState state =
                bi.AddBodiesPrepare(added.data(), static_cast<int>(added.size()));
            bi.AddBodiesFinalize(added.data(), static_cast<int>(added.size()), state,
                                 JPH::EActivation::DontActivate);
        }
        return ids;
    }

    void jolt_remove_bodies(JoltWorld &world, rust::Slice<const std::uint32_t> ids)
    {
        std::vector<JPH::BodyID> bodies;
        bodies.reserve(ids.size());
        for (const std::uint32_t id : ids)
        {
            const JPH::BodyID body(id);
            if (!body.IsInvalid())
            {
                bodies.push_back(body);
            }
        }
        if (bodies.empty())
        {
            return;
        }
        JPH::BodyInterface &bi = world.system.GetBodyInterface();
        bi.RemoveBodies(bodies.data(), static_cast<int>(bodies.size()));
        bi.DestroyBodies(bodies.data(), static_cast<int>(bodies.size()));
    }

    void jolt_body_position_rotation(const JoltWorld &world, std::uint32_t id,
                                     std::array<float, 3> &position, std::array<float, 4> &rotation)
    {
        JPH::RVec3 pos;
        JPH::Quat rot;
        world.system.GetBodyInterface().GetPositionAndRotation(body_id(id), pos, rot);
        position = { pos.GetX(), pos.GetY(), pos.GetZ() };
        rotation = { rot.GetX(), rot.GetY(), rot.GetZ(), rot.GetW() };
    }

    std::array<float, 3> jolt_body_position(const JoltWorld &world, std::uint32_t id)
    {
        const JPH::RVec3 pos = world.system.GetBodyInterface().GetPosition(body_id(id));
        return { pos.GetX(), pos.GetY(), pos.GetZ() };
    }

    bool jolt_body_is_active(const JoltWorld &world, std::uint32_t id)
    {
        return world.system.GetBodyInterface().IsActive(body_id(id));
    }

    std::array<float, 3> jolt_body_linear_velocity(const JoltWorld &world, std::uint32_t id)
    {
        const JPH::Vec3 v = world.system.GetBodyInterface().GetLinearVelocity(body_id(id));
        return { v.GetX(), v.GetY(), v.GetZ() };
    }

    std::array<float, 3> jolt_body_angular_velocity(const JoltWorld &world, std::uint32_t id)
    {
        const JPH::Vec3 v = world.system.GetBodyInterface().GetAngularVelocity(body_id(id));
        return { v.GetX(), v.GetY(), v.GetZ() };
    }

    void jolt_body_add_impulse(JoltWorld &world, std::uint32_t id,
                               const std::array<float, 3> &impulse)
    {
        JPH::BodyInterface &bi = world.system.GetBodyInterface();
        const JPH::BodyID body = body_id(id);
        bi.ActivateBody(body);
        bi.AddImpulse(body, JPH::Vec3(impulse[0], impulse[1], impulse[2]));
    }

    void jolt_body_add_force(JoltWorld &world, std::uint32_t id, const std::array<float, 3> &force)
    {
        JPH::BodyInterface &bi = world.system.GetBodyInterface();
        const JPH::BodyID body = body_id(id);
        bi.ActivateBody(body);
        bi.AddForce(body, JPH::Vec3(force[0], force[1], force[2]));
    }

    void jolt_body_set_linear_velocity(JoltWorld &world, std::uint32_t id,
                                       const std::array<float, 3> &velocity)
    {
        JPH::BodyInterface &bi = world.system.GetBodyInterface();
        const JPH::BodyID body = body_id(id);
        bi.ActivateBody(body);
        bi.SetLinearVelocity(body, JPH::Vec3(velocity[0], velocity[1], velocity[2]));
    }

    void jolt_body_set_angular_velocity(JoltWorld &world, std::uint32_t id,
                                        const std::array<float, 3> &velocity)
    {
        JPH::BodyInterface &bi = world.system.GetBodyInterface();
        const JPH::BodyID body = body_id(id);
        bi.ActivateBody(body);
        bi.SetAngularVelocity(body, JPH::Vec3(velocity[0], velocity[1], velocity[2]));
    }

    void jolt_move_kinematic(JoltWorld &world, std::uint32_t id, const std::array<float, 3> &position,
                             const std::array<float, 4> &rotation, float dt)
    {
        // MoveKinematic derives the body's linear+angular velocity from the target pose and dt, so
        // the swept motion imparts contact velocity to the dynamics it hits (a teleport via
        // SetPositionAndRotation would give zero contact velocity). `rotation` is xyzw — glam's
        // component order is Jolt's.
        world.system.GetBodyInterface().MoveKinematic(
            body_id(id), JPH::RVec3(position[0], position[1], position[2]),
            JPH::Quat(rotation[0], rotation[1], rotation[2], rotation[3]), dt);
    }

    std::uint32_t jolt_add_character(JoltWorld &world, const CharacterCreate &create)
    {
        // The capsule is the entity's collider (radius = half_extents.x, half-height =
        // half_extents.y), clamped above a degenerate floor.
        const float radius = std::max(create.radius, 0.05F);
        const float halfHeight = std::max(create.half_height, 0.05F);
        const JPH::ShapeRefC shape =
            create_shape(JPH::CapsuleShapeSettings(halfHeight, radius), "character capsule");
        if (shape == nullptr)
        {
            return JPH::BodyID::cInvalidBodyID;
        }
        JPH::CharacterVirtualSettings settings;
        settings.mShape = shape;
        settings.mMaxSlopeAngle = create.max_slope_angle;
        const JPH::RVec3 position(create.position[0], create.position[1], create.position[2]);
        JPH::Ref<JPH::CharacterVirtual> character =
            new JPH::CharacterVirtual(&settings, position, JPH::Quat::sIdentity(), &world.system);
        world.characters.push_back(std::move(character));
        return static_cast<std::uint32_t>(world.characters.size() - 1);
    }

    void jolt_character_set_linear_velocity(JoltWorld &world, std::uint32_t index,
                                            const std::array<float, 3> &velocity)
    {
        if (index >= world.characters.size())
        {
            return;
        }
        world.characters[index]->SetLinearVelocity(
            JPH::Vec3(velocity[0], velocity[1], velocity[2]));
    }

    void jolt_character_extended_update(JoltWorld &world, std::uint32_t index, float dt,
                                        const std::array<float, 3> &gravity, float step_up)
    {
        if (index >= world.characters.size())
        {
            return;
        }
        JPH::CharacterVirtual &character = *world.characters[index];
        JPH::CharacterVirtual::ExtendedUpdateSettings updateSettings;
        updateSettings.mWalkStairsStepUp = JPH::Vec3(0.0F, step_up, 0.0F);
        const auto layer = static_cast<JPH::ObjectLayer>(ObjectLayer::Character);
        character.ExtendedUpdate(dt, JPH::Vec3(gravity[0], gravity[1], gravity[2]), updateSettings,
                                 world.system.GetDefaultBroadPhaseLayerFilter(layer),
                                 world.system.GetDefaultLayerFilter(layer), JPH::BodyFilter{},
                                 JPH::ShapeFilter{}, *world.tempAllocator);
    }

    bool jolt_character_on_ground(const JoltWorld &world, std::uint32_t index)
    {
        if (index >= world.characters.size())
        {
            return false;
        }
        return world.characters[index]->GetGroundState()
               == JPH::CharacterBase::EGroundState::OnGround;
    }

    std::array<float, 3> jolt_character_position(const JoltWorld &world, std::uint32_t index)
    {
        if (index >= world.characters.size())
        {
            return { 0.0F, 0.0F, 0.0F };
        }
        const JPH::RVec3 pos = world.characters[index]->GetPosition();
        return { pos.GetX(), pos.GetY(), pos.GetZ() };
    }

    std::array<float, 3> jolt_world_gravity(const JoltWorld &world)
    {
        const JPH::Vec3 g = world.system.GetGravity();
        return { g.GetX(), g.GetY(), g.GetZ() };
    }

    namespace
    {
        // A position motor from a bone's PD gains: frequency/damping spring + torque limit, with
        // sensible defaults when authored ~0 (a fresh ragdoll motors gently). Inert until the motor
        // *state* is set to Position (active ragdoll only).
        JPH::MotorSettings bone_motor_settings(const BonePart &bone)
        {
            JPH::MotorSettings motor;
            motor.mSpringSettings.mFrequency =
                bone.drive_stiffness > 0.001F ? bone.drive_stiffness : 8.0F;
            motor.mSpringSettings.mDamping = bone.drive_damping > 0.001F ? bone.drive_damping : 1.0F;
            motor.SetTorqueLimit(bone.drive_max_force > 0.001F ? bone.drive_max_force : 1000.0F);
            return motor;
        }

        // Build the constraint attaching a bone's part to its parent, from the bone's joint kind.
        // Anchored (world space) at the child bone's joint origin, twist along the bone. A zero
        // swing/twist limit falls back to a sensible default so a freshly-imported ragdoll is
        // floppy, not rigid. SwingTwist carries the per-bone PD motors (driven only when active).
        // `joint` is the raw discriminant of the scene `Joint` enum (0 Fixed, 1 Hinge,
        // 2 SwingTwist, 3 Free).
        JPH::Ref<JPH::TwoBodyConstraintSettings>
        build_joint_constraint(const BonePart &bone, JPH::Vec3Arg childPos, JPH::Vec3Arg parentPos)
        {
            const JPH::Vec3 anchor = childPos;
            const JPH::Vec3 along = childPos - parentPos;
            const JPH::Vec3 twist =
                along.Length() > 1e-4F ? along.Normalized() : JPH::Vec3::sAxisY();
            const JPH::Vec3 plane = twist.GetNormalizedPerpendicular();
            constexpr float swingDefault = 0.7F; // ~40°, used when the authored limit is ~0
            const float normalCone =
                bone.swing_twist_limits[0] > 0.001F ? bone.swing_twist_limits[0] : swingDefault;
            const float planeCone =
                bone.swing_twist_limits[1] > 0.001F ? bone.swing_twist_limits[1] : swingDefault;
            const float twistLimit =
                bone.swing_twist_limits[2] > 0.001F ? bone.swing_twist_limits[2] : swingDefault;
            switch (bone.joint)
            {
            case 0: // Fixed
            {
                auto *settings = new JPH::FixedConstraintSettings();
                settings->mAutoDetectPoint = true;
                return settings;
            }
            case 1: // Hinge
            {
                auto *settings = new JPH::HingeConstraintSettings();
                settings->mPoint1 = settings->mPoint2 = anchor;
                settings->mHingeAxis1 = settings->mHingeAxis2 = plane;
                settings->mNormalAxis1 = settings->mNormalAxis2 = twist;
                settings->mLimitsMin = -normalCone;
                settings->mLimitsMax = normalCone;
                return settings;
            }
            case 3: // Free
            {
                auto *settings = new JPH::PointConstraintSettings();
                settings->mPoint1 = settings->mPoint2 = anchor;
                return settings;
            }
            case 2: // SwingTwist
            default:
            {
                auto *settings = new JPH::SwingTwistConstraintSettings();
                settings->mPosition1 = settings->mPosition2 = anchor;
                settings->mTwistAxis1 = settings->mTwistAxis2 = twist;
                settings->mPlaneAxis1 = settings->mPlaneAxis2 = plane;
                settings->mNormalHalfConeAngle = normalCone;
                settings->mPlaneHalfConeAngle = planeCone;
                settings->mTwistMinAngle = -twistLimit;
                settings->mTwistMaxAngle = twistLimit;
                settings->mSwingMotorSettings = bone_motor_settings(bone);
                settings->mTwistMotorSettings = bone_motor_settings(bone);
                return settings;
            }
            }
        }

        // The SwingTwist parent constraint of a ragdoll part, or nullptr when the part is a root
        // (no parent constraint) or its joint is not a SwingTwist. Used by both the subtype query
        // and the motor setter.
        JPH::SwingTwistConstraint *part_swing_twist(const RagdollEntry &entry, std::uint32_t part)
        {
            if (entry.ragdoll == nullptr || entry.settings == nullptr)
            {
                return nullptr;
            }
            const int constraintIdx =
                entry.settings->GetConstraintIndexForBodyIndex(static_cast<int>(part));
            if (constraintIdx < 0)
            {
                return nullptr; // the root bone has no parent constraint
            }
            JPH::TwoBodyConstraint *constraint = entry.ragdoll->GetConstraint(constraintIdx);
            if (constraint == nullptr
                || constraint->GetSubType() != JPH::EConstraintSubType::SwingTwist)
            {
                return nullptr;
            }
            return static_cast<JPH::SwingTwistConstraint *>(constraint);
        }
    } // namespace

    std::uint32_t jolt_add_ragdoll(JoltWorld &world, std::uint64_t rig_uuid,
                                   rust::Slice<const BonePart> parts)
    {
        const std::size_t count = parts.size();

        // Skeleton joints (1:1 with the bones) + capsule parts seeded at each bone's world pose +
        // the parent constraint for every non-root bone.
        const JPH::Ref<JPH::RagdollSettings> settings = new JPH::RagdollSettings();
        settings->mSkeleton = new JPH::Skeleton();
        for (std::size_t i = 0; i < count; i = i + 1)
        {
            settings->mSkeleton->AddJoint(std::to_string(i), parts[i].parent_index);
        }
        settings->mParts.resize(count);
        for (std::size_t i = 0; i < count; i = i + 1)
        {
            const BonePart &bone = parts[i];
            const float radius = std::max(bone.radius, 0.03F);
            const float halfHeight = std::max(bone.half_height, 0.03F);
            const JPH::ShapeRefC shape =
                create_shape(JPH::CapsuleShapeSettings(halfHeight, radius), "ragdoll capsule");
            JPH::RagdollSettings::Part &part = settings->mParts[i];
            part.SetShape(shape);
            part.mPosition = JPH::RVec3(bone.position[0], bone.position[1], bone.position[2]);
            // glam's Quat storage is xyzw — the same component order as JPH::Quat.
            part.mRotation =
                JPH::Quat(bone.rotation[0], bone.rotation[1], bone.rotation[2], bone.rotation[3]);
            part.mMotionType = JPH::EMotionType::Dynamic;
            part.mObjectLayer = static_cast<JPH::ObjectLayer>(ObjectLayer::Moving);
            part.mOverrideMassProperties = JPH::EOverrideMassProperties::CalculateInertia;
            part.mMassPropertiesOverride.mMass = std::max(bone.mass, 0.01F);
            if (bone.parent_index >= 0)
            {
                const BonePart &parent = parts[static_cast<std::size_t>(bone.parent_index)];
                const JPH::Vec3 childPos(bone.position[0], bone.position[1], bone.position[2]);
                const JPH::Vec3 parentPos(parent.position[0], parent.position[1], parent.position[2]);
                part.mToParent = build_joint_constraint(bone, childPos, parentPos);
            }
        }
        settings->Stabilize();
        settings->CalculateBodyIndexToConstraintIndex();
        JPH::Ragdoll *created = settings->CreateRagdoll(0, rig_uuid, &world.system);
        if (created == nullptr)
        {
            std::fprintf(stderr, "physics: CreateRagdoll failed\n");
            return JPH::BodyID::cInvalidBodyID;
        }
        created->AddToPhysicsSystem(JPH::EActivation::Activate);

        RagdollEntry entry;
        entry.settings = settings;
        entry.ragdoll = created;
        world.ragdolls.push_back(std::move(entry));
        return static_cast<std::uint32_t>(world.ragdolls.size() - 1);
    }

    void jolt_remove_ragdoll(JoltWorld &world, std::uint32_t index)
    {
        if (index >= world.ragdolls.size())
        {
            return;
        }
        RagdollEntry &entry = world.ragdolls[index];
        if (entry.ragdoll != nullptr)
        {
            entry.ragdoll->RemoveFromPhysicsSystem();
        }
        world.ragdolls.erase(world.ragdolls.begin() + static_cast<std::ptrdiff_t>(index));
    }

    std::uint32_t jolt_ragdoll_body_count(const JoltWorld &world, std::uint32_t index)
    {
        if (index >= world.ragdolls.size() || world.ragdolls[index].ragdoll == nullptr)
        {
            return 0;
        }
        return static_cast<std::uint32_t>(world.ragdolls[index].ragdoll->GetBodyCount());
    }

    void jolt_ragdoll_part_transform(const JoltWorld &world, std::uint32_t index, std::uint32_t part,
                                     std::array<float, 3> &position, std::array<float, 4> &rotation)
    {
        position = { 0.0F, 0.0F, 0.0F };
        rotation = { 0.0F, 0.0F, 0.0F, 1.0F };
        if (index >= world.ragdolls.size() || world.ragdolls[index].ragdoll == nullptr)
        {
            return;
        }
        const JPH::Ragdoll &ragdoll = *world.ragdolls[index].ragdoll;
        if (part >= ragdoll.GetBodyCount())
        {
            return;
        }
        const JPH::RMat44 world_transform =
            world.system.GetBodyInterface().GetWorldTransform(ragdoll.GetBodyID(static_cast<int>(part)));
        const JPH::RVec3 pos = world_transform.GetTranslation();
        const JPH::Quat rot = world_transform.GetQuaternion();
        position = { pos.GetX(), pos.GetY(), pos.GetZ() };
        rotation = { rot.GetX(), rot.GetY(), rot.GetZ(), rot.GetW() };
    }

    bool jolt_ragdoll_part_is_swing_twist(const JoltWorld &world, std::uint32_t index,
                                          std::uint32_t part)
    {
        if (index >= world.ragdolls.size())
        {
            return false;
        }
        return part_swing_twist(world.ragdolls[index], part) != nullptr;
    }

    void jolt_ragdoll_set_swing_twist_motor(JoltWorld &world, std::uint32_t index, std::uint32_t part,
                                            bool active, const std::array<float, 4> &target)
    {
        if (index >= world.ragdolls.size())
        {
            return;
        }
        JPH::SwingTwistConstraint *constraint = part_swing_twist(world.ragdolls[index], part);
        if (constraint == nullptr)
        {
            return;
        }
        const JPH::EMotorState state = active ? JPH::EMotorState::Position : JPH::EMotorState::Off;
        constraint->SetSwingMotorState(state);
        constraint->SetTwistMotorState(state);
        if (active)
        {
            // glam's Quat storage is xyzw — the same component order as JPH::Quat.
            constraint->SetTargetOrientationBS(
                JPH::Quat(target[0], target[1], target[2], target[3]));
        }
    }

    RayHit jolt_raycast(const JoltWorld &world, const std::array<float, 3> &origin,
                        const std::array<float, 3> &dir, float max_dist)
    {
        const JPH::RRayCast ray{ JPH::RVec3(origin[0], origin[1], origin[2]),
                                 JPH::Vec3(dir[0], dir[1], dir[2]) * max_dist };
        JPH::RayCastResult result;
        if (!world.system.GetNarrowPhaseQuery().CastRay(ray, result))
        {
            return RayHit{ .hit = false,
                           .body = JPH::BodyID::cInvalidBodyID,
                           .point = { 0.0F, 0.0F, 0.0F },
                           .normal = { 0.0F, 0.0F, 0.0F },
                           .distance = 0.0F };
        }
        const JPH::RVec3 point = ray.GetPointOnRay(result.mFraction);
        JPH::Vec3 normal = JPH::Vec3::sZero();
        // The surface normal needs the body, read under a lock so the query never races the sim.
        const JPH::BodyLockRead lock(world.system.GetBodyLockInterface(), result.mBodyID);
        if (lock.Succeeded())
        {
            normal = lock.GetBody().GetWorldSpaceSurfaceNormal(result.mSubShapeID2, point);
        }
        return RayHit{ .hit = true,
                       .body = result.mBodyID.GetIndexAndSequenceNumber(),
                       .point = { point.GetX(), point.GetY(), point.GetZ() },
                       .normal = { normal.GetX(), normal.GetY(), normal.GetZ() },
                       .distance = result.mFraction * max_dist };
    }

    RayHit jolt_sphere_cast(const JoltWorld &world, const std::array<float, 3> &origin,
                            const std::array<float, 3> &dir, float radius, float max_dist)
    {
        const JPH::ShapeRefC sphere =
            create_shape(JPH::SphereShapeSettings(std::max(radius, 0.001F)), "query sphere");
        if (sphere == nullptr)
        {
            return RayHit{ .hit = false,
                           .body = JPH::BodyID::cInvalidBodyID,
                           .point = { 0.0F, 0.0F, 0.0F },
                           .normal = { 0.0F, 0.0F, 0.0F },
                           .distance = 0.0F };
        }
        const JPH::RVec3 base(origin[0], origin[1], origin[2]);
        const JPH::RShapeCast shapeCast = JPH::RShapeCast::sFromWorldTransform(
            sphere, JPH::Vec3::sReplicate(1.0F), JPH::RMat44::sTranslation(base),
            JPH::Vec3(dir[0], dir[1], dir[2]) * max_dist);
        const JPH::ShapeCastSettings settings;
        JPH::ClosestHitCollisionCollector<JPH::CastShapeCollector> collector;
        world.system.GetNarrowPhaseQuery().CastShape(shapeCast, settings, base, collector);
        if (!collector.HadHit())
        {
            return RayHit{ .hit = false,
                           .body = JPH::BodyID::cInvalidBodyID,
                           .point = { 0.0F, 0.0F, 0.0F },
                           .normal = { 0.0F, 0.0F, 0.0F },
                           .distance = 0.0F };
        }
        // mContactPointOn2 is relative to the base offset (origin); the normal is the negated,
        // normalized penetration axis.
        const JPH::Vec3 point = JPH::Vec3(base) + collector.mHit.mContactPointOn2;
        const JPH::Vec3 normal = -collector.mHit.mPenetrationAxis.Normalized();
        return RayHit{ .hit = true,
                       .body = collector.mHit.mBodyID2.GetIndexAndSequenceNumber(),
                       .point = { point.GetX(), point.GetY(), point.GetZ() },
                       .normal = { normal.GetX(), normal.GetY(), normal.GetZ() },
                       .distance = collector.mHit.mFraction * max_dist };
    }
} // namespace saffron::physics
