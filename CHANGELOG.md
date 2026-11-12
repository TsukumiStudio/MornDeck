# Changelog

Releases use the section whose heading is the version number as their notes.

## Unreleased

### Added

- `mdeck kill` stops the server and every tab. `mdeck uninstall` also removes the hooks.
- Hooks can be removed from the settings (`Enter` on an installed agent).
- "使用量のゲージ" in the settings turns the usage gauge off; while off, MornDeck reads no token and calls no usage API.
- `mdeck --licenses` prints the licenses of the bundled libraries. The same text ships as `THIRD_PARTY_LICENSES` in the app and the release.
- `mdeck --help`, `-h` and `-V`.
- `add_webhook` returns a `warning` for an `http://` URL that is not on this Mac.
- English README. MCP tool descriptions are now in English.

### Changed

- The `Enter` key on an installed hook now removes it instead of reinstalling it.
- A request that is not valid JSON gets `{"ok": false, "error": {"code": "invalid", ...}}` instead of a string `error`.
- External commands (`curl`, `security` and others) run by absolute path.

### Fixed

- Installing hooks replaced a symlinked `settings.json` with a regular file and could widen its permissions. The link and the permissions are now kept, and the original content is saved once as `.mdeck-bak`.
- `events.log` recorded webhook URLs in full. They are now logged only up to the host, and curl receives the URL through a private file instead of its arguments.
- An empty `tab` argument matched the first unnamed tab. It is now `invalid`.
- A very small terminal made the screen panic in a loop.
- The codex gauge could not read a session log whose last 512 KB started in the middle of a character.
- The usage gauge fill crossed the "now" line while under pace.
- Server panics are now recorded in `events.log`.

## 0.3.3

- Hooks from another conversation inside a tab no longer change the tab's state or reply.
- `/clear` no longer counts as the end of the session.
- Log lines written at the same time no longer mix.

Earlier versions have no notes; see the commit history.
