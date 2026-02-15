-- tier2_pickup_bob.lua — Simple bobbing animation for pickup items.

function init()
    self.time = math.random() * 6.28
    self.base_x, self.base_y, self.base_z = entity.get_position(_entity_string_id)
end

function update(dt)
    self.time = self.time + dt
    local bob = math.sin(self.time * 2) * 0.3
    entity.set_position(_entity_string_id, self.base_x, self.base_y + bob, self.base_z)
    entity.set_rotation(_entity_string_id, 0, self.time * 40, 0)
end
