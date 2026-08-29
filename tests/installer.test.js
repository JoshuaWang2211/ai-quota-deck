import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

const hook = fs.readFileSync(new URL("../src-tauri/installer-hooks.nsh", import.meta.url), "utf8");
const config = JSON.parse(
  fs.readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
);

test("only Tauri's finish-page action creates the desktop shortcut", () => {
  assert.equal(config.productName, "AI Quota Deck");
  assert.equal(config.bundle.windows.nsis.template, undefined);
  assert.doesNotMatch(hook, /CreateShortcut/);
  assert.doesNotMatch(hook, /EnsureDeckDesktopShortcut/);
  assert.match(hook, /!macro NSIS_HOOK_POSTINSTALL/);
  assert.match(hook, /Delete "\$DESKTOP\\\$\{DECK_OLD_SHORTCUT_NAME\}\.lnk"/);
});

test("update-mode uninstall preserves current shortcuts", () => {
  assert.match(hook, /\$\{If\} \$UpdateMode <> 1/);
  const guarded = hook.slice(hook.indexOf("${If} $UpdateMode <> 1"), hook.indexOf("${EndIf}"));
  assert.match(guarded, /DECK_SHORTCUT_NAME/);
  assert.doesNotMatch(guarded, /DECK_OLD_SHORTCUT_NAME/);
});
