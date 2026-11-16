struct Member {
    position: vec4<f32>,
    velocity: vec4<f32>,
    life: vec4<f32>,
};

struct DamageVolume {
    a: vec4<f32>,
    b: vec4<f32>,
};

struct SpawnRequest {
    center: vec4<f32>,
    range: vec4<u32>,
};

struct SwarmParams {
    target_pos: vec4<f32>,
    motion: vec4<f32>,
    base_color: vec4<f32>,
    material: vec4<f32>,
    emission: vec4<f32>,
    region: vec4<f32>,
    counts: vec4<u32>,
    steering: vec4<f32>,
    combat: vec4<f32>,
    frame: vec4<u32>,
    volumes: array<DamageVolume, 64>,
    spawns: array<SpawnRequest, 8>,
};

// Same layout as the CPU DrawUniforms and the G-buffer and shadow shaders.
struct DrawUniforms {
    model_matrix: mat4x4<f32>,
    normal_matrix: mat4x4<f32>,
    base_color: vec4<f32>,
    roughness: f32,
    metallic: f32,
    has_texture: f32,
    has_skin: f32,
    emission: vec4<f32>,
    _padding: array<vec4<f32>, 5>,
};

const HIT_FLASH: f32 = 0.12;
const TAU: f32 = 6.2831855;
const TARGET_PUSH: f32 = 3.0;

@group(0) @binding(0) var<uniform> params: SwarmParams;
@group(0) @binding(1) var<storage, read> members_in: array<Member>;
@group(0) @binding(2) var<storage, read_write> members_out: array<Member>;
@group(0) @binding(3) var<storage, read_write> draws: array<DrawUniforms>;
// Per flow-field cell: direction x, direction z, distance (-1 unreachable), blocked.
@group(0) @binding(4) var<storage, read> field: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> cell_counts: array<atomic<u32>>;
@group(0) @binding(6) var<storage, read_write> cell_members: array<u32>;
// Kills and attacks (running totals), living members and spawn claims (this step).
@group(0) @binding(7) var<storage, read_write> stats: array<atomic<u32>, 4>;

fn grid_coords(pos: vec2<f32>) -> vec2<i32> {
    let n = i32(params.counts.z);
    let c = floor((pos - params.region.xy) / (params.region.z / f32(n)));
    return clamp(vec2<i32>(c), vec2<i32>(0, 0), vec2<i32>(n - 1, n - 1));
}

fn blocked_at(pos: vec2<f32>) -> bool {
    let n = i32(params.counts.y);
    let c = vec2<i32>(floor((pos - params.region.xy) / (params.region.z / f32(n))));
    if (c.x < 0 || c.y < 0 || c.x >= n || c.y >= n) {
        return false;
    }
    return field[c.y * n + c.x].w > 0.5;
}

fn clamp_to_region(p: vec2<f32>) -> vec2<f32> {
    let lo = params.region.xy + vec2<f32>(0.01, 0.01);
    let hi = params.region.xy + vec2<f32>(params.region.z - 0.01, params.region.z - 0.01);
    return clamp(p, lo, hi);
}

// Bilinear blend of the four nearest cells' directions, skipping blocked and
// unreachable cells. Zero when there is nothing to follow.
fn flow_direction(pos: vec2<f32>) -> vec2<f32> {
    let n = i32(params.counts.y);
    let g = (pos - params.region.xy) / (params.region.z / f32(n)) - 0.5;
    let base = floor(g);
    let t = g - base;
    let origin = vec2<i32>(base);
    var sum = vec2<f32>(0.0, 0.0);
    for (var k = 0; k < 4; k++) {
        let d = vec2<i32>(k & 1, k >> 1u);
        let c = origin + d;
        if (c.x < 0 || c.y < 0 || c.x >= n || c.y >= n) {
            continue;
        }
        let cell = field[c.y * n + c.x];
        if (cell.z < 0.0 || cell.w > 0.5) {
            continue;
        }
        let w = select(1.0 - t.x, t.x, d.x == 1) * select(1.0 - t.y, t.y, d.y == 1);
        sum += cell.xy * w;
    }
    if (length(sum) > 0.0001) {
        return normalize(sum);
    }
    return vec2<f32>(0.0, 0.0);
}

fn hash32(v: u32) -> u32 {
    var x = v;
    x ^= x >> 16u;
    x *= 0x7feb352du;
    x ^= x >> 15u;
    x *= 0x846ca68bu;
    x ^= x >> 16u;
    return x;
}

// A value in [0, 1) for spawn number `index` of a swarm seeded `seed`.
fn spawn_random(seed: u32, index: u32, stream: u32) -> f32 {
    let h = hash32(index ^ hash32(seed * 0x9e3779b9u + stream));
    return f32(h >> 8u) / 16777216.0;
}

fn segment_distance(p: vec3<f32>, a: vec3<f32>, b: vec3<f32>) -> f32 {
    let ab = b - a;
    let len2 = dot(ab, ab);
    var t = 0.0;
    if (len2 > 1e-8) {
        t = clamp(dot(p - a, ab) / len2, 0.0, 1.0);
    }
    return length(p - (a + ab * t));
}

fn heading_toward(start: vec2<f32>, goal: vec2<f32>) -> f32 {
    let d = goal - start;
    if (length(d) < 0.0001) {
        return 0.0;
    }
    return atan2(d.x, d.y);
}

fn write_draw(i: u32, pos: vec3<f32>, heading: f32, flash: f32) {
    // Model matrix: translate * rotate about Y by heading * uniform scale.
    let scale = params.material.w;
    let c = cos(heading);
    let s = sin(heading);
    let x_axis = vec4<f32>(c, 0.0, -s, 0.0);
    let y_axis = vec4<f32>(0.0, 1.0, 0.0, 0.0);
    let z_axis = vec4<f32>(s, 0.0, c, 0.0);
    draws[i].model_matrix = mat4x4<f32>(x_axis * scale, y_axis * scale, z_axis * scale, vec4<f32>(pos, 1.0));
    // Normals only need the rotation; the shaders normalize them.
    draws[i].normal_matrix = mat4x4<f32>(x_axis, y_axis, z_axis, vec4<f32>(0.0, 0.0, 0.0, 1.0));
    draws[i].base_color = params.base_color;
    draws[i].roughness = params.material.x;
    draws[i].metallic = params.material.y;
    draws[i].has_texture = params.material.z;
    draws[i].has_skin = 0.0;
    draws[i].emission = params.emission + vec4<f32>(3.0, 2.4, 1.6, 0.0) * (flash / HIT_FLASH);
}

// A dead member draws nothing: every vertex lands on one point far below.
fn hide_draw(i: u32) {
    let zero = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    draws[i].model_matrix = mat4x4<f32>(zero, zero, zero, vec4<f32>(0.0, -10000.0, 0.0, 1.0));
}

@compute @workgroup_size(64)
fn bin_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.counts.x || members_in[i].life.x <= 0.0) {
        return;
    }
    let c = grid_coords(members_in[i].position.xz);
    let cell = u32(c.y) * params.counts.z + u32(c.x);
    let slot = atomicAdd(&cell_counts[cell], 1u);
    if (slot < params.counts.w) {
        cell_members[cell * params.counts.w + slot] = i;
    }
}

// A dead member fills a spawn request if one is left this step; otherwise
// it stays dead.
fn dead_or_revive(i: u32, m: Member) {
    if (params.frame.z > 0u) {
        let slot = atomicAdd(&stats[3], 1u);
        for (var r = 0u; r < params.frame.y; r++) {
            let req = params.spawns[r];
            if (slot < req.range.x || slot >= req.range.x + req.range.y) {
                continue;
            }
            let n = req.range.z + (slot - req.range.x);
            let seed = req.range.w;
            let radius = req.center.w * sqrt(spawn_random(seed, n, 0u));
            let angle = TAU * spawn_random(seed, n, 1u);
            let p = clamp_to_region(req.center.xz + radius * vec2<f32>(cos(angle), sin(angle)));
            let height = params.motion.z;
            let heading = heading_toward(p, params.target_pos.xz);
            let speed_factor = 0.85 + 0.3 * spawn_random(seed, n, 2u);
            members_out[i] = Member(
                vec4<f32>(p.x, height, p.y, heading),
                vec4<f32>(0.0, 0.0, 0.0, speed_factor),
                vec4<f32>(params.steering.y, params.combat.y, 0.0, 0.0),
            );
            atomicAdd(&stats[2], 1u);
            write_draw(i, vec3<f32>(p.x, height, p.y), heading, 0.0);
            return;
        }
    }
    members_out[i] = m;
    hide_draw(i);
}

@compute @workgroup_size(64)
fn steer_main(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = id.x;
    if (i >= params.counts.x) {
        return;
    }
    let m = members_in[i];
    if (m.life.x <= 0.0) {
        dead_or_revive(i, m);
        return;
    }
    let dt = params.target_pos.w;
    let max_speed = params.motion.x;
    let acceleration = params.motion.y;
    let height = params.motion.z;
    let arrive_radius = params.motion.w;
    let separation_radius = params.region.w;
    let pos = m.position.xz;
    let speed_factor = m.velocity.w;

    // Damage volumes, measured where the member was drawn last frame.
    var health = m.life.x;
    var flash = max(0.0, m.life.z - dt);
    for (var k = 0u; k < params.frame.x; k++) {
        let volume = params.volumes[k];
        if (segment_distance(m.position.xyz, volume.a.xyz, volume.b.xyz) <= volume.a.w + params.steering.z) {
            health -= volume.b.w;
            flash = HIT_FLASH;
        }
    }
    if (health <= 0.0) {
        atomicAdd(&stats[0], 1u);
        members_out[i] = Member(m.position, vec4<f32>(0.0, 0.0, 0.0, speed_factor), vec4<f32>(0.0, 0.0, 0.0, 0.0));
        hide_draw(i);
        return;
    }

    // Toward the target: along the flow field, or straight when close or
    // when the field has nothing to follow.
    let to_target = params.target_pos.xz - pos;
    let dist = length(to_target);
    let field_cell = params.region.z / f32(params.counts.y);
    var dir = vec2<f32>(0.0, 0.0);
    if (dist > 0.0001) {
        dir = to_target / dist;
        if (dist > 2.0 * field_cell) {
            let flow = flow_direction(pos);
            if (dot(flow, flow) > 0.0) {
                dir = flow;
            }
        }
    }
    // Slow down on arrival, at the stand-off distance from the target.
    let stand_off = params.steering.w;
    let speed = max_speed * speed_factor * clamp((dist - stand_off) / arrive_radius, 0.0, 1.0);

    // Attack the target when in reach and ready.
    var cooldown = max(0.0, m.life.y - dt);
    if (params.combat.x > 0.0 && dt > 0.0 && dist <= params.combat.x && cooldown <= 0.0) {
        atomicAdd(&stats[1], 1u);
        cooldown = params.combat.y;
    }
    atomicAdd(&stats[2], 1u);

    // Away from neighbours closer than the separation radius.
    var push = vec2<f32>(0.0, 0.0);
    var pressure = 0.0;
    let n = i32(params.counts.z);
    let cap = params.counts.w;
    let home = grid_coords(pos);
    for (var dz = -1; dz <= 1; dz++) {
        for (var dx = -1; dx <= 1; dx++) {
            let c = home + vec2<i32>(dx, dz);
            if (c.x < 0 || c.y < 0 || c.x >= n || c.y >= n) {
                continue;
            }
            let cell = u32(c.y * n + c.x);
            let count = min(atomicLoad(&cell_counts[cell]), cap);
            for (var k = 0u; k < count; k++) {
                let j = cell_members[cell * cap + k];
                if (j == i) {
                    continue;
                }
                let d = pos - members_in[j].position.xz;
                let len = length(d);
                if (len > 0.00001 && len < separation_radius) {
                    let closeness = 1.0 - len / separation_radius;
                    push += d / len * closeness;
                    pressure += closeness;
                }
            }
        }
    }
    // Keep out of the target's space the way members keep out of each
    // other's, only firmer, so the crowd behind cannot squeeze them in.
    if (dist > 0.00001 && dist < stand_off) {
        let closeness = 1.0 - dist / stand_off;
        push -= to_target / dist * (closeness * TARGET_PUSH);
        pressure += closeness;
    }
    // Separation comes first: the more crowded a member is (however the
    // pushes balance out), the less it presses on toward the target, so a
    // crowd keeps its spacing instead of compressing.
    let crowding = min(1.0, pressure);
    var desired = dir * speed * (1.0 - crowding) + push * (max_speed * params.steering.x);
    let top = max_speed * speed_factor;
    let desired_len = length(desired);
    if (desired_len > top) {
        desired *= top / desired_len;
    }

    let blend = min(1.0, acceleration * dt);
    var v = m.velocity.xz + (desired - m.velocity.xz) * blend;

    // Slide along walls: never step into a blocked cell (unless already in one).
    var next = pos + v * dt;
    if (!blocked_at(pos)) {
        if (blocked_at(vec2<f32>(next.x, pos.y))) {
            next.x = pos.x;
            v.x = 0.0;
        }
        if (blocked_at(next)) {
            next.y = pos.y;
            v.y = 0.0;
        }
    }
    next = clamp_to_region(next);

    // Face the direction of travel; keep the last heading when nearly still.
    var heading = m.position.w;
    if (length(v) > 0.05) {
        heading = atan2(v.x, v.y);
    }

    members_out[i] = Member(
        vec4<f32>(next.x, height, next.y, heading),
        vec4<f32>(v.x, 0.0, v.y, speed_factor),
        vec4<f32>(health, cooldown, flash, 0.0),
    );
    write_draw(i, vec3<f32>(next.x, height, next.y), heading, flash);
}
