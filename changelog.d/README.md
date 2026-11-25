# Changelog fragments

Each pull request with a user-visible change adds one file here, `changelog.d/<topic>.md`, and leaves
`CHANGELOG.md` as it is. Two pull requests then never change the same lines, and GitHub can merge them one
after the other without a conflict.

## A fragment

- The name is a short topic in lowercase words joined by hyphens, unique in this folder, for example
  `sketch-axis-picks.md`. The branch name without its prefix is a good choice.
- The content is one section in the form of the sections of `CHANGELOG.md`: a `###` title with
  `(unreleased, <month> <year>)`, then bullets that say what the change does for the user, in words.
- A later pull request that changes the same feature before a release edits that fragment.

```markdown
### Sketches and axes yield to the part (unreleased, November 2026)

- **A click on the part picks the face or edge under it** while a sketch or an axis crosses it, unless the
  command takes sketches or axes.
```

## Reading them, newest first

The order is the order in which the fragments came into `main`; fragments that one commit added come in
name order:

```sh
git log --diff-filter=A --name-only --format= -- 'changelog.d/*.md' ':!changelog.d/README.md'
```

Every fragment is newer than the sections in `CHANGELOG.md`.

## At a release

The release commit folds the fragments into `CHANGELOG.md`:

1. Put the section of each fragment at the top of *Unreleased*, above the sections already there, newest
   first (the order above), with `unreleased` in its title replaced by the version, as in
   `### Ellipses in sketches (0.0.12, November 2026)`.
2. Delete the fragments and keep this README.
