// What a product writes at a node, its rows of `A x` in hand: `A x`, the residual
// `b − A x`, or a Chebyshev step's correction `c1 out + c2 D⁻¹ (b − A x)`. Expects `b`, `out`
// and `dinv` bound as the products bind them.

fn finish(n: u32, ax: vec3<f32>) {
    let o = 3u * n;
    if (p.mode == 0u) {
        out[o] = ax.x;
        out[o + 1u] = ax.y;
        out[o + 2u] = ax.z;
        return;
    }
    let r = vec3<f32>(b[o] - ax.x, b[o + 1u] - ax.y, b[o + 2u] - ax.z);
    if (p.mode == 1u) {
        out[o] = r.x;
        out[o + 1u] = r.y;
        out[o + 2u] = r.z;
        return;
    }
    let q = 9u * n;
    let v = vec3<f32>(
        dinv[q] * r.x + dinv[q + 1u] * r.y + dinv[q + 2u] * r.z,
        dinv[q + 3u] * r.x + dinv[q + 4u] * r.y + dinv[q + 5u] * r.z,
        dinv[q + 6u] * r.x + dinv[q + 7u] * r.y + dinv[q + 8u] * r.z,
    );
    out[o] = p.c1 * out[o] + p.c2 * v.x;
    out[o + 1u] = p.c1 * out[o + 1u] + p.c2 * v.y;
    out[o + 2u] = p.c1 * out[o + 2u] + p.c2 * v.z;
}
