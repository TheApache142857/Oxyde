### Converted edges follow the part (unreleased, November 2026)

- **Convert Entities' copies follow what they copy**: when the part changes, a copied edge
  or vertex moves with it on the next rebuild, as in SolidWorks; deleting the copy ends
  the link. While the edge or vertex is gone (a feature suppressed, a face removed) its
  copy stays where it was and the sketch shows a warning; it follows again once it is
  back.
- **File format**: sketches write an optional `projections` list (FORMAT.md §3 Sketches).
  Older Oxyde opens such parts and keeps the copies fixed where they were saved.
