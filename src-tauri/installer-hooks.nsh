!define DECK_OLD_SHORTCUT_NAME "ai-quota-deck"
!define DECK_SHORTCUT_NAME "AI Quota Deck"

; Remove shortcuts from pre-release builds that used the lowercase product
; name. Tauri's finish-page action is the only creator of the desktop shortcut.
!macro NSIS_HOOK_POSTINSTALL
  Delete "$DESKTOP\${DECK_OLD_SHORTCUT_NAME}.lnk"
  Delete "$SMPROGRAMS\${DECK_OLD_SHORTCUT_NAME}.lnk"
!macroend

; Also remove pre-release lowercase shortcuts left on development machines.
!macro NSIS_HOOK_PREUNINSTALL
  Delete "$DESKTOP\${DECK_OLD_SHORTCUT_NAME}.lnk"
  Delete "$SMPROGRAMS\${DECK_OLD_SHORTCUT_NAME}.lnk"
  ; Tauri's /UPDATE flow preserves current shortcuts and the new installer does
  ; not recreate them. Only a real uninstall should remove the current names.
  ${If} $UpdateMode <> 1
    Delete "$DESKTOP\${DECK_SHORTCUT_NAME}.lnk"
    Delete "$SMPROGRAMS\${DECK_SHORTCUT_NAME}.lnk"
  ${EndIf}
!macroend
