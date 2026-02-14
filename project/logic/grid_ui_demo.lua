-- Grid UI Demo: Snake Sweeper-style grid with numbers on tiles
-- Demonstrates: entity.spawn, ui.text (world-space overlay), ui.rect, ui.flash

local GRID_SIZE = 7
local TILE_SIZE = 1.0
local TILE_GAP = 0.05

-- Minesweeper-style number colors
local NUM_COLORS = {
    {0.2, 0.5, 1.0},  -- 1: blue
    {0.2, 0.8, 0.2},  -- 2: green
    {1.0, 0.2, 0.2},  -- 3: red
    {0.6, 0.2, 0.8},  -- 4: purple
}

function init()
    self.time = 0
    self.grid = {}
    self.revealed = {}
    self.mines = {}
    self.snake_pos = {x = 4, y = 4}
    self.snake_dir = {x = 1, y = 0}
    self.move_timer = 0
    self.move_interval = 0.6
    self.game_score = 0
    self.game_state = "playing" -- playing, won, lost

    -- Generate random mines (about 15% of tiles)
    math.randomseed(42) -- deterministic for demo
    for y = 1, GRID_SIZE do
        self.mines[y] = {}
        for x = 1, GRID_SIZE do
            self.mines[y][x] = (math.random() < 0.15) and (x ~= 4 or y ~= 4)
        end
    end

    -- Compute numbers (adjacent mine counts)
    for y = 1, GRID_SIZE do
        self.grid[y] = {}
        for x = 1, GRID_SIZE do
            if self.mines[y][x] then
                self.grid[y][x] = -1 -- mine
            else
                local count = 0
                for dy = -1, 1 do
                    for dx = -1, 1 do
                        local nx, ny = x + dx, y + dy
                        if nx >= 1 and nx <= GRID_SIZE and ny >= 1 and ny <= GRID_SIZE then
                            if self.mines[ny][nx] then count = count + 1 end
                        end
                    end
                end
                self.grid[y][x] = count
            end
        end
    end

    -- Initialize revealed grid (snake starting position is revealed)
    for y = 1, GRID_SIZE do
        self.revealed[y] = {}
        for x = 1, GRID_SIZE do
            self.revealed[y][x] = false
        end
    end
    self.revealed[4][4] = true

    -- Spawn tile entities
    for y = 1, GRID_SIZE do
        for x = 1, GRID_SIZE do
            local id = tile_id(x, y)
            local wx, wz = grid_to_world(x, y)
            entity.spawn(id, "assets/meshes/cube.gltf", "assets/materials/obsidian.yaml",
                wx, 0, wz, TILE_SIZE - TILE_GAP, 0.15, TILE_SIZE - TILE_GAP)
        end
    end

    -- Spawn snake head
    local sx, sz = grid_to_world(4, 4)
    entity.spawn("snake_head", "procedural:sphere", "assets/materials/neon_green.yaml",
        sx, 0.3, sz, 0.35, 0.35, 0.35)

    ui.flash(0.1, 0.8, 0.3, 0.4, 0.5)
end

function tile_id(x, y)
    return "tile_" .. x .. "_" .. y
end

function grid_to_world(gx, gy)
    local offset = (GRID_SIZE + 1) / 2
    local wx = (gx - offset) * TILE_SIZE
    local wz = (gy - offset) * TILE_SIZE
    return wx, wz
end

function update(dt)
    self.time = self.time + dt
    local sw = ui.screen_width()
    local sh = ui.screen_height()

    -- ═══════════════════════════════════
    -- Snake movement (auto-walk with direction changes)
    -- ═══════════════════════════════════
    if self.game_state == "playing" then
        -- Auto-change direction at boundaries
        self.move_timer = self.move_timer + dt
        if self.move_timer >= self.move_interval then
            self.move_timer = 0
            local nx = self.snake_pos.x + self.snake_dir.x
            local ny = self.snake_pos.y + self.snake_dir.y

            -- Bounce off walls
            if nx < 1 or nx > GRID_SIZE or ny < 1 or ny > GRID_SIZE then
                -- Turn right
                local tmp = self.snake_dir.x
                self.snake_dir.x = -self.snake_dir.y
                self.snake_dir.y = tmp
                nx = self.snake_pos.x + self.snake_dir.x
                ny = self.snake_pos.y + self.snake_dir.y
            end

            -- Still out of bounds? Turn left instead
            if nx < 1 or nx > GRID_SIZE or ny < 1 or ny > GRID_SIZE then
                self.snake_dir.x = -self.snake_dir.x
                self.snake_dir.y = -self.snake_dir.y
                nx = self.snake_pos.x + self.snake_dir.x
                ny = self.snake_pos.y + self.snake_dir.y
            end

            if nx >= 1 and nx <= GRID_SIZE and ny >= 1 and ny <= GRID_SIZE then
                self.snake_pos.x = nx
                self.snake_pos.y = ny

                -- Reveal tile
                if not self.revealed[ny][nx] then
                    self.revealed[ny][nx] = true
                    self.game_score = self.game_score + 10

                    if self.mines[ny][nx] then
                        -- Hit a mine!
                        self.game_state = "lost"
                        ui.flash(1.0, 0.1, 0.0, 0.8, 1.0)
                        -- Make mine tile red
                        entity.set_scale(tile_id(nx, ny), TILE_SIZE - TILE_GAP, 0.3, TILE_SIZE - TILE_GAP)
                    end
                end

                -- Move snake head
                local wx, wz = grid_to_world(nx, ny)
                entity.set_position("snake_head", wx, 0.3, wz)
            end

            -- Random direction change every few moves
            if math.random() < 0.3 then
                local tmp = self.snake_dir.x
                self.snake_dir.x = -self.snake_dir.y
                self.snake_dir.y = tmp
            end
        end
    end

    -- ═══════════════════════════════════
    -- Update tile visuals
    -- ═══════════════════════════════════
    for y = 1, GRID_SIZE do
        for x = 1, GRID_SIZE do
            local id = tile_id(x, y)
            if self.revealed[y][x] then
                -- Revealed: raise tile slightly
                entity.set_scale(id, TILE_SIZE - TILE_GAP, 0.05, TILE_SIZE - TILE_GAP)
                -- Set emission based on content
                if self.mines[y][x] then
                    entity.set_emission(id, 2.0, 0.1, 0.1) -- Red glow for mines
                elseif self.grid[y][x] > 0 then
                    local c = NUM_COLORS[math.min(self.grid[y][x], 4)]
                    entity.set_emission(id, c[1] * 0.3, c[2] * 0.3, c[3] * 0.3)
                else
                    entity.set_emission(id, 0.05, 0.15, 0.05) -- Safe green
                end
            else
                -- Unrevealed: subtle pulse
                local p = math.sin(self.time * 2 + x * 0.5 + y * 0.7) * 0.02
                entity.set_emission(id, 0.05 + p, 0.05 + p, 0.1 + p)
            end
        end
    end

    -- ═══════════════════════════════════
    -- HUD: Title bar
    -- ═══════════════════════════════════
    ui.rect(0, 0, sw, 36, 0.0, 0.0, 0.0, 0.8)
    ui.text(10, 6, "SNAKE SWEEPER - Grid UI Demo", 22, 0.3, 1.0, 0.5, 1.0)

    -- Score
    ui.text(sw - 180, 8, "SCORE: " .. tostring(self.game_score), 18, 1.0, 0.9, 0.3, 1.0)

    -- ═══════════════════════════════════
    -- HUD: Minesweeper numbers overlay
    -- Project grid positions to screen space (approximate for top-down camera)
    -- ═══════════════════════════════════
    -- With a top-down camera at y=12, fov=60, the grid maps roughly to the center of screen
    -- We'll use a simple mapping: grid center = screen center
    local grid_pixel_size = math.min(sw, sh) * 0.55 -- grid takes ~55% of screen
    local cell_px = grid_pixel_size / GRID_SIZE
    local grid_ox = (sw - grid_pixel_size) / 2
    local grid_oy = (sh - grid_pixel_size) / 2

    for y = 1, GRID_SIZE do
        for x = 1, GRID_SIZE do
            if self.revealed[y][x] and not self.mines[y][x] and self.grid[y][x] > 0 then
                local num = self.grid[y][x]
                local c = NUM_COLORS[math.min(num, 4)]
                local px = grid_ox + (x - 0.5) * cell_px - cell_px * 0.15
                local py = grid_oy + (y - 0.5) * cell_px - cell_px * 0.2
                local font_size = cell_px * 0.5
                ui.text(px, py, tostring(num), font_size, c[1], c[2], c[3], 1.0)
            elseif self.revealed[y][x] and self.mines[y][x] then
                local px = grid_ox + (x - 0.5) * cell_px - cell_px * 0.15
                local py = grid_oy + (y - 0.5) * cell_px - cell_px * 0.2
                local font_size = cell_px * 0.5
                local blink = math.floor(self.time * 4) % 2
                if blink == 0 then
                    ui.text(px, py, "*", font_size, 1.0, 0.2, 0.1, 1.0)
                end
            end
        end
    end

    -- ═══════════════════════════════════
    -- Game over overlay
    -- ═══════════════════════════════════
    if self.game_state == "lost" then
        ui.rect(0, sh / 2 - 40, sw, 80, 0.0, 0.0, 0.0, 0.8)
        ui.text(sw / 2 - 100, sh / 2 - 25, "MINE HIT!", 36, 1.0, 0.2, 0.1, 1.0)
        ui.text(sw / 2 - 80, sh / 2 + 15, "Score: " .. tostring(self.game_score), 20, 1.0, 1.0, 1.0, 0.8)
    end

    -- ═══════════════════════════════════
    -- Bottom bar: legend
    -- ═══════════════════════════════════
    ui.rect(0, sh - 28, sw, 28, 0.0, 0.0, 0.0, 0.7)
    ui.text(10, sh - 22, "1", 14, 0.2, 0.5, 1.0, 1.0)
    ui.text(22, sh - 22, "=1 mine", 12, 0.5, 0.5, 0.5, 1.0)
    ui.text(90, sh - 22, "2", 14, 0.2, 0.8, 0.2, 1.0)
    ui.text(102, sh - 22, "=2 mines", 12, 0.5, 0.5, 0.5, 1.0)
    ui.text(180, sh - 22, "3", 14, 1.0, 0.2, 0.2, 1.0)
    ui.text(192, sh - 22, "=3 mines", 12, 0.5, 0.5, 0.5, 1.0)
    ui.text(270, sh - 22, "*", 14, 1.0, 0.3, 0.1, 1.0)
    ui.text(282, sh - 22, "=mine!", 12, 0.5, 0.5, 0.5, 1.0)
end
