-- Levitating object with slow spin and gentle bob
function init()
    self.time = math.random() * 6.28
    self.base_x, self.base_y, self.base_z = entity.get_position(_entity_string_id)
    self.bob_speed = 0.4 + math.random() * 0.3
    self.bob_height = 0.3 + math.random() * 0.4
    self.spin_speed = 10 + math.random() * 15
end

function update(dt)
    self.time = self.time + dt
    local bob = math.sin(self.time * self.bob_speed) * self.bob_height
    entity.set_position(_entity_string_id, self.base_x, self.base_y + bob, self.base_z)
    entity.set_rotation(_entity_string_id, self.time * 3, self.time * self.spin_speed, self.time * 2)
end
