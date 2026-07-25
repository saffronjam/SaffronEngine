// The C++ side of the `cxx` bridge declared in `src/bridge.rs`. Defines the opaque `JoltWorld`
// (Jolt objects + the four virtual shim classes `cxx` cannot synthesize) and declares the free
// functions `cxx` binds; `jolt_bridge.cpp` implements those free functions.
//
// `cxx` emits `#include "jolt_bridge.h"` at the top of the generated `bridge.rs.h`, *before* it
// defines the shared `PendingContact` struct — so this header must not include the generated
// header and must not need `PendingContact`'s definition. It only forward-declares it; the
// `rust::Vec<PendingContact>` return types are mere declarations here, and their bodies (in
// `jolt_bridge.cpp`) see the full definition because that TU includes the generated header.
//
// `JoltWorld` is *fully defined* here (not merely forward-declared) because the `cxx`-generated TU
// returns `UniquePtr<JoltWorld>` by value and so needs the complete type. That pulls `<Jolt/...>`
// into this header — fine, because the only TUs that include it (the generated bridge glue and
// `jolt_bridge.cpp`) are the two compiled with the determinism + AVX2 flags, confined to this
// crate.
//
// The `JPH_*` defines this TU sees must match every Jolt TU's (they change `Vec3`/`Quat`/`RVec3`
// layout), so an ABI mismatch is silent memory corruption, not a link error. The `#error` guards
// fail the build loudly on a determinism flag drift.

#ifndef SAFFRON_JOLT_BRIDGE_H
#define SAFFRON_JOLT_BRIDGE_H

#include <Jolt/Jolt.h>

#include <Jolt/Core/JobSystemThreadPool.h>
#include <Jolt/Core/Reference.h>
#include <Jolt/Core/TempAllocator.h>
#include <Jolt/Physics/Body/Body.h>
#include <Jolt/Physics/Body/BodyCreationSettings.h>
#include <Jolt/Physics/Character/CharacterVirtual.h>
#include <Jolt/Physics/Collision/BroadPhase/BroadPhaseLayer.h>
#include <Jolt/Physics/Collision/ContactListener.h>
#include <Jolt/Physics/Collision/ObjectLayer.h>
#include <Jolt/Physics/Collision/Shape/BoxShape.h>
#include <Jolt/Physics/Collision/Shape/RotatedTranslatedShape.h>
#include <Jolt/Physics/Collision/Shape/SubShapeIDPair.h>
#include <Jolt/Physics/PhysicsSystem.h>
#include <Jolt/Physics/Ragdoll/Ragdoll.h>

#include <array>
#include <cstdint>
#include <memory>
#include <mutex>
#include <vector>

#include "rust/cxx.h"

#ifndef JPH_CROSS_PLATFORM_DETERMINISTIC
#error "saffron-physics-sys: JPH_CROSS_PLATFORM_DETERMINISTIC must be defined — the determinism build is the contract (see plans/.../05-physics-jolt-bridge)."
#endif

#ifdef JPH_DOUBLE_PRECISION
#error "saffron-physics-sys: JPH_DOUBLE_PRECISION must NOT be defined — Jolt must build in single precision for bit-exact cross-platform determinism."
#endif

namespace saffron::physics
{
    // The shared structs `cxx` defines in the generated `bridge.rs.h`. Forward-declared here so the
    // function *declarations* below resolve without the full definitions (which are unavailable
    // this early in the generated header's include order). Every body that *reads or constructs*
    // one lives in `jolt_bridge.cpp`, which includes the generated header.
    struct PendingContact;
    struct BodyCreate;
    struct CharacterCreate;
    struct BonePart;
    struct RayHit;

    // Object-layer slots. The raw `u8` crossing the bridge is one of these.
    enum class ObjectLayer : JPH::ObjectLayer
    {
        Static,
        Moving,
        Character,
        Debris,
        Sensor,
        Count,
    };

    // The v1 collision matrix (symmetric): a sensor overlaps every solid layer to generate
    // triggers but never another sensor; two static bodies never collide; debris collides with the
    // world and characters but not other debris; everything else collides.
    inline bool layers_collide_impl(ObjectLayer a, ObjectLayer b)
    {
        if (a == ObjectLayer::Sensor || b == ObjectLayer::Sensor)
        {
            return !(a == ObjectLayer::Sensor && b == ObjectLayer::Sensor);
        }
        if (a == ObjectLayer::Static && b == ObjectLayer::Static)
        {
            return false;
        }
        if (a == ObjectLayer::Debris && b == ObjectLayer::Debris)
        {
            return false;
        }
        return true;
    }

    // Broad-phase layers (Jolt's coarse AABB tier): everything non-moving in one, everything that
    // moves in the other. v1 keeps it to two.
    namespace BroadPhase
    {
        constexpr JPH::BroadPhaseLayer NonMoving{ 0 };
        constexpr JPH::BroadPhaseLayer Moving{ 1 };
        constexpr JPH::uint Count = 2;
    }

    // Only Static-layer bodies live in the non-moving broad phase; every other object layer is in
    // the moving one, so a sensor over a static floor still pairs through the moving tier.
    inline JPH::BroadPhaseLayer broad_phase_for(ObjectLayer layer)
    {
        return layer == ObjectLayer::Static ? BroadPhase::NonMoving : BroadPhase::Moving;
    }

    // Object layer -> broad-phase layer.
    class BroadPhaseLayerImpl final : public JPH::BroadPhaseLayerInterface
    {
      public:
        JPH::uint GetNumBroadPhaseLayers() const override
        {
            return BroadPhase::Count;
        }

        JPH::BroadPhaseLayer GetBroadPhaseLayer(JPH::ObjectLayer layer) const override
        {
            return broad_phase_for(static_cast<ObjectLayer>(layer));
        }

#if defined(JPH_EXTERNAL_PROFILE) || defined(JPH_PROFILE_ENABLED)
        const char *GetBroadPhaseLayerName(JPH::BroadPhaseLayer layer) const override
        {
            return layer == BroadPhase::NonMoving ? "NonMoving" : "Moving";
        }
#endif
    };

    // "May an object in this layer collide with this broad-phase layer?" (the coarse cull).
    class ObjectVsBroadPhaseImpl final : public JPH::ObjectVsBroadPhaseLayerFilter
    {
      public:
        bool ShouldCollide(JPH::ObjectLayer obj, JPH::BroadPhaseLayer bp) const override
        {
            return static_cast<ObjectLayer>(obj) != ObjectLayer::Static || bp == BroadPhase::Moving;
        }
    };

    // "May these two object layers collide?" — the v1 matrix.
    class ObjectLayerPairImpl final : public JPH::ObjectLayerPairFilter
    {
      public:
        bool ShouldCollide(JPH::ObjectLayer a, JPH::ObjectLayer b) const override
        {
            return layers_collide_impl(static_cast<ObjectLayer>(a), static_cast<ObjectLayer>(b));
        }
    };

    // A raw contact transition captured on a Jolt job thread, before it is translated to the
    // bridge's `PendingContact`. Recording into this Jolt-side POD keeps the callback off the
    // generated `rust::Vec` type (whose member functions are not yet defined this early in the
    // include order); `drain()` does the `PendingContact` translation in `jolt_bridge.cpp`.
    struct RawContact
    {
        std::uint32_t a;
        std::uint32_t b;
        float point[3];
        float normal[3];
        bool begin;
    };

    // Jolt invokes the contact callbacks from job threads during Update, so they must not touch any
    // Rust state directly. They buffer raw POD pairs under a mutex; the sim thread drains them
    // after Update via `drain()`. OnContactPersisted is ignored — v1 emits Begin/End transitions
    // only.
    class ContactListenerImpl final : public JPH::ContactListener
    {
      public:
        void OnContactAdded(const JPH::Body &body1, const JPH::Body &body2,
                            const JPH::ContactManifold &manifold, JPH::ContactSettings &) override
        {
            const JPH::RVec3 point = manifold.GetWorldSpaceContactPointOn1(0);
            const JPH::Vec3 normal = manifold.mWorldSpaceNormal;
            const std::scoped_lock lock(mutex_);
            pending_.push_back(RawContact{
                .a = body1.GetID().GetIndexAndSequenceNumber(),
                .b = body2.GetID().GetIndexAndSequenceNumber(),
                .point = { point.GetX(), point.GetY(), point.GetZ() },
                .normal = { normal.GetX(), normal.GetY(), normal.GetZ() },
                .begin = true,
            });
        }

        void OnContactRemoved(const JPH::SubShapeIDPair &pair) override
        {
            const std::scoped_lock lock(mutex_);
            pending_.push_back(RawContact{
                .a = pair.GetBody1ID().GetIndexAndSequenceNumber(),
                .b = pair.GetBody2ID().GetIndexAndSequenceNumber(),
                .point = { 0.0F, 0.0F, 0.0F },
                .normal = { 0.0F, 0.0F, 0.0F },
                .begin = false,
            });
        }

        // Swap-and-clear the mutex-guarded buffer into the bridge's `rust::Vec<PendingContact>`.
        // Defined in jolt_bridge.cpp (where the generated `PendingContact` is complete);
        // declared here so `JoltWorld` can be defined in this header.
        rust::Vec<PendingContact> drain();

      private:
        std::mutex mutex_;
        std::vector<RawContact> pending_;
    };

    // One live ragdoll: the Jolt `Ragdoll` plus the `RagdollSettings` it references (kept alive so
    // the ragdoll's constraints stay valid). Parts are 1:1 with the rig's bones in index order; the
    // safe layer (`saffron-physics`) holds the bone-index/weight bookkeeping.
    struct RagdollEntry
    {
        JPH::Ref<JPH::RagdollSettings> settings;
        JPH::Ref<JPH::Ragdoll> ragdoll;
    };

    // The Jolt objects behind the opaque handle. The filter interfaces and the contact listener are
    // held by value and declared before `system`, so they are destroyed *after* it (Jolt borrows
    // them for the world's lifetime). `characters` and `ragdolls` reference `system` too, so they
    // are declared after it (destroyed first); the destructor removes every ragdoll from the system
    // before its bodies destruct.
    class JoltWorld
    {
      public:
        BroadPhaseLayerImpl broadPhaseLayer;
        ObjectVsBroadPhaseImpl objectVsBroadPhase;
        ObjectLayerPairImpl objectLayerPair;
        ContactListenerImpl contactListener;
        std::unique_ptr<JPH::TempAllocatorImpl> tempAllocator;
        std::unique_ptr<JPH::JobSystemThreadPool> jobSystem;
        JPH::PhysicsSystem system;
        // CharacterVirtual sweep objects (not bodies); released before `system`, which they
        // reference.
        std::vector<JPH::Ref<JPH::CharacterVirtual>> characters;
        // Live ragdolls; released before `system` (their bodies live in it).
        std::vector<RagdollEntry> ragdolls;

        // Remove every live ragdoll from the system before its bodies are destroyed: `~Ragdoll`
        // destroys its bodies but never removes their constraints, so a ragdoll left live at world
        // teardown would strand dangling constraints and stall the drop. This runs before the
        // members destruct, so the subsequent `~Ragdoll` DestroyBodies is safe.
        ~JoltWorld()
        {
            for (RagdollEntry &entry : ragdolls)
            {
                if (entry.ragdoll != nullptr)
                {
                    entry.ragdoll->RemoveFromPhysicsSystem();
                }
            }
        }
    };

    bool jolt_init();
    void jolt_shutdown();
    std::uint32_t jolt_version();
    bool jolt_is_deterministic();
    bool jolt_is_single_precision();

    bool jolt_layers_collide(std::uint8_t a, std::uint8_t b);

    std::unique_ptr<JoltWorld> jolt_world_new();
    void jolt_world_init(JoltWorld &world);
    std::uint32_t jolt_world_body_count(const JoltWorld &world);
    void jolt_world_step(JoltWorld &world, float dt, std::int32_t collision_steps);
    rust::Vec<PendingContact> jolt_drain_contacts(JoltWorld &world);

    std::uint32_t jolt_create_body(JoltWorld &world, const BodyCreate &create,
                                   rust::Slice<const float> hull_points,
                                   rust::Slice<const float> mesh_vertices,
                                   rust::Slice<const std::uint32_t> mesh_indices);
    rust::Vec<std::uint32_t> jolt_create_static_batch(JoltWorld &world,
                                                      rust::Slice<const BodyCreate> creates);
    void jolt_remove_bodies(JoltWorld &world, rust::Slice<const std::uint32_t> ids);
    void jolt_body_position_rotation(const JoltWorld &world, std::uint32_t id,
                                     std::array<float, 3> &position, std::array<float, 4> &rotation);
    std::array<float, 3> jolt_body_position(const JoltWorld &world, std::uint32_t id);
    bool jolt_body_is_active(const JoltWorld &world, std::uint32_t id);
    std::array<float, 3> jolt_body_linear_velocity(const JoltWorld &world, std::uint32_t id);
    std::array<float, 3> jolt_body_angular_velocity(const JoltWorld &world, std::uint32_t id);
    void jolt_body_add_impulse(JoltWorld &world, std::uint32_t id, const std::array<float, 3> &impulse);
    void jolt_body_add_force(JoltWorld &world, std::uint32_t id, const std::array<float, 3> &force);
    void jolt_body_set_linear_velocity(JoltWorld &world, std::uint32_t id,
                                       const std::array<float, 3> &velocity);
    void jolt_move_kinematic(JoltWorld &world, std::uint32_t id, const std::array<float, 3> &position,
                             const std::array<float, 4> &rotation, float dt);

    std::uint32_t jolt_add_character(JoltWorld &world, const CharacterCreate &create);
    void jolt_character_set_linear_velocity(JoltWorld &world, std::uint32_t index,
                                            const std::array<float, 3> &velocity);
    void jolt_character_extended_update(JoltWorld &world, std::uint32_t index, float dt,
                                        const std::array<float, 3> &gravity, float step_up);
    bool jolt_character_on_ground(const JoltWorld &world, std::uint32_t index);
    std::array<float, 3> jolt_character_position(const JoltWorld &world, std::uint32_t index);
    std::array<float, 3> jolt_world_gravity(const JoltWorld &world);

    std::uint32_t jolt_add_ragdoll(JoltWorld &world, std::uint64_t rig_uuid,
                                   rust::Slice<const BonePart> parts);
    void jolt_remove_ragdoll(JoltWorld &world, std::uint32_t index);
    std::uint32_t jolt_ragdoll_body_count(const JoltWorld &world, std::uint32_t index);
    void jolt_ragdoll_part_transform(const JoltWorld &world, std::uint32_t index, std::uint32_t part,
                                     std::array<float, 3> &position, std::array<float, 4> &rotation);
    bool jolt_ragdoll_part_is_swing_twist(const JoltWorld &world, std::uint32_t index,
                                          std::uint32_t part);
    void jolt_ragdoll_set_swing_twist_motor(JoltWorld &world, std::uint32_t index, std::uint32_t part,
                                            bool active, const std::array<float, 4> &target);

    RayHit jolt_raycast(const JoltWorld &world, const std::array<float, 3> &origin,
                        const std::array<float, 3> &dir, float max_dist);
    RayHit jolt_sphere_cast(const JoltWorld &world, const std::array<float, 3> &origin,
                            const std::array<float, 3> &dir, float radius, float max_dist);
}

#endif // SAFFRON_JOLT_BRIDGE_H
