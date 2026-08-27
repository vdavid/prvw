; Prvw's Windows installer.
;
; Built from macOS: `./scripts/build-windows-installer.sh`. See `CLAUDE.md` beside this file for
; the decisions behind it, and `docs/guides/releasing.md` for the release story around it.
;
; This file is UTF-8 with a BOM, and it has to stay that way: makensis reads a .nsi in the host's
; ANSI code page unless a BOM says otherwise, which would mangle "Rymdskottkärra" into whatever
; the build machine's locale makes of it. The `installer` check fails if the BOM goes missing.

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma

; ── What the build script hands in ───────────────────────────────────────────

!ifndef PRVW_VERSION
  !error "PRVW_VERSION isn't set. Build with ./scripts/build-windows-installer.sh, which reads it from apps/desktop/Cargo.toml."
!endif
!ifndef PRVW_EXE
  !error "PRVW_EXE isn't set: it's the path to the prvw.exe being packaged."
!endif
!ifndef PRVW_OUTFILE
  !error "PRVW_OUTFILE isn't set: it's where the finished installer goes."
!endif
!ifndef PRVW_LICENSE
  !define PRVW_LICENSE "${__FILEDIR__}/../../../../LICENSE"
!endif

!define PRVW_NAME "Prvw"
!define PRVW_PUBLISHER "Rymdskottkärra AB"
!define PRVW_WEBSITE "https://getprvw.com"
; Where Apps & features reads Prvw's entry. Under HKCU, so it's this user's install and nobody
; else's, and removing it needs no elevation either.
!define PRVW_UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Prvw"

; ── The installer itself ─────────────────────────────────────────────────────

Name "${PRVW_NAME}"
OutFile "${PRVW_OUTFILE}"
BrandingText "${PRVW_NAME} ${PRVW_VERSION}"

; Per-user, into the folder Windows keeps for exactly this. It's what lets the whole install run
; `asInvoker` with no UAC prompt, and it matches the file-type registration, which is HKCU-only
; because Windows gives an app no machine-wide say in defaults anyway.
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Prvw"
; An upgrade lands where the last install went, wherever the user chose to put it.
InstallDirRegKey HKCU "${PRVW_UNINSTALL_KEY}" "InstallLocation"

VIProductVersion "${PRVW_VERSION}.0"
VIAddVersionKey "ProductName" "${PRVW_NAME}"
VIAddVersionKey "ProductVersion" "${PRVW_VERSION}.0"
VIAddVersionKey "FileVersion" "${PRVW_VERSION}.0"
VIAddVersionKey "FileDescription" "Prvw setup"
VIAddVersionKey "CompanyName" "${PRVW_PUBLISHER}"
VIAddVersionKey "LegalCopyright" "© 2026 ${PRVW_PUBLISHER}"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!include "${__FILEDIR__}/file-associations.nsh"

!define MUI_ICON "${__FILEDIR__}/../../resources/AppIcon.ico"
!define MUI_UNICON "${__FILEDIR__}/../../resources/AppIcon.ico"
!define MUI_ABORTWARNING

!define MUI_WELCOMEPAGE_TITLE "Welcome to Prvw"
!define MUI_WELCOMEPAGE_TEXT "Prvw opens your photos the moment you double-click them. Arrow keys take you through the rest of the folder.$\r$\n$\r$\nThis installs Prvw for your account only, so it needs no administrator rights.$\r$\n$\r$\nClick Next to continue."
!insertmacro MUI_PAGE_WELCOME

!define MUI_PAGE_HEADER_TEXT "License"
!define MUI_PAGE_HEADER_SUBTEXT "Prvw is free forever for personal use."
!define MUI_LICENSEPAGE_TEXT_TOP "Here are the terms in full."
!define MUI_LICENSEPAGE_TEXT_BOTTOM "Click I agree to continue."
!define MUI_LICENSEPAGE_BUTTON "I agree"
!insertmacro MUI_PAGE_LICENSE "${PRVW_LICENSE}"

!define MUI_PAGE_HEADER_TEXT "Where to install"
!define MUI_PAGE_HEADER_SUBTEXT "Prvw goes in your own user folder."
!define MUI_DIRECTORYPAGE_TEXT_TOP "Prvw installs into your user folder, which is why it needs no administrator rights. Pick somewhere else if you'd rather."
!insertmacro MUI_PAGE_DIRECTORY

!define MUI_PAGE_HEADER_TEXT "Installing"
!define MUI_PAGE_HEADER_SUBTEXT "Copying Prvw into place."
!insertmacro MUI_PAGE_INSTFILES

!define MUI_FINISHPAGE_TITLE "Prvw is ready"
!define MUI_FINISHPAGE_TEXT "Right-click a photo and choose Open with to use Prvw, or open Windows Settings to make it your default image viewer."
!define MUI_FINISHPAGE_RUN "$INSTDIR\prvw.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Open Prvw now"
!insertmacro MUI_PAGE_FINISH

!define MUI_UNCONFIRMPAGE_TEXT_TOP "This removes Prvw and takes its file types back out of the registry. Your photos and your Prvw settings stay where they are."
!insertmacro MUI_UNPAGE_CONFIRM
!define MUI_PAGE_HEADER_TEXT "Uninstalling"
!define MUI_PAGE_HEADER_SUBTEXT "Taking Prvw back out."
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

; ── Shared bits ──────────────────────────────────────────────────────────────

; Windows refuses to open a running executable for writing, which is how this asks whether Prvw
; is open without a process-list plugin. Installing over a running Prvw would otherwise fail
; halfway with a message about `prvw.exe` that says nothing about what to do.
!macro EnsurePrvwIsClosed
  ${If} ${FileExists} "$INSTDIR\prvw.exe"
    ${Do}
      ClearErrors
      FileOpen $R0 "$INSTDIR\prvw.exe" a
      ${IfNot} ${Errors}
        FileClose $R0
        ${ExitDo}
      ${EndIf}
      ; Retry jumps two instructions on, past the Abort. Cancel falls into it, and so does a
      ; silent run, where there's nobody to close the window.
      MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "Prvw is still open. Close it, then click Retry." /SD IDCANCEL IDRETRY +2
      Abort
    ${Loop}
  ${EndIf}
!macroend

; Tell Explorer the "Open with" list changed, so it doesn't need a sign-out to notice.
; SHCNE_ASSOCCHANGED, with no flags and no paths.
!macro RefreshShellAssociations
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, i 0, i 0)'
!macroend

; ── Install ──────────────────────────────────────────────────────────────────

Function .onInit
  ; The only build we ship is x64. Windows on ARM runs it under emulation, so that's allowed;
  ; 32-bit Windows can't run it at all, and saying so beats a "not a valid Win32 application".
  ${IfNot} ${IsNativeAMD64}
  ${AndIfNot} ${IsNativeARM64}
    MessageBox MB_OK|MB_ICONSTOP "Prvw needs 64-bit Windows."
    Abort
  ${EndIf}
FunctionEnd

Section "Prvw"
  SetShellVarContext current
  !insertmacro EnsurePrvwIsClosed

  SetOutPath "$INSTDIR"
  File "${PRVW_EXE}"
  File "/oname=LICENSE.txt" "${PRVW_LICENSE}"

  CreateShortcut "$SMPROGRAMS\Prvw.lnk" "$INSTDIR\prvw.exe"

  DetailPrint "Registering Prvw's file types"
  !insertmacro PrvwRegisterFileTypes
  !insertmacro RefreshShellAssociations

  WriteUninstaller "$INSTDIR\Uninstall Prvw.exe"

  ; What Apps & features shows, and what it runs when someone clicks Uninstall.
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "DisplayName" "${PRVW_NAME}"
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "DisplayVersion" "${PRVW_VERSION}"
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\prvw.exe,0"
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "Publisher" "${PRVW_PUBLISHER}"
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "URLInfoAbout" "${PRVW_WEBSITE}"
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall Prvw.exe"'
  WriteRegStr HKCU "${PRVW_UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall Prvw.exe" /S'
  WriteRegDWORD HKCU "${PRVW_UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${PRVW_UNINSTALL_KEY}" "NoRepair" 1

  ; Apps & features shows a size only if we put one there, in KB.
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${PRVW_UNINSTALL_KEY}" "EstimatedSize" "$0"
SectionEnd

; ── Uninstall ────────────────────────────────────────────────────────────────

Section "Uninstall"
  SetShellVarContext current
  !insertmacro EnsurePrvwIsClosed

  !insertmacro PrvwUnregisterFileTypes
  !insertmacro RefreshShellAssociations

  Delete "$SMPROGRAMS\Prvw.lnk"
  Delete "$INSTDIR\prvw.exe"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\Uninstall Prvw.exe"
  ; Only if it's empty: someone may keep something of their own in there, and a viewer that
  ; deletes a folder it didn't fill is a viewer with a bad story to tell.
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "${PRVW_UNINSTALL_KEY}"
SectionEnd
