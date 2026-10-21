// The finest level's product, node by node, never assembled: each voxel around the node a
// brick (positive stiffness) or, void, the brick's Laplacian (negative), its rows for this
// node summed in the order of the node's corners; held nodes projected, `P K P + d (I − P)`,
// nodes no voxel touches the identity. Then `mode` says what to write (see Params).

@group(0) @binding(0) var<storage, read> x: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
// Per voxel: its stiffness, negative for a Laplacian, 0 for nothing.
@group(0) @binding(3) var<storage, read> cell: array<f32>;
// The brick for a unit stiffness (576 values), then its Laplacian (64).
@group(0) @binding(4) var<storage, read> consts: array<f32>;
// Per node: FREE, INACTIVE, or the index of its projector.
@group(0) @binding(5) var<storage, read> held: array<u32>;
// Per held node: its projector (9 values, row-major), its held stiffness, two to spare.
@group(0) @binding(6) var<storage, read> proj: array<f32>;
// Per node: the inverse of its diagonal block (9 values).
@group(0) @binding(7) var<storage, read> dinv: array<f32>;

fn value(n: u32) -> vec3<f32> {
    return vec3<f32>(x[3u * n], x[3u * n + 1u], x[3u * n + 2u]);
}

fn project(h: u32, v: vec3<f32>) -> vec3<f32> {
    let o = 12u * h;
    return vec3<f32>(
        proj[o] * v.x + proj[o + 1u] * v.y + proj[o + 2u] * v.z,
        proj[o + 3u] * v.x + proj[o + 4u] * v.y + proj[o + 5u] * v.z,
        proj[o + 6u] * v.x + proj[o + 7u] * v.y + proj[o + 8u] * v.z,
    );
}

fn projected(n: u32) -> vec3<f32> {
    let h = held[n];
    if (h >= INACTIVE) {
        return value(n);
    }
    return project(h, value(n));
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let sx = p.nx + 1u;
    let sy = p.ny + 1u;
    let i = n % sx;
    let j = (n / sx) % sy;
    let k = n / (sx * sy);
    var sum = vec3<f32>(0.0);
    for (var a = 0u; a < 8u; a++) {
        let ka = a & 1u;
        let kb = (a >> 1u) & 1u;
        let kc = a >> 2u;
        if (i < ka || j < kb || k < kc) {
            continue;
        }
        let ci = i - ka;
        let cj = j - kb;
        let ck = k - kc;
        if (ci >= p.nx || cj >= p.ny || ck >= p.nz) {
            continue;
        }
        let s = cell[ci + p.nx * (cj + p.ny * ck)];
        if (s == 0.0) {
            continue;
        }
        let base = n - (ka + sx * (kb + sy * kc));
        var acc = vec3<f32>(0.0);
        if (s > 0.0) {
            for (var c = 0u; c < 8u; c++) {
                let nc = base + (c & 1u) + sx * (((c >> 1u) & 1u) + sy * (c >> 2u));
                let v = projected(nc);
                let row = 72u * a + 3u * c;
                acc.x += consts[row] * v.x + consts[row + 1u] * v.y + consts[row + 2u] * v.z;
                acc.y += consts[row + 24u] * v.x + consts[row + 25u] * v.y + consts[row + 26u] * v.z;
                acc.z += consts[row + 48u] * v.x + consts[row + 49u] * v.y + consts[row + 50u] * v.z;
            }
            sum += s * acc;
        } else {
            // A Laplacian has no held corner: nothing to project.
            for (var c = 0u; c < 8u; c++) {
                let nc = base + (c & 1u) + sx * (((c >> 1u) & 1u) + sy * (c >> 2u));
                acc += consts[576u + 8u * a + c] * value(nc);
            }
            sum += (-s) * acc;
        }
    }
    let h = held[n];
    let xn = value(n);
    var ax = sum;
    if (h == INACTIVE) {
        ax = xn;
    } else if (h != FREE) {
        let px = project(h, xn);
        let d = proj[12u * h + 9u];
        ax = project(h, sum) + d * (xn - px);
    }
    finish(n, ax);
}
