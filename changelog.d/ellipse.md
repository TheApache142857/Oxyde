### Ellipses in sketches (unreleased, November 2026)

- **Convert Entities projects a tilted circle exactly**: a circle or arc of the part whose
  plane is tilted to the sketch becomes the ellipse or elliptic arc it projects to, fixed in
  place, and one seen edge-on the line across it. Before, such edges offered their vertices
  only.
- **Ellipses and elliptic arcs are sketch entities**, extruded, cut and revolved like circles
  and arcs, with *point on ellipse* and *major* and *minor semi-axis* dimensions.
  The assistant's sketches take `{"type": "ellipse", "center", "radii": [a, b], "angle"}`,
  fully dimensioned. Trim, Extend and Offset Entities leave ellipses alone; Mirror Entities
  copies them. There is no ellipse drawing tool yet.
- **File format**: a part whose sketches hold an ellipse is written as `format_version` 3
  (FORMAT.md §2, §3 Sketches); older Oxyde refuses it with a message. Parts without one are
  written as before.
