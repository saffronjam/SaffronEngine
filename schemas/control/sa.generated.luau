---@meta
-- Saffron Anima Lua API. Generated from the saffron-script binding table; do not edit by hand.
-- Types only: the real bindings are the mlua registration walk over the same table.

---@class sa.Vec3
---@field x number
---@field y number
---@field z number
---@operator add(sa.Vec3): sa.Vec3
---@operator sub(sa.Vec3): sa.Vec3
---@operator mul(number): sa.Vec3
---@operator unm: sa.Vec3
local Vec3 = {}
function Vec3:length() end ---@return number
function Vec3:normalized() end ---@return sa.Vec3
function Vec3:dot(other) end ---@param other sa.Vec3 @return number
function Vec3:cross(other) end ---@param other sa.Vec3 @return sa.Vec3
function Vec3:lerp(other, t) end ---@param other sa.Vec3 @param t number @return sa.Vec3

---@class sa.RayHit
---@field hit boolean
---@field distance number
---@field point sa.Vec3
---@field normal sa.Vec3
---@field entity sa.Entity?

---@class sa.RagdollState
---@field present boolean
---@field active boolean
---@field body_weight number
---@field bones integer

---@alias sa.ComponentName "Name"|"Transform"|"Mesh"|"Camera"|"MaterialSet"|"ModelInstance"|"Script"|"AnimationPlayer"|"DirectionalLight"|"PointLight"|"SpotLight"|"ReflectionProbe"|"FogVolume"|"Relationship"|"SkinnedMesh"|"Morph"|"Bone"|"FootIk"|"BonePhysics"|"Rigidbody"|"Collider"|"KinematicBones"|"CharacterController"

---@class sa.Entity
local Entity = {}
function Entity:valid() end ---@return boolean
function Entity:name() end ---@return string
function Entity:uuid() end ---@return string
function Entity:get_position() end ---@return sa.Vec3
function Entity:get_rotation() end ---@return sa.Vec3
function Entity:get_scale() end ---@return sa.Vec3
function Entity:get_world_position() end ---@return sa.Vec3
function Entity:get_world_rotation() end ---@return sa.Vec3
function Entity:set_position(value) end ---@param value sa.Vec3
function Entity:set_rotation(value) end ---@param value sa.Vec3
function Entity:set_scale(value) end ---@param value sa.Vec3
function Entity:set_component(name, value) end ---@param name sa.ComponentName @param value table @return boolean
function Entity:add_component(name) end ---@param name sa.ComponentName @return boolean
function Entity:remove_component(name) end ---@param name sa.ComponentName @return boolean
function Entity:has_component(name) end ---@param name sa.ComponentName @return boolean
function Entity:destroy() end
function Entity:set_parent(parent) end ---@param parent sa.Entity @return boolean
function Entity:parent() end ---@return sa.Entity
function Entity:children() end ---@return sa.Entity[]
function Entity:send(handler, payload) end ---@param handler string @param payload any
function Entity:move_character(velocity, jump) end ---@param velocity sa.Vec3 @param jump boolean
function Entity:apply_impulse(impulse) end ---@param impulse sa.Vec3
function Entity:add_force(force) end ---@param force sa.Vec3
function Entity:set_velocity(velocity) end ---@param velocity sa.Vec3
function Entity:set_morph_weights(weights) end ---@param weights number[]
function Entity:get_velocity() end ---@return sa.Vec3
function Entity:enable_ragdoll() end ---@return boolean
function Entity:disable_ragdoll() end
function Entity:set_ragdoll_blend(active, weight) end ---@param active boolean @param weight number
function Entity:ragdoll_state() end ---@return sa.RagdollState

---@class sa.ScriptSelf
---@field entity sa.Entity
local ScriptSelf = {}
function ScriptSelf:on_create() end
function ScriptSelf:on_update(dt) end ---@param dt number
function ScriptSelf:on_destroy() end
function ScriptSelf:on_trigger_enter(other) end ---@param other sa.Entity
function ScriptSelf:on_trigger_exit(other) end ---@param other sa.Entity
function ScriptSelf:on_contact(other, point, normal) end ---@param other sa.Entity @param point sa.Vec3 @param normal sa.Vec3

sa = {}
function sa.vec3(x, y, z) end ---@param x number @param y number @param z number @return sa.Vec3
function sa.lerp(a, b, t) end ---@param a sa.Vec3 @param b sa.Vec3 @param t number @return sa.Vec3
function sa.look_at(eye, target, up) end ---@param eye sa.Vec3 @param target sa.Vec3 @param up sa.Vec3 @return sa.Vec3
function sa.log(message) end ---@param message string
function sa.is_key_down(key) end ---@param key string @return boolean
function sa.is_key_pressed(key) end ---@param key string @return boolean
function sa.is_key_up(key) end ---@param key string @return boolean
function sa.mouse_position() end ---@return sa.Vec3
function sa.mouse_delta() end ---@return sa.Vec3
function sa.is_mouse_down(button) end ---@param button string @return boolean
function sa.is_mouse_pressed(button) end ---@param button string @return boolean
function sa.is_mouse_up(button) end ---@param button string @return boolean
function sa.mouse_scroll() end ---@return number
function sa.get_entity_by_name(name) end ---@param name string @return sa.Entity
function sa.find_all_by_name(name) end ---@param name string @return sa.Entity[]
function sa.find_by_uuid(uuid) end ---@param uuid string @return sa.Entity
function sa.primary_camera() end ---@return sa.Entity
function sa.spawn(name) end ---@param name string @return sa.Entity
function sa.broadcast(handler, payload) end ---@param handler string @param payload any
function sa.raycast(ox, oy, oz, dx, dy, dz, max_dist) end ---@param ox number @param oy number @param oz number @param dx number @param dy number @param dz number @param max_dist number @return sa.RayHit
function sa.spherecast(ox, oy, oz, dx, dy, dz, radius, max_dist) end ---@param ox number @param oy number @param oz number @param dx number @param dy number @param dz number @param radius number @param max_dist number @return sa.RayHit
function sa.spawn_task(fn) end ---@param fn any @return any
function sa.wait(seconds) end ---@param seconds number @return number
function sa.delay(seconds, fn) end ---@param seconds number @param fn any @return any

-- Typed component snapshots. get_component(name) returns the component as a read-only table in
-- its serialized wire shape (vectors as {x,y,z} tables, ids as decimal strings); nil when absent.
---@class sa.AnimationPlayer
---@field clip string
---@field autoplay boolean
---@field speed number
---@field wrap "once"|"loop"|"pingpong"
---@field transitionMode "inertialize"|"crossfade"
---@field loopBlend number

---@class sa.BVec3
---@field x boolean
---@field y boolean
---@field z boolean

---@class sa.Bone

---@class sa.BonePhysics
---@field bones sa.BonePhysicsDto[]

---@class sa.BonePhysicsDto
---@field shapeHalfExtents { x: number, y: number, z: number }
---@field mass number
---@field joint "fixed"|"hinge"|"swingtwist"|"free"
---@field swingTwistLimits { x: number, y: number, z: number }
---@field driveStiffness number
---@field driveDamping number
---@field driveMaxForce number

---@class sa.Camera
---@field fov number
---@field near number
---@field far number
---@field primary boolean
---@field showModel boolean
---@field showFrustum boolean
---@field frustumMaxDistance number

---@class sa.CharacterController
---@field maxSpeed number
---@field maxSlopeAngle number
---@field maxStepHeight number
---@field gravityFactor number

---@class sa.Collider
---@field shape "box"|"sphere"|"capsule"|"convexhull"|"mesh"
---@field halfExtents { x: number, y: number, z: number }
---@field sourceMesh string
---@field offset { x: number, y: number, z: number }
---@field material sa.PhysicsMaterial
---@field isSensor boolean

---@class sa.DirectionalLight
---@field atmosphereRole "sun"|"moon"
---@field direction { x: number, y: number, z: number }
---@field color { x: number, y: number, z: number }
---@field intensity number
---@field ambient number
---@field volumetricScattering number
---@field castVolumetricShadow boolean

---@class sa.FogVolume
---@field shape "box"|"sphere"
---@field extents { x: number, y: number, z: number }
---@field radius number
---@field edgeFalloff number
---@field density number
---@field albedo { x: number, y: number, z: number }
---@field emissive { x: number, y: number, z: number }
---@field phaseG number
---@field heightFalloff number
---@field noiseScale number
---@field noiseIntensity number
---@field noiseDetail number
---@field wind { x: number, y: number, z: number }
---@field speed number

---@class sa.FootChainDto
---@field upper number
---@field mid number
---@field end number
---@field poleVector { x: number, y: number, z: number }

---@class sa.FootIk
---@field enabled boolean
---@field groundHeight number
---@field chains sa.FootChainDto[]

---@class sa.KinematicBones
---@field enabled boolean
---@field driven number[]

---@class sa.MaterialSet
---@field slots sa.MaterialSlot[]

---@class sa.MaterialSlot
---@field material string
---@field overrides table<string, any>

---@class sa.Mesh
---@field mesh string

---@class sa.ModelInstance
---@field modelId string

---@class sa.Morph
---@field weights number[]
---@field names string[]

---@class sa.Name
---@field name string

---@class sa.PhysicsMaterial
---@field friction number
---@field restitution number

---@class sa.PointLight
---@field color { x: number, y: number, z: number }
---@field intensity number
---@field range number
---@field volumetricScattering number
---@field castVolumetricShadow boolean

---@class sa.ReflectionProbe
---@field influenceRadius number
---@field intensity number
---@field boxProjection boolean
---@field boxExtent { x: number, y: number, z: number }

---@class sa.Relationship
---@field parent string

---@class sa.Rigidbody
---@field motion "static"|"kinematic"|"dynamic"
---@field mass number
---@field linearDamping number
---@field angularDamping number
---@field gravityFactor number
---@field lockPosition sa.BVec3
---@field lockRotation sa.BVec3
---@field collisionLayer number

---@class sa.Script
---@field scripts sa.ScriptSlot[]

---@class sa.ScriptSlot
---@field scriptPath string
---@field overrides table<string, any>

---@class sa.SkinnedMesh
---@field mesh string
---@field rootBone string
---@field bones string[]
---@field inverseBind number[][]

---@class sa.SpotLight
---@field direction { x: number, y: number, z: number }
---@field color { x: number, y: number, z: number }
---@field intensity number
---@field range number
---@field innerAngle number
---@field outerAngle number
---@field volumetricScattering number
---@field castVolumetricShadow boolean

---@class sa.Transform
---@field translation { x: number, y: number, z: number }
---@field scale { x: number, y: number, z: number }
---@field rotation { x: number, y: number, z: number }

---@overload fun(self: sa.Entity, name: "AnimationPlayer"): sa.AnimationPlayer?
---@overload fun(self: sa.Entity, name: "Bone"): sa.Bone?
---@overload fun(self: sa.Entity, name: "BonePhysics"): sa.BonePhysics?
---@overload fun(self: sa.Entity, name: "Camera"): sa.Camera?
---@overload fun(self: sa.Entity, name: "CharacterController"): sa.CharacterController?
---@overload fun(self: sa.Entity, name: "Collider"): sa.Collider?
---@overload fun(self: sa.Entity, name: "DirectionalLight"): sa.DirectionalLight?
---@overload fun(self: sa.Entity, name: "FogVolume"): sa.FogVolume?
---@overload fun(self: sa.Entity, name: "FootIk"): sa.FootIk?
---@overload fun(self: sa.Entity, name: "KinematicBones"): sa.KinematicBones?
---@overload fun(self: sa.Entity, name: "MaterialSet"): sa.MaterialSet?
---@overload fun(self: sa.Entity, name: "Mesh"): sa.Mesh?
---@overload fun(self: sa.Entity, name: "ModelInstance"): sa.ModelInstance?
---@overload fun(self: sa.Entity, name: "Morph"): sa.Morph?
---@overload fun(self: sa.Entity, name: "Name"): sa.Name?
---@overload fun(self: sa.Entity, name: "PointLight"): sa.PointLight?
---@overload fun(self: sa.Entity, name: "ReflectionProbe"): sa.ReflectionProbe?
---@overload fun(self: sa.Entity, name: "Relationship"): sa.Relationship?
---@overload fun(self: sa.Entity, name: "Rigidbody"): sa.Rigidbody?
---@overload fun(self: sa.Entity, name: "Script"): sa.Script?
---@overload fun(self: sa.Entity, name: "SkinnedMesh"): sa.SkinnedMesh?
---@overload fun(self: sa.Entity, name: "SpotLight"): sa.SpotLight?
---@overload fun(self: sa.Entity, name: "Transform"): sa.Transform?
function Entity:get_component(name) end  ---@param name sa.ComponentName @return table?
