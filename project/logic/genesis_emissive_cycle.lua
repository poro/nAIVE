-- Floating emissive cubes with intense color cycling

function init()
    self.time = math.random() * 6.28
    self.base_x, self.base_y, self.base_z = entity.get_position(_entity_string_id)
    self.bob_speed = 0.6 + math.random() * 0.4
    self.bob_height = 0.3
    self.spin_speed = 18 + math.random() * 12
    self.color_speed = 0.4 + math.random() * 0.3
    self.phase = math.random() * 6.28
    -- Start bright
    entity.set_emission(_entity_string_id, 2.0, 1.0, 2.0)
end

function update(dt)
    self.time = self.time + dt

    -- Bob and spin
    local y = self.base_y + math.sin(self.time * self.bob_speed) * self.bob_height
    entity.set_position(_entity_string_id, self.base_x, y, self.base_z)
    entity.set_rotation(_entity_string_id, self.time * 8, self.time * self.spin_speed, self.time * 5)

    -- Intense color cycling through RGB spectrum
    local t = self.time * self.color_speed + self.phase
    local r = math.sin(t) * 0.5 + 0.5
    local g = math.sin(t + 2.09) * 0.5 + 0.5
    local b = math.sin(t + 4.19) * 0.5 + 0.5
    -- HDR emission for strong bloom
    entity.set_emission(_entity_string_id, r * 4.0, g * 4.0, b * 4.0)
end
