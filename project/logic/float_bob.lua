function init()
    self.time = math.random() * 6.28
    self.base_x, self.base_y, self.base_z = entity.get_position(_entity_string_id)
    self.bob_speed = 0.8 + math.random() * 0.5
    self.bob_height = 0.3
    self.spin_speed = 15 + math.random() * 10
end

function update(dt)
    self.time = self.time + dt
    local y_offset = math.sin(self.time * self.bob_speed) * self.bob_height
    entity.set_position(_entity_string_id, self.base_x, self.base_y + y_offset, self.base_z)
    local yaw = self.time * self.spin_speed
    entity.set_rotation(_entity_string_id, 0, yaw, 0)
end
