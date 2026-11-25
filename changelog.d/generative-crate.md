### The generative design solver in its own crate (unreleased, November 2026)

- **Generative design studies take about half as long again**: the solver runs each
  parallel step on threads started for that step, without `unsafe` code, where it kept
  threads between steps before. Its outcomes are the same. The solver is now the crate
  `oxyde-generative`, which depends on no other crate.
