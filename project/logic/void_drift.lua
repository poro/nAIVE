-- Slow drifting orbit for void objects
function init()
    self.time = math.random() * 6.28
    self.base_x, self.base_y, self.base_z = entity.get_position(_entity_string_id)
    self.radius = math.sqrt(self.base_x * self.base_x + self.base_z * self.base_z)
    self.angle = math.atan2(self.base_z, self.base_x)
    self.orbit_speed = 0.015 + math.random() * 0.02
    self.bob_speed = 0.15 + math.random() * 0.1
    self.bob_height = 0.3 + math.random() * 0.5
    self.spin_speed = 5 + math.random() * 8
end

function update(dt)
    self.time = self.time + dt
    self.angle = self.angle + dt * self.orbit_speed
    local x = self.radius * math.cos(self.angle)
    local z = self.radius * math.sin(self.angle)
    local bob = math.sin(self.time * self.bob_speed) * self.bob_height
    entity.set_position(_entity_string_id, x, self.base_y + bob, z)
    entity.set_rotation(_entity_string_id, self.time * 2, self.time * self.spin_speed, self.time * 1.5)
end
