; Super Copier Windows installer.
;
; Build with:
;   makensis -DVERSION=0.1.0 -DEXE_PATH=..\..\target\x86_64-pc-windows-gnu\release\super-copier.exe packaging\windows\installer.nsi
;
; Produces SuperCopierSetup.exe in the working directory.

!ifndef VERSION
  !define VERSION "0.1.0"
!endif
!ifndef EXE_PATH
  !define EXE_PATH "..\..\target\x86_64-pc-windows-gnu\release\super-copier.exe"
!endif

!define APP_NAME "Super Copier"
!define APP_EXE "super-copier.exe"
!define COMPANY "Super Copier"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\SuperCopier"

Name "${APP_NAME}"
OutFile "SuperCopierSetup.exe"
InstallDir "$PROGRAMFILES64\${APP_NAME}"
InstallDirRegKey HKLM "${UNINST_KEY}" "InstallLocation"
RequestExecutionLevel admin
Unicode true
SetCompressor /SOLID lzma

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!define MUI_ICON "..\..\app\assets\icon.ico"
!define MUI_UNICON "..\..\app\assets\icon.ico"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_RUN "$INSTDIR\${APP_EXE}"
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Install" SEC_INSTALL
  SetOutPath "$INSTDIR"
  File /oname=${APP_EXE} "${EXE_PATH}"

  CreateDirectory "$SMPROGRAMS\${APP_NAME}"
  CreateShortcut "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}"

  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr HKLM "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKLM "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKLM "${UNINST_KEY}" "Publisher" "${COMPANY}"
  WriteRegStr HKLM "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINST_KEY}" "NoRepair" 1
SectionEnd

Section "Explorer context menu" SEC_SHELL
  ; "Copy with Super Copier" / "Move with Super Copier" on right-click, for
  ; both files (*) and folders (Directory). Per-user (HKCU) so it needs no
  ; extra privilege beyond what installing to Program Files already does.
  ;
  ; Note: when multiple items are selected, Explorer invokes this command
  ; once per selected item (that's how classic shell verbs work) — the app
  ; forwards every invocation after the first to the one already-running
  ; window instead of opening several (see app/src/ipc.rs).
  WriteRegStr HKCU "Software\Classes\*\shell\SuperCopierCopy" "" "Copy with Super Copier"
  WriteRegStr HKCU "Software\Classes\*\shell\SuperCopierCopy" "Icon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "Software\Classes\*\shell\SuperCopierCopy\command" "" '"$INSTDIR\${APP_EXE}" "%1"'

  WriteRegStr HKCU "Software\Classes\*\shell\SuperCopierMove" "" "Move with Super Copier"
  WriteRegStr HKCU "Software\Classes\*\shell\SuperCopierMove" "Icon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "Software\Classes\*\shell\SuperCopierMove\command" "" '"$INSTDIR\${APP_EXE}" --move "%1"'

  WriteRegStr HKCU "Software\Classes\Directory\shell\SuperCopierCopy" "" "Copy with Super Copier"
  WriteRegStr HKCU "Software\Classes\Directory\shell\SuperCopierCopy" "Icon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "Software\Classes\Directory\shell\SuperCopierCopy\command" "" '"$INSTDIR\${APP_EXE}" "%1"'

  WriteRegStr HKCU "Software\Classes\Directory\shell\SuperCopierMove" "" "Move with Super Copier"
  WriteRegStr HKCU "Software\Classes\Directory\shell\SuperCopierMove" "Icon" "$INSTDIR\${APP_EXE}"
  WriteRegStr HKCU "Software\Classes\Directory\shell\SuperCopierMove\command" "" '"$INSTDIR\${APP_EXE}" --move "%1"'
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\${APP_NAME}\${APP_NAME}.lnk"
  RMDir "$SMPROGRAMS\${APP_NAME}"
  Delete "$DESKTOP\${APP_NAME}.lnk"

  DeleteRegKey HKLM "${UNINST_KEY}"

  DeleteRegKey HKCU "Software\Classes\*\shell\SuperCopierCopy"
  DeleteRegKey HKCU "Software\Classes\*\shell\SuperCopierMove"
  DeleteRegKey HKCU "Software\Classes\Directory\shell\SuperCopierCopy"
  DeleteRegKey HKCU "Software\Classes\Directory\shell\SuperCopierMove"
SectionEnd
