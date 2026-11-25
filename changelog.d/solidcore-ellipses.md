### Ellipses on the solidcore kernel (unreleased, November 2026)

- **The solidcore kernel extrudes ellipses and elliptic arcs exactly**, as conic pieces of
  at most a third of a turn; before, it refused them with a message. An ellipse's wall is
  three faces, named as the ellipse's parts 1, 2 and 3, where OpenCASCADE makes one face
  (part 0), so face names on such walls differ between the two kernels.
