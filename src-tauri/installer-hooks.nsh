!define DECK_OLD_SHORTCUT_NAME "ai-quota-deck"
!define DECK_SHORTCUT_NAME "AI Quota Deck"

!macro RenameDeckShortcuts
  ${If} ${FileExists} "$DESKTOP\${DECK_OLD_SHORTCUT_NAME}.lnk"
    Delete "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk"
    Rename "$DESKTOP\${DECK_OLD_SHORTCUT_NAME}.lnk" "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk"
  ${EndIf}

  ${If} ${FileExists} "$SMPROGRAMS\${DECK_OLD_SHORTCUT_NAME}.lnk"
    Delete "$SMPROGRAMS\${DECK_SHORTCUT_NAME}.lnk"
    Rename "$SMPROGRAMS\${DECK_OLD_SHORTCUT_NAME}.lnk" "$SMPROGRAMS\${DECK_SHORTCUT_NAME}.lnk"
  ${EndIf}
!macroend

; Silent/passive installs create their shortcuts before this hook. Interactive
; desktop shortcuts are created on the finish page and are handled by .onGUIEnd.
!macro NSIS_HOOK_POSTINSTALL
  !insertmacro RenameDeckShortcuts
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
