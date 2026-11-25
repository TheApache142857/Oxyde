### Every edge converts exactly (unreleased, November 2026)

- **Convert Entities copies any edge of the part into a sketch exactly**: the part's own
  ellipses, spline edges (of lofts, sweeps, fillets, imported STEP), Bézier curves,
  parabolas and hyperbolas, in the sketch plane or projected onto it. A spline edge becomes
  the spline on its projected poles, which is its projection exactly; before, such edges
  offered their vertices only. The kernel approximates offset curves within a micrometre.
- **Splines are sketch entities**, standing on their poles, extruded, cut and revolved like
  other curves. Mirror Entities copies them; Trim, Extend and Offset Entities leave them
  alone. There is no spline drawing tool yet.
- **File format**: a part whose sketches hold a spline is written as `format_version` 4
  (FORMAT.md §2, §3 Sketches); older Oxyde refuses it with a message. Parts without one are
  written as before.
