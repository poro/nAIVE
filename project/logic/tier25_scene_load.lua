-- Tier 2.5 Demo: Scene Loading
-- Shared script used by both Scene A (blue) and Scene B (red).
-- Press E to load the other scene. Walk to the portal to trigger transition.

function init()
    self.is_scene_a = entity.has_tag(_entity_string_id, "scene_a")
    self.loading = false
    self.portal_pulse = 0

    if self.is_scene_a then
        -- Blue room styling
        entity.set_base_color("floor", 0.15, 0.2, 0.35)
        entity.set_base_color("wall_back", 0.1, 0.15, 0.3)
        entity.set_base_color("wall_left", 0.1, 0.15, 0.3)
        entity.set_base_color("wall_right", 0.1, 0.15, 0.3)
        entity.set_base_color("wall_front", 0.1, 0.15, 0.3)
        entity.set_base_color("portal", 0.3, 0.5, 1.0)
        entity.set_emission("portal", 2.0, 3.0, 8.0)
        entity.set_base_color("decor_1", 0.2, 0.3, 0.6)
        entity.set_base_color("decor_2", 0.2, 0.3, 0.6)
        entity.set_emission("decor_1", 0.1, 0.2, 0.5)
        entity.set_emission("decor_2", 0.1, 0.2, 0.5)
    else
        -- Red room styling
        entity.set_base_color("floor", 0.35, 0.15, 0.12)
        entity.set_base_color("wall_back", 0.3, 0.1, 0.08)
        entity.set_base_color("wall_left", 0.3, 0.1, 0.08)
        entity.set_base_color("wall_right", 0.3, 0.1, 0.08)
        entity.set_base_color("wall_front", 0.3, 0.1, 0.08)
        entity.set_base_color("portal", 1.0, 0.3, 0.2)
        entity.set_emission("portal", 8.0, 2.0, 1.0)
        entity.set_base_color("decor_1", 0.6, 0.2, 0.15)
        entity.set_base_color("decor_2", 0.6, 0.2, 0.15)
        entity.set_emission("decor_1", 0.5, 0.1, 0.05)
        entity.set_emission("decor_2", 0.5, 0.1, 0.05)
    end
end

function on_trigger_enter(other_id)
    if other_id == "portal" and not self.loading then
        self.loading = true
        load_other_scene()
    end
end

function load_other_scene()
    if self.is_scene_a then
        scene.load("scenes/tier25_scene_load_b.yaml")
    else
        scene.load("scenes/tier25_scene_load_a.yaml")
    end
end

function update(dt)
    self.portal_pulse = (self.portal_pulse or 0) + dt

    -- Press E to load the other scene
    if input.just_pressed("interact") and not self.loading then
        self.loading = true
        -- Flash effect on transition
        if self.is_scene_a then
            ui.flash(0.3, 0.5, 1.0, 0.4, 0.3)
        else
            ui.flash(1.0, 0.3, 0.2, 0.4, 0.3)
        end
        load_other_scene()
    end

    -- Pulse the portal emission
    local pulse = math.sin(self.portal_pulse * 3.0) * 0.5 + 0.5
    if self.is_scene_a then
        entity.set_emission("portal", 1.0 + pulse * 3.0, 2.0 + pulse * 2.0, 5.0 + pulse * 5.0)
    else
        entity.set_emission("portal", 5.0 + pulse * 5.0, 1.0 + pulse * 2.0, 0.5 + pulse * 1.0)
    end

    -- Draw HUD
    local sw = ui.screen_width()
    local scene_name = self.is_scene_a and "SCENE A (Blue)" or "SCENE B (Red)"
    local target_name = self.is_scene_a and "Scene B (Red)" or "Scene A (Blue)"

    if self.is_scene_a then
        ui.text(sw * 0.5 - 110, 20, "SCENE LOADING DEMO", 24, 0.5, 0.7, 1.0, 1)
    else
        ui.text(sw * 0.5 - 110, 20, "SCENE LOADING DEMO", 24, 1.0, 0.5, 0.4, 1)
    end

    ui.rect(15, 55, 290, 75, 0, 0, 0, 0.6)
    ui.text(20, 60, "Current: " .. scene_name, 16, 1, 1, 1, 1)
    ui.text(20, 82, "Press E to load " .. target_name, 16, 0.8, 0.8, 0.8, 1)
    ui.text(20, 104, "Or walk into the portal", 14, 0.6, 0.6, 0.6, 1)

    -- Portal label
    if entity.exists("portal") then
        local px, py, pz = entity.get_position("portal")
        local sx, sy, vis = camera.world_to_screen(px, py + 1.8, pz)
        if vis then
            ui.text(sx - 20, sy, "PORTAL", 16, 1, 1, 1, 0.9)
        end
    end
end
