function init()
    self.time = 0
    self.radius = 10
    self.height = 3
    self.speed = 0.1
end

function update(dt)
    self.time = self.time + dt * self.speed
    local x = self.radius * math.cos(self.time)
    local z = self.radius * math.sin(self.time)
    entity.set_position(_entity_string_id, x, self.height, z)
    local yaw = math.deg(math.atan2(-x, -z))
    local pitch = -math.deg(math.atan2(self.height - 1.5, self.radius))
    entity.set_rotation(_entity_string_id, pitch, yaw, 0)
end
