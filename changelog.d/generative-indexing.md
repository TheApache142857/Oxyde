### A broken generative study is refused with a message (unreleased, November 2026)

- **Generative design checks its input**: a body of the design space whose triangles name
  points it does not have, whose face labels do not match its triangles, or with a point
  that is not a number, is refused with a message naming the fault, where those triangles
  were dropped before. A voxel grid that does not match its outcome is refused instead of
  stopping Oxyde.
