// The conjugate gradients' kernels in double precision, one per `//!! name` section, built
// as small.wgsl's are.

//!! to_f32
// The residual handed to the single-precision cycle.
@group(0) @binding(0) var<storage, read> src: array<f64>;
@group(0) @binding(1) var<storage, read_write> dst: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        dst[i] = f32(src[i]);
    }
}

//!! to_f64
// The cycle's answer, kept to the allowed motions, back in double precision.
@group(0) @binding(0) var<storage, read> src: array<f32>;
@group(0) @binding(1) var<storage, read_write> dst: array<f64>;
@group(0) @binding(2) var<storage, read> held: array<u32>;
@group(0) @binding(3) var<storage, read> proj: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let o = 3u * n;
    let h = held[n];
    let v = vec3<f64>(f64(src[o]), f64(src[o + 1u]), f64(src[o + 2u]));
    if (h == FREE) {
        dst[o] = v.x;
        dst[o + 1u] = v.y;
        dst[o + 2u] = v.z;
    } else if (h == INACTIVE) {
        dst[o] = f64(0.0);
        dst[o + 1u] = f64(0.0);
        dst[o + 2u] = f64(0.0);
    } else {
        let q = 12u * h;
        dst[o] = proj[q] * v.x + proj[q + 1u] * v.y + proj[q + 2u] * v.z;
        dst[o + 1u] = proj[q + 3u] * v.x + proj[q + 4u] * v.y + proj[q + 5u] * v.z;
        dst[o + 2u] = proj[q + 6u] * v.x + proj[q + 7u] * v.y + proj[q + 8u] * v.z;
    }
}

//!! copy64
@group(0) @binding(0) var<storage, read> src: array<f64>;
@group(0) @binding(1) var<storage, read_write> dst: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        dst[i] = src[i];
    }
}

//!! dot_partial64
@group(0) @binding(0) var<storage, read> a: array<f64>;
@group(0) @binding(1) var<storage, read> b: array<f64>;
@group(0) @binding(2) var<storage, read_write> partials: array<f64>;

var<workgroup> shared_sums: array<f64, 256>;

@compute @workgroup_size(256)
fn main(
    @builtin(local_invocation_index) lid: u32,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) groups: vec3<u32>,
) {
    let w = wid.x + wid.y * groups.x;
    let start = w * 1024u + 4u * lid;
    var s = f64(0.0);
    for (var k = 0u; k < 4u; k++) {
        let i = start + k;
        if (i < p.count) {
            s += a[i] * b[i];
        }
    }
    shared_sums[lid] = s;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width >>= 1u) {
        if (lid < width) {
            shared_sums[lid] += shared_sums[lid + width];
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        partials[w] = shared_sums[0];
    }
}

//!! dot_finish64
@group(0) @binding(0) var<storage, read> partials: array<f64>;
@group(0) @binding(1) var<storage, read_write> scalars: array<f64>;

var<workgroup> shared_sums: array<f64, 256>;

@compute @workgroup_size(256)
fn main(@builtin(local_invocation_index) lid: u32) {
    var s = f64(0.0);
    for (var i = lid; i < p.count; i += 256u) {
        s += partials[i];
    }
    shared_sums[lid] = s;
    workgroupBarrier();
    for (var width = 128u; width > 0u; width >>= 1u) {
        if (lid < width) {
            shared_sums[lid] += shared_sums[lid + width];
        }
        workgroupBarrier();
    }
    if (lid == 0u) {
        scalars[p.slot] = shared_sums[0];
    }
}

//!! alpha64
@group(0) @binding(0) var<storage, read_write> scalars: array<f64>;

@compute @workgroup_size(1)
fn main() {
    let pq = scalars[PQ];
    var alpha = f64(0.0);
    if (pq > f64(0.0)) {
        alpha = scalars[RZ] / pq;
    }
    scalars[ALPHA] = alpha;
}

//!! beta64
@group(0) @binding(0) var<storage, read_write> scalars: array<f64>;
@group(0) @binding(1) var<storage, read_write> history: array<f64>;

@compute @workgroup_size(1)
fn main() {
    let rz = scalars[RZ];
    var beta = f64(0.0);
    if (rz > f64(0.0)) {
        beta = scalars[RZ_NEXT] / rz;
    }
    scalars[BETA] = beta;
    scalars[RZ] = scalars[RZ_NEXT];
    let k = u32(scalars[COUNTER]);
    if (k < arrayLength(&history)) {
        history[k] = scalars[RR];
    }
    scalars[COUNTER] = scalars[COUNTER] + f64(1.0);
}

//!! start64
@group(0) @binding(0) var<storage, read_write> scalars: array<f64>;
@group(0) @binding(1) var<storage, read_write> history: array<f64>;

@compute @workgroup_size(1)
fn main() {
    history[0] = scalars[RR];
    scalars[COUNTER] = f64(1.0);
}

//!! update_xr64
@group(0) @binding(0) var<storage, read_write> x: array<f64>;
@group(0) @binding(1) var<storage, read_write> r: array<f64>;
@group(0) @binding(2) var<storage, read> dir: array<f64>;
@group(0) @binding(3) var<storage, read> q: array<f64>;
@group(0) @binding(4) var<storage, read> scalars: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        let alpha = scalars[ALPHA];
        x[i] += alpha * dir[i];
        r[i] -= alpha * q[i];
    }
}

//!! update_dir64
@group(0) @binding(0) var<storage, read_write> dir: array<f64>;
@group(0) @binding(1) var<storage, read> z: array<f64>;
@group(0) @binding(2) var<storage, read> scalars: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        dir[i] = z[i] + scalars[BETA] * dir[i];
    }
}

//!! project64
// The frequency limit's eigensolve: a vector kept to the allowed motions in place, projected
// at held nodes, zero at nodes no cell touches.
@group(0) @binding(0) var<storage, read_write> v: array<f64>;
@group(0) @binding(1) var<storage, read> held: array<u32>;
@group(0) @binding(2) var<storage, read> proj: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let h = held[n];
    if (h == FREE) {
        return;
    }
    let o = 3u * n;
    if (h == INACTIVE) {
        v[o] = f64(0.0);
        v[o + 1u] = f64(0.0);
        v[o + 2u] = f64(0.0);
        return;
    }
    let q = 12u * h;
    let x = vec3<f64>(v[o], v[o + 1u], v[o + 2u]);
    v[o] = proj[q] * x.x + proj[q + 1u] * x.y + proj[q + 2u] * x.z;
    v[o + 1u] = proj[q + 3u] * x.x + proj[q + 4u] * x.y + proj[q + 5u] * x.z;
    v[o + 2u] = proj[q + 6u] * x.x + proj[q + 7u] * x.y + proj[q + 8u] * x.z;
}

//!! mass64
// The lumped masses times a vector, node by node, kept to the allowed motions.
@group(0) @binding(0) var<storage, read> src: array<f64>;
@group(0) @binding(1) var<storage, read_write> dst: array<f64>;
@group(0) @binding(2) var<storage, read> mass: array<f64>;
@group(0) @binding(3) var<storage, read> held: array<u32>;
@group(0) @binding(4) var<storage, read> proj: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let n = record(gid, groups);
    if (n >= p.count) {
        return;
    }
    let o = 3u * n;
    let h = held[n];
    let x = mass[n] * vec3<f64>(src[o], src[o + 1u], src[o + 2u]);
    if (h == FREE) {
        dst[o] = x.x;
        dst[o + 1u] = x.y;
        dst[o + 2u] = x.z;
    } else if (h == INACTIVE) {
        dst[o] = f64(0.0);
        dst[o + 1u] = f64(0.0);
        dst[o + 2u] = f64(0.0);
    } else {
        let q = 12u * h;
        dst[o] = proj[q] * x.x + proj[q + 1u] * x.y + proj[q + 2u] * x.z;
        dst[o + 1u] = proj[q + 3u] * x.x + proj[q + 4u] * x.y + proj[q + 5u] * x.z;
        dst[o + 2u] = proj[q + 6u] * x.x + proj[q + 7u] * x.y + proj[q + 8u] * x.z;
    }
}

//!! axpy64
// dst += c src, c the coefficients' slot `p.slot`.
@group(0) @binding(0) var<storage, read_write> dst: array<f64>;
@group(0) @binding(1) var<storage, read> src: array<f64>;
@group(0) @binding(2) var<storage, read> coef: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        dst[i] += coef[p.slot] * src[i];
    }
}

//!! scale64
// v *= c, c the coefficients' slot `p.slot`.
@group(0) @binding(0) var<storage, read_write> v: array<f64>;
@group(0) @binding(1) var<storage, read> coef: array<f64>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) groups: vec3<u32>) {
    let i = record(gid, groups);
    if (i < p.count) {
        v[i] *= coef[p.slot];
    }
}
