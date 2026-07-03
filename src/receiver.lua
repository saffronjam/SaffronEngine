local Receiver = {}
function Receiver.on_update(self, dt) end
function Receiver.boom(self, sender, payload) error("msg boom") end
function Receiver.ping(self, sender, payload)
  self.entity:set_position(sa.vec3(payload or 0, 0, 0))
end
return Receiver
