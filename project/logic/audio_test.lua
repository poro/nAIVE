-- Audio Test: plays testAssets MP3 files (snake/minesweeper SFX)
-- Demonstrates: audio.play_music, audio.play_sfx, audio.stop_music, ui overlay

local AUDIO_ROOT = "../testAssets/"

-- Schedule of sound effects to play
local SFX_SCHEDULE = {
    { time = 2.0,  id = "eat_1",       file = "eat.mp3",       vol = 0.8, label = "EAT (food pickup)" },
    { time = 4.0,  id = "eat_2",       file = "eat.mp3",       vol = 0.6, label = "EAT (quieter)" },
    { time = 6.0,  id = "explosion_1", file = "explosion.mp3", vol = 0.9, label = "EXPLOSION (mine hit)" },
    { time = 9.0,  id = "eat_3",       file = "eat.mp3",       vol = 0.7, label = "EAT" },
    { time = 11.0, id = "death_1",     file = "death.mp3",     vol = 0.8, label = "DEATH (game over)" },
    { time = 14.0, id = "explosion_2", file = "explosion.mp3", vol = 1.0, label = "EXPLOSION (loud)" },
    { time = 17.0, id = "eat_4",       file = "eat.mp3",       vol = 0.5, label = "EAT (soft)" },
    { time = 19.0, id = "death_2",     file = "death.mp3",     vol = 0.9, label = "DEATH" },
}

function init()
    self.time = 0
    self.schedule_idx = 1
    self.last_played = ""
    self.last_played_time = 0
    self.music_started = false
    self.flash_timer = 0

    -- Spawn visual indicator spheres for each sound type
    entity.spawn("sfx_eat", "procedural:sphere", "assets/materials/neon_green.yaml",
        -3, 0.5, 0, 0.6, 0.6, 0.6)
    entity.spawn("sfx_explosion", "procedural:sphere", "assets/materials/neon_pink.yaml",
        0, 0.5, -3, 0.6, 0.6, 0.6)
    entity.spawn("sfx_death", "procedural:sphere", "assets/materials/neon_cyan.yaml",
        3, 0.5, 0, 0.6, 0.6, 0.6)

    ui.flash(0.1, 0.3, 0.8, 0.5, 0.6)
end

function update(dt)
    self.time = self.time + dt
    self.flash_timer = math.max(0, self.flash_timer - dt)

    local sw = ui.screen_width()
    local sh = ui.screen_height()

    -- Start background music at 0.5 seconds
    if not self.music_started and self.time >= 0.5 then
        self.music_started = true
        audio.play_music(AUDIO_ROOT .. "music.mp3", 0.3, 2.0)
        self.last_played = "MUSIC (fade-in 2s)"
        self.last_played_time = self.time
    end

    -- Play scheduled sound effects
    if self.schedule_idx <= #SFX_SCHEDULE then
        local entry = SFX_SCHEDULE[self.schedule_idx]
        if self.time >= entry.time then
            audio.play_sfx(entry.id, AUDIO_ROOT .. entry.file, entry.vol)
            self.last_played = entry.label .. " [vol=" .. string.format("%.1f", entry.vol) .. "]"
            self.last_played_time = self.time
            self.schedule_idx = self.schedule_idx + 1

            -- Visual feedback
            if string.find(entry.file, "explosion") then
                ui.flash(1.0, 0.4, 0.1, 0.6, 0.4)
                self.flash_timer = 0.3
            elseif string.find(entry.file, "death") then
                ui.flash(0.8, 0.1, 0.1, 0.5, 0.5)
                self.flash_timer = 0.4
            elseif string.find(entry.file, "eat") then
                ui.flash(0.1, 0.8, 0.3, 0.3, 0.2)
                self.flash_timer = 0.15
            end
        end
    end

    -- Pulse the speaker cube
    local pulse = 1.0 + math.sin(self.time * 4) * 0.15
    entity.set_scale("speaker", pulse, pulse, pulse)

    -- Pulse the relevant indicator sphere when a sound plays
    local sfx_pulse = 1.0
    if self.flash_timer > 0 then
        sfx_pulse = 0.6 + self.flash_timer * 3.0
    end

    -- Animate indicator spheres with gentle bob
    local bob = math.sin(self.time * 2) * 0.2
    entity.set_position("sfx_eat", -3, 0.5 + bob, 0)
    entity.set_position("sfx_explosion", 0, 0.5 + math.sin(self.time * 2 + 1) * 0.2, -3)
    entity.set_position("sfx_death", 3, 0.5 + math.sin(self.time * 2 + 2) * 0.2, 0)

    -- ═══════════════════════════════════
    -- HUD: Title bar
    -- ═══════════════════════════════════
    ui.rect(0, 0, sw, 40, 0.0, 0.0, 0.0, 0.8)
    ui.text(10, 8, "AUDIO TEST - Snake Sweeper SFX", 22, 0.3, 0.8, 1.0, 1.0)
    ui.text(sw - 140, 12, "testAssets/", 14, 0.5, 0.5, 0.5, 0.8)

    -- ═══════════════════════════════════
    -- Audio status panel
    -- ═══════════════════════════════════
    local panel_x = 10
    local panel_y = 55
    local panel_w = 320
    local panel_h = 200

    ui.rect(panel_x, panel_y, panel_w, panel_h, 0.05, 0.05, 0.12, 0.8)
    ui.rect(panel_x, panel_y, panel_w, 2, 0.3, 0.8, 1.0, 0.9)

    ui.text(panel_x + 10, panel_y + 10, "AUDIO STATUS", 16, 0.3, 0.8, 1.0, 1.0)

    -- Music status
    local music_status = self.music_started and "Playing" or "Waiting..."
    ui.text(panel_x + 10, panel_y + 35, "Music: " .. music_status, 14, 0.8, 0.8, 0.8, 1.0)
    ui.text(panel_x + 10, panel_y + 55, "File: music.mp3", 12, 0.5, 0.5, 0.5, 1.0)

    -- Last played SFX
    local sfx_age = self.time - self.last_played_time
    local sfx_alpha = math.max(0.3, 1.0 - sfx_age * 0.3)
    ui.text(panel_x + 10, panel_y + 80, "Last SFX:", 14, 0.8, 0.8, 0.8, 1.0)
    if self.last_played ~= "" then
        ui.text(panel_x + 90, panel_y + 80, self.last_played, 14, 1.0, 0.9, 0.3, sfx_alpha)
    end

    -- Schedule progress
    local progress = (self.schedule_idx - 1) / #SFX_SCHEDULE
    ui.text(panel_x + 10, panel_y + 110, string.format("Schedule: %d/%d", self.schedule_idx - 1, #SFX_SCHEDULE), 14, 0.8, 0.8, 0.8, 1.0)

    -- Progress bar
    local bar_x = panel_x + 10
    local bar_y = panel_y + 135
    local bar_w = panel_w - 20
    ui.rect(bar_x, bar_y, bar_w, 10, 0.15, 0.15, 0.25, 1.0)
    ui.rect(bar_x, bar_y, bar_w * progress, 10, 0.3, 0.8, 1.0, 1.0)

    -- Time
    ui.text(panel_x + 10, panel_y + 155, string.format("Time: %.1fs", self.time), 14, 0.6, 0.6, 0.6, 1.0)

    -- Next SFX countdown
    if self.schedule_idx <= #SFX_SCHEDULE then
        local next_entry = SFX_SCHEDULE[self.schedule_idx]
        local countdown = next_entry.time - self.time
        ui.text(panel_x + 10, panel_y + 175, string.format("Next: %s in %.1fs", next_entry.label, countdown), 12, 0.5, 1.0, 0.5, 0.8)
    else
        ui.text(panel_x + 10, panel_y + 175, "All SFX played!", 12, 1.0, 0.9, 0.3, 1.0)
    end

    -- ═══════════════════════════════════
    -- Sound legend (right side)
    -- ═══════════════════════════════════
    local legend_x = sw - 250
    local legend_y = 55

    ui.rect(legend_x, legend_y, 240, 130, 0.05, 0.05, 0.12, 0.8)
    ui.rect(legend_x, legend_y, 240, 2, 0.8, 0.3, 1.0, 0.9)

    ui.text(legend_x + 10, legend_y + 10, "SOUND FILES", 16, 0.8, 0.3, 1.0, 1.0)

    ui.text(legend_x + 10, legend_y + 35, "eat.mp3", 14, 0.2, 0.9, 0.3, 1.0)
    ui.text(legend_x + 120, legend_y + 35, "Food pickup", 12, 0.6, 0.6, 0.6, 1.0)

    ui.text(legend_x + 10, legend_y + 55, "explosion.mp3", 14, 1.0, 0.4, 0.2, 1.0)
    ui.text(legend_x + 120, legend_y + 55, "Mine hit", 12, 0.6, 0.6, 0.6, 1.0)

    ui.text(legend_x + 10, legend_y + 75, "death.mp3", 14, 0.8, 0.2, 0.2, 1.0)
    ui.text(legend_x + 120, legend_y + 75, "Game over", 12, 0.6, 0.6, 0.6, 1.0)

    ui.text(legend_x + 10, legend_y + 100, "music.mp3", 14, 0.3, 0.7, 1.0, 1.0)
    ui.text(legend_x + 120, legend_y + 100, "Background loop", 12, 0.6, 0.6, 0.6, 1.0)

    -- ═══════════════════════════════════
    -- Bottom bar
    -- ═══════════════════════════════════
    ui.rect(0, sh - 28, sw, 28, 0.0, 0.0, 0.0, 0.7)
    ui.text(10, sh - 22, "Source: testAssets/ (Unity snake/minesweeper game)", 12, 0.5, 0.5, 0.5, 1.0)

    -- Animated audio waveform visualization (fake)
    local wave_x = sw / 2 - 100
    local wave_y = sh - 22
    for i = 0, 19 do
        local h = 4 + math.abs(math.sin(self.time * 8 + i * 0.5)) * 10
        local r = 0.3 + math.sin(self.time * 2 + i * 0.3) * 0.2
        ui.rect(wave_x + i * 10, wave_y - h / 2, 6, h, r, 0.6, 1.0, 0.7)
    end
end
