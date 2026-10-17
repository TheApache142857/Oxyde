// The Render tab's path tracer on the GPU: the CPU tracer of `oxyde_render::pathtrace`
// (render.rs, bsdf.rs, scene.rs, environment.rs) ported line for line, one invocation per
// pixel, one sample per dispatch, over a band of rows so no single dispatch runs long.
// The data layouts are documented in `oxyde_render::pathtrace::gpu`, which writes them.
//
// `trace` adds a sample to the sums of the band's pixels; `develop` turns the sums into
// the picture shown (averaged, exposed, tonemapped, sRGB-encoded, premultiplied as egui
// wants), row by row into a buffer copied to the texture egui draws. The final picture,
// denoised, is made on the CPU from the sums read back (`develop_sums`), as the CPU
// renderer makes its own.

struct Node {
    lo: vec3<f32>,
    a: u32,
    hi: vec3<f32>,
    b: u32,
};

struct Tri {
    v0: vec3<f32>,
    look: u32,
    e1: vec3<f32>,
    n0: u32,
    e2: vec3<f32>,
    n1: u32,
    n2: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};

struct Look {
    base: vec3<f32>,
    opacity: f32,
    metallic: f32,
    roughness: f32,
    transmission: f32,
    ior: f32,
};

struct Light {
    // xyz: towards the light; w: cosine of its angular radius.
    dir: vec4<f32>,
    radiance: vec4<f32>,
};

struct Params {
    // w: 0 perspective, 1 orthographic.
    eye: vec4<f32>,
    // w: tangent of half the vertical angle of view, or the view's height (mm).
    forward: vec4<f32>,
    // w: how far behind the centre orthographic rays start (mm).
    right: vec4<f32>,
    // w: offset of new rays from surfaces (mm).
    up: vec4<f32>,
    // width, height, seed, sample being added.
    size: vec4<u32>,
    // first row, rows; for `develop`: rows of the sample in progress done, samples done.
    band: vec4<u32>,
    // environment (0 studio, 1 sky, 2 uniform), background (0 environment, 1 colour,
    // 2 transparent), floor (0 none, 1 shadow catcher, 2 visible), lights.
    kinds: vec4<u32>,
    // max bounces, output row stride (words).
    limits: vec4<u32>,
    // floor height, environment strength, exposure factor.
    shade: vec4<f32>,
    // The plain background's colour, chosen so the tone curve gives it back.
    background: vec4<f32>,
    // The visible floor's colour and roughness.
    floor_look: vec4<f32>,
    lights: array<Light, 4>,
};

@group(0) @binding(0) var<uniform> P: Params;
@group(0) @binding(1) var<storage, read> nodes: array<Node>;
@group(0) @binding(2) var<storage, read> tris: array<Tri>;
@group(0) @binding(3) var<storage, read> looks: array<Look>;
// Per pixel, summed over samples: colour premultiplied by coverage, and alpha.
@group(0) @binding(4) var<storage, read_write> accum: array<vec4<f32>>;
// First surface seen: colour and depth; normal. For the denoiser.
@group(0) @binding(5) var<storage, read_write> feat_a: array<vec4<f32>>;
@group(0) @binding(6) var<storage, read_write> feat_b: array<vec4<f32>>;
// The picture shown: RGBA8, rows `limits.y` words long.
@group(0) @binding(7) var<storage, read_write> picture: array<u32>;

const PI: f32 = 3.14159265358979;
const TAU: f32 = 6.28318530717959;
const BIG: f32 = 1e30;
const MAX_CROSSINGS: u32 = 64u;
const MIN_ALPHA: f32 = 0.002;
const LEAF_BIT: u32 = 0x80000000u;
// A walk that takes longer than this is broken data: stop rather than hang the GPU.
const MAX_STEPS: u32 = 100000u;

// ---- Random numbers -------------------------------------------------------------------

var<private> rng: u32;

fn pcg(v: u32) -> u32 {
    let state = v * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

fn rand() -> f32 {
    rng = rng * 747796405u + 2891336453u;
    let word = ((rng >> ((rng >> 28u) + 4u)) ^ rng) * 277803737u;
    let r = (word >> 22u) ^ word;
    return f32(r >> 8u) * (1.0 / 16777216.0);
}

// ---- Directions -------------------------------------------------------------------------

struct Basis {
    t: vec3<f32>,
    b: vec3<f32>,
};

fn make_basis(n: vec3<f32>) -> Basis {
    let s = select(-1.0, 1.0, n.z >= 0.0);
    let a = -1.0 / (s + n.z);
    let b = n.x * n.y * a;
    return Basis(vec3<f32>(1.0 + s * n.x * n.x * a, s * b, -s * n.x), vec3<f32>(b, s + n.y * n.y * a, -n.y));
}

fn safe_normalize(v: vec3<f32>, fallback: vec3<f32>) -> vec3<f32> {
    let len = length(v);
    if len > 1e-20 {
        return v / len;
    }
    return fallback;
}

fn cosine_hemisphere(n: vec3<f32>) -> vec3<f32> {
    let u1 = rand();
    let u2 = rand();
    let r = sqrt(u1);
    let phi = TAU * u2;
    let bs = make_basis(n);
    let z = sqrt(max(1.0 - u1, 0.0));
    return safe_normalize(bs.t * (r * cos(phi)) + bs.b * (r * sin(phi)) + n * z, n);
}

fn uniform_cone(axis: vec3<f32>, cos_max: f32, u1: f32, u2: f32) -> vec3<f32> {
    let cos_t = 1.0 - u1 * (1.0 - cos_max);
    let sin_t = sqrt(max(1.0 - cos_t * cos_t, 0.0));
    let phi = TAU * u2;
    let bs = make_basis(axis);
    return safe_normalize(bs.t * (sin_t * cos(phi)) + bs.b * (sin_t * sin(phi)) + axis * cos_t, axis);
}

fn cone_pdf(cos_max: f32) -> f32 {
    return 1.0 / (TAU * max(1.0 - cos_max, 1e-7));
}

fn power_heuristic(a: f32, b: f32) -> f32 {
    let a2 = a * a;
    let b2 = b * b;
    if a2 + b2 <= 0.0 {
        return 0.0;
    }
    return a2 / (a2 + b2);
}

fn luminance(c: vec3<f32>) -> f32 {
    return 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
}

fn max3(c: vec3<f32>) -> f32 {
    return max(c.x, max(c.y, c.z));
}

fn decode_normal(p: u32) -> vec3<f32> {
    let e = unpack2x16snorm(p);
    var v = vec3<f32>(e.x, e.y, 1.0 - abs(e.x) - abs(e.y));
    let t = max(-v.z, 0.0);
    v.x = v.x + select(t, -t, v.x >= 0.0);
    v.y = v.y + select(t, -t, v.y >= 0.0);
    return normalize(v);
}

// ---- The scene --------------------------------------------------------------------------

// Distance and barycentric coordinates where the ray crosses triangle `i`; t < 0: missed.
fn intersect_tri(i: u32, o: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    let tri = tris[i];
    let p = cross(d, tri.e2);
    let det = dot(tri.e1, p);
    if abs(det) < 1e-14 {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    let inv = 1.0 / det;
    let s = o - tri.v0;
    let u = dot(s, p) * inv;
    if u < 0.0 || u > 1.0 {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    let q = cross(s, tri.e1);
    let v = dot(d, q) * inv;
    if v < 0.0 || u + v > 1.0 {
        return vec3<f32>(-1.0, 0.0, 0.0);
    }
    return vec3<f32>(dot(tri.e2, q) * inv, u, v);
}

// Where the ray enters the box before `t_max`, or -1.
fn box_entry(lo: vec3<f32>, hi: vec3<f32>, o: vec3<f32>, inv: vec3<f32>, t_max: f32) -> f32 {
    let t0 = (lo - o) * inv;
    let t1 = (hi - o) * inv;
    let near = max(max3(min(t0, t1)), 0.0);
    let fa = max(t0, t1);
    let far = min(min(min(fa.x, fa.y), fa.z), t_max);
    if near <= far {
        return near;
    }
    return -1.0;
}

fn inverse_dir(d: vec3<f32>) -> vec3<f32> {
    // Keep the slab test away from 0 × ∞.
    let s = select(vec3<f32>(-1e-20), vec3<f32>(1e-20), d >= vec3<f32>(0.0));
    let safe = select(d, s, abs(d) < vec3<f32>(1e-20));
    return 1.0 / safe;
}

struct Hit {
    found: bool,
    t: f32,
    point: vec3<f32>,
    geometric: vec3<f32>,
    shading: vec3<f32>,
    look: u32,
};

fn intersect(o: vec3<f32>, d: vec3<f32>) -> Hit {
    var hit: Hit;
    hit.found = false;
    let count = arrayLength(&nodes);
    if count == 0u || arrayLength(&tris) == 0u {
        return hit;
    }
    let inv = inverse_dir(d);
    var t_max = BIG;
    var best_tri = 0u;
    var best_uv = vec2<f32>(0.0);
    if box_entry(nodes[0].lo, nodes[0].hi, o, inv, t_max) < 0.0 {
        return hit;
    }
    var stack: array<u32, 64>;
    var depth = 1u;
    stack[0] = 0u;
    var steps = 0u;
    loop {
        if depth == 0u || steps > MAX_STEPS {
            break;
        }
        steps = steps + 1u;
        depth = depth - 1u;
        let index = stack[depth];
        let node = nodes[index];
        if (node.b & LEAF_BIT) != 0u {
            let n = node.b & ~LEAF_BIT;
            for (var k = node.a; k < node.a + n; k = k + 1u) {
                let r = intersect_tri(k, o, d);
                if r.x > 0.0 && r.x < t_max {
                    t_max = r.x;
                    best_tri = k;
                    best_uv = r.yz;
                    hit.found = true;
                }
            }
            continue;
        }
        // Children always come after their parent; anything else is not a tree.
        if node.a <= index || node.b <= index || node.a >= count || node.b >= count {
            continue;
        }
        let ta = box_entry(nodes[node.a].lo, nodes[node.a].hi, o, inv, t_max);
        let tb = box_entry(nodes[node.b].lo, nodes[node.b].hi, o, inv, t_max);
        // The farther child first, so the nearer is walked first.
        if ta >= 0.0 && tb >= 0.0 {
            if depth + 2u <= 64u {
                if ta <= tb {
                    stack[depth] = node.b;
                    stack[depth + 1u] = node.a;
                } else {
                    stack[depth] = node.a;
                    stack[depth + 1u] = node.b;
                }
                depth = depth + 2u;
            }
        } else if ta >= 0.0 {
            if depth < 64u {
                stack[depth] = node.a;
                depth = depth + 1u;
            }
        } else if tb >= 0.0 {
            if depth < 64u {
                stack[depth] = node.b;
                depth = depth + 1u;
            }
        }
    }
    if hit.found {
        let tri = tris[best_tri];
        let g = normalize(cross(tri.e1, tri.e2));
        let u = best_uv.x;
        let v = best_uv.y;
        let n = decode_normal(tri.n0) * (1.0 - u - v) + decode_normal(tri.n1) * u + decode_normal(tri.n2) * v;
        hit.t = t_max;
        hit.point = o + d * t_max;
        hit.geometric = g;
        hit.shading = safe_normalize(n, g);
        hit.look = tri.look;
    }
    return hit;
}

// How much light gets from `o` along `d` unblocked: 0 behind an opaque surface, a tint
// through glass and see-through looks, 1 in the open.
fn transmittance(o: vec3<f32>, d: vec3<f32>) -> vec3<f32> {
    var through = vec3<f32>(1.0);
    let count = arrayLength(&nodes);
    if count == 0u || arrayLength(&tris) == 0u {
        return through;
    }
    let inv = inverse_dir(d);
    if box_entry(nodes[0].lo, nodes[0].hi, o, inv, BIG) < 0.0 {
        return through;
    }
    var stack: array<u32, 64>;
    var depth = 1u;
    stack[0] = 0u;
    var steps = 0u;
    loop {
        if depth == 0u || steps > MAX_STEPS {
            break;
        }
        steps = steps + 1u;
        depth = depth - 1u;
        let index = stack[depth];
        let node = nodes[index];
        if (node.b & LEAF_BIT) != 0u {
            let n = node.b & ~LEAF_BIT;
            for (var k = node.a; k < node.a + n; k = k + 1u) {
                let r = intersect_tri(k, o, d);
                if r.x <= 0.0 {
                    continue;
                }
                let look = looks[tris[k].look];
                if look.transmission >= 0.5 {
                    through = through * sqrt(max(look.base, vec3<f32>(0.0))) * 0.96;
                } else if look.opacity < 0.999 {
                    through = through * (1.0 - look.opacity);
                } else {
                    return vec3<f32>(0.0);
                }
                if max3(through) <= 1e-4 {
                    return vec3<f32>(0.0);
                }
            }
            continue;
        }
        if node.a <= index || node.b <= index || node.a >= count || node.b >= count {
            continue;
        }
        if box_entry(nodes[node.a].lo, nodes[node.a].hi, o, inv, BIG) >= 0.0 && depth < 64u {
            stack[depth] = node.a;
            depth = depth + 1u;
        }
        if box_entry(nodes[node.b].lo, nodes[node.b].hi, o, inv, BIG) >= 0.0 && depth < 64u {
            stack[depth] = node.b;
            depth = depth + 1u;
        }
    }
    return through;
}

// ---- The environment ----------------------------------------------------------------------

fn background(dir: vec3<f32>) -> vec3<f32> {
    let h = dir.y;
    let k = P.shade.y;
    let kind = P.kinds.x;
    if kind == 0u {
        let top = vec3<f32>(0.62, 0.63, 0.66);
        let horizon = vec3<f32>(0.42, 0.42, 0.43);
        let ground = vec3<f32>(0.16, 0.16, 0.16);
        if h >= 0.0 {
            return mix(horizon, top, sqrt(h)) * k;
        }
        return mix(horizon, ground, min(sqrt(-h), 1.0)) * k;
    }
    if kind == 1u {
        let zenith = vec3<f32>(0.18, 0.34, 0.75);
        let horizon = vec3<f32>(0.70, 0.80, 0.95);
        let ground = vec3<f32>(0.22, 0.20, 0.17);
        if h >= 0.0 {
            return mix(horizon, zenith, pow(h, 0.6)) * k;
        }
        return mix(horizon, ground, min(-h * 4.0, 1.0)) * k;
    }
    return vec3<f32>(1.0) * k;
}

fn light_count() -> u32 {
    return min(P.kinds.w, 4u);
}

// The light whose disc `dir` falls in, or 4 for none.
fn light_in(dir: vec3<f32>) -> u32 {
    for (var i = 0u; i < light_count(); i = i + 1u) {
        if dot(dir, P.lights[i].dir.xyz) >= P.lights[i].dir.w {
            return i;
        }
    }
    return 4u;
}

// What a camera ray sees when it leaves the scene, before exposure, and its alpha.
fn backdrop(dir: vec3<f32>) -> vec4<f32> {
    let kind = P.kinds.y;
    if kind == 1u {
        return vec4<f32>(P.background.xyz, 1.0);
    }
    if kind == 2u {
        return vec4<f32>(0.0);
    }
    var d = dir;
    if P.kinds.x == 0u {
        // A seamless studio sweep: no dark floor behind the model.
        d = safe_normalize(vec3<f32>(dir.x, max(dir.y, 0.0), dir.z), vec3<f32>(0.0, 1.0, 0.0));
    }
    return vec4<f32>(background(d), 1.0);
}

fn floor_hit(o: vec3<f32>, d: vec3<f32>) -> f32 {
    if P.kinds.z == 0u || d.y >= -1e-9 || o.y <= P.shade.x {
        return -1.0;
    }
    let t = (P.shade.x - o.y) / d.y;
    if t > 0.0 {
        return t;
    }
    return -1.0;
}

// ---- Surfaces -------------------------------------------------------------------------------

struct Surface {
    base: vec3<f32>,
    metallic: f32,
    alpha: f32,
    ior: f32,
};

fn make_surface(base: vec3<f32>, metallic: f32, roughness: f32, ior: f32) -> Surface {
    var s: Surface;
    s.base = base;
    s.metallic = clamp(metallic, 0.0, 1.0);
    s.alpha = max(roughness * roughness, MIN_ALPHA);
    s.ior = select(1.5, ior, ior > 1.0);
    return s;
}

fn schlick(f0: vec3<f32>, c: f32) -> vec3<f32> {
    let m = clamp(1.0 - c, 0.0, 1.0);
    let m5 = m * m * m * m * m;
    return f0 + (vec3<f32>(1.0) - f0) * m5;
}

fn ggx_d(alpha: f32, n_h: f32) -> f32 {
    let a2 = alpha * alpha;
    let d = n_h * n_h * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 1e-12);
}

fn smith_g1(alpha: f32, n_v: f32) -> f32 {
    let a2 = alpha * alpha;
    return 2.0 * n_v / max(n_v + sqrt(a2 + (1.0 - a2) * n_v * n_v), 1e-12);
}

fn surface_f0(s: Surface) -> vec3<f32> {
    let r = (s.ior - 1.0) / (s.ior + 1.0);
    return mix(vec3<f32>(r * r), s.base, s.metallic);
}

fn surface_diffuse(s: Surface) -> vec3<f32> {
    return s.base * (1.0 - s.metallic);
}

fn specular_chance(s: Surface, n_v: f32) -> f32 {
    let diffuse = luminance(surface_diffuse(s));
    if diffuse <= 1e-4 {
        return 1.0;
    }
    let f = luminance(schlick(surface_f0(s), n_v));
    return clamp(f / (f + diffuse * (1.0 - f)), 0.15, 0.85);
}

// Scattering × cosine (xyz) and the density `sample_surface` draws `wi` with (w).
fn eval_surface(s: Surface, n: vec3<f32>, wo: vec3<f32>, wi: vec3<f32>) -> vec4<f32> {
    let n_v = dot(n, wo);
    let n_l = dot(n, wi);
    if n_v <= 1e-6 || n_l <= 1e-6 {
        return vec4<f32>(0.0);
    }
    let h = safe_normalize(wo + wi, n);
    let n_h = max(dot(n, h), 0.0);
    let v_h = max(dot(wo, h), 1e-6);
    let d = ggx_d(s.alpha, n_h);
    let g = smith_g1(s.alpha, n_v) * smith_g1(s.alpha, n_l);
    let f = schlick(surface_f0(s), v_h);
    let specular = f * (d * g / (4.0 * n_v));
    let fresnel_view = luminance(schlick(surface_f0(s), n_v));
    let diffuse = surface_diffuse(s) * ((1.0 - fresnel_view) * n_l / PI);
    let ps = specular_chance(s, n_v);
    let pdf = ps * d * n_h / (4.0 * v_h) + (1.0 - ps) * n_l / PI;
    return vec4<f32>(specular + diffuse, pdf);
}

struct Scatter {
    ok: bool,
    wi: vec3<f32>,
    weight: vec3<f32>,
    pdf: f32,
    delta: bool,
};

fn sample_surface(s: Surface, n: vec3<f32>, wo: vec3<f32>) -> Scatter {
    var out: Scatter;
    out.ok = false;
    out.delta = false;
    let n_v = dot(n, wo);
    if n_v <= 1e-6 {
        return out;
    }
    let ps = specular_chance(s, n_v);
    var wi: vec3<f32>;
    if rand() < ps {
        let u1 = rand();
        let u2 = rand();
        let a2 = s.alpha * s.alpha;
        let cos_t = sqrt(max((1.0 - u1) / (1.0 + (a2 - 1.0) * u1), 0.0));
        let sin_t = sqrt(max(1.0 - cos_t * cos_t, 0.0));
        let phi = TAU * u2;
        let bs = make_basis(n);
        let h = normalize(bs.t * (sin_t * cos(phi)) + bs.b * (sin_t * sin(phi)) + n * cos_t);
        wi = reflect(-wo, h);
    } else {
        wi = cosine_hemisphere(n);
    }
    let e = eval_surface(s, n, wo, wi);
    if e.w <= 1e-8 {
        return out;
    }
    out.ok = true;
    out.wi = wi;
    out.weight = e.xyz / e.w;
    out.pdf = e.w;
    return out;
}

fn fresnel_dielectric(cos_i: f32, eta: f32) -> f32 {
    let sin2_t = eta * eta * max(1.0 - cos_i * cos_i, 0.0);
    if sin2_t >= 1.0 {
        return 1.0;
    }
    let cos_t = sqrt(1.0 - sin2_t);
    let rs = (eta * cos_i - cos_t) / (eta * cos_i + cos_t);
    let rp = (cos_i - eta * cos_t) / (cos_i + eta * cos_t);
    return 0.5 * (rs * rs + rp * rp);
}

fn sample_glass(outward: vec3<f32>, wo: vec3<f32>, ior: f32, tint: vec3<f32>) -> Scatter {
    var out: Scatter;
    out.ok = true;
    out.delta = true;
    out.pdf = 1.0;
    out.weight = vec3<f32>(1.0);
    let entering = dot(outward, wo) > 0.0;
    var n = outward;
    var eta = 1.0 / ior;
    if !entering {
        n = -outward;
        eta = ior;
    }
    let cos_i = clamp(dot(n, wo), 0.0, 1.0);
    let f = fresnel_dielectric(cos_i, eta);
    let d = -wo;
    if rand() < f {
        out.wi = reflect(d, n);
        return out;
    }
    let cos_d = -dot(n, d);
    let k = 1.0 - eta * eta * (1.0 - cos_d * cos_d);
    if k < 0.0 {
        out.wi = reflect(d, n);
        return out;
    }
    out.wi = normalize(d * eta + n * (eta * cos_d - sqrt(k)));
    out.weight = tint;
    return out;
}

// ---- One camera path -------------------------------------------------------------------------

struct PathOut {
    color: vec3<f32>,
    alpha: f32,
    albedo: vec3<f32>,
    normal: vec3<f32>,
    depth: f32,
};

// For a point on a shadow-catcher floor: the share of the light it would get that the
// model does not block, from one light sample and one sample of the sky.
fn shadow_ratio(point: vec3<f32>) -> f32 {
    let lift = point + vec3<f32>(0.0, 1.0, 0.0) * P.up.w;
    var open = 0.0;
    var total = 0.0;
    let count = light_count();
    if count > 0u {
        let pick = min(u32(rand() * f32(count)), count - 1u);
        let light = P.lights[pick];
        let u1 = rand();
        let u2 = rand();
        let wi = uniform_cone(light.dir.xyz, light.dir.w, u1, u2);
        if wi.y > 0.0 {
            let c = luminance(light.radiance.xyz) * wi.y * f32(count) / cone_pdf(light.dir.w);
            total = total + c;
            open = open + c * luminance(transmittance(lift, wi));
        }
    }
    let wi = cosine_hemisphere(vec3<f32>(0.0, 1.0, 0.0));
    let c = luminance(background(wi)) * PI;
    total = total + c;
    open = open + c * luminance(transmittance(lift, wi));
    if total > 0.0 {
        return clamp(open / total, 0.0, 1.0);
    }
    return 1.0;
}

fn trace_path(origin_in: vec3<f32>, dir_in: vec3<f32>) -> PathOut {
    var origin = origin_in;
    var dir = dir_in;
    var throughput = vec3<f32>(1.0);
    var radiance = vec3<f32>(0.0);
    var have_first = false;
    var first_albedo = vec3<f32>(0.0);
    var first_normal = vec3<f32>(0.0);
    var first_depth = 0.0;
    var prev_pdf = 0.0;
    var prev_delta = true;
    var bounce = 0u;
    var crossings = 0u;
    let count = light_count();
    let eps = P.up.w;
    var out: PathOut;
    loop {
        let hit = intersect(origin, dir);
        var floor_t = floor_hit(origin, dir);
        if hit.found && floor_t >= hit.t {
            floor_t = -1.0;
        }
        // ---- Into the environment ----
        if !hit.found && floor_t < 0.0 {
            if bounce == 0u && crossings == 0u {
                let b = backdrop(dir);
                out.color = b.xyz * b.w;
                out.alpha = b.w;
                out.albedo = b.xyz;
                out.normal = vec3<f32>(0.0);
                out.depth = 0.0;
                return out;
            }
            radiance = radiance + throughput * background(dir);
            let li = light_in(dir);
            if li < 4u {
                var weight = 1.0;
                if !prev_delta && count > 0u {
                    let light_pdf = cone_pdf(P.lights[li].dir.w) / f32(count);
                    weight = power_heuristic(prev_pdf, light_pdf);
                }
                radiance = radiance + throughput * P.lights[li].radiance.xyz * weight;
            }
            break;
        }
        // ---- The floor, or a surface ----
        var point: vec3<f32>;
        var geometric: vec3<f32>;
        var shading: vec3<f32>;
        var look: Look;
        var base: vec3<f32>;
        if floor_t >= 0.0 {
            point = origin + dir * floor_t;
            if P.kinds.z == 1u {
                if bounce == 0u && crossings == 0u {
                    let ratio = shadow_ratio(point);
                    let b = backdrop(dir);
                    if b.w > 0.0 {
                        out.color = b.xyz * ratio;
                        out.alpha = 1.0;
                    } else {
                        out.color = vec3<f32>(0.0);
                        out.alpha = 1.0 - ratio;
                    }
                    out.albedo = b.xyz;
                    out.normal = vec3<f32>(0.0, 1.0, 0.0);
                    out.depth = floor_t;
                    return out;
                }
                // Seen from the model, the catcher is not there.
                origin = point + dir * eps;
                crossings = crossings + 1u;
                if crossings > MAX_CROSSINGS {
                    break;
                }
                continue;
            }
            geometric = vec3<f32>(0.0, 1.0, 0.0);
            shading = geometric;
            look.base = P.floor_look.xyz;
            look.opacity = 1.0;
            look.metallic = 0.0;
            look.roughness = P.floor_look.w;
            look.transmission = 0.0;
            look.ior = 1.5;
            base = look.base;
        } else {
            point = hit.point;
            geometric = hit.geometric;
            shading = hit.shading;
            look = looks[hit.look];
            base = look.base;
        }
        let wo = -dir;
        if !have_first {
            have_first = true;
            first_albedo = base;
            first_normal = select(shading, -shading, dot(shading, wo) < 0.0);
            first_depth = length(point - origin);
        }
        let glass = look.transmission >= 0.5;
        // ---- See-through looks: straight on ----
        if !glass && look.opacity < 0.999 && rand() >= look.opacity {
            origin = point + dir * eps;
            crossings = crossings + 1u;
            if crossings > MAX_CROSSINGS {
                break;
            }
            continue;
        }
        // ---- Glass ----
        if glass {
            let s = sample_glass(geometric, wo, look.ior, sqrt(max(base, vec3<f32>(0.0))));
            throughput = throughput * s.weight;
            origin = point + s.wi * eps;
            dir = s.wi;
            prev_delta = true;
            bounce = bounce + 1u;
            if bounce > P.limits.x + 8u {
                break;
            }
            continue;
        }
        // ---- Opaque: light it, then scatter ----
        let ng = select(geometric, -geometric, dot(geometric, wo) < 0.0);
        var n = select(shading, -shading, dot(shading, ng) < 0.0);
        if dot(n, wo) <= 1e-4 {
            n = ng;
        }
        let surface = make_surface(base, look.metallic, look.roughness, look.ior);
        let lift = point + ng * eps;
        if count > 0u {
            let pick = min(u32(rand() * f32(count)), count - 1u);
            let u1 = rand();
            let u2 = rand();
            let light = P.lights[pick];
            let wi = uniform_cone(light.dir.xyz, light.dir.w, u1, u2);
            if dot(wi, ng) > 0.0 {
                let e = eval_surface(surface, n, wo, wi);
                if max3(e.xyz) > 0.0 {
                    let through = transmittance(lift, wi);
                    if max3(through) > 0.0 {
                        let light_pdf = cone_pdf(light.dir.w) / f32(count);
                        let weight = power_heuristic(light_pdf, e.w);
                        radiance = radiance + throughput * e.xyz * light.radiance.xyz * through * (weight / light_pdf);
                    }
                }
            }
        }
        let s = sample_surface(surface, n, wo);
        if !s.ok || dot(s.wi, ng) <= 0.0 {
            break;
        }
        throughput = throughput * s.weight;
        prev_pdf = s.pdf;
        prev_delta = s.delta;
        origin = lift;
        dir = s.wi;
        bounce = bounce + 1u;
        if bounce > P.limits.x {
            break;
        }
        if bounce > 3u {
            let p = clamp(max3(throughput), 0.05, 0.95);
            if rand() >= p {
                break;
            }
            throughput = throughput * (1.0 / p);
        }
    }
    out.color = radiance;
    out.alpha = 1.0;
    out.albedo = first_albedo;
    out.normal = first_normal;
    out.depth = first_depth;
    return out;
}

// ---- Entry points ------------------------------------------------------------------------------

fn camera_ray(x: f32, y: f32) -> array<vec3<f32>, 2> {
    let w = f32(P.size.x);
    let h = max(f32(P.size.y), 1.0);
    let u = (x - w / 2.0) / h;
    let v = -(y - h / 2.0) / h;
    if P.eye.w < 0.5 {
        let k = 2.0 * P.forward.w;
        let d = normalize(P.forward.xyz + P.right.xyz * (u * k) + P.up.xyz * (v * k));
        return array<vec3<f32>, 2>(P.eye.xyz, d);
    }
    let height = P.forward.w;
    let o = P.eye.xyz + P.right.xyz * (u * height) + P.up.xyz * (v * height) - P.forward.xyz * P.right.w;
    return array<vec3<f32>, 2>(o, P.forward.xyz);
}

@compute @workgroup_size(8, 8)
fn trace(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = P.band.x + id.y;
    if x >= P.size.x || y >= P.size.y || id.y >= P.band.y {
        return;
    }
    let pixel = y * P.size.x + x;
    rng = pcg(pixel ^ pcg(P.size.w * 0x9E3779B9u + P.size.z));
    let jx = rand();
    let jy = rand();
    let ray = camera_ray(f32(x) + jx, f32(y) + jy);
    let r = trace_path(ray[0], ray[1]);
    // A rare NaN from a degenerate triangle would ruin the pixel for good.
    let finite = all(r.color == r.color) && all(abs(r.color) < vec3<f32>(BIG)) && r.alpha == r.alpha;
    if finite {
        accum[pixel] = accum[pixel] + vec4<f32>(r.color, r.alpha);
    }
    feat_a[pixel] = feat_a[pixel] + vec4<f32>(r.albedo, r.depth);
    feat_b[pixel] = feat_b[pixel] + vec4<f32>(r.normal, 0.0);
}

fn aces(x: f32) -> f32 {
    let c = max(x, 0.0);
    return clamp((c * (2.51 * c + 0.03)) / (c * (2.43 * c + 0.59) + 0.14), 0.0, 1.0);
}

fn to_srgb(c: f32) -> f32 {
    let v = clamp(c, 0.0, 1.0);
    if v <= 0.0031308 {
        return v * 12.92;
    }
    return 1.055 * pow(v, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(8, 8)
fn develop(@builtin(global_invocation_id) id: vec3<u32>) {
    let x = id.x;
    let y = id.y;
    if x >= P.size.x || y >= P.size.y {
        return;
    }
    let pixel = y * P.size.x + x;
    // Rows the sample in progress has reached have one sample more.
    var samples = P.band.w;
    if y < P.band.z {
        samples = samples + 1u;
    }
    var out = vec4<f32>(0.0);
    if samples > 0u {
        let s = accum[pixel] / f32(samples);
        let alpha = clamp(s.w, 0.0, 1.0);
        if s.w > 1e-6 {
            let straight = s.xyz / s.w * P.shade.z;
            let mapped = vec3<f32>(aces(straight.x), aces(straight.y), aces(straight.z));
            // egui draws premultiplied colours.
            let pre = mapped * alpha;
            out = vec4<f32>(to_srgb(pre.x), to_srgb(pre.y), to_srgb(pre.z), alpha);
        }
    }
    picture[y * P.limits.y + x] = pack4x8unorm(out);
}
