-- UI & Text Rendering Demo
-- Showcases: ui.text, ui.rect, ui.flash, ui.screen_width/height

function init()
    self.time = 0
    self.score = 0
    self.flash_cooldown = 0
    self.spawn_count = 0
    self.show_panel = true

    -- Trigger a welcome flash
    ui.flash(0.2, 0.5, 1.0, 0.6, 0.8)
end

function update(dt)
    self.time = self.time + dt
    self.flash_cooldown = math.max(0, self.flash_cooldown - dt)

    local sw = ui.screen_width()
    local sh = ui.screen_height()

    -- ═══════════════════════════════════════════
    -- Title bar
    -- ═══════════════════════════════════════════
    ui.rect(0, 0, sw, 40, 0.0, 0.0, 0.0, 0.7)
    ui.text(10, 8, "nAIVE ENGINE - UI DEMO", 24, 1.0, 1.0, 1.0, 1.0)

    -- Version tag (right-aligned)
    ui.text(sw - 120, 12, "v0.16.0", 16, 0.5, 0.8, 1.0, 0.8)

    -- ═══════════════════════════════════════════
    -- HUD panel (left side)
    -- ═══════════════════════════════════════════
    local panel_x = 10
    local panel_y = 60
    local panel_w = 220
    local panel_h = 200

    -- Panel background
    ui.rect(panel_x, panel_y, panel_w, panel_h, 0.05, 0.05, 0.15, 0.75)
    -- Panel border (top)
    ui.rect(panel_x, panel_y, panel_w, 2, 0.3, 0.6, 1.0, 0.9)

    -- Panel title
    ui.text(panel_x + 10, panel_y + 10, "STATS", 18, 0.3, 0.8, 1.0, 1.0)

    -- Score (increments over time)
    self.score = math.floor(self.time * 100)
    ui.text(panel_x + 10, panel_y + 40, "Score: " .. tostring(self.score), 16, 1.0, 1.0, 1.0, 1.0)

    -- FPS estimate
    local fps = math.floor(1.0 / math.max(dt, 0.001))
    ui.text(panel_x + 10, panel_y + 65, "FPS: " .. tostring(fps), 16, 0.0, 1.0, 0.5, 1.0)

    -- Time
    local mins = math.floor(self.time / 60)
    local secs = math.floor(self.time % 60)
    local time_str = string.format("Time: %02d:%02d", mins, secs)
    ui.text(panel_x + 10, panel_y + 90, time_str, 16, 1.0, 0.9, 0.4, 1.0)

    -- Resolution
    ui.text(panel_x + 10, panel_y + 115, string.format("Res: %dx%d", sw, sh), 14, 0.6, 0.6, 0.6, 1.0)

    -- Spawned entities count
    ui.text(panel_x + 10, panel_y + 140, "Spawned: " .. tostring(self.spawn_count), 14, 0.8, 0.6, 1.0, 1.0)

    -- Entity controls hint
    ui.text(panel_x + 10, panel_y + 165, "[Auto-spawning]", 12, 0.5, 0.5, 0.5, 1.0)

    -- ═══════════════════════════════════════════
    -- Bottom status bar
    -- ═══════════════════════════════════════════
    ui.rect(0, sh - 30, sw, 30, 0.0, 0.0, 0.0, 0.7)

    -- Animated dots
    local dots = string.rep(".", math.floor(self.time * 2) % 4)
    ui.text(10, sh - 22, "System running" .. dots, 14, 0.4, 0.8, 0.4, 1.0)

    -- Right-side info
    ui.text(sw - 200, sh - 22, "Text + Rect + Flash", 14, 0.7, 0.7, 0.7, 1.0)

    -- ═══════════════════════════════════════════
    -- Right side: feature showcase cards
    -- ═══════════════════════════════════════════
    local card_x = sw - 260
    local card_y = 60

    -- Card 1: Text sizes
    ui.rect(card_x, card_y, 240, 90, 0.1, 0.05, 0.15, 0.75)
    ui.rect(card_x, card_y, 240, 2, 1.0, 0.5, 0.2, 0.9)
    ui.text(card_x + 10, card_y + 8, "TEXT SIZES", 12, 1.0, 0.5, 0.2, 1.0)
    ui.text(card_x + 10, card_y + 28, "Small 12px", 12, 1.0, 1.0, 1.0, 0.8)
    ui.text(card_x + 10, card_y + 45, "Medium 16px", 16, 1.0, 1.0, 1.0, 0.9)
    ui.text(card_x + 10, card_y + 65, "Large 24px", 24, 1.0, 1.0, 1.0, 1.0)

    -- Card 2: Color palette
    card_y = card_y + 100
    ui.rect(card_x, card_y, 240, 70, 0.1, 0.05, 0.15, 0.75)
    ui.rect(card_x, card_y, 240, 2, 0.2, 1.0, 0.5, 0.9)
    ui.text(card_x + 10, card_y + 8, "COLORS", 12, 0.2, 1.0, 0.5, 1.0)
    ui.text(card_x + 10, card_y + 28, "Red", 16, 1.0, 0.2, 0.2, 1.0)
    ui.text(card_x + 60, card_y + 28, "Green", 16, 0.2, 1.0, 0.2, 1.0)
    ui.text(card_x + 120, card_y + 28, "Blue", 16, 0.2, 0.5, 1.0, 1.0)
    ui.text(card_x + 10, card_y + 48, "Gold", 16, 1.0, 0.85, 0.2, 1.0)
    ui.text(card_x + 60, card_y + 48, "Cyan", 16, 0.2, 1.0, 1.0, 1.0)
    ui.text(card_x + 120, card_y + 48, "Pink", 16, 1.0, 0.4, 0.8, 1.0)

    -- Card 3: Animated elements
    card_y = card_y + 80
    ui.rect(card_x, card_y, 240, 70, 0.1, 0.05, 0.15, 0.75)
    ui.rect(card_x, card_y, 240, 2, 0.8, 0.3, 1.0, 0.9)
    ui.text(card_x + 10, card_y + 8, "ANIMATED", 12, 0.8, 0.3, 1.0, 1.0)

    -- Pulsing alpha text
    local pulse = (math.sin(self.time * 3) + 1) * 0.5
    ui.text(card_x + 10, card_y + 28, "Pulse!", 20, 1.0, 1.0, 1.0, 0.3 + pulse * 0.7)

    -- Color cycling text
    local r = (math.sin(self.time * 2) + 1) * 0.5
    local g = (math.sin(self.time * 2 + 2.094) + 1) * 0.5
    local b = (math.sin(self.time * 2 + 4.189) + 1) * 0.5
    ui.text(card_x + 100, card_y + 28, "Rainbow", 20, r, g, b, 1.0)

    -- Moving progress bar
    local bar_w = 220
    local progress = (math.sin(self.time * 0.5) + 1) * 0.5
    ui.rect(card_x + 10, card_y + 52, bar_w, 8, 0.15, 0.15, 0.25, 1.0)
    ui.rect(card_x + 10, card_y + 52, bar_w * progress, 8, 0.3, 0.8, 1.0, 1.0)

    -- ═══════════════════════════════════════════
    -- Center: large animated title
    -- ═══════════════════════════════════════════
    local title_alpha = (math.sin(self.time * 1.5) + 1) * 0.3 + 0.4
    local title_text = "nAIVE"
    local title_size = 48
    local title_w = #title_text * title_size * 0.75 -- approximate
    ui.text((sw - title_w) / 2, sh / 2 - 80, title_text, title_size, 1.0, 0.9, 0.3, title_alpha)

    -- ═══════════════════════════════════════════
    -- Auto-spawn entities every 3 seconds (up to 8)
    -- ═══════════════════════════════════════════
    if self.spawn_count < 8 and math.floor(self.time / 3) > self.spawn_count then
        self.spawn_count = self.spawn_count + 1
        local angle = (self.spawn_count / 8) * math.pi * 2
        local sx = math.cos(angle) * 4
        local sz = math.sin(angle) * 4
        local id = "spawned_" .. tostring(self.spawn_count)
        entity.spawn(id, "procedural:sphere", "assets/materials/neon_cyan.yaml", sx, 0.5, sz, 0.4, 0.4, 0.4)

        -- Flash on spawn
        ui.flash(0.0, 0.8, 1.0, 0.3, 0.3)
    end

    -- Animate spawned entities: bob up and down
    for i = 1, self.spawn_count do
        local id = "spawned_" .. tostring(i)
        local angle = (i / 8) * math.pi * 2
        local sx = math.cos(angle + self.time * 0.5) * 4
        local sz = math.sin(angle + self.time * 0.5) * 4
        local sy = 0.5 + math.sin(self.time * 2 + i) * 0.3
        entity.set_position(id, sx, sy, sz)
    end

    -- Blink visibility of last spawned entity
    if self.spawn_count > 0 then
        local last_id = "spawned_" .. tostring(self.spawn_count)
        local visible = math.floor(self.time * 4) % 2 == 0
        entity.set_visible(last_id, visible)
    end
end
