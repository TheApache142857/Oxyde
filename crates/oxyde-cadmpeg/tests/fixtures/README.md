# Test fixtures

FreeCAD files from cadmpeg's public corpus
(<https://github.com/cadmpeg/cadmpeg>, commit `d92395b`, `corpus/freecad_fcstd/fixtures/`),
authored from blank documents by the corpus's scripts and dedicated to the public domain
under [CC0-1.0](https://creativecommons.org/publicdomain/zero/1.0/) (`corpus/manifest.toml`).
Copied unchanged; the SHA-256 is the manifest's.

| File | What it holds (from its authoring script) | Volume, mm³ | SHA-256 |
|---|---|---|---|
| `uniform_appearance.FCStd` | `Part.makeBox(10, 20, 30)` (`author_uniform_fixture.py`) | 6000 | `b6840ea50a0b17df9385d99fbaa4895164ca0739219d876ab2f3ac8ec186cf48` |
| `external_component.FCStd` | `Part.makeCylinder(5, 4)` (`author_assembly_fixture.py`) | 100π ≈ 314.159 | `e1a563bd89e46c00237b65ac543482aaa361c833deef759164143687d0de557b` |
| `binary_exact_shape.FCStd` | `Part.makeCylinder(7, 12)` fused with `Part.makeSphere(8, (0, 0, 12))` (`author_fixtures.py`) | ≈ 3041.260 | `86ecddcee6d677370343c2f5a9682e57ca58db5808775b0e8812bdbe844dbe1e` |

The last volume is the cylinder's and the sphere's less their common part, the sphere below
z = 12 inside radius 7: 588π + 2048π/3 − π(512 − 512/3 − 59√15 + 49√15).
