# overlays/ — vendored

A byte-identical copy of the corpus maintained in
[cli-permissions](https://github.com/aiperceivable/cli-permissions). **That
repository is upstream**: entries are written and verified there, against the
procedure in [`docs/overlays.md`](../docs/overlays.md), and land here as a
snapshot.

Vendored rather than submoduled because these files are compiled in with
`include_str!` — a missing one is a hard compile error, not a degraded feature,
and the same coupling runs through the test suite and `cargo package`. A clone
without `--recursive` would simply fail to build.

## Changing an entry

Fix it upstream. Then update the snapshot here and confirm the two agree:

```bash
python3 ../cli-permissions/tools/check-vendored.py ./overlays
```

Editing a file here alone produces a copy that disagrees with the corpus every
other consumer reads, which the check reports as `differs` without being able to
say which side is right.

## Which entries are vendored is apexe's decision

Upstream holds the corpus; this directory holds the set apexe ships built in,
and the comment on `BUILTIN_OVERLAYS` in `src/scanner/overlay_store.rs` explains
why that set is deliberately short. Users reach the rest through `overlay_dirs`
— see [`docs/overlays.md`](../docs/overlays.md) for the three destinations and
what each costs.
