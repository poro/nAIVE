-- Tier 2.5 Demo: Physics Impulse, Force & Velocity
-- Press 1-4 to apply different physics operations to the balls.

local balls = { "ball_1", "ball_2", "ball_3", "ball_4" }
local labels = {
    "1: Impulse (up)",
    "2: Force (push right)",
    "3: Set Velocity",
    "4: Read Velocities",
}

function init()
    self.show_velocities = false
    self.vel_display = {}
    -- Color each ball differently
    entity.set_base_color("ball_1", 0.9, 0.2, 0.2)
    entity.set_base_color("ball_2", 0.2, 0.9, 0.2)
    entity.set_base_color("ball_3", 0.2, 0.3, 0.9)
    entity.set_base_color("ball_4", 0.9, 0.8, 0.1)
    -- Color pedestals to match
    entity.set_base_color("pedestal_1", 0.5, 0.1, 0.1)
    entity.set_base_color("pedestal_2", 0.1, 0.5, 0.1)
    entity.set_base_color("pedestal_3", 0.1, 0.15, 0.5)
    entity.set_base_color("pedestal_4", 0.5, 0.4, 0.05)
end

function update(dt)
    -- Key 1: Apply upward impulse to ball_1
    if input.just_pressed("slot1") then
        physics.apply_impulse("ball_1", 0, 15, 0)
        self.show_velocities = false
    end

    -- Key 2: Apply sustained force to ball_2 (push right)
    if input.pressed("slot2") then
        physics.apply_force("ball_2", 40, 0, 0)
        self.show_velocities = false
    end

    -- Key 3: Set velocity on ball_3 directly
    if input.just_pressed("slot3") then
        physics.set_velocity("ball_3", 0, 10, -5)
        self.show_velocities = false
    end

    -- Key 4: Read and display all velocities
    if input.just_pressed("slot4") then
        self.show_velocities = true
        self.vel_display = {}
        for i, bid in ipairs(balls) do
            if entity.exists(bid) then
                local vx, vy, vz = physics.get_velocity(bid)
                self.vel_display[i] = string.format("%s: (%.1f, %.1f, %.1f)", bid, vx, vy, vz)
            else
                self.vel_display[i] = bid .. ": [gone]"
            end
        end
    end

    -- Always read velocities for live display
    if self.show_velocities then
        for i, bid in ipairs(balls) do
            if entity.exists(bid) then
                local vx, vy, vz = physics.get_velocity(bid)
                self.vel_display[i] = string.format("%s: (%.1f, %.1f, %.1f)", bid, vx, vy, vz)
            end
        end
    end

    -- Draw HUD
    local sw = ui.screen_width()
    local sh = ui.screen_height()

    -- Title
    ui.text(sw * 0.5 - 140, 20, "IMPULSE & FORCE DEMO", 24, 1, 1, 1, 1)

    -- Instructions
    local y = 60
    for i, label in ipairs(labels) do
        ui.text(20, y, label, 16, 0.8, 0.8, 0.8, 1)
        y = y + 22
    end
    ui.text(20, y + 5, "Hold 2 for sustained force", 14, 0.6, 0.6, 0.6, 1)

    -- Velocity readout
    if self.show_velocities and #self.vel_display > 0 then
        ui.rect(sw - 310, 55, 290, 110, 0, 0, 0, 0.6)
        ui.text(sw - 300, 60, "VELOCITIES:", 16, 0.3, 1.0, 0.3, 1)
        local vy = 82
        for i, line in ipairs(self.vel_display) do
            ui.text(sw - 300, vy, line, 14, 0.9, 0.9, 0.9, 1)
            vy = vy + 20
        end
    end

    -- Ball labels (world-to-screen)
    local ball_labels = { "IMPULSE", "FORCE", "SET VEL", "READ VEL" }
    for i, bid in ipairs(balls) do
        if entity.exists(bid) then
            local bx, by, bz = entity.get_position(bid)
            local sx, sy, vis = camera.world_to_screen(bx, by + 1.2, bz)
            if vis then
                ui.text(sx - 25, sy, ball_labels[i], 14, 1, 1, 1, 0.9)
            end
        end
    end
end
