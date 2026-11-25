### Solver threads capped for a whole run (unreleased, November 2026)

- **`OXYDE_SOLVER_THREADS`**, set to a whole number above zero, caps the threads of every
  study, thermal, frequency, topology and generative design solve in the process, the
  application's and the tests' alike. Unset, solves use every core as before.
