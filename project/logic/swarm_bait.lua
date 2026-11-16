-- Bait for the GPU swarm demo: runs a figure-eight so the horde has to turn.
function init()
    self.time = 0
    local _, y, _ = entity.get_position(_entity_string_id)
    self.height = y
    self.size = 45      -- half-width of the figure-eight, in metres
    self.rate = 0.11    -- radians per second along the curve
end

function update(dt)
    self.time = self.time + dt
    local t = self.time * self.rate
    local x = self.size * math.sin(t)
    local z = self.size * math.sin(t) * math.cos(t)
    entity.set_position(_entity_string_id, x, self.height, z)
end
