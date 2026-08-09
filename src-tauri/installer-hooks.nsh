!define DECK_OLD_SHORTCUT_NAME "ai-quota-deck"
!define DECK_SHORTCUT_NAME "AI Quota Deck"

!macro RenameDeckShortcuts
  ${If} ${FileExists} "$SMPROGRAMS\${DECK_OLD_SHORTCUT_NAME}.lnk"
    Delete "$SMPROGRAMS\${DECK_SHORTCUT_NAME}.lnk"
    Rename "$SMPROGRAMS\${DECK_OLD_SHORTCUT_NAME}.lnk" "$SMPROGRAMS\${DECK_SHORTCUT_NAME}.lnk"
  ${EndIf}
!macroend

!macro EnsureDeckDesktopShortcut
  ; Tauri deliberately skips its desktop-shortcut helper in update mode. That
  ; leaves users who did not already have a shortcut unable to add one by
  ; reinstalling. Honour the explicit /NS opt-out, but otherwise ensure the
  ; user-facing shortcut exists after both a fresh install and an update.
  ${If} $NoShortcutMode <> 1
    Delete "$DESKTOP\${DECK_OLD_SHORTCUT_NAME}.lnk"
    Delete "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk"
    CreateShortcut "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk" "$INSTDIR\${MAINBINARYNAME}.exe"
    !insertmacro SetLnkAppUserModelId "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk"
  ${EndIf}
!macroend

; Silent/passive installs create their shortcuts before this hook. Interactive
; desktop shortcuts are created on the finish page and are handled by .onGUIEnd.
!macro NSIS_HOOK_POSTINSTALL
  !insertmacro RenameDeckShortcuts
  !insertmacro EnsureDeckDesktopShortcut
!macroend

Function .onGUIEnd
  !insertmacro RenameDeckShortcuts
FunctionEnd

; Tauri's default uninstaller knows the original product-name shortcut. Remove
; the user-facing name here so an uninstall does not leave a dead link behind.
!macro NSIS_HOOK_PREUNINSTALL
  Delete "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk"
  Delete "$SMPROGRAMS\${DECK_SHORTCUT_NAME}.lnk"
!macroend
