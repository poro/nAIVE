-- Pillars rise quickly from underground with staggered timing

function init()
    self.time = 0
    self.base_x, self.base_y, self.base_z = entity.get_position(_entity_string_id)
    self.start_y = -4
    self.end_y = self.base_y
    -- Quick staggered emergence (0-3 seconds)
    self.delay = math.random() * 2.0
    self.rise_duration = 2.0
    entity.set_position(_entity_string_id, self.base_x, self.start_y, self.base_z)
end

function update(dt)
    self.time = self.time + dt

    local rise_start = self.delay
    local rise_end = rise_start + self.rise_duration

    if self.time < rise_start then
        entity.set_position(_entity_string_id, self.base_x, self.start_y, self.base_z)
    elseif self.time < rise_end then
        local t = (self.time - rise_start) / self.rise_duration
        local e = 1.0 - (1.0 - t) * (1.0 - t)
        local y = self.start_y + (self.end_y - self.start_y) * e
        entity.set_position(_entity_string_id, self.base_x, y, self.base_z)
    else
        -- Subtle bob after settling
        local bob = math.sin(self.time * 0.3) * 0.05
        entity.set_position(_entity_string_id, self.base_x, self.end_y + bob, self.base_z)
    end
end
