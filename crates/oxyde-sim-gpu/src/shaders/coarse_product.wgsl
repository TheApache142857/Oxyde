// A coarse level's product, node by node: each cell around the node a full 24 × 24 matrix or
// a scalar 8 × 8 one (the same for each direction), its rows for this node summed in the
// order of the node's corners; nodes no cell touches the identity. Then `mode` says what to
// write (see Params).

@group(0) @binding(0) var<storage, read> x: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> out: array<f32>;
// Per cell: NONE, the index of its full matrix, or SCALAR and the index of its scalar one.
@group(0) @binding(3) var<storage, read> info: array<u32>;
@group(0) @binding(4) var<storage, read> matrices: array<f32>;
@group(0) @binding(5) var<storage, read> scalars: array<f32>;
// Per node: 1 when a cell with a matrix touches it.
@group(0) @binding(6) var<storage, read> touched: array<u32>;
@group(0) @binding(7) var<storage, read> dinv: array<f32>;

fn value(n: u32) -> vec3<f32> {
    return vec3<f32>(x[3u * n], x[3u * n + 1u], x[3u * n + 2u]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    if (touched[n] == 0u) {
        finish(n, value(n));
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
        let e = info[ci + p.nx * (cj + p.ny * ck)];
        if (e == NONE) {
            continue;
        }
        let base = n - (ka + sx * (kb + sy * kc));
        if ((e & SCALAR) == 0u) {
            let m = 576u * e + 72u * a;
            for (var c = 0u; c < 8u; c++) {
                let v = value(base + (c & 1u) + sx * (((c >> 1u) & 1u) + sy * (c >> 2u)));
                let row = m + 3u * c;
                sum.x += matrices[row] * v.x + matrices[row + 1u] * v.y + matrices[row + 2u] * v.z;
                sum.y += matrices[row + 24u] * v.x + matrices[row + 25u] * v.y + matrices[row + 26u] * v.z;
                sum.z += matrices[row + 48u] * v.x + matrices[row + 49u] * v.y + matrices[row + 50u] * v.z;
            }
        } else {
            let m = 64u * (e & ~SCALAR) + 8u * a;
            for (var c = 0u; c < 8u; c++) {
                let v = value(base + (c & 1u) + sx * (((c >> 1u) & 1u) + sy * (c >> 2u)));
                sum += scalars[m + c] * v;
            }
        }
    }
    finish(n, sum);
}
