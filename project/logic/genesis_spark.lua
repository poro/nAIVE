-- Central spark: always blazing, pulses dramatically

function init()
    self.time = 0
    -- Start bright immediately
    entity.set_emission(_entity_string_id, 6.0, 4.0, 2.0)
end

function update(dt)
    self.time = self.time + dt

    -- Intense pulse between two bright states
    local pulse = math.sin(self.time * 2.0) * 0.4 + 0.6
    local flare = math.sin(self.time * 7.3) * 0.15  -- fast shimmer
    local e = pulse + flare
    entity.set_emission(_entity_string_id, 8.0 * e, 5.0 * e, 2.5 * e)

    -- Slow rotation
    entity.set_rotation(_entity_string_id, self.time * 8, self.time * 15, self.time * 5)
end
