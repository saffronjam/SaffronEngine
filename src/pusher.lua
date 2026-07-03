local Pusher = {}
function Pusher.on_update(self, dt)
  if not self.pushed then self.pushed = true self.entity:apply_impulse(sa.vec3(0, 0, 12)) end
end
return Pusher
