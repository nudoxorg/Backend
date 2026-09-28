# Lead's rulings on D-Browse's checkpoint (2026-09-27)

> Copied verbatim from the lead's scratch folder (`$S/wave4/browse/RULINGS.md`) into
> `docs/architecture/briefs/package-browsing/` by the W-Browse finisher, 2026-09-27. These
> rulings are binding; where any other document in this brief disagrees with them, this file
> wins (see `package-browsing.md` §4).

I accept it. The stills I read are find-words-1440, compare-1440, compare-costs-1440, compare-preview-1440 and tree-1440.

## Defects the implementation must not copy
1. **False equivalence in the adoption preview.** compare-costs-1440 says "77 of your 77 lines change only in name" for toml_edit, while compare-1440 correctly marks `as_table` and `as_array` as reading differently (`Item::as_table`). Worse, `toml_edit::Value` is a different type from `toml::Value`: it is a formatted value, and tables are `Item`s. The rule is:
   - A line changes "only in name" **only when every substituted item has the same shape**: parameters in words, output in words, and the receiver type.
   - A same-named item with a different shape is "reads differently", and the line shows the difference.
   - A substituted *type* is a rename only if its capabilities cover the uses on that line.
   - The index can answer this through shapes; name matching can't. Until shapes are available, show the preview as "matched by name" and never as "only in name".
2. **The sticky column header masks rows into dotted fragments** (compare-preview-1440, the costs band). The header must either fully cover what scrolls under it or not overlap it. Legible fragments must never show.
3. **The care line in the judge card** ("Yours · 80 places in …") sits in a box with a mint rule. Drop the box: the mint word carries it (§6.2: space, not boxes).

## Open questions
1. **Where Find lives:** its own reader route, `nudox://find?q=`. ⌘K Ask offers "all answers as a page ↵" as its last row. Ask stays the quick list.
2. **What "add" writes:** the target defaults to the member you're reading, otherwise the role's main user, with a menu.
   - If the workspace uses `[workspace.dependencies]`, add the dependency there and write `x = { workspace = true }` into the member. Otherwise write the member's table.
   - Show the exact diff before writing. Never write silently.
   - The write happens through the backend, not by shelling out to `cargo add`.
3. **Tree count:** count what builds for this host, and put "and N for other platforms" on hover.
4. **Measured churn:** lazy. Measure the last 12 releases when a package is judged or compared, cache the result, and read the full history on the package page's release lens.
5. **Role vocabulary:** derived, not editable in v1. Rename and merge per project come later.

## Requests to D-Marks (route to the marks implementation)
- `versionComb` compact variant: below 240 px it becomes a band.
- `ecosystemMark` gets a `quiet` option (no card) for rows where the ecosystem is implied.
