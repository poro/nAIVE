-- The HAVOC player: a piercing rifle, a ground slam, and the HUD.
--
--   Hold left mouse  rifle: 10 shots a second, 22 damage to every goblin
--                    along the shot, up to 60 m or the first wall. Shots
--                    fly level at goblin height in the direction you face.
--   E                ground slam: 60 damage within 7 m, every 4 seconds
--   WASD, Shift      move, sprint (you can outrun them)

local SWARM = "goblins"
local GOBLIN_HEIGHT = 0.5   -- the swarm's plane (its transform's y)
local RIFLE_INTERVAL = 0.1
local RIFLE_DAMAGE = 22
local RIFLE_RANGE = 60
local RIFLE_WIDTH = 0.6
local SLAM_COOLDOWN = 4
local SLAM_RADIUS = 7
local SLAM_DAMAGE = 60

-- Where the player's eye is and which way it looks.
local function aim()
    local px, py, pz = entity.get_position(_entity_string_id)
    local pitch, yaw = entity.get_rotation(_entity_string_id)
    local yaw_rad, pitch_rad = math.rad(yaw), math.rad(pitch)
    local dx = -math.sin(yaw_rad) * math.cos(pitch_rad)
    local dy = math.sin(pitch_rad)
    local dz = -math.cos(yaw_rad) * math.cos(pitch_rad)
    return px, py + 0.7, pz, dx, dy, dz
end

local function fire_rifle()
    -- The shot flies level at goblin height, wherever you face: on flat
    -- ground you aim by turning, not by looking down at your feet.
    local ex, _, ez, dx, _, dz = aim()
    local flat = math.sqrt(dx * dx + dz * dz)
    if flat < 0.01 then
        return
    end
    dx, dz = dx / flat, dz / flat
    local sx, sz = ex + dx * 0.5, ez + dz * 0.5
    -- Stop at the first wall.
    local range = RIFLE_RANGE
    local hit, _, dist = physics.hitscan(sx, GOBLIN_HEIGHT, sz, dx, 0, dz, RIFLE_RANGE)
    if hit then
        range = dist
    end
    swarm.damage_line(SWARM, sx, GOBLIN_HEIGHT, sz, sx + dx * range, GOBLIN_HEIGHT, sz + dz * range, RIFLE_WIDTH, RIFLE_DAMAGE)
    if ui then
        ui.flash(1, 0.85, 0.5, 0.06, 0.03)
    end
end

local function slam()
    local px, _, pz = entity.get_position(_entity_string_id)
    swarm.damage_sphere(SWARM, px, GOBLIN_HEIGHT, pz, SLAM_RADIUS, SLAM_DAMAGE)
    if ui then
        camera.shake(0.35, 0.3)
        ui.flash(1, 0.6, 0.2, 0.25, 0.1)
    end
end

function init()
    self.rifle_timer = 0
    self.slam_timer = 0
    self.dead = false
end

function update(dt)
    self.rifle_timer = math.max(0, self.rifle_timer - dt)
    self.slam_timer = math.max(0, self.slam_timer - dt)

    if not self.dead then
        if input.pressed("attack") and self.rifle_timer <= 0 then
            self.rifle_timer = RIFLE_INTERVAL
            fire_rifle()
        end
        if input.just_pressed("interact") and self.slam_timer <= 0 then
            self.slam_timer = SLAM_COOLDOWN
            slam()
        end
    end

    -- The headless test runner has no screen, and no `ui` table.
    if ui then
        draw_hud()
    end
end

function draw_hud()
    local w, h = ui.screen_width(), ui.screen_height()

    -- Health bar
    local hp, max_hp = entity.get_health(_entity_string_id)
    local bar_w, bar_h, bar_x, bar_y = 260, 22, 24, h - 56
    ui.rect(bar_x, bar_y, bar_w, bar_h, 0.15, 0.15, 0.15, 0.85)
    local frac = math.clamp(hp / max_hp, 0, 1)
    ui.rect(bar_x, bar_y, bar_w * frac, bar_h, 1 - frac, frac, 0.1, 0.95)
    ui.text(bar_x + 6, bar_y + 3, math.floor(hp) .. " / " .. math.floor(max_hp), 16, 1, 1, 1, 1)

    -- Slam readiness
    local slam_text = self.slam_timer > 0 and string.format("SLAM %.1f", self.slam_timer) or "SLAM READY [E]"
    ui.text(bar_x + bar_w + 20, bar_y + 3, slam_text, 16, 1, 0.75, 0.3, 1)

    -- Wave, goblins left, kills
    local wave = game.wave or 0
    local alive = swarm.alive(SWARM)
    ui.text(24, 20, "HAVOC", 30, 1, 0.35, 0.2, 1)
    ui.text(24, 56, "WAVE " .. wave .. " / 3", 20, 1, 1, 1, 1)
    ui.text(24, 82, "GOBLINS " .. alive, 20, 0.6, 1, 0.5, 1)
    ui.text(24, 108, "KILLS " .. (game.kills or 0), 20, 1, 0.9, 0.4, 1)

    -- Crosshair
    local cx, cy = w / 2, h / 2
    ui.rect(cx - 1, cy - 9, 2, 18, 1, 1, 1, 0.85)
    ui.rect(cx - 9, cy - 1, 18, 2, 1, 1, 1, 0.85)

    -- Banners
    local banner
    if game.state == "starting" or game.state == "resting" then
        banner = string.format("WAVE %d IN %d", wave + 1, math.ceil(game.next_wave_in or 0))
    elseif game.state == "won" then
        banner = "YOU SURVIVED HAVOC"
    elseif game.state == "lost" then
        banner = "OVERRUN ON WAVE " .. wave
    end
    if banner then
        local size = 34
        ui.text((w - ui.text_width(banner, size)) / 2, h * 0.3, banner, size, 1, 0.9, 0.5, 1)
    end
end

function on_damage(amount, source_id)
    if ui then
        ui.flash(1, 0, 0, 0.2, 0.12)
    end
end

function on_death()
    self.dead = true
    events.emit("havoc.player_died", {})
end
