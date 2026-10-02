# Tests

Always write tests. Every test must pass on Windows, Linux and macOS with 100% certainty before it is reported done: no system binaries, no fixed system paths, no assumptions about what is installed, no timing that depends on machine speed. A test that fails in CI on a platform I did not run it on breaks a release and forces a re-tag. That already happened once with `/bin/true` on macOS. It never happens again.

# Do not break what works

Adding a feature or fixing a bug must leave everything else working. These rules exist because changes that looked local have broken unrelated features.

- Run the full `cargo test` before reporting anything done, and show the result. A subset is not enough: the break is usually in a part you did not think you touched.
- Never delete, weaken, skip or `#[ignore]` an existing test to make a change pass. A test that fails after your change means your change broke something. Stop and ask.
- Before changing a shared function, find every caller and check that each still behaves the same. Example: a guard added to `Buffer::perform` in `src/code_editor/buffer.rs` to fix Backspace silently broke Select All and Shift+arrow selection, because both go through the same function.
- Every bug fix gets a test that fails without the fix.
- Change only what was asked. No refactors, renames, restyling or "while I'm here" edits to code outside the request.
- Read `ICED_NOTES.md` before touching iced code. It records facts about this project's iced version that are easy to get wrong.
