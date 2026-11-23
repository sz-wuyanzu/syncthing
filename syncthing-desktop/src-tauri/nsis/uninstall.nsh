!macro NSIS_HOOK_PREUNINSTALL
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2|/SD IDNO \
    "是否删除 Syncthing 桌面应用的本地配置、数据库和日志？同步文件夹不会被删除。" \
    IDYES DeleteSyncthingData
  Goto SkipSyncthingData

DeleteSyncthingData:
  RMDir /r "$LOCALAPPDATA\Syncthing"
  RMDir /r "$LOCALAPPDATA\net.syncthing.desktop"
  RMDir /r "$APPDATA\net.syncthing.desktop"

SkipSyncthingData:
!macroend
