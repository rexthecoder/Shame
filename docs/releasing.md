# Releasing Shame

## Version policy

The release version is `0.0.1+N`, where `N` is the build number. It starts at
`0.0.1+1` and increments by one for each release. The version lives in two
places and they must match:

- `VERSION` in the `Makefile` (the source of truth for the macOS bundle and DMG)
- `version` in `crates/app/Cargo.toml`

## Steps

1. Bump `VERSION` and the app crate version to the next `0.0.1+N`.
2. Write `docs/releases/<version>.md`. The release workflow uses it as the
   GitHub Release body; without it the workflow falls back to generated notes.
3. Run `cargo test --workspace` and `make release` locally if possible.
4. Commit the version and notes, then tag and push:

   ```sh
   git tag "v$VERSION"
   git push origin main "v$VERSION"
   ```

The tag triggers `.github/workflows/release.yml`: it checks the tag against
`Makefile`, builds the universal `.dmg`, and publishes the GitHub Release with
the notes file.
