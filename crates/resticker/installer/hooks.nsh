; 2026-09-07: хук выполняется в секции удаления установщиком Tauri.
; Причина: автозапуск создаётся приложением в HKCU и не входит в записи NSIS.
Var restickerRemoveUserData

!macro NSIS_HOOK_PREUNINSTALL
  ; В тихом режиме нельзя задавать вопросы, а пользовательские данные нужно сохранить.
  StrCpy $restickerRemoveUserData 0
  IfSilent resticker_skip_user_data_prompt

  ; 2026-09-07: ответ «Нет» выбран по умолчанию, чтобы данные не исчезли случайно.
  ; Текст английский, как и весь интерфейс программы (i18n.rs: приложение
  ; англоязычное целиком). В списке установщика два языка, но вопрос про
  ; данные обязан читаться одинаково в обоих: русский вопрос от английской
  ; программы читается как чужой.
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 \
    "Remove resticker settings and stickers?$\r$\n$\r$\nThis deletes data in %APPDATA%\resticker and %LOCALAPPDATA%\resticker." \
    IDYES resticker_mark_user_data
  Goto resticker_skip_user_data_prompt

resticker_mark_user_data:
  StrCpy $restickerRemoveUserData 1

resticker_skip_user_data_prompt:
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; Выполняем очистку после проверки/остановки процесса, чтобы не оставить частичный результат.
  ; Значение удаляется только после успешного удаления, а при отмене деинсталлятора остаётся.
  ; Удаляем только собственное значение, не затрагивая автозапуск других программ.
  DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "resticker"
  StrCmp $restickerRemoveUserData 1 0 resticker_post_uninstall_done
  SetShellVarContext current
  RMDir /r "$APPDATA\resticker"
  RMDir /r "$LOCALAPPDATA\resticker"

resticker_post_uninstall_done:
!macroend
