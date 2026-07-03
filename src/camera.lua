local Cam = {}
function Cam.on_update(self, dt)
  local cam = sa.primary_camera()
  if cam:valid() then
    cam:set_position(sa.vec3(0, 5, 10))
  end
end
return Cam
