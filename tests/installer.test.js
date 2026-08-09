import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const hook = fs.readFileSync(new URL("../src-tauri/installer-hooks.nsh", import.meta.url), "utf8");

test("NSIS installs the user-facing desktop shortcut on fresh installs and updates", () => {
  assert.match(hook, /\$NoShortcutMode <> 1/);
  assert.match(
    hook,
    /CreateShortcut "\$DESKTOP\\\$\{DECK_SHORTCUT_NAME\}\.lnk" "\$INSTDIR\\\$\{MAINBINARYNAME\}\.exe"/,
  );
  assert.match(hook, /!macro NSIS_HOOK_POSTINSTALL[\s\S]*!insertmacro EnsureDeckDesktopShortcut/);
  assert.match(hook, /Function \.onGUIEnd[\s\S]*!insertmacro RenameDeckShortcuts/);
  assert.doesNotMatch(hook, /Function \.onGUIEnd[\s\S]*!insertmacro EnsureDeckDesktopShortcut/);
});
