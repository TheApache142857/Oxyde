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

The order is the order in which the fragments came into `main`:

```sh
git log --diff-filter=A --name-only --format= -- 'changelog.d/*.md' ':!changelog.d/README.md'
```

The sections under *Unreleased* in `CHANGELOG.md` are older than every fragment.

## At a release

The release commit folds the fragments into `CHANGELOG.md`:

1. Replace the *Unreleased* heading with the version's heading.
2. Under it, put the section of each fragment, newest first (the order above), with `unreleased` in its
   title replaced by the version, above the sections that were under *Unreleased*.
3. Delete the fragments and keep this README.
