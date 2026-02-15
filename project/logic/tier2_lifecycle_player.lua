-- tier2_lifecycle_player.lua — Demonstrates Tier 2 systems:
--   System 2: Safe entity lifecycle (destroy + spawn same frame)
--   System 3: Runtime entity queries (find_by_tag, has_tag, add_tag, remove_tag)
--   System 4: Event subscription (events.on)
--   System 5: Entity pooling (pool_create, pool_acquire, pool_release)
--
-- Controls:
--   WASD      = move
--   Mouse     = look
--   1         = find all enemies (find_by_tag demo)
--   2         = find only goblins vs demons
--   3         = destroy + respawn entity same frame (lifecycle fix)
--   4         = acquire from pool / release back
--   E         = tag nearest enemy as "marked"

function init()
    self.time = 0
    self.marked_count = 0
    self.pool_items = {}
    self.pool_acquired = 0
    self.event_log = {}
    self.key_cooldown = {}

    -- System 4: Event subscription demo — listen for events
    events.on("enemy.marked", function(e)
        local msg = "Event: enemy.marked — " .. (e.data.id or "?")
        table.insert(self.event_log, 1, msg)
        if #self.event_log > 5 then table.remove(self.event_log, 6) end
    end)

    events.on("pool.acquired", function(e)
        local msg = "Event: pool.acquired — " .. (e.data.id or "?")
        table.insert(self.event_log, 1, msg)
        if #self.event_log > 5 then table.remove(self.event_log, 6) end
    end)

    events.on("lifecycle.respawn", function(e)
        local msg = "Event: lifecycle.respawn — " .. (e.data.id or "?")
        table.insert(self.event_log, 1, msg)
        if #self.event_log > 5 then table.remove(self.event_log, 6) end
    end)

    -- System 5: Create an entity pool
    entity.pool_create("bullet_pool", "procedural:sphere", "assets/materials/bullet.yaml", 10)
    log("Created bullet_pool with 10 pre-warmed entities")

    log("=== TIER 2 LIFECYCLE DEMO ===")
    log("[1] find_by_tag  [2] find goblins vs demons  [3] destroy+spawn  [4] pool acquire/release  [E] tag enemy")
end

function cooldown_ready(key, interval)
    if not self.key_cooldown[key] then self.key_cooldown[key] = 0 end
    if self.key_cooldown[key] <= 0 then
        self.key_cooldown[key] = interval
        return true
    end
    return false
end

function update(dt)
    self.time = self.time + dt
    for k, v in pairs(self.key_cooldown) do
        self.key_cooldown[k] = math.max(0, v - dt)
    end

    -- [1] System 3: Find all enemies by tag
    if input.just_pressed("slot1") and cooldown_ready("1", 0.3) then
        local enemies = scene.find_by_tag("enemy")
        local count = 0
        for _, id in ipairs(enemies) do count = count + 1 end
        log("[find_by_tag] Found " .. count .. " entities with 'enemy' tag")
    end

    -- [2] System 3: Find goblins vs demons (find_by_tags with multiple tags)
    if input.just_pressed("slot2") and cooldown_ready("2", 0.3) then
        local goblins = scene.find_by_tags("enemy", "goblin")
        local demons = scene.find_by_tags("enemy", "demon")
        local gc, dc = 0, 0
        for _ in ipairs(goblins) do gc = gc + 1 end
        for _ in ipairs(demons) do dc = dc + 1 end
        log("[find_by_tags] Goblins: " .. gc .. " | Demons: " .. dc)
    end

    -- [3] System 2: Destroy + spawn same frame (was broken before Tier 2)
    if input.just_pressed("slot3") and cooldown_ready("3", 0.5) then
        local test_id = "lifecycle_test"
        if entity.exists(test_id) then
            entity.destroy(test_id)
        end
        -- Spawn with same ID in the same frame — this used to fail!
        entity.spawn(test_id,
            "procedural:cube",
            "assets/materials/neon_gold.yaml",
            math.random() * 10 - 5, 1, math.random() * -10,
            0.6, 0.6, 0.6
        )
        events.emit("lifecycle.respawn", { id = test_id })
        log("[lifecycle] Destroyed + spawned '" .. test_id .. "' in same frame")
    end

    -- [4] System 5: Pool acquire / release toggle
    if input.just_pressed("slot4") and cooldown_ready("4", 0.3) then
        if self.pool_acquired < 5 then
            -- Acquire from pool
            local id = entity.pool_acquire("bullet_pool")
            if id then
                self.pool_acquired = self.pool_acquired + 1
                table.insert(self.pool_items, id)
                -- Position acquired entity in a line
                local x = -4 + self.pool_acquired * 2
                entity.set_position(id, x, 1.5, 5)
                entity.set_scale(id, 0.4, 0.4, 0.4)
                events.emit("pool.acquired", { id = id })
                log("[pool] Acquired: " .. id)
            end
        else
            -- Release all back to pool
            for _, id in ipairs(self.pool_items) do
                entity.pool_release(id)
            end
            log("[pool] Released " .. #self.pool_items .. " entities back to pool")
            self.pool_items = {}
            self.pool_acquired = 0
        end
    end

    -- [E] System 3: Tag nearest enemy as "marked"
    if input.just_pressed("interact") and cooldown_ready("E", 0.3) then
        local enemies = scene.find_by_tag("enemy")
        local px, py, pz = entity.get_position(_entity_string_id)
        local closest_id = nil
        local closest_dist = 999

        for _, eid in ipairs(enemies) do
            if not entity.has_tag(eid, "marked") then
                local ex, ey, ez = entity.get_position(eid)
                local dx, dz = ex - px, ez - pz
                local dist = math.sqrt(dx * dx + dz * dz)
                if dist < closest_dist then
                    closest_dist = dist
                    closest_id = eid
                end
            end
        end

        if closest_id then
            entity.add_tag(closest_id, "marked")
            self.marked_count = self.marked_count + 1
            entity.set_emission(closest_id, 2, 0.2, 0.2)
            events.emit("enemy.marked", { id = closest_id })
            log("[tag] Marked " .. closest_id .. " (total marked: " .. self.marked_count .. ")")
        else
            log("[tag] No unmarked enemies remaining")
        end
    end

    -- === HUD ===
    local sw = ui.screen_width()
    local sh = ui.screen_height()

    -- Title
    ui.text(20, 20, "TIER 2: LIFECYCLE + QUERIES + EVENTS + POOLING", 22, 1, 0.7, 0.2, 1)

    -- Controls
    ui.text(20, 50, "[1] find_by_tag  [2] goblins vs demons  [3] destroy+spawn  [4] pool  [E] mark enemy", 13, 0.6, 0.6, 0.6, 1)

    -- Pool stats
    local total, avail = entity.pool_size("bullet_pool")
    ui.text(20, 72, "Pool: " .. total .. " total, " .. avail .. " available, " .. self.pool_acquired .. " active", 14, 0.3, 0.8, 1, 1)

    -- Marked enemies
    local marked = scene.find_by_tag("marked")
    local mc = 0
    for _ in ipairs(marked) do mc = mc + 1 end
    ui.text(20, 90, "Marked enemies: " .. mc, 14, 1, 0.3, 0.3, 1)

    -- Event log
    ui.text(sw - 400, 20, "EVENT LOG:", 16, 0.4, 1, 0.4, 1)
    for i, msg in ipairs(self.event_log) do
        local alpha = 1.0 - (i - 1) * 0.15
        ui.text(sw - 400, 40 + (i - 1) * 18, msg, 13, 0.4, 0.9, 0.4, alpha)
    end

    -- Crosshair
    local cx = sw / 2
    local cy = sh / 2
    ui.rect(cx - 1, cy - 6, 2, 12, 1, 1, 1, 0.6)
    ui.rect(cx - 6, cy - 1, 12, 2, 1, 1, 1, 0.6)
end
