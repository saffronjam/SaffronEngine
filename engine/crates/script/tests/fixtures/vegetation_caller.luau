-- Drives the filtered vegetation queries and the vegetation-event handler against a stub host
-- bridge, recording the observable results into the entity's Transform so the test can assert
-- both that the filter reached the bridge and that a mismatching filter answers as a miss.
local Caller = {}

function Caller:on_create()
	self.events = 0
	self.last_kind = ""
	self.last_health = -1
end

function Caller:on_update(dt)
	-- The filter the stub plant satisfies: same family, tag, lifecycle, and policy.
	local accepting = {
		families = { "7" },
		tags = { 42 },
		lifecycles = { "mature" },
		policies = { "harvestable" },
	}
	-- A filter the stub plant fails on lifecycle alone.
	local rejecting = { lifecycles = { "dead" } }

	local hit = sa.vegetation_raycast(0, 0, 0, 0, 0, 1, 100, accepting)
	local nearest = sa.vegetation_nearest(0, 0, 0, 50, accepting)
	local radius = sa.vegetation_in_radius(0, 0, 0, 50, 8, accepting)
	self.entity:set_position(sa.vec3(hit.hit and 1 or 0, nearest.hit and 1 or 0, #radius))

	local missed = sa.vegetation_raycast(0, 0, 0, 0, 0, 1, 100, rejecting)
	local missed_nearest = sa.vegetation_nearest(0, 0, 0, 50, rejecting)
	local missed_radius = sa.vegetation_in_radius(0, 0, 0, 50, 8, rejecting)
	self.entity:set_rotation(
		sa.vec3(missed.hit and 1 or 0, missed_nearest.hit and 1 or 0, #missed_radius)
	)

	self.entity:set_scale(sa.vec3(self.events, self.last_health, 1))
end

function Caller:on_vegetation_event(event)
	self.events += 1
	self.last_kind = event.kind
	if event.health ~= nil then
		self.last_health = event.health
	end
	sa.log(event.kind .. " " .. tostring(event.plant))
end

return Caller
