// The multigrid cycle's small kernels, in single precision, one per `//!! name` section:
// the Rust side cuts this file there and builds each section after common.wgsl as a module
// of its own, so each declares only what it binds.
//!! dinv_start
// A Chebyshev smoothing's first step from zero: d = c2 D⁻¹ b, and x = d.
@group(0) @binding(0) var<storage, read> b: array<f32>;
@group(0) @binding(1) var<storage, read> dinv: array<f32>;
@group(0) @binding(2) var<storage, read_write> d: array<f32>;
@group(0) @binding(3) var<storage, read_write> x: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let o = 3u * n;
    let q = 9u * n;
    let v = vec3<f32>(
        dinv[q] * b[o] + dinv[q + 1u] * b[o + 1u] + dinv[q + 2u] * b[o + 2u],
        dinv[q + 3u] * b[o] + dinv[q + 4u] * b[o + 1u] + dinv[q + 5u] * b[o + 2u],
        dinv[q + 6u] * b[o] + dinv[q + 7u] * b[o + 1u] + dinv[q + 8u] * b[o + 2u],
    );
    for (var r = 0u; r < 3u; r++) {
        d[o + r] = p.c2 * v[r];
        x[o + r] = p.c2 * v[r];
    }
}

//!! add
// x += d, value by value.
@group(0) @binding(0) var<storage, read_write> x: array<f32>;
@group(0) @binding(1) var<storage, read> d: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        x[i] += d[i];
    }
}

//!! restrict
// The fine residual gathered onto each coarse node by the transpose of trilinear
// interpolation: along an axis halved, the fine nodes 2I − 1, 2I and 2I + 1 weigh 1/2, 1
// and 1/2; along one kept, node I weighs 1.
@group(0) @binding(0) var<storage, read> fine: array<f32>;
@group(0) @binding(1) var<storage, read_write> coarse: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let csx = p.cx + 1u;
    let csy = p.cy + 1u;
    let c = vec3<u32>(n % csx, (n / csx) % csy, n / (csx * csy));
    let fs = vec3<u32>(p.nx + 1u, p.ny + 1u, p.nz + 1u);
    let factor = vec3<u32>(p.fx, p.fy, p.fz);
    var sum = vec3<f32>(0.0);
    for (var dz = 0u; dz < 3u; dz++) {
        for (var dy = 0u; dy < 3u; dy++) {
            for (var dx = 0u; dx < 3u; dx++) {
                let dd = vec3<u32>(dx, dy, dz);
                var w = 1.0;
                var f = vec3<u32>(0u);
                var ok = true;
                for (var a = 0u; a < 3u; a++) {
                    if (factor[a] == 1u) {
                        // Kept along this axis: only the middle offset, the same node.
                        if (dd[a] != 1u) {
                            ok = false;
                        }
                        f[a] = c[a];
                    } else {
                        // Fine node 2c − 1 + dd.
                        let at = 2u * c[a] + dd[a];
                        if (at == 0u || at - 1u >= fs[a]) {
                            ok = false;
                        } else {
                            f[a] = at - 1u;
                            if (dd[a] != 1u) {
                                w *= 0.5;
                            }
                        }
                    }
                }
                if (ok) {
                    let fi = f.x + fs.x * (f.y + fs.y * f.z);
                    sum += w * vec3<f32>(fine[3u * fi], fine[3u * fi + 1u], fine[3u * fi + 2u]);
                }
            }
        }
    }
    coarse[3u * n] = sum.x;
    coarse[3u * n + 1u] = sum.y;
    coarse[3u * n + 2u] = sum.z;
}

//!! prolong
// The coarse correction interpolated onto each fine node and added to it: along an axis
// halved, an even node takes its coarse node, an odd one half of each neighbour.
@group(0) @binding(0) var<storage, read> coarse: array<f32>;
@group(0) @binding(1) var<storage, read_write> fine: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let fsx = p.nx + 1u;
    let fsy = p.ny + 1u;
    let f = vec3<u32>(n % fsx, (n / fsx) % fsy, n / (fsx * fsy));
    let cs = vec3<u32>(p.cx + 1u, p.cy + 1u, p.cz + 1u);
    let factor = vec3<u32>(p.fx, p.fy, p.fz);
    var lo = vec3<u32>(0u);
    var hi = vec3<u32>(0u);
    var two = vec3<bool>(false);
    for (var a = 0u; a < 3u; a++) {
        if (factor[a] == 1u) {
            lo[a] = f[a];
        } else if ((f[a] & 1u) == 0u) {
            lo[a] = f[a] / 2u;
        } else {
            lo[a] = f[a] / 2u;
            hi[a] = f[a] / 2u + 1u;
            two[a] = true;
        }
    }
    var sum = vec3<f32>(0.0);
    for (var dz = 0u; dz < 2u; dz++) {
        if (dz == 1u && !two.z) {
            continue;
        }
        for (var dy = 0u; dy < 2u; dy++) {
            if (dy == 1u && !two.y) {
                continue;
            }
            for (var dx = 0u; dx < 2u; dx++) {
                if (dx == 1u && !two.x) {
                    continue;
                }
                var w = 1.0;
                if (two.x) { w *= 0.5; }
                if (two.y) { w *= 0.5; }
                if (two.z) { w *= 0.5; }
                let cx = select(lo.x, hi.x, dx == 1u);
                let cy = select(lo.y, hi.y, dy == 1u);
                let cz = select(lo.z, hi.z, dz == 1u);
                let cn = cx + cs.x * (cy + cs.y * cz);
                sum += w * vec3<f32>(coarse[3u * cn], coarse[3u * cn + 1u], coarse[3u * cn + 2u]);
            }
        }
    }
    fine[3u * n] += sum.x;
    fine[3u * n + 1u] += sum.y;
    fine[3u * n + 2u] += sum.z;
}

//!! coarsest
// The coarsest level solved by its inverse: x = A⁻¹ b, a row per invocation.
@group(0) @binding(0) var<storage, read> inverse: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> x: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i >= p.count) {
        return;
    }
    var sum = 0.0;
    for (var j = 0u; j < p.count; j++) {
        sum += inverse[i * p.count + j] * b[j];
    }
    x[i] = sum;
}
