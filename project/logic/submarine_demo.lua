-- Submarine demo: slow turntable rotation + periodic ball rain
function init()
    self.angle = 0
    self.speed = 15  -- degrees per second
    self.timer = 0
    self.drop_interval = 3   -- seconds between ball drops
    self.ball_count = 50
    self.dropped = false
end

function update(dt)
    -- Slow turntable rotation
    self.angle = self.angle + self.speed * dt
    entity.set_rotation(_entity_string_id, 0, self.angle, 0)

    -- Periodic ball rain
    self.timer = self.timer + dt
    if self.timer >= self.drop_interval then
        self.timer = 0
        for i = 1, self.ball_count do
            local x = (math.random() - 0.5) * 8
            local y = 15 + math.random() * 10
            local z = (math.random() - 0.5) * 8
            entity.spawn_dynamic(
                "procedural:sphere", "procedural:default",
                x, y, z,
                0, -2, 0,
                0.2,   -- radius
                2.0,   -- mass
                0.7,   -- restitution (bouncy)
                0.3,   -- friction
                15.0   -- lifetime
            )
        end
    end

    if ui then
        ui.text(20, 20, "Submarine Demo", 24, 1, 1, 1, 1)
        ui.text(20, 50, "Textured GLB + Physics Ball Rain", 14, 0.7, 0.7, 0.7, 1)
        ui.text(20, 74, "WASD to move | Scroll to zoom", 14, 0.5, 0.5, 0.5, 1)
    end
end
