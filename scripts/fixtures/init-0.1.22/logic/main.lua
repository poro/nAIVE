-- Main game logic script
-- Attached to welcome_cube in scenes/main.yaml

function init()
    self.angle = 0
    log("Hello from " .. _entity_string_id .. "!")
end

function update(dt)
    -- Rotate the cube slowly
    self.angle = self.angle + 30 * dt
    entity.set_rotation(_entity_string_id, 0, self.angle, 0)
end
