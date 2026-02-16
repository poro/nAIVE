-- Tier 2.5 Demo: Collider Materials (Restitution & Friction)
-- Balls drop onto surfaces with varying restitution (left side).
-- Balls slide down a ramp with varying friction (right side).
-- Press 1 to drop bounce balls, Press 2 to spawn ramp balls.

local bounce_x = { -8, -4, 0, 4, 8 }
local bounce_rest = { 0.0, 0.25, 0.5, 0.75, 1.0 }
local bounce_pads = { "bounce_pad_0", "bounce_pad_25", "bounce_pad_50", "bounce_pad_75", "bounce_pad_100" }

local friction_vals = { 0.0, 0.25, 0.5, 0.75, 1.0 }
local ramp_lane_x = { -8, -4, 0, 4, 8 }

function init()
    self.bounce_gen = 0
    self.ramp_gen = 0
    self.time = 0
    self.auto_timer = 0

    -- Color bounce pads by restitution (blue = low, red = high)
    for i, pad_id in ipairs(bounce_pads) do
        local t = (i - 1) / 4.0
        entity.set_base_color(pad_id, t, 0.2, 1.0 - t)
    end

    -- Color the ramp
    entity.set_base_color("ramp", 0.3, 0.3, 0.35)
    entity.set_metallic("ramp", 0.2)
end

function update(dt)
    self.time = self.time + dt
    self.auto_timer = self.auto_timer + dt

    -- Auto-spawn bounce balls every 3 seconds, or press 1
    local spawn_bounce = input.just_pressed("slot1") or self.auto_timer > 3.0

    if spawn_bounce then
        if self.auto_timer > 3.0 then self.auto_timer = 0 end
        self.bounce_gen = self.bounce_gen + 1

        for i = 1, 5 do
            local bid = "bounce_" .. self.bounce_gen .. "_" .. i
            entity.spawn(
                bid,
                "procedural:sphere",
                "assets/materials/default.yaml",
                bounce_x[i], 8, -6,
                0.4, 0.4, 0.4
            )
            -- Ball restitution matches the pad it drops onto
            physics.set_restitution(bid, bounce_rest[i])
            -- Color the ball to match its pad
            local t = (i - 1) / 4.0
            entity.set_base_color(bid, t, 0.2, 1.0 - t)
        end
    end

    -- Press 2: Spawn balls at top of ramp with different friction
    if input.just_pressed("slot2") then
        self.ramp_gen = self.ramp_gen + 1

        for i = 1, 5 do
            local rid = "ramp_" .. self.ramp_gen .. "_" .. i
            entity.spawn(
                rid,
                "procedural:sphere",
                "assets/materials/default.yaml",
                ramp_lane_x[i], 6, 4,
                0.4, 0.4, 0.4
            )
            physics.set_friction(rid, friction_vals[i])
            -- Color: green = low friction (slippery), yellow = high friction (grippy)
            local t = (i - 1) / 4.0
            entity.set_base_color(rid, 0.2 + 0.8 * t, 0.9 - 0.5 * t, 0.1)
        end
    end

    -- Clean up balls that fall off
    for gen = 1, self.bounce_gen do
        for i = 1, 5 do
            local bid = "bounce_" .. gen .. "_" .. i
            if entity.exists(bid) then
                local x, y, z = entity.get_position(bid)
                if y < -3 then entity.destroy(bid) end
            end
        end
    end
    for gen = 1, self.ramp_gen do
        for i = 1, 5 do
            local rid = "ramp_" .. gen .. "_" .. i
            if entity.exists(rid) then
                local x, y, z = entity.get_position(rid)
                if y < -3 then entity.destroy(rid) end
            end
        end
    end

    -- Draw HUD
    local sw = ui.screen_width()
    local sh = ui.screen_height()

    ui.text(sw * 0.5 - 130, 20, "MATERIAL PROPERTIES DEMO", 24, 1, 1, 1, 1)

    ui.rect(15, 55, 330, 75, 0, 0, 0, 0.6)
    ui.text(20, 60, "1: Drop bounce balls (auto every 3s)", 16, 0.8, 0.8, 0.8, 1)
    ui.text(20, 82, "2: Spawn ramp balls (friction test)", 16, 0.8, 0.8, 0.8, 1)
    ui.text(20, 106, "Restitution: 0.0 (blue) to 1.0 (red)", 14, 0.6, 0.6, 0.6, 1)

    -- Restitution labels over each pad
    for i, pad_id in ipairs(bounce_pads) do
        local px, py, pz = entity.get_position(pad_id)
        local sx, sy, vis = camera.world_to_screen(px, py + 0.8, pz)
        if vis then
            ui.text(sx - 15, sy, string.format("%.2f", bounce_rest[i]), 12, 1, 1, 1, 0.8)
        end
    end

    -- Friction labels
    local ramp_label_z = 12
    for i = 1, 5 do
        local sx, sy, vis = camera.world_to_screen(ramp_lane_x[i], 1, ramp_label_z)
        if vis then
            ui.text(sx - 20, sy, string.format("f=%.2f", friction_vals[i]), 12, 1, 1, 1, 0.8)
        end
    end

    -- Section labels
    local bsx, bsy, bvis = camera.world_to_screen(0, 9.5, -6)
    if bvis then
        ui.text(bsx - 40, bsy, "RESTITUTION", 16, 0.5, 0.5, 1.0, 1)
    end
    local rsx, rsy, rvis = camera.world_to_screen(0, 7, 8)
    if rvis then
        ui.text(rsx - 25, rsy, "FRICTION", 16, 0.5, 1.0, 0.5, 1)
    end
end
